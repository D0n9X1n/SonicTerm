//! Owner-local cause generations, attempt clocks and settlement transitions.

use super::*;

impl WindowRedrawState {
    /// Record one cause; only a topology-capable cause may unpark structural invalidity.
    pub(in crate::app) fn mark(&mut self, cause: RedrawCause) {
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

    /// Note a `Resized` outcome: only a changed surface size owes a frame that forces any hold.
    pub(in crate::app) fn note_resize(&mut self, outcome: sonicterm_gpu::core::ResizeOutcome) {
        if outcome == sonicterm_gpu::core::ResizeOutcome::Changed {
            // A new surface size must reach the screen even mid-update.
            self.resize_pending = true;
        }
    }

    /// Whether the admitted attempt settles a status line change and nothing else: no other cause
    /// is unsettled, no forced resize or surface frame is owed, and no content retry is carried.
    pub(in crate::app) fn status_only_attempt(&self) -> bool {
        if self.resize_pending || self.surface_recovery_pending || self.retry_content {
            // When: resize_pending, surface_recovery_pending or retry_content is set, it is not strip-only.
            return false;
        }
        let captured = self.attempt_causes.map_or(self.pending, |snapshot| snapshot.0);
        let differs = |index: usize| captured[index] != self.observed[index];
        differs(RedrawCause::StatusLine as usize)
            && (0..CAUSES).all(|index| index == RedrawCause::StatusLine as usize || !differs(index))
    }

    /// Whether `cause` has a generation no attempt has settled yet.
    pub(in crate::app) fn cause_pending(&self, cause: RedrawCause) -> bool {
        self.pending[cause as usize] != self.observed[cause as usize]
    }

    /// Whether a synchronized-output hold keeps this window's frame back; the link does not run.
    ///
    /// The winning `Sync` rule alone decides, not `deferred`: an early wake that clears `deferred`
    /// before admission re-evaluates must not restart a stored `Link`. Only admission, which
    /// rewrites `deferred_rule`, or settlement, which clears it, ends the hold.
    pub(in crate::app) fn sync_hold(&self) -> bool {
        self.deferred_rule == Some(super::super::frame_counters::DeferRule::Sync)
    }

    /// Whether an input generation has not yet spent its immediate-attempt privilege.
    pub(in crate::app) fn input_pending(&self) -> bool {
        self.pending[RedrawCause::Input as usize] != self.observed[RedrawCause::Input as usize]
    }

    /// Adopt a monitor rate in millihertz; an unavailable or zero rate keeps the last known period.
    pub(in crate::app) fn apply_monitor_rate(&mut self, rate_millihertz: Option<u32>) {
        if let Some(rate) = rate_millihertz.filter(|rate| *rate > 0) {
            self.monitor_period = Duration::from_micros(1_000_000_000 / u64::from(rate));
        }
    }

    /// Whether `snapshot` carries an input generation this state has not yet observed.
    pub(in crate::app) fn captures_new_input(&self, snapshot: CauseSnapshot) -> bool {
        snapshot.0[RedrawCause::Input as usize] != self.observed[RedrawCause::Input as usize]
    }

    /// Whether any captured request remains unsettled.
    pub(in crate::app) fn has_pending(&self) -> bool {
        self.pending != self.observed
    }

    /// Capture cause identities without consuming later work.
    pub(in crate::app) fn snapshot(&self) -> CauseSnapshot {
        CauseSnapshot(self.pending)
    }

    /// Test-only: the pending generation of one cause, so a test can tell which cause was marked.
    #[cfg(test)]
    pub(in crate::app) fn cause_generation(&self, cause: RedrawCause) -> u64 {
        self.pending[cause as usize]
    }

    /// Settle only captured causes; every real attempt consumes input immediacy, even a retry.
    pub(in crate::app) fn settle(
        &mut self,
        snapshot: CauseSnapshot,
        outcome: FrameSettlement,
        now: Instant,
    ) {
        if let Some(counters) = self.frame_counters.as_deref_mut() {
            // the App's gate is on, every settled attempt counts its outcome.
            counters.record_settlement(outcome, now);
        }
        self.attempt_causes = None;
        // Read before `observed` moves: a captured cause not yet observed was settled by this
        // attempt. Strip, cursor-blink and retry causes alone are not content.
        const NOT_CONTENT: [RedrawCause; 4] = [
            RedrawCause::StatusLine,
            RedrawCause::Cursor,
            RedrawCause::SurfaceRetry,
            RedrawCause::AtlasRetry,
        ];
        let content = self.retry_content
            || (0..CAUSES).any(|index| {
                NOT_CONTENT.iter().all(|cause| *cause as usize != index)
                    && snapshot.0[index] != self.observed[index]
            });
        let elapsed_us =
            self.attempt_started.take().map_or(0, |started| started.elapsed().as_micros() as u64);
        self.retry_content = match outcome {
            FrameSettlement::Presented => {
                if let Some((frames, total_us)) = self.presented_frames.as_mut().filter(|_| content)
                {
                    *frames += 1;
                    *total_us = total_us.saturating_add(elapsed_us);
                }
                false
            }
            // A retry re-attempts this frame; it counts only if this attempt carried content.
            FrameSettlement::Retry(_)
            | FrameSettlement::SurfaceRetry(_)
            | FrameSettlement::Failed => content,
            FrameSettlement::Cached | FrameSettlement::Settled | FrameSettlement::Stopped(_) => {
                false
            }
        };
        self.observed[RedrawCause::Input as usize] = snapshot.0[RedrawCause::Input as usize];
        self.deferred = false;
        self.deferred_rule = None;
        self.request_in_flight = false;
        self.timeout_pending = false;
        match outcome {
            FrameSettlement::Presented | FrameSettlement::Cached | FrameSettlement::Settled => {
                self.observed = snapshot.0;
                // A successful outcome ends the held stretch; a failure keeps it, so the cap holds.
                self.sync_stretch_start = None;
                if outcome == FrameSettlement::Presented {
                    // Only a presented frame meets a resize or a recovered surface.
                    self.resize_pending = false;
                    self.surface_recovery_pending = false;
                }
                if matches!(outcome, FrameSettlement::Presented | FrameSettlement::Cached) {
                    self.last_present = Some(now);
                }
            }
            FrameSettlement::Stopped(generation) => {
                self.stopped_generation = Some(generation);
                self.invalidate_link_pacing();
                self.cancel_surface_probe();
            }
            FrameSettlement::SurfaceRetry(reason) => {
                self.mark(RedrawCause::SurfaceRetry);
                match reason {
                    SurfaceRetryReason::Timeout => {
                        self.timeout_pending = true;
                        self.deferred = true;
                        self.deferred_rule = Some(super::super::frame_counters::DeferRule::Timeout);
                        self.store_pacing(super::super::display_link::PacingMode::Timer);
                    }
                    SurfaceRetryReason::Occluded => {
                        self.backend_occluded = true;
                        self.invalidate_link_pacing();
                        #[cfg(target_os = "macos")]
                        if !self.native_occluded {
                            self.surface_probe_at = Some(now + SURFACE_PROBE_PERIOD);
                        }
                    }
                    SurfaceRetryReason::Outdated
                    | SurfaceRetryReason::Suboptimal
                    | SurfaceRetryReason::SurfaceLost => {
                        // The presenter recovered the surface and owns the retry; the next frame is forced.
                        self.surface_recovery_pending = true;
                    }
                }
            }
            FrameSettlement::Retry(cause) => self.mark(cause),
            FrameSettlement::Failed => {
                // When: `Failed` retains pending work, the failure itself must not create a retry timer.
            }
        }
    }

    /// Complete one renderer call: the attempt clock always moves, and the streaming clock moves
    /// unless a hardware attempt for new input settled with nothing presented and no retry pending.
    pub(in crate::app) fn complete_attempt(
        &mut self,
        clocks: AttemptClocks<'_>,
        snapshot: &FrameSnapshot,
        outcome: FrameSettlement,
        at: Instant,
        software: bool,
    ) {
        // Read before `settle`, which records the snapshot's input generation as observed.
        let new_input = self.captures_new_input(snapshot.causes);
        *clocks.last_render = at;
        self.settle(snapshot.causes, outcome, at);
        let exempt = outcome == FrameSettlement::Settled
            && new_input
            && !software
            && !self.timeout_pending
            && !clocks.retry_armed;
        if exempt {
            if let Some(counters) = self.frame_counters.as_deref_mut() {
                // the App's gate is on, each attempt that kept the streaming clock is counted.
                counters.stream_clock_exempt += 1;
            }
        } else {
            // When: `exempt` is false, the attempt drew, retried, failed, lacked new input or ran in software, so it paces.
            *clocks.stream_clock = at;
        }
    }

    /// Cancel only the exceptional backend probe, never output or input identities.
    pub(in crate::app) fn cancel_surface_probe(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.surface_probe_at = None;
        }
    }

