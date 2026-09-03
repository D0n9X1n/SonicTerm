//! Cross-platform "open this URI in the user's default handler" helper.
//!
//! Used for OSC 8 hyperlink clicks and plain-text URI clicks. Dispatch is
//! fire-and-forget: the call returns once the request has been handed to the
//! platform, never after the handler has finished starting.
//!
//! ## Dispatch boundary
//!
//! No platform routes a URI through a command interpreter. Windows calls
//! `ShellExecuteExW` directly, from a worker thread that owns its own COM
//! apartment, and passes the URI as one NUL-terminated UTF-16 string. macOS and
//! freedesktop pass the URI as a single `argv` entry to `open` / `xdg-open`.
//! Because no argument string is ever re-parsed by a shell, quoting and
//! metacharacter handling cannot split one URI into a second command.
//!
//! ## Security
//!
//! OSC 8 URIs come from untrusted pty output, so every URI is checked against
//! a strict allow-list before any dispatch happens. That check is retained
//! defense in depth rather than the sole barrier: it bounds what reaches the
//! handler even though no shell tokenizer stands behind it.
//!
//! - Only `http://`, `https://`, `mailto:`, and `file://` schemes are
//!   permitted.
//! - The URI must not contain a shell metacharacter
//!   (`& | ^ < > " ' \` CR LF NUL + other control chars`).
//! - Capped at 4096 chars.
//!
//! The URI text itself is preserved when encoded as UTF-16. Percent-encoded
//! triplets, `%`-delimited runs that resemble environment references, and
//! non-ASCII characters all reach the handler exactly as validated; nothing
//! expands or canonicalizes them.

use std::io;
#[cfg(not(target_os = "windows"))]
use std::process::{Command, Stdio};

#[cfg(target_os = "windows")]
use windows::core::{HRESULT, PCWSTR};
#[cfg(target_os = "windows")]
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// COM apartment the handler worker initializes before dispatching.
///
/// Single-threaded apartment matches what shell handlers expect, and
/// disabling OLE1 DDE keeps a stale DDE registration from servicing the
/// request instead of the modern handler.
#[cfg(target_os = "windows")]
const HANDLER_COINIT: COINIT = COINIT(COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0);

/// Name given to the thread that owns the COM apartment and dispatches.
#[cfg(target_os = "windows")]
const HANDLER_THREAD_NAME: &str = "sonicterm-url-open";

/// Open `url` with the platform's default handler.
///
/// Validates first and returns `InvalidInput` for an unsafe or unsupported
/// URI, so nothing is dispatched for a rejected URI. On Windows the validated
/// URI is moved to a worker thread that owns a COM apartment and calls
/// `ShellExecuteExW`; `open` returns without waiting for the handler and
/// retains no handler process, so it fails synchronously only when validation
/// or thread spawn fails.
pub fn open(url: &str) -> io::Result<()> {
    validate(url)?;
    open_validated(url)
}

/// Dispatch an already validated URI to the Windows default handler.
#[cfg(target_os = "windows")]
fn open_validated(url: &str) -> io::Result<()> {
    let uri = url.to_owned();
    std::thread::Builder::new()
        .name(HANDLER_THREAD_NAME.to_owned())
        .spawn(move || {
            if let Err(err) = open_uri_with_default_handler(&uri) {
                // The worker owns the only report of a failed dispatch, because
                // `open` has already returned to its caller.
                tracing::warn!(error = %err, "default handler did not accept the uri");
            }
        })
        // Dropping the handle detaches the worker: no handler process or join
        // handle is retained past dispatch.
        .map(|_| ())
}

