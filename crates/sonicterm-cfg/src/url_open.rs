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
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty url"));
    }
    if url.len() > 4096 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "url too long"));
    }
    let lower = url.to_ascii_lowercase();
    let scheme_ok = lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("file://");
    if !scheme_ok {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "scheme not allowed"));
    }
    for ch in url.chars() {
        match ch {
            '&' | '|' | '^' | '<' | '>' | '"' | '\'' | '`' | '\r' | '\n' | '\0' => {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "forbidden character"));
            }
            c if (c as u32) < 0x20 => {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "control character"));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[doc(hidden)]
#[doc(hidden)]
pub fn build_command(url: &str) -> Command {
    let mut c = Command::new("open");
    c.arg(url);
    c
}

#[cfg(target_os = "windows")]
#[doc(hidden)]
#[doc(hidden)]
pub fn build_command(url: &str) -> Command {
    let mut c = Command::new("cmd");
    c.args(["/C", "start", "", url]);
    c
}

#[cfg(all(unix, not(target_os = "macos")))]
#[doc(hidden)]
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
///
/// This was extracted from `App::do_window_event`'s `MouseInput`
/// arm so the modifier-aware dispatch decision is unit-testable
/// without a real winit event loop.
pub fn dispatch_modifier_click<F>(
    modifier_held: bool,
    uri_at_cell: Option<String>,
    open_fn: F,
) -> Option<String>
where
    F: FnOnce(&str) -> io::Result<()>,
{
    if !modifier_held {
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
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn modifier_click_on_url_cell_calls_opener() {
        let captured: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let out = dispatch_modifier_click(true, Some("https://example.com".to_string()), |u| {
            captured.borrow_mut().push(u.to_string());
            Ok(())
        });
        assert_eq!(out.as_deref(), Some("https://example.com"));
        assert_eq!(*captured.borrow(), vec!["https://example.com".to_string()]);
    }

    #[test]
    fn click_without_modifier_does_not_call_opener() {
        let captured: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let out = dispatch_modifier_click(false, Some("https://example.com".to_string()), |u| {
            captured.borrow_mut().push(u.to_string());
            Ok(())
        });
        assert!(out.is_none(), "no opener invocation expected without modifier");
        assert!(captured.borrow().is_empty(), "opener must not be called: {:?}", captured.borrow());
    }

    #[test]
    fn modifier_click_on_non_url_cell_does_not_call_opener() {
        let captured: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let out = dispatch_modifier_click(true, None, |u| {
            captured.borrow_mut().push(u.to_string());
            Ok(())
        });
        assert!(out.is_none());
        assert!(captured.borrow().is_empty());
    }

    #[test]
    fn opener_error_still_consumes_click() {
        // If the opener spawn fails (e.g. no `xdg-open` available),
        // the click is still consumed — the user clearly intended
        // to open the link, not start a selection. Caller logs.
        let out = dispatch_modifier_click(true, Some("https://example.com".to_string()), |_| {
            Err(io::Error::other("simulated spawn failure"))
        });
        assert_eq!(out.as_deref(), Some("https://example.com"));
    }
}
