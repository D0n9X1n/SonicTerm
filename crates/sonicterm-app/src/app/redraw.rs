//! Owner-local redraw identities, pacing, and deadline service.
//!
//! Scheduling acknowledgement is independent of grid dirty acknowledgement.
//! `last_render` remains the public last-attempt clock; this state adds no second
//! pacing timestamp and never turns retained grid dirt into a timer.

use std::{
    collections::HashMap,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use sonicterm_gpu::{
    core::{PresentOutcome, SurfaceRetryReason},
    device_errors::{DeviceErrorSnapshot, DeviceState},
};
use sonicterm_ui::tabs::{CommandStatus, TabId};
use winit::window::WindowId;

use super::{App, WindowState};

/// Finite owner-addressed frame causes; each has its own generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RedrawCause {
    Input,
    Output,
    Expose,
    Chrome,
    Cursor,
    Scrollbar,
    SurfaceRetry,
    AtlasRetry,
    Visibility,
    DeviceRecovered,
    Topology,
}
const CAUSES: usize = 11;

/// Captured cause generations; newer causes cannot be cleared by an older attempt.
#[derive(Debug, Clone, Copy)]
pub(super) struct CauseSnapshot([u64; CAUSES]);

/// One pane's published output identity, never a max or sum across panes.
#[derive(Debug, Clone, Copy)]
pub(super) struct PaneGeneration {
    pub(super) id: u64,
    pub(super) generation: u64,
}

/// Snapshot taken before the attempt's first parser or image lock.
#[derive(Debug, Clone)]
pub(super) struct FrameSnapshot {
    pub(super) causes: CauseSnapshot,
    pub(super) panes: Vec<PaneGeneration>,
}

/// Scheduling disposition read before the typed renderer outcome is consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameSettlement {
    Presented,
    Cached,
    Settled,
    Retry(RedrawCause),
    SurfaceRetry(SurfaceRetryReason),
    Stopped(u64),
    Failed,
}

impl FrameSettlement {
    /// Classify the existing presenter outcome without changing its retry ownership.
    pub(super) fn of(outcome: &PresentOutcome) -> Self {
        match outcome {
            PresentOutcome::Presented => Self::Presented,
            PresentOutcome::CachedReblit => Self::Cached,
            PresentOutcome::Skipped(_) => Self::Settled,
            PresentOutcome::AtlasRetry => Self::Retry(RedrawCause::AtlasRetry),
            PresentOutcome::SurfaceRetry(reason) => Self::SurfaceRetry(*reason),
            PresentOutcome::RenderingUnavailable(context) => Self::Stopped(context.generation),
            PresentOutcome::Failed(_) => Self::Failed,
        }
    }
}

/// Per-window scheduling state, separate from public compatibility clocks and native redraw requests.
#[derive(Debug, Clone)]
pub(crate) struct WindowRedrawState {
    pending: [u64; CAUSES],
    observed: [u64; CAUSES],
    pub(super) attempt_causes: Option<CauseSnapshot>,
    pub(super) last_present: Option<Instant>,
    pub(super) monitor_period: Duration,
    pub(super) deferred: bool,
    pub(super) request_in_flight: bool,
    pub(super) parked: bool,
    pub(super) stopped_generation: Option<u64>,
    pub(super) native_occluded: bool,
    pub(super) backend_occluded: bool,
    pub(super) timeout_pending: bool,
    #[cfg(target_os = "macos")]
    pub(super) surface_probe_at: Option<Instant>,
}

impl Default for WindowRedrawState {
    fn default() -> Self {
        Self {
            pending: [0; CAUSES],
            observed: [0; CAUSES],
            attempt_causes: None,
            last_present: None,
            monitor_period: Duration::from_micros(16_667),
            deferred: false,
            request_in_flight: false,
            parked: false,
            stopped_generation: None,
            native_occluded: false,
            backend_occluded: false,
            timeout_pending: false,
            #[cfg(target_os = "macos")]
            surface_probe_at: None,
        }
    }
}

