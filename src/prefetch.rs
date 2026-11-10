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
    decoder::Demand,
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
/// small frames at the previous value. `consumers` is how many clips of one
/// call take each payload, which is what makes a second request for one index
/// free: a call that hands out two clips asks for every frame twice, so the
/// window it needs is twice as wide in bytes.
#[must_use]
pub fn automatic_budget(sizes: &[usize], window: usize, consumers: usize) -> usize {
    let largest = sizes.iter().copied().max().unwrap_or(0);
    window
        .saturating_mul(largest)
        .saturating_mul(consumers.max(1))
        .max(DEFAULT_BYTE_BUDGET)
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

/// Turns one output frame into the payload the clips of a call ask for.
///
/// Decoding and payload building both happen on the worker that produced the
/// frame, so the thread that answers a request only hands out what is already
/// finished.
///
/// One output frame is not always one file: an animated source expands a single
/// path into several frames, and the implementor is what knows which
/// presentation of which file an index names. The pool only ever counts frames,
/// which is why it can stay unaware of that.
pub trait Prepare: Send + Sync + 'static {
    /// What a request for one index hands out.
    type Payload: Payload;

    /// Number of output frames the sequence has.
    fn frames(&self) -> usize;

    /// How many clips of one call take the payload of one index.
    ///
    /// `Read` hands out one clip and `ReadAlpha` two, and both are served by the
    /// same decode, so a payload a second clip still wants has to be held
    /// differently from one every clip has already taken. This is also the
    /// multiplier the lookahead budget is sized by, because a call of two clips
    /// asks for every frame twice.
    fn consumers(&self) -> usize;

    /// What the decode that feeds this payload has to produce.
    ///
    /// The decode is told before it starts, so a payload that does not need the
    /// alpha plane does not pay for it. The payload has to be buildable from a
    /// decode made for exactly this demand: a decode told to skip the alpha
    /// plane hands over an image with no alpha, which [`Self::build`] may write
    /// as the opaque fill but cannot recover the file's own plane from.
    fn demand(&self) -> Demand;

    /// Payload bytes one output frame is expected to hold.
    ///
    /// This is what the lookahead budget is sized from, before anything is
    /// decoded; [`Payload::bytes`] reports what a payload really holds.
    fn estimate(&self, index: usize) -> usize;

    /// Decodes one output frame and builds its payload, on the calling thread.
    ///
    /// # Errors
    ///
    /// Returns [`ImgSeqError`] when the frame cannot be decoded or written.
    fn produce(&self, index: usize) -> Result<Self::Payload>;
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
    /// The subset of `in_flight` a consumer thread is decoding.
    ///
    /// The pool can neither evict nor refuse those bytes, so they are not the
    /// pool's to budget while the claim is open; see [`committed_bytes`].
    claimed: HashSet<i32>,
    /// Finished payloads waiting for a `fetch` call.
    ready: BTreeMap<i32, std::result::Result<P::Payload, String>>,
    ready_bytes: usize,
    /// Clips that take the payload of one index, from [`Prepare::consumers`].
    consumers: usize,
    /// Hand-outs each cached payload still owes the clips of its call.
    ///
    /// A payload at zero has been taken by every clip of the call, which is the
    /// one eviction drops first.
    holds: BTreeMap<i32, usize>,
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
/// clips shares one decode per frame. A payload a second clip still wants is
/// the last thing eviction drops, and the budget is sized for the number of
/// clips the call hands out, so that share survives as long as the frames fit
/// it.
pub struct Prefetcher<P: Prepare> {
    /// Payload bytes of every output frame, in frame order.
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
    pub fn new(prepare: P, workers: usize, budget: Option<usize>) -> Self {
        let workers = workers.min(MAX_WORKERS);
        let window = if workers == 0 {
            0
        } else {
            (workers + 2).min(MAX_WINDOW)
        };
        let sizes: Arc<[usize]> = (0..prepare.frames())
            .map(|index| prepare.estimate(index))
            .collect::<Vec<_>>()
            .into();
        let consumers = prepare.consumers().max(1);
        let budget = budget.unwrap_or_else(|| automatic_budget(&sizes, window, consumers));

        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                in_flight: HashSet::new(),
                claimed: HashSet::new(),
                ready: BTreeMap::new(),
                ready_bytes: 0,
                consumers,
                holds: BTreeMap::new(),
                generation: 0,
                last_index: None,
                window,
                entry_cap: window.saturating_mul(consumers) + READY_ENTRY_MARGIN,
                budget,
            }),
            signal: Condvar::new(),
            stop: AtomicBool::new(false),
            prepare,
        });

        let workers = (0..workers)
            .map(|_| {
                let shared = Arc::clone(&shared);
                thread::Builder::new()
                    .name(String::from("imgseqs-prefetch"))
                    .spawn(move || worker(&shared))
                    .expect("spawning a prefetch worker should succeed")
            })
            .collect();

        Self {
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
                let cached = match state.ready.get(&index) {
                    Some(Ok(payload)) => Some(payload.clone()),
                    _ => None,
                };
                if let Some(payload) = cached {
                    note_handout(&mut state, index);
                    return Ok(payload);
                }
                if claim(&mut state, index) {
                    drop(state);
                    let built = self.build(index);
                    let mut state = self.lock();
                    state.in_flight.remove(&index);
                    let result = match built {
                        Ok(payload) => {
                            store(&mut state, index, Ok(payload.clone()));
                            note_handout(&mut state, index);
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

    /// Decodes one output frame and builds its payload, on the calling thread.
    fn build(&self, index: i32) -> Result<P::Payload> {
        let Some(index) = usize::try_from(index)
            .ok()
            .filter(|&index| index < self.shared.prepare.frames())
        else {
            return Err(out_of_range(index, self.shared.prepare.frames()));
        };
        self.shared.prepare.produce(index)
    }

    /// Records the request and queues the following frames when the access
    /// pattern is sequential.
    fn plan(&self, index: i32) {
        let mut state = self.lock();
        // A filter that returns several clips asks for the same frame once per
        // clip, so a repeated request still counts as sequential access. A
        // request that moves forward by one step is the same walk. A request
        // behind the newest position is a second clip of the call asking for a
        // frame the first one has already passed, or a small replay: it is
        // served from the cache or claimed by the caller, and it cannot
        // invalidate a lookahead that is ahead of it. So only a jump forward is
        // a new access pattern. Discarding the queue for every lagging request
        // cost 4.19x the cpu of one decode per delivered frame on `ReadAlpha`
        // with its two clips a few frames apart, against 1.28x with no pool.
        let sequential = state
            .last_index
            .is_some_and(|last| i64::from(index) <= i64::from(last) + 1);
        if !sequential {
            state.generation = state.generation.wrapping_add(1);
            state.queue.clear();
        }
        // The newest position is a high-water mark. Lowering it to a request
        // behind it would make the next forward step look like a seek even
        // though only one walk moved.
        state.last_index = Some(state.last_index.map_or(index, |last| last.max(index)));
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
        // A worker tests `stop` and then waits on the same mutex, so the store
        // and the notification are made with that mutex held: a worker that has
        // already tested `stop` would otherwise be able to enter `wait` after
        // the notification was delivered and sleep until the process exits.
        // Without the lock, freeing a clip hangs in `worker.join()`
        // intermittently; see `docs/improvements/43-hayro-jpeg-2000.md`.
        {
            let _state = self.lock();
            self.shared.stop.store(true, Ordering::Relaxed);
            self.shared.signal.notify_all();
        }
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
        let Some(victim) = behind(state, index) else {
            break;
        };
        freed = freed.saturating_add(frame_size(sizes, victim));
        release(state, victim);
    }
}

/// Oldest cached payload behind `index`, preferring one every clip has taken.
///
/// The lookahead needs room for a frame the consumer is about to ask for, so
/// what it may release is what is already behind it. A payload a second clip
/// still wants is the last thing to go, because releasing it is what makes that
/// clip decode the file again.
fn behind<P: Prepare>(state: &State<P>, index: i32) -> Option<i32> {
    let mut oldest = None;
    for &key in state.ready.keys() {
        if key >= index {
            break;
        }
        oldest.get_or_insert(key);
        if state.holds.get(&key).copied().unwrap_or(0) == 0 {
            return Some(key);
        }
    }
    oldest
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
    state.claimed.insert(index);
    true
}

/// Error for a frame index the sequence does not have.
fn out_of_range(index: i32, frames: usize) -> ImgSeqError {
    ImgSeqError::new(format!(
        "requested frame {index}, but the clip has {frames} frames"
    ))
}

fn worker<P: Prepare>(shared: &Shared<P>) {
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

        let result = match usize::try_from(index)
            .ok()
            .filter(|&index| index < shared.prepare.frames())
        {
            Some(index) => shared
                .prepare
                .produce(index)
                .map_err(|error| error.to_string()),
            None => Err(out_of_range(index, shared.prepare.frames()).to_string()),
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
        state.holds.insert(index, state.consumers);
    }
    state.ready.insert(index, result);
    // A stored payload ends whatever claim was open on its index, so the term
    // `committed_bytes` leaves out is only ever a decode still in progress.
    state.claimed.remove(&index);
    evict(state);
}

/// Records that one clip has taken the payload of `index`.
///
/// A payload at zero has been taken by every clip of the call, which is the one
/// eviction prefers to drop: it is kept only so that a later request can share
/// it.
fn note_handout<P: Prepare>(state: &mut State<P>, index: i32) {
    if let Some(holds) = state.holds.get_mut(&index) {
        *holds = holds.saturating_sub(1);
    }
}

/// Releases one cached payload, keeping the byte accounting in sync.
fn release<P: Prepare>(state: &mut State<P>, index: i32) {
    if let Some(Ok(payload)) = state.ready.remove(&index) {
        state.ready_bytes = state.ready_bytes.saturating_sub(payload.bytes());
    }
    state.holds.remove(&index);
}

/// Everything the pool holds or has committed to produce: the payloads cached
/// for a consumer, the payloads a worker is building, and the ones still queued.
///
/// A frame a consumer claimed is not one of them. The pool can neither evict it
/// nor refuse it, so counting it only turns the consumer's pipelining into the
/// pool's starvation: with more frames in flight than the budget holds,
/// `plan_window` would see a full budget and queue nothing, which is the state
/// [45](45-async-request-cache.md) measured four pool workers contributing 0.2%
/// in. A claim is open from `claim` until the payload is stored, so the term
/// left out here is only ever a decode still in progress.
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
            .filter(|&&index| !state.claimed.contains(&index))
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
        let Some(victim) = pick_victim(state) else {
            break;
        };
        release(state, victim);
    }
}

/// Chooses the payload eviction drops next.
///
/// A payload every clip of the call has already taken goes before one a second
/// clip still wants: the first is only kept so that a later request can share
/// it, and the second is the share [`Prepare::consumers`] exists for. Inside
/// each group the rule is unchanged -- the oldest payload behind the newest
/// request first, because consumers only move forward, and otherwise the
/// furthest one ahead, because it is the cheapest to decode again later.
fn pick_victim<P: Prepare>(state: &State<P>) -> Option<i32> {
    let oldest = *state.ready.keys().next()?;
    let newest = *state.ready.keys().next_back()?;
    let holds = |key: i32| state.holds.get(&key).copied().unwrap_or(0);
    let preferred = match state.last_index {
        Some(last) if oldest < last => oldest,
        _ => newest,
    };
    if holds(preferred) == 0 {
        return Some(preferred);
    }
    state
        .ready
        .keys()
        .copied()
        .find(|&key| holds(key) == 0)
        .or(Some(preferred))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet, VecDeque};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use crate::layout::{ColorType, Orientation, SourceColorType};

    use super::{
        DEFAULT_BYTE_BUDGET, Payload, Prefetcher, Prepare, READY_ENTRY_MARGIN, State,
        automatic_budget, committed_bytes, note_handout, plan_window, store,
    };
    use crate::decoder::{self, DecodeTimings, DecodedImage, Demand, ImageInfo, Pixels};
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
                decoder::probe(&fixture(name), true, false)
                    .expect("the fixture is a supported image")
            })
            .collect::<Vec<_>>()
            .into()
    }

    /// An image that is never read, to size budgets in tests.
    fn synthetic(width: u32, height: u32, color_type: ColorType) -> ImageInfo {
        ImageInfo {
            route: None,
            subimage: None,
            path: PathBuf::from("synthetic"),
            width,
            height,
            color_type,
            original_color_type: SourceColorType::from(color_type),
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
            claimed: HashSet::new(),
            ready: BTreeMap::new(),
            consumers: 1,
            holds: BTreeMap::new(),
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
    /// only exercised from a graph. One file is one output frame here, which is
    /// what a sequence of stills is.
    struct Decode {
        images: Arc<[ImageInfo]>,
        /// How many clips of one call take each payload; one file is one frame.
        consumers: usize,
    }

    impl Decode {
        fn of(images: Arc<[ImageInfo]>) -> Self {
            Self {
                images,
                consumers: 1,
            }
        }

        fn named(names: &[&str]) -> Self {
            Self::of(images(names))
        }

        fn sharing(mut self, consumers: usize) -> Self {
            self.consumers = consumers;
            self
        }
    }

    impl Payload for Arc<DecodedImage> {
        fn bytes(&self) -> usize {
            match &self.pixels {
                Pixels::Interleaved { buffer, .. } => buffer.len(),
                Pixels::Planar { planes, alpha } => {
                    planes.iter().map(Vec::len).sum::<usize>() + alpha.as_ref().map_or(0, Vec::len)
                }
                // One buffer, so one buffer's worth: the planes are inside it.
                Pixels::Strided { buffer, .. } => buffer.len(),
                // A stream holds no pixels of its own, so this synthetic
                // payload has nothing to report for one.
                Pixels::Stream(_) => 0,
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

        fn frames(&self) -> usize {
            self.images.len()
        }

        fn consumers(&self) -> usize {
            self.consumers
        }

        fn demand(&self) -> Demand {
            Demand::ALL
        }

        fn estimate(&self, index: usize) -> usize {
            self.images.get(index).map_or(0, bytes)
        }

        fn produce(&self, index: usize) -> Result<Arc<DecodedImage>> {
            let image = self.images.get(index).ok_or_else(|| {
                super::out_of_range(i32::try_from(index).unwrap_or(i32::MAX), self.images.len())
            })?;
            let decoded = decoder::decode(image, Demand::ALL)?;
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
                buffer: vec![0; bytes].into(),
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
            let prefetcher = Prefetcher::new(Decode::named(&["gray.pgm"]), workers, None);
            let first = prefetcher.fetch(0).expect("the fixture decodes");
            // Another clip asking for the same frame must reuse the decode
            // instead of reading the file a second time.
            let second = prefetcher.fetch(0).expect("the cached frame is reused");
            assert!(Arc::ptr_eq(&first, &second), "workers={workers}");
        }
    }

    #[test]
    fn keeps_frames_of_a_variable_sequence_apart() {
        let prefetcher = Prefetcher::new(Decode::named(&["gray.pgm", "rgb.ppm"]), 0, None);
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
        let mut info = decoder::probe(&source, true, false).expect("the fixture probes");
        info.path = temp.clone();

        let prefetcher = Prefetcher::new(Decode::of(vec![info].into()), 0, None);
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
            automatic_budget(&[bytes(&small)], 6, 1),
            DEFAULT_BYTE_BUDGET,
            "payloads that fit the floor do not move it"
        );

        let large = synthetic(3672, 5274, ColorType::Rgb8);
        assert_eq!(bytes(&large), 3672 * 5274 * 3);
        let sizes = [bytes(&large)];
        assert_eq!(
            automatic_budget(&sizes, 6, 1),
            bytes(&large) * 6,
            "a deep window on large payloads raises the budget"
        );
        assert_eq!(
            automatic_budget(&sizes, 0, 1),
            DEFAULT_BYTE_BUDGET,
            "without lookahead only the floor is needed"
        );
    }

    #[test]
    fn a_budget_too_small_for_one_frame_still_delivers() {
        let prefetcher = Prefetcher::new(Decode::named(&["gray.pgm", "rgb.ppm"]), 2, Some(1));
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
            claimed: HashSet::new(),
            ready: BTreeMap::new(),
            consumers: 1,
            holds: BTreeMap::new(),
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

    #[test]
    fn a_claimed_frame_is_not_the_pools_to_account_for() {
        // A consumer that claimed a frame holds it until it stores the payload,
        // so those bytes are the graph's and must not stop the pool from
        // planning ahead of them.
        let sizes = [10, 20, 30];
        let mut state = state_with(2, 30);
        state.in_flight = HashSet::from([1]);
        state.claimed = HashSet::from([1]);
        assert_eq!(
            committed_bytes(&state, &sizes),
            0,
            "a claimed frame is the caller's to hold"
        );

        state.claimed.clear();
        assert_eq!(
            committed_bytes(&state, &sizes),
            20,
            "a frame a worker is decoding is the pool's"
        );
    }

    #[test]
    fn a_claimed_frame_does_not_stop_the_lookahead() {
        // The budget `plan_window` walks is the one `committed_bytes` reports, so
        // a frame a consumer is decoding must not count as the pool's own.
        let sizes = [10, 10, 10, 10];
        let mut state = state_with(2, 15);
        state.in_flight = HashSet::from([0]);
        state.claimed = HashSet::from([0]);

        plan_window(&mut state, &sizes, 0);

        assert!(
            state.queue.iter().any(|&(_, index)| index == 1),
            "the lookahead plans past a frame the consumer is decoding"
        );
    }

    #[test]
    fn a_budget_follows_the_consumers_of_a_call() {
        let sizes = [64 * 1024 * 1024];
        assert_eq!(automatic_budget(&sizes, 4, 1), 4 * 64 * 1024 * 1024);
        assert_eq!(
            automatic_budget(&sizes, 4, 2),
            8 * 64 * 1024 * 1024,
            "a call of two clips asks for every frame twice"
        );
        assert_eq!(
            automatic_budget(&sizes, 0, 2),
            DEFAULT_BYTE_BUDGET,
            "without lookahead only the floor is needed, whatever the clip count"
        );
    }

    #[test]
    fn a_two_clip_call_caches_two_clips_worth() {
        let one = Prefetcher::new(Decode::named(&["gray.pgm"]), 2, None);
        let two = Prefetcher::new(Decode::named(&["gray.pgm"]).sharing(2), 2, None);
        let window = 2 + 2;
        assert_eq!(
            two.lock().entry_cap - one.lock().entry_cap,
            window,
            "a call of two clips keeps twice the window worth of payloads"
        );
    }

    #[test]
    fn a_payload_a_second_clip_still_wants_is_the_last_one_dropped() {
        // Two clips take every payload. Frame 1 has been taken by both, frame 0
        // has only been taken by the first, so eviction must drop frame 1 even
        // though frame 0 is the older one.
        let mut state = state_with(4, usize::MAX);
        state.consumers = 2;
        state.entry_cap = 2;
        store(&mut state, 0, Ok(Arc::new(decoded(10))));
        store(&mut state, 1, Ok(Arc::new(decoded(10))));
        note_handout(&mut state, 1);
        note_handout(&mut state, 1);
        state.last_index = Some(2);

        store(&mut state, 2, Ok(Arc::new(decoded(10))));

        assert!(
            !state.ready.contains_key(&1),
            "a payload every clip has taken goes first"
        );
        assert!(
            state.ready.contains_key(&0),
            "a payload a second clip still wants stays"
        );
    }

    #[test]
    fn a_clip_that_lags_behind_is_not_a_new_access_pattern() {
        // `ReadAlpha` hands out two clips, and a graph that consumes them as
        // separate branches asks for one frame from each, a few frames apart.
        // That is one walk, so it must not reset the lookahead, and it must
        // not move the newest position backwards.
        let prefetcher = Prefetcher::new(Decode::named(&["gray.pgm", "rgb.ppm"]), 4, None);
        prefetcher.plan(10);
        let generation = prefetcher.lock().generation;

        prefetcher.plan(2);
        assert_eq!(
            prefetcher.lock().last_index,
            Some(10),
            "the lagging clip does not lower the mark"
        );
        prefetcher.plan(11);
        assert_eq!(
            prefetcher.lock().generation,
            generation,
            "a lagging clip then one step forward are the same walk"
        );

        prefetcher.plan(20);
        assert_ne!(
            prefetcher.lock().generation,
            generation,
            "a jump forward is a new access pattern"
        );
    }
}
