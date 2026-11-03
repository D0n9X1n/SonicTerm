//! Connecting on Windows: a local named pipe whose server must run as the caller's user.
//!
//! The pipe is opened with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`, so a server that
//! squatted the name can identify the bridge but never act as it, and then
//! `GetNamedPipeServerProcessId` must name a process whose token user is the caller's. Reads and
//! writes are overlapped, one per direction, so a blocked read never holds up a write.

use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_BROKEN_PIPE, ERROR_IO_PENDING,
    ERROR_PIPE_NOT_CONNECTED, ERROR_SEM_TIMEOUT, HANDLE, HLOCAL, STILL_ACTIVE, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_NONE, OPEN_EXISTING, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

/// The prefix every SonicTerm pipe name has.
pub const PIPE_PREFIX: &str = r"\\.\pipe\sonicterm-control-";

/// The discovery directory SonicTerm writes `<pid>` records to.
pub fn endpoint_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join("SonicTerm").join("control"))
}

/// The pipe name a discovery record holds, if it is a SonicTerm pipe.
pub fn record_address(path: &Path) -> Option<String> {
    let address = std::fs::read_to_string(path).ok()?;
    let address = address.trim();
    address.starts_with(PIPE_PREFIX).then(|| address.to_owned())
}

/// Whether a server pipe named `address` exists, without opening it: a free instance, or every
/// instance busy, both mean a server is listening; no pipe by that name means a stale record.
pub fn listening(address: &str) -> bool {
    let wide = HSTRING::from(address);
    let ready =
        // SAFETY: `wide` is a NUL-terminated name alive for the call, which only takes values.
        unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 1) };
    if ready.as_bool() {
        // When: `ready` is true, an instance is free, so a server is listening.
        return true;
    }
    let error =
        // SAFETY: `GetLastError` takes no arguments and reads this thread's last error.
        unsafe { GetLastError() };
    error == ERROR_SEM_TIMEOUT
}

/// An owned kernel handle, closed on drop.
struct OwnedHandle(HANDLE);

// SAFETY: a pipe or event handle is a process-wide kernel object reference; each direction issues
// overlapped IO with its own OVERLAPPED and event, which Windows allows concurrently.
unsafe impl Send for OwnedHandle {}
// SAFETY: as above, shared use only issues independent overlapped calls.
unsafe impl Sync for OwnedHandle {}

// Lifecycle: OwnedHandle drop calls CloseHandle exactly once.
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ =
            // SAFETY: the handle is owned by this value and closed only here.
            unsafe { CloseHandle(self.0) };
    }
}

/// The SID string of the user a process token belongs to.
fn token_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: `process` is a live process handle; `token` receives an owned handle.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }?;
    let token = OwnedHandle(token);
    let mut needed = 0u32;
    // The first call only reports the size; its error is expected.
    let _ =
        // SAFETY: `token` is live; a null buffer of length 0 asks only for `needed`.
        unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut needed) };
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: `buffer` is writable for `needed` bytes and outlives the call.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
    }?;
    let user =
        // SAFETY: the call succeeded, so `buffer` starts with an initialized, aligned `TOKEN_USER`.
        unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = PWSTR::null();
    // SAFETY: the SID points into `buffer`, alive here; `text` is freed with `LocalFree` below.
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }?;
    let sid =
        // SAFETY: `text` is a NUL-terminated wide string until the `LocalFree` below.
        unsafe { text.to_string() };
    // SAFETY: `text` came from `ConvertSidToStringSidW`, which allocates with `LocalAlloc`.
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    sid.map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
}

/// The SID string of the user running process `pid`.
fn process_user_sid(pid: u32) -> io::Result<String> {
    let opened =
        // SAFETY: `OpenProcess` takes only values (`pid` and access flags); `process` owns the handle.
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }?;
    let process = OwnedHandle(opened);
    token_user_sid(process.0)
}

/// Whether process `pid` is still running.
pub fn process_alive(pid: u32) -> bool {
    let opened =
        // SAFETY: `OpenProcess` takes only values (`pid` and access flags); `handle` owns it below.
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
    let Ok(handle) = opened else {
        // When: `opened` holds an error for `pid`, the process is gone or cannot be queried.
        return false;
    };
    let handle = OwnedHandle(handle);
    let mut code = 0u32;
    let read =
        // SAFETY: `handle` is live and `code` is writable.
        unsafe { GetExitCodeProcess(handle.0, &mut code) };
    read.is_ok() && code == STILL_ACTIVE.0 as u32
}

