//! The per-pane OSC 133 shell token: a fresh secret each pane's shell receives in its
//! environment and the pane's parser checks on every shell-integration marker, so markers a
//! program prints cannot pass for the shell's own.

use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_vt::vt::Parser;

/// The variable that carries a pane's shell token. A SonicTerm shell integration reads it once,
/// unsets it so children never inherit it, and appends it to every OSC 133 marker.
pub(crate) const SHELL_TOKEN_ENV: &str = "SONICTERM_SHELL_TOKEN";

/// A fresh 128-bit shell token as 32 lowercase hex digits, from the OS random source; `None` if
/// that source fails, in which case the pane's markers stay unverified as before.
pub(crate) fn new_shell_token() -> Option<String> {
    let mut bytes = [0u8; 16];
    match getrandom::fill(&mut bytes) {
        Ok(()) => Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect()),
        Err(error) => {
            tracing::warn!(target: "mcp", %error, "no shell token: the OS random source failed");
            None
        }
    }
}

/// Hand `parser` the shell token `shell_opts` gives the pane's shell, so the parser checks the
/// very token the shell holds.
pub(crate) fn arm_shell_token(
    parser: &Arc<Mutex<Parser>>,
    shell_opts: &sonicterm_io::pty::ShellSpawnOpts,
) {
    let token = shell_opts
        .extra_env
        .iter()
        .find(|(name, _)| name == SHELL_TOKEN_ENV)
        .map(|(_, value)| value.clone());
    crate::app::frame_counters::lock_parser(parser).set_shell_token(token);
}
