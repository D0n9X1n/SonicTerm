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
    /// Row glyph cache entries `invalidate_row_abs` examined: one per call, a keyed removal.
    pub row_cache_invalidate_visits: u64,
    /// Microseconds spent invalidating dirty rows; one clock pair per pane with a dirty row.
    pub row_cache_invalidate_us: u64,
    /// Glyphs `recolor_cursor_glyphs_in` examined on the frame's main glyph list.
    pub recolor_glyphs_visited: u64,
    /// Assembled frames by CPU assembly time, per [`ASSEMBLY_BOUNDS_US`] bucket, overflow last.
    pub assembly_buckets: [u64; ASSEMBLY_BUCKETS],
    /// The exact sum of assembly times, in microseconds.
    pub assembly_sum_us: u64,
}

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
        software_frames: 0,
        gpu_frames: 0,
        row_cache_hits: 0,
        row_cache_misses: 0,
        shape_requests: 0,
        native_request_redraw: 0,
        full_frames: 0,
        row_cache_invalidate_visits: 0,
        row_cache_invalidate_us: 0,
        recolor_glyphs_visited: 0,
        assembly_buckets: [0; ASSEMBLY_BUCKETS],
        assembly_sum_us: 0,
    };

    /// Add `other`'s counts to these.
    pub fn add(&mut self, other: &Self) {
        self.vertex_bytes += other.vertex_bytes;
        self.index_bytes += other.index_bytes;
        self.damage_permille_sum += other.damage_permille_sum;
        self.damaged_frames += other.damaged_frames;
        self.software_frames += other.software_frames;
        self.gpu_frames += other.gpu_frames;
        self.row_cache_hits += other.row_cache_hits;
        self.row_cache_misses += other.row_cache_misses;
        self.shape_requests += other.shape_requests;
        self.native_request_redraw += other.native_request_redraw;
        self.full_frames += other.full_frames;
        self.row_cache_invalidate_visits += other.row_cache_invalidate_visits;
        self.row_cache_invalidate_us += other.row_cache_invalidate_us;
        self.recolor_glyphs_visited += other.recolor_glyphs_visited;
        for (slot, count) in self.assembly_buckets.iter_mut().zip(other.assembly_buckets) {
            *slot += count;
        }
        self.assembly_sum_us += other.assembly_sum_us;
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
}

/// A renderer entry point's collection scope. It starts an empty collector, and when dropped
/// moves what it collected into the renderer that opened it and restores the enclosing scope.
pub(crate) struct CollectGuard {
    /// The opening renderer's sink; `None` for a renderer that does not count.
    sink: Option<FrameStatsSink>,
    /// The enclosing scope's flag and pending notes, when this guard replaced them.
    enclosing: Option<(bool, FrameStats)>,
}

impl CollectGuard {
    /// Open a scope for a renderer whose sink is `sink`; `None` when it does not count.
    pub(crate) fn enter(sink: Option<&FrameStatsSink>) -> Self {
        let enclosing = COLLECTING.with(Cell::get);
        if sink.is_none() && !enclosing {
            // When: `sink` is None and no `enclosing` scope counts, nothing is written.
            return Self { sink: None, enclosing: None };
        }
        let outer_pending = PENDING.with(|cell| cell.replace(FrameStats::ZERO));
        COLLECTING.with(|cell| cell.set(sink.is_some()));
        Self { sink: sink.cloned(), enclosing: Some((enclosing, outer_pending)) }
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

/// Count one `FontStack` shaping or measuring request.
pub(crate) fn note_shape_request() {
    record(|stats| stats.shape_requests += 1);
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

/// Count the row glyph cache entries one `invalidate_row_abs` call examines; a keyed removal
/// examines one. `visited` runs only inside a counting scope.
pub(crate) fn note_row_cache_invalidate_visits(visited: impl FnOnce() -> usize) {
    record(|stats| stats.row_cache_invalidate_visits += visited() as u64);
}

/// The start of one pane's row invalidation: read only inside a counting scope and only when
/// `dirty_rows` is at least one, so a pane that invalidates nothing reads no clock.
pub(crate) fn invalidation_clock(dirty_rows: impl FnOnce() -> usize) -> Option<Instant> {
    (COLLECTING.with(Cell::get) && dirty_rows() > 0).then(Instant::now)
}

/// Add one pane's invalidation time, measured from `started`, as plain microseconds.
pub(crate) fn note_row_cache_invalidate_us(started: Option<Instant>) {
    if let Some(started) = started {
        // the pane invalidated rows under a counting scope, so its clock pair closes here.
        let elapsed_us = micros_since(started);
        record(|stats| stats.row_cache_invalidate_us += elapsed_us);
    }
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
        record_assembly_us(micros_since(started));
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

#[cfg(test)]
#[path = "frame_stats_tests.rs"]
mod frame_stats_tests;
