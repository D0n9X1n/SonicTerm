//! Behavior + security tests for the safe-open policy.
//!
//! These never launch a real OS handler. `open()` is exercised only on inputs
//! that fail `validate()` before any dispatch. The Windows tests read back the
//! real `SHELLEXECUTEINFOW` through the `with_shell_execute_info` seam and
//! drive `run_handler_lifecycle` with counting closures, so the dispatch
//! structure and the apartment lifecycle are pinned without calling
//! `ShellExecuteExW`. `dispatch_modifier_click` is driven with a capturing
//! closure that records the URI instead of opening it.

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
    // Retained defense in depth: no dispatch path re-tokenizes the URI, but
    // each of these embedded in an otherwise-valid https URL is still refused.
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

// ---- open() never dispatches for invalid input -------------------------

#[test]
fn open_returns_invalid_input_without_dispatching_for_bad_urls() {
    // These all fail validation, so `open` returns before any platform
    // dispatch — exercising the guard path without touching an OS handler.
    for bad in ["", "javascript:alert(1)", "https://a.com/\n", "https://a.com/&x"] {
        let err = open(bad).expect_err("invalid url must not dispatch");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

// ---- build_command: inspect argv, never spawn (non-Windows only) -------

// Windows has no `build_command`: it calls `ShellExecuteExW` directly rather
// than constructing any command, so this covers only the argv platforms.
#[cfg(not(target_os = "windows"))]
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

// ---- Windows: the real SHELLEXECUTEINFOW, read without dispatching -----

/// Read a NUL-terminated UTF-16 buffer back, including its terminator.
///
/// # Safety
///
/// `ptr` must be non-null and point to a NUL-terminated UTF-16 buffer that
/// stays valid and unmodified for the whole read. Every caller reads a
/// `SHELLEXECUTEINFOW` string field inside `with_shell_execute_info`, where the
/// owned buffers are still alive.
// SAFETY: read_wide_with_nul dereferences ptr, so the caller must supply a live
// NUL-terminated UTF-16 buffer as the rustdoc contract above states.
#[cfg(target_os = "windows")]
unsafe fn read_wide_with_nul(ptr: *const u16) -> Vec<u16> {
    assert!(!ptr.is_null(), "pointer field must be set");
    let mut out = Vec::new();
    let mut index = 0isize;
    loop {
        let unit = {
            // SAFETY: the caller guarantees a NUL-terminated buffer, and the
            // loop stops at the first NUL, so every offset read stays inside it.
            unsafe { *ptr.offset(index) }
        };
        out.push(unit);
        if unit == 0 {
            // The terminator has been copied, so the buffer is complete.
            break;
        }
        index += 1;
    }
    out
}

/// Expected UTF-16 encoding of `text`, with exactly one trailing NUL.
#[cfg(target_os = "windows")]
fn expected_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "windows")]
#[test]
fn shell_execute_info_pins_every_dispatch_field() {
    // Pins the exact structure handed to ShellExecuteExW: a correct cbSize,
    // the synchronous mask, the "open" verb, no parameters/directory/class,
    // and a normal window. A drift in any field changes what the shell does.
    // The verb is decoded inside the closure: the owned buffers die with the
    // call, so reading lpVerb after it returned would dangle.
    let info_snapshot = with_shell_execute_info("https://example.com/safe", |info| {
        let verb_units = {
            // SAFETY: lpVerb points at the owned NUL-terminated verb buffer,
            // which with_shell_execute_info keeps alive across this closure.
            unsafe { read_wide_with_nul(info.lpVerb.0) }
        };
        (
            info.cbSize,
            info.fMask,
            info.nShow,
            verb_units,
            info.lpParameters.0,
            info.lpDirectory.0,
            info.lpClass.0,
            info.hwnd.0,
            info.hkeyClass.0,
            info.lpIDList,
            info.dwHotKey,
        )
    });
    let (
        cb_size,
        mask,
        n_show,
        verb_units,
        parameters,
        directory,
        class,
        hwnd,
        hkey,
        id_list,
        hot_key,
    ) = info_snapshot;

    assert_eq!(
        cb_size as usize,
        std::mem::size_of::<SHELLEXECUTEINFOW>(),
        "cbSize must equal the structure size or the shell rejects the call"
    );
    assert_eq!(n_show, SW_SHOWNORMAL.0, "handler window is shown normally");
    assert!(parameters.is_null(), "a URI carries no separate parameters");
    assert!(directory.is_null(), "dispatch must not set a current directory");
    assert!(class.is_null(), "no explicit class overrides the registered handler");
    assert!(hwnd.is_null(), "dispatch is not parented to a window");
    assert!(hkey.is_null(), "no class key accompanies the dispatch");
    assert!(id_list.is_null(), "no PIDL accompanies the dispatch");
    assert_eq!(hot_key, 0, "no hot key is requested");
    assert_ne!(mask, 0, "the mask must carry the synchronous dispatch bit");
    assert_eq!(verb_units, expected_wide("open"), "verb must be exactly \"open\"");
}

