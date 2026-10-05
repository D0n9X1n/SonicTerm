//! Debug-only renderer statistics.
//!
//! A renderer counts only once its App gives it a
//! [`FrameStatsSink`](crate::frame_stats::FrameStatsSink), before it draws. Each
//! public entry point that can shape or draw opens a `CollectGuard` for that renderer's sink;
//! notes taken under it land in a thread-local collector that the guard moves into the sink
//! when it closes, so a scope's notes always belong to the renderer that opened it, however
//! renderers interleave on one thread. Free functions and pipelines therefore count without
//! holding the renderer. With no sink, notes read one thread-local flag and write nothing.

use std::cell::Cell;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use sonicterm_render_model::geometry::PixelRect;
use sonicterm_text::glyph_atlas::{RasterTile, Rasterizer};
use sonicterm_types::GlyphKey;

/// Counts and times inside one class of render attempts; all durations are nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttemptStats {
    /// `render_releasing` calls in this class.
    pub attempts: u64,
    /// Of those, the ones that presented.
    pub presented: u64,
    /// Time inside those calls.
    pub attempt_ns: u64,
    /// Time inside shaping and measuring requests made during them, face merging included.
    pub shape_ns: u64,
    /// Time inside rasterizer calls made during them, glyph-zero resolution included.
    pub raster_ns: u64,
    /// Shaping and measuring requests made during them.
    pub shape_requests: u64,
    /// Rasterizer calls made during them; a call can return no tile.
    pub raster_calls: u64,
    /// Rasterizer calls that returned a tile with pixels.
    pub raster_tiles: u64,
}

impl AttemptStats {
    /// All zero.
    pub const ZERO: Self = Self {
        attempts: 0,
        presented: 0,
        attempt_ns: 0,
        shape_ns: 0,
        raster_ns: 0,
        shape_requests: 0,
        raster_calls: 0,
        raster_tiles: 0,
    };

    /// Add `other`'s counts to these.
    pub fn add(&mut self, other: &Self) {
        self.attempts += other.attempts;
        self.presented += other.presented;
        self.attempt_ns += other.attempt_ns;
        self.shape_ns += other.shape_ns;
        self.raster_ns += other.raster_ns;
        self.shape_requests += other.shape_requests;
        self.raster_calls += other.raster_calls;
        self.raster_tiles += other.raster_tiles;
    }
}

/// Cumulative renderer statistics; nobody resets them, so readers take deltas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Bytes written to the vertex buffer.
    pub vertex_bytes: u64,
    /// Bytes written to the index buffer.
    pub index_bytes: u64,
    /// Sum of each frame's damaged share of the surface, in permille.
    pub damage_permille_sum: u64,
    /// Frames whose damage was recorded.
    pub damaged_frames: u64,
    /// Sum of each damaged frame's waste, in permille: the share of its single damage rectangle
    /// minus the share its damage parts exactly cover. Its denominator is `damaged_frames`.
    pub damage_waste_permille_sum: u64,
    /// Frames drawn while software rendering was degraded.
    pub software_frames: u64,
    /// Frames drawn on the GPU path.
    pub gpu_frames: u64,
    /// Row glyph cache lookups that hit.
    pub row_cache_hits: u64,
    /// Row glyph cache lookups that missed.
    pub row_cache_misses: u64,
    /// Font shaping and measuring requests, failures included.
    pub shape_requests: u64,
    /// Native redraw requests the renderer issued itself.
    pub native_request_redraw: u64,
    /// Frames whose render plan was `RenderMode::Full`.
    pub full_frames: u64,
    /// Presented frames whose render plan was `RenderMode::Partial`; counted after presentation.
    pub partial_frames: u64,
    /// Partial plans reassembled `Full` because the final damage reached a row they did not emit.
    pub partial_fallbacks: u64,
    /// Cells hashed into row glyph cache keys, one row per emitted terminal row.
    pub row_cells_hashed: u64,
    /// Row glyph cache entries examined to drop dirty rows. Kept for counter-contract
    /// compatibility: the content-keyed cache drops nothing for dirt, so it is always 0.
    pub row_cache_invalidate_visits: u64,
    /// Microseconds spent dropping dirty glyph rows. Kept for compatibility; always 0.
    pub row_cache_invalidate_us: u64,
    /// Glyphs `recolor_cursor_glyphs_in` examined on the frame's main glyph list.
    pub recolor_glyphs_visited: u64,
    /// Frames whose font preparation applied a newer fallback notice or generation.
    pub font_fallback_applies: u64,
    /// Assembled frames by CPU assembly time, per [`ASSEMBLY_BOUNDS_US`] bucket, overflow last.
    pub assembly_buckets: [u64; ASSEMBLY_BUCKETS],
    /// The exact sum of assembly times, in microseconds.
    pub assembly_sum_us: u64,
    /// Glyph atlas size doublings, counted once each at the end-of-frame check or at teardown.
    pub glyph_atlas_growths: u64,
    /// Growths whose next presented frame never came: device loss or teardown cleared them.
    pub atlas_growth_abandoned: u64,
    /// Growths by the time from the growing frame's start to the next presented frame, per
    /// [`GROWTH_TO_PRESENT_BOUNDS_MS`] bucket, overflow last.
    pub atlas_growth_to_present_buckets: [u64; GROWTH_TO_PRESENT_BUCKETS],
    /// The exact sum of growth-to-present times, in microseconds.
    pub atlas_growth_to_present_sum_us: u64,
    /// Nanoseconds inside every shaping and measuring request, rendering or not.
    pub shape_ns: u64,
    /// Nanoseconds inside every glyph-atlas rasterizer call.
    pub raster_ns: u64,
    /// Glyph-atlas rasterizer calls.
    pub raster_calls: u64,
    /// Glyph-atlas rasterizer calls that returned a tile with pixels.
    pub raster_tiles: u64,
    /// Preparations that applied a newer generation of the notice already applied.
    pub font_generation_applies: u64,
    /// Nanoseconds inside frame font preparation, invalidation included.
    pub font_prepare_ns: u64,
    /// Of `font_prepare_ns`, the preparations that applied a newer generation.
    pub font_generation_prepare_ns: u64,
    /// Every render attempt.
    pub attempts: AttemptStats,
    /// Render attempts that carried a fallback generation apply.
    pub apply_attempts: AttemptStats,
}