impl WindowRedrawState {
    /// Record one cause; only a topology-capable cause may unpark structural invalidity.
    pub(super) fn mark(&mut self, cause: RedrawCause) {
        self.pending[cause as usize] = self.pending[cause as usize].wrapping_add(1);
        if matches!(
            cause,
            RedrawCause::Input
                | RedrawCause::Topology
                | RedrawCause::Visibility
                | RedrawCause::DeviceRecovered
        ) {
            self.parked = false;
        }
    }

    /// Whether an input generation has not yet spent its immediate-attempt privilege.
    pub(super) fn input_pending(&self) -> bool {
        self.pending[RedrawCause::Input as usize] != self.observed[RedrawCause::Input as usize]
    }

    /// Whether any captured request remains unsettled.
    pub(super) fn has_pending(&self) -> bool {
        self.pending != self.observed
    }

    /// Capture cause identities without consuming later work.
    pub(super) fn snapshot(&self) -> CauseSnapshot {
        CauseSnapshot(self.pending)
    }

    /// Settle only captured causes; every real attempt consumes input immediacy, even a retry.
    pub(super) fn settle(
        &mut self,
        snapshot: CauseSnapshot,
        outcome: FrameSettlement,
        now: Instant,
    ) {
        self.attempt_causes = None;
        self.observed[RedrawCause::Input as usize] = snapshot.0[RedrawCause::Input as usize];
        self.deferred = false;
        self.request_in_flight = false;
        self.timeout_pending = false;
        match outcome {
            FrameSettlement::Presented | FrameSettlement::Cached | FrameSettlement::Settled => {
                self.observed = snapshot.0;
                if matches!(outcome, FrameSettlement::Presented | FrameSettlement::Cached) {
                    self.last_present = Some(now);
                }
            }
            FrameSettlement::Stopped(generation) => {
                self.stopped_generation = Some(generation);
                self.cancel_surface_probe();
            }
            FrameSettlement::SurfaceRetry(reason) => {
                self.mark(RedrawCause::SurfaceRetry);
                match reason {
                    SurfaceRetryReason::Timeout => {
                        self.timeout_pending = true;
                        self.deferred = true;
                    }
                    SurfaceRetryReason::Occluded => {
                        self.backend_occluded = true;
                        #[cfg(target_os = "macos")]
                        if !self.native_occluded {
                            self.surface_probe_at = Some(now + SURFACE_PROBE_PERIOD);
                        }
                    }
                    SurfaceRetryReason::Outdated
                    | SurfaceRetryReason::Suboptimal
                    | SurfaceRetryReason::SurfaceLost => {
                        // When: `Outdated`, `Suboptimal`, or `SurfaceLost` recovered the surface, the presenter's native request owns retry.
                    }
                }
            }
            FrameSettlement::Retry(cause) => self.mark(cause),
            FrameSettlement::Failed => {
                // When: `Failed` retains pending work, the failure itself must not create a retry timer.
            }
        }
    }

    /// Cancel only the exceptional backend probe, never output or input identities.
    pub(super) fn cancel_surface_probe(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.surface_probe_at = None;
        }
    }

    /// A native event supersedes backend occlusion; return only a genuine transition back to visibility.
    fn observe_native_occlusion(&mut self, occluded: bool) -> bool {
        let was_occluded = self.native_occluded || self.backend_occluded;
        self.native_occluded = occluded;
        self.backend_occluded = false;
        self.cancel_surface_probe();
        if occluded {
            self.deferred = false;
            self.timeout_pending = false;
        }
        was_occluded && !occluded
    }

    /// Consume a structurally invalid attempt and exclude every frame-family deadline.
    pub(super) fn park(&mut self, snapshot: CauseSnapshot) {
        self.observed = snapshot.0;
        self.attempt_causes = None;
        self.parked = true;
        self.cancel_surface_probe();
        self.deferred = false;
        self.request_in_flight = false;
    }
}

/// Backend-only Metal occlusion probes are bounded to one attempt per second, never frame cadence.
#[cfg(target_os = "macos")]
const SURFACE_PROBE_PERIOD: Duration = Duration::from_secs(1);

