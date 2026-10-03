//! Debug-only renderer statistics.
//!
//! A renderer counts only while its `counting` flag is set, which its App sets once when it
//! creates the renderer. Its entry points open a [`CollectGuard`]; notes taken under a counting
//! guard land in a thread-local collector that the renderer drains into its cumulative
//! [`FrameStats`]. Free functions and pipelines therefore count without holding the renderer.
//! With the flag off, notes read one thread-local flag and write nothing.

use std::cell::Cell;

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
}

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
    }
}

thread_local! {
    /// Whether the innermost open renderer entry point counts.
    static COLLECTING: Cell<bool> = const { Cell::new(false) };
    /// Notes taken since the last drain.
    static PENDING: Cell<FrameStats> = const { Cell::new(FrameStats::ZERO) };
}

/// A renderer entry point's collection scope; it restores the enclosing scope when dropped.
pub(crate) struct CollectGuard {
    /// The enclosing scope's flag, when this guard replaced it.
    enclosing: Option<bool>,
}

impl CollectGuard {
    /// Open a scope for a renderer whose counting flag is `counting`.
    pub(crate) fn enter(counting: bool) -> Self {
        let enclosing = COLLECTING.with(Cell::get);
        if !counting && !enclosing {
            // When: neither this renderer nor an enclosing one counts, nothing is written.
            return Self { enclosing: None };
        }
        COLLECTING.with(|cell| cell.set(counting));
        Self { enclosing: Some(enclosing) }
    }
}

// Lifecycle: CollectGuard restores the enclosing scope's flag, so a nested renderer never leaks it.
impl Drop for CollectGuard {
    fn drop(&mut self) {
        if let Some(enclosing) = self.enclosing.take() {
            COLLECTING.with(|cell| cell.set(enclosing));
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

/// Record one frame's damaged share of the surface, in permille.
pub(crate) fn note_damage(permille: u64) {
    record(|stats| {
        stats.damage_permille_sum += permille;
        stats.damaged_frames += 1;
    });
}

/// Count one drawn frame by its path.
pub(crate) fn note_frame(software: bool) {
    record(|stats| {
        if software {
            stats.software_frames += 1;
        } else {
            // When: software rendering is not degraded, the frame took the GPU path.
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

/// Take and clear everything collected on this thread since the last drain.
pub(crate) fn drain() -> FrameStats {
    PENDING.with(|cell| cell.replace(FrameStats::ZERO))
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
