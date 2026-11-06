//! The per-pane OSC 133 shell token: a fresh secret each pane's shell receives in its
//! environment and the pane's parser checks on every shell-integration marker, so markers a
//! program prints cannot pass for the shell's own.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_vt::vt::Parser;

/// The variable that carries a pane's shell token. A SonicTerm shell integration reads it once,
/// unsets it so children never inherit it, and appends it to every OSC 133 marker.
pub(crate) use sonicterm_io::pty::SHELL_TOKEN_ENV;

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

/// Give a pane's shell a fresh token in `shell_opts` and hand `parser` the same token, with the
/// input flag the pane's writer sets, so the parser checks the very token the shell holds and
/// stops latching it once anyone typed. A direct command launch gets neither: it runs no shell
/// integration, and the token would only let that program and its children forge markers.
pub(crate) fn arm_shell_token(
    parser: &Arc<Mutex<Parser>>,
    shell_opts: &mut sonicterm_io::pty::ShellSpawnOpts,
    launch: &super::pane_launch::PaneLaunch,
) {
    // A launch with a `command` runs no shell to keep the token from its children.
    let token = launch.command.is_none().then(new_shell_token).flatten();
    let input_seen = token.as_ref().map(|_| Arc::new(AtomicBool::new(false)));
    if let Some(token) = &token {
        shell_opts.extra_env.push((SHELL_TOKEN_ENV.to_owned(), token.clone()));
    }
    shell_opts.input_seen = input_seen.clone();
    let mut parser = crate::app::frame_counters::lock_parser(parser);
    parser.set_shell_token(token);
    parser.set_shell_input_seen(input_seen);
}

#[cfg(test)]
#[path = "shell_token_tests.rs"]
mod shell_token_tests;