/// Captured command identity prevents an old timer from consuming a replacement command's badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BadgeTransition {
    Running(Instant),
    Done { exit: Option<u8>, until: Instant },
}

impl BadgeTransition {
    /// Capture only a status whose visible badge still has a strictly future transition.
    pub(super) fn capture(
        status: &CommandStatus,
        now: Instant,
        is_active: bool,
    ) -> Option<(Self, Instant)> {
        let deadline = status.next_visual_deadline(now, is_active)?;
        let transition = match status {
            CommandStatus::Running(started) => Self::Running(*started),
            CommandStatus::Done { exit, until } => Self::Done { exit: *exit, until: *until },
            CommandStatus::Idle => {
                // When: status is Idle, no command identity can own a visual transition.
                return None;
            }
        };
        Some((transition, deadline))
    }

    /// Revalidate the command and current activity instead of trusting a captured tab index.
    fn matches(self, status: &CommandStatus, is_active: bool) -> bool {
        match (self, status) {
            (Self::Running(expected), CommandStatus::Running(started)) => {
                !is_active && expected == *started
            }
            (
                Self::Done { exit: expected_exit, until: expected_until },
                CommandStatus::Done { exit, until },
            ) => expected_exit == *exit && expected_until == *until,
            _ => false,
        }
    }
}

/// Timer contributors retain their identity instead of becoming a global wake boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DueCause {
    Frame,
    Cursor,
    Scrollbar,
    Notification,
    CommandBadge {
        tab: TabId,
        transition: BadgeTransition,
    },
    #[cfg(windows)]
    Foreground,
    QuitHold,
    Memory,
    PointerMotion,
    GpuRecovery,
    #[cfg(windows)]
    Osc52,
    Smoke,
    #[cfg(target_os = "macos")]
    SurfaceProbe,
}

/// A deadline may repaint one owner or service maintenance without any owner.
#[derive(Debug, Clone, Copy)]
pub(super) struct DueWork {
    pub(super) owner: Option<WindowId>,
    pub(super) cause: DueCause,
    pub(super) deadline: Instant,
}

/// Record captured pane identities through a field-split borrow, not the whole window.
pub(super) fn settle_pane_generations(
    panes: &mut HashMap<u64, super::PaneState>,
    snapshot: &FrameSnapshot,
) {
    for captured in &snapshot.panes {
        if let Some(pane) = panes.get_mut(&captured.id) {
            pane.observed_output_generation = captured.generation;
        }
    }
}

impl WindowState {
    /// Mark an owner-local cause while keeping request_redraw's native-only public contract.
    pub(super) fn mark_redraw(&mut self, cause: RedrawCause) {
        self.redraw.mark(cause);
    }

    /// Read only this window's pane output atomics before any collection locks.
    // Ordering: output_generation Acquire pairs with the worker's after-batch Release publication.
    pub(super) fn capture_redraw_snapshot(&self) -> FrameSnapshot {
        let mut panes: Vec<_> = self
            .panes
            .iter()
            .map(|(id, pane)| PaneGeneration {
                id: *id,
                generation: pane.output_generation.load(Ordering::Acquire),
            })
            .collect();
        panes.sort_unstable_by_key(|pane| pane.id);
        FrameSnapshot { causes: self.redraw.snapshot(), panes }
    }

    /// Only visible output affects burst pacing; hidden generations are observed without media locks.
    // Ordering: output_generation Acquire observes completed batches only, never in-flight parser work.
    pub(super) fn visible_output_advanced(&self) -> bool {
        let Some(tab) = self.tab_states.get(self.tabs.active_index()) else {
            // When: `tab` is absent, no visible output can authorize a frame burst.
            return false;
        };
        let ids = tab.tree.zoomed_pane_id().map_or_else(|| tab.tree.leaves(), |id| vec![id]);
        ids.into_iter().any(|id| {
            self.panes.get(&id).is_some_and(|pane| {
                pane.output_generation.load(Ordering::Acquire) != pane.observed_output_generation
            })
        })
    }

