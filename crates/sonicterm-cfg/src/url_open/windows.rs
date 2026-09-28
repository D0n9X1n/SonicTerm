//! URI dispatch on Windows: `ShellExecuteExW`, called from a worker thread that
//! owns its own COM apartment.

use super::*;
use windows::core::{HRESULT, PCWSTR};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// COM apartment the handler worker initializes before dispatching.
///
/// Single-threaded apartment matches what shell handlers expect, and
/// disabling OLE1 DDE keeps a stale DDE registration from servicing the
/// request instead of the modern handler.
const HANDLER_COINIT: COINIT = COINIT(COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0);

/// Name given to the thread that owns the COM apartment and dispatches.
const HANDLER_THREAD_NAME: &str = "sonicterm-url-open";

/// Dispatch an already validated URI to the Windows default handler.
pub(super) fn open_validated(url: &str) -> io::Result<()> {
    let uri = url.to_owned();
    std::thread::Builder::new()
        .name(HANDLER_THREAD_NAME.to_owned())
        .spawn(move || {
            if let Err(err) = open_uri_with_default_handler(&uri) {
                // The worker owns the only report of a failed dispatch, because
                // `open` has already returned to its caller.
                tracing::warn!(
                    target: "sonicterm_cfg::url_open",
                    error = %err,
                    "default handler did not accept the uri"
                );
            }
        })
        // Dropping the handle detaches the worker: no handler process or join
        // handle is retained past dispatch.
        .map(|_| ())
}

/// Encode `text` as a NUL-terminated UTF-16 buffer for a native string field.
fn wide_nul(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Build the real `SHELLEXECUTEINFOW` for `uri` and hand it to `use_info`.
///
/// The owned UTF-16 verb and target buffers outlive the call, so the `lpVerb`
/// and `lpFile` pointers stay valid for as long as `use_info` runs. Tests use
/// this seam to read back the exact structure without invoking a handler.
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
fn map_handler_result(outcome: windows::core::Result<()>) -> io::Result<()> {
    outcome.map_err(|err| io::Error::other(format!("default handler rejected the uri: {err}")))
}

/// Ask the shell to open `uri` with its registered default handler.
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

#[cfg(test)]
#[path = "windows_tests.rs"]
mod windows_tests;
