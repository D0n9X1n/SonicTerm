//! Owner-local redraw identities, pacing, and deadline service.
//!
//! Scheduling acknowledgement is independent of grid dirty acknowledgement.
//! `last_render` remains the public last-attempt clock. On the hardware path streaming is
//! paced from the window's `stream_clock`, which a settled input attempt that presented nothing
//! leaves in place; this state never turns retained grid dirt into a timer.

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
use winit::{event::WindowEvent, window::WindowId};

use super::{App, WindowState};

/// Whether a frame pass may set foreground-process probe demand: never for an output burst, and
/// never for a frame that only redraws the status line, whose probe result could repaint the
/// strip and so draw another frame.
pub(super) fn sets_fg_demand(pty_burst: bool, status_only: bool) -> bool {
    !pty_burst && !status_only
}

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
    StatusLine,
}
const CAUSES: usize = 12;

/// Captured cause generations; newer causes cannot be cleared by an older attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// A window's two pacing clocks and its contention floor, borrowed apart from its other fields so a
/// role adapter that still holds pane or renderer borrows can complete an attempt.
pub(super) struct AttemptClocks<'clocks> {
    /// The attempt clock, `WindowState::last_render`.
    pub(super) last_render: &'clocks mut Instant,
    /// The hardware streaming clock, `WindowState::stream_clock`.
    pub(super) stream_clock: &'clocks mut Instant,
    /// Whether a contention retry floor is armed for the window.
    pub(super) retry_armed: bool,
}

/// Per-window scheduling state, separate from public compatibility clocks and native redraw requests.
/// Not `Clone`: its timeline owner releases a slot's diagnostic buffers when it drops.
#[derive(Debug)]
pub(crate) struct WindowRedrawState {
    pending: [u64; CAUSES],
    observed: [u64; CAUSES],
    pub(super) attempt_causes: Option<CauseSnapshot>,
    /// While `frame` is listed: content frames presented and their summed CPU-side time in µs.
    pub(super) presented_frames: Option<(u64, u64)>,
    /// A failed or retried attempt carried content, so the retry that presents counts it.
    retry_content: bool,
    /// When the current attempt started collecting, on the real clock.
    pub(super) attempt_started: Option<Instant>,
    pub(super) last_present: Option<Instant>,
    pub(super) monitor_period: Duration,
    pub(super) deferred: bool,
    pub(super) request_in_flight: bool,
    pub(super) parked: bool,
    pub(super) stopped_generation: Option<u64>,
    pub(super) native_occluded: bool,
    pub(super) backend_occluded: bool,
    /// When native occlusion began, at the dispatch clock; `None` while visible. Backend-only
    /// occlusion never sets it, so only a native cover can age into a trim.
    pub(super) occluded_since: Option<Instant>,
    /// The renderer was trimmed during the current covered stretch; visibility clears it.
    pub(super) trimmed: bool,
    /// Requested GPU bytes that stretch's trim gave back; zero while untrimmed.
    pub(super) trim_released_requested_bytes: u64,
    pub(super) timeout_pending: bool,
    /// The pending admission's pacing mode, stored at the deferral that won; `None` when none is stored.
    pub(super) pacing: Option<super::display_link::PacingMode>,
    /// The link generation, bumped on every start and invalidation; the native tick target reads it.
    pub(super) link_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The generation of the running link interval, or `None` while it is paused or invalidated.
    pub(super) link_live: Option<u64>,
    /// The generation of an accepted tick not yet spent by an admission.
    pub(super) link_permit: Option<u64>,
    /// The rule that won the pending deferral; read only while `deferred` is set.
    pub(super) deferred_rule: Option<super::frame_counters::DeferRule>,
    /// The first `Sync` deferral of the current held stretch; the window holds at most 150 ms from it.
    pub(super) sync_stretch_start: Option<Instant>,
    /// A changed surface size not yet presented; it forces frames through a synchronized hold.
    pub(super) resize_pending: bool,
    /// A reconfigured or recreated surface not yet presented; it forces frames through a hold.
    pub(super) surface_recovery_pending: bool,
    /// Each visible pane's reset count read for the attempt in flight, credited on success.
    pub(super) attempt_sync_resets: Vec<(u64, u64)>,
    /// This window's frame counters; `Some` only when its App's gate is on.
    pub(super) frame_counters: Option<Box<super::frame_counters::WindowFrameCounters>>,
    /// The echo timeline's accepted-tick metadata and owner; inert while the gate is off.
    pub(super) timeline: super::echo_timeline::WindowTimeline,
    #[cfg(target_os = "macos")]
    pub(super) surface_probe_at: Option<Instant>,
    /// Test-only monitor source: `Some(rate)` replaces the native refresh-rate read.
    #[cfg(test)]
    pub(super) monitor_rate_override: Option<Option<u32>>,
    /// Test-only: how many times this window's retained frame was dropped, counted even when a
    /// headless test window has no renderer, so a test can check what the planner will see.
    #[cfg(test)]
    pub(super) retained_frame_drops: u64,
}