/// Upper bounds of the `atlas_growth_to_present_ms` buckets in milliseconds, the App's frame bounds.
pub const GROWTH_TO_PRESENT_BOUNDS_MS: [u64; 9] = [4, 7, 9, 12, 17, 25, 34, 50, 100];

/// Buckets of `atlas_growth_to_present_ms`: one per bound and one for the overflow.
pub const GROWTH_TO_PRESENT_BUCKETS: usize = GROWTH_TO_PRESENT_BOUNDS_MS.len() + 1;

/// Upper bounds of the `assembly_us` buckets in microseconds, the App's microsecond bounds.
pub const ASSEMBLY_BOUNDS_US: [u64; 6] = [10, 50, 100, 500, 1_000, 5_000];

/// Buckets of `assembly_us`: one per bound and one for the overflow.
pub const ASSEMBLY_BUCKETS: usize = ASSEMBLY_BOUNDS_US.len() + 1;

impl FrameStats {
    /// All zero.
    pub const ZERO: Self = Self {
        vertex_bytes: 0,
        index_bytes: 0,
        damage_permille_sum: 0,
        damaged_frames: 0,
        damage_waste_permille_sum: 0,
        software_frames: 0,
        gpu_frames: 0,
        row_cache_hits: 0,
        row_cache_misses: 0,
        shape_requests: 0,
        native_request_redraw: 0,
        full_frames: 0,
        partial_frames: 0,
        partial_fallbacks: 0,
        row_cells_hashed: 0,
        row_cache_invalidate_visits: 0,
        row_cache_invalidate_us: 0,
        recolor_glyphs_visited: 0,
        font_fallback_applies: 0,
        assembly_buckets: [0; ASSEMBLY_BUCKETS],
        assembly_sum_us: 0,
        glyph_atlas_growths: 0,
        atlas_growth_abandoned: 0,
        atlas_growth_to_present_buckets: [0; GROWTH_TO_PRESENT_BUCKETS],
        atlas_growth_to_present_sum_us: 0,
        shape_ns: 0,
        raster_ns: 0,
        raster_calls: 0,
        raster_tiles: 0,
        font_generation_applies: 0,
        font_prepare_ns: 0,
        font_generation_prepare_ns: 0,
        attempts: AttemptStats::ZERO,
        apply_attempts: AttemptStats::ZERO,
    };

