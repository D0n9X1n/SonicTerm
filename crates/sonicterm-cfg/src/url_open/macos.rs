//! URI dispatch on macOS: the `open` command, which receives the URI as one
//! argument.

use super::*;

/// Build the macOS default-handler command for `url`.
///
/// `open` receives the URI as one argv entry, so no shell re-tokenization
/// applies; [`validate`] remains the gate that decides whether it may spawn.
#[doc(hidden)]
pub fn build_command(url: &str) -> Command {
    let mut command = Command::new("open");
    command.arg(url);
    command
}
