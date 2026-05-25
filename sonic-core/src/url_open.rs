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
fn build_command(url: &str) -> Command {
    let mut c = Command::new("open");
    c.arg(url);
    c
}

#[cfg(target_os = "windows")]
fn build_command(url: &str) -> Command {
    let mut c = Command::new("cmd");
    c.args(["/C", "start", "", url]);
    c
}

#[cfg(all(unix, not(target_os = "macos")))]
fn build_command(url: &str) -> Command {
    let mut c = Command::new("xdg-open");
    c.arg(url);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program_of(cmd: &Command) -> String {
        cmd.get_program().to_string_lossy().into_owned()
    }

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_open() {
        let c = build_command("https://example.com");
        assert_eq!(program_of(&c), "open");
        assert_eq!(args_of(&c), vec!["https://example.com"]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_uses_cmd_start() {
        let c = build_command("https://example.com");
        assert_eq!(program_of(&c), "cmd");
        assert_eq!(args_of(&c), vec!["/C", "start", "", "https://example.com"]);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_uses_xdg_open() {
        let c = build_command("https://example.com");
        assert_eq!(program_of(&c), "xdg-open");
        assert_eq!(args_of(&c), vec!["https://example.com"]);
    }

    #[test]
    fn validate_accepts_known_schemes() {
        for ok in [
            "https://example.com",
            "http://example.com/path?q=1",
            "mailto:a@b.com",
            "file:///etc/hosts",
            "HTTPS://EXAMPLE.COM",
        ] {
            assert!(validate(ok).is_ok(), "expected ok: {ok}");
        }
    }

    #[test]
    fn validate_rejects_other_schemes() {
        for bad in ["javascript:alert(1)", "ssh://host", "data:text/html,<x>", "ftp://x"] {
            assert!(validate(bad).is_err(), "expected err: {bad}");
        }
    }

    #[test]
    fn validate_rejects_shell_metacharacters() {
        for bad in [
            "https://x&calc",
            "https://x|nc",
            "https://x^a",
            "https://x<y",
            "https://x>y",
            "https://x\"y",
            "https://x'y",
            "https://x`y",
            "https://x\ny",
            "https://x\ry",
        ] {
            assert!(validate(bad).is_err(), "expected err: {bad}");
        }
    }

    #[test]
    fn validate_rejects_empty_and_overlong() {
        assert!(validate("").is_err());
        let huge = format!("https://example.com/{}", "a".repeat(5000));
        assert!(validate(&huge).is_err());
    }
}
