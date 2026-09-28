//! URI dispatch on every Unix except macOS, including Linux: the freedesktop
//! `xdg-open` command, which receives the URI as one argument.

use super::*;

/// Build the freedesktop default-handler command for `url`.
///
/// `xdg-open` receives the URI as one argv entry, so no shell re-tokenization
/// applies; [`validate`] remains the gate that decides whether it may spawn.
#[doc(hidden)]
pub fn build_command(url: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(url);
    command
}