    /// Add `other`'s counts to these.
    pub fn add(&mut self, other: &Self) {
        self.vertex_bytes += other.vertex_bytes;
        self.index_bytes += other.index_bytes;
        self.damage_permille_sum += other.damage_permille_sum;
        self.damaged_frames += other.damaged_frames;
        self.damage_waste_permille_sum += other.damage_waste_permille_sum;
        self.software_frames += other.software_frames;
        self.gpu_frames += other.gpu_frames;
        self.row_cache_hits += other.row_cache_hits;
        self.row_cache_misses += other.row_cache_misses;
        self.shape_requests += other.shape_requests;
        self.native_request_redraw += other.native_request_redraw;
        self.full_frames += other.full_frames;
        self.partial_frames += other.partial_frames;
        self.partial_fallbacks += other.partial_fallbacks;
        self.row_cells_hashed += other.row_cells_hashed;
        self.row_cache_invalidate_visits += other.row_cache_invalidate_visits;
        self.row_cache_invalidate_us += other.row_cache_invalidate_us;
        self.recolor_glyphs_visited += other.recolor_glyphs_visited;
        self.font_fallback_applies += other.font_fallback_applies;
        for (slot, count) in self.assembly_buckets.iter_mut().zip(other.assembly_buckets) {
            *slot += count;
        }
        self.assembly_sum_us += other.assembly_sum_us;
        self.glyph_atlas_growths += other.glyph_atlas_growths;
        self.atlas_growth_abandoned += other.atlas_growth_abandoned;
        for (slot, count) in self
            .atlas_growth_to_present_buckets
            .iter_mut()
            .zip(other.atlas_growth_to_present_buckets)
        {
            *slot += count;
        }
        self.atlas_growth_to_present_sum_us += other.atlas_growth_to_present_sum_us;
        self.shape_ns += other.shape_ns;
        self.raster_ns += other.raster_ns;
        self.raster_calls += other.raster_calls;
        self.raster_tiles += other.raster_tiles;
        self.font_generation_applies += other.font_generation_applies;
        self.font_prepare_ns += other.font_prepare_ns;
        self.font_generation_prepare_ns += other.font_generation_prepare_ns;
        self.attempts.add(&other.attempts);
        self.apply_attempts.add(&other.apply_attempts);
    }
}

/// A counting renderer's cumulative statistics, shared with the collection scopes it opens so
/// each scope can hand its notes to this renderer when it closes.
#[derive(Clone, Debug, Default)]
pub struct FrameStatsSink {
    stats: Arc<Mutex<FrameStats>>,
}

impl FrameStatsSink {
    /// The statistics so far; every closed scope's notes are already in.
    pub fn snapshot(&self) -> FrameStats {
        *self.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Add one closed scope's notes.
    fn absorb(&self, other: &FrameStats) {
        self.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner).add(other);
    }

    /// Count growths and abandoned growth timings found when an episode is finalized, outside
    /// any frame scope.
    pub(crate) fn note_teardown_growths(&self, growths: u64, abandoned: u64) {
        let mut stats = self.stats.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        stats.glyph_atlas_growths += growths;
        stats.atlas_growth_abandoned += abandoned;
    }

    /// This sink's identity: the address of its shared statistics, never 0.
    fn identity(&self) -> usize {
        Arc::as_ptr(&self.stats) as usize
    }

    /// Count one native redraw request the renderer issued.
    pub(crate) fn note_native_request(&self) {
        self.stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .native_request_redraw += 1;
    }
}

thread_local! {
    /// Whether the innermost open renderer entry point counts.
    static COLLECTING: Cell<bool> = const { Cell::new(false) };
    /// Notes taken since the last drain.
    static PENDING: Cell<FrameStats> = const { Cell::new(FrameStats::ZERO) };
    /// Open shaping and rasterizing timers; only the outermost one measures.
    static TIMING_DEPTH: Cell<u32> = const { Cell::new(0) };
    /// The open render attempt's notes; `None` outside an attempt of a counting renderer.
    static ATTEMPT: Cell<Option<AttemptStats>> = const { Cell::new(None) };
    /// The innermost counting scope's sink identity, 0 for none.
    static SCOPE_SINK: Cell<usize> = const { Cell::new(0) };
    /// Assembly time of the open frame's passes, recorded as one sample when the frame closes.
    static PENDING_ASSEMBLY_US: Cell<Option<u64>> = const { Cell::new(None) };
}

/// The attempt and timer state an isolated scope replaced, restored when it closes.
#[derive(Clone, Copy)]
struct IsolatedState {
    attempt: Option<AttemptStats>,
    timing_depth: u32,
}

/// A renderer entry point's collection scope. It starts an empty collector, and when dropped
/// moves what it collected into the renderer that opened it and restores the enclosing scope.
pub(crate) struct CollectGuard {
    /// The opening renderer's sink; `None` for a renderer that does not count.
    sink: Option<FrameStatsSink>,
    /// The enclosing scope's flag and pending notes, when this guard replaced them.
    enclosing: Option<(bool, FrameStats)>,
    /// The enclosing scope's sink identity, restored on drop.
    enclosing_sink: usize,
    /// The attempt and timer state this guard replaced; `None` when the same renderer re-entered.
    isolated: Option<IsolatedState>,
}

