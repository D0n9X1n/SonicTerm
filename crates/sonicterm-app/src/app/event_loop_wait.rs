//! Owner-local wait deadlines, due work and wake servicing.

use super::*;

impl App {
    pub(in crate::app) fn expire_notifications(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        for window in self.windows.values_mut() {
            let Some(expires_at) =
                window.notification.as_ref().and_then(|bubble| bubble.expires_at)
            else {
                // When: a notification carries no expires_at; nothing in this pass
                // expires it and it contributes no wake deadline.
                continue;
            };
            if expires_at <= now {
                window.notification = None;
                window.mark_redraw(super::super::redraw::RedrawCause::Chrome);
                if window.frame_deadlines_allowed() && !window.redraw.request_in_flight {
                    window.redraw.request_in_flight = true;
                    window.request_window_redraw();
                }
            } else {
                // When: expires_at is still ahead of now; min-fold it so the loop
                // wakes exactly when the soonest bubble is due, not later.
                if window.frame_deadlines_allowed() {
                    next = Some(next.map_or(expires_at, |cur| cur.min(expires_at)));
                }
            }
        }
        next
    }

    /// Service due scrollbar idle deadlines in every window that may present,
    /// Snap and Fade alike; returns the windows that need a frame for it.
    pub(in crate::app) fn expire_due_scrollbar_idle(&mut self, now: Instant) -> Vec<WindowId> {
        self.windows
            .iter_mut()
            .filter(|(_, window)| window.frame_deadlines_allowed())
            .filter_map(|(window_id, window)| {
                let motion = crate::app::scrollbar_visibility::window_scrollbar_motion(
                    window.renderer.as_ref().map(GpuRenderer::is_software_render_degraded),
                    self.software_render_degrade,
                );
                let drag_pane = window.scrollbar_drag.as_ref().map(|drag| drag.pane_id);
                crate::app::scrollbar_visibility::expire_due_idle(
                    &mut window.scrollbar_vis,
                    self.config.appearance.scrollbar,
                    drag_pane,
                    motion,
                    now,
                )
                .then_some(*window_id)
            })
            .collect()
    }

