//! The Windows endpoint: a local named pipe only the current user can open, and a discovery file
//! naming it.
//!
//! The pipe is `\\.\pipe\sonicterm-control-<pid>`. Its security descriptor names the current user
//! as owner and holds one allow entry, read and write for that user's SID, with inheritance off;
//! remote clients are rejected. The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`,
//! so another process that squats the name makes start fail instead of sharing it. All IO is
//! overlapped and bounded, so a reader rechecks its stop flags and a writer gives up on a client
//! that stopped reading.
//!
//! The pipe name is also written to `%LOCALAPPDATA%\SonicTerm\control\<pid>`, in a directory
//! created with an owner-only, inheritance-protected DACL and refused unless it has exactly that
//! owner and DACL. Records whose process is gone are removed at start; the server's own record
//! goes when the server is dropped.

use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE, ERROR_IO_PENDING,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE, STILL_ACTIVE, WAIT_TIMEOUT, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1,
    SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, DACL_SECURITY_INFORMATION, OBJECT_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateDirectoryW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use super::{McpShared, Transport, READ_POLL, WRITE_DEADLINE};

/// How often the listener checks the stop flag while it waits for a client.
const ACCEPT_POLL: Duration = Duration::from_millis(100);
/// The pipe's buffer size in each direction.
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// The pipe name for process `pid`.
pub(crate) fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\sonicterm-control-{pid}")
}

/// The descriptor string for the discovery directory: owner `sid`, full control for it alone,
/// inherited by the files it holds, and no inheritance from above.
pub(crate) fn owner_only_dir_sddl(sid: &str) -> String {
    format!("O:{sid}D:P(A;OICI;FA;;;{sid})")
}

/// The security descriptor string that admits only `sid`: owner `sid`, a protected DACL with one
/// read-and-write allow entry for it.
pub(crate) fn owner_only_sddl(sid: &str) -> String {
    format!("O:{sid}D:P(A;;GRGW;;;{sid})")
}

/// An owned kernel handle, closed on drop.
pub(crate) struct OwnedHandle(HANDLE);

// SAFETY: a pipe or event handle is a process-wide kernel object reference; the reader and writer
// each issue IO with their own OVERLAPPED and event, which Windows allows concurrently.
unsafe impl Send for OwnedHandle {}
// SAFETY: as above, shared use only issues independent overlapped calls on the same handle.
unsafe impl Sync for OwnedHandle {}

// Lifecycle: OwnedHandle drop calls CloseHandle once; a pipe's client then sees end of stream.
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ =
            // SAFETY: the handle is owned by this value and closed exactly once, here.
            unsafe { CloseHandle(self.0) };
    }
}

/// A `LocalAlloc`'d buffer from a conversion call, freed on drop.
struct LocalBuffer(*mut core::ffi::c_void);

// SAFETY: the buffer is plain memory owned by this value alone; moving it to the listener thread
// moves the only pointer to it.
unsafe impl Send for LocalBuffer {}

// Lifecycle: LocalBuffer drop calls LocalFree on what the security conversion calls allocated.
impl Drop for LocalBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // When: the pointer `is_null`, nothing was allocated and nothing is freed.
            // SAFETY: the pointer came from a call documented to allocate with `LocalAlloc`, and
            // is freed exactly once, here.
            unsafe { LocalFree(Some(HLOCAL(self.0))) };
        }
    }
}

/// The current process user's SID in string form.
pub(crate) fn current_user_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle from `GetCurrentProcess` needs no close, and `token` is writable
    // storage for one owned handle.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }?;
    let token = OwnedHandle(token);
    let mut needed = 0u32;
    // The first call only reports the buffer size; its error is expected.
    let _ =
        // SAFETY: `token` is live; a null buffer of length 0 asks only for `needed`.
        unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut needed) };
    // `u64` words keep the buffer aligned for `TOKEN_USER`'s pointer field.
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
        // SAFETY: the call succeeded, so `buffer` begins with an initialized, aligned `TOKEN_USER`.
        unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = PWSTR::null();
    // SAFETY: `user.User.Sid` points into `buffer`, which is alive; `text` receives a LocalAlloc'd
    // string freed by `LocalBuffer`.
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }?;
    let owned = LocalBuffer(text.0.cast());
    let sid =
        // SAFETY: `text` is a NUL-terminated wide string that `owned` keeps alive past this read.
        unsafe { text.to_string() };
    drop(owned);
    sid.map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
}

