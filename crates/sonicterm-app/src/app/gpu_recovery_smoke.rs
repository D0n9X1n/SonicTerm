//! Native shared-device replacement with original-shell and per-window presentation proof.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sonicterm_gpu::core::{live_renderer_count, PresentOutcome};
use sonicterm_gpu::device_errors::{DeviceErrorState, DeviceState, GpuFaultKind};
use sonicterm_gpu::recovery::{RecoveryPhase, RecoverySnapshot};
use sonicterm_io::pty::PtyChildExitProbe;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

use super::{grid_marker_rows, RuntimeSmokeFailure, RuntimeSmokePhase, RuntimeSmokeState};
use crate::app::{App, UserEvent};

const POLL_INTERVAL: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(24);
const QUIET_INTERVAL: Duration = Duration::from_millis(250);
const FAILURE: RuntimeSmokeFailure = RuntimeSmokeFailure::GpuDeviceRecovery;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Create,
    Capture,
    Baseline,
    Recovering,
    Quiet { until: Instant },
    Release,
}

struct PaneProof {
    window: WindowId,
    pane: u64,
    pid: u32,
    exit: PtyChildExitProbe,
    marker_rows: usize,
    presented_generation: Option<u64>,
}

/// Bounded native evidence for the original panes across exactly one device replacement.
pub(super) struct RecoveryProbe {
    started: Instant,
    stage: Stage,
    child: Option<WindowId>,
    original_generation: u64,
    recovered_generation: Option<u64>,
    stale_event_observed: bool,
    original_state: Option<Arc<DeviceErrorState>>,
    panes: Vec<PaneProof>,
}

impl RecoveryProbe {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            stage: Stage::Create,
            child: None,
            original_generation: 0,
            recovered_generation: None,
            stale_event_observed: false,
            original_state: None,
            panes: Vec::new(),
        }
    }
}

fn one_rebuild(snapshot: RecoverySnapshot, old: u64) -> bool {
    snapshot.committed != old
        && snapshot.phase == RecoveryPhase::Active
        && snapshot.counts.requests == 1
        && snapshot.counts.rebuilds == 1
        && snapshot.counts.failed_attempts == 0
}

impl RecoveryProbe {
    /// Accept only a visible fresh marker presented by a generation valid for the current proof stage.
    pub(super) fn observe_frame(
        &mut self,
        window: WindowId,
        generation: u64,
        panes: &[sonicterm_render_model::PaneRender<'_>],
        outcome: &PresentOutcome,
        marker: &str,
    ) {
        if !accepts_proof_generation(self.stage, self.original_generation, generation)
            || !matches!(outcome, PresentOutcome::Presented)
        {
            // When: `accepts_proof_generation` or `matches!` refuses, neither old nor unpresented pixels prove recovery.
            return;
        }
        for proof in self.panes.iter_mut().filter(|proof| proof.window == window) {
            if let Some(pane) = panes.iter().find(|pane| pane.id == proof.pane) {
                if grid_marker_rows(pane.grid, marker) > proof.marker_rows
                    && visible_marker(pane.grid, pane.viewport_top_abs, marker)
                {
                    proof.presented_generation = Some(generation);
                }
            }
        }
    }

    /// Observe delivery after the queued stale event passes through the real App handler.
    pub(super) fn observe_device_event(&mut self, generation: u64) {
        if matches!(self.stage, Stage::Quiet { .. }) && generation == self.original_generation {
            self.stale_event_observed = true;
        }
    }

    /// Distinguish a phase deadline from the earlier process watchdog without changing either bound.
    pub(super) fn report_timeout(&self, now: Instant, cause: &'static str) {
        tracing::error!(target: "sonic::gpu::recovery", cause, stage = ?self.stage,
            elapsed_ms = now.saturating_duration_since(self.started).as_millis(),
            "runtime smoke GPU recovery timeout");
    }
}

fn accepts_proof_generation(stage: Stage, original: u64, generation: u64) -> bool {
    match stage {
        Stage::Baseline => generation == original,
        Stage::Recovering => generation != original,
        Stage::Create | Stage::Capture | Stage::Quiet { .. } | Stage::Release => false,
    }
}

fn visible_marker(
    grid: &sonicterm_grid::grid::Grid,
    viewport_top_abs: Option<u64>,
    marker: &str,
) -> bool {
    let top = sonicterm_gpu::core::GpuRenderer::resolved_view_top_abs(grid, viewport_top_abs);
    (top..top + u64::from(grid.rows))
        .filter_map(|row| grid.row_at_abs(row))
        .any(|row| row.iter().map(|cell| cell.ch).collect::<String>().contains(marker))
}

impl App {
    /// Only the explicit recovery smoke arms this native-observation timer.
    pub(in crate::app) fn gpu_recovery_smoke_deadline(&self) -> Option<Instant> {
        self.runtime_smoke
            .as_ref()
            .filter(|smoke| smoke.phase == RuntimeSmokePhase::RecoveryReady)
            .map(|smoke| smoke.next_probe_at.unwrap_or_else(Instant::now))
    }

