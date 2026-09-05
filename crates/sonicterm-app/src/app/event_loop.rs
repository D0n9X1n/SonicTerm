//! `App::do_resumed` / `do_user_event` / `do_new_events` /
//! `do_about_to_wait` — extracted from the `ApplicationHandler` trait impl
//! from the monolithic app module.
//!
//! The trait methods in `mod.rs` are 1-line delegators that call into
//! these `impl App` methods. Splitting the bodies out of the trait impl
//! lets us keep the event-loop logic in its own file without breaking
//! the trait-impl rule that all methods must live in one `impl` block.

#![allow(unused_imports)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use sonicterm_gpu::core::GpuRenderer;
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

#[cfg(windows)]
use super::FOREGROUND_PROCESS_TTL;
use super::{
    mark_all_panes_dirty, runtime_smoke::RuntimeSmokeFailure, window_dpi, with_integrated_titlebar,
    App, UserEvent,
};
use sonicterm_ui::selection::SelectMode;
use winit::event_loop::ControlFlow;

/// The earlier of two optional deadlines.
///
/// A deadline is a "wake no later than" bound, so the earliest wins. Folding
/// rather than overwriting is what keeps an earlier contributor from being
/// pushed out past its due instant by a later one.
fn earliest(left: Option<Instant>, right: Option<Instant>) -> Option<Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

/// Whether the wake about to be armed exists only to sample memory.
///
/// The distinction decides whether the resulting `ResumeTimeReached` may return
/// before frame and foreground-probe work. A memory-only deadline must not
/// repaint an idle session every retention interval.
///
/// A memory wake is "only" a memory wake when no non-memory contributor is
/// armed, or when the memory deadline lands strictly first. Ties go to the
/// non-memory side so a simultaneous frame or foreground sample is not skipped.
///
/// Pure and free-standing so the decision is testable without a winit event
/// loop, a window, or a GPU.
fn wake_is_memory_only(non_memory_wake: Option<Instant>, memory_wake: Option<Instant>) -> bool {
    match (non_memory_wake, memory_wake) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(non_memory), Some(memory)) => memory < non_memory,
    }
}

#[cfg(windows)]
fn wake_is_foreground_probe_only(
    frame_wake: Option<Instant>,
    foreground_wake: Option<Instant>,
    memory_wake: Option<Instant>,
) -> bool {
    foreground_wake.is_some_and(|foreground| {
        frame_wake.is_none_or(|frame| foreground < frame)
            && memory_wake.is_none_or(|memory| foreground <= memory)
    })
}

impl App {
    #[cfg(windows)]
    pub(super) fn arm_foreground_probe_after_input(&mut self, now: Instant) {
        if self.process_privilege.is_privileged() {
            // When: `process_privilege.is_privileged()` is true, every tab already carries the global warning.
            self.foreground_probe_wake = None;
            return;
        }
        self.foreground_probe_wake =
            Some(super::PendingForegroundProbe { due: now + FOREGROUND_PROCESS_TTL, fixed: true });
    }

    #[cfg(windows)]
    fn arm_foreground_probe_after_output(&mut self, now: Instant) {
        if self.process_privilege.is_privileged() {
            // When: `process_privilege.is_privileged()` is true, foreground output cannot add another warning state.
            self.foreground_probe_wake = None;
            return;
        }
        if self.foreground_probe_wake.is_some_and(|wake| wake.fixed) {
            // When: accepted input already fixed a deadline, output cannot postpone its sample.
            return;
        }
        self.foreground_probe_wake =
            Some(super::PendingForegroundProbe { due: now + FOREGROUND_PROCESS_TTL, fixed: false });
    }

    #[cfg(windows)]
    fn finish_foreground_process_probe(&mut self, now: Instant, warning_active: bool) {
        self.foreground_probe_wake =
            (!self.process_privilege.is_privileged() && warning_active).then_some(
                super::PendingForegroundProbe { due: now + FOREGROUND_PROCESS_TTL, fixed: true },
            );
    }

    #[cfg(windows)]
    fn foreground_probe_is_due(&self, now: Instant) -> bool {
        self.foreground_probe_wake.is_some_and(|wake| wake.due <= now)
    }

    #[cfg(windows)]
    fn refresh_foreground_privileges_if_due(&mut self, now: Instant) -> Vec<WindowId> {
        if !self.foreground_probe_is_due(now) {
            // When: `foreground_probe_is_due(now)` is false, leave every foreground cache untouched.
            return Vec::new();
        }
        self.foreground_probe_wake = None;
        let mut changed_windows = Vec::new();
        let mut warning_active = false;
        for (window_id, window) in &mut self.windows {
            let changed = super::force_refresh_window_tab_privileges(
                &mut window.tabs,
                &window.tab_states,
                &mut window.panes,
                now,
            );
            warning_active |= window.tabs.tabs().iter().any(|tab| tab.foreground_privileged);
            if changed {
                changed_windows.push(*window_id);
            }
        }
        self.finish_foreground_process_probe(now, warning_active);
        changed_windows
    }

