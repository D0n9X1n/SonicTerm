//! Window and App counter aggregation, line emission and retirement.

use super::*;

impl WindowFrameCounters {
    /// Count one settled attempt; a presented frame also records the interval since the last one.
    pub(in crate::app) fn record_settlement(
        &mut self,
        outcome: super::super::redraw::FrameSettlement,
        now: Instant,
    ) {
        use super::super::redraw::FrameSettlement;
        match outcome {
            FrameSettlement::Presented => {
                self.presented += 1;
                if let Some(previous) = self.last_presented {
                    // an earlier frame presented, the interval between the two is recorded.
                    self.present_interval.record_us(micros_between(previous, now));
                }
                self.last_presented = Some(now);
            }
            FrameSettlement::Cached => self.cached += 1,
            FrameSettlement::Settled => self.settled += 1,
            FrameSettlement::Retry(_) => self.retry += 1,
            FrameSettlement::SurfaceRetry(_) => self.surface_retry += 1,
            FrameSettlement::Stopped(_) => self.stopped += 1,
            FrameSettlement::Failed => self.failed += 1,
        }
    }

    /// Take one drawn pane's pending flush; its age to this redraw is one observation.
    pub(crate) fn take_flush(&mut self, slot: &AtomicU64, now_ns: u64) {
        if let Some(age_ns) = consume_flush(slot, now_ns) {
            // the pane had a pending flush, its age to this redraw is one observation.
            self.flush_to_redraw.record_us(age_ns / 1_000);
        }
    }

    /// Record one `window_event` dispatch's duration.
    pub(crate) fn note_handler(&mut self, elapsed_us: u64) {
        self.handler.record_us(elapsed_us);
    }

    /// Whether a frame attempt or a flush observation happened since the last line, read from
    /// two plain counts so a redraw decides without building a record.
    pub(crate) fn redraw_activity_since_line(&self) -> bool {
        self.attempts > self.line_attempts || self.flush_to_redraw.count() > self.line_flushes
    }

    /// Remember the counts a line just printed.
    pub(super) fn mark_line(&mut self) {
        self.line_attempts = self.attempts;
        self.line_flushes = self.flush_to_redraw.count();
    }

    /// Count one serviced output event (`PaneOutput` or `RequestRedraw`) for the window.
    pub(crate) fn note_user_request(&mut self) {
        self.user_request_redraw += 1;
    }

    /// Count the rule that deferred a redraw.
    pub(crate) fn note_defer(&mut self, rule: DeferRule) {
        match rule {
            DeferRule::Timeout => self.defer_timeout += 1,
            DeferRule::Contention => self.defer_contention += 1,
            DeferRule::Sync => self.defer_sync += 1,
            DeferRule::Streaming => self.defer_streaming += 1,
        }
    }

    /// Count a frame collection that found a visible lock busy.
    pub(crate) fn note_contention(&mut self, images: bool) {
        if images {
            self.contention_images += 1;
        } else {
            // When: `images` is false, the busy lock was a visible parser, not an image store.
            self.contention_parser += 1;
        }
    }
}

impl CounterRecord {
    /// The count named `name`, if the record has it.
    pub fn count(&self, name: &str) -> Option<u64> {
        self.counts.iter().find(|(field, _)| *field == name).map(|(_, value)| *value)
    }

    /// Events in the histogram named `name`, if the record has it.
    pub fn histogram_count(&self, name: &str) -> Option<u64> {
        self.histogram(name).map(Histogram::count)
    }

    /// The exact microsecond sum of the histogram named `name`, if the record has it.
    pub fn histogram_sum_us(&self, name: &str) -> Option<u64> {
        self.histogram(name).map(Histogram::sum_us)
    }

    pub(super) fn histogram(&self, name: &str) -> Option<&Histogram> {
        self.histograms.iter().find(|(field, _)| *field == name).map(|(_, histogram)| histogram)
    }

    /// Append a count.
    pub(crate) fn push_count(&mut self, name: &'static str, value: u64) {
        self.counts.push((name, value));
    }

    /// Append a histogram.
    pub(crate) fn push_histogram(&mut self, name: &'static str, histogram: Histogram) {
        self.histograms.push((name, histogram));
    }