impl CollectGuard {
    /// Open a scope for a renderer whose sink is `sink`; `None` when it does not count.
    pub(crate) fn enter(sink: Option<&FrameStatsSink>) -> Self {
        let enclosing = COLLECTING.with(Cell::get);
        if sink.is_none() && !enclosing {
            // When: `sink` is None and no `enclosing` scope counts, nothing is written.
            return Self { sink: None, enclosing: None, enclosing_sink: 0, isolated: None };
        }
        let outer_pending = PENDING.with(|cell| cell.replace(FrameStats::ZERO));
        COLLECTING.with(|cell| cell.set(sink.is_some()));
        let sink_id = sink.map_or(0, FrameStatsSink::identity);
        let enclosing_sink = SCOPE_SINK.with(|cell| cell.replace(sink_id));
        // A helper of the renderer already counting keeps the open attempt and timers, so its
        // shaping joins that attempt; any other renderer starts from none and restores them.
        let isolated = (sink_id == 0 || sink_id != enclosing_sink).then(|| IsolatedState {
            attempt: ATTEMPT.with(|cell| cell.replace(None)),
            timing_depth: TIMING_DEPTH.with(|cell| cell.replace(0)),
        });
        Self {
            sink: sink.cloned(),
            enclosing: Some((enclosing, outer_pending)),
            enclosing_sink,
            isolated,
        }
    }
}

// Lifecycle: CollectGuard moves its notes into its sink and restores the enclosing scope's
// flag and pending notes, so no note reaches another renderer.
impl Drop for CollectGuard {
    fn drop(&mut self) {
        let Some((enclosing, outer_pending)) = self.enclosing.take() else {
            // When: `enclosing` is None, this scope replaced nothing and collected nothing.
            return;
        };
        let collected = PENDING.with(|cell| cell.replace(outer_pending));
        COLLECTING.with(|cell| cell.set(enclosing));
        SCOPE_SINK.with(|cell| cell.set(self.enclosing_sink));
        if let Some(state) = self.isolated.take() {
            // the scope isolated another renderer's attempt and timers; they resume as they were.
            ATTEMPT.with(|cell| cell.set(state.attempt));
            TIMING_DEPTH.with(|cell| cell.set(state.timing_depth));
        }
        if let Some(sink) = &self.sink {
            // the opening renderer counts, so its notes join its statistics.
            sink.absorb(&collected);
        }
    }
}

/// Apply `update` to the collector when a counting scope is open.
fn record(update: impl FnOnce(&mut FrameStats)) {
    if !COLLECTING.with(Cell::get) {
        // When: `COLLECTING` is false, no counting renderer is drawing on this thread; nothing is recorded.
        return;
    }
    PENDING.with(|cell| {
        let mut pending = cell.get();
        update(&mut pending);
        cell.set(pending);
    });
}

/// Count one `FontStack` shaping or measuring request, and in the open attempt.
pub(crate) fn note_shape_request() {
    record(|stats| stats.shape_requests += 1);
    note_attempt(|attempt| attempt.shape_requests += 1);
}

/// Apply `update` to the open render attempt, if a counting renderer has one.
fn note_attempt(update: impl FnOnce(&mut AttemptStats)) {
    ATTEMPT.with(|cell| {
        if let Some(mut attempt) = cell.get() {
            // the innermost scope belongs to a counting renderer drawing an attempt.
            update(&mut attempt);
            cell.set(Some(attempt));
        }
    });
}

