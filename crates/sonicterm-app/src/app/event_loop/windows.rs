//! Windows event-loop glue for `App`: the foreground-process privilege probe.
//!
//! Accepted input or quiet PTY output arms one foreground-process sample, and
//! a sample re-arms only while a per-tab elevation warning is shown in a
//! non-elevated process. Shared code calls these from its `cfg(windows)`
//! branches. The delayed OSC 52 reassertion runs on the clipboard worker.

use super::*;

impl App {
    #[cfg(windows)]
    pub(in crate::app) fn arm_foreground_probe_after_input(&mut self, now: Instant) {
        let privileged = self.process_privilege.is_privileged();
        self.services.foreground_schedule.arm_after_input(now, privileged);
    }

    #[cfg(windows)]
    pub(in crate::app) fn arm_foreground_probe_after_output(&mut self, now: Instant) {
        let privileged = self.process_privilege.is_privileged();
        self.services.foreground_schedule.arm_after_output(now, privileged);
    }
}
