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

/// Decoded data the pool keeps for small frames, and the floor of the
/// automatic budget.
const DEFAULT_BYTE_BUDGET: usize = 192 * 1024 * 1024;
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

/// Byte budget used when the `prefetch_memory` argument is omitted.
///
/// A pool cannot be faster than the frames it can hold: the fixed 192 MiB fit
/// three of the 55 MiB sandbox webp frames, so a window of 16 queued frames it
/// had to evict again and decoded them a second time instead of reading ahead.
/// The budget therefore follows the requested window, and the floor keeps
/// small frames at the previous value.
#[must_use]
pub fn automatic_budget(images: &[ImageInfo], window: usize) -> usize {
    let largest = images.iter().map(ImageInfo::frame_bytes).max().unwrap_or(0);
    window.saturating_mul(largest).max(DEFAULT_BYTE_BUDGET)
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
    /// Decoded data the pool may hold or have in flight.
    budget: usize,
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
    /// Decoded size of every frame, in the same order as `images`.
    sizes: Arc<[usize]>,
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl Prefetcher {
    /// Creates a pool with `workers` background decoders.
    ///
    /// A worker count of zero disables lookahead decoding and makes
    /// [`Prefetcher::fetch`] decode on the calling thread. `budget` is the
    /// amount of decoded frame data the pool may hold or have in flight;
    /// `None` derives it from the window and the largest frame of the sequence,
    /// see [`automatic_budget`].
    pub fn new(images: Arc<[ImageInfo]>, workers: usize, budget: Option<usize>) -> Self {
        let workers = workers.min(MAX_WORKERS);
        let window = if workers == 0 {
            0
        } else {
            (workers + 2).min(MAX_WINDOW)
        };
        let sizes: Arc<[usize]> = images
            .iter()
            .map(ImageInfo::frame_bytes)
            .collect::<Vec<_>>()
            .into();
        let budget = budget.unwrap_or_else(|| automatic_budget(&images, window));

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
                budget,
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
            sizes,
            shared,
            workers,
        }
    }

    /// Bytes of decoded frame data this pool may hold.
    #[must_use]
    pub fn byte_budget(&self) -> usize {
        self.lock().budget
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
        plan_window(&mut state, &self.sizes, index);

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

/// Scheduler state, for tests that need to look at the queue itself.
#[cfg(test)]
impl Prefetcher {
    fn queued(&self) -> usize {
        self.lock().queue.len()
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

/// Queues the frames of the lookahead window that fit the byte budget.
///
/// The budget covers what is ready, being decoded and still queued, so a frame
/// that does not fit is left to the consumer instead of being queued and
/// evicted before it is asked for, which is what made a deep `prefetch`
/// re-decode frames rather than read ahead.
///
/// Cached frames the consumer has already been handed are dropped to make room
/// first: they are only kept so that a second clip can share the decode, and
/// holding them would otherwise stall the lookahead on a full cache. Frames
/// ahead of the request are never dropped here, they are what the consumer asks
/// for next. Skipping rather than stopping at a frame that does not fit keeps
/// the window useful on sequences that mix small and large frames.
fn plan_window(state: &mut State, sizes: &[usize], index: i32) {
    if state.window == 0 {
        return;
    }
    let last = i32::try_from(sizes.len())
        .unwrap_or(i32::MAX)
        .saturating_sub(1);
    let end = index.saturating_add(state.window as i32).min(last);
    let generation = state.generation;
    let mut committed = committed_bytes(state, sizes);

    for candidate in index.saturating_add(1)..=end {
        if state.ready.contains_key(&candidate)
            || state.in_flight.contains(&candidate)
            || state.queue.iter().any(|&(_, queued)| queued == candidate)
        {
            continue;
        }
        let bytes = frame_size(sizes, candidate);
        if committed.saturating_add(bytes) > state.budget {
            make_room(state, sizes, index, bytes);
            committed = committed_bytes(state, sizes);
            if committed.saturating_add(bytes) > state.budget {
                continue;
            }
        }
        committed = committed.saturating_add(bytes);
        state.queue.push_back((generation, candidate));
    }
}

/// Releases frames the consumer has already been handed until `wanted` bytes
/// are free.
///
/// Every frame a second clip has already asked for is behind the newest
/// request, so it can be decoded again if a later graph asks for it a third
/// time. The oldest ones go first because they are the least likely to be
/// asked for again.
fn make_room(state: &mut State, sizes: &[usize], index: i32, wanted: usize) {
    let mut freed = 0;
    while freed < wanted {
        let Some(victim) = state.ready.keys().find(|&&key| key < index).copied() else {
            break;
        };
        freed = freed.saturating_add(frame_size(sizes, victim));
        release(state, victim);
    }
}

/// Decoded size of one frame, zero when the index is out of range.
fn frame_size(sizes: &[usize], index: i32) -> usize {
    usize::try_from(index)
        .ok()
        .and_then(|index| sizes.get(index))
        .copied()
        .unwrap_or(0)
}

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
        state.ready_bytes = state.ready_bytes.saturating_add(image.pixels.bytes());
    }
    state.ready.insert(index, result);
    evict(state);
}

/// Releases one cached frame, keeping the byte accounting in sync.
fn release(state: &mut State, index: i32) {
    if let Some(Ok(image)) = state.ready.remove(&index) {
        state.ready_bytes = state.ready_bytes.saturating_sub(image.pixels.bytes());
    }
}

/// Everything the pool holds or has committed to produce: the frames cached for
/// a consumer, the frames a worker is decoding, and the frames still queued.
///
/// Derived from the maps rather than kept as a counter, so it cannot drift when
/// a decode fails, is claimed by a consumer, or is dropped with a generation.
fn committed_bytes(state: &State, sizes: &[usize]) -> usize {
    let size = |index: i32| {
        usize::try_from(index)
            .ok()
            .and_then(|index| sizes.get(index))
            .copied()
            .unwrap_or(0)
    };
    state.ready_bytes
        + state
            .queue
            .iter()
            .map(|&(_, index)| size(index))
            .sum::<usize>()
        + state
            .in_flight
            .iter()
            .map(|&index| size(index))
            .sum::<usize>()
}

/// Drops cached frames until the cache fits the entry and byte budgets.
///
/// Frames behind the newest request are released first because consumers only
/// move forward. When every cached frame is still ahead of the request, the
/// furthest one is dropped because it is the cheapest to decode again later.
fn evict(state: &mut State) {
    while state.ready.len() > state.entry_cap || state.ready_bytes > state.budget {
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
    use std::collections::{BTreeMap, HashSet, VecDeque};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use image::{ColorType, ExtendedColorType, metadata::Orientation};

    use super::{
        DEFAULT_BYTE_BUDGET, Prefetcher, READY_ENTRY_MARGIN, State, automatic_budget,
        committed_bytes, plan_window,
    };
    use crate::decoder::{self, DecodeTimings, DecodedImage, ImageInfo, Pixels};
    use crate::pixel::PixelFormat;

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

    /// An image that is never read, to size budgets in tests.
    fn synthetic(width: u32, height: u32, color_type: ColorType) -> ImageInfo {
        ImageInfo {
            path: PathBuf::from("synthetic"),
            width,
            height,
            color_type,
            original_color_type: ExtendedColorType::from(color_type),
            has_icc_profile: false,
            orientation: Orientation::NoTransforms,
            format: PixelFormat::from_color_type(color_type).expect("a supported color type"),
        }
    }

    /// A scheduler state with an empty cache.
    fn state_with(window: usize, budget: usize) -> State {
        State {
            queue: VecDeque::new(),
            in_flight: HashSet::new(),
            ready: BTreeMap::new(),
            ready_bytes: 0,
            generation: 0,
            last_index: None,
            window,
            entry_cap: window + READY_ENTRY_MARGIN,
            budget,
        }
    }

    /// A fake decode result that holds `bytes` bytes.
    fn decoded(bytes: usize) -> DecodedImage {
        DecodedImage {
            width: 1,
            height: 1,
            format: PixelFormat::Gray8,
            pixels: Pixels::Interleaved {
                color_type: ColorType::L8,
                buffer: vec![0; bytes],
            },
            timings: DecodeTimings {
                open: Duration::ZERO,
                metadata: Duration::ZERO,
                buffer: Duration::ZERO,
                read: Duration::ZERO,
            },
        }
    }

    #[test]
    fn shares_one_decode_between_consumers() {
        for workers in [0, 2] {
            let prefetcher = Prefetcher::new(images(&["gray.pgm"]), workers, None);
            let first = prefetcher.fetch(0).expect("the fixture decodes");
            // Another clip asking for the same frame must reuse the decode
            // instead of reading the file a second time.
            let second = prefetcher.fetch(0).expect("the cached frame is reused");
            assert!(Arc::ptr_eq(&first, &second), "workers={workers}");
        }
    }

    #[test]
    fn keeps_frames_of_a_variable_sequence_apart() {
        let prefetcher = Prefetcher::new(images(&["gray.pgm", "rgb.ppm"]), 0, None);
        let gray = prefetcher.fetch(0).expect("the first fixture decodes");
        let rgb = prefetcher.fetch(1).expect("the second fixture decodes");
        assert_ne!(gray.format, rgb.format);
        assert_eq!(prefetcher.fetch(0).expect("cached").pixels, gray.pixels);
    }

    #[test]
    fn retries_failed_decodes() {
        let source = fixture("gray.pgm");
        let temp = std::env::temp_dir().join("imgseqs-prefetch-retry.pgm");
        let _ = std::fs::remove_file(&temp);
        let mut info = decoder::probe(&source).expect("the fixture probes");
        info.path = temp.clone();

        let prefetcher = Prefetcher::new(vec![info].into(), 0, None);
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

    #[test]
    fn automatic_budget_keeps_the_floor_and_follows_the_window() {
        let small = synthetic(1404, 2000, ColorType::Rgb8);
        assert_eq!(
            automatic_budget(&[small], 6),
            DEFAULT_BYTE_BUDGET,
            "frames that fit the floor do not move it"
        );

        let large = synthetic(3672, 5274, ColorType::Rgb8);
        assert_eq!(large.frame_bytes(), 3672 * 5274 * 3);
        let images = [large.clone()];
        assert_eq!(
            automatic_budget(&images, 6),
            large.frame_bytes() * 6,
            "a deep window on large frames raises the budget"
        );
        assert_eq!(
            automatic_budget(&images, 0),
            DEFAULT_BYTE_BUDGET,
            "without lookahead only the floor is needed"
        );
    }

    #[test]
    fn a_budget_too_small_for_one_frame_still_delivers() {
        let prefetcher = Prefetcher::new(images(&["gray.pgm", "rgb.ppm"]), 2, Some(1));
        for index in 0..2 {
            assert!(
                prefetcher.fetch(index).is_ok(),
                "frame {index} still decodes"
            );
        }
        assert_eq!(prefetcher.queued(), 0, "no frame fits the budget");
    }

    #[test]
    fn the_window_stops_at_the_budget() {
        let sizes = [10, 10, 10, 10, 10, 10];
        let mut state = state_with(4, 25);
        plan_window(&mut state, &sizes, 0);
        let queued: Vec<i32> = state.queue.iter().map(|&(_, index)| index).collect();
        assert_eq!(queued, [1, 2], "only the frames that fit are queued");
    }

    #[test]
    fn a_budget_that_fits_the_window_queues_all_of_it() {
        let sizes = [10, 10, 10, 10, 10, 10];
        let mut state = state_with(4, 1024);
        plan_window(&mut state, &sizes, 0);
        let queued: Vec<i32> = state.queue.iter().map(|&(_, index)| index).collect();
        assert_eq!(queued, [1, 2, 3, 4]);
    }

    #[test]
    fn a_frame_that_does_not_fit_is_skipped_not_stopped() {
        let sizes = [10, 100, 10, 10];
        let mut state = state_with(3, 25);
        plan_window(&mut state, &sizes, 0);
        let queued: Vec<i32> = state.queue.iter().map(|&(_, index)| index).collect();
        assert_eq!(queued, [2, 3], "the large frame is left to the consumer");
    }

    #[test]
    fn delivered_frames_are_dropped_to_keep_the_lookahead_running() {
        let sizes = [10, 10, 10, 10, 10, 10];
        let mut state = state_with(4, 25);
        for index in [0, 1] {
            state.ready.insert(index, Ok(Arc::new(decoded(10))));
        }
        state.ready_bytes = 20;
        state.last_index = Some(0);

        plan_window(&mut state, &sizes, 1);
        let queued: Vec<i32> = state.queue.iter().map(|&(_, index)| index).collect();
        assert_eq!(queued, [2], "the delivered frame made room for one more");
        assert!(
            !state.ready.contains_key(&0),
            "a frame the consumer already has is the cheapest to drop"
        );
        assert!(
            state.ready.contains_key(&1),
            "frames ahead of the request are kept"
        );
    }

    #[test]
    fn a_queued_frame_is_not_queued_again() {
        let sizes = [10, 10, 10, 10];
        let mut state = state_with(3, 1024);
        plan_window(&mut state, &sizes, 0);
        plan_window(&mut state, &sizes, 1);
        let queued: Vec<i32> = state.queue.iter().map(|&(_, index)| index).collect();
        assert_eq!(
            queued,
            [1, 2, 3],
            "the frames already queued are not repeated"
        );
    }

    #[test]
    fn committed_bytes_counts_ready_decoding_and_queued() {
        let sizes = [10, 20, 30, 40];
        let mut state = State {
            queue: VecDeque::from([(0, 1)]),
            in_flight: HashSet::from([2]),
            ready: BTreeMap::new(),
            ready_bytes: 40,
            generation: 0,
            last_index: None,
            window: 4,
            entry_cap: 8,
            budget: 100,
        };
        assert_eq!(committed_bytes(&state, &sizes), 20 + 30 + 40);
        // A failed decode is cached without holding a frame of data.
        state.ready.insert(0, Err(String::from("boom")));
        assert_eq!(committed_bytes(&state, &sizes), 90);
    }
}
