//! Feature-gated App measurement hooks and serialized run aggregation.

use super::*;

/// Take attempt `attempt` at checkpoint `index`'s memory sample; true when every pane was measured.
#[cfg(feature = "perf-hook-checkpoint-memory")]
pub(super) fn sample_checkpoint_memory(
    app: &mut App,
    index: usize,
    label: &str,
    attempt: u32,
) -> bool {
    app.__perf_checkpoint_memory(index, label, attempt).complete
}

/// This build has no checkpoint-memory hook, so its checkpoints never sample and this is never called.
#[cfg(not(feature = "perf-hook-checkpoint-memory"))]
pub(super) fn sample_checkpoint_memory(
    _app: &mut App,
    _index: usize,
    _label: &str,
    _attempt: u32,
) -> bool {
    false
}

/// Install the App's counting progress surface, which both compared trees define.
#[cfg(perf_taskbar_progress_api)]
pub(super) fn install_counting_taskbar_progress(app: &mut App) {
    app.__install_counting_taskbar_progress();
}

/// `perf_taskbar_progress_api` is off, so a tree in this comparison lacks the counting surface;
/// no surface is installed and the `program` phase runs without its delivery checks.
#[cfg(not(perf_taskbar_progress_api))]
pub(super) fn install_counting_taskbar_progress(_app: &mut App) {}

/// What the counting surface received since the last take, timed from its first delivery.
#[cfg(perf_taskbar_progress_api)]
pub(super) fn take_taskbar_deliveries(app: &mut App) -> Option<Vec<scenarios::Delivered>> {
    let taken = app.__take_taskbar_progress_deliveries();
    let first = taken.first().map(|(at, _)| *at);
    Some(
        taken
            .into_iter()
            .map(|(at, percent)| scenarios::Delivered {
                at_ms: first.map_or(0, |first| at.duration_since(first).as_millis() as u64),
                percent,
            })
            .collect(),
    )
}

/// `perf_taskbar_progress_api` is off, so there is no record to read.
#[cfg(not(perf_taskbar_progress_api))]
pub(super) fn take_taskbar_deliveries(_app: &mut App) -> Option<Vec<scenarios::Delivered>> {
    None
}

/// The main renderer's attempt, presentation, row-cache and shaping counts from its window's counter
/// record, its glyph atlas resets and its atlas dimension; `None` when any counter field is missing.
#[cfg(feature = "perf-counters")]
pub(super) fn atlas_retry_counts(app: &App) -> Option<Counts> {
    let window_id = app.main_window()?.id();
    let snapshot = app.frame_counters_snapshot()?;
    let record = &snapshot.windows.iter().find(|(id, _)| *id == window_id)?.1;
    let renderer = app.main_renderer()?;
    Some(Counts {
        attempts: record.count("render_attempts")?,
        presented: record.count("render_attempts_presented")?,
        resets: renderer.__test_glyph_atlas_resets(),
        hits: record.count("row_cache_hits")?,
        misses: record.count("row_cache_misses")?,
        shapes: record.count("shape_requests")?,
        atlas_dim: renderer.glyph_atlas_facts().dim,
    })
}

/// This build has no counter API, so the episodes cannot be observed.
#[cfg(not(feature = "perf-counters"))]
pub(super) fn atlas_retry_counts(_app: &App) -> Option<Counts> {
    None
}

/// The counters a tab-title change moves: the main window's title-cache misses, and the App's completed
/// foreground-worker batches and stale foreground results; each `None` when it cannot be read.
#[cfg(feature = "perf-counters")]
pub(super) fn atlas_retry_title_counters(app: &App) -> (Option<u64>, Option<u64>, Option<u64>) {
    let Some(snapshot) = app.frame_counters_snapshot() else {
        return (None, None, None);
    };
    let window = app.main_window().map(|window| window.id());
    let prepares = snapshot
        .windows
        .iter()
        .find(|(id, _)| Some(*id) == window)
        .and_then(|(_, record)| record.count("tab_title_prepares"));
    (prepares, snapshot.app.count("fg_worker_probes"), snapshot.app.count("fg_results_stale"))
}