/// Nanoseconds on this thread's counter clock: a monotonic process clock, or under test a
/// stepping clock when one is installed.
fn now_ns() -> u64 {
    #[cfg(test)]
    {
        if let Some(reading) = test_clock::read() {
            // When: a test installed a stepping clock, every reading is exact and counted.
            return reading;
        }
    }
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// A stepping clock for tests: each reading returns the current value and advances it.
#[cfg(test)]
pub(crate) mod test_clock {
    use std::cell::Cell;

    thread_local! {
        /// The next reading and the step after it; `None` when no clock is installed.
        static CLOCK: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
        /// Readings taken since the clock was installed.
        static READS: Cell<u64> = const { Cell::new(0) };
    }

    /// Install a clock that starts at `start_ns` and advances `step_ns` per reading.
    pub(crate) fn install(start_ns: u64, step_ns: u64) {
        CLOCK.with(|cell| cell.set(Some((start_ns, step_ns))));
        READS.with(|cell| cell.set(0));
    }

    /// Remove the clock.
    pub(crate) fn remove() {
        CLOCK.with(|cell| cell.set(None));
    }

    /// Readings taken since `install`.
    pub(crate) fn reads() -> u64 {
        READS.with(Cell::get)
    }

    /// Take one reading, or `None` with no clock installed.
    pub(super) fn read() -> Option<u64> {
        let (reading, step) = CLOCK.with(Cell::get)?;
        CLOCK.with(|cell| cell.set(Some((reading + step, step))));
        READS.with(|cell| cell.set(cell.get() + 1));
        Some(reading)
    }
}

/// What a timer measures.
#[derive(Clone, Copy)]
enum TimedWork {
    /// A shaping or measuring request.
    Shape,
    /// A rasterizer call.
    Raster,
}

/// One shaping or rasterizing timer. Only the outermost timer on the thread reads the clock, so
/// nested work is counted once, by the outer category.
struct WorkTimer {
    work: TimedWork,
    /// The start reading; `None` when not counting or nested inside another timer.
    started_ns: Option<u64>,
    /// Whether this timer raised `TIMING_DEPTH`.
    counted: bool,
}

impl WorkTimer {
    /// Start timing `work`; with the gate off this reads one flag and no clock.
    fn start(work: TimedWork) -> Self {
        if !COLLECTING.with(Cell::get) {
            // When: `COLLECTING` is false, no counting renderer is drawing, so nothing is timed.
            return Self { work, started_ns: None, counted: false };
        }
        let depth = TIMING_DEPTH.with(|cell| cell.replace(cell.get() + 1));
        let started_ns = (depth == 0).then(now_ns);
        Self { work, started_ns, counted: true }
    }
}

// Lifecycle: WorkTimer lowers TIMING_DEPTH and records the outermost timer's duration, on unwind
// too, so a panicking request never leaves later timers suppressed.
impl Drop for WorkTimer {
    fn drop(&mut self) {
        if self.counted {
            // this timer raised the depth when it started, so it lowers it.
            TIMING_DEPTH.with(|cell| cell.set(cell.get().saturating_sub(1)));
        }
        let Some(started_ns) = self.started_ns else {
            // When: `started_ns` is None, the timer was nested or not counting; nothing to add.
            return;
        };
        let elapsed_ns = now_ns().saturating_sub(started_ns);
        match self.work {
            TimedWork::Shape => {
                record(|stats| stats.shape_ns += elapsed_ns);
                note_attempt(|attempt| attempt.shape_ns += elapsed_ns);
            }
            TimedWork::Raster => {
                record(|stats| stats.raster_ns += elapsed_ns);
                note_attempt(|attempt| attempt.raster_ns += elapsed_ns);
            }
        }
    }
}

/// A rasterizer that counts and times each call of the one it wraps. Every glyph-atlas insertion
/// in this crate passes one, so no rasterization goes uncounted.
pub(crate) struct CountingRasterizer<'inner, Inner: Rasterizer + ?Sized> {
    inner: &'inner mut Inner,
}

impl<'inner, Inner: Rasterizer + ?Sized> CountingRasterizer<'inner, Inner> {
    /// Wrap `inner`.
    pub(crate) fn new(inner: &'inner mut Inner) -> Self {
        Self { inner }
    }
}

impl<Inner: Rasterizer + ?Sized> Rasterizer for CountingRasterizer<'_, Inner> {
    fn rasterize(&mut self, key: GlyphKey) -> Option<RasterTile> {
        let tile = {
            let _timer = WorkTimer::start(TimedWork::Raster);
            self.inner.rasterize(key)
        };
        let drawn = tile.as_ref().is_some_and(|tile| !tile.is_empty());
        note_raster_call(drawn);
        tile
    }
}

/// Count one rasterizer call, and a tile when `drawn`.
fn note_raster_call(drawn: bool) {
    record(|stats| {
        stats.raster_calls += 1;
        stats.raster_tiles += u64::from(drawn);
    });
    note_attempt(|attempt| {
        attempt.raster_calls += 1;
        attempt.raster_tiles += u64::from(drawn);
    });
}

/// A render entry's scopes in their only valid order: the renderer's collection scope, then the
/// attempt inside it. Fields drop in declaration order, so the attempt folds into the collector
/// before the collector closes into the renderer's sink.
pub(crate) struct RenderScope {
    _attempt: AttemptScope,
    _collect: CollectGuard,
}

