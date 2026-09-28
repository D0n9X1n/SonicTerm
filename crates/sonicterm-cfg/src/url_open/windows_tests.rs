//! Windows dispatch tests. They read back the real `SHELLEXECUTEINFOW` through
//! the `with_shell_execute_info` seam and drive `run_handler_lifecycle` with
//! counting closures, so the dispatch structure and the apartment lifecycle are
//! pinned without calling `ShellExecuteExW`.

use super::*;
use std::cell::Cell;

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
fn expected_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

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

#[test]
fn shell_execute_target_preserves_uri_text_exactly() {
    // The validated URI reaches lpFile byte for byte: percent triplets, an
    // environment-looking %NAME% run, non-ASCII text, a drive-letter-looking
    // path, query and fragment all survive, with exactly one NUL and no
    // canonicalization. The corpus covers all three dispatch schemes.
    for uri in [
        "https://example.com/%20space",
        "https://example.com/%USERNAME%/report",
        "https://example.com/\u{00e9}\u{4e2d}\u{6587}?q=1#frag",
        "http://example.com/C:/Users/name/notes.txt",
        "mailto:user@example.com?subject=hi",
        "https://dev.azure.com/example/project/_git/repo?path=%2Fsrc%2Ffile.cs&line=1&lineEnd=10&_a=contents",
    ] {
        assert!(validate(uri).is_ok());
        // Decoded inside the closure, where the owned target buffer is alive.
        let units = with_shell_execute_info(uri, |info| {
            // SAFETY: lpFile points at the owned NUL-terminated target buffer,
            // which with_shell_execute_info keeps alive across this closure.
            unsafe { read_wide_with_nul(info.lpFile.0) }
        });

        assert_eq!(units, expected_wide(uri), "lpFile must match the URI exactly: {uri:?}");
        assert_eq!(units.iter().filter(|&&unit| unit == 0).count(), 1, "exactly one NUL terminator");
        assert_eq!(units.last().copied(), Some(0), "the NUL must terminate the buffer");
    }
}

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