    pub(in crate::app) fn do_about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Deferred-exit drain: a quit action, or a close that leaves no active
        // terminal window, sets `pending_exit` from a path with no
        // `ActiveEventLoop` handle, so this is the first chance to call
        // `event_loop.exit()`.
        if self.pending_exit {
            // When: pending_exit was set by any quit or last-window path; clear it
            // before event_loop.exit() so the drain cannot re-enter on a later pass.
            self.pending_exit = false;
            event_loop.exit();
            return;
        }
        if self.drive_gpu_fault_smoke(Instant::now()) {
            // When: drive_gpu_fault_smoke reaches a terminal verdict, keep it for the shell runner.
            event_loop.exit();
            return;
        }
        self.service_gpu_recovery(event_loop, Instant::now());
        self.clear_closed_broadcast_source();
        self.drain_winit_file_drops();
        // A report that waited for a selection or copy mode is shown once neither holds the pane.
        self.apply_deferred_memory_report();
        self.expire_quit_confirmation();
        if self.runtime_smoke.as_ref().is_some_and(|smoke| smoke.needs_fresh_window()) {
            // When: smoke needs a fresh HWND, disable replenishment rather than accidentally adopting another spare.
            self.config.window.warm_window_pool = 0;
            self.warm_window_pool.clear();
            let request = self.window_request(self.main_window_id);
            self.create_new_terminal_window(event_loop, request);
            let child = self.windows.keys().copied().find(|id| Some(*id) != self.main_window_id);
            let Some(child) = child else {
                // When: child is absent after creation, the fresh native lifecycle has failed.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.fail(RuntimeSmokeFailure::WarmLifecycle);
                }
                event_loop.exit();
                return;
            };
            let native = self.windows.get(&child).and_then(|window| window.window.clone());
            if native
                .as_ref()
                .is_none_or(|native| !self.smoke_check_native_title(native, "#3 SonicTerm"))
                || !self.smoke_exercise_window_name(child)
            {
                // When: native title readback or smoke_exercise_window_name fails, preserve its display-boundary error.
                event_loop.exit();
                return;
            }
            let baseline = self
                .windows
                .get(&child)
                .and_then(|window| window.renderer.as_ref())
                .map(GpuRenderer::successful_frame_count)
                .unwrap_or(0);
            if let Some(smoke) = self.runtime_smoke.as_mut() {
                // When: runtime_smoke remains installed, bind the fresh child before accepting a presentation.
                let _ = smoke.begin_fresh_window(child, baseline);
            }
            if let Some(native) = native {
                crate::app::frame_counters::request_native_redraw(&native);
            }
        }
        self.warm_window_pool_maintain(event_loop);
        if self.drive_gpu_recovery_smoke(event_loop, Instant::now()) {
            // When: `drive_gpu_recovery_smoke` is terminal, preserve its verdict for the shell runner.
            event_loop.exit();
            return;
        }
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
                // When: `baseline`, `warm_count`, `warm_reported`, and `live_count` prove one reportable spare, adopt it.
                tracing::info!(
                    warm_count,
                    live_count,
                    "runtime smoke warm renderer created and reported"
                );
                let expected_child = self.warm_window_pool.last().map(|warm| warm.window.id());
                let main_id = self.main_window_id;
                self.new_tab("runtime smoke warm child");
                let child = main_id.and_then(|id| {
                    let index = self.windows.get(&id)?.tabs.active_index();
                    self.tear_out_tab(event_loop, index);
                    self.windows.keys().copied().find(|child| Some(*child) != main_id)
                });
                if let Some(child) = child {
                    // When: `child` exists after tear-out, verify it adopted the exact warm window before presenting.
                    let adopted_live_count = sonicterm_gpu::core::live_renderer_count();
                    if expected_child != Some(child)
                        || !self.warm_window_pool.is_empty()
                        || baseline.is_none_or(|baseline| adopted_live_count != baseline + 2)
                    {
                        // When: `expected_child`, `warm_window_pool`, or `adopted_live_count` disagrees with `baseline`, adoption failed.
                        if let Some(smoke) = self.runtime_smoke.as_mut() {
                            smoke.fail(RuntimeSmokeFailure::WarmLifecycle);
                        }
                        event_loop.exit();
                        return;
                    }
                    let native = self.windows.get(&child).and_then(|state| state.window.clone());
                    if native
                        .as_ref()
                        .is_none_or(|native| !self.smoke_check_native_title(native, "#2 SonicTerm"))
                        || !self.smoke_exercise_window_name(child)
                    {
                        // When: native child title or its rename/reset fails, do not credit warm adoption.
                        event_loop.exit();
                        return;
                    }
                    let present_baseline = self
                        .windows
                        .get(&child)
                        .and_then(|window| window.renderer.as_ref())
                        .map(GpuRenderer::successful_frame_count)
                        .unwrap_or(0);
                    if let Some(smoke) = self.runtime_smoke.as_mut() {
                        // When: `smoke` remains installed, bind the adopted child and its presentation baseline.
                        let _ = smoke.begin_warm_adoption(child, present_baseline);
                    }
                } else if let Some(smoke) = self.runtime_smoke.as_mut() {
                    // When: `child` is absent but `smoke` remains installed, record warm-lifecycle failure.
                    smoke.fail(RuntimeSmokeFailure::WarmLifecycle);
                    event_loop.exit();
                    return;
                }
            } else if warm_count > 0 {
                // When: `warm_count` is nonzero but creation/reporting invariants disagree, fail instead of timing out.
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.fail(RuntimeSmokeFailure::WarmLifecycle);
                }
                event_loop.exit();
                return;
            }
        }
        self.sample_pane_retention(Instant::now());
        let _ = self.expire_notifications(Instant::now());
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
        let now = self.dispatch_now();
        let motion_wake = self.flush_pointer_motion(now);
        let mut due = self.refresh_frame_due_work_at(now);
        // Links follow the modes the frame deadlines above already read; the deadlines do not depend on it.
        self.sync_display_links();
        if let Some(deadline) = self.sync_status_line(now) {
            // A strip sample or pending path check is due; wake for it without a frame.
            due.push(super::super::redraw::DueWork {
                owner: None,
                cause: super::super::redraw::DueCause::StatusLine,
                deadline,
            });
        }
        // A font, reload or DPI change resized the About pane without a frame yet: lay it out again,
        // so its rows and its accessibility tree both follow the new width.
        self.relayout_about_if_resized();
        // Accessibility trees are built here, once per dispatch after its redraws, never in one.
        if let Some(deadline) = self.flush_accessibility(now) {
            // A queued announcement becomes ready; wake to deliver it.
            due.push(super::super::redraw::DueWork {
                owner: None,
                cause: super::super::redraw::DueCause::StatusLine,
                deadline,
            });
        }
        if let Some(deadline) = self.sync_taskbar_progress(now) {
            // A progress value waits for its surface's trailing edge; wake to deliver it.
            due.push(super::super::redraw::DueWork {
                owner: None,
                cause: super::super::redraw::DueCause::TaskbarProgress,
                deadline,
            });
        }
        if let Some(deadline) = self.memory_sample_deadline() {
            due.push(super::super::redraw::DueWork {
                owner: None,
                cause: super::super::redraw::DueCause::Memory,
                deadline,
            });
        }
        if let Some(deadline) = motion_wake {
            due.push(super::super::redraw::DueWork {
                owner: None,
                cause: super::super::redraw::DueCause::PointerMotion,
                deadline,
            });
        }
        if let Some(deadline) = self.gpu_fault_smoke_deadline() {
            due.push(super::super::redraw::DueWork {
                owner: None,
                cause: super::super::redraw::DueCause::Smoke,
                deadline,
            });
        }
        if let Some(deadline) = self.gpu_recovery_smoke_deadline() {
            due.push(super::super::redraw::DueWork {
                owner: None,
                cause: super::super::redraw::DueCause::Smoke,
                deadline,
            });
        }
        if let Some(deadline) = self.gpu_recovery_deadline() {
            due.push(super::super::redraw::DueWork {
                owner: None,
                cause: super::super::redraw::DueCause::GpuRecovery,
                deadline,
            });
        }
        self.redraw_due = due;
        match self.redraw_due.iter().map(|work| work.deadline).min() {
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
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
    pub(super) fn memory_sample_deadline(&self) -> Option<Instant> {
        self.last_retention_sample
            .map(|last| last + crate::app::retention::RETENTION_SAMPLE_INTERVAL)
    }

    /// Service captured transitions before recomputing the next wait, including non-timer event turns.
    pub(in crate::app) fn refresh_frame_due_work_at(
        &mut self,
        now: Instant,
    ) -> Vec<super::super::redraw::DueWork> {
        self.service_redraw_due(now);
        self.frame_due_work_at(now)
    }

    /// Preserve existing test callers while production supplies the wait fold's clock explicitly.
    #[cfg(test)]
    pub(in crate::app) fn frame_due_work(&self) -> Vec<super::super::redraw::DueWork> {
        self.frame_due_work_at(Instant::now())
    }

    /// Collect typed frame and maintenance deadlines without erasing their window identity.
    // Ordering: cursor_visible Relaxed reads only choose whether a blink deadline exists.
    pub(in crate::app) fn frame_due_work_at(
        &self,
        now: Instant,
    ) -> Vec<super::super::redraw::DueWork> {
        use super::super::redraw::{DueCause, DueWork};
        let mut due = Vec::new();
        if let Some(deadline) = self.quit_hold.deadline() {
            due.push(DueWork { owner: None, cause: DueCause::QuitHold, deadline });
        }
        #[cfg(windows)]
        if let Some(deadline) = self.foreground_schedule.next_deadline() {
            due.push(DueWork { owner: None, cause: DueCause::Foreground, deadline });
        }
        #[cfg(windows)]
        if let Some(pending) = self.pending_osc52_reassert.as_ref() {
            due.push(DueWork { owner: None, cause: DueCause::Osc52, deadline: pending.due });
        }
        for (id, window) in &self.windows {
            #[cfg(target_os = "macos")]
            if let Some(deadline) = window.surface_probe_deadline() {
                due.push(DueWork { owner: Some(*id), cause: DueCause::SurfaceProbe, deadline });
            }
            if !window.frame_deadlines_allowed() {
                // When: `window` is suppressed, no frame-family deadline can wake it.
                continue;
            }
            if self.tab_bar_visible {
                due.extend(window.command_badge_due(*id, now));
            }
            // An idle window's image atlas is released by deadline alone; its deadline joins the wake.
            if window.image_atlas_release_collectable() {
                if let Some(deadline) = window.image_atlas_release_deadline() {
                    due.push(DueWork {
                        owner: Some(*id),
                        cause: DueCause::ImageAtlasRelease,
                        deadline,
                    });
                }
            }
            if window.redraw.deferred && !window.redraw.request_in_flight {
                let period = super::super::effective_frame_period(
                    self.software_render_degrade,
                    window.ime.is_composing(),
                    window.redraw.monitor_period,
                );
                // A Sync-held window wakes at its earliest held deadline or its cap, not its pacing floor.
                let deadline = if window.redraw.sync_hold() {
                    window.sync_wake_at(now)
                } else {
                    // When: `sync_hold()` is false, the pacing floor or link ceiling decides the wake.
                    window.frame_deadline(period, self.software_render_degrade)
                };
                due.push(DueWork { owner: Some(*id), cause: DueCause::Frame, deadline });
            }
            if let Some(deadline) =
                window.notification.as_ref().and_then(|bubble| bubble.expires_at)
            {
                due.push(DueWork { owner: Some(*id), cause: DueCause::Notification, deadline });
            }
            // Snap and Fade both settle until one idle deadline; this owner alone services it.
            if let Some(deadline) = super::super::scrollbar_visibility::next_idle_deadline(
                &window.scrollbar_vis,
                self.config.appearance.scrollbar,
                window.scrollbar_drag.as_ref().map(|drag| drag.pane_id),
            ) {
                due.push(DueWork { owner: Some(*id), cause: DueCause::Scrollbar, deadline });
            }
            if Some(*id) == self.main_window_id {
                let cursor = window
                    .tab_states
                    .get(window.tabs.active_index())
                    .and_then(|tab| window.panes.get(&tab.active_pane))
                    .is_some_and(|pane| {
                        pane.cursor_visible.load(std::sync::atomic::Ordering::Relaxed)
                    });
                if cursor {
                    if let Some(deadline) = window
                        .renderer
                        .as_ref()
                        .and_then(|renderer| renderer.next_blink_redraw_at())
                    {
                        due.push(DueWork { owner: Some(*id), cause: DueCause::Cursor, deadline });
                    }
                }
            }
        }
        due
    }

    #[cfg(test)]
    pub(super) fn frame_wake_deadline(
        &self,
        notification_wake: Option<Instant>,
    ) -> Option<Instant> {
        earliest(notification_wake, self.frame_due_work().iter().map(|work| work.deadline).min())
    }

    #[cfg(test)]
    pub(super) fn wake_deadline(&self, notification_wake: Option<Instant>) -> Option<Instant> {
        self.frame_wake_deadline(notification_wake)
    }

    pub(in crate::app) fn do_new_events(
        &mut self,
        _el: &ActiveEventLoop,
        cause: winit::event::StartCause,
    ) {
        self.service_resume_time(&cause);
    }

    /// On a timer wake, service every elapsed due identity at the dispatch clock's instant, the
    /// clock admission and the wait fold read, so all three agree on which deadlines have passed.
    pub(in crate::app) fn service_resume_time(&mut self, cause: &winit::event::StartCause) {
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            // Service every elapsed identity without turning maintenance into a repaint.
            let now = self.dispatch_now();
            self.service_redraw_due(now);
        }
    }
}

#[cfg(test)]
#[path = "event_loop_wait_tests.rs"]
mod event_loop_wait_tests;