/// The pipe's security descriptor, kept alive while instances are created with it.
pub(crate) struct PipeSecurity {
    /// The self-relative descriptor `ConvertStringSecurityDescriptorToSecurityDescriptorW` built.
    descriptor: LocalBuffer,
}

impl PipeSecurity {
    /// Build the owner-only descriptor for the current user.
    pub(crate) fn current_user() -> io::Result<Self> {
        let sddl = HSTRING::from(owner_only_sddl(&current_user_sid()?));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a NUL-terminated string alive for the call; `descriptor` receives a
        // LocalAlloc'd descriptor that `LocalBuffer` frees.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }?;
        Ok(Self { descriptor: LocalBuffer(descriptor.0) })
    }

    /// Attributes that point at the descriptor; valid while `self` lives.
    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor.0,
            bInheritHandle: false.into(),
        }
    }
}

/// Create one pipe instance; `first` refuses a name another process already holds.
pub(crate) fn create_instance(
    name: &HSTRING,
    security: &PipeSecurity,
    first: bool,
) -> io::Result<OwnedHandle> {
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let attributes = security.attributes();
    let handle =
        // SAFETY: `name` and `attributes` (and the descriptor it points to) outlive the call.
        unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(open_mode.0),
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            Some(&attributes),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        // When: `CreateNamedPipeW` returned `INVALID_HANDLE_VALUE`, the OS error says why.
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(handle))
}

/// A manual-reset event for one overlapped operation.
fn new_event() -> io::Result<OwnedHandle> {
    let event =
        // SAFETY: an unnamed event with default security; the returned handle is owned.
        unsafe { CreateEventW(None, true, false, PCWSTR::null()) }?;
    Ok(OwnedHandle(event))
}

/// The Win32 code inside a `windows` error, when it carries one.
fn win32_code(error: &windows::core::Error) -> Option<WIN32_ERROR> {
    WIN32_ERROR::from_error(error)
}

/// A `windows` error as an `io::Error` carrying the Win32 code, so `kind()` and
/// `raw_os_error()` mean what they do for std's own pipe errors.
fn io_error(error: &windows::core::Error) -> io::Error {
    match win32_code(error) {
        Some(code) => io::Error::from_raw_os_error(code.0 as i32),
        None => io::Error::other(error.message()),
    }
}

/// Whether `error` means the client closed its end.
fn is_disconnect(error: &windows::core::Error) -> bool {
    matches!(win32_code(error), Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED))
}

/// Whether an overlapped call was accepted and is still running.
fn is_pending(error: &windows::core::Error) -> bool {
    win32_code(error) == Some(ERROR_IO_PENDING)
}

/// Whether the event `overlapped` signals fired within `wait`.
fn signalled(overlapped: &OVERLAPPED, wait: Duration) -> bool {
    let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
    let outcome =
        // SAFETY: `hEvent` is a live event owned by the operation's issuer for the whole call.
        unsafe { WaitForSingleObject(overlapped.hEvent, millis) };
    outcome != WAIT_TIMEOUT
}

/// The finished operation's byte count; call only after it completed or was cancelled.
fn reap(handle: HANDLE, overlapped: &OVERLAPPED) -> windows::core::Result<u32> {
    let mut transferred = 0u32;
    // SAFETY: `overlapped` belongs to an operation issued on `handle`; waiting (`true`) returns
    // only once it is finished, after which Windows no longer touches it or its buffer.
    unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, true) }?;
    Ok(transferred)
}

/// Cancel the operation `overlapped` tracks and wait until Windows has finished with it.
fn cancel(handle: HANDLE, overlapped: &OVERLAPPED) -> windows::core::Result<u32> {
    // A failed cancel means the operation already finished; the reap below collects it.
    let _ =
        // SAFETY: `overlapped` identifies an operation issued on `handle` that is still alive.
        unsafe { CancelIoEx(handle, Some(overlapped)) };
    reap(handle, overlapped)
}