/// Dispatch an already validated URI to the platform default handler.
#[cfg(not(target_os = "windows"))]
fn open_validated(url: &str) -> io::Result<()> {
    let mut cmd = build_command(url);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

/// Strict allow-list check applied to every URI before dispatch. Public so
/// callers can also use it to gate which OSC 8 cells render as clickable.
pub fn validate(url: &str) -> io::Result<()> {
    if url.is_empty() {
        // When: url carries no scheme to match against the allow-list, so no
        // handler may be invoked for it.
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
        // file, so no other URI reaches a handler.
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "scheme not allowed"));
    }
    for ch in url.chars() {
        match ch {
            '&' | '|' | '^' | '<' | '>' | '"' | '\'' | '`' | '\r' | '\n' | '\0' => {
                // When: ch is a shell metacharacter, refused as defense in
                // depth even though no dispatch path re-tokenizes the URI.
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

/// Encode `text` as a NUL-terminated UTF-16 buffer for a native string field.
#[cfg(target_os = "windows")]
fn wide_nul(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Build the real `SHELLEXECUTEINFOW` for `uri` and hand it to `use_info`.
///
/// The owned UTF-16 verb and target buffers outlive the call, so the `lpVerb`
/// and `lpFile` pointers stay valid for as long as `use_info` runs. Tests use
/// this seam to read back the exact structure without invoking a handler.
#[cfg(target_os = "windows")]
fn with_shell_execute_info<R>(uri: &str, use_info: impl FnOnce(&mut SHELLEXECUTEINFOW) -> R) -> R {
    // Both buffers are bound to locals so they outlive `use_info`; pointing
    // the structure at a temporary would dangle before the call is made.
    let verb = wide_nul("open");
    let target = wide_nul(uri);

    let mut info = SHELLEXECUTEINFOW {
        // The shell validates cbSize against the structure it expects, and
        // rejects the call outright when it does not match.
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // SEE_MASK_NOASYNC is ignored for ordinary URI launches but retained for
        // any file-association path; DOENVSUBST stays absent so `%` remains literal.
        fMask: SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(target.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        // Remaining fields stay zeroed: no parameters, directory, class,
        // parent window, or PIDL accompanies a URI dispatch.
        ..Default::default()
    };

    use_info(&mut info)
}

/// Map the native dispatch outcome onto an `io::Result`.
///
/// `ShellExecuteExW` already reports failure through the Win32 `BOOL` plus
/// `GetLastError` contract that the binding folds into `Result`, so the
/// legacy `hInstApp > 32` comparison is not consulted.
#[cfg(target_os = "windows")]
fn map_handler_result(outcome: windows::core::Result<()>) -> io::Result<()> {
    outcome.map_err(|err| io::Error::other(format!("default handler rejected the uri: {err}")))
}

/// Ask the shell to open `uri` with its registered default handler.
#[cfg(target_os = "windows")]
fn shell_execute_uri(uri: &str) -> io::Result<()> {
    let outcome = with_shell_execute_info(uri, |info| {
        // SAFETY: info is a fully initialized SHELLEXECUTEINFOW whose cbSize
        // matches and whose verb and target buffers outlive this closure call.
        unsafe { ShellExecuteExW(info) }
    });
    map_handler_result(outcome)
}

/// Run the initialize / invoke / uninitialize sequence for one dispatch.
///
/// `initialize` failing means the apartment was never entered, so neither
/// `invoke` nor `uninitialize` may run. Any success code, including `S_FALSE`
/// for an apartment this thread already entered, runs `invoke` exactly once
/// and `uninitialize` exactly once even when `invoke` fails.
#[cfg(target_os = "windows")]
fn run_handler_lifecycle(
    initialize: impl FnOnce() -> HRESULT,
    invoke: impl FnOnce() -> io::Result<()>,
    uninitialize: impl FnOnce(),
) -> io::Result<()> {
    // `ok()` treats every non-negative code as success, so S_FALSE — this
    // thread already holds a compatible apartment — still owes an uninitialize.
    if let Err(err) = initialize().ok() {
        // When: initialize().ok() reports failure, so no apartment was entered
        // and neither invoke nor uninitialize may run against COM state.
        return Err(io::Error::other(format!("com apartment unavailable: {err}")));
    }

    let dispatched = invoke();
    // Runs on both dispatch outcomes: the apartment is owed exactly one
    // uninitialize once initialization reported success.
    uninitialize();
    dispatched
}

/// Enter a COM apartment, dispatch `uri` to the default handler, and leave.
#[cfg(target_os = "windows")]
fn open_uri_with_default_handler(uri: &str) -> io::Result<()> {
    run_handler_lifecycle(
        || {
            // SAFETY: CoInitializeEx receives a null reserved pointer and a valid
            // COINIT value, and its HRESULT is checked before any COM call runs.
            unsafe { CoInitializeEx(None, HANDLER_COINIT) }
        },
        || shell_execute_uri(uri),
        || {
            // SAFETY: reached only after CoInitializeEx reported success on this
            // same thread, so it balances exactly one successful initialization.
            unsafe { CoUninitialize() }
        },
    )
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
    // Best-effort dispatch; an error from the opener does NOT cause us
    // to fall through to selection start (the user clearly intended
    // to open a link). Caller logs the error.
    let _ = open_fn(&uri);
    Some(uri)
}

#[cfg(test)]
#[path = "url_open_tests.rs"]
mod url_open_tests;