impl Default for WindowRedrawState {
    fn default() -> Self {
        Self {
            pending: [0; CAUSES],
            observed: [0; CAUSES],
            attempt_causes: None,
            presented_frames: None,
            retry_content: false,
            attempt_started: None,
            last_present: None,
            monitor_period: Duration::from_micros(16_667),
            frame_counters: None,
            deferred: false,
            request_in_flight: false,
            parked: false,
            stopped_generation: None,
            native_occluded: false,
            backend_occluded: false,
            occluded_since: None,
            trimmed: false,
            trim_released_requested_bytes: 0,
            timeout_pending: false,
            pacing: None,
            link_generation: super::display_link::new_link_generation(),
            link_live: None,
            link_permit: None,
            deferred_rule: None,
            sync_stretch_start: None,
            resize_pending: false,
            surface_recovery_pending: false,
            attempt_sync_resets: Vec::new(),
            timeline: super::echo_timeline::WindowTimeline::default(),
            #[cfg(target_os = "macos")]
            surface_probe_at: None,
            #[cfg(test)]
            monitor_rate_override: None,
            #[cfg(test)]
            retained_frame_drops: 0,
        }
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
    /// The renderer's promoted image atlas has had no renderable media for 30 s; released without a frame.
    ImageAtlasRelease,
    /// A status line sample or pending path check is due; the sync requests any frame itself.
    StatusLine,
    /// A taskbar, Dock or launcher progress value waits for its trailing edge; never a frame.
    TaskbarProgress,
}

/// A headless window's stand-in for the renderer's interval release: its deadline and how often it ran.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct TestImageAtlasRelease {
    pub(crate) deadline: Option<Instant>,
    pub(crate) releases: u32,
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
}

/// The windows asked for a frame in the current event-loop dispatch, so every step a change
/// passes through asks each window at most once per dispatch.
#[derive(Debug, Default)]
pub(crate) struct DispatchRedraws {
    /// Bumped as each dispatch opens; read through `&self`, as the dispatch scope is.
    dispatch: std::cell::Cell<u64>,
    /// The dispatch `asked` belongs to.
    asked_in: u64,
    /// Windows already asked during dispatch `asked_in`.
    asked: std::collections::HashSet<WindowId>,
}

impl DispatchRedraws {
    /// Open a new dispatch: every window may be asked once more.
    pub(super) fn begin(&self) {
        self.dispatch.set(self.dispatch.get().wrapping_add(1));
    }

    /// Whether `id` is asked for the first time this dispatch; records the ask.
    pub(super) fn first_ask(&mut self, id: WindowId) -> bool {
        let dispatch = self.dispatch.get();
        if self.asked_in != dispatch {
            // A record from an earlier dispatch no longer holds.
            self.asked_in = dispatch;
            self.asked.clear();
        }
        self.asked.insert(id)
    }

    /// Ask `window` for a frame unless this dispatch already asked it.
    pub(super) fn request(&mut self, id: WindowId, window: &WindowState) {
        if self.first_ask(id) {
            window.request_window_redraw();
        }
    }
}

impl App {
    /// Refresh a window's monitor period on the events that can change or reveal its display.
    pub(super) fn refresh_monitor_for_event(&mut self, id: WindowId, event: &WindowEvent) {
        if !matches!(
            event,
            WindowEvent::Moved(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Resized(_)
                | WindowEvent::Focused(true)
        ) {
            // When: matches! excludes `event`, it cannot move the window to another display, so the period stays.
            return;
        }
        if let Some(window) = self.windows.get_mut(&id) {
            window.refresh_monitor_period();
        }
    }

    /// Consume native occlusion centrally for either role, after excluding warm and stale identities.
    pub(super) fn handle_window_occlusion(&mut self, id: WindowId, occluded: bool) {
        if self.is_warm_window_id(id) {
            // When: `id` names a warm spare, native visibility cannot promote it into a terminal owner.
            return;
        }
        // One dispatch-clock read stamps the covered stretch, so a test's fake clock ages it.
        let now = (self.dispatch_clock)();
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` is stale, no other window may inherit its visibility transition.
            return;
        };
        if !occluded {
            // A window that reappears may be on another display, so read its rate first.
            window.refresh_monitor_period();
        }
        if window.redraw.observe_native_occlusion(occluded, now) {
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
    /// Ask window `id` for a frame once per dispatch; a later ask in the same dispatch adds nothing.
    pub(super) fn request_redraw_once(&mut self, id: WindowId) {
        let Self { windows, dispatch_redraws, .. } = self;
        if let Some(window) = windows.get(&id) {
            dispatch_redraws.request(id, window);
        }
    }

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
    /// Returns what the request did: a new native request, a mark onto one in flight, or nothing.
    pub(super) fn request_owner_redraw(
        &mut self,
        id: WindowId,
        cause: RedrawCause,
    ) -> super::OutputCheckOutcomeV1 {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` names no live window, nothing is marked or requested.
            return super::OutputCheckOutcomeV1::None;
        };
        window.mark_redraw(cause);
        if !window.frame_deadlines_allowed() {
            // When: `frame_deadlines_allowed` is false, the window is held back and no request is made.
            return super::OutputCheckOutcomeV1::None;
        }
        if window.redraw.request_in_flight {
            // When: `request_in_flight`, the cause is marked onto the request already in flight.
            return super::OutputCheckOutcomeV1::MarkedInFlight;
        }
        window.redraw.request_in_flight = true;
        window.request_window_redraw();
        super::OutputCheckOutcomeV1::NativeRequest
    }

