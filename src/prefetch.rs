use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    decoder::{self, DecodedImage, ImageInfo},
    error::{ImgSeqError, Result},
};

/// Upper bound on the decoded data kept ready ahead of the last request.
const READY_BYTE_BUDGET: usize = 192 * 1024 * 1024;
/// Extra cached frames kept beyond the lookahead window.
const READY_ENTRY_MARGIN: usize = 4;
/// Upper bound on the lookahead window, regardless of the worker count.
const MAX_WINDOW: usize = 16;
/// Upper bound on the number of workers a `prefetch` argument can request.
pub const MAX_WORKERS: usize = 16;
/// Upper bound on the automatically selected worker count.
pub const AUTO_MAX_WORKERS: usize = 4;
/// Safety net for consumers that end up waiting for an in-flight decode.
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Worker count used when the `prefetch` argument is omitted.
///
/// Leaves headroom for the requesting thread and the rest of the graph by
/// using half of the logical cores, clamped to a small number of workers.
#[must_use]
pub fn automatic_workers() -> usize {
    let parallelism = thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1);
    if parallelism >= 2 {
        (parallelism / 2).clamp(1, AUTO_MAX_WORKERS)
    } else {
        0
    }
}

struct Shared {
    state: Mutex<State>,
    signal: Condvar,
    stop: AtomicBool,
}

struct State {
    /// Pending prefetch requests, oldest first, with the generation they
    /// belong to.
    queue: VecDeque<(u64, i32)>,
    /// Frames currently being decoded by a worker or by a consumer.
    in_flight: HashSet<i32>,
    /// Decoded frames waiting for a `fetch` call.
    ready: BTreeMap<i32, std::result::Result<Arc<DecodedImage>, String>>,
    ready_bytes: usize,
    /// Bumped whenever requests stop being sequential so that results from
    /// the previous access pattern can be discarded.
    generation: u64,
    last_index: Option<i32>,
    window: usize,
    /// Number of frames the pool keeps cached, independent of their size.
    entry_cap: usize,
}