impl RenderScope {
    /// Open the scopes for a renderer whose sink is `sink`, taking its owed fallback apply, so
    /// exactly one attempt carries each apply however often the frame's token is reused.
    pub(crate) fn enter(sink: Option<&FrameStatsSink>, owed_apply: &mut bool) -> Self {
        let collect = CollectGuard::enter(sink);
        let attempt = AttemptScope::enter(std::mem::take(owed_apply));
        Self { _attempt: attempt, _collect: collect }
    }
}

/// One render attempt's scope: it collects the attempt's notes and, when it closes, adds them to
/// every attempt and, for an attempt carrying a fallback apply, to the apply attempts.
pub(crate) struct AttemptScope {
    /// Whether the attempt carries a fallback generation apply.
    applied: bool,
    /// The start reading; `None` when the renderer does not count.
    started_ns: Option<u64>,
}

impl AttemptScope {
    /// Open an attempt; open it inside the renderer's `CollectGuard`. With the gate off this
    /// reads one flag and no clock.
    pub(crate) fn enter(applied: bool) -> Self {
        if !COLLECTING.with(Cell::get) {
            // When: `COLLECTING` is false, the renderer does not count and the attempt records nothing.
            return Self { applied, started_ns: None };
        }
        ATTEMPT.with(|cell| cell.set(Some(AttemptStats::ZERO)));
        Self { applied, started_ns: Some(now_ns()) }
    }
}

// Lifecycle: AttemptScope folds its attempt into the open collector on every exit, unwind included,
// then closes it; the CollectGuard restores any enclosing attempt.
impl Drop for AttemptScope {
    fn drop(&mut self) {
        let Some(started_ns) = self.started_ns else {
            // When: `started_ns` is None, the renderer does not count; nothing was opened.
            return;
        };
        let mut attempt = ATTEMPT.with(|cell| cell.replace(None)).unwrap_or(AttemptStats::ZERO);
        attempt.attempts = 1;
        attempt.presented = attempt.presented.min(1);
        attempt.attempt_ns = now_ns().saturating_sub(started_ns);
        let applied = self.applied;
        record(|stats| {
            stats.attempts.add(&attempt);
            if applied {
                // An attempt carrying a fallback apply also counts as an apply attempt.
                stats.apply_attempts.add(&attempt);
            }
        });
    }
}

/// Mark the open render attempt as presented; both presenters call it where a frame passes the
/// present boundary.
pub(crate) fn note_attempt_presented() {
    note_attempt(|attempt| attempt.presented = 1);
}

/// The start of a frame's font preparation; the clock is read only inside a counting scope.
pub(crate) fn prepare_clock() -> Option<u64> {
    COLLECTING.with(Cell::get).then(now_ns)
}

/// Add one font preparation's time from `started_ns`; `generation` when it applied a newer
/// generation. `None` (gate off) records nothing.
pub(crate) fn note_font_prepare(started_ns: Option<u64>, generation: bool) {
    if let Some(started_ns) = started_ns {
        // the preparation ran under a counting scope, so its clock pair closes here.
        let elapsed_ns = now_ns().saturating_sub(started_ns);
        record(|stats| {
            stats.font_prepare_ns += elapsed_ns;
            stats.font_generation_prepare_ns += if generation { elapsed_ns } else { 0 };
        });
    }
}

/// Count one preparation that applied a newer generation of the notice already applied.
pub(crate) fn note_font_generation_apply() {
    record(|stats| stats.font_generation_applies += 1);
}

/// Count the bytes one frame wrote to the vertex and index buffers.
pub(crate) fn note_buffer_writes(vertex_bytes: usize, index_bytes: usize) {
    record(|stats| {
        stats.vertex_bytes += vertex_bytes as u64;
        stats.index_bytes += index_bytes as u64;
    });
}

/// Record one frame's damaged share of the surface, in permille. `permille` runs only inside a
/// counting scope, so with the gate off the share is never computed.
pub(crate) fn note_damage(permille: impl FnOnce() -> u64) {
    record(|stats| {
        stats.damage_permille_sum += permille();
        stats.damaged_frames += 1;
    });
}

/// Record one damaged frame's waste in permille, beside [`note_damage`]. `waste` runs only inside
/// a counting scope, so with the gate off the exact cover is never computed.
pub(crate) fn note_damage_waste(waste: impl FnOnce() -> u64) {
    record(|stats| stats.damage_waste_permille_sum += waste());
}

