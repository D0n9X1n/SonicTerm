//! sonicterm-mux daemon entrypoint.
//!
//! Subcommands:
//!
//! ```text
//! sonic-mux daemon [--socket <path>]          run the server
//! sonic-mux list [--socket <path>]            list live sessions
//! sonic-mux kill <pane_id> [--socket <path>]  terminate a pane
//! ```

#[cfg(unix)]
use std::io;
#[cfg(windows)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::io::{AsHandle, AsRawHandle};
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::{env, process::ExitCode, sync::Arc, thread};

use anyhow::{anyhow, bail, Context, Result};
#[cfg(unix)]
use interprocess::{local_socket::GenericFilePath, os::unix::local_socket::ListenerOptionsExt};
#[cfg(windows)]
use interprocess::{
    local_socket::GenericNamespaced,
    os::windows::{local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor},
};
use interprocess::{
    local_socket::{prelude::*, Listener, ListenerOptions, Stream},
    TryClone,
};
use sonicterm_mux::{
    frame::{read_frame, write_frame},
    handle_connection_with_shutdown,
    proto::{ClientMsg, ServerMsg},
    ServerState,
};
#[cfg(windows)]
use widestring::U16CString;
#[cfg(windows)]
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{GetLengthSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER},
    System::{
        Pipes::DisconnectNamedPipe,
        RemoteDesktop::ProcessIdToSessionId,
        Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcessToken},
        IO::CancelIoEx,
    },
};

#[cfg(unix)]
const UNIX_SOCKET_MODE: libc::mode_t = 0o600;
#[cfg(unix)]
const UNIX_SOCKET_PERMISSION_BITS: u32 = 0o600;
#[cfg(unix)]
const UNIX_RUNTIME_DIR_MODE: u32 = 0o700;

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct AuditMask {
    success: libc::c_uint,
    failure: libc::c_uint,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct AuditTerminalId {
    port: libc::dev_t,
    kind: libc::c_uint,
    address: [libc::c_uint; 4],
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct AuditInfoAddress {
    user_id: libc::uid_t,
    mask: AuditMask,
    terminal_id: AuditTerminalId,
    session_id: libc::pid_t,
    flags: u64,
}

#[cfg(target_os = "macos")]
// SAFETY: `getaudit_addr` writes only the caller-sized C `auditinfo_addr` record.
unsafe extern "C" {
    fn getaudit_addr(info: *mut AuditInfoAddress, length: libc::c_int) -> libc::c_int;
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sonicterm-mux: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let sub = args.next().ok_or_else(|| anyhow!(usage()))?;
    let rest: Vec<String> = args.collect();
    let explicit_socket = extract_explicit_socket(&rest)?;
    let socket = explicit_socket.clone().map_or_else(default_socket, Ok)?;
    match sub.as_str() {
        "daemon" => cmd_daemon(&socket, explicit_socket.is_none()),
        "list" => cmd_list(&socket),
        "kill" => {
            let pane_id_str = rest
                .iter()
                .find(|a| !a.starts_with("--"))
                .ok_or_else(|| anyhow!("kill: pane id required"))?;
            let pane_id: u64 = pane_id_str.parse().context("pane id must be u64")?;
            cmd_kill(&socket, pane_id)
        }
        "--help" | "-h" | "help" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(anyhow!("unknown subcommand: {other}\n\n{}", usage())),
    }
}

fn usage() -> &'static str {
    "sonic-mux <daemon|list|kill <pane_id>> [--socket <path>]"
}

fn extract_explicit_socket(args: &[String]) -> Result<Option<String>> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == "--socket" {
            return iter
                .next()
                .cloned()
                .map(Some)
                .ok_or_else(|| anyhow!("--socket requires a value"));
        }
    }
    Ok(None)
}