/// This build has no counter API, so none of the title counters can be read.
#[cfg(not(feature = "perf-counters"))]
pub(super) fn atlas_retry_title_counters(_app: &App) -> (Option<u64>, Option<u64>, Option<u64>) {
    (None, None, None)
}

/// Change the glyph atlas during `renderer`'s next assembly, so that frame retries.
#[cfg(perf_atlas_retry_api)]
pub(super) fn change_atlas_during_next_assembly(renderer: &mut GpuRenderer) {
    renderer.__change_glyph_atlas_during_next_assembly();
}

/// Without `perf_atlas_retry_api` the driver never starts, so nothing is armed.
#[cfg(not(perf_atlas_retry_api))]
pub(super) fn change_atlas_during_next_assembly(_renderer: &mut GpuRenderer) {}

/// Window `window_id`'s active tab title, its renderer's fallback notice id and how many characters
/// the last frame drew as missing, in the grid and in chrome; `None` when any cannot be read.
#[cfg(perf_atlas_retry_api)]
pub(super) fn atlas_retry_scene_parts(
    app: &App,
    window_id: WindowId,
) -> Option<(String, u64, usize)> {
    let title = app.__test_window_active_tab_title(window_id)?;
    let renderer = app.main_renderer()?;
    let missing = renderer.last_missing_tofu().len() + renderer.last_missing_chrome().len();
    Some((title, renderer.font_fallback_notice_id()?, missing))
}

/// Without `perf_atlas_retry_api` the scene cannot be read.
#[cfg(not(perf_atlas_retry_api))]
pub(super) fn atlas_retry_scene_parts(
    _app: &App,
    _window_id: WindowId,
) -> Option<(String, u64, usize)> {
    None
}

/// Frames the main window's renderer applied a font fallback in, cumulative; `None` when the
/// counter cannot be read.
#[cfg(feature = "perf-counters")]
pub(super) fn atlas_retry_fallback_applies(app: &App) -> Option<u64> {
    let window_id = app.main_window()?.id();
    let snapshot = app.frame_counters_snapshot()?;
    snapshot.windows.iter().find(|(id, _)| *id == window_id)?.1.count("font_fallback_applies")
}

/// This build has no counter API, so the fallback state cannot be read.
#[cfg(not(feature = "perf-counters"))]
pub(super) fn atlas_retry_fallback_applies(_app: &App) -> Option<u64> {
    None
}

/// Call the App's covered-window trim hook for `window_id`, with the trim number a trim reported;
/// only a tree that declares the hook builds this.
#[cfg(feature = "perf-hook-trim")]
pub(super) fn trim_covered(app: &mut App, window_id: WindowId) -> (TrimHookOutcome, Option<u64>) {
    use sonicterm_app::app::TrimDecision;
    match app.__trim_covered_now(window_id) {
        TrimDecision::Trimmed { trim_seq } => (TrimHookOutcome::Trimmed, Some(trim_seq)),
        TrimDecision::Skipped(_) => (TrimHookOutcome::Skipped, None),
        TrimDecision::Unsupported => (TrimHookOutcome::Unsupported, None),
    }
}

/// This build has no trim hook, so the covered window is never trimmed and the run is a baseline.
#[cfg(not(feature = "perf-hook-trim"))]
pub(super) fn trim_covered(_app: &mut App, _window_id: WindowId) -> (TrimHookOutcome, Option<u64>) {
    (TrimHookOutcome::Unsupported, None)
}

impl Probe {
    /// Open the App's counter gate for a `--counters` run, before any window or pane exists.
    /// Then the App's gate must match the reported mode, or the run is void.
    #[cfg(feature = "perf-counters")]
    pub(super) fn enable_counters(&mut self) -> Result<(), String> {
        if self.counters_mode == CountersMode::On {
            // the run counts, so the gate is forced on before the first window.
            crate::counters::enable(&mut self.app)?;
        }
        let gate_on = crate::counters::gate_on(&self.app);
        if let Some(reason) = crate::counters::gate_mismatch(self.counters_mode, gate_on) {
            // the App's gate disagrees with the report, so the run's numbers are not its own.
            self.counter_error.get_or_insert(reason);
        }
        Ok(())
    }
}