    /// Add `other`'s counts and histograms, matching fields by name.
    pub(crate) fn merge(&mut self, other: &Self) {
        for (name, value) in &other.counts {
            match self.counts.iter_mut().find(|entry| entry.0 == *name) {
                Some(entry) => entry.1 += value,
                None => self.counts.push((name, *value)),
            }
        }
        for (name, histogram) in &other.histograms {
            match self.histograms.iter_mut().find(|entry| entry.0 == *name) {
                Some(entry) => entry.1.add(histogram),
                None => self.histograms.push((name, histogram.clone())),
            }
        }
    }

    /// Record one `value_us` observation in the histogram named `name`, adding it in `unit`
    /// when the record has none.
    pub(crate) fn record_histogram_us(
        &mut self,
        name: &'static str,
        unit: HistogramUnit,
        value_us: u64,
    ) {
        match self.histograms.iter_mut().find(|entry| entry.0 == name) {
            Some(entry) => entry.1.record_us(value_us),
            None => {
                let mut histogram = Histogram::new(unit);
                histogram.record_us(value_us);
                self.histograms.push((name, histogram));
            }
        }
    }

    /// What grew since `earlier`, an older record of the same source.
    pub(crate) fn delta_since(&self, earlier: &Self) -> Self {
        let counts = self
            .counts
            .iter()
            .map(|(name, value)| (*name, value.saturating_sub(earlier.count(name).unwrap_or(0))))
            .collect();
        let histograms = self
            .histograms
            .iter()
            .map(|(name, histogram)| {
                let delta = earlier
                    .histogram(name)
                    .map_or_else(|| histogram.clone(), |before| histogram.delta_since(before));
                (*name, delta)
            })
            .collect();
        Self { counts, histograms }
    }

    /// The histogram named `name` with its unit, bounds, used bucket counts and exact sum.
    #[doc(hidden)]
    pub fn histogram_buckets(&self, name: &str) -> Option<HistogramBuckets<'_>> {
        self.histogram(name).map(Histogram::buckets)
    }

    /// The record's nonzero fields on a line.
    pub(super) fn line_fields(&self) -> String {
        let counts = self
            .counts
            .iter()
            .filter(|(_, value)| *value > 0)
            .map(|(name, value)| format!("{name}={value}"));
        let histograms =
            self.histograms.iter().map(|(name, histogram)| histogram.line_fields(name));
        counts.chain(histograms).filter(|field| !field.is_empty()).collect::<Vec<_>>().join(" ")
    }
}

impl LineState {
    /// A source that opened at `now`.
    pub(crate) fn new(now: Instant) -> Self {
        Self { cadence: LineCadence::default(), previous: CounterRecord::default(), since: now }
    }

    /// Whether a line could print at `now`; nothing is allocated to find out.
    pub(crate) fn ready(&self, now: Instant) -> bool {
        self.cadence.ready(now)
    }

    /// The line for `current` at `now`, at most one a second and only when `authorized`.
    pub(crate) fn line(
        &mut self,
        label: &str,
        current: &CounterRecord,
        now: Instant,
        authorized: bool,
    ) -> Option<String> {
        if !self.cadence.line_due(now, authorized) {
            // When: `line_due` refuses, the source printed within a second, only maintains, or is closed.
            return None;
        }
        Some(self.render(label, "", current, now))
    }

    /// The `final=1` line at close or exit, when counts are pending; nothing prints after it.
    pub(crate) fn final_line(
        &mut self,
        label: &str,
        current: &CounterRecord,
        now: Instant,
    ) -> Option<String> {
        let pending = !current.delta_since(&self.previous).line_fields().is_empty();
        if !self.cadence.final_line(pending) {
            // When: nothing is pending, or the final line already printed, the source just closes.
            return None;
        }
        Some(self.render(label, " final=1", current, now))
    }

    pub(super) fn render(
        &mut self,
        label: &str,
        marker: &str,
        current: &CounterRecord,
        now: Instant,
    ) -> String {
        let delta = current.delta_since(&self.previous);
        let span_ms = now.saturating_duration_since(self.since).as_millis();
        self.previous = current.clone();
        self.since = now;
        format!("window={label}{marker} span_ms={span_ms} {}", delta.line_fields())
            .trim_end()
            .to_owned()
    }
}

/// Print one counter line.
pub(super) fn log_line(line: &str) {
    tracing::debug!(target: "frame_counters", "[frame_counters] {line}");
}