    /// Preserve newly published output while acknowledging exactly the pre-lock snapshot.
    pub(super) fn settle_output_snapshot(&mut self, snapshot: &FrameSnapshot) {
        settle_pane_generations(&mut self.panes, snapshot);
    }

    /// Capture this window's future transitions with stable tab and command identities.
    pub(super) fn command_badge_due(&self, id: WindowId, now: Instant) -> Vec<DueWork> {
        let active = self.tabs.active_index();
        self.tabs
            .tabs()
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| {
                let (transition, deadline) =
                    BadgeTransition::capture(&tab.command, now, index == active)?;
                Some(DueWork {
                    owner: Some(id),
                    cause: DueCause::CommandBadge { tab: tab.id, transition },
                    deadline,
                })
            })
            .collect()
    }

    /// True only for a window whose frame-family deadlines may currently contribute.
    pub(super) fn frame_deadlines_allowed(&self) -> bool {
        !self.hidden
            && !self.redraw.native_occluded
            && !self.redraw.backend_occluded
            && !self.redraw.parked
            && self.redraw.stopped_generation.is_none()
    }

    /// Mark one full visibility frame without acquiring pane locks or performing GPU work.
    pub(super) fn invalidate_visibility_frame(&mut self) {
        self.mark_redraw(RedrawCause::Visibility);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.invalidate_retained_frame();
        }
    }

    /// Request visibility only for an attached usable renderer; device stop always wins.
    pub(super) fn request_visible_frame(&mut self) {
        if self.frame_deadlines_allowed()
            && !self.redraw.request_in_flight
            && self.renderer.as_ref().is_some_and(|renderer| renderer.device_accepts_gpu_work())
        {
            self.redraw.request_in_flight = true;
            self.request_redraw();
        }
    }

    /// Return only a live backend-only probe; native-hidden and stopped owners never contribute one.
    #[cfg(target_os = "macos")]
    pub(super) fn surface_probe_deadline(&self) -> Option<Instant> {
        (!self.hidden
            && !self.redraw.native_occluded
            && !self.redraw.parked
            && self.redraw.stopped_generation.is_none()
            && self.redraw.backend_occluded)
            .then_some(self.redraw.surface_probe_at)
            .flatten()
    }

    /// Settle a native probe result without consuming pending frame or output identity.
    #[cfg(target_os = "macos")]
    fn finish_surface_probe(
        &mut self,
        outcome: anyhow::Result<sonicterm_gpu::core::SurfaceAvailability>,
        device_usable: bool,
        now: Instant,
    ) {
        use sonicterm_gpu::core::SurfaceAvailability;
        self.redraw.surface_probe_at = None;
        if !device_usable {
            // When: `device_usable` is false after probing, a native result cannot revive the device.
            return;
        }
        // Surface creation errors leave no visibility signal, so the existing slow probe must retry.
        let outcome = outcome.unwrap_or(SurfaceAvailability::Retry);
        match outcome {
            SurfaceAvailability::Available => {
                self.redraw.backend_occluded = false;
                self.invalidate_visibility_frame();
                self.request_visible_frame();
            }
            SurfaceAvailability::Retry => {
                if !self.hidden
                    && !self.redraw.native_occluded
                    && !self.redraw.parked
                    && self.redraw.stopped_generation.is_none()
                    && self.redraw.backend_occluded
                {
                    self.redraw.surface_probe_at = Some(now + SURFACE_PROBE_PERIOD);
                }
            }
            SurfaceAvailability::Unavailable => {
                // When: `Unavailable` refuses a probe, only a native event or actual device replacement can retry.
            }
        }
    }

    /// Accept only a usable replacement renderer generation, never an arbitrary recovery cause.
    fn accept_device_recovery(&mut self, snapshot: &DeviceErrorSnapshot) -> bool {
        let Some(stopped) = self.redraw.stopped_generation else {
            // When: `stopped_generation` is absent, there is no recovery transition to repeat.
            return false;
        };
        if stopped == snapshot.generation
            || snapshot.state != DeviceState::Usable
            || snapshot.destroy_requested
        {
            // When: `snapshot` names the old generation or refuses work, keep the stopped owner suppressed.
            return false;
        }
        self.redraw.stopped_generation = None;
        // Backend occlusion belonged to the replaced device; native visibility remains authoritative.
        self.redraw.backend_occluded = false;
        self.redraw.cancel_surface_probe();
        self.mark_redraw(RedrawCause::DeviceRecovered);
        super::mark_all_panes_dirty(&self.panes);
        true
    }

    /// Apply verified recovery once and coalesce its visible native request with any pending request.
    fn request_device_recovery(&mut self, snapshot: &DeviceErrorSnapshot) -> bool {
        if !self.accept_device_recovery(snapshot) {
            // When: `accept_device_recovery` refuses, neither causes nor native requests can revive the stopped generation.
            return false;
        }
        if self.frame_deadlines_allowed() && !self.redraw.request_in_flight {
            self.redraw.request_in_flight = true;
            self.request_redraw();
        }
        true
    }

    /// Refresh the raw native-monitor period, preserving the last known rate when unavailable.
    pub(super) fn refresh_monitor_period(&mut self) {
        if let Some(rate) = self
            .window
            .as_ref()
            .and_then(|window| window.current_monitor())
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .filter(|rate| *rate > 0)
        {
            self.redraw.monitor_period = Duration::from_micros(1_000_000_000 / u64::from(rate));
        }
    }
}