    /// Run the recovery oracle outside parser/render borrows and return only terminal completion.
    pub(in crate::app) fn drive_gpu_recovery_smoke(
        &mut self,
        el: &ActiveEventLoop,
        now: Instant,
    ) -> bool {
        if !self.runtime_smoke.as_ref().is_some_and(|smoke| {
            smoke.phase == RuntimeSmokePhase::RecoveryReady
                && smoke.next_probe_at.is_none_or(|due| now >= due)
        }) {
            // When: `runtime_smoke` has no due recovery phase, normal sessions gain no polling or native work.
            return false;
        }
        let mut smoke = self.runtime_smoke.take().expect("due recovery smoke remains installed");
        let mut probe = smoke.recovery_probe.take().unwrap_or_else(|| RecoveryProbe::new(now));
        let result = if now.saturating_duration_since(probe.started) >= DEADLINE {
            probe.report_timeout(now, "phase-deadline");
            Err(FAILURE)
        } else {
            // When: `saturating_duration_since` is below DEADLINE, advance only the due native stage.
            self.tick_gpu_recovery_smoke(el, &smoke, &mut probe, now)
        };
        let terminal = match result {
            Ok(true) => {
                smoke.phase = RuntimeSmokePhase::Complete;
                smoke.outcome = Some(Ok(()));
                true
            }
            Ok(false) => false,
            Err(failure) => {
                // Preserve the recovery boundary instead of falling back to warm-lifecycle success.
                smoke.fail(failure);
                true
            }
        };
        if terminal {
            tracing::warn!(target: "sonic::gpu::recovery", stage = ?probe.stage, outcome = ?smoke.outcome, "runtime smoke GPU recovery verdict");
        }
        smoke.recovery_probe = Some(probe);
        smoke.next_probe_at = Some(Instant::now() + POLL_INTERVAL);
        self.runtime_smoke = Some(smoke);
        terminal
    }