    pub(super) fn expire_notifications(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        for ws in self.windows.values_mut() {
            let Some(expires_at) = ws.notification.as_ref().and_then(|bubble| bubble.expires_at)
            else {
                // When: a notification carries no expires_at; nothing in this pass
                // expires it and it contributes no wake deadline.
                continue;
            };
            if expires_at <= now {
                ws.notification = None;
                ws.request_redraw();
            } else {
                // When: expires_at is still ahead of now; min-fold it so the loop
                // wakes exactly when the soonest bubble is due, not later.
                next = Some(next.map_or(expires_at, |cur| cur.min(expires_at)));
            }
        }
        next
    }

    fn scrollbar_snap_deadline(&self) -> Option<Instant> {
        self.windows
            .values()
            .filter(|window| {
                matches!(
                    crate::app::scrollbar_visibility::window_scrollbar_motion(
                        window.renderer.as_ref().map(GpuRenderer::is_software_render_degraded),
                        self.software_render_degrade,
                    ),
                    crate::app::scrollbar_visibility::ScrollbarMotion::Snap
                )
            })
            .filter_map(|window| {
                let drag_pane = window.scrollbar_drag.as_ref().map(|drag| drag.pane_id);
                crate::app::scrollbar_visibility::next_snap_deadline(
                    &window.scrollbar_vis,
                    self.config.appearance.scrollbar,
                    drag_pane,
                )
            })
            .min()
    }

    fn expire_due_scrollbar_snaps(&mut self, now: Instant) -> Vec<WindowId> {
        self.windows
            .iter_mut()
            .filter(|(_, window)| {
                matches!(
                    crate::app::scrollbar_visibility::window_scrollbar_motion(
                        window.renderer.as_ref().map(GpuRenderer::is_software_render_degraded),
                        self.software_render_degrade,
                    ),
                    crate::app::scrollbar_visibility::ScrollbarMotion::Snap
                )
            })
            .filter_map(|(window_id, window)| {
                let drag_pane = window.scrollbar_drag.as_ref().map(|drag| drag.pane_id);
                crate::app::scrollbar_visibility::expire_due_snaps(
                    &mut window.scrollbar_vis,
                    self.config.appearance.scrollbar,
                    drag_pane,
                    now,
                )
                .then_some(*window_id)
            })
            .collect()
    }

