//! Integration tests for the sonic-core url_open re-exports.

use std::process::Command;

use sonic_core::url_open::build_command;
use sonic_core::url_open::validate;

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