impl AppFrameCounters {
    /// Print a retired window's `final=1` line and move its totals to `closed_windows`.
    pub(super) fn retire(
        &mut self,
        counters: &mut WindowFrameCounters,
        stats: Option<sonicterm_gpu::frame_stats::FrameStats>,
        native: u64,
    ) {
        let record = counters.record(stats, native);
        if let Some(line) = counters.line.final_line(&counters.label(), &record, Instant::now()) {
            log_line(&line);
        }
        self.closed_windows.merge(&record);
    }

    /// Finalize the window a dispatch closed, now that its handler time is recorded.
    pub(super) fn finish_closing(&mut self) {
        if let Some(mut closing) = self.closing.take() {
            // a dispatch closed a window, so it retires now with that dispatch's time.
            self.retire(&mut closing.counters, closing.stats, closing.native);
        }
    }

    /// Counters for a window registering now.
    pub(crate) fn window_counters(&mut self, main: bool) -> Box<WindowFrameCounters> {
        self.windows_registered += 1;
        Box::new(WindowFrameCounters {
            ordinal: self.windows_registered,
            main,
            ..WindowFrameCounters::default()
        })
    }

    /// The App's cumulative counts: VT statistics, dispatch waits and probes, wakes and stalls.
    // Ordering: each value loads Relaxed; a record is observational and orders nothing.
    pub(crate) fn record(&self) -> CounterRecord {
        let mut record = CounterRecord::default();
        let (vt, dispatch) = (&self.vt, &self.dispatch);
        for (name, value) in [
            ("parse_bytes", &vt.parse_bytes),
            ("batches", &vt.batches),
            ("flushes", &vt.flushes),
            ("flushes_untargeted", &vt.flushes_untargeted),
            ("flushes_coalesced", &vt.flushes_coalesced),
            ("flushes_suppressed", &vt.flushes_suppressed),
            ("sync_timeouts", &vt.sync_timeouts),
            ("ui_parser_locks", &dispatch.locks),
            ("parser_lock_wait_ns", &vt.parser_lock_wait_ns),
            ("parser_sections", &vt.parser_sections),
            ("ui_guard_custody_ns", &self.custody.custody_ns),
            ("ui_guard_custodies", &self.custody.custodies),
            ("frame_dispatch_ns", &self.custody.dispatch_ns),
            ("frame_dispatches", &self.custody.dispatches),
            ("frame_dispatch_failed_ns", &self.custody.dispatch_failed_ns),
            ("frame_dispatches_failed", &self.custody.dispatches_failed),
            ("fg_worker_probes", &self.fg_worker.probes),
            ("fg_worker_panes", &self.fg_worker.panes),
            ("fg_results_stale", &self.fg_worker.stale),
        ] {
            record.push_count(name, value.load(Ordering::Relaxed));
        }
        for (name, value) in [
            // The event loop no longer probes; these stay as real zeros so a comparison against a
            // base that probed there shows that work falling to zero.
            ("fg_probe_calls", 0),
            ("fg_probe_panes", 0),
            ("native_request_redraw_unregistered", dispatch.unregistered_requests()),
            ("wake_init", self.wake_init),
            ("wake_poll", self.wake_poll),
            ("wake_wait_cancelled", self.wake_wait_cancelled),
            ("wake_resume_time", self.wake_resume_time),
            ("wake_user", self.wake_user),
            ("hover_lookups", self.hover.lookups),
            ("hover_detections", self.hover.detections),
            ("hover_cache_hits", self.hover.cache_hits),
            ("hover_rows_read", self.hover.rows_read),
            ("path_probes_submitted", self.hover.probes_submitted),
            ("quick_select_rows_scanned", self.hover.quick_select_rows_scanned),
            ("status_line_samples", self.status_line.samples),
            ("status_line_wakes", self.status_line.wakes),
            ("status_line_redraws", self.status_line.redraws),
            ("status_line_sel_counts", self.status_line.sel_counts),
            ("status_line_sel_cells", self.status_line.sel_cells),
            ("status_line_history_shows", self.status_line.history_shows),
            ("status_line_held_shows", self.status_line.held_shows),
            ("status_line_glyph_shows", self.status_line.glyph_shows),
            ("program_progress_events", self.status_line.program_progress),
            ("program_notify_events", self.status_line.program_notifies),
            ("program_toast_redraws", self.status_line.program_toast_redraws),
            ("taskbar_progress_deliveries", self.status_line.taskbar_deliveries),
            ("status_line_tree_builds", self.status_line.tree_builds),
            ("status_line_full_passes", self.status_line.full_passes),
            ("startup_window_ns", self.startup.window_ns),
            ("startup_adapter_install_ns", self.startup.adapter_install_ns),
            ("startup_renderer_ns", self.startup.renderer_ns),
            ("startup_load_checks_ns", self.startup.load_checks_ns),
            ("startup_pane_spawn_ns", self.startup.pane_spawn_ns),
        ] {
            record.push_count(name, value);
        }
        for (name, histogram) in [
            ("parser_lock_wait", vt.parser_lock_wait.snapshot()),
            ("parser_lock_hold", vt.parser_lock_hold.snapshot()),
            ("parse", vt.parse.snapshot()),
            ("ui_parser_wait", dispatch.wait.snapshot()),
            ("fg_probe", Histogram::new(HistogramUnit::Micros)),
            ("fg_worker_probe", self.fg_worker.probe.snapshot()),
            ("about_to_wait", self.about_to_wait.clone()),
            ("user_event", self.user_event.clone()),
            ("new_events", self.new_events.clone()),
        ] {
            record.push_histogram(name, histogram);
        }
        record
    }
}