    fn tick_gpu_recovery_smoke(
        &mut self,
        el: &ActiveEventLoop,
        smoke: &RuntimeSmokeState,
        probe: &mut RecoveryProbe,
        now: Instant,
    ) -> Result<bool, RuntimeSmokeFailure> {
        match probe.stage {
            Stage::Create => {
                // When: `stage` is Create, use production window creation so the shell and renderer keep normal ownership.
                if self.windows.len() != 1 {
                    // When: `windows` contains another owner already, the two-window oracle has no controlled baseline.
                    return Err(FAILURE);
                }
                self.config.window.warm_window_pool = 1;
                let main = self.main_window_id.ok_or(FAILURE)?;
                self.create_new_terminal_window(el, self.window_request(Some(main)));
                let child = self.windows.keys().copied().find(|id| *id != main).ok_or(FAILURE)?;
                if let (Some(main), Some(child)) = (
                    self.main_window(),
                    self.windows.get(&child).and_then(|window| window.window.as_ref()),
                ) {
                    if let Ok(position) = main.outer_position() {
                        child.set_outer_position(winit::dpi::PhysicalPosition::new(
                            position.x + 64,
                            position.y + 64,
                        ));
                    }
                }
                probe.child = Some(child);
                probe.stage = Stage::Capture;
            }
            Stage::Capture => {
                // When: `stage` is Capture, require the complete topology before freezing original shell identities.
                if self.windows.len() != 2 || self.warm_window_pool.len() != 1 {
                    // When: `warm_window_pool` is not replenished yet, wait under the smoke's fixed deadline.
                    return Ok(false);
                }
                if live_renderer_count() != smoke.renderer_baseline + 3 {
                    // When: `live_renderer_count` differs from baseline plus three, an untracked renderer invalidates proof.
                    return Err(FAILURE);
                }
                let context = self.gpu_recovery_snapshot().ok_or(FAILURE)?;
                if context.phase != RecoveryPhase::Active || context.counts.requests != 0 {
                    // When: `context` is not an untouched active generation, the single-rebuild baseline is already lost.
                    return Err(FAILURE);
                }
                let mut panes = Vec::with_capacity(2);
                let mut ids: Vec<_> = self.windows.keys().copied().collect();
                ids.sort_unstable();
                for id in ids {
                    let window = &self.windows[&id];
                    let renderer = window.renderer.as_ref().ok_or(FAILURE)?;
                    if renderer.successful_frame_count() == 0 {
                        // When: `successful_frame_count` is zero, establish presentation before injecting any loss.
                        window.request_redraw();
                        return Ok(false);
                    }
                    if renderer.device_generation() != context.committed
                        || !renderer.device_accepts_gpu_work()
                    {
                        // When: `device_generation` or `device_accepts_gpu_work` differs, this is not one healthy shared device.
                        return Err(FAILURE);
                    }
                    let pane_id = window
                        .tab_states
                        .get(window.tabs.active_index())
                        .ok_or(FAILURE)?
                        .active_pane;
                    let pane = window.panes.get(&pane_id).ok_or(FAILURE)?;
                    let Some(parser) = pane.parser.try_lock() else {
                        // When: `parser` is busy, defer the complete proof without retaining any partial parser borrow.
                        return Ok(false);
                    };
                    let marker_rows = grid_marker_rows(parser.grid(), smoke.marker());
                    drop(parser);
                    let pty = pane.pty.as_ref().ok_or(FAILURE)?;
                    let exit = pty.child_exit_probe();
                    if exit.has_exited().map_err(|_| FAILURE)? {
                        // When: `has_exited` is true, an already-dead shell cannot prove survival through recovery.
                        return Err(FAILURE);
                    }
                    panes.push(PaneProof {
                        window: id,
                        pane: pane_id,
                        pid: pty.pid().ok_or(FAILURE)?,
                        exit,
                        marker_rows,
                        presented_generation: None,
                    });
                }
                if !self
                    .warm_window_pool
                    .iter()
                    .all(|warm| warm.renderer.device_generation() == context.committed)
                {
                    // When: `warm_window_pool` holds a different generation, recovery would not exercise one shared device.
                    return Err(FAILURE);
                }
                probe.original_generation = context.committed;
                probe.panes = panes;
                self.send_recovery_markers(smoke, &probe.panes)?;
                probe.stage = Stage::Baseline;
            }
            Stage::Baseline => {
                // When: `stage` is Baseline, prove both original shells presented their markers before device destruction.
                if !self.observe_recovery_markers(smoke, &probe.panes, probe.original_generation)? {
                    // When: `observe_recovery_markers` lacks a fresh acknowledged marker, keep waiting on the same shells.
                    return Ok(false);
                }
                if !self.capture_recovery_marker_baselines(smoke, &mut probe.panes)? {
                    // When: `capture_recovery_marker_baselines` is contended, delay injection rather than keep a partial baseline.
                    return Ok(false);
                }
                let renderer = self.main_renderer_mut().ok_or(FAILURE)?;
                probe.original_state = Some(Arc::clone(renderer.device_error_state()));
                tracing::warn!(target: "sonic::gpu::recovery", generation = probe.original_generation, windows = 2, warm = 1, "runtime smoke destroy shared GPU device");
                renderer.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
                if renderer.device_error_snapshot().state != DeviceState::Lost {
                    // When: `device_error_snapshot` did not record Lost, the destroy hook failed to reach the recovery trigger.
                    return Err(FAILURE);
                }
                self.send_recovery_markers(smoke, &probe.panes)?;
                probe.stage = Stage::Recovering;
            }
            Stage::Recovering => {
                // When: `stage` is Recovering, no old marker or device can satisfy the replacement and liveness checks.
                self.recovery_shells_alive(&probe.panes)?;
                let snapshot = self.gpu_recovery_snapshot().ok_or(FAILURE)?;
                if snapshot.counts.failed_attempts != 0
                    || snapshot.phase == RecoveryPhase::Exhausted
                {
                    // When: `snapshot` records a failed attempt or Exhausted, the one-rebuild golden path did not occur.
                    return Err(FAILURE);
                }
                if !one_rebuild(snapshot, probe.original_generation) {
                    // When: `one_rebuild` is not yet established, retain the original identities under the fixed deadline.
                    return Ok(false);
                }
                self.recovery_renderers_share(snapshot.committed, smoke.renderer_baseline)?;
                if !self.observe_recovery_markers(smoke, &probe.panes, snapshot.committed)? {
                    // When: `observe_recovery_markers` lacks a fresh acknowledged marker, keep waiting on the same shells.
                    return Ok(false);
                }
                probe.recovered_generation = Some(snapshot.committed);
                probe.stage = Stage::Quiet { until: now + QUIET_INTERVAL };
                self.event_loop_proxy
                    .as_ref()
                    .ok_or(FAILURE)?
                    .send_event(UserEvent::GpuDeviceGenerationChanged {
                        generation: probe.original_generation,
                    })
                    .map_err(|_| FAILURE)?;
            }
            Stage::Quiet { until } => {
                // When: `stage` is Quiet, a stale callback must leave the successful replacement and its shells unchanged.
                self.recovery_shells_alive(&probe.panes)?;
                let snapshot = self.gpu_recovery_snapshot().ok_or(FAILURE)?;
                if !one_rebuild(snapshot, probe.original_generation) {
                    // When: `one_rebuild` stops holding after the stale event, a second request or loss violated containment.
                    return Err(FAILURE);
                }
                self.recovery_renderers_share(snapshot.committed, smoke.renderer_baseline)?;
                let old = probe.original_state.as_ref().ok_or(FAILURE)?.snapshot();
                if !old.destroy_requested || old.state != DeviceState::Lost {
                    // When: `old` lacks the retired loss state, the quiet interval cannot prove intentional-destroy handling.
                    return Err(FAILURE);
                }
                if now < until || !probe.stale_event_observed {
                    // When: `now` is before `until` or the queued stale event is unobserved, completion is still unproven.
                    return Ok(false);
                }
                self.config.window.warm_window_pool = 0;
                let child = probe.child.ok_or(FAILURE)?;
                if !self.close_child_window(child) {
                    // When: `close_child_window` refuses the exact child, renderer-count agreement cannot prove its release.
                    return Err(FAILURE);
                }
                self.warm_window_pool_maintain(el);
                probe.stage = Stage::Release;
            }
            Stage::Release => {
                // When: `stage` is Release, require child exit and the original main shell before accepting renderer teardown.
                let snapshot = self.gpu_recovery_snapshot().ok_or(FAILURE)?;
                if !one_rebuild(snapshot, probe.original_generation)
                    || probe.recovered_generation != Some(snapshot.committed)
                    || !probe.stale_event_observed
                    || self.main_renderer().is_none_or(|renderer| {
                        renderer.device_generation() != snapshot.committed
                            || !renderer.device_accepts_gpu_work()
                    })
                {
                    // When: `snapshot` or renderer identity changed during teardown, the final single-rebuild proof is invalid.
                    return Err(FAILURE);
                }
                let child = probe.child.ok_or(FAILURE)?;
                let exited = probe.panes.iter().find(|pane| pane.window == child).ok_or(FAILURE)?;
                if !exited.exit.has_exited().map_err(|_| FAILURE)? {
                    // When: `has_exited` is false, asynchronous PTY cleanup still owns the child and needs another observation.
                    return Ok(false);
                }
                let main = probe.panes.iter().find(|pane| pane.window != child).ok_or(FAILURE)?;
                self.recovery_shells_alive(std::slice::from_ref(main))?;
                if self.windows.len() != 1
                    || !self.warm_window_pool.is_empty()
                    || live_renderer_count() != smoke.renderer_baseline + 1
                {
                    // When: `windows`, `warm_window_pool`, or `live_renderer_count` retains an extra owner, cleanup is incomplete.
                    return Err(FAILURE);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn recovery_shells_alive(&self, panes: &[PaneProof]) -> Result<(), RuntimeSmokeFailure> {
        for proof in panes {
            let pane = self
                .windows
                .get(&proof.window)
                .and_then(|window| window.panes.get(&proof.pane))
                .ok_or(FAILURE)?;
            let pty = pane.pty.as_ref().ok_or(FAILURE)?;
            if pty.pid() != Some(proof.pid) || proof.exit.has_exited().map_err(|_| FAILURE)? {
                // When: `pid` changed or `has_exited` is true, a replacement shell cannot satisfy original-session survival.
                return Err(FAILURE);
            }
        }
        Ok(())
    }

    fn send_recovery_markers(
        &self,
        smoke: &RuntimeSmokeState,
        panes: &[PaneProof],
    ) -> Result<(), RuntimeSmokeFailure> {
        self.recovery_shells_alive(panes)?;
        for proof in panes {
            self.windows[&proof.window].panes[&proof.pane]
                .pty
                .as_ref()
                .ok_or(FAILURE)?
                .send_input_nonblocking(smoke.command().to_vec())
                .map_err(|_| FAILURE)?;
            tracing::warn!(target: "sonic::gpu::recovery", window = ?proof.window, pane = proof.pane, pid = proof.pid, "runtime smoke recovery shell marker queued");
        }
        Ok(())
    }

    fn capture_recovery_marker_baselines(
        &self,
        smoke: &RuntimeSmokeState,
        panes: &mut [PaneProof],
    ) -> Result<bool, RuntimeSmokeFailure> {
        self.recovery_shells_alive(panes)?;
        let mut counts = Vec::with_capacity(panes.len());
        for proof in panes.iter() {
            let pane = &self.windows[&proof.window].panes[&proof.pane];
            let Some(parser) = pane.parser.try_lock() else {
                // When: `parser` is busy, capture none of this round's baseline until every pane is observable.
                return Ok(false);
            };
            counts.push(grid_marker_rows(parser.grid(), smoke.marker()));
        }
        for (proof, count) in panes.iter_mut().zip(counts) {
            proof.marker_rows = count;
            proof.presented_generation = None;
        }
        Ok(true)
    }

    fn observe_recovery_markers(
        &self,
        smoke: &RuntimeSmokeState,
        panes: &[PaneProof],
        generation: u64,
    ) -> Result<bool, RuntimeSmokeFailure> {
        self.recovery_shells_alive(panes)?;
        let mut ready = true;
        for proof in panes {
            let window = &self.windows[&proof.window];
            let pane = &window.panes[&proof.pane];
            let Some(parser) = pane.parser.try_lock() else {
                // When: `parser` is busy, no stale marker or presentation count can satisfy this observation.
                return Ok(false);
            };
            let fresh = grid_marker_rows(parser.grid(), smoke.marker()) > proof.marker_rows;
            drop(parser);
            if !fresh || proof.presented_generation != Some(generation) {
                ready = false;
                window.request_redraw();
            }
        }
        Ok(ready)
    }

    fn recovery_renderers_share(
        &self,
        generation: u64,
        baseline: usize,
    ) -> Result<(), RuntimeSmokeFailure> {
        if self.windows.len() != 2
            || self.warm_window_pool.len() != 1
            || live_renderer_count() != baseline + 3
        {
            // When: `windows`, `warm_window_pool`, or `live_renderer_count` differs, the required three-renderer topology was lost.
            return Err(FAILURE);
        }
        let accepts = |renderer: &sonicterm_gpu::core::GpuRenderer| {
            renderer.device_generation() == generation && renderer.device_accepts_gpu_work()
        };
        if !self.windows.values().all(|window| window.renderer.as_ref().is_some_and(accepts))
            || !self.warm_window_pool.iter().all(|warm| accepts(&warm.renderer))
        {
            // When: `accepts` rejects any live or warm renderer, partial replacement cannot count as recovery.
            return Err(FAILURE);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "gpu_recovery_smoke_tests.rs"]
mod gpu_recovery_smoke_tests;
