//! Covered-window trim eligibility and the matching explicit trim hook.

use super::*;

/// Every eligibility rule of a covered-window trim except the renderer itself, device-free.
///
/// `device_accepts` is `None` without a renderer, else whether its device accepts GPU work. The
/// renderer is checked last, so a test without a device can prove every earlier rule passed.
pub(in crate::app) fn trim_eligibility(
    redraw: &super::super::redraw::WindowRedrawState,
    device_accepts: Option<bool>,
    now: Instant,
    rule: TrimRule,
) -> Result<(), TrimSkip> {
    let Some(occluded_since) = redraw.occluded_since.filter(|_| redraw.native_occluded) else {
        // When: native_occluded is false or occluded_since is None, the window is not covered.
        return Err(TrimSkip::NotOccluded);
    };
    if rule == TrimRule::After30s
        && now.saturating_duration_since(occluded_since) < COVERED_TRIM_AFTER
    {
        // When: rule is After30s and the covered stretch is shorter than COVERED_TRIM_AFTER.
        return Err(TrimSkip::TooRecent);
    }
    if redraw.trimmed {
        // When: trimmed is set, this covered stretch was already trimmed once.
        return Err(TrimSkip::AlreadyTrimmed);
    }
    if redraw.parked {
        // When: parked is set, the window's topology is invalid and its renderer must stay as is.
        return Err(TrimSkip::Parked);
    }
    if redraw.stopped_generation.is_some() || device_accepts == Some(false) {
        // When: stopped_generation is set or device_accepts is false, recovery owns the renderer.
        return Err(TrimSkip::DeviceUnavailable);
    }
    if device_accepts.is_none() {
        // When: device_accepts is None, the window has no renderer to trim.
        return Err(TrimSkip::NoRenderer);
    }
    Ok(())
}

impl super::super::App {
    /// Trim window `window_id`'s renderer when every covered-window rule holds under `rule`.
    ///
    /// The one production body behind both the retention pass and the perf hook. A skipped or
    /// refused trim changes nothing; a trim numbers itself in `trim_seq` and marks the stretch.
    pub(in crate::app) fn trim_window_if_eligible(
        &mut self,
        window_id: WindowId,
        now: Instant,
        rule: TrimRule,
    ) -> TrimDecision {
        if self.is_warm_window_id(window_id) {
            // When: is_warm_window_id names a warm spare, it was never shown and is never trimmed.
            return TrimDecision::Skipped(TrimSkip::Warm);
        }
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: window_id names no live window, there is nothing to trim.
            return TrimDecision::Skipped(TrimSkip::NoWindow);
        };
        let device_accepts =
            window.renderer.as_ref().map(|renderer| renderer.device_accepts_gpu_work());
        if let Err(reason) = trim_eligibility(&window.redraw, device_accepts, now, rule) {
            // When: trim_eligibility refuses, the reason is reported and nothing is released.
            return TrimDecision::Skipped(reason);
        }
        let Some(renderer) = window.renderer.as_mut() else {
            // When: renderer is None, which trim_eligibility already excluded.
            return TrimDecision::Skipped(TrimSkip::NoRenderer);
        };
        let report = renderer.trim_for_occlusion();
        if report.refused {
            // When: report.refused, the device stopped between the check and the trim.
            return TrimDecision::Skipped(TrimSkip::DeviceUnavailable);
        }
        window.redraw.trimmed = true;
        window.redraw.trim_released_requested_bytes = report.gpu_released_requested_bytes;
        self.trim_seq += 1;
        self.last_trim_source = Some(rule.source());
        TrimDecision::Trimmed { trim_seq: self.trim_seq }
    }

    /// The retention pass's trim: every live window covered for 30 s gives back what it can rebuild.
    pub(in crate::app) fn trim_covered_renderers(&mut self, now: Instant) {
        let window_ids: Vec<WindowId> = self.windows.keys().copied().collect();
        for window_id in window_ids {
            let decision = self.trim_window_if_eligible(window_id, now, TrimRule::After30s);
            #[cfg(test)]
            self.test_scheduler_trims.push((window_id, decision));
            #[cfg(not(test))]
            let _ = decision;
        }
    }

    /// Trim window `window_id`'s renderer now, as the covered-window retention rule would but
    /// without its 30 s age, for a perf checkpoint. Every other rule applies; it runs no
    /// retention pass, reclamation or charge and leaves the sample cadence alone.
    #[cfg(any(test, feature = "perf-hook-trim"))]
    #[doc(hidden)]
    pub fn __trim_covered_now(&mut self, window_id: WindowId) -> TrimDecision {
        let now = (self.dispatch_clock)();
        self.trim_window_if_eligible(window_id, now, TrimRule::IgnoreAge)
    }
}