    /// Command maintenance runs even for hidden, structurally parked, or device-stopped windows;
    /// changed command chrome marks `Chrome`, and an `Output` frame is always requested.
    pub(super) fn output_redraw_notification(&mut self, id: WindowId, now: Instant) {
        if self.command_maintenance(id, now) {
            if let Some(window) = self.windows.get_mut(&id) {
                window.mark_redraw(RedrawCause::Chrome);
            }
        }
        self.request_owner_redraw(id, RedrawCause::Output);
    }

    /// Drain the window's pane command events into its tabs and expire finished badges; returns
    /// whether any tab's command chrome changed.
    ///
    /// The poll writes each `TabState::command`, each tab-bar `Tab::command` (through
    /// `set_command_status` and the badge expiry), drains `PaneState::command_events`, and may
    /// raise a desktop notification. Of these only `Tab::command` enters the frame key, through
    /// `command_status_hash`, so the comparison takes that hash beside the drawn badge.
    pub(super) fn command_maintenance(&mut self, id: WindowId, now: Instant) -> bool {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: no window has `id`, there is no tab bar to maintain.
            return false;
        };
        let before = command_chrome(&window.tabs, now);
        let marked = super::poll_command_events_for_child_window(window, &self.config);
        window.tabs.clear_expired_command_badges(now);
        marked || before != command_chrome(&window.tabs, now)
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
                        window.request_window_redraw();
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
        let mut repaint: HashMap<WindowId, Vec<RedrawCause>> = HashMap::new();
        if due.iter().any(|work| work.deadline <= now && work.cause == DueCause::Scrollbar) {
            // Only owners whose bar changed get a frame; activity or a hold
            // after collection leaves the expiry a no-op.
            for id in self.expire_due_scrollbar_idle(now) {
                repaint.entry(id).or_default().push(RedrawCause::Scrollbar);
            }
        }
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
                DueCause::Scrollbar => {
                    // When: a due `Scrollbar` was serviced above, which already queued any frame it needs.
                }
                DueCause::ImageAtlasRelease => {
                    // The whole predicate is checked again: a window that became busy since collection
                    // drops the work, which is collected again once it is idle. No repaint is queued.
                    if let Some(window) = work.owner.and_then(|id| self.windows.get_mut(&id)) {
                        if window.image_atlas_release_eligible(now) {
                            window.release_idle_image_atlas(now);
                        }
                    }
                }
                DueCause::Frame | DueCause::Cursor | DueCause::Notification => {
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
                | DueCause::StatusLine
                | DueCause::TaskbarProgress
                | DueCause::PointerMotion
                | DueCause::GpuRecovery
                | DueCause::Smoke => {
                    // When: Memory, PointerMotion, GpuRecovery or Smoke is due, about-to-wait services it without a frame.
                }
            }
        }
        for (id, causes) in repaint {
            self.repaint_owner(id, &causes);
        }
    }

    /// Mark `causes` on window `id` and request one frame if its deadlines allow; a hidden
    /// or parked window is marked only and paints when it is next shown.
    pub(super) fn repaint_owner(&mut self, id: WindowId, causes: &[RedrawCause]) {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `windows` no longer holds `id`, the owner closed and nothing repaints.
            return;
        };
        for &cause in causes {
            window.mark_redraw(cause);
        }
        if window.frame_deadlines_allowed() && !window.redraw.request_in_flight {
            window.redraw.deferred = false;
            window.redraw.request_in_flight = true;
            window.request_window_redraw();
        }
    }
}

#[cfg(test)]
#[path = "redraw_tests.rs"]
mod redraw_tests;

// The fake dispatch clock and paced owners are shared with the display-link tests; `redraw` is
// private to `app`, so this public declaration reaches no further than `app`.
#[cfg(test)]
#[path = "redraw_dispatch_tests.rs"]
pub mod redraw_dispatch_tests;

/// Each tab's drawn command badge and its frame-key `command_status_hash` at `now`.
fn command_chrome(
    tabs: &sonicterm_ui::tabs::TabBar,
    now: Instant,
) -> Vec<(Option<&'static str>, u64)> {
    let active = tabs.active_index();
    tabs.tabs()
        .iter()
        .enumerate()
        .map(|(index, tab)| {
            (
                tab.command.clone().badge(now, index == active),
                sonicterm_gpu::core::command_status_hash(&tab.command, now, index == active),
            )
        })
        .collect()
}

#[path = "redraw_admission.rs"]
mod admission;
#[path = "redraw_state.rs"]
mod state;

#[path = "redraw_window.rs"]
mod window;
