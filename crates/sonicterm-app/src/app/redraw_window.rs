//! Window visibility, surface-probe and image-atlas deadline handling.

use super::*;

impl WindowState {
    /// When this window's promoted image atlas may be released without a frame, if it is promoted and
    /// no renderable media has been visible for 30 s. Tests inject it through `test_image_atlas_release`.
    pub(in crate::app) fn image_atlas_release_deadline(&self) -> Option<Instant> {
        #[cfg(test)]
        if let Some(fake) = self.test_image_atlas_release.as_ref() {
            // When: test_image_atlas_release is set, the headless window has no renderer to ask.
            return fake.deadline;
        }
        self.renderer.as_ref().and_then(|renderer| renderer.image_atlas_release_deadline())
    }

    /// Whether the release may be collected: the window is visible and idle, with no frame postponed by
    /// pacing (`deferred`), no owner-local request in flight and no marked cause still unsettled. A
    /// native-only request is invisible here; a release racing one only makes that frame promote again,
    /// before any image is emitted, so it never draws a wrong pixel.
    pub(in crate::app) fn image_atlas_release_collectable(&self) -> bool {
        self.frame_deadlines_allowed()
            && !self.redraw.deferred
            && !self.redraw.request_in_flight
            && !self.redraw.has_pending()
    }

    /// Whether the release may run at `now`: collectable, and its deadline has passed.
    pub(in crate::app) fn image_atlas_release_eligible(&self, now: Instant) -> bool {
        self.image_atlas_release_collectable()
            && self.image_atlas_release_deadline().is_some_and(|deadline| deadline <= now)
    }

    /// Release the idle image atlas; the renderer checks its own rule again, so a stale firing is a no-op.
    pub(super) fn release_idle_image_atlas(&mut self, now: Instant) {
        #[cfg(test)]
        if let Some(fake) = self.test_image_atlas_release.as_mut() {
            // When: test_image_atlas_release is set, the stand-in releases once and clears its deadline.
            if fake.deadline.is_some_and(|deadline| deadline <= now) {
                fake.deadline = None;
                fake.releases += 1;
            }
            return;
        }
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.release_idle_image_atlas(now);
        }
    }

    /// True only for a window whose frame-family deadlines may currently contribute.
    pub(in crate::app) fn frame_deadlines_allowed(&self) -> bool {
        !self.hidden
            && !self.redraw.native_occluded
            && !self.redraw.backend_occluded
            && !self.redraw.parked
            && self.redraw.stopped_generation.is_none()
    }

    /// Mark one full visibility frame without acquiring pane locks or performing GPU work.
    pub(in crate::app) fn invalidate_visibility_frame(&mut self) {
        self.mark_redraw(RedrawCause::Visibility);
        #[cfg(test)]
        {
            self.redraw.retained_frame_drops += 1;
        }
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.invalidate_retained_frame();
        }
    }

    /// Request visibility only for an attached usable renderer; device stop always wins.
    pub(in crate::app) fn request_visible_frame(&mut self) {
        if self.frame_deadlines_allowed()
            && !self.redraw.request_in_flight
            && self.renderer.as_ref().is_some_and(|renderer| renderer.device_accepts_gpu_work())
        {
            self.redraw.request_in_flight = true;
            self.request_window_redraw();
        }
    }

    /// Return only a live backend-only probe; native-hidden and stopped owners never contribute one.
    #[cfg(target_os = "macos")]
    pub(in crate::app) fn surface_probe_deadline(&self) -> Option<Instant> {
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
    pub(super) fn finish_surface_probe(
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
    pub(super) fn accept_device_recovery(&mut self, snapshot: &DeviceErrorSnapshot) -> bool {
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
        super::super::mark_all_panes_dirty(&self.panes);
        true
    }

    /// Apply verified recovery once and coalesce its visible native request with any pending request.
    pub(super) fn request_device_recovery(&mut self, snapshot: &DeviceErrorSnapshot) -> bool {
        if !self.accept_device_recovery(snapshot) {
            // When: `accept_device_recovery` refuses, neither causes nor native requests can revive the stopped generation.
            return false;
        }
        if self.frame_deadlines_allowed() && !self.redraw.request_in_flight {
            self.redraw.request_in_flight = true;
            self.request_window_redraw();
        }
        true
    }

    /// Refresh the raw native-monitor period, preserving the last known rate when unavailable.
    /// A changed period is passed on to the window's display link, so its ticks follow the display.
    pub(in crate::app) fn refresh_monitor_period(&mut self) {
        let before = self.redraw.monitor_period;
        let rate = self.read_monitor_rate();
        self.redraw.apply_monitor_rate(rate);
        if self.redraw.monitor_period != before {
            self.push_preferred_period();
        }
    }

    /// The native monitor's refresh rate in millihertz, or a test's override of it.
    pub(super) fn read_monitor_rate(&self) -> Option<u32> {
        #[cfg(test)]
        if let Some(rate) = self.redraw.monitor_rate_override {
            // When: a test supplies `monitor_rate_override`, it stands in for the native monitor.
            return rate;
        }
        self.window
            .as_ref()
            .and_then(|window| window.current_monitor())
            .and_then(|monitor| monitor.refresh_rate_millihertz())
    }
}

#[cfg(test)]
#[path = "redraw_window_tests.rs"]
mod redraw_window_tests;
