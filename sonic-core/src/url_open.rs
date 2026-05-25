//! Cross-platform "open this URL in the user's default handler" helper.
//!
//! Used for OSC 8 hyperlink click handling. We deliberately do not block on
//! the spawned child — we just fire-and-forget.

use std::io;
use std::process::{Command, Stdio};

/// Open `url` (or any path) with the platform's default handler.
pub fn open(url: &str) -> io::Result<()> {
    let mut cmd = build_command(url);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
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
}