impl WindowFrameCounters {
    /// The window's line label: `main`, or `child-N` by registration order.
    pub(crate) fn label(&self) -> String {
        if self.main {
            "main".to_owned()
        } else {
            // When: `main` is false, the window is numbered by registration order.
            format!("child-{}", self.ordinal)
        }
    }

    /// The window's cumulative counts, with its renderer's statistics when it has a renderer.
    /// `native_requests` are the App's native redraw requests for the window; the renderer's
    /// own requests add to them.
    pub(crate) fn record(
        &self,
        renderer: Option<sonicterm_gpu::frame_stats::FrameStats>,
        native_requests: u64,
    ) -> CounterRecord {
        let renderer_requests = renderer.map_or(0, |stats| stats.native_request_redraw);
        let mut record = CounterRecord::default();
        for (name, value) in [
            ("attempts", self.attempts),
            ("presented", self.presented),
            ("cached", self.cached),
            ("settled", self.settled),
            ("retry", self.retry),
            ("surface_retry", self.surface_retry),
            ("stopped", self.stopped),
            ("failed", self.failed),
            ("contention_parser", self.contention_parser),
            ("contention_images", self.contention_images),
            ("defer_timeout", self.defer_timeout),
            ("defer_contention", self.defer_contention),
            ("defer_sync", self.defer_sync),
            ("defer_streaming", self.defer_streaming),
            ("stream_clock_exempt", self.stream_clock_exempt),
            ("display_link_ticks", self.display_link_ticks),
            ("display_link_admissions", self.display_link_admissions),
            ("display_link_fallbacks", self.display_link_fallbacks),
            ("contention_retry_armed", self.contention_retry_armed),
            ("dirt_ack_dropped", self.dirt_ack_dropped),
            ("native_request_redraw", native_requests + renderer_requests),
            ("user_request_redraw", self.user_request_redraw),
            ("redraw_requested", self.redraw_requested),
        ] {
            record.push_count(name, value);
        }
        for (name, histogram) in [
            ("present_interval", &self.present_interval),
            ("handler", &self.handler),
            ("flush_to_redraw", &self.flush_to_redraw),
        ] {
            record.push_histogram(name, histogram.clone());
        }
        if let Some(stats) = renderer {
            // the window has a renderer, its statistics join the window's record.
            for (name, value) in [
                ("vertex_bytes", stats.vertex_bytes),
                ("index_bytes", stats.index_bytes),
                ("damage_permille_sum", stats.damage_permille_sum),
                ("damaged_frames", stats.damaged_frames),
                // Union-rect waste in permille; damaged_frames is its denominator.
                ("damage_waste_permille_sum", stats.damage_waste_permille_sum),
                ("software_frames", stats.software_frames),
                ("gpu_frames", stats.gpu_frames),
                ("row_cache_hits", stats.row_cache_hits),
                ("row_cache_misses", stats.row_cache_misses),
                ("shape_requests", stats.shape_requests),
                ("full_frames", stats.full_frames),
                // Presented partial frames, partial plans reassembled Full, and hashed row cells.
                ("partial_frames", stats.partial_frames),
                ("partial_fallbacks", stats.partial_fallbacks),
                ("row_cells_hashed", stats.row_cells_hashed),
                ("row_cache_invalidate_visits", stats.row_cache_invalidate_visits),
                ("row_cache_invalidate_us", stats.row_cache_invalidate_us),
                ("recolor_glyphs_visited", stats.recolor_glyphs_visited),
                ("font_fallback_applies", stats.font_fallback_applies),
                ("glyph_atlas_growths", stats.glyph_atlas_growths),
                ("atlas_growth_abandoned", stats.atlas_growth_abandoned),
                // Attempt and preparation timings are nanoseconds, kept as plain counts.
                ("shape_ns", stats.shape_ns),
                ("raster_ns", stats.raster_ns),
                ("raster_calls", stats.raster_calls),
                ("raster_tiles", stats.raster_tiles),
                ("font_generation_applies", stats.font_generation_applies),
                ("font_prepare_ns", stats.font_prepare_ns),
                ("font_generation_prepare_ns", stats.font_generation_prepare_ns),
                // Kept tab titles and chrome runs drawn without shaping, and those shaped on a
                // miss; the misses' requests also count in `shape_requests`.
                ("tab_title_reuses", stats.tab_title_reuses),
                ("tab_title_prepares", stats.tab_title_prepares),
                ("chrome_run_reuses", stats.chrome_run_reuses),
                ("chrome_run_prepares", stats.chrome_run_prepares),
                ("render_attempts", stats.attempts.attempts),
                ("render_attempts_presented", stats.attempts.presented),
                ("render_attempt_ns", stats.attempts.attempt_ns),
                ("render_attempt_shape_ns", stats.attempts.shape_ns),
                ("render_attempt_raster_ns", stats.attempts.raster_ns),
                ("render_attempt_shape_requests", stats.attempts.shape_requests),
                ("render_attempt_raster_calls", stats.attempts.raster_calls),
                ("render_attempt_raster_tiles", stats.attempts.raster_tiles),
                ("apply_attempts", stats.apply_attempts.attempts),
                ("apply_attempts_presented", stats.apply_attempts.presented),
                ("apply_attempt_ns", stats.apply_attempts.attempt_ns),
                ("apply_attempt_shape_ns", stats.apply_attempts.shape_ns),
                ("apply_attempt_raster_ns", stats.apply_attempts.raster_ns),
                ("apply_attempt_shape_requests", stats.apply_attempts.shape_requests),
                ("apply_attempt_raster_calls", stats.apply_attempts.raster_calls),
                ("apply_attempt_raster_tiles", stats.apply_attempts.raster_tiles),
            ] {
                record.push_count(name, value);
            }
            let assembly = Histogram::from_micros(&stats.assembly_buckets, stats.assembly_sum_us);
            record.push_histogram("assembly", assembly);
            let growth = Histogram::from_millis(
                &stats.atlas_growth_to_present_buckets,
                stats.atlas_growth_to_present_sum_us,
            );
            record.push_histogram("atlas_growth_to_present", growth);
        }
        record
    }
}

