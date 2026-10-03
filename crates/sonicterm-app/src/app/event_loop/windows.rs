//! Windows event-loop glue for `App`: the foreground-process privilege probe
//! and the delayed OSC 52 clipboard reassertion.
//!
//! Accepted input or quiet PTY output arms one foreground-process sample, and
//! a sample re-arms only while a per-tab elevation warning is shown in a
//! non-elevated process. The reassertion re-applies one OSC 52 write when the
//! clipboard has reverted to its previous text, as a failing clipboard
//! helper's cleanup can leave it. Shared code calls these from its
//! `cfg(windows)` branches.

use super::*;

impl App {
    #[cfg(windows)]
    pub(in crate::app) fn arm_foreground_probe_after_input(&mut self, now: Instant) {
        let privileged = self.process_privilege.is_privileged();
        self.foreground_schedule.arm_after_input(now, privileged);
    }

    #[cfg(windows)]
    pub(super) fn arm_foreground_probe_after_output(&mut self, now: Instant) {
        let privileged = self.process_privilege.is_privileged();
        self.foreground_schedule.arm_after_output(now, privileged);
    }

    /// Whether any window shows a per-tab foreground warning in accepted tab state.
    #[cfg(windows)]
    pub(in crate::app) fn foreground_warning_active(&self) -> bool {
        self.windows
            .values()
            .any(|window| window.tabs.tabs().iter().any(|tab| tab.foreground_privileged))
    }

    /// Re-arm or clear the warning wake identically on the due and drain paths; a cleared
    /// warning also drops every demand that is still warning-only.
    #[cfg(windows)]
    pub(in crate::app) fn finish_foreground_process_probe(
        &mut self,
        now: Instant,
        warning_active: bool,
    ) {
        let privileged = self.process_privilege.is_privileged();
        // A cleared warning wake leaves warning-only demand with nothing to serve.
        if self.foreground_schedule.settle_warning(now, warning_active, privileged) {
            self.fg_probes.clear_warning_wants();
        }
    }

    /// Consume the wakes due at `now` and set one demand for every tab's active pane.
    ///
    /// Only demand is set here; results arrive through the worker. The returned windows are
    /// those whose warning changed synchronously (exit, no identity, or `Unavailable`).
    #[cfg(windows)]
    pub(in crate::app) fn refresh_foreground_privileges_if_due(
        &mut self,
        now: Instant,
    ) -> Vec<WindowId> {
        let privileged = self.process_privilege.is_privileged();
        let warning_active = self.foreground_warning_active();
        let origin = self.foreground_schedule.take_due(now, warning_active, privileged);
        let mut changed_windows = Vec::new();
        if let Some(origin) = origin {
            // When: `origin` names a due wake, one demand covers every tab's active pane.
            let probes = std::sync::Arc::clone(&self.fg_probes);
            let mut queued = false;
            for (window_id, window) in &mut self.windows {
                let mut changed = false;
                for (tab_idx, tab_state) in window.tab_states.iter().enumerate() {
                    let Some(pane) = window.panes.get_mut(&tab_state.active_pane) else {
                        // When: the tab's active pane is gone, its warning cannot stand.
                        changed |= window.tabs.set_foreground_privileged(tab_idx, false);
                        continue;
                    };
                    match probes.request(tab_state.active_pane, pane, origin, now, true) {
                        crate::app::fg_probe::Demand::Queued => queued = true,
                        crate::app::fg_probe::Demand::Resolved { changed: cleared } => {
                            changed |= cleared;
                        }
                        crate::app::fg_probe::Demand::Fresh => {
                            // When: `Fresh`, the pane's cache or demand is younger than the TTL.
                        }
                    }
                    let privileged_here = crate::app::privilege::cached_foreground_privileged(pane);
                    changed |= window.tabs.set_foreground_privileged(tab_idx, privileged_here);
                }
                if changed {
                    changed_windows.push(*window_id);
                }
            }
            // One wake serves the whole due demand.
            if queued {
                probes.wake();
            }
        }
        let warning_active = self.foreground_warning_active();
        self.finish_foreground_process_probe(now, warning_active);
        changed_windows
    }

    #[cfg(target_os = "windows")]
    pub(in crate::app) fn reassert_osc52_clipboard_if_due(&mut self, now: Instant) {
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
}