/// Wait up to `wait` for an issued operation; cancel it on timeout. The buffer it uses is never
/// touched by Windows after this returns. `None` means it timed out with nothing transferred.
fn complete(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    wait: Duration,
) -> windows::core::Result<Option<u32>> {
    if signalled(overlapped, wait) {
        // When: the event was `signalled`, the operation finished and only needs reaping.
        return reap(handle, overlapped).map(Some);
    }
    match cancel(handle, overlapped) {
        Ok(0) => Ok(None),
        Ok(count) => Ok(Some(count)),
        Err(error) if win32_code(&error) == Some(ERROR_OPERATION_ABORTED) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The reading half of a connected pipe; a read that waits [`READ_POLL`] reports `TimedOut`.
struct PipeReader {
    /// The shared pipe handle.
    pipe: Arc<OwnedHandle>,
    /// This half's own event.
    event: OwnedHandle,
}

impl Read for PipeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut overlapped = OVERLAPPED { hEvent: self.event.0, ..Default::default() };
        let length = buffer.len().min(u32::MAX as usize);
        let issued =
            // SAFETY: `buffer` and `overlapped` outlive the operation: `complete` returns only after it
            // completed or was cancelled and reaped.
            unsafe {
            ReadFile(
                self.pipe.0,
                Some(&mut buffer[..length]),
                None,
                Some(std::ptr::from_mut(&mut overlapped)),
            )
        };
        let outcome = match issued {
            Err(error) if !is_pending(&error) => Err(error),
            _ => complete(self.pipe.0, &overlapped, READ_POLL),
        };
        match outcome {
            Ok(Some(count)) => Ok(count as usize),
            Ok(None) => Err(ErrorKind::TimedOut.into()),
            // A closed client end reads as end of stream, like a socket.
            Err(error) if is_disconnect(&error) => Ok(0),
            Err(error) => Err(io_error(&error)),
        }
    }
}

/// The writing half of a connected pipe; a write that cannot finish in [`WRITE_DEADLINE`] fails.
struct PipeWriter {
    /// The shared pipe handle.
    pipe: Arc<OwnedHandle>,
    /// This half's own event.
    event: OwnedHandle,
}

impl Write for PipeWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut overlapped = OVERLAPPED { hEvent: self.event.0, ..Default::default() };
        let length = buffer.len().min(u32::MAX as usize);
        let issued =
            // SAFETY: `buffer` and `overlapped` outlive the operation: `complete` returns only after it
            // completed or was cancelled and reaped.
            unsafe {
            WriteFile(
                self.pipe.0,
                Some(&buffer[..length]),
                None,
                Some(std::ptr::from_mut(&mut overlapped)),
            )
        };
        let outcome = match issued {
            Err(error) if !is_pending(&error) => Err(error),
            _ => complete(self.pipe.0, &overlapped, WRITE_DEADLINE),
        };
        match outcome {
            Ok(Some(count)) => Ok(count as usize),
            Ok(None) => Err(ErrorKind::TimedOut.into()),
            Err(error) => Err(io_error(&error)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Split a connected instance into the reader and writer a connection uses.
pub(crate) fn transport(pipe: OwnedHandle) -> io::Result<Transport> {
    let pipe = Arc::new(pipe);
    let reader = PipeReader { pipe: Arc::clone(&pipe), event: new_event()? };
    let writer = PipeWriter { pipe, event: new_event()? };
    Ok(Transport { reader: Box::new(reader), writer: Box::new(writer) })
}

/// The discovery directory under `%LOCALAPPDATA%`.
pub(crate) fn discovery_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join("SonicTerm").join("control"))
}

/// Removes the server's discovery record when dropped.
pub(crate) struct DiscoveryGuard {
    /// The record this process wrote.
    path: PathBuf,
}

// Lifecycle: DiscoveryGuard drop calls remove_file on this process's discovery record.
impl Drop for DiscoveryGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// `dir`'s owner and DACL as a descriptor string.
pub(crate) fn dir_sddl(dir: &Path) -> io::Result<String> {
    let wide = HSTRING::from(dir.as_os_str());
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let information = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let status =
        // SAFETY: `wide` is a NUL-terminated path alive for the call; `descriptor` receives a
        // LocalAlloc'd descriptor freed by `LocalBuffer` below.
        unsafe {
            GetNamedSecurityInfoW(
                PCWSTR(wide.as_ptr()),
                SE_FILE_OBJECT,
                information,
                None,
                None,
                None,
                None,
                &mut descriptor,
            )
        };
    if status.is_err() {
        // When: the `status` is an error, the directory cannot be vouched for.
        return Err(io::Error::from_raw_os_error(status.0 as i32));
    }
    let _owned = LocalBuffer(descriptor.0);
    descriptor_text(descriptor, information)
}