impl Probe {
    /// Without the counter API, `--counters` was already refused while parsing.
    #[cfg(not(feature = "perf-counters"))]
    pub(super) fn enable_counters(&mut self) -> Result<(), String> {
        Ok(())
    }
}

impl Probe {
    /// The App's counter totals now, when this run counts. A snapshot the contract cannot hold
    /// becomes the run's invalid reason instead of a reported value.
    #[cfg(feature = "perf-counters")]
    pub(super) fn counter_totals(&mut self) -> Option<CounterTotals> {
        if self.counters_mode != CountersMode::On {
            // When: `counters_mode` is not On, this run reports no counters.
            return None;
        }
        match crate::counters::snapshot_totals(&self.app)? {
            Ok(totals) => Some(totals),
            Err(reason) => {
                self.counter_error.get_or_insert(reason);
                None
            }
        }
    }
}

impl Probe {
    /// Without the counter API a run reports no counters.
    #[cfg(not(feature = "perf-counters"))]
    pub(super) fn counter_totals(&mut self) -> Option<CounterTotals> {
        None
    }
}

impl Probe {
    /// The run's result; a run that ended early keeps its partial phases and samples.
    pub(super) fn result(&mut self, settled: bool) -> RunResult {
        self.close_sample(UnattributedReason::NoCandidate);
        self.finish_meter();
        let (mut status, mut invalid_reason) = self.outcome.take().unwrap_or_else(|| {
            (Status::Invalid, Some("the event loop ended before the run finished".to_owned()))
        });
        if let (Status::Valid, Some(reason)) = (status, self.counter_error.take()) {
            // a counter snapshot the contract cannot hold voids the run rather than report a guess.
            status = Status::Invalid;
            invalid_reason = Some(format!("frame counters: {reason}"));
        }
        // The same measurements progress.json records, copied rather than taken.
        let measured = self.measurements();
        RunResult {
            harness_hash: self.request.harness_hash.clone(),
            scenario: self.plan.scenario,
            variant: self.plan.variant,
            managed: self.request.managed,
            short: self.request.short,
            laps: self.request.laps,
            alloc_counting: self.allocation_counter.is_some(),
            status,
            invalid_reason,
            harness_pid: std::process::id(),
            grid: self.grid,
            monitor: self.monitor.clone(),
            window_path: "production",
            synthetic_occlusion: self.synthetic_occlusion,
            trim_hook: self.trim_hook,
            trim_experiment: self.plan.trim_experiment,
            trim_seq_after_hook: self.trim_seq_after_hook,
            atlas_recovery: self.atlas_recovery.clone(),
            native_focus_events_dropped: self.native_focus_dropped,
            native_cursor_rest_events_dropped: self.native_pointer.rest_dropped(),
            finish_session_settled: settled,
            frame_counters: self.counters_mode,
            presenter: self.presenter.clone(),
            phases: measured.phases.to_vec(),
            latency: measured.latency.map(|samples| samples.to_vec()),
            throughput: measured.throughput,
            uncover_ms: measured.uncover_ms,
            scrollback_rows_retained: measured.scrollback_rows_retained,
            checkpoints: measured.checkpoints.to_vec(),
            notes: self.notes(),
        }
    }
}