/// Count one drawn frame by the presenter that drew it; `software` is [`presents_software`].
pub(crate) fn note_frame(software: bool) {
    record(|stats| {
        if software {
            stats.software_frames += 1;
        } else {
            // When: `software` is false, the frame went through the wgpu presenter.
            stats.gpu_frames += 1;
        }
    });
}

/// Count one row glyph cache lookup.
pub(crate) fn note_row_cache(hit: bool) {
    record(|stats| {
        if hit {
            stats.row_cache_hits += 1;
        } else {
            // When: `hit` is false, the lookup missed and the row is shaped again.
            stats.row_cache_misses += 1;
        }
    });
}

/// Count a frame whose render plan is `RenderMode::Full` when `full`; with the gate off this is
/// one check.
pub(crate) fn note_full_frame(full: bool) {
    record(|stats| stats.full_frames += u64::from(full));
}

/// Count one presented frame as partial when `partial`; called only once a frame presented.
pub(crate) fn note_partial_frame(partial: bool) {
    record(|stats| stats.partial_frames += u64::from(partial));
}

/// Count one partial plan reassembled as `Full` by the post-assembly check.
pub(crate) fn note_partial_fallback() {
    record(|stats| stats.partial_fallbacks += 1);
}

/// Count the cells one row hashed into its row-cache key. `cells` runs only inside a counting
/// scope, so with the gate off the row is never measured.
pub(crate) fn note_row_cells_hashed(cells: impl FnOnce() -> usize) {
    record(|stats| stats.row_cells_hashed += cells() as u64);
}

/// Count one frame whose font preparation applied a newer fallback notice or generation.
pub(crate) fn note_font_fallback_apply() {
    record(|stats| stats.font_fallback_applies += 1);
}

/// Count the glyphs one `recolor_cursor_glyphs_in` call examined on the main glyph list: rows
/// whose ink meets the target plus every glyph outside the recorded rows.
pub(crate) fn note_recolor_glyphs_visited(glyph_count: impl FnOnce() -> usize) {
    record(|stats| stats.recolor_glyphs_visited += glyph_count() as u64);
}

/// The start of a frame's CPU assembly; the clock is read only inside a counting scope.
pub(crate) fn assembly_clock() -> Option<Instant> {
    COLLECTING.with(Cell::get).then(Instant::now)
}

/// Record one assembled frame from `started`; `None` (gate off) records nothing.
pub(crate) fn note_assembly(started: Option<Instant>) {
    if let Some(started) = started {
        // the frame reached the end of assembly under a counting scope: one sample.
        note_assembly_us(micros_since(started));
    }
}

/// Add one assembly pass of `elapsed_us` to the open frame; with the gate off nothing is kept.
pub(crate) fn note_assembly_us(elapsed_us: u64) {
    if !COLLECTING.with(Cell::get) {
        // When: `COLLECTING` is false, no counting renderer is drawing, so the pass is not kept.
        return;
    }
    PENDING_ASSEMBLY_US.with(|pending| {
        pending.set(Some(pending.get().unwrap_or(0).saturating_add(elapsed_us)));
    });
}

/// Close the frame's assembly: its passes' summed time is one sample, so a frame assembled twice
/// by a partial fallback counts once at its real cost. A frame with no timed pass records nothing.
pub(crate) fn finish_assembly() {
    if let Some(elapsed_us) = PENDING_ASSEMBLY_US.with(Cell::take) {
        record_assembly_us(elapsed_us);
    }
}

/// Record one assembled frame that took `elapsed_us`; a value at a bound is in that bucket.
fn record_assembly_us(elapsed_us: u64) {
    let bucket = ASSEMBLY_BOUNDS_US
        .iter()
        .position(|bound| elapsed_us <= *bound)
        .unwrap_or(ASSEMBLY_BOUNDS_US.len());
    record(|stats| {
        stats.assembly_buckets[bucket] += 1;
        stats.assembly_sum_us += elapsed_us;
    });
}

/// One renderer's glyph atlas growth episodes: the growths already counted, and the start of
/// the first growing frame no successful present has completed yet.
#[derive(Debug, Default)]
pub(crate) struct GrowthEpisodes {
    /// Glyph atlas growths already added to the frame counters.
    counted_growths: u64,
    /// Start of the first frame that grew the atlas since the last successful present.
    pending_since: Option<Instant>,
}

impl GrowthEpisodes {
    /// At an end-of-frame check, count the growths `atlas_growths` gained since the last reading
    /// and start their timing at `frame_start`, unless an earlier growth's timing is pending.
    pub(crate) fn count(&mut self, atlas_growths: u64, frame_start: Instant) {
        let growths = atlas_growths.saturating_sub(self.counted_growths);
        if growths == 0 {
            // When: growths is zero the atlas kept its size since the last check; nothing to count.
            return;
        }
        self.counted_growths = atlas_growths;
        note_glyph_atlas_growths(growths);
        self.pending_since.get_or_insert(frame_start);
    }

