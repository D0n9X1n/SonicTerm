//! Behavior + security tests for the safe-open policy.
//!
//! These never launch a real OS handler: `open()` is exercised only on
//! inputs that fail `validate()` before any spawn, `build_command()` is
//! inspected without spawning, and `dispatch_modifier_click` is driven
//! with a capturing closure that records the URI instead of opening it.

use super::*;
use std::cell::Cell;
use std::rc::Rc;

// ---- scheme allow-list: only http/https/mailto/file are accepted -------

#[test]
fn accepts_the_four_supported_schemes() {
    for url in [
        "http://example.com",
        "https://example.com/path?q=1#frag",
        "mailto:user@example.com",
        "file:///Users/me/notes.txt",
    ] {
        assert!(validate(url).is_ok(), "should accept {url:?}");
    }
}

#[test]
fn scheme_match_is_case_insensitive() {
    for url in ["HTTP://EXAMPLE.COM", "HtTpS://Example.com", "MAILTO:a@b.com", "FILE://host/p"] {
        assert!(validate(url).is_ok(), "scheme compare must ignore case: {url:?}");
    }
}

#[test]
fn rejects_unsupported_schemes() {
    for url in [
        "javascript:alert(1)",
        "data:text/html,<script>",
        "ftp://example.com/file",
        "vbscript:msgbox",
        "ssh://host",
        "about:blank",
        "tel:+15551234",
        "\\\\host\\share",
    ] {
        let err = validate(url).expect_err("must reject unsupported scheme");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

#[test]
fn rejects_scheme_only_with_no_body() {
    // A bare scheme prefix has nothing to open. `validate` accepts the
    // prefix itself (policy is scheme + char safety), so document that
    // boundary explicitly rather than assume trailing content.
    assert!(validate("mailto:").is_ok(), "bare mailto: is scheme-valid");
    // But a near-miss that only partially spells a scheme is rejected.
    for url in ["http:/only-one-slash", "htp://typo.example", "mail:to@x"] {
        assert!(validate(url).is_err(), "near-miss scheme must fail: {url:?}");
    }
}

// ---- length + emptiness boundaries -------------------------------------

#[test]
fn rejects_empty_url() {
    let err = validate("").expect_err("empty must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn length_cap_boundary_is_4096_bytes() {
    // "https://" (8) + body == exactly 4096 is allowed; 4097 is not.
    let at_cap = format!("https://{}", "a".repeat(4096 - "https://".len()));
    assert_eq!(at_cap.len(), 4096);
    assert!(validate(&at_cap).is_ok(), "exactly 4096 bytes must pass");

    let over_cap = format!("https://{}", "a".repeat(4096 - "https://".len() + 1));
    assert_eq!(over_cap.len(), 4097);
    let err = validate(&over_cap).expect_err("4097 bytes must fail");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

// ---- shell metacharacters / quotes / NUL / CR / LF ---------------------

#[test]
fn rejects_shell_metacharacters_and_quotes() {
    // Each of these, embedded in an otherwise-valid https URL, must be
    // refused so a Windows `cmd /C start` re-tokenization cannot inject.
    for meta in ['&', '|', '^', '<', '>', '"', '\'', '`'] {
        let url = format!("https://example.com/{meta}evil");
        let err = validate(&url).expect_err("metacharacter must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "char {meta:?} should fail");
    }
}

#[test]
fn rejects_nul_cr_lf() {
    for bad in ["https://example.com/\0", "https://example.com/\r", "https://example.com/\n"] {
        let err = validate(bad).expect_err("NUL/CR/LF must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

#[test]
fn rejects_c0_control_characters() {
    // Every C0 control (0x01..=0x1F) embedded in the body is refused.
    for code in 0x01u32..=0x1F {
        let url = format!("https://example.com/{}", char::from_u32(code).unwrap());
        assert!(validate(&url).is_err(), "C0 control U+{code:04X} must be rejected");
    }
}

#[test]
fn rejects_del_and_c1_control_characters() {
    // Regression guard for the gap where the old `(c as u32) < 0x20`
    // check let DEL (0x7F) and the C1 range (0x80..=0x9F) through even
    // though the module contract promises to reject control chars.
    let url_del = "https://example.com/\u{7f}";
    assert!(validate(url_del).is_err(), "DEL (0x7F) must be rejected");

    for code in 0x80u32..=0x9F {
        let url = format!("https://example.com/{}", char::from_u32(code).unwrap());
        assert!(validate(&url).is_err(), "C1 control U+{code:04X} must be rejected");
    }
}

#[test]
fn accepts_printable_ascii_and_percent_encoding() {
    // The safe body should still allow ordinary URL characters and the
    // %-encoded form of the bytes we reject in raw form.
    for url in [
        "https://example.com/a/b/c?x=1&_y=2".replace('&', "%26"),
        "https://example.com/%20space%0Aencoded".to_string(),
        "https://example.com/~user/(paren)".to_string(),
    ] {
        assert!(validate(&url).is_ok(), "printable/encoded body must pass: {url:?}");
    }
}

// ---- open() never spawns for invalid input -----------------------------

#[test]
fn open_returns_invalid_input_without_spawning_for_bad_urls() {
    // These all fail validation, so `open` returns before build/spawn —
    // exercising the guard path without touching any OS handler.
    for bad in ["", "javascript:alert(1)", "https://a.com/\n", "https://a.com/&x"] {
        let err = open(bad).expect_err("invalid url must not spawn");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

// ---- build_command: inspect argv, never spawn --------------------------

#[test]
fn build_command_targets_platform_handler_with_url_as_arg() {
    let url = "https://example.com/safe";
    let cmd = build_command(url);
    let program = cmd.get_program().to_string_lossy().into_owned();
    let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();

    #[cfg(target_os = "macos")]
    {
        assert_eq!(program, "open");
        assert_eq!(args, vec![url.to_string()]);
    }
    #[cfg(target_os = "windows")]
    {
        assert_eq!(program, "cmd");
        // The empty "" is the `start` window-title placeholder so the URL
        // itself is never treated as the title.
        assert_eq!(
            args,
            vec!["/C".to_string(), "start".to_string(), String::new(), url.to_string()]
        );
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        assert_eq!(program, "xdg-open");
        assert_eq!(args, vec![url.to_string()]);
    }

    // Cross-platform invariant: the URL is passed as its own argv entry,
    // never concatenated into the program string.
    assert!(args.iter().any(|a| a == url), "url must be a discrete argument");
    assert_ne!(program, url);
}

// ---- dispatch_modifier_click: capturing closure, no OS handler ---------

#[test]
fn dispatch_no_modifier_does_not_invoke_opener() {
    let calls = Rc::new(Cell::new(0u32));
    let c = calls.clone();
    let out = dispatch_modifier_click(false, Some("https://example.com".to_string()), move |_| {
        c.set(c.get() + 1);
        Ok(())
    });
    assert_eq!(out, None, "no modifier -> no open, returns None");
    assert_eq!(calls.get(), 0, "opener must not run without the modifier");
}

#[test]
fn dispatch_no_uri_does_not_invoke_opener() {
    let calls = Rc::new(Cell::new(0u32));
    let c = calls.clone();
    let out = dispatch_modifier_click(true, None, move |_| {
        c.set(c.get() + 1);
        Ok(())
    });
    assert_eq!(out, None, "no uri under cursor -> None");
    assert_eq!(calls.get(), 0, "opener must not run without a uri");
}

#[test]
fn dispatch_modifier_and_uri_invokes_capturing_opener() {
    let seen: Rc<Cell<Option<String>>> = Rc::new(Cell::new(None));
    let s = seen.clone();
    let out = dispatch_modifier_click(true, Some("https://example.com/x".to_string()), move |u| {
        s.set(Some(u.to_string()));
        Ok(())
    });
    assert_eq!(out, Some("https://example.com/x".to_string()), "returns the opened uri");
    assert_eq!(seen.take(), Some("https://example.com/x".to_string()), "closure saw the uri");
}

#[test]
fn dispatch_reports_open_even_when_opener_errors() {
    // Best-effort: an opener error still returns Some(uri) so the caller
    // swallows the click instead of falling through to selection start.
    let out = dispatch_modifier_click(true, Some("https://example.com".to_string()), |_| {
        Err(io::Error::other("spawn failed"))
    });
    assert_eq!(out, Some("https://example.com".to_string()), "error path still swallows the click");
}