#[cfg(target_os = "macos")]
fn unix_login_session_id() -> Result<u32> {
    let mut info = AuditInfoAddress::default();
    let length = libc::c_int::try_from(std::mem::size_of::<AuditInfoAddress>())
        .context("macOS audit session record is too large")?;
    let result =
        // SAFETY: `info` is writable for exactly `length` bytes and remains live for the call.
        unsafe { getaudit_addr(&mut info, length) };
    if result == -1 {
        // When: `getaudit_addr` failed, no stable login-session namespace can be claimed.
        return Err(io::Error::last_os_error()).context("resolve macOS audit login session");
    }
    u32::try_from(info.session_id).context("macOS returned an invalid audit login session")
}

#[cfg(any(target_os = "linux", test))]
fn linux_login_session_id_from(raw: std::io::Result<String>) -> Result<u32> {
    let raw = match raw {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).context("read Linux audit login session"),
    };
    let session = raw.trim().parse::<u32>().context("parse Linux audit login session")?;
    // A kernel with no audit login assignment reports UINT32_MAX. The private
    // directory remains user-scoped, and zero is stable across that user's terminals.
    Ok(if session == u32::MAX { 0 } else { session })
}

#[cfg(target_os = "linux")]
fn unix_login_session_id() -> Result<u32> {
    linux_login_session_id_from(std::fs::read_to_string("/proc/self/sessionid"))
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn unix_login_session_id() -> Result<u32> {
    Ok(0)
}

#[cfg(unix)]
fn resolve_unix_default_socket_with(
    xdg_runtime_dir: Option<&Path>,
    temp_dir: &Path,
    uid: u32,
    login_session: impl FnOnce() -> Result<u32>,
) -> Result<PathBuf> {
    if let Some(runtime_dir) = xdg_runtime_dir.filter(|path| private_runtime_directory(path, uid)) {
        return Ok(runtime_dir.join("sonicterm-mux.sock"));
    }
    // When: no trusted XDG directory exists, the fallback path needs stable login identity.
    resolve_unix_default_socket(xdg_runtime_dir, temp_dir, uid, login_session()?)
}

#[cfg(unix)]
fn default_socket() -> Result<String> {
    let uid =
        // SAFETY: `geteuid` has no pointer arguments or caller-managed invariants.
        unsafe { libc::geteuid() };
    let runtime = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let path = resolve_unix_default_socket_with(
        runtime.as_deref(),
        &env::temp_dir(),
        uid,
        unix_login_session_id,
    )?;
    path.into_os_string()
        .into_string()
        .map_err(|_| anyhow!("default mux socket path is not valid Unicode"))
}

#[cfg(windows)]
fn default_socket() -> Result<String> {
    Ok(windows_endpoint_name(&current_windows_user_sid()?, current_windows_session_id()?))
}

#[cfg(any(windows, test))]
fn windows_endpoint_name(user_sid: &str, session_id: u32) -> String {
    format!("sonicterm-mux-{}-{session_id}", user_sid.replace('-', "_"))
}

#[cfg(any(windows, test))]
fn sid_string(bytes: &[u8]) -> Result<String> {
    if bytes.len() < 8 {
        bail!("Windows returned a truncated user SID");
    }
    let revision = bytes[0];
    let subauthority_count = bytes[1] as usize;
    let expected = 8usize
        .checked_add(
            subauthority_count
                .checked_mul(4)
                .ok_or_else(|| anyhow!("Windows SID subauthority length overflow"))?,
        )
        .ok_or_else(|| anyhow!("Windows SID length overflow"))?;
    if expected != bytes.len() {
        bail!("Windows returned a malformed user SID length");
    }
    let authority = bytes[2..8].iter().fold(0u64, |value, byte| (value << 8) | u64::from(*byte));
    let mut sid = format!("S-{revision}-{authority}");
    let (subauthorities, remainder) = bytes[8..].as_chunks::<4>();
    debug_assert!(remainder.is_empty(), "validated SID length is four-byte aligned");
    for chunk in subauthorities {
        let value = u32::from_le_bytes(*chunk);
        sid.push('-');
        sid.push_str(&value.to_string());
    }
    Ok(sid)
}

#[cfg(windows)]
struct TokenHandle(HANDLE);

#[cfg(windows)]
// Lifecycle: `TokenHandle` Drop closes the process token handle exactly once.
impl Drop for TokenHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` remains the owned handle returned by `OpenProcessToken`.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn current_windows_user_sid() -> Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle identifies this process and `token` receives one owned query handle.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)? };
    let token = TokenHandle(token);

    let mut required = 0u32;
    // SAFETY: the zero-length probe writes only the required byte count while `token` remains live.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut required) };
    if required < std::mem::size_of::<TOKEN_USER>() as u32 {
        bail!("Windows returned an invalid token-user length");
    }
    let words = (required as usize).div_ceil(std::mem::size_of::<usize>());
    let mut buffer = vec![0usize; words];
    // SAFETY: the aligned buffer is writable for `required` bytes and remains live while its SID is read.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            required,
            &mut required,
        )?;
    }
    // SAFETY: successful `TokenUser` output starts with an aligned initialized `TOKEN_USER` value.
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let sid = token_user.User.Sid;
    if sid.is_invalid() {
        bail!("Windows returned a null user SID");
    }
    let length =
        // SAFETY: `sid` points into the live successful `TokenUser` response buffer.
        unsafe { GetLengthSid(sid) } as usize;
    if length == 0 || length > required as usize {
        bail!("Windows returned an invalid user SID length");
    }
    // SAFETY: `GetLengthSid` reports the readable byte extent of `sid` inside the live response buffer.
    let bytes = unsafe { std::slice::from_raw_parts(sid.0.cast::<u8>(), length) };
    sid_string(bytes)
}

#[cfg(windows)]
fn current_windows_session_id() -> Result<u32> {
    let mut session_id = 0;
    // SAFETY: the current process id is valid and `session_id` is writable for one `u32`.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session_id)? };
    Ok(session_id)
}

#[cfg(unix)]
fn fallback_unix_socket_path(temp_dir: &Path, uid: u32, session_id: u32) -> PathBuf {
    temp_dir.join(format!("sonicterm-mux-{uid}-{session_id}")).join("sonicterm-mux.sock")
}

#[cfg(unix)]
fn private_runtime_directory(path: &Path, uid: u32) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    metadata.file_type().is_dir()
        && metadata.uid() == uid
        && metadata.mode() & 0o777 == UNIX_RUNTIME_DIR_MODE
}

#[cfg(unix)]
fn resolve_unix_default_socket(
    xdg_runtime_dir: Option<&Path>,
    temp_dir: &Path,
    uid: u32,
    session_id: u32,
) -> Result<PathBuf> {
    if let Some(runtime_dir) = xdg_runtime_dir.filter(|path| private_runtime_directory(path, uid)) {
        return Ok(runtime_dir.join("sonicterm-mux.sock"));
    }

    let socket = fallback_unix_socket_path(temp_dir, uid, session_id);
    let runtime_dir = socket.parent().expect("fallback socket always has a parent");
    match std::fs::symlink_metadata(runtime_dir) {
        Ok(_) if !private_runtime_directory(runtime_dir, uid) => {
            bail!("refusing unsafe mux runtime directory {}", runtime_dir.display());
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("inspect mux runtime directory {}", runtime_dir.display())
            });
        }
    }
    Ok(socket)
}

#[cfg(unix)]
fn ensure_unix_runtime_directory(path: &Path, uid: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("mux endpoint {} has no parent directory", path.display()))?;
    match std::fs::symlink_metadata(parent) {
        Ok(_) if !private_runtime_directory(parent, uid) => {
            bail!("refusing unsafe mux runtime directory {}", parent.display());
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect mux runtime directory {}", parent.display()));
        }
    }
    std::fs::DirBuilder::new()
        .mode(UNIX_RUNTIME_DIR_MODE)
        .create(parent)
        .with_context(|| format!("create mux runtime directory {}", parent.display()))?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(UNIX_RUNTIME_DIR_MODE))
        .with_context(|| format!("restrict mux runtime directory {}", parent.display()))?;
    if !private_runtime_directory(parent, uid) {
        bail!("mux runtime directory {} is not private", parent.display());
    }
    Ok(())
}

#[cfg(unix)]
fn prepare_unix_endpoint(path: &Path, uid: u32) -> Result<()> {
    let before = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if !before.file_type().is_socket() {
        bail!("refusing non-socket mux endpoint {}", path.display());
    }
    if before.uid() != uid {
        bail!("refusing mux endpoint not owned by the current user: {}", path.display());
    }

    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => bail!("refusing to replace live mux endpoint {}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot prove mux endpoint {} is stale", path.display()));
        }
    }

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("mux endpoint {} has no parent directory", path.display()))?;
    if !private_runtime_directory(parent, uid) {
        bail!("refusing stale mux endpoint outside an owned 0700 directory: {}", path.display());
    }
    let after = std::fs::symlink_metadata(path)
        .with_context(|| format!("recheck stale mux endpoint {}", path.display()))?;
    if !after.file_type().is_socket()
        || after.uid() != uid
        || after.dev() != before.dev()
        || after.ino() != before.ino()
    {
        bail!("mux endpoint {} changed during stale validation", path.display());
    }
    std::fs::remove_file(path)
        .with_context(|| format!("remove stale mux endpoint {}", path.display()))
}

#[cfg(unix)]
fn bind_unix_listener(path: &Path) -> Result<Listener> {
    let uid =
        // SAFETY: `geteuid` has no pointer arguments or caller-managed invariants.
        unsafe { libc::geteuid() };
    prepare_unix_endpoint(path, uid)?;
    let name = path.to_fs_name::<GenericFilePath>()?;
    match ListenerOptions::new().name(name).mode(UNIX_SOCKET_MODE).create_sync() {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            let parent = path.parent().ok_or_else(|| {
                anyhow!("mux endpoint {} has no parent directory", path.display())
            })?;
            if !private_runtime_directory(parent, uid) {
                bail!(
                    "platform requires mux endpoint {} to be inside an owned 0700 directory",
                    path.display()
                );
            }
            let name = path.to_fs_name::<GenericFilePath>()?;
            let listener = ListenerOptions::new()
                .name(name)
                .create_sync()
                .with_context(|| format!("bind mux endpoint {}", path.display()))?;
            let permissions = std::fs::Permissions::from_mode(UNIX_SOCKET_PERMISSION_BITS);
            if let Err(error) = std::fs::set_permissions(path, permissions) {
                drop(listener);
                return Err(error)
                    .with_context(|| format!("restrict mux endpoint {}", path.display()));
            }
            Ok(listener)
        }
        Err(error) => Err(error).with_context(|| format!("bind mux endpoint {}", path.display())),
    }
}

#[cfg(unix)]
fn bind_listener(socket: &str) -> Result<Listener> {
    bind_unix_listener(Path::new(socket))
}

#[cfg(any(windows, test))]
fn current_user_pipe_sddl(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;{user_sid})")
}

#[cfg(windows)]
fn current_user_security_descriptor(user_sid: &str) -> Result<SecurityDescriptor> {
    let sddl = U16CString::from_str(current_user_pipe_sddl(user_sid))
        .map_err(|error| anyhow!("build current-user pipe DACL: {error}"))?;
    SecurityDescriptor::deserialize(sddl.as_ucstr()).context("parse current-user pipe DACL")
}

#[cfg(windows)]
fn bind_listener(socket: &str) -> Result<Listener> {
    let name = make_socket_name(socket)?;
    let user_sid = current_windows_user_sid()?;
    let descriptor = current_user_security_descriptor(&user_sid)?;
    Ok(ListenerOptions::new().name(name).security_descriptor(descriptor).create_sync()?)
}

fn cmd_daemon(socket: &str, prepare_default_parent: bool) -> Result<()> {
    #[cfg(unix)]
    if prepare_default_parent {
        // When: the daemon selected its generated fallback, create the private parent before bind.
        let uid =
            // SAFETY: `geteuid` has no pointer arguments or caller-managed invariants.
            unsafe { libc::geteuid() };
        ensure_unix_runtime_directory(Path::new(socket), uid)?;
    }
    #[cfg(windows)]
    let _ = prepare_default_parent;
    let listener = bind_listener(socket)?;
    let state = ServerState::new();
    tracing::info!(socket, "sonicterm-mux daemon listening");
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let state = state.clone();
                thread::spawn(move || {
                    if let Err(e) = serve_stream(state, stream) {
                        tracing::warn!(error = %e, "client connection ended with error");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
            }
        }
    }
    Ok(())
}

fn serve_stream(state: Arc<ServerState>, stream: Stream) -> Result<()> {
    // interprocess 2.x streams are full-duplex; split with try_clone so
    // reader and writer can live on separate threads.
    #[cfg(unix)]
    {
        let writer = stream.try_clone()?;
        let shutdown = stream.try_clone()?;
        handle_connection_with_shutdown(state, stream, writer, move || {
            let Stream::UdSocket(stream) = shutdown;
            let _ = stream.inner().shutdown(std::net::Shutdown::Both);
        })
    }
    #[cfg(windows)]
    {
        let writer = Arc::new(stream.try_clone()?);
        let shutdown = writer.clone();
        handle_connection_with_shutdown(state, stream, SharedWriter(writer), move || {
            let Stream::NamedPipe(named_pipe) = shutdown.as_ref();
            let writer_handle = HANDLE(named_pipe.as_handle().as_raw_handle());
            // SAFETY: `writer_handle` comes from this live named pipe; cancellation and disconnect take it by value.
            unsafe {
                let _ = CancelIoEx(writer_handle, None);
                let _ = DisconnectNamedPipe(writer_handle);
            }
        })
    }
}

#[cfg(windows)]
struct SharedWriter(Arc<Stream>);

#[cfg(windows)]
impl Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        (&*self.0).write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.0).flush()
    }
}

fn cmd_list(socket: &str) -> Result<()> {
    let (mut r, mut w) = connect(socket)?;
    write_frame(&mut w, &ClientMsg::ListSessions)?;
    let resp: ServerMsg = read_frame(&mut r)?;
    match resp {
        ServerMsg::Sessions(list) => {
            if list.is_empty() {
                println!("(no sessions)");
            } else {
                for s in list {
                    println!("session {} — {} pane(s)", s.id, s.pane_count);
                }
            }
            Ok(())
        }
        other => Err(anyhow!("unexpected reply: {other:?}")),
    }
}

fn cmd_kill(socket: &str, pane_id: u64) -> Result<()> {
    let (mut _r, mut w) = connect(socket)?;
    write_frame(&mut w, &ClientMsg::Kill { pane_id })?;
    println!("killed pane {pane_id}");
    Ok(())
}

fn connect(socket: &str) -> Result<(Stream, Stream)> {
    let name = make_socket_name(socket)?;
    let stream = Stream::connect(name)?;
    let write_half = stream.try_clone()?;
    Ok((stream, write_half))
}

#[cfg(unix)]
fn make_socket_name(path: &str) -> Result<interprocess::local_socket::Name<'_>> {
    Ok(path.to_fs_name::<GenericFilePath>()?)
}

#[cfg(windows)]
fn make_socket_name(path: &str) -> Result<interprocess::local_socket::Name<'_>> {
    // On Windows, treat the path as a named-pipe-style identifier under the
    // namespaced root. Strip leading separators to keep it portable.
    let trimmed = path.trim_start_matches(['/', '\\']);
    Ok(trimmed.to_ns_name::<GenericNamespaced>()?)
}

#[allow(dead_code)]
fn _unused_io() {}

#[cfg(test)]
#[path = "main_tests.rs"]
mod main_tests;
