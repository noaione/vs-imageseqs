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
/// A pool cannot be faster than the payloads it can hold: the fixed 192 MiB fit
/// three of the 55 MiB sandbox webp frames, so a window of 16 queued frames it
/// had to evict again and decoded them a second time instead of reading ahead.
/// The budget therefore follows the requested window, and the floor keeps
/// small frames at the previous value.
#[must_use]
pub fn automatic_budget(sizes: &[usize], window: usize) -> usize {
    let largest = sizes.iter().copied().max().unwrap_or(0);
    window.saturating_mul(largest).max(DEFAULT_BYTE_BUDGET)
}

/// What one finished decode hands to the clips that ask for an index.
///
/// The pool never looks inside a payload: it counts [`Payload::bytes`] against
/// the lookahead budget, and hands out a clone instead of decoding again.
///
/// A payload is handed to one thread at a time, so it does not have to be
/// `Sync`; a VapourSynth frame is `Send` but not `Sync`.
pub trait Payload: Clone + Send + 'static {
    /// Memory one payload holds, in bytes.
    fn bytes(&self) -> usize;
}

/// Turns one decoded image into the payload the clips of a call ask for.
///
/// Decoding and payload building both happen on the worker that read the file,
/// so the thread that answers a request only hands out what is already
/// finished.
pub trait Prepare: Send + Sync + 'static {
    /// What a request for one index hands out.
    type Payload: Payload;

    /// Payload bytes one image is expected to hold.
    ///
    /// This is what the lookahead budget is sized from, before anything is
    /// decoded; [`Payload::bytes`] reports what a payload really holds.
    fn estimate(&self, image: &ImageInfo) -> usize;

    /// Builds the payload of one decoded image.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the image cannot be turned into a payload.
    fn build(&self, image: &ImageInfo, index: i32, decoded: DecodedImage) -> Result<Self::Payload>;
}

struct Shared<P: Prepare> {
    state: Mutex<State<P>>,
    signal: Condvar,
    stop: AtomicBool,
    /// Turns a decoded image into what the clips of a call are handed.
    prepare: P,
}

struct State<P: Prepare> {
    /// Pending prefetch requests, oldest first, with the generation they
    /// belong to.
    queue: VecDeque<(u64, i32)>,
    /// Frames currently being decoded by a worker or by a consumer.
    in_flight: HashSet<i32>,
    /// Finished payloads waiting for a `fetch` call.
    ready: BTreeMap<i32, std::result::Result<P::Payload, String>>,
    ready_bytes: usize,
    /// Bumped whenever requests stop being sequential so that results from
    /// the previous access pattern can be discarded.
    generation: u64,
    last_index: Option<i32>,
    window: usize,
    /// Number of payloads the pool keeps cached, independent of their size.
    entry_cap: usize,
    /// Payload data the pool may hold or have in flight.
    budget: usize,
}

/// Decodes upcoming frames in the background so that sequential frame
/// requests overlap with the rest of the filter graph.
///
/// The pool is deliberately conservative: it only looks ahead after
/// sequential requests, limits the lookahead window, caps the amount of
/// payload data held in memory, and never blocks progress on the worker
/// threads (a consumer decodes its own frame when nothing else has claimed
/// it).
///
/// Everything a request needs is finished before it is cached, so the thread
/// that answers a frame request never decodes or writes pixels. Successful
/// decodes stay cached until they are evicted, so a filter that returns several
/// clips shares one decode per frame.
pub struct Prefetcher<P: Prepare> {
    images: Arc<[ImageInfo]>,
    /// Payload bytes of every image, in the same order as `images`.
    sizes: Arc<[usize]>,
    shared: Arc<Shared<P>>,
    workers: Vec<JoinHandle<()>>,
}

