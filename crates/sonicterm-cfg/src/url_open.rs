//! Cross-platform "open this URL in the user's default handler" helper.
//!
//! Used for OSC 8 hyperlink click handling. We deliberately do not block on
//! the spawned child — we just fire-and-forget.
//!
//! ## Security
//!
//! OSC 8 URIs come from untrusted pty output. On Windows, `cmd /C start`
//! re-tokenizes its arguments through cmd's own parser, so an attacker
//! could inject commands even with `Command::args`. We defend with a
//! small, strict allow-list applied to every URI:
//!
//! - Only `http://`, `https://`, `mailto:`, and `file://` schemes are
//!   permitted.
//! - The URI must not contain any cmd / shell metacharacter
//!   (`& | ^ < > " ' \` CR LF NUL + other control chars`).
//! - Capped at 4096 chars.

use std::io;
use std::process::{Command, Stdio};

/// Open `url` with the platform's default handler. Validates the URI before
/// spawning; returns `InvalidInput` for unsafe or unsupported URIs.
pub fn open(url: &str) -> io::Result<()> {
    validate(url)?;
    let mut cmd = build_command(url);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

/// Strict allow-list check applied to every URI before spawning. Public so
/// callers can also use it to gate which OSC 8 cells render as clickable.
pub fn validate(url: &str) -> io::Result<()> {
    if url.is_empty() {
        // When: url carries no scheme to match against the allow-list, so no
        // handler may be spawned for it.
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty url"));
    }
    if url.len() > 4096 {
        // When: url exceeds the 4096-char cap, bounding what reaches the
        // platform handler regardless of scheme.
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "url too long"));
    }
    let lower = url.to_ascii_lowercase();
    let scheme_ok = lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("file://");
    if !scheme_ok {
        // When: scheme_ok rejects anything outside http, https, mailto, and
        // file, so no other URI reaches a handler command.
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "scheme not allowed"));
    }
    for ch in url.chars() {
        match ch {
            '&' | '|' | '^' | '<' | '>' | '"' | '\'' | '`' | '\r' | '\n' | '\0' => {
                // When: ch is a cmd or shell metacharacter that `start` would
                // re-tokenize into a separate command, so the URI is refused.
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "forbidden character"));
            }
            c if c.is_control() => {
                // When: c is any control code, which a URI must percent-encode
                // rather than carry raw.

                // Rejects the full Unicode control set: C0 (< 0x20),
                // DEL (0x7F), and C1 (0x80..=0x9F). A raw control code
                // is never legitimate in a URI (it must be %-encoded),
                // so refusing it keeps the allow-list honest against
                // untrusted OSC 8 input.
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "control character"));
            }
            _ => {
                // When: ch is outside the forbidden and control sets, so it
                // survives validation unchanged.
            }
        }
    }
    Ok(())
}

/// Build the macOS default-handler command for `url`.
///
/// `open` receives the URI as one argv entry, so no shell re-tokenization
/// applies; [`validate`] remains the gate that decides whether it may spawn.
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub fn build_command(url: &str) -> Command {
    let mut c = Command::new("open");
    c.arg(url);
    c
}

/// Build the Windows default-handler command for `url`.
///
/// `cmd /C start` re-parses its arguments through cmd's own tokenizer, which is
/// why [`validate`] must reject shell metacharacters before this command spawns.
/// The empty argument is `start`'s window title, so the URI is not consumed as
/// one.
#[cfg(target_os = "windows")]
#[doc(hidden)]
pub fn build_command(url: &str) -> Command {
    let mut c = Command::new("cmd");
    c.args(["/C", "start", "", url]);
    c
}

/// Build the freedesktop default-handler command for `url`.
///
/// `xdg-open` receives the URI as one argv entry, so no shell re-tokenization
/// applies; [`validate`] remains the gate that decides whether it may spawn.
#[cfg(all(unix, not(target_os = "macos")))]
#[doc(hidden)]
pub fn build_command(url: &str) -> Command {
    let mut c = Command::new("xdg-open");
    c.arg(url);
    c
}

/// Pure dispatch helper for modifier-aware URL-click handling.
///
/// Decides whether a mouse-down event should open a URL, given:
/// - `modifier_held`: did the platform open-URL modifier (Cmd on
///   macOS, Ctrl on Windows/Linux) accompany the click?
/// - `uri_at_cell`: the URI under the cursor cell, if any (OSC 8
///   hyperlink OR plain-text URL detected by `url_scan`).
/// - `open_fn`: how to actually open a validated URI. Production
///   passes `url_open::open`; tests pass a capturing closure.
///
/// Returns `Some(uri)` when the opener was invoked (so the caller
/// knows to swallow the click and skip selection start), `None`
/// otherwise. Validation happens inside `open_fn` for the production
/// path; this helper does not duplicate it.
pub fn dispatch_modifier_click<F>(
    modifier_held: bool,
    uri_at_cell: Option<String>,
    open_fn: F,
) -> Option<String>
where
    F: FnOnce(&str) -> io::Result<()>,
{
    if !modifier_held {
        // When: modifier_held is absent, so the click falls through to
        // selection start instead of opening a URI.
        return None;
    }
    let uri = uri_at_cell?;
    // Best-effort spawn; an error from the opener does NOT cause us
    // to fall through to selection start (the user clearly intended
    // to open a link). Caller logs the error.
    let _ = open_fn(&uri);
    Some(uri)
}

#[cfg(test)]
#[path = "url_open_tests.rs"]
mod url_open_tests;
