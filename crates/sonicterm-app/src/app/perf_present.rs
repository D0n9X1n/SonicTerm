//! The S10 attribution watch's App surface: doc-hidden methods a perf harness calls to arm and disarm
//! per-present attribution for one pane.

impl super::App {
    /// Arm per-present S10 attribution for pane `pane_id`: the sentinel line bound to `nonce` and the
    /// shell's `prompt` (each at most 128 bytes) are the markers, and `updates` is the phase's logical
    /// update count that bounds the watch's lines. Returns the arming id every line carries. In this
    /// build the watch records nothing and returns `None`, so a harness reads attribution as unavailable.
    #[doc(hidden)]
    pub fn arm_s10_attribution(
        &mut self,
        _pane_id: u64,
        _nonce: &str,
        _prompt: &str,
        _updates: u32,
    ) -> Option<u64> {
        None
    }

    /// Disarm pane `pane_id`'s S10 attribution watch. Lines already emitted stay in the log; there is
    /// nothing to drain. In this build no watch is ever armed, so this does nothing.
    #[doc(hidden)]
    pub fn disarm_s10_attribution(&mut self, _pane_id: u64) {}
}

// Pins both signatures in every build, test or not: a gate on either method, or a changed parameter or
// return type, fails a plain library build, which is what an overlaid harness on any base compiles against.
const _: () = {
    let _: fn(&mut super::App, u64, &str, &str, u32) -> Option<u64> =
        super::App::arm_s10_attribution;
    let _: fn(&mut super::App, u64) = super::App::disarm_s10_attribution;
};

#[cfg(test)]
#[path = "perf_present_tests.rs"]
mod perf_present_tests;