/// Decodes upcoming frames in the background so that sequential frame
/// requests overlap with the rest of the filter graph.
///
/// The pool is deliberately conservative: it only looks ahead after
/// sequential requests, limits the lookahead window, caps the amount of
/// decoded data held in memory, and never blocks progress on the worker
/// threads (a consumer decodes its own frame when nothing else has claimed
/// it).
///
/// Successful decodes stay cached until they are evicted, so a filter that
/// returns several clips can share one decode per frame.
pub struct Prefetcher {
    images: Arc<[ImageInfo]>,
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl Prefetcher {
    /// Creates a pool with `workers` background decoders. A worker count of
    /// zero disables lookahead decoding and makes [`Prefetcher::fetch`]
    /// decode on the calling thread.
    pub fn new(images: Arc<[ImageInfo]>, workers: usize) -> Self {
        let workers = workers.min(MAX_WORKERS);
        let window = if workers == 0 {
            0
        } else {
            (workers + 2).min(MAX_WINDOW)
        };

        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                in_flight: HashSet::new(),
                ready: BTreeMap::new(),
                ready_bytes: 0,
                generation: 0,
                last_index: None,
                window,
                entry_cap: window + READY_ENTRY_MARGIN,
            }),
            signal: Condvar::new(),
            stop: AtomicBool::new(false),
        });

        let workers = (0..workers)
            .map(|_| {
                let shared = Arc::clone(&shared);
                let images = Arc::clone(&images);
                thread::Builder::new()
                    .name(String::from("imgseqs-prefetch"))
                    .spawn(move || worker(&shared, &images))
                    .expect("spawning a prefetch worker should succeed")
            })
            .collect();

        Self {
            images,
            shared,
            workers,
        }
    }

    /// Returns the decoded frame `index`, using a prefetched result when one
    /// is ready and decoding it on the calling thread otherwise.
    ///
    /// Several consumers may share one pool: a frame that has already been
    /// decoded is handed out again instead of being decoded twice.
    pub fn fetch(&self, index: i32) -> Result<Arc<DecodedImage>> {
        self.plan(index);

        loop {
            {
                let mut state = self.lock();
                if matches!(state.ready.get(&index), Some(Err(_))) {
                    // A failed decode is reported once and retried by the next
                    // request instead of being cached.
                    if let Some(Err(message)) = state.ready.remove(&index) {
                        return Err(ImgSeqError::new(message));
                    }
                }
                if let Some(Ok(image)) = state.ready.get(&index) {
                    return Ok(Arc::clone(image));
                }
                if claim(&mut state, index) {
                    drop(state);
                    let decoded = decoder::decode(&self.images[index as usize]);
                    let mut state = self.lock();
                    state.in_flight.remove(&index);
                    let result = match decoded {
                        Ok(image) => {
                            let image = Arc::new(image);
                            store(&mut state, index, Ok(Arc::clone(&image)));
                            Ok(image)
                        }
                        Err(error) => {
                            store(&mut state, index, Err(error.to_string()));
                            Err(error)
                        }
                    };
                    drop(state);
                    self.shared.signal.notify_all();
                    return result;
                }
            }

            // Another thread is already decoding this frame; wait for it
            // instead of duplicating the work.
            let (state, _) = self
                .shared
                .signal
                .wait_timeout_while(self.lock(), WAIT_TIMEOUT, |state| {
                    state.in_flight.contains(&index) && !state.ready.contains_key(&index)
                })
                .unwrap_or_else(|error| error.into_inner());
            if state.in_flight.contains(&index) {
                // The worker did not deliver in time; take the frame over.
                let mut state = state;
                state.in_flight.remove(&index);
            }
        }
    }

    /// Records the request and queues the following frames when the access
    /// pattern is sequential.
    fn plan(&self, index: i32) {
        let mut state = self.lock();
        // A filter that returns several clips asks for the same frame once per
        // clip, so a repeated request still counts as sequential access.
        let sequential = state
            .last_index
            .is_some_and(|last| index == last || index == last.saturating_add(1));
        if !sequential {
            state.generation = state.generation.wrapping_add(1);
            state.queue.clear();
        }
        state.last_index = Some(index);

        if state.window > 0 {
            let last = i32::try_from(self.images.len())
                .unwrap_or(i32::MAX)
                .saturating_sub(1);
            let end = index.saturating_add(state.window as i32).min(last);
            let generation = state.generation;
            for candidate in index.saturating_add(1)..=end {
                if state.ready.contains_key(&candidate)
                    || state.in_flight.contains(&candidate)
                    || state.queue.iter().any(|&(_, queued)| queued == candidate)
                {
                    continue;
                }
                state.queue.push_back((generation, candidate));
            }
        }

        drop(state);
        self.shared.signal.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

impl Drop for Prefetcher {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.signal.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

// All shared mutation happens behind a mutex and lock poisoning is handled by
// continuing with the inner state, so the prefetcher cannot observe a broken
// invariant after a panic.
impl std::panic::RefUnwindSafe for Prefetcher {}
impl std::panic::UnwindSafe for Prefetcher {}

/// Marks `index` as being decoded by the caller unless a worker owns it.
fn claim(state: &mut State, index: i32) -> bool {
    state.queue.retain(|&(_, queued)| queued != index);
    if state.in_flight.contains(&index) {
        return false;
    }
    state.in_flight.insert(index);
    true
}

fn worker(shared: &Shared, images: &[ImageInfo]) {
    loop {
        let (generation, index) = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            while state.queue.is_empty() && !shared.stop.load(Ordering::Relaxed) {
                state = shared
                    .signal
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
            let Some((generation, index)) = state.queue.pop_front() else {
                return;
            };
            if generation != state.generation
                || state.in_flight.contains(&index)
                || state.ready.contains_key(&index)
            {
                continue;
            }
            state.in_flight.insert(index);
            (generation, index)
        };

        let result = decoder::decode(&images[index as usize])
            .map(Arc::new)
            .map_err(|error| error.to_string());

        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.in_flight.remove(&index);
        if generation == state.generation && !shared.stop.load(Ordering::Relaxed) {
            store(&mut state, index, result);
        }

        drop(state);
        shared.signal.notify_all();
    }
}

/// Caches a decoded frame (or its error) and trims the cache.
fn store(state: &mut State, index: i32, result: std::result::Result<Arc<DecodedImage>, String>) {
    if state.ready.contains_key(&index) {
        return;
    }
    if let Ok(image) = &result {
        state.ready_bytes = state.ready_bytes.saturating_add(image.pixels.len());
    }
    state.ready.insert(index, result);
    evict(state);
}

/// Releases one cached frame, keeping the byte accounting in sync.
fn release(state: &mut State, index: i32) {
    if let Some(Ok(image)) = state.ready.remove(&index) {
        state.ready_bytes = state.ready_bytes.saturating_sub(image.pixels.len());
    }
}

/// Drops cached frames until the cache fits the entry and byte budgets.
///
/// Frames behind the newest request are released first because consumers only
/// move forward. When every cached frame is still ahead of the request, the
/// furthest one is dropped because it is the cheapest to decode again later.
fn evict(state: &mut State) {
    while state.ready.len() > state.entry_cap || state.ready_bytes > READY_BYTE_BUDGET {
        if state.ready.len() <= 1 {
            break;
        }
        let (Some(&oldest), Some(&newest)) =
            (state.ready.keys().next(), state.ready.keys().next_back())
        else {
            break;
        };
        let victim = match state.last_index {
            Some(last) if oldest < last => oldest,
            _ => newest,
        };
        release(state, victim);
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::Prefetcher;
    use crate::decoder::{self, ImageInfo};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn images(names: &[&str]) -> Arc<[ImageInfo]> {
        names
            .iter()
            .map(|name| decoder::probe(&fixture(name)).expect("the fixture is a supported image"))
            .collect::<Vec<_>>()
            .into()
    }

    #[test]
    fn shares_one_decode_between_consumers() {
        for workers in [0, 2] {
            let prefetcher = Prefetcher::new(images(&["gray.pgm"]), workers);
            let first = prefetcher.fetch(0).expect("the fixture decodes");
            // Another clip asking for the same frame must reuse the decode
            // instead of reading the file a second time.
            let second = prefetcher.fetch(0).expect("the cached frame is reused");
            assert!(Arc::ptr_eq(&first, &second), "workers={workers}");
        }
    }

    #[test]
    fn keeps_frames_of_a_variable_sequence_apart() {
        let prefetcher = Prefetcher::new(images(&["gray.pgm", "rgb.ppm"]), 0);
        let gray = prefetcher.fetch(0).expect("the first fixture decodes");
        let rgb = prefetcher.fetch(1).expect("the second fixture decodes");
        assert_ne!(gray.color_type, rgb.color_type);
        assert_eq!(prefetcher.fetch(0).expect("cached").pixels, gray.pixels);
    }

    #[test]
    fn retries_failed_decodes() {
        let source = fixture("gray.pgm");
        let temp = std::env::temp_dir().join("imgseqs-prefetch-retry.pgm");
        let _ = std::fs::remove_file(&temp);
        let mut info = decoder::probe(&source).expect("the fixture probes");
        info.path = temp.clone();

        let prefetcher = Prefetcher::new(vec![info].into(), 0);
        assert!(
            prefetcher.fetch(0).is_err(),
            "a missing file reports an error"
        );
        assert!(
            prefetcher.fetch(0).is_err(),
            "both consumers are told about the failed decode"
        );
        std::fs::copy(&source, &temp).expect("the fixture is copied");
        assert!(
            prefetcher.fetch(0).is_ok(),
            "a failed decode is retried instead of being cached"
        );
        let _ = std::fs::remove_file(&temp);
    }
}