#[cfg(target_os = "windows")]
#[test]
fn shell_execute_mask_is_synchronous_and_disables_environment_substitution() {
    // Both mask directions matter: NOASYNC is retained for any file-association
    // path, while DOENVSUBST must stay clear so `%` text remains literal.
    use windows::Win32::UI::Shell::SEE_MASK_DOENVSUBST;

    let mask = with_shell_execute_info("https://example.com/%USERNAME%", |info| info.fMask);

    assert_eq!(
        mask & SEE_MASK_NOASYNC,
        SEE_MASK_NOASYNC,
        "SEE_MASK_NOASYNC must remain available to file-association dispatch"
    );
    assert_eq!(
        mask & SEE_MASK_DOENVSUBST,
        0,
        "SEE_MASK_DOENVSUBST must stay clear so % text is not expanded"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn shell_execute_target_preserves_uri_text_exactly() {
    // The validated URI reaches lpFile byte for byte: percent triplets, an
    // environment-looking %NAME% run, non-ASCII text, query and fragment all
    // survive, with exactly one NUL and no canonicalization.
    for uri in [
        "https://example.com/%20space",
        "https://example.com/%USERNAME%/report",
        "https://example.com/\u{00e9}\u{4e2d}\u{6587}?q=1#frag",
        "file:///C:/Users/name/notes.txt",
        "mailto:user@example.com?subject=hi",
    ] {
        // Decoded inside the closure, where the owned target buffer is alive.
        let units = with_shell_execute_info(uri, |info| {
            // SAFETY: lpFile points at the owned NUL-terminated target buffer,
            // which with_shell_execute_info keeps alive across this closure.
            unsafe { read_wide_with_nul(info.lpFile.0) }
        });

        assert_eq!(units, expected_wide(uri), "lpFile must match the URI exactly: {uri:?}");
        assert_eq!(units.iter().filter(|&&u| u == 0).count(), 1, "exactly one NUL terminator");
        assert_eq!(units.last().copied(), Some(0), "the NUL must terminate the buffer");
    }
}

#[cfg(target_os = "windows")]
#[test]
fn native_outcome_maps_success_and_failure() {
    // The binding's Result already encodes the BOOL + GetLastError contract,
    // so success maps to Ok and a failure HRESULT maps to an io error rather
    // than being read back out of hInstApp.
    use windows::core::Error as WindowsError;

    assert!(map_handler_result(Ok(())).is_ok(), "a successful dispatch maps to Ok");

    let failure = WindowsError::from_hresult(HRESULT(0x8000_4005_u32 as i32));
    let err = map_handler_result(Err(failure)).expect_err("a failed dispatch maps to an error");
    assert!(!err.to_string().is_empty(), "the error must describe the failed dispatch");
}

// ---- Windows: apartment lifecycle, injected over the real sequence -----

#[cfg(target_os = "windows")]
#[test]
fn failed_apartment_initialization_skips_invoke_and_uninitialize() {
    // A failed CoInitializeEx means the apartment was never entered, so
    // dispatching anyway or calling CoUninitialize would unbalance COM.
    let invokes = Cell::new(0u32);
    let uninits = Cell::new(0u32);

    let result = run_handler_lifecycle(
        || HRESULT(0x8000_4005_u32 as i32),
        || {
            invokes.set(invokes.get() + 1);
            Ok(())
        },
        || uninits.set(uninits.get() + 1),
    );

    assert!(result.is_err(), "a failed apartment must surface as an error");
    assert_eq!(invokes.get(), 0, "no dispatch may run without an apartment");
    assert_eq!(uninits.get(), 0, "CoUninitialize must not run without initialization");
}

#[cfg(target_os = "windows")]
#[test]
fn successful_apartment_initialization_always_balances_uninitialize() {
    // S_OK and S_FALSE both mean this thread holds an apartment, so each is
    // crossed with a succeeding and a failing dispatch: exactly one invoke
    // and exactly one uninitialize in all four combinations.
    for init in [HRESULT(0), HRESULT(1)] {
        for invoke_ok in [true, false] {
            let invokes = Cell::new(0u32);
            let uninits = Cell::new(0u32);

            let result = run_handler_lifecycle(
                || init,
                || {
                    invokes.set(invokes.get() + 1);
                    if invoke_ok {
                        Ok(())
                    } else {
                        Err(io::Error::other("handler refused"))
                    }
                },
                || uninits.set(uninits.get() + 1),
            );

            assert_eq!(result.is_ok(), invoke_ok, "the dispatch result is reported unchanged");
            assert_eq!(invokes.get(), 1, "exactly one dispatch for init {init:?}");
            assert_eq!(
                uninits.get(),
                1,
                "exactly one CoUninitialize for init {init:?}, invoke_ok {invoke_ok}"
            );
        }
    }
}

#[cfg(target_os = "windows")]
#[test]
fn handler_apartment_constant_is_apartment_threaded_without_ole1_dde() {
    // The worker must enter a single-threaded apartment with OLE1 DDE off, so
    // a stale DDE registration cannot service the request.
    assert_eq!(
        HANDLER_COINIT.0,
        COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0,
        "apartment flags must be exactly APARTMENTTHREADED | DISABLE_OLE1DDE"
    );
    assert_eq!(HANDLER_COINIT.0 & COINIT_APARTMENTTHREADED.0, COINIT_APARTMENTTHREADED.0);
    assert_eq!(HANDLER_COINIT.0 & COINIT_DISABLE_OLE1DDE.0, COINIT_DISABLE_OLE1DDE.0);
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
        Err(io::Error::other("dispatch failed"))
    });
    assert_eq!(out, Some("https://example.com".to_string()), "error path still swallows the click");
}