impl super::super::App {
    /// This App's counters, read field by field; `None` when its gate is off. Nothing is reset.
    #[doc(hidden)]
    pub fn frame_counters_snapshot(&self) -> Option<FrameCountersSnapshot> {
        let app = self.frame_counters.as_ref()?;
        let windows = self
            .windows
            .iter()
            .filter_map(|(id, window)| {
                let counters = window.redraw.frame_counters.as_deref()?;
                let stats =
                    window.renderer.as_ref().map(sonicterm_gpu::core::GpuRenderer::frame_stats);
                Some((*id, counters.record(stats, app.dispatch.native_requests(*id))))
            })
            .collect();
        // A window its own dispatch is closing already counts as closed, so no total dips.
        let mut closed_windows = app.closed_windows.clone();
        if let Some(closing) = &app.closing {
            // a window is between retirement and its handler time; its totals are closed ones.
            closed_windows.merge(&closing.counters.record(closing.stats, closing.native));
        }
        Some(FrameCountersSnapshot { app: app.record(), windows, closed_windows })
    }

    /// Print the lines a `window_event` or `user_event` authorizes. `window` names the window
    /// and whether the event was its `RedrawRequested`, which authorizes a line only when it
    /// led to a frame attempt or took a flush.
    pub(in crate::app) fn emit_frame_lines(
        &mut self,
        window: Option<(winit::window::WindowId, bool)>,
    ) {
        if self.frame_counters.is_none() {
            // When: `frame_counters` is None, the gate is off; no clock is read and nothing prints.
            return;
        }
        self.emit_frame_lines_at(window, Instant::now());
    }
}

#[cfg(test)]
#[path = "frame_counters_record_tests.rs"]
mod frame_counters_record_tests;
