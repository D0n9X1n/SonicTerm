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
use crate::app::{PendingForegroundProbe, FOREGROUND_PROCESS_TTL};

impl App {
    #[cfg(windows)]
    pub(in crate::app) fn arm_foreground_probe_after_input(&mut self, now: Instant) {
        if self.process_privilege.is_privileged() {
            // When: `process_privilege.is_privileged()` is true, every tab already carries the global warning.
            self.foreground_probe_wake = None;
            return;
        }
        self.foreground_probe_wake =
            Some(PendingForegroundProbe { due: now + FOREGROUND_PROCESS_TTL, fixed: true });
    }

    #[cfg(windows)]
    pub(super) fn arm_foreground_probe_after_output(&mut self, now: Instant) {
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
            Some(PendingForegroundProbe { due: now + FOREGROUND_PROCESS_TTL, fixed: false });
    }

    #[cfg(windows)]
    pub(super) fn finish_foreground_process_probe(&mut self, now: Instant, warning_active: bool) {
        self.foreground_probe_wake = (!self.process_privilege.is_privileged() && warning_active)
            .then_some(PendingForegroundProbe { due: now + FOREGROUND_PROCESS_TTL, fixed: true });
    }

    #[cfg(windows)]
    fn foreground_probe_is_due(&self, now: Instant) -> bool {
        self.foreground_probe_wake.is_some_and(|wake| wake.due <= now)
    }

    #[cfg(windows)]
    pub(in crate::app) fn refresh_foreground_privileges_if_due(
        &mut self,
        now: Instant,
    ) -> Vec<WindowId> {
        if !self.foreground_probe_is_due(now) {
            // When: `foreground_probe_is_due(now)` is false, leave every foreground cache untouched.
            return Vec::new();
        }
        self.foreground_probe_wake = None;
        let mut changed_windows = Vec::new();
        let mut warning_active = false;
        for (window_id, window) in &mut self.windows {
            let changed = crate::app::force_refresh_window_tab_privileges(
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