/// `descriptor`'s owner and DACL as Windows writes them in SDDL, which may use an alias such as
/// `LA` (the local administrator) where a SID string was given.
fn descriptor_text(
    descriptor: PSECURITY_DESCRIPTOR,
    information: OBJECT_SECURITY_INFORMATION,
) -> io::Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: `descriptor` is a valid self-relative descriptor; `text` is freed by `_text` below.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            information,
            &mut text,
            None,
        )
    }?;
    let _text = LocalBuffer(text.0.cast());
    // SAFETY: `text` is a NUL-terminated wide string kept alive by `_text`.
    unsafe { text.to_string() }.map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
}

/// `sddl` as Windows writes it back: parsed into a descriptor and written out again, so a SID
/// that has an alias (the local administrator is `LA`) compares equal to what a real
/// directory's descriptor reports.
pub(crate) fn canonical_sddl(sddl: &str) -> io::Result<String> {
    let wide = HSTRING::from(sddl);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `wide` is a NUL-terminated string alive for the call; `descriptor` is freed by
    // `_owned` below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }?;
    let _owned = LocalBuffer(descriptor.0);
    descriptor_text(descriptor, OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION)
}

/// Create the discovery directory owner-only if missing, then refuse it unless its owner and DACL
/// are exactly the owner-only ones: a directory someone else made is never trusted.
pub(crate) fn prepare_discovery_dir(dir: &Path, sid: &str) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sddl = HSTRING::from(owner_only_dir_sddl(sid));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `sddl` is a NUL-terminated string alive for the call; `descriptor` is freed by
    // `_owned` below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }?;
    let _owned = LocalBuffer(descriptor.0);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let wide = HSTRING::from(dir.as_os_str());
    let created =
        // SAFETY: `wide` and `attributes` (and the descriptor it points to) outlive the call.
        unsafe { CreateDirectoryW(PCWSTR(wide.as_ptr()), Some(&attributes)) };
    // An existing directory is checked below like a new one.
    if let Err(error) = created.or_else(|error| match win32_code(&error) {
        Some(ERROR_ALREADY_EXISTS) => Ok(()),
        _ => Err(error),
    }) {
        // When: `created` failed with an `error` other than `ERROR_ALREADY_EXISTS`, stop.
        return Err(io_error(&error));
    }
    let found = dir_sddl(dir)?;
    // Both sides go through Windows' own SDDL writer, so an aliased SID cannot cause a mismatch.
    if found != canonical_sddl(&owner_only_dir_sddl(sid))? {
        // When: the `found` descriptor is not exactly the owner-only one, the directory is refused.
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!("discovery dir is {found}"),
        ));
    }
    Ok(())
}

/// Whether process `pid` is still running.
fn process_alive(pid: u32) -> bool {
    let opened =
        // SAFETY: `OpenProcess` takes only values; the handle is owned and closed by `OwnedHandle`.
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
    let Ok(handle) = opened else {
        // When: the process could not be `opened`, it is gone, or not ours and so not ours to keep.
        return false;
    };
    let handle = OwnedHandle(handle);
    let mut code = 0u32;
    let read =
        // SAFETY: `handle` is a live process handle and `code` is writable storage.
        unsafe { GetExitCodeProcess(handle.0, &mut code) };
    read.is_ok() && code == STILL_ACTIVE.0 as u32
}

