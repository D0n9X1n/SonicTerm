//! Negative-path tests for `sonic_cfg::url_open::validate`.
//!
//! These exist because the original PR-192 review (Haiku) flagged that
//! the URL-scanner tests only asserted match/no-match shape; they did
//! not assert that the security-critical `validate()` gate *rejects*
//! shell-meta, control chars, over-length payloads, and disallowed
//! schemes. This file pins those rejections down.

use sonic_cfg::url_open::validate;
use sonic_cfg::url_scan::find_urls;

#[test]
fn validate_rejects_shell_metachars() {
    assert!(validate("https://x.com/&calc.exe").is_err());
    assert!(validate("https://x.com/|whoami").is_err());
    // The denied set is `& | ^ < > " ' ` CR LF NUL` + controls
    // (cmd.exe's tokenizer, not bash). Pin each one:
    assert!(validate("https://x.com/^bad").is_err());
    assert!(validate("https://x.com/<bad").is_err());
    assert!(validate("https://x.com/>bad").is_err());
    assert!(validate("https://x.com/\"bad").is_err());
    assert!(validate("https://x.com/'bad").is_err());
    assert!(validate("https://x.com/`bad`").is_err());
}

#[test]
fn validate_rejects_control_chars() {
    assert!(validate("https://x.com/\x00").is_err());
    assert!(validate("https://x.com/\x0a").is_err()); // LF
    assert!(validate("https://x.com/\x0d").is_err()); // CR
    assert!(validate("https://x.com/\x07").is_err()); // BEL
    assert!(validate("https://x.com/\x1b").is_err()); // ESC
}

#[test]
fn validate_rejects_overlength() {
    let long = format!("https://x.com/{}", "a".repeat(5000));
    assert!(validate(&long).is_err(), "5KB URL must be rejected");
    let just_over = format!("https://x.com/{}", "a".repeat(4096));
    assert!(just_over.len() > 4096);
    assert!(validate(&just_over).is_err());
}

#[test]
fn validate_rejects_javascript_scheme() {
    assert!(validate("javascript:alert(1)").is_err());
    assert!(validate("data:text/html,<script>x</script>").is_err());
    assert!(validate("vbscript:msgbox").is_err());
    assert!(validate("ssh://host").is_err());
    assert!(validate("ftp://host").is_err());
    assert!(validate("gopher://host").is_err());
}

#[test]
fn validate_rejects_empty() {
    assert!(validate("").is_err());
}

#[test]
fn validate_accepts_allowed_schemes() {
    // Positive smoke so the negative cases above can't all spuriously
    // succeed due to a broken validator (e.g. always-Err).
    assert!(validate("https://example.com").is_ok());
    assert!(validate("http://example.com/path?q=1").is_ok());
    assert!(validate("mailto:user@example.com").is_ok());
    assert!(validate("file:///etc/hosts").is_ok());
}

/// The URL scanner pipeline must never surface a payload that
/// `validate` would reject. Hand the scanner a row of text containing
/// shell-meta, control chars, and unknown schemes and assert the
/// scanner either trims the bad part off or returns nothing.
#[test]
fn scanner_does_not_surface_validate_rejected_payloads() {
    let lines = [
        "click javascript:alert(1) please",
        "click data:text/html,<x> please",
        "click https://evil.test&calc.exe please",
        "click https://evil.test|whoami please",
        "click https://evil.test`id` please",
        "click https://evil.test<x please",
        "click https://evil.test\"x please",
        "click https://evil.test\x00bad please",
    ];
    for line in lines {
        for m in find_urls(line) {
            assert!(
                validate(&m.url).is_ok(),
                "scanner surfaced a URL that validate() rejects: line={line:?} match={m:?}",
            );
        }
    }
}
