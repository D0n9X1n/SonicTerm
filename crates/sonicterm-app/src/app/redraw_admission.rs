//! Source-window redraw admission, snapshots and typed completion.

use super::*;

impl App {
    /// Device-stop reporting outranks occlusion and topology parking, then the owner-local pacing gate runs.
    pub(in crate::app) fn begin_window_redraw(&mut self, id: WindowId, now: Instant) -> bool {
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
                // The refusal is `Held`; the reset that follows clears a held permit as its own event.
                window.redraw.timeline.note_admission(
                    super::super::AdmissionDecisionV1::Held,
                    None,
                    None,
                );
                window.redraw.invalidate_link_pacing();
                return false;
            }
            if window.redraw.native_occluded || window.redraw.backend_occluded {
                // When: `native_occluded` or `backend_occluded` suppresses this owner, even recovery dirt waits without pane locks.
                window.redraw.timeline.note_admission(
                    super::super::AdmissionDecisionV1::Held,
                    None,
                    None,
                );
                return false;
            }
            if window.redraw.stopped_generation.is_some() {
                // When: `stopped_generation` is set, validate recovery; ordinary usable frames need no error-record snapshot.
                let snapshot = renderer.device_error_snapshot();
                if !window.accept_device_recovery(&snapshot) {
                    // When: `accept_device_recovery` refuses the live snapshot, no cause may bypass the stopped generation.
                    window.redraw.timeline.note_admission(
                        super::super::AdmissionDecisionV1::Held,
                        None,
                        None,
                    );
                    return false;
                }
            }
        }
        if !window.frame_deadlines_allowed() {
            // When: `window` is hidden, occluded, parked, or stopped, it contributes no frame attempt or deadline.
            window.redraw.timeline.note_admission(
                super::super::AdmissionDecisionV1::Held,
                None,
                None,
            );
            return false;
        }
        if !window.redraw.has_pending() {
            window.mark_redraw(RedrawCause::Expose);
        }
        let period = super::super::effective_frame_period(
            self.software_render_degrade,
            window.ime.is_composing(),
            window.redraw.monitor_period,
        );
        let software = self.software_render_degrade;
        // A stored mode decides; otherwise a Streaming deferral would store `Link` when a link is installed.
        let mode =
            window.redraw.pacing.unwrap_or(if window.display_link.source.is_some() && !software {
                super::super::display_link::PacingMode::Link
            } else {
                // No link is installed or the software path runs, so streaming stays on the timer.
                super::super::display_link::PacingMode::Timer
            });
        // The streaming check's inputs, kept so a link-paced admission is classified from the same values.
        let mut streaming_inputs = None;
        // The watched pane's generation, loaded beside the streaming check only when that check runs.
        let mut admission_check = None;
        let rule = super::super::frame_counters::defer_rule(
            || window.redraw.timeout_pending && now < window.last_render + period,
            || window.contention_blocks_redraw(now, period, software),
            || window.sync_defers(now),
            || {
                let input_pending = window.redraw.input_pending();
                admission_check = window.watched_observation();
                let output_advanced = window.visible_output_advanced();
                streaming_inputs = Some((input_pending, output_advanced));
                match mode {
                    super::super::display_link::PacingMode::Link => {
                        super::super::frame_pacing::streaming_work(
                            input_pending,
                            output_advanced,
                            software,
                        ) && !window.redraw.link_permit_valid()
                            && now < window.link_ceiling(period, software)
                    }
                    super::super::display_link::PacingMode::Timer => {
                        super::super::should_defer_streaming_redraw(
                            input_pending,
                            output_advanced,
                            software,
                            now.saturating_duration_since(window.pacing_clock(software)),
                            period,
                        )
                    }
                }
            },
        );
        let defer = rule.is_some();
        if let Some(counters) = window.redraw.frame_counters.as_deref_mut() {
            // the App's gate is on, count the winning deferral rule, or the attempt.
            match rule {
                Some(rule) => counters.note_defer(rule),
                None => counters.attempts += 1,
            }
        }
        window.redraw.deferred_rule = rule;
        if let Some(rule) = rule {
            // A winning rule's decision is `Deferred`; the permit is untouched.
            let decision = super::super::AdmissionDecisionV1::Deferred(
                super::super::echo_timeline::defer_reason(rule),
            );
            window.redraw.timeline.note_admission(decision, None, admission_check);
        }
        match rule {
            Some(super::super::frame_counters::DeferRule::Streaming) => {
                window.redraw.store_pacing(mode)
            }
            Some(super::super::frame_counters::DeferRule::Sync) => {
                // No pacing mode is stored; a stored `Link` waits for the release.
                window.note_sync_deferral(now);
            }
            Some(_) => window.redraw.store_pacing(super::super::display_link::PacingMode::Timer),
            None => {
                window.record_attempt_sync_resets();
                let mut decision = super::super::AdmissionDecisionV1::Admitted;
                if window.redraw.pacing == Some(super::super::display_link::PacingMode::Link) {
                    // A link-paced admission is classified once, from the inputs the streaming check read.
                    let streaming =
                        streaming_inputs.is_some_and(|(input_pending, output_advanced)| {
                            super::super::frame_pacing::streaming_work(
                                input_pending,
                                output_advanced,
                                software,
                            )
                        });
                    let ticked = window.redraw.link_permit_valid();
                    if streaming {
                        // A streaming link-paced admission was made by a tick's permit or the ceiling.
                        decision = if ticked {
                            let tick_seq =
                                window.redraw.timeline.permit_tick_seq(window.redraw.link_permit);
                            super::super::AdmissionDecisionV1::Permit { tick_seq }
                        } else {
                            // When: `ticked` is false, the fallback ceiling admitted the frame.
                            super::super::AdmissionDecisionV1::Fallback
                        };
                    }
                    if let (true, Some(counters)) =
                        (streaming, window.redraw.frame_counters.as_deref_mut())
                    {
                        // the App's gate is on, a streaming admission counts as a tick or a fallback.
                        if ticked {
                            counters.display_link_admissions += 1;
                        } else {
                            // When: `ticked` is false, the fallback ceiling admitted the frame, not a tick.
                            counters.display_link_fallbacks += 1;
                        }
                    }
                }
                window.redraw.pacing = None;
                let held = window.redraw.link_permit.take();
                // A `Permit` admission consumes the permit; any other admission discards a held one.
                let consumed = matches!(decision, super::super::AdmissionDecisionV1::Permit { .. });
                let discarded = held.filter(|_| !consumed);
                window.redraw.timeline.note_admission(decision, discarded, admission_check);
            }
        }
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
    pub(in crate::app) fn snapshot_window_redraw(&mut self, id: WindowId) -> Option<FrameSnapshot> {
        self.snapshot_window_redraw_at(id, Instant::now())
    }

    /// Capture future badge transitions before the frame samples them, not after presentation.
    pub(in crate::app) fn snapshot_window_redraw_at(
        &mut self,
        id: WindowId,
        now: Instant,
    ) -> Option<FrameSnapshot> {
        let window = self.windows.get_mut(&id)?;
        let snapshot = window.capture_redraw_snapshot();
        window.redraw.attempt_causes = Some(snapshot.causes);
        // `frame` measures CPU-side time from here to settlement, on the real clock.
        window.redraw.attempt_started = Some(Instant::now());
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
    pub(in crate::app) fn finish_window_redraw(
        &mut self,
        id: WindowId,
        snapshot: &FrameSnapshot,
        outcome: FrameSettlement,
        at: Instant,
    ) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.complete_attempt(snapshot, outcome, at, self.software_render_degrade);
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

    /// The instant every pacing read uses, from the App's dispatch clock.
    pub(in crate::app) fn dispatch_now(&self) -> Instant {
        (self.dispatch_clock)()
    }

    /// Admit or defer one `RedrawRequested` for either window role at the dispatch clock's instant.
    pub(in crate::app) fn admit_window_redraw(&mut self, id: WindowId) -> bool {
        let now = self.dispatch_now();
        self.begin_window_redraw(id, now)
    }

    /// Complete one renderer call for either window role at the dispatch clock's instant.
    pub(in crate::app) fn complete_window_redraw(
        &mut self,
        id: WindowId,
        snapshot: &FrameSnapshot,
        outcome: FrameSettlement,
    ) {
        let at = self.dispatch_now();
        self.finish_window_redraw(id, snapshot, outcome, at);
    }
}

#[cfg(test)]
#[path = "redraw_admission_tests.rs"]
mod redraw_admission_tests;