impl<P: Prepare> Prefetcher<P> {
    /// Creates a pool with `workers` background decoders.
    ///
    /// A worker count of zero disables lookahead decoding and makes
    /// [`Prefetcher::fetch`] decode and build on the calling thread. `budget`
    /// is the amount of payload data the pool may hold or have in flight;
    /// `None` derives it from the window and the largest payload of the
    /// sequence, see [`automatic_budget`].
    pub fn new(
        images: Arc<[ImageInfo]>,
        prepare: P,
        workers: usize,
        budget: Option<usize>,
    ) -> Self {
        let workers = workers.min(MAX_WORKERS);
        let window = if workers == 0 {
            0
        } else {
            (workers + 2).min(MAX_WINDOW)
        };
        let sizes: Arc<[usize]> = images
            .iter()
            .map(|image| prepare.estimate(image))
            .collect::<Vec<_>>()
            .into();
        let budget = budget.unwrap_or_else(|| automatic_budget(&sizes, window));

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
            prepare,
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

    /// Bytes of payload data this pool may hold.
    #[must_use]
    pub fn byte_budget(&self) -> usize {
        self.lock().budget
    }

    /// Returns the payload of `index`, using a prefetched result when one is
    /// ready and building it on the calling thread otherwise.
    ///
    /// Several consumers may share one pool: a payload that has already been
    /// built is handed out again instead of being decoded twice.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the index is out of range or its image
    /// cannot be decoded.
    pub fn fetch(&self, index: i32) -> Result<P::Payload> {
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
                if let Some(Ok(payload)) = state.ready.get(&index) {
                    return Ok(payload.clone());
                }
                if claim(&mut state, index) {
                    drop(state);
                    let built = self.build(index);
                    let mut state = self.lock();
                    state.in_flight.remove(&index);
                    let result = match built {
                        Ok(payload) => {
                            store(&mut state, index, Ok(payload.clone()));
                            Ok(payload)
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

    /// Decodes one image and builds its payload, on the calling thread.
    fn build(&self, index: i32) -> Result<P::Payload> {
        let image = self.image(index)?;
        let decoded = decoder::decode(image)?;
        self.shared.prepare.build(image, index, decoded)
    }

    /// Image of one frame index.
    fn image(&self, index: i32) -> Result<&ImageInfo> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.images.get(index))
            .ok_or_else(|| out_of_range(index, self.images.len()))
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

    fn lock(&self) -> MutexGuard<'_, State<P>> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

/// Scheduler state, for tests that need to look at the queue itself.
#[cfg(test)]
impl<P: Prepare> Prefetcher<P> {
    fn queued(&self) -> usize {
        self.lock().queue.len()
    }
}

impl<P: Prepare> Drop for Prefetcher<P> {
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
impl<P: Prepare + std::panic::RefUnwindSafe> std::panic::RefUnwindSafe for Prefetcher<P> {}
impl<P: Prepare + std::panic::RefUnwindSafe> std::panic::UnwindSafe for Prefetcher<P> {}

/// Queues the frames of the lookahead window that fit the byte budget.
///
/// The budget covers what is ready, being decoded and still queued, so a frame
/// that does not fit is left to the consumer instead of being queued and
/// evicted before it is asked for, which is what made a deep `prefetch`
/// re-decode frames rather than read ahead.
///
/// Cached payloads the consumer has already been handed are dropped to make room
/// first: they are only kept so that a second clip can share the decode, and
/// holding them would otherwise stall the lookahead on a full cache. Payloads
/// ahead of the request are never dropped here, they are what the consumer asks
/// for next. Skipping rather than stopping at a frame that does not fit keeps
/// the window useful on sequences that mix small and large frames.
fn plan_window<P: Prepare>(state: &mut State<P>, sizes: &[usize], index: i32) {
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
fn make_room<P: Prepare>(state: &mut State<P>, sizes: &[usize], index: i32, wanted: usize) {
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
fn claim<P: Prepare>(state: &mut State<P>, index: i32) -> bool {
    state.queue.retain(|&(_, queued)| queued != index);
    if state.in_flight.contains(&index) {
        return false;
    }
    state.in_flight.insert(index);
    true
}

/// Error for a frame index the sequence does not have.
fn out_of_range(index: i32, frames: usize) -> ImgSeqError {
    ImgSeqError::new(format!(
        "requested frame {index}, but the clip has {frames} frames"
    ))
}

fn worker<P: Prepare>(shared: &Shared<P>, images: &[ImageInfo]) {
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

        // The queue only ever holds indices of this sequence, so an image that
        // is missing here is a bug rather than a failed decode.
        let result = match images.get(usize::try_from(index).unwrap_or(usize::MAX)) {
            Some(image) => match decoder::decode(image) {
                Ok(decoded) => shared
                    .prepare
                    .build(image, index, decoded)
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            },
            None => Err(out_of_range(index, images.len()).to_string()),
        };

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

/// Caches a finished payload (or its error) and trims the cache.
fn store<P: Prepare>(
    state: &mut State<P>,
    index: i32,
    result: std::result::Result<P::Payload, String>,
) {
    if state.ready.contains_key(&index) {
        return;
    }
    if let Ok(payload) = &result {
        state.ready_bytes = state.ready_bytes.saturating_add(payload.bytes());
    }
    state.ready.insert(index, result);
    evict(state);
}

/// Releases one cached payload, keeping the byte accounting in sync.
fn release<P: Prepare>(state: &mut State<P>, index: i32) {
    if let Some(Ok(payload)) = state.ready.remove(&index) {
        state.ready_bytes = state.ready_bytes.saturating_sub(payload.bytes());
    }
}

/// Everything the pool holds or has committed to produce: the payloads cached
/// for a consumer, the payloads a worker is building, and the ones still queued.
///
/// Derived from the maps rather than kept as a counter, so it cannot drift when
/// a decode fails, is claimed by a consumer, or is dropped with a generation.
fn committed_bytes<P: Prepare>(state: &State<P>, sizes: &[usize]) -> usize {
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

/// Drops cached payloads until the cache fits the entry and byte budgets.
///
/// Payloads behind the newest request are released first because consumers only
/// move forward. When every cached payload is still ahead of the request, the
/// furthest one is dropped because it is the cheapest to decode again later.
fn evict<P: Prepare>(state: &mut State<P>) {
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
        DEFAULT_BYTE_BUDGET, Payload, Prefetcher, Prepare, READY_ENTRY_MARGIN, State,
        automatic_budget, committed_bytes, plan_window,
    };
    use crate::decoder::{self, DecodeTimings, DecodedImage, ImageInfo, Pixels};
    use crate::error::Result;
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
            .map(|name| {
                decoder::probe(&fixture(name), true).expect("the fixture is a supported image")
            })
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
            icc_profile: None,
            cicp: None,
            chroma_location: None,
            orientation: Orientation::NoTransforms,
            transform: crate::pixel::Transform::IDENTITY,
            format: PixelFormat::from_color_type(color_type).expect("a supported color type"),
        }
    }

    /// A scheduler state with an empty cache.
    fn state_with(window: usize, budget: usize) -> State<Decode> {
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

    /// Hands the decode straight to the consumer.
    ///
    /// The scheduler only cares about how many bytes a payload holds and when
    /// it is ready, so the real payload, which builds VapourSynth frames, is
    /// only exercised from a graph.
    struct Decode;

    impl Payload for Arc<DecodedImage> {
        fn bytes(&self) -> usize {
            match &self.pixels {
                Pixels::Interleaved { buffer, .. } => buffer.len(),
                Pixels::Planar { planes, alpha } => {
                    planes.iter().map(Vec::len).sum::<usize>() + alpha.as_ref().map_or(0, Vec::len)
                }
            }
        }
    }

    /// Payload bytes the pool sizes its budget from for one synthetic image,
    /// which is what the production estimate reports too.
    fn bytes(image: &ImageInfo) -> usize {
        crate::clip::expected_bytes(crate::clip::READ_CLIPS, image)
    }

    impl Prepare for Decode {
        type Payload = Arc<DecodedImage>;

        fn estimate(&self, image: &ImageInfo) -> usize {
            bytes(image)
        }

        fn build(
            &self,
            _image: &ImageInfo,
            _index: i32,
            decoded: DecodedImage,
        ) -> Result<Arc<DecodedImage>> {
            Ok(Arc::new(decoded))
        }
    }

    /// A fake decode result that holds `bytes` bytes.
    fn decoded(bytes: usize) -> DecodedImage {
        DecodedImage {
            width: 1,
            height: 1,
            format: PixelFormat::Gray8,
            transform: crate::pixel::Transform::IDENTITY,
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
            let prefetcher = Prefetcher::new(images(&["gray.pgm"]), Decode, workers, None);
            let first = prefetcher.fetch(0).expect("the fixture decodes");
            // Another clip asking for the same frame must reuse the decode
            // instead of reading the file a second time.
            let second = prefetcher.fetch(0).expect("the cached frame is reused");
            assert!(Arc::ptr_eq(&first, &second), "workers={workers}");
        }
    }

    #[test]
    fn keeps_frames_of_a_variable_sequence_apart() {
        let prefetcher = Prefetcher::new(images(&["gray.pgm", "rgb.ppm"]), Decode, 0, None);
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
        let mut info = decoder::probe(&source, true).expect("the fixture probes");
        info.path = temp.clone();

        let prefetcher = Prefetcher::new(vec![info].into(), Decode, 0, None);
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
            automatic_budget(&[bytes(&small)], 6),
            DEFAULT_BYTE_BUDGET,
            "payloads that fit the floor do not move it"
        );

        let large = synthetic(3672, 5274, ColorType::Rgb8);
        assert_eq!(bytes(&large), 3672 * 5274 * 3);
        let sizes = [bytes(&large)];
        assert_eq!(
            automatic_budget(&sizes, 6),
            bytes(&large) * 6,
            "a deep window on large payloads raises the budget"
        );
        assert_eq!(
            automatic_budget(&sizes, 0),
            DEFAULT_BYTE_BUDGET,
            "without lookahead only the floor is needed"
        );
    }

    #[test]
    fn a_budget_too_small_for_one_frame_still_delivers() {
        let prefetcher = Prefetcher::new(images(&["gray.pgm", "rgb.ppm"]), Decode, 2, Some(1));
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
        let mut state = State::<Decode> {
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