/// Whether `address` names a pipe on this machine; any other name could reach a remote server.
pub fn is_local_pipe(address: &str) -> bool {
    address.starts_with(r"\\.\pipe\")
}

/// Whether a server whose user lookup gave `server` may be talked to by a caller whose user is
/// `caller`: only the same SID, and a failed lookup is refused.
pub fn trust_server(server: io::Result<String>, caller: &str) -> bool {
    server.is_ok_and(|server| server == caller)
}

/// The Win32 code inside a `windows` error.
fn win32_code(error: &windows::core::Error) -> Option<WIN32_ERROR> {
    WIN32_ERROR::from_error(error)
}

/// One direction of the pipe: the shared handle and this direction's own event.
struct Half {
    /// The pipe.
    pipe: Arc<OwnedHandle>,
    /// This direction's manual-reset event.
    event: OwnedHandle,
}

impl Half {
    /// A direction of `pipe` with a fresh event.
    fn new(pipe: Arc<OwnedHandle>) -> io::Result<Self> {
        let event =
            // SAFETY: `CreateEventW` makes an unnamed manual-reset event; `OwnedHandle` owns `event`.
            unsafe { CreateEventW(None, true, false, PCWSTR::null()) }?;
        Ok(Self { pipe, event: OwnedHandle(event) })
    }

    /// Wait for the operation `overlapped` tracks and return its byte count.
    fn finish(&self, overlapped: &OVERLAPPED) -> windows::core::Result<u32> {
        let mut transferred = 0u32;
        // SAFETY: `overlapped` belongs to an operation issued on this pipe; waiting (`true`)
        // returns only once Windows has finished with it and its buffer.
        unsafe { GetOverlappedResult(self.pipe.0, overlapped, &mut transferred, true) }?;
        Ok(transferred)
    }
}

impl Read for Half {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut overlapped = OVERLAPPED { hEvent: self.event.0, ..Default::default() };
        let length = buffer.len().min(u32::MAX as usize);
        let issued =
            // SAFETY: `buffer` and `overlapped` outlive the operation; `finish` waits for it.
            unsafe {
            ReadFile(
                self.pipe.0,
                Some(&mut buffer[..length]),
                None,
                Some(std::ptr::from_mut(&mut overlapped)),
            )
        };
        let outcome = match issued {
            Err(error) if win32_code(&error) != Some(ERROR_IO_PENDING) => Err(error),
            _ => self.finish(&overlapped),
        };
        match outcome {
            Ok(count) => Ok(count as usize),
            Err(error)
                if matches!(
                    win32_code(&error),
                    Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED)
                ) =>
            {
                Ok(0)
            }
            Err(error) => Err(io::Error::other(error.message())),
        }
    }
}

impl Write for Half {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut overlapped = OVERLAPPED { hEvent: self.event.0, ..Default::default() };
        let length = buffer.len().min(u32::MAX as usize);
        let issued =
            // SAFETY: `buffer` and `overlapped` outlive the operation; `finish` waits for it.
            unsafe {
            WriteFile(
                self.pipe.0,
                Some(&buffer[..length]),
                None,
                Some(std::ptr::from_mut(&mut overlapped)),
            )
        };
        let outcome = match issued {
            Err(error) if win32_code(&error) != Some(ERROR_IO_PENDING) => Err(error),
            _ => self.finish(&overlapped),
        };
        outcome
            .map(|count| count as usize)
            .map_err(|error| io::Error::new(ErrorKind::BrokenPipe, error.message()))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Open `address`, check its server runs as the caller, and split it into a reader and a writer.
pub fn connect(
    address: &str,
) -> io::Result<(impl Read + Send + 'static, impl Write + Send + 'static)> {
    if !is_local_pipe(address) {
        // When: `address` does not start with the local pipe prefix, it could name a remote server.
        return Err(io::Error::new(ErrorKind::InvalidInput, "the endpoint is not a local pipe"));
    }
    let wide = HSTRING::from(address);
    let handle =
        // SAFETY: `wide` is a NUL-terminated name alive for the call; `pipe` owns the handle below.
        unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            (FILE_GENERIC_READ | FILE_GENERIC_WRITE).0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            None,
        )
    }?;
    let pipe = Arc::new(OwnedHandle(handle));
    let mut server_pid = 0u32;
    // SAFETY: `pipe` is a live client handle and `server_pid` is writable.
    unsafe { GetNamedPipeServerProcessId(pipe.0, &mut server_pid) }?;
    let current =
        // SAFETY: `GetCurrentProcess` takes no arguments; its pseudo-handle `current` needs no close.
        unsafe { GetCurrentProcess() };
    let caller = token_user_sid(current)?;
    if !trust_server(process_user_sid(server_pid), &caller) {
        // When: `trust_server` refused `server_pid`, its user differs from `caller` or is unknown.
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "the endpoint belongs to another user",
        ));
    }
    Ok((Half::new(Arc::clone(&pipe))?, Half::new(pipe)?))
}

#[cfg(test)]
#[path = "connect_windows_tests.rs"]
mod connect_windows_tests;