    /// A native event supersedes backend occlusion; return only a genuine transition back to visibility.
    /// `now` stamps the start of a covered stretch; a repeated cover keeps the first stamp.
    pub(in crate::app) fn observe_native_occlusion(
        &mut self,
        occluded: bool,
        now: Instant,
    ) -> bool {
        let was_occluded = self.native_occluded || self.backend_occluded;
        self.native_occluded = occluded;
        self.backend_occluded = false;
        self.cancel_surface_probe();
        if occluded {
            // A repeated cover keeps the earlier `occluded_since`, so it cannot delay the trim.
            self.occluded_since.get_or_insert(now);
            self.deferred = false;
            self.timeout_pending = false;
            self.invalidate_link_pacing();
        } else {
            // When: `occluded` is false, the covered stretch ends and a later one starts untrimmed.
            self.occluded_since = None;
            self.trimmed = false;
            self.trim_released_requested_bytes = 0;
        }
        was_occluded && !occluded
    }

    /// Consume a structurally invalid attempt and exclude every frame-family deadline.
    pub(in crate::app) fn park(&mut self, snapshot: CauseSnapshot) {
        self.observed = snapshot.0;
        self.attempt_causes = None;
        self.parked = true;
        self.invalidate_link_pacing();
        self.cancel_surface_probe();
        self.deferred = false;
        self.request_in_flight = false;
    }
}

impl BadgeTransition {
    /// Capture only a status whose visible badge still has a strictly future transition.
    pub(in crate::app) fn capture(
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
    pub(super) fn matches(self, status: &CommandStatus, is_active: bool) -> bool {
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

#[cfg(test)]
#[path = "redraw_state_tests.rs"]
mod redraw_state_tests;