    /// At a successful present, record the pending episode's growth-to-present time.
    pub(crate) fn present(&mut self) {
        if let Some(pending_since) = self.pending_since.take() {
            note_atlas_growth_presented(pending_since);
        }
    }

    /// End the open episode where no later frame can present it (device stop, rebind, a final
    /// read, teardown): add the growths `atlas_growths` gained since the last reading, and one
    /// abandoned episode if one was pending or uncounted, straight into `sink`, so it works
    /// outside any collection scope. Idempotent: a second call finds nothing left to add.
    pub(crate) fn finalize(&mut self, atlas_growths: u64, sink: Option<&FrameStatsSink>) {
        let growths = atlas_growths.saturating_sub(self.counted_growths);
        self.counted_growths = atlas_growths;
        let abandoned = u64::from(self.pending_since.take().is_some() || growths > 0);
        if growths == 0 && abandoned == 0 {
            // When: `growths` and `abandoned` are both zero, finalization already ran; add nothing.
            return;
        }
        if let Some(sink) = sink {
            sink.note_teardown_growths(growths, abandoned);
        }
    }
}

/// Count `growths` glyph atlas doublings found at an end-of-frame check.
pub(crate) fn note_glyph_atlas_growths(growths: u64) {
    record(|stats| stats.glyph_atlas_growths += growths);
}

/// Record the time from a growing frame's start, `pending_since`, to this successful present.
pub(crate) fn note_atlas_growth_presented(pending_since: Instant) {
    record_growth_to_present_us(micros_since(pending_since));
}

/// Record one growth-to-present time of `elapsed_us`; a value at a bound is in that bucket.
fn record_growth_to_present_us(elapsed_us: u64) {
    let bucket = GROWTH_TO_PRESENT_BOUNDS_MS
        .iter()
        .position(|bound_ms| elapsed_us <= bound_ms * 1_000)
        .unwrap_or(GROWTH_TO_PRESENT_BOUNDS_MS.len());
    record(|stats| {
        stats.atlas_growth_to_present_buckets[bucket] += 1;
        stats.atlas_growth_to_present_sum_us += elapsed_us;
    });
}

/// Whole microseconds since `started`.
fn micros_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// Whether a frame with software rendering degraded by `degrade` goes through the software
/// presenter. Only Windows has one; elsewhere a degraded frame still presents through wgpu.
pub(crate) fn presents_software(degrade: bool) -> bool {
    presents_software_on(cfg!(target_os = "windows"), degrade)
}

/// [`presents_software`] on a host that is Windows when `windows` is set.
pub(crate) fn presents_software_on(windows: bool, degrade: bool) -> bool {
    windows && degrade
}

/// Make one `FontStack` shaping or measuring request, counted once whether or not it succeeds.
/// Every such call in this crate goes through here, so no caller counts it again.
pub(crate) fn shape_request<Output>(shape: impl FnOnce() -> Output) -> Output {
    note_shape_request();
    let _timer = WorkTimer::start(TimedWork::Shape);
    shape()
}

/// `damage`'s share of a `width` by `height` surface in permille, at most 1000; 0 for no surface.
pub(crate) fn damage_permille(damage: &PixelRect, width: u32, height: u32) -> u64 {
    let surface = u64::from(width) * u64::from(height);
    if surface == 0 {
        // When: the surface has no area, no share can be computed.
        return 0;
    }
    let damaged = u64::from(damage.w) * u64::from(damage.h);
    (damaged * 1_000 / surface).min(1_000)
}

/// The waste of drawing `damage` as one rectangle, in permille of a `width` by `height` surface:
/// its share minus the share of the exact area `parts` cover. `parts` lie inside `damage`.
pub(crate) fn damage_waste_permille(
    damage: &PixelRect,
    parts: &[PixelRect],
    width: u32,
    height: u32,
) -> u64 {
    let surface = u64::from(width) * u64::from(height);
    if surface == 0 {
        // When: the surface has no area, no share can be computed.
        return 0;
    }
    let covered = sonicterm_render_model::geometry::covered_area(parts);
    let covered_permille = (covered * 1_000 / surface).min(1_000);
    damage_permille(damage, width, height).saturating_sub(covered_permille)
}

#[cfg(test)]
#[path = "frame_stats_tests.rs"]
mod frame_stats_tests;