    pub(super) fn do_about_to_wait(&mut self, el: &ActiveEventLoop) {
        // Deferred-exit drain: `run_action` (keymap dispatcher) sets
        // `pending_exit` when the user's Cmd+W chain has just closed
        // the last tab of the last window in
        // `quit_on_last_window_close = true` mode. The dispatcher does
        // not have an `ActiveEventLoop` handle, so honoring it here is
        // the first opportunity to call `el.exit()`.
        if self.pending_exit {
            // When: pending_exit was set by any quit or last-window path; clear it
            // before el.exit() so the drain cannot re-enter on a later pass.
            self.pending_exit = false;
            el.exit();
            return;
        }
        self.expire_quit_confirmation();
        self.warm_window_pool_maintain(el);
        if self.runtime_smoke.as_ref().is_some_and(|smoke| smoke.should_maintain_warm_pool()) {
            // When: `runtime_smoke` requests warm-pool maintenance, prove the default spare before adoption.
            let baseline = self.runtime_smoke.as_ref().map(|smoke| smoke.renderer_baseline());
            let snapshot = self.build_memory_snapshot();
            let warm_reported = snapshot.renderers.iter().any(|renderer| renderer.role == "warm");
            let warm_count = self.warm_window_pool.len();
            let live_count = sonicterm_gpu::core::live_renderer_count();
            if baseline.is_some_and(|baseline| {
                warm_count == 1 && warm_reported && live_count == baseline + 2
            }) {
                tracing::info!(
                    warm_count,
                    live_count,
                    "runtime smoke warm renderer created and reported"
                );
                let expected_child = self.warm_window_pool.last().map(|warm| warm.window.id());
                let main_id = self.main_window_id;
                let child = main_id.and_then(|id| {
                    let index = self.windows.get(&id)?.tabs.active_index();
                    self.tear_out_tab(el, index);
                    self.windows.keys().copied().find(|child| Some(*child) != main_id)
                });
                if let Some(child) = child {
                    let adopted_live_count = sonicterm_gpu::core::live_renderer_count();
                    if expected_child != Some(child)
                        || !self.warm_window_pool.is_empty()
                        || baseline.is_none_or(|baseline| adopted_live_count != baseline + 2)
                    {
                        // When: the pool still owns a spare or renderer count changed, production adoption did not consume the reported renderer.
                        if let Some(smoke) = self.runtime_smoke.as_mut() {
                            smoke.fail(RuntimeSmokeFailure::WarmLifecycle);
                        }
                        el.exit();
                        return;
                    }
                    let present_baseline = self
                        .windows
                        .get(&child)
                        .and_then(|window| window.renderer.as_ref())
                        .map(GpuRenderer::successful_frame_count)
                        .unwrap_or(0);
                    if let Some(smoke) = self.runtime_smoke.as_mut() {
                        let _ = smoke.begin_warm_adoption(child, present_baseline);
                    }
                } else if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.fail(RuntimeSmokeFailure::WarmLifecycle);
                    el.exit();
                    return;
                }
            } else if warm_count > 0 {
                // When: a spare exists but count/reporting invariants disagree, fail rather than loop until timeout.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.fail(RuntimeSmokeFailure::WarmLifecycle);
                }
                el.exit();
                return;
            }
        }
        self.sample_pane_retention(Instant::now());
        let notification_wake = self.expire_notifications(Instant::now());
        // Reset the control flow on every pass rather than leaving the previous
        // `WaitUntil` in place: that is what keeps idle CPU near zero. A
        // deadline that has already elapsed re-fires `ResumeTimeReached` on
        // every iteration, so a stale one spins the loop instead of letting it
        // idle. With no contributor armed, idle parks in `Wait` and the app
        // drives no wakes at all.
        //
        // Memory and foreground sampling are timed work, not inherently frames.
        // Their deadline identities stay separate from frame contributors so a
        // sample that changes nothing cannot become a heartbeat redraw. An idle
        // session has neither a frame nor foreground-probe contributor; only the
        // retention cadence wakes it, and that wake remains draw-free.
        let frame_wake = self.frame_wake_deadline(notification_wake);
        #[cfg(windows)]
        let foreground_wake = self.foreground_probe_wake.map(|wake| wake.due);
        #[cfg(not(windows))]
        let foreground_wake = None;
        let non_memory_wake = earliest(frame_wake, foreground_wake);
        let memory_wake = self.memory_sample_deadline();
        self.wake_is_memory_only = wake_is_memory_only(non_memory_wake, memory_wake);
        #[cfg(windows)]
        {
            self.wake_is_foreground_probe_only =
                wake_is_foreground_probe_only(frame_wake, foreground_wake, memory_wake);
        }
        match earliest(non_memory_wake, memory_wake) {
            Some(at) => el.set_control_flow(ControlFlow::WaitUntil(at)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }

    /// When the next memory snapshot is due.
    ///
    /// `None` only before the first sample has been taken, which cannot
    /// persist: the first `do_about_to_wait` samples unconditionally and
    /// records the instant, so the deadline is armed from then on.
    ///
    /// Armed regardless of log level. The level check belongs at the emission
    /// site, not here — a deadline that existed only for sessions already
    /// logging would make the reclamation passes that share this cadence run
    /// at a different rate depending on whether anyone was watching, and those
    /// passes free memory a user gets back either way.
    fn memory_sample_deadline(&self) -> Option<Instant> {
        self.last_retention_sample
            .map(|last| last + crate::app::retention::RETENTION_SAMPLE_INTERVAL)
    }

    /// Earliest instant at which a frame-producing event is due.
    ///
    /// Folds notification expiry, the Cmd+Q confirmation window, the main
    /// window's deferred-redraw frame boundary, each pending child window's
    /// frame boundary, and the cursor-blink phase boundary. Foreground and
    /// retention sampling remain separate because they may produce no frame.
    /// `None` means nothing is armed and the loop may park indefinitely.
    ///
    /// Every contributor min-folds. A deadline is a "wake no later than" bound,
    /// so the earliest one wins; a contributor that overwrote instead of
    /// folding would push an earlier deadline out past its due instant.
    // Ordering: cursor_visible loads Relaxed; a stale read only mis-times the next
    // blink wake, which the following do_about_to_wait pass corrects.
    fn frame_wake_deadline(&self, notification_wake: Option<Instant>) -> Option<Instant> {
        let mut next = earliest(notification_wake, self.scrollbar_snap_deadline());
        #[cfg(target_os = "windows")]
        if let Some(pending) = self.pending_osc52_reassert.as_ref() {
            next = Some(next.map_or(pending.due, |current| current.min(pending.due)));
        }
        // Wake when the Cmd+Q confirmation window expires so a stale first press
        // does not make a much later Cmd+Q quit unexpectedly.
        if let Some(at) = self.quit_hold.deadline() {
            next = Some(next.map_or(at, |cur| cur.min(at)));
        }
        // A redraw deferred for vsync pacing wakes at its upcoming frame
        // boundary: typing latency must still feel instant, and the frame
        // boundary is the tightest budget that preserves vsync alignment.
        if self.pending_redraw {
            if let Some(last_render) = self.main().map(|ws| ws.last_render) {
                let composing = self.main().map(|ws| ws.ime.is_composing()).unwrap_or(false);
                let period = crate::app::effective_frame_period(
                    self.software_render_degrade,
                    composing,
                    self.frame_period,
                );
                let at = last_render + period;
                next = Some(next.map_or(at, |cur| cur.min(at)));
            }
        }
        // same vsync-pacing schedule for any CHILD window that
        // deferred a redraw (PTY-streaming gate or lock-contention
        // backoff). Each torn-out child keys off its own
        // `WindowState.last_render`, so fold each pending child's next
        // frame boundary into the wake deadline. Stale ids (window
        // reaped) are skipped here and pruned in `new_events`.
        for win_id in &self.pending_redraw_windows {
            if let Some(ws) = self.windows.get(win_id) {
                let period = crate::app::effective_frame_period(
                    self.software_render_degrade,
                    ws.ime.is_composing(),
                    self.frame_period,
                );
                let at = ws.last_render + period;
                next = Some(next.map_or(at, |cur| cur.min(at)));
            }
        }
        // The next cursor-blink phase boundary is scheduled through this wake
        // deadline rather than by calling `request_redraw()` from inside the
        // render path, which would spin a tight redraw loop. The renderer
        // returns the exact instant of the next phase bucket, or `None` when
        // blinking is off, the window is unfocused, or no renderer exists.
        if let Some(r) = self.main_renderer() {
            // cursor_visible is per-pane — read from the
            // active pane of the active tab so the DECTCEM flag
            // survives tear-out.
            let cursor_visible = self
                .main()
                .and_then(|ws| {
                    let i = ws.tabs.active_index();
                    let active_id = ws.tab_states.get(i).map(|t| t.active_pane)?;
                    ws.panes
                        .get(&active_id)
                        .map(|p| p.cursor_visible.load(std::sync::atomic::Ordering::Relaxed))
                })
                .unwrap_or(true);
            if cursor_visible {
                let blink = r.next_blink_redraw_at();
                next = match (next, blink) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            }
        }
        next
    }

    #[cfg(test)]
    fn wake_deadline(&self, notification_wake: Option<Instant>) -> Option<Instant> {
        let frame_wake = self.frame_wake_deadline(notification_wake);
        #[cfg(windows)]
        let next = earliest(frame_wake, self.foreground_probe_wake.map(|wake| wake.due));
        #[cfg(not(windows))]
        let next = frame_wake;
        next
    }

    pub(super) fn do_new_events(&mut self, _el: &ActiveEventLoop, cause: winit::event::StartCause) {
        // When `WaitUntil(..)` expires, winit fires
        // `NewEvents(ResumeTimeReached)` and nothing else. Frame contributors
        // must request their repaint here; probe-only wakes sample first and
        // repaint only windows whose foreground state changed.
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            // When: cause matches ResumeTimeReached; winit sends nothing further on
            // its own, so every deferred repaint must be re-requested here.
            let now = Instant::now();
            #[cfg(target_os = "windows")]
            self.reassert_osc52_clipboard_if_due(now);

            // A wake armed solely to sample memory draws nothing.
            //
            // `do_about_to_wait` takes the sample later in this event-loop turn;
            // this branch only suppresses the repaint that a diagnostic wake
            // does not need. An idle session would otherwise
            // repaint every thirty seconds forever purely to record that it
            // was idle: a heartbeat redraw in all but name, and the exact
            // thing this crate's guardrails forbid.
            //
            // Cleared on read. The flag describes the wake that just fired,
            // and leaving it set would suppress the next genuine render wake.
            if std::mem::take(&mut self.wake_is_memory_only) {
                // When: wake_is_memory_only marks a diagnostic-only wake; repainting
                // here would be a heartbeat redraw this crate's guardrails forbid.
                return;
            }
            #[cfg(windows)]
            let foreground_probe_only = std::mem::take(&mut self.wake_is_foreground_probe_only);
            #[cfg(windows)]
            let foreground_changed = self.refresh_foreground_privileges_if_due(now);
            #[cfg(windows)]
            if foreground_probe_only {
                // When: `foreground_probe_only` is true, repaint exactly the windows whose observed state changed.
                for window_id in foreground_changed {
                    if let Some(window) = self.windows.get(&window_id) {
                        window.request_redraw();
                    }
                }
                return;
            }
            let expired_scrollbars = self.expire_due_scrollbar_snaps(now);
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
            #[cfg(windows)]
            for window_id in foreground_changed {
                if Some(window_id) != self.main_window_id {
                    if let Some(window) = self.windows.get(&window_id) {
                        window.request_redraw();
                    }
                }
            }
            for window_id in expired_scrollbars {
                if Some(window_id) != self.main_window_id {
                    if let Some(window) = self.windows.get(&window_id) {
                        window.request_redraw();
                    }
                }
            }
            // also re-request the redraw on every CHILD window
            // that deferred one (vsync gate or lock-contention backoff).
            // We do NOT clear an entry on request — exactly like the main
            // window's `pending_redraw`, the marker is cleared when the
            // child actually renders past the gate in
            // `handle_child_window_event`. Take the set out to avoid
            // borrowing `self.windows` and `self.pending_redraw_windows`
            // at once, prune ids whose window was reaped (so the set can't
            // leak / wake the loop forever), then put the survivors back.
            let pending = std::mem::take(&mut self.pending_redraw_windows);
            self.pending_redraw_windows = pending
                .into_iter()
                .filter(|win_id| match self.windows.get(win_id) {
                    Some(ws) => {
                        ws.request_redraw();
                        true
                    }
                    None => false,
                })
                .collect();
        }
    }

    pub(super) fn do_user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::MenuAction => self.drain_menubar_actions(el),
            UserEvent::OpenScripts => {
                self.drain_open_script_requests();
            }
            UserEvent::OsDrag => self.drain_os_drag(),
            UserEvent::DragMoved => {
                // When: DragMoved arrives; handle_os_drag_moved only drains the
                // cursor mailbox, and the drag chip renders from tab_drag state.
                let _ = self.handle_os_drag_moved();
            }
            UserEvent::DragEnded => {
                // When: DragEnded arrives; handle_os_drag_ended routes the drop
                // internally and returns the outcome only for tests to assert on.
                let _ = self.handle_os_drag_ended();
            }
            UserEvent::RequestRedraw(window_id) => {
                #[cfg(windows)]
                self.arm_foreground_probe_after_output(Instant::now());
                if let Some(window) = self.windows.get(&window_id) {
                    window.request_redraw();
                }
            }
            UserEvent::ClearShapeCache => self.handle_clear_shape_cache(),
            UserEvent::UpdateCheckFinished { level, message } => {
                self.show_notification_for_kind(self.frontmost_kind(), level, message);
            }
            UserEvent::PaneProcessExited { pane_id, was_clean } => {
                self.handle_pane_process_exited(pane_id, was_clean);
            }
            UserEvent::ScriptDraftRejected { message } => {
                self.handle_script_draft_rejected(message);
            }
            UserEvent::ClipboardWrite { text } => {
                self.handle_clipboard_write(text);
            }
            UserEvent::PathProbeFinished(result) => {
                self.handle_path_probe_finished(*result);
            }
            UserEvent::PtyInputRejected { bytes, reason } => {
                self.show_notification_for_kind(
                    self.frontmost_kind(),
                    sonicterm_ui::overlays::NotificationLevel::Error,
                    format!(
                        "Terminal input was not sent ({reason}; {} bytes). Retry after the terminal responds.",
                        bytes.len()
                    ),
                );
            }
            UserEvent::RuntimeSmokeTimeout => {
                // When: `event` is `RuntimeSmokeTimeout`, classify the boundary before exiting.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    // When: `self.runtime_smoke.as_mut()` yields `smoke`, preserve its active boundary.
                    let failure = smoke.timeout_failure();
                    smoke.fail(failure);
                    el.exit();
                    return;
                }
            }
        }
        // Any path above that ran an action may have requested a new
        // top-level window; create it now that we have an ActiveEventLoop.
        self.drain_pending_window_creates(el);
        // Drain deferred OS-drag teardown AFTER `drain_pending_window_creates`
        // so any tear-out-spawn from the `DroppedOnEmpty` branch has produced
        // its new window before cross-window drag-residue cleanup
        // runs. Ordering is the entire point — do not move above.
        self.drain_pending_os_teardown();
    }

    pub(super) fn handle_clipboard_write(&mut self, text: String) {
        #[cfg(not(target_os = "windows"))]
        let _ = self.set_clipboard_text(text);
        #[cfg(target_os = "windows")]
        {
            let previous_text = self.clipboard_text_for_reassert();
            if !self.set_clipboard_text(text.clone()) {
                // When: set_clipboard_text returns false, no successful value exists to reassert.
                return;
            }
            self.pending_osc52_reassert = Some(super::PendingOsc52Reassert {
                text,
                previous_text,
                due: Instant::now() + super::OSC52_CLIPBOARD_REASSERT_DELAY,
            });
        }
    }

    #[cfg(target_os = "windows")]
    pub(super) fn reassert_osc52_clipboard_if_due(&mut self, now: Instant) {
        if self.pending_osc52_reassert.as_ref().is_none_or(|pending| pending.due > now) {
            // When: pending_osc52_reassert is absent or due is after now, do nothing.
            return;
        }
        let pending = self.pending_osc52_reassert.take().expect("due reassertion present");
        let Some(previous_text) = pending.previous_text else {
            // When: previous_text was unavailable, avoid overwriting an unreadable clipboard owner.
            return;
        };
        if self.clipboard_text_for_reassert().as_deref() != Some(previous_text.as_str()) {
            // When: clipboard_text_for_reassert differs from previous_text, preserve that newer owner.
            return;
        }
        let _ = self.set_clipboard_text(pending.text);
    }

    pub(super) fn handle_script_draft_rejected(&mut self, message: String) {
        self.show_notification_for_kind(
            self.frontmost_kind(),
            sonicterm_ui::overlays::NotificationLevel::Warning,
            message,
        );
    }

    /// Drain a `UserEvent::ClearShapeCache`:
    /// an async font fallback family just landed in
    /// [`sonicterm_text::async_fallback::AsyncFallbackLoader`]. Clear every
    /// live renderer's shape / row / line caches (bumping `style_rev`)
    /// and request a redraw on every live window. The next frame
    /// re-walks the fallback chain and the user's tofu cells flip to
    /// real glyphs.
    pub(super) fn handle_clear_shape_cache(&mut self) {
        // main window lives in `self.windows` with `renderer=Some`,
        // so a single iteration covers main + all torn-out children.
        for child in self.windows.values_mut() {
            if let Some(r) = child.renderer.as_mut() {
                r.clear_shape_cache();
                child.request_redraw();
            }
        }
    }

    pub(super) fn drain_open_script_requests(&mut self) -> usize {
        if self.main().is_none() {
            // When: no main window exists yet to host a tab; returning before
            // drain() leaves the requests queued for a later pass.
            return 0;
        }
        let requests = crate::open_script_bridge::drain();
        let count = requests.len();
        for request in requests {
            let title = request
                .launch_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("script")
                .to_string();
            self.new_tab_with_launch(title, super::pane_launch::PaneLaunch::for_script(request));
        }
        if count > 0 && self.main_is_hidden() {
            self.show_main_window();
        }
        count
    }

    pub(super) fn seed_initial_tabs(&mut self) {
        if self.drain_open_script_requests() == 0 {
            self.new_tab("shell");
        }
    }

    pub(super) fn do_resumed(&mut self, el: &ActiveEventLoop) {
        // Fire the one-shot post-resume hook before any window work.
        // macOS uses this slot to install the native NSMenu — by now
        // winit has built the AppKit event loop, so `setMainMenu`
        // sticks. Installing it before `run_app` left AppKit with only
        // the default `Apple, sonicterm-mac` menubar (bug caught by the
        // release-binary smoke).
        if let Some(hook) = self.on_resumed.take() {
            hook();
        }

        let (cols, rows) = sonicterm_grid::grid::bounded_grid_size(
            u64::from(self.config.window.cols),
            u64::from(self.config.window.rows),
        );

        let attrs = super::with_app_icon(super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title(super::NATIVE_WINDOW_TITLE)
                    .with_decorations(true)
                    .with_inner_size(winit::dpi::LogicalSize::new(
                        f32::from(cols) * 9.0
                            + self.config.window.padding_left
                            + self.config.window.padding_right,
                        f32::from(rows) * (self.config.font.size * self.config.font.line_height)
                            + self.config.window.padding_top
                            + self.config.window.padding_bottom
                            + sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT,
                    )),
            ),
            self.config.appearance.backdrop,
            self.config.appearance.software_render_mode,
        ));
        let window = match el.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                // When: `el.create_window(attrs)` returns `Err(error)`, smoke exits while normal startup panics.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    // When: `self.runtime_smoke.as_mut()` yields `smoke`, retain the display failure.
                    tracing::error!(%error, "runtime smoke could not create a window");
                    smoke.fail(RuntimeSmokeFailure::Display);
                    el.exit();
                    return;
                }
                panic!("create window: {error}");
            }
        };
        if let Some(smoke) = self.runtime_smoke.as_mut() {
            smoke.begin_gpu();
        }
        // PANIC (above): `create_window` only fails when winit cannot reach
        // the windowing system at all (no display, broken connection). At
        // app startup this is unrecoverable — the user has no terminal to
        // see an error in. Documented per panic audit.
        // Enable IME so CJK input methods (Pinyin, Japanese, Korean…) can
        // deliver preedit + commit events instead of raw keystrokes.
        window.set_ime_allowed(true);
        super::install_native_window_background(&window, self.theme.colors.background.0.as_str());
        let dpi_scale = f64::from(window_dpi(&window));

        // Gate redraws to the monitor's vsync cadence.
        // `refresh_rate_millihertz` returns e.g. 60_000 for 60Hz,
        // 120_000 for 120Hz ProMotion, etc. A zero or absent value
        // means winit could not determine it (headless, virtual
        // display) — fall back to the 60Hz default seeded by `new`.
        if let Some(monitor) = window.current_monitor() {
            if let Some(mhz) = monitor.refresh_rate_millihertz() {
                if mhz > 0 {
                    // period_us = 1_000_000_000 / mhz
                    let period_us = 1_000_000_000u64 / u64::from(mhz);
                    self.monitor_frame_period = Duration::from_micros(period_us);
                    self.frame_period = self.monitor_frame_period;
                    tracing::debug!(
                        "vsync pacing: monitor reports {}.{:03} Hz, frame period {:?}",
                        mhz / 1000,
                        mhz % 1000,
                        self.frame_period,
                    );
                }
            }
        }

        let renderer_result = GpuRenderer::new(
            window.clone(),
            el,
            &self.theme,
            sonicterm_gpu::core::RendererSettings {
                font_family: &self.config.font.family,
                font_dirs: &self.font_dirs,
                font_size: self.config.font.size,
                line_height_mult: self.config.font.line_height,
                font_weight_scale: self.config.font.effective_weight_scale(),
                subpixel_aa: self.config.font.subpixel_aa,
                padding: [
                    self.config.window.padding_left,
                    self.config.window.padding_right,
                    self.config.window.padding_top,
                    self.config.window.padding_bottom,
                ],
                appearance: sonicterm_gpu::core::SurfaceAppearance {
                    backdrop: self.config.appearance.backdrop,
                    opacity: self.config.appearance.opacity,
                    scrollbar: self.config.appearance.scrollbar,
                    panel_padding: self.config.appearance.panel_padding,
                    software_render_mode: self.config.appearance.software_render_mode,
                },
                role: "main",
            },
        );
        let mut renderer = match renderer_result {
            Ok(renderer) => renderer,
            Err(error) => {
                // When: `renderer_result` is `Err(error)`, smoke exits while normal startup panics.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    // When: `self.runtime_smoke.as_mut()` yields `smoke`, retain the GPU failure.
                    tracing::error!(%error, "runtime smoke could not initialize the renderer");
                    smoke.fail(RuntimeSmokeFailure::Gpu);
                    el.exit();
                    return;
                }
                panic!("init renderer: {error}");
            }
        };
        // Attach the async font fallback
        // loader so frame-time misses on CJK / emoji / nerd-font
        // codepoints trigger a background `request_load` and a
        // `UserEvent::ClearShapeCache` wake-up on completion. Skipped
        // when tests construct the App without a proxy; the existing
        // tofu fallback keeps working in that case.
        if let Some(proxy) = self.event_loop_proxy.clone() {
            super::build_async_fallback_loader_for_proxy(proxy);
            renderer.set_async_loader(());
        }
        // Seed cursor visuals from config so the very first frame draws
        // the user-selected shape rather than the default. Later edits to
        // sonicterm.toml reach the renderer only when the user asks for a
        // reload; nothing watches the file.
        renderer.set_cursor_shape(self.config.terminal.cursor_shape);
        renderer.set_cursor_blink(self.config.terminal.cursor_blink);

        // resolve the no-GPU degrade decision now that the
        // renderer (and its adapter) exists. Combine the config mode with
        // runtime software-rasterizer detection, then clamp the frame period
        // so the CPU isn't asked to rasterize at the monitor's full refresh.
        self.software_render_degrade = crate::app::should_degrade_for_software_render(
            self.config.appearance.software_render_mode,
            renderer.is_software_rendering(),
        );
        renderer.set_software_render_degrade(self.software_render_degrade);
        if let Some(recorder) = &self.breadcrumb_recorder {
            // When: a breadcrumb_recorder is installed; the adapter class is only
            // knowable once the renderer exists, so it is recorded here.
            use sonicterm_logging::breadcrumbs::{
                AdapterClass, BreadcrumbEvent, RendererIdentity, RendererMode,
            };
            let software_adapter = renderer.is_software_rendering();
            let mode = if software_adapter { RendererMode::Software } else { RendererMode::Gpu };
            let adapter =
                if software_adapter { AdapterClass::Software } else { AdapterClass::Hardware };
            let identity = if cfg!(target_os = "windows") && renderer.is_software_render_degraded()
            {
                // When: target_os is windows and the renderer degraded; that pairing
                // gets its own identity so a later report separates it from wgpu.
                RendererIdentity::Software
            } else {
                // When: target_os is not windows, or is_software_render_degraded is
                // false; Wgpu covers hardware and every non-degraded adapter.
                RendererIdentity::Wgpu
            };
            let _ = recorder.record(BreadcrumbEvent::Renderer { identity, mode, adapter });
        }
        if self.software_render_degrade {
            let before = self.frame_period;
            // Resolved from the monitor's own period, not from `frame_period`:
            // that field is the resolved value and would already hold the cap
            // on a re-resolution, making the decision one-way.
            self.frame_period =
                crate::app::software_render_frame_period(true, self.monitor_frame_period);
            tracing::info!(
                detected = renderer.is_software_rendering(),
                mode = ?self.config.appearance.software_render_mode,
                frame_period = ?self.frame_period,
                "software-render degrade engaged: frame cap {:?} -> {:?}",
                before,
                self.frame_period,
            );
        }

        // Register the main window's HWND with
        // the OS-drag backend through the unified entry point so the
        // main and torn-out windows share code paths. No-op on mac.
        let main_id = window.id();
        self.register_window_with_os_drag_backend(main_id, &window);
        // Fire the one-shot window-ready hook (Windows uses this slot
        // to install the muda menubar against the HWND). Best-effort:
        // if the platform can't surface a raw handle, skip the hook
        // and log — the rest of the app still runs.
        if let Some(hook) = self.on_window_ready.take() {
            use raw_window_handle::HasWindowHandle;
            match window.window_handle() {
                Ok(h) => hook(h.as_raw()),
                Err(e) => tracing::warn!("on_window_ready: no raw handle: {e}"),
            }
        }
        renderer.set_titlebar_inset(0.0);
        let _ = super::apply_terminal_window_minimum(&window, &mut renderer);
        // Apply the user's `tab_close_button_color` from sonicterm.toml
        // BEFORE the first frame so a custom always-visible × shows
        // up on the very first paint, not after a config edit.
        renderer.set_tab_close_override(self.config.tab_close_button_color.as_deref());

        // renderer is now owned by `WindowState.renderer`.
        // Insert the main entry BEFORE `new_tab` so `spawn_pane` (which
        // reads cell size through `self.main_renderer()`) sees it.
        // drop any synthetic main entry seeded by tests
        // (`App::__test_synthetic_main`); production `do_resumed` is
        // the authoritative source for `main_window_id`.
        if let Some(prev) = self.main_window_id.take() {
            self.windows.remove(&prev);
        }
        self.main_window_id = Some(main_id);
        let shadow = super::WindowState {
            // Registered when the window is inserted; construction has no
            // governor in scope.
            owner: None,
            role: super::WindowRole::Terminal,
            window: Some(window.clone()),
            renderer: Some(renderer),
            tabs: sonicterm_ui::tabs::TabBar::new(),
            tab_states: Vec::new(),
            panes: std::collections::HashMap::new(),
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            pointer_gesture: None,
            selection: None,
            last_click_time: None,
            last_click_cell: (0, 0),
            click_count: 0,
            select_mode: SelectMode::Cell,
            select_anchor: (0, 0),
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            pty_pressed_keys: std::collections::HashMap::new(),
            last_render: std::time::Instant::now(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale,
            ime: sonicterm_ui::ime::ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            path_probe: super::path_target::PathProbeState::default(),
            notification: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: std::collections::HashMap::new(),
            pending_tear_out_timing: None,
            test_drag_chip_marker: None,
            test_renderer_focus_marker: None,
            test_pane_viewport: None,
        };
        self.insert_window_registered(main_id, shadow);

        // Seed script-file tabs when launch events arrived before the window;
        // otherwise preserve the normal one-shell startup.
        if let Some(smoke) = self.runtime_smoke.as_mut() {
            smoke.begin_pty();
        }
        self.seed_initial_tabs();
        if self.runtime_smoke.is_some() {
            // When: `self.runtime_smoke.is_some()` is true, verify and exercise the smoke PTY.
            let command = self.runtime_smoke.as_ref().map(|smoke| smoke.command().to_vec());
            let active_pane = self
                .main_active_pane_id()
                .and_then(|pane_id| self.main().and_then(|window| window.panes.get(&pane_id)));
            let expected_shell = self.config.terminal.shell.as_deref();
            let smoke_failure = match (active_pane.and_then(|pane| pane.pty.as_ref()), command) {
                (Some(pty), Some(command)) if expected_shell == Some(pty.shell_program_path()) => {
                    pty.send_input_nonblocking(command).err().map(|error| {
                        tracing::error!(%error, "runtime smoke could not queue its shell marker");
                        RuntimeSmokeFailure::Marker
                    })
                }
                _ => Some(RuntimeSmokeFailure::Pty),
            };
            if let Some(failure) = smoke_failure {
                // When: `smoke_failure` contains `failure`, retain that PTY/marker boundary.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.fail(failure);
                }
                el.exit();
                return;
            }
            if let Some(smoke) = self.runtime_smoke.as_mut() {
                smoke.begin_marker_wait();
            }
        }
        self.drain_pending_os_drag_payloads();

        if let Some(recorder) = &self.breadcrumb_recorder {
            // When: a breadcrumb_recorder is installed; Ready marks the end of
            // startup so a later hang is placed before or after the first frame.
            use sonicterm_logging::breadcrumbs::{BreadcrumbEvent, LifecycleEvent};
            let windows = u32::try_from(self.windows.len()).unwrap_or(u32::MAX);
            let panes = u32::try_from(
                self.windows
                    .values()
                    .fold(0usize, |total, window| total.saturating_add(window.panes.len())),
            )
            .unwrap_or(u32::MAX);
            let _ = recorder.record(BreadcrumbEvent::Counts { windows, panes });
            let _ = recorder.record(BreadcrumbEvent::Lifecycle(LifecycleEvent::Ready));
        }

        let (rc, rr) = self.main_renderer().map(|r| r.cells()).unwrap_or((0, 0));
        tracing::info!(
            "SonicTerm ready. theme={} keymap={} bindings={} grid={}x{}",
            self.theme.name,
            self.keymap.meta.name,
            self.keymap.bindings.len(),
            rc,
            rr,
        );
        window.request_redraw();
    }
}

#[cfg(test)]
#[path = "event_loop_tests.rs"]
mod event_loop_tests;
