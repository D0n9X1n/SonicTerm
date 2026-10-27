//! Native and fallback occlusion delivery, trim and final deadlines.

use super::*;

impl Probe {
    /// Drop the window to the normal level and open the harness's opaque, never-activated cover
    /// at the floating level over the same frame, so the cover is above it by level.
    pub(super) fn cover(&mut self, event_loop: &ActiveEventLoop) {
        let Some(main) = self.app.main_window().cloned() else {
            self.invalidate(event_loop, "no measurement window to cover".to_owned());
            return;
        };
        main.set_window_level(WindowLevel::Normal);
        let attributes = Window::default_attributes()
            .with_title("perf_scenarios cover")
            .with_decorations(false)
            .with_active(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(main.outer_position().unwrap_or_default())
            .with_inner_size(main.outer_size());
        match event_loop.create_window(attributes) {
            Ok(cover) => {
                self.cover = Some(cover);
                self.occlusion.cover_up = true;
                self.arm_occlusion_wait(true);
            }
            Err(error) => {
                self.invalidate(event_loop, format!("cannot open the occlusion cover: {error}"))
            }
        }
    }
}

impl Probe {
    /// Ask for the covered-window trim. On a host without native occlusion the probe delivers
    /// `Occluded(true)` itself first; elsewhere the trim waits for the native event or its fallback.
    /// Both sides of a comparison run this same protocol whatever their hook reports.
    pub(super) fn request_trim(&mut self, event_loop: &ActiveEventLoop) {
        if self.cover.is_none() {
            // When: cover is None, no cover opened, so there is no covered window to trim.
            return;
        }
        self.trim_pending = true;
        if !waits::occlusion_wait_applies(scenarios::BUILD_HOST)
            && self.occlusion.delivered != Some(true)
        {
            // When: this host reports no occlusion, the App learns of the cover only from the probe.
            self.synthetic_occlusion = true;
            self.deliver_occlusion(event_loop, true);
        }
        self.service_trim(false);
    }
}

impl Probe {
    /// Call the trim hook on the first turn the App holds `Occluded(true)`, once; when the covered
    /// hold ends first (`hold_over`), the hook stays unreached. It never changes the plan's steps.
    pub(super) fn service_trim(&mut self, hold_over: bool) {
        match waits::trim_dispatch(self.trim_pending, self.occlusion.delivered, hold_over) {
            TrimDispatch::Idle | TrimDispatch::Wait => {}
            TrimDispatch::Call => {
                self.trim_pending = false;
                if let Some(window_id) = self.main_id {
                    // When: main_id names the measurement window, the hook is asked about it.
                    (self.trim_hook, self.trim_seq_after_hook) =
                        trim_covered(&mut self.app, window_id);
                }
            }
            TrimDispatch::Lapsed => {
                self.trim_pending = false;
                self.trim_hook = TrimHookOutcome::NotReached;
            }
        }
    }
}

impl Probe {
    /// Raise the window back to the floating level, then close the cover.
    pub(super) fn uncover(&mut self) {
        if let Some(main) = self.app.main_window() {
            main.set_window_level(WindowLevel::AlwaysOnTop);
        }
        self.cover = None;
        self.occlusion.cover_up = false;
        self.occlusion.uncover_requested = true;
        self.arm_occlusion_wait(false);
    }
}

impl Probe {
    /// Expect the harness-caused occlusion state `occluded`, and deliver it synthetically when
    /// no native event brings it within 2 s.
    pub(super) fn arm_occlusion_wait(&mut self, occluded: bool) {
        if waits::occlusion_wait_applies(scenarios::BUILD_HOST) {
            // When: the host reports occlusion natively (macOS), the 2 s fallback covers a missing event.
            // Windows reports none: only the trim experiment delivers occlusion there, itself.
            self.occlusion.wait = Some((occluded, Instant::now() + OCCLUSION_FALLBACK));
        }
    }
}

impl Probe {
    /// A native occlusion change for the measurement window. Each state reaches the App at most
    /// once. Reveal events before the first present are expected; after it, a change the harness
    /// did not cause voids the run: covered while no cover is up, or uncovered while it is.
    pub(super) fn native_occlusion(&mut self, event_loop: &ActiveEventLoop, occluded: bool) {
        if self.occlusion.delivered == Some(occluded) {
            // When: the App already has this state, natively or from the fallback, it is dropped.
            return;
        }
        if self.first_present && occluded != self.occlusion.cover_up {
            let what = if occluded {
                "Occluded(true) outside the cover"
            } else {
                "Occluded(false) while covered"
            };
            self.invalidate(event_loop, format!("unrequested native occlusion change: {what}"));
            return;
        }
        self.deliver_occlusion(event_loop, occluded);
    }
}

impl Probe {
    pub(super) fn deliver_occlusion(&mut self, event_loop: &ActiveEventLoop, occluded: bool) {
        self.occlusion.delivered = Some(occluded);
        if self.occlusion.wait.is_some_and(|(expected, _)| expected == occluded) {
            self.occlusion.wait = None;
        }
        if !occluded && self.occlusion.uncover_requested {
            // Uncover time runs from here to the end of the first dispatch that presents.
            self.occlusion.uncover_requested = false;
            self.occlusion.uncover_from = Some(Instant::now());
        }
        self.window_input(event_loop, WindowEvent::Occluded(occluded));
    }
}

impl Probe {
    /// Deliver the state the harness caused when no native event brought it within 2 s.
    pub(super) fn deliver_overdue_occlusion(&mut self, event_loop: &ActiveEventLoop, now: Instant) {
        let Some((expected, deadline)) = self.occlusion.wait else {
            return;
        };
        if now < deadline {
            return;
        }
        self.occlusion.wait = None;
        if self.occlusion.delivered != Some(expected) {
            self.synthetic_occlusion = true;
            tracing::info!(target: LOG_TARGET, occluded = expected, "no native occlusion change within 2 s; synthetic one delivered");
            self.deliver_occlusion(event_loop, expected);
        }
    }
}

impl Probe {
    /// The probe's next wake. A measured phase adds only its next injection, its deadline and one
    /// trailing scan check; files are polled only outside measured phases.
    pub(super) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let deadline = match self.stage {
            Stage::Done => return None,
            Stage::Startup => self.first_present_bound.wake(self.run_deadline),
            Stage::Setup { .. } | Stage::Acks | Stage::Ready => now + POLL_INTERVAL,
            Stage::Steps(_) => steps_wake(
                self.checkpoint_pending.as_ref(),
                now,
                [
                    self.driver_due(),
                    self.current_phase().and_then(|phase| self.phase_deadline(phase)),
                    self.scan.trailing(),
                    self.occlusion.wait.map(|(_, at)| at),
                ],
                self.run_deadline,
            ),
        };
        Some(deadline.min(self.run_deadline))
    }
}

/// When a barrier or anchored-hold phase ends by the clock: a pending barrier's bound (a met one adds
/// none), or a hold counted from `anchor` (else from `started`). Other phases are not handled here.
pub(super) fn barrier_phase_deadline(
    end: &PhaseEnd,
    barrier: Option<&FrameBarrier>,
    started: Instant,
    anchor: Option<Instant>,
) -> Option<Instant> {
    match end {
        PhaseEnd::MediaFree | PhaseEnd::Reshow => barrier.and_then(FrameBarrier::deadline),
        PhaseEnd::HoldFrom { hold_ms, .. } => {
            Some(waits::anchored_hold_end(anchor, started, *hold_ms))
        }
        _ => None,
    }
}
