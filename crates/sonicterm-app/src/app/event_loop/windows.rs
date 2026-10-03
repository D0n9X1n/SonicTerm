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