impl Probe {
    /// Conditions a reader needs to interpret this run's numbers.
    pub(super) fn notes(&self) -> Vec<String> {
        let mut notes = vec![
            "The example runs the same App, renderer, PTY and VT as the shipping binary; platform main() setup (menus, crash markers, breadcrumb recording) is outside the measurement on both sides.".to_owned(),
            "A measurement ends when the dispatch returns, not at scanout.".to_owned(),
            "Hypothesis: the first about_to_wait after the first present fills the default warm pool of one; the pool is not observable from outside the crate.".to_owned(),
            "The window takes focus only from a synthetic Focused(true) before setup; native Focused events are recorded and dropped.".to_owned(),
            "Completion is proven only by the sentinel in the parsed grid; done/<role> files are never read.".to_owned(),
        ];
        if self.has_typing() {
            notes.push("Typing is a labelled proxy: one Ime::Commit per character, which skips keymap lookup and key encoding.".to_owned());
        }
        if !self.request.managed {
            notes.push("Unmanaged: the probe acknowledged its own session records, so this run never enters a comparison.".to_owned());
        }
        if self.allocation_counter.is_some() {
            notes.push("allocations_per_frame counts alloc, alloc_zeroed and realloc calls in the whole process, every thread, during each RedrawRequested dispatch.".to_owned());
        }
        if self.request.laps {
            notes.push("Laps run: logging at debug adds a render_timing line per frame, so it is never pooled with timed runs.".to_owned());
        }
        if self.plan.roles.iter().any(|role| matches!(role, Workload::Frames { .. })) {
            notes.push("Frames are paced by sleep 0.016 between writes, so slightly fewer than 60 arrive each second.".to_owned());
        }
        if self.throughput.is_some() {
            notes.push("Throughput counts workload output bytes before the terminal turns LF into CR LF, from GO to the sentinel seen in the grid.".to_owned());
        }
        if self.plan.scrollback_rows > 1_000 {
            notes.push(format!(
                "Scrollback asks for {} rows; the per-pane cell budget clamps it, and scrollback_rows_retained records what was kept.",
                self.plan.scrollback_rows
            ));
        }
        if self.synthetic_occlusion {
            notes.push(if waits::occlusion_wait_applies(scenarios::BUILD_HOST) {
                "No native occlusion change arrived within 2 s, so a synthetic one was delivered."
                    .to_owned()
            } else {
                "This host reports no occlusion, so the trim experiment delivered Occluded(true) \
                 and Occluded(false) synthetically."
                    .to_owned()
            });
        }
        if let Some(note) = &self.foreground_lock_failure {
            notes.push(note.clone());
        }
        notes
    }
}

impl Probe {
    /// The finalization of the run: the partial phase, its transport if none ran yet, and the outcome. False
    /// when the run had already finished, which changes nothing.
    pub(super) fn finalize_run(&mut self, status: Status, reason: Option<String>) -> bool {
        if self.outcome.is_some() {
            return false;
        }
        match &reason {
            Some(reason) => {
                tracing::warn!(target: LOG_TARGET, status = status.as_str(), %reason, "perf_scenarios run ended early");
            }
            None => tracing::info!(target: LOG_TARGET, "perf_scenarios run completed"),
        }
        self.close_sample(UnattributedReason::NoCandidate);
        self.driver = DriverState::None;
        self.finish_meter();
        // A phase finalized by end_phase and not yet transported (an early finish inside it) is transported here.
        self.transport_guard_correlation();
        self.cover = None;
        self.checkpoint_pending = None;
        self.outcome = Some((status, reason));
        self.stage = Stage::Done;
        true
    }
}

/// The main renderer's glyph completeness checkpoint, in a build with `perf_completeness_api`.
#[cfg(perf_completeness_api)]
pub(super) fn renderer_completeness(renderer: &GpuRenderer) -> CompletenessRecord {
    let scale = f64::from(renderer.scale_factor());
    completeness_from_checkpoint(renderer.completeness_checkpoint(), scale)
}

/// Without `perf_completeness_api` the renderer may have no checkpoint, so none is read.
#[cfg(not(perf_completeness_api))]
pub(super) fn renderer_completeness(renderer: &GpuRenderer) -> CompletenessRecord {
    crate::record::completeness_api_disabled(f64::from(renderer.scale_factor()))
}

/// The frame texture's bytes for a checkpoint: width x height x 4 at `end`, only with
/// `perf-frame-texture`; any other checkpoint, or a build without the feature, records none.
pub(super) fn checkpoint_frame_texture_bytes(
    label: &str,
    extent: Option<(u32, u32)>,
) -> Option<u64> {
    if !cfg!(feature = "perf-frame-texture") || label != "end" {
        // When: the feature is off or the label is not `end`, the field is left out.
        return None;
    }
    extent.map(|(width, height)| u64::from(width) * u64::from(height) * 4)
}
