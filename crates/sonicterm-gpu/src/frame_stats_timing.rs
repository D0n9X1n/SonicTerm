//! Gated renderer work timers, attempt scope and growth episode observations.

use super::*;

impl WorkTimer {
    /// Start timing `work`; with the gate off this reads one flag and no clock.
    pub(super) fn start(work: TimedWork) -> Self {
        if !COLLECTING.with(Cell::get) {
            // When: `COLLECTING` is false, no counting renderer is drawing, so nothing is timed.
            return Self { work, started_ns: None, counted: false };
        }
        let depth = TIMING_DEPTH.with(|cell| cell.replace(cell.get() + 1));
        let started_ns = (depth == 0).then(now_ns);
        Self { work, started_ns, counted: true }
    }
}

impl<'inner, Inner: Rasterizer + ?Sized> CountingRasterizer<'inner, Inner> {
    /// Wrap `inner`.
    pub(crate) fn new(inner: &'inner mut Inner) -> Self {
        Self { inner }
    }
}

/// Count one rasterizer call, and a tile when `drawn`.
pub(super) fn note_raster_call(drawn: bool) {
    record(|stats| {
        stats.raster_calls += 1;
        stats.raster_tiles += u64::from(drawn);
    });
    note_attempt(|attempt| {
        attempt.raster_calls += 1;
        attempt.raster_tiles += u64::from(drawn);
    });
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

/// Count one tab title drawn: from the title cache when `reused`, otherwise shaped on a miss.
pub(crate) fn note_tab_title(reused: bool) {
    record(|stats| {
        if reused {
            stats.tab_title_reuses += 1;
        } else {
            // When: `reused` is false, the title was fitted and shaped on a miss.
            stats.tab_title_prepares += 1;
        }
    });
}

/// Count one chrome-run lookup: served from the cache when `reused`, otherwise shaped.
pub(crate) fn note_chrome_run(reused: bool) {
    record(|stats| {
        if reused {
            stats.chrome_run_reuses += 1;
        } else {
            // When: `reused` is false, the lookup shaped its run.
            stats.chrome_run_prepares += 1;
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
pub(super) fn record_assembly_us(elapsed_us: u64) {
    let bucket = ASSEMBLY_BOUNDS_US
        .iter()
        .position(|bound| elapsed_us <= *bound)
        .unwrap_or(ASSEMBLY_BOUNDS_US.len());
    record(|stats| {
        stats.assembly_buckets[bucket] += 1;
        stats.assembly_sum_us += elapsed_us;
    });
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
}

impl GrowthEpisodes {
    /// At a successful present, record the pending episode's growth-to-present time.
    pub(crate) fn present(&mut self) {
        if let Some(pending_since) = self.pending_since.take() {
            note_atlas_growth_presented(pending_since);
        }
    }
}

impl GrowthEpisodes {
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
pub(super) fn record_growth_to_present_us(elapsed_us: u64) {
    let bucket = GROWTH_TO_PRESENT_BOUNDS_MS
        .iter()
        .position(|bound_ms| elapsed_us <= bound_ms * 1_000)
        .unwrap_or(GROWTH_TO_PRESENT_BOUNDS_MS.len());
    record(|stats| {
        stats.atlas_growth_to_present_buckets[bucket] += 1;
        stats.atlas_growth_to_present_sum_us += elapsed_us;
    });
}

#[cfg(test)]
#[path = "frame_stats_timing_tests.rs"]
mod frame_stats_timing_tests;