impl App {
    /// Consume native occlusion centrally for either role, after excluding warm and stale identities.
    pub(super) fn handle_window_occlusion(&mut self, id: WindowId, occluded: bool) {
        if self.is_warm_window_id(id) {
            // When: `id` names a warm spare, native visibility cannot promote it into a terminal owner.
            return;
        }
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` is stale, no other window may inherit its visibility transition.
            return;
        };
        if window.redraw.observe_native_occlusion(occluded) {
            window.invalidate_visibility_frame();
            window.request_visible_frame();
        }
        self.redraw_due.retain(|work| {
            if work.owner != Some(id) {
                // When: `work.owner` differs, a native event cannot cancel a sibling's deadline.
                return true;
            }
            if occluded {
                // When: `occluded` is true, remove this owner's armed frame-family deadlines without consuming their causes.
                return false;
            }
            match work.cause {
                #[cfg(target_os = "macos")]
                DueCause::SurfaceProbe => {
                    // When: `SurfaceProbe` is armed, any native event supersedes backend-only observation.
                    false
                }
                _ => true,
            }
        });
    }

    /// Service only the exact armed Metal probe, not a stale deadline or another owner's work.
    #[cfg(target_os = "macos")]
    fn service_surface_probe(&mut self, id: WindowId, deadline: Instant, now: Instant) {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` no longer exists, its old probe has no target.
            return;
        };
        if deadline > now || window.surface_probe_deadline() != Some(deadline) {
            // When: `deadline` is later or no longer current, leave the live probe identity untouched.
            return;
        }
        window.redraw.surface_probe_at = None;
        let Some(renderer) = window.renderer.as_mut() else {
            // When: `renderer` is absent, do not manufacture an availability retry loop.
            return;
        };
        let outcome = renderer.probe_surface_availability();
        let device_usable = renderer.device_accepts_gpu_work();
        if let Err(error) = &outcome {
            tracing::warn!(?id, %error, "surface availability probe failed");
        }
        window.finish_surface_probe(outcome, device_usable, Instant::now());
    }

    /// Mark only a currently live owner; stale notifications never fall back to main.
    pub(super) fn mark_window_redraw(&mut self, id: WindowId, cause: RedrawCause) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.mark_redraw(cause);
        }
    }

    /// Configuration changes intentionally invalidate input policy in every live window.
    pub(super) fn mark_all_window_inputs(&mut self) {
        for window in self.windows.values_mut() {
            window.mark_redraw(RedrawCause::Input);
        }
    }

    /// Queue at most one native request per owner; parked output cannot unpark or schedule a heartbeat.
    pub(super) fn request_owner_redraw(&mut self, id: WindowId, cause: RedrawCause) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.mark_redraw(cause);
            if window.frame_deadlines_allowed() && !window.redraw.request_in_flight {
                window.redraw.request_in_flight = true;
                window.request_redraw();
            }
        }
    }

    /// Command maintenance runs even for hidden, structurally parked, or device-stopped windows.
    pub(super) fn output_redraw_notification(&mut self, id: WindowId, now: Instant) {
        if let Some(window) = self.windows.get_mut(&id) {
            let active = window.tabs.active_index();
            let before: Vec<_> = window
                .tabs
                .tabs()
                .iter()
                .enumerate()
                .map(|(i, tab)| tab.command.clone().badge(now, i == active))
                .collect();
            super::poll_command_events_for_child_window(window, &self.config);
            window.tabs.clear_expired_command_badges(now);
            let after: Vec<_> = window
                .tabs
                .tabs()
                .iter()
                .enumerate()
                .map(|(i, tab)| tab.command.clone().badge(now, i == active))
                .collect();
            if before != after {
                window.mark_redraw(RedrawCause::Chrome);
            }
        }
        self.request_owner_redraw(id, RedrawCause::Output);
    }

    /// Deliver stopped-device reports without turning unchanged usable devices into redraw causes.
    pub(super) fn request_device_state_redraws(&mut self) {
        let owners: Vec<_> = self.windows.keys().copied().collect();
        for id in owners {
            if !self.request_recovered_window(id) {
                if let Some(window) = self.windows.get(&id) {
                    if window
                        .renderer
                        .as_ref()
                        .is_some_and(|renderer| !renderer.device_accepts_gpu_work())
                    {
                        // Even hidden windows must reach the stopped-device reporting boundary.
                        window.request_redraw();
                    }
                }
            }
        }
    }

    /// Request one visible recovery frame only after inspecting the owner's replacement renderer.
    pub(super) fn request_recovered_window(&mut self, id: WindowId) -> bool {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` is stale, recovery cannot be redirected to another window.
            return false;
        };
        let Some(snapshot) =
            window.renderer.as_ref().map(|renderer| renderer.device_error_snapshot())
        else {
            // When: `renderer` is absent, a DeviceRecovered cause alone proves no usable replacement.
            return false;
        };
        window.request_device_recovery(&snapshot)
    }

    /// Device-stop reporting outranks occlusion and topology parking, then the owner-local pacing gate runs.
    pub(super) fn begin_window_redraw(&mut self, id: WindowId, now: Instant) -> bool {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` no longer names a live `window`, refuse without using a sibling.
            return false;
        };
        window.redraw.request_in_flight = false;
        if let Some(renderer) = window.renderer.as_mut() {
            // When: `renderer` exists, report a device stop before topology or pacing can suppress it.
            if !renderer.device_accepts_gpu_work() {
                // When: `renderer` refuses device work, one report is allowed but no frame assembly follows.
                window.redraw.stopped_generation = Some(renderer.device_generation());
                window.redraw.cancel_surface_probe();
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.note_stopped_redraw_refusal(id, &renderer.device_error_snapshot());
                }
                if let Some(outcome) = renderer.take_stopped_render_outcome() {
                    if let Err(error) = outcome.into_render_result() {
                        tracing::warn!(?id, %error, "render error");
                    }
                }
                window.redraw.deferred = false;
                return false;
            }
            if window.redraw.native_occluded || window.redraw.backend_occluded {
                // When: `native_occluded` or `backend_occluded` suppresses this owner, even recovery dirt waits without pane locks.
                return false;
            }
            if window.redraw.stopped_generation.is_some() {
                // When: `stopped_generation` is set, validate recovery; ordinary usable frames need no error-record snapshot.
                let snapshot = renderer.device_error_snapshot();
                if !window.accept_device_recovery(&snapshot) {
                    // When: `accept_device_recovery` refuses the live snapshot, no cause may bypass the stopped generation.
                    return false;
                }
            }
        }
        if !window.frame_deadlines_allowed() {
            // When: `window` is hidden, occluded, parked, or stopped, it contributes no frame attempt or deadline.
            return false;
        }
        if !window.redraw.has_pending() {
            window.mark_redraw(RedrawCause::Expose);
        }
        let period = super::effective_frame_period(
            self.software_render_degrade,
            window.ime.is_composing(),
            window.redraw.monitor_period,
        );
        let defer = (window.redraw.timeout_pending && now < window.last_render + period)
            || window.contention_blocks_redraw(now, period)
            || super::should_defer_streaming_redraw(
                window.redraw.input_pending(),
                window.visible_output_advanced(),
                self.software_render_degrade,
                now.saturating_duration_since(window.last_render),
                period,
            );
        window.redraw.deferred = defer;
        if !defer {
            window.redraw.attempt_causes = Some(window.redraw.snapshot());
        }
        if self.main_window_id == Some(id) {
            self.pending_redraw = defer;
        }
        if defer && self.main_window_id != Some(id) {
            self.pending_redraw_windows.insert(id);
        } else {
            // When: `defer` is false or `id` is main, no child compatibility marker is needed.
            self.pending_redraw_windows.remove(&id);
        }
        !defer
    }

    /// Return the actual pre-lock typed snapshot; a missing owner cannot be acknowledged.
    pub(super) fn snapshot_window_redraw(&mut self, id: WindowId) -> Option<FrameSnapshot> {
        self.snapshot_window_redraw_at(id, Instant::now())
    }

    /// Capture future badge transitions before the frame samples them, not after presentation.
    pub(super) fn snapshot_window_redraw_at(
        &mut self,
        id: WindowId,
        now: Instant,
    ) -> Option<FrameSnapshot> {
        let window = self.windows.get_mut(&id)?;
        let snapshot = window.capture_redraw_snapshot();
        window.redraw.attempt_causes = Some(snapshot.causes);
        if self.tab_bar_visible && window.frame_deadlines_allowed() {
            let badges = window.command_badge_due(id, now);
            for work in badges {
                if !self.redraw_due.iter().any(|current| {
                    current.owner == work.owner
                        && current.cause == work.cause
                        && current.deadline == work.deadline
                }) {
                    self.redraw_due.push(work);
                }
            }
        }
        Some(snapshot)
    }

    /// Complete one renderer call without acknowledging a sibling or a newer cause/output generation.
    pub(super) fn finish_window_redraw(
        &mut self,
        id: WindowId,
        snapshot: &FrameSnapshot,
        outcome: FrameSettlement,
        at: Instant,
    ) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.last_render = at;
            window.redraw.settle(snapshot.causes, outcome, at);
            if window.hidden {
                window.redraw.cancel_surface_probe();
            }
            if matches!(
                outcome,
                FrameSettlement::Presented | FrameSettlement::Cached | FrameSettlement::Settled
            ) {
                window.settle_output_snapshot(snapshot);
            }
        }
        if self.main_window_id == Some(id) {
            self.pending_redraw = false;
        }
        self.pending_redraw_windows.remove(&id);
    }

    /// Consume a still-current badge transition without borrowing parser state or waking another owner.
    fn consume_command_badge_due(
        &mut self,
        id: WindowId,
        tab: TabId,
        transition: BadgeTransition,
    ) -> bool {
        if !self.tab_bar_visible {
            // When: tab_bar_visible is false, an old badge wake cannot request hidden chrome.
            return false;
        }
        let Some(window) = self.windows.get_mut(&id) else {
            // When: id no longer resolves, a removed owner must not fall back to the main window.
            return false;
        };
        let Some(index) = window.tabs.tabs().iter().position(|current| current.id == tab) else {
            // When: tab closed or moved, leave the new occupant of its previous index untouched.
            return false;
        };
        let active = index == window.tabs.active_index();
        if !transition.matches(&window.tabs.tabs()[index].command, active) {
            // When: transition no longer matches status or activity, newer state owns its own deadline.
            return false;
        }
        if matches!(transition, BadgeTransition::Done { .. }) {
            window.tabs.set_command_status(index, CommandStatus::Idle);
        }
        true
    }

    /// Fold due work by identity, servicing only elapsed contributors and coalescing each owner's request.
    pub(super) fn service_redraw_due(&mut self, now: Instant) {
        let due = std::mem::take(&mut self.redraw_due);
        if due.iter().any(|work| work.deadline <= now && work.cause == DueCause::Scrollbar) {
            // When: `due` includes a scrollbar expiry, update its visibility before requesting the owner frame.
            let _ = self.expire_due_scrollbar_snaps(now);
        }
        let mut repaint: HashMap<WindowId, Vec<RedrawCause>> = HashMap::new();
        for work in due {
            if work.deadline > now {
                // When: `work.deadline` is later than `now`, preserve that owner without waking it early.
                self.redraw_due.push(work);
                continue;
            }
            match work.cause {
                #[cfg(target_os = "macos")]
                DueCause::SurfaceProbe => {
                    // When: `SurfaceProbe` is due on macOS, service its identity without frame collection.
                    if let Some(id) = work.owner {
                        self.service_surface_probe(id, work.deadline, now);
                    }
                }
                DueCause::CommandBadge { tab, transition } => {
                    if let Some(id) = work.owner {
                        if self.consume_command_badge_due(id, tab, transition) {
                            // Keep current badge dirt even when its window became hidden after arming.
                            repaint.entry(id).or_default().push(RedrawCause::Chrome);
                        }
                    }
                }
                DueCause::Frame
                | DueCause::Cursor
                | DueCause::Scrollbar
                | DueCause::Notification => {
                    if let Some(id) = work.owner {
                        if self.windows.get(&id).is_some_and(WindowState::frame_deadlines_allowed) {
                            if work.cause == DueCause::Notification {
                                if let Some(window) = self.windows.get_mut(&id) {
                                    if window
                                        .notification
                                        .as_ref()
                                        .and_then(|bubble| bubble.expires_at)
                                        .is_some_and(|at| at <= now)
                                    {
                                        window.notification = None;
                                    }
                                }
                            }
                            let cause = match work.cause {
                                DueCause::Cursor => RedrawCause::Cursor,
                                DueCause::Scrollbar => RedrawCause::Scrollbar,
                                DueCause::Notification => RedrawCause::Chrome,
                                _ => RedrawCause::Expose,
                            };
                            repaint.entry(id).or_default().push(cause);
                        }
                    }
                }
                #[cfg(windows)]
                DueCause::Foreground => {
                    // When: `Foreground` is due, only windows with changed privilege chrome repaint.
                    #[cfg(windows)]
                    for id in self.refresh_foreground_privileges_if_due(now) {
                        repaint.entry(id).or_default().push(RedrawCause::Chrome);
                    }
                }
                #[cfg(windows)]
                DueCause::Osc52 => {
                    // When: `Osc52` is due, clipboard maintenance requests no frame by itself.
                    #[cfg(windows)]
                    self.reassert_osc52_clipboard_if_due(now);
                }
                DueCause::QuitHold => {
                    // When: `QuitHold` expires, only its confirmation state changes, not window pixels.
                    let _ = self.quit_hold.on_tick(now);
                }
                DueCause::Memory
                | DueCause::PointerMotion
                | DueCause::GpuRecovery
                | DueCause::Smoke => {
                    // When: Memory, PointerMotion, GpuRecovery or Smoke is due, about-to-wait services it without a frame.
                }
            }
        }
        for (id, causes) in repaint {
            if let Some(window) = self.windows.get_mut(&id) {
                for cause in causes {
                    window.mark_redraw(cause);
                }
                if window.frame_deadlines_allowed() && !window.redraw.request_in_flight {
                    window.redraw.deferred = false;
                    window.redraw.request_in_flight = true;
                    window.request_redraw();
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "redraw_tests.rs"]
mod redraw_tests;
