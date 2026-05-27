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