/// Remove discovery records whose process is gone.
pub(crate) fn remove_stale_records(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // When: `read_dir` fails, there is nothing to clean.
        return;
    };
    for entry in entries.flatten() {
        let pid = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok());
        if pid.is_some_and(|pid| !process_alive(pid)) {
            // When: the record's `pid` is not `process_alive`, the record is stale.
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Write the discovery record naming `pipe` in `dir` and return its guard.
pub(crate) fn publish_in(dir: &Path, pipe: &str) -> io::Result<DiscoveryGuard> {
    prepare_discovery_dir(dir, &current_user_sid()?)?;
    remove_stale_records(dir);
    let path = dir.join(std::process::id().to_string());
    std::fs::write(&path, pipe)?;
    Ok(DiscoveryGuard { path })
}

/// Create the pipe's first instance, publish its discovery record and start the listener thread;
/// returns the pipe name and the record's guard. A record that cannot be written is logged; panes
/// still learn the pipe's name from `SONICTERM_CONTROL`.
pub(crate) fn start(shared: &Arc<McpShared>) -> io::Result<(String, Option<DiscoveryGuard>)> {
    let name = pipe_name(std::process::id());
    let wide = HSTRING::from(name.as_str());
    let security = PipeSecurity::current_user()?;
    let first = create_instance(&wide, &security, true)?;
    let record = discovery_dir()
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "LOCALAPPDATA is unset"))
        .and_then(|dir| publish_in(&dir, &name));
    let guard = match record {
        Ok(guard) => Some(guard),
        Err(error) => {
            tracing::warn!(target: "mcp", %error, "discovery record not written");
            None
        }
    };
    let shared = Arc::clone(shared);
    std::thread::Builder::new().name("sonicterm-mcp-listen".into()).spawn(move || {
        accept_loop(&shared, &wide, &security, first);
    })?;
    Ok((name, guard))
}

/// Wait for a client on `pending`, hand it to a connection, and open the next instance, until the
/// server stops.
fn accept_loop(
    shared: &Arc<McpShared>,
    name: &HSTRING,
    security: &PipeSecurity,
    first: OwnedHandle,
) {
    let mut pending = Some(first);
    while !shared.is_stopped() {
        let pipe = match pending.take().map_or_else(|| create_instance(name, security, false), Ok) {
            Ok(pipe) => pipe,
            Err(error) => {
                // When: `create_instance` failed, the `error` is logged and the loop retries.
                tracing::warn!(target: "mcp", %error, "pipe instance could not be created");
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
        };
        match wait_for_client(&shared.stopped, &pipe) {
            Ok(true) => match transport(pipe) {
                Ok(transport) => super::spawn_connection(shared, transport),
                Err(error) => tracing::warn!(target: "mcp", %error, "connection setup failed"),
            },
            Ok(false) => {
                // When: `wait_for_client` saw the stop flag (`false`), the listener ends.
                return;
            }
            Err(error) => {
                // A failed wait is logged and the loop retries after a pause.
                tracing::warn!(target: "mcp", %error, "accept failed");
                std::thread::sleep(ACCEPT_POLL);
            }
        }
    }
}

/// Wait for a client to open `pipe`; `Ok(false)` means `stopped` was set first.
// Ordering: stopped Acquire pairs with the Release store in `McpShared::stop`; read alone.
pub(crate) fn wait_for_client(stopped: &AtomicBool, pipe: &OwnedHandle) -> io::Result<bool> {
    let event = new_event()?;
    // Windows writes completion state into `overlapped`, so it is mutable storage, borrowed
    // through a raw pointer for the call and read only through that pointer's place afterwards.
    let mut overlapped = OVERLAPPED { hEvent: event.0, ..Default::default() };
    let overlapped_ptr = std::ptr::from_mut(&mut overlapped);
    let issued =
        // SAFETY: `overlapped` outlives the operation: every path below reaps or cancels it first.
        unsafe { ConnectNamedPipe(pipe.0, Some(overlapped_ptr)) };
    match issued {
        Ok(()) => {
            // When: the connect was `issued` and completed at once, a client is connected.
            return Ok(true);
        }
        Err(error) if win32_code(&error) == Some(ERROR_PIPE_CONNECTED) => {
            // When: `ERROR_PIPE_CONNECTED` says a client arrived before the call, it is ready.
            return Ok(true);
        }
        Err(error) if !is_pending(&error) => {
            // When: the connect is not `is_pending`, nothing waits and the error says why.
            return Err(io_error(&error));
        }
        Err(_) => {
            // When: the `issued` connect is pending, the wait below polls it.
        }
    }
    while !signalled(&overlapped, ACCEPT_POLL) {
        if stopped.load(Ordering::Acquire) {
            // When: `stopped` was set before a client came, the connect is cancelled.
            // A client that won the race with the cancel is dropped with the instance.
            let _ = cancel(pipe.0, &overlapped);
            return Ok(false);
        }
    }
    reap(pipe.0, &overlapped).map(|_| true).map_err(|error| io_error(&error))
}

#[cfg(test)]
#[path = "endpoint_windows_tests.rs"]
mod endpoint_windows_tests;
