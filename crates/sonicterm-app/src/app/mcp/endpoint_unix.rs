//! The macOS and Linux endpoint: a Unix socket in a per-user `0700` directory.
//!
//! The directory is `$XDG_RUNTIME_DIR/sonicterm/`, else `$TMPDIR/sonicterm-<uid>/`. Its path is
//! canonicalized and every ancestor must be owned by the user or root and not group- or
//! world-writable unless sticky, so no other user can replace an entry on the path. The
//! directory itself is opened with `O_NOFOLLOW` and checked through its handle: a real directory
//! owned by the user, mode exactly `0700`. The socket is `<pid>.sock` at `0600`; every accepted
//! peer's uid must equal ours.

use std::fs::OpenOptions;
use std::io::{self, ErrorKind};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::{McpShared, Transport, READ_POLL, WRITE_DEADLINE};

/// How often the listener checks the stop flag between connection attempts.
const ACCEPT_POLL: Duration = Duration::from_millis(100);

/// The current process's effective uid.
pub(crate) fn current_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, cannot fail and touches no memory we own.
    unsafe { libc::geteuid() }
}

/// The per-user directory the socket lives in, before validation.
pub(crate) fn endpoint_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        Some(runtime) => PathBuf::from(runtime).join("sonicterm"),
        None => std::env::temp_dir().join(format!("sonicterm-{}", current_uid())),
    }
}

/// Why a path cannot hold the socket.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DirRefusal {
    /// An ancestor is owned by another user.
    AncestorOwner(PathBuf),
    /// An ancestor is group- or world-writable without the sticky bit.
    AncestorWritable(PathBuf),
    /// The directory is a symlink or not a directory.
    NotDirectory,
    /// The directory is owned by another user.
    Owner,
    /// The directory's mode is not exactly `0700`.
    Mode(u32),
}

/// Check every ancestor of `dir` from its parent up to `/`.
pub(crate) fn check_ancestors(dir: &Path, uid: u32) -> Result<(), DirRefusal> {
    for ancestor in dir.ancestors().skip(1) {
        let Ok(meta) = std::fs::metadata(ancestor) else {
            // When: `metadata` cannot read an ancestor, the check cannot vouch for it.
            return Err(DirRefusal::AncestorOwner(ancestor.to_path_buf()));
        };
        if meta.uid() != uid && meta.uid() != 0 {
            // When: the ancestor's `uid` is neither ours nor root's, another user could replace it.
            return Err(DirRefusal::AncestorOwner(ancestor.to_path_buf()));
        }
        let mode = meta.mode();
        let sticky = mode & 0o1000 != 0;
        if mode & 0o022 != 0 && !sticky {
            // When: the ancestor is group- or world-writable and not `sticky`, entries can be replaced.
            return Err(DirRefusal::AncestorWritable(ancestor.to_path_buf()));
        }
    }
    Ok(())
}

/// Check the directory through a handle opened without following a symlink.
pub(crate) fn check_dir(dir: &Path, uid: u32) -> Result<(), DirRefusal> {
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(dir)
        .map_err(|_| DirRefusal::NotDirectory)?;
    let meta = handle.metadata().map_err(|_| DirRefusal::NotDirectory)?;
    if !meta.is_dir() {
        // When: the handle `is_dir` reports no directory, nothing is bound inside it.
        return Err(DirRefusal::NotDirectory);
    }
    if meta.uid() != uid {
        // When: the directory's `uid` is not ours, another user owns it.
        return Err(DirRefusal::Owner);
    }
    let mode = meta.mode() & 0o7777;
    if mode != 0o700 {
        // When: `mode` is not exactly 0700, the directory is refused, not repaired.
        return Err(DirRefusal::Mode(mode));
    }
    Ok(())
}

/// Create the directory with mode `0700` if missing, then validate it and its ancestors. Only the
/// parent is canonicalized; the directory itself is checked without following a symlink.
pub(crate) fn prepare_dir(dir: &Path, uid: u32) -> io::Result<PathBuf> {
    // An existing directory is validated below like a new one.
    std::fs::DirBuilder::new().mode(0o700).create(dir).or_else(|error| match error.kind() {
        ErrorKind::AlreadyExists => Ok(()),
        _ => Err(error),
    })?;
    let refused =
        |refusal: DirRefusal| io::Error::new(ErrorKind::PermissionDenied, format!("{refusal:?}"));
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        // When: `dir` has no `parent` or final `name`, it cannot be a private directory.
        return Err(refused(DirRefusal::NotDirectory));
    };
    // The final component is joined to the canonical parent unresolved, so `check_dir`'s
    // O_NOFOLLOW refuses it when it is a symlink.
    let resolved = parent.canonicalize()?.join(name);
    check_ancestors(&resolved, uid).and_then(|()| check_dir(&resolved, uid)).map_err(refused)?;
    Ok(resolved)
}

/// The uid of the process at the other end of `stream`.
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "macos")]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        let result =
            // SAFETY: `fd` is a live connected socket owned by `stream`, and both out-pointers point
            // to locals that outlive the call.
            unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
        if result != 0 {
            // When: `getpeereid` returned a nonzero `result`, the peer cannot be identified.
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let result =
            // SAFETY: `fd` is a live connected socket owned by `stream`; `cred` and `len` are locals
            // sized for `SO_PEERCRED`, and the kernel writes at most `len` bytes.
            unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if result != 0 {
            // When: `getsockopt` returned a nonzero `result`, the peer cannot be identified.
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
}

/// Remove sockets in `dir` whose owning process is gone, so a crashed instance does not
/// confuse discovery.
pub(crate) fn remove_stale_sockets(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // When: `read_dir` fails, there is nothing to clean.
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let pid = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".sock"))
            .and_then(|pid| pid.parse::<i32>().ok());
        let Some(pid) = pid else {
            // When: the name carries no `pid`, the file is not ours and stays.
            continue;
        };
        let alive =
            // SAFETY: signal 0 only checks that `pid` exists; no signal is delivered.
            unsafe { libc::kill(pid, 0) } == 0
            || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            // When: the owning process is not `alive`, the socket is stale and removed.
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Removes the socket on drop, only if it is still the one this process bound.
pub(crate) struct SocketGuard {
    /// The socket's path.
    pub(crate) path: PathBuf,
    /// The bound socket's device and inode.
    pub(crate) identity: (u64, u64),
}

// Lifecycle: SocketGuard drop calls remove_file with the server unless the path names another file;
// a killed process skips it and the next start cleans up.
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let same = std::fs::symlink_metadata(&self.path)
            .is_ok_and(|meta| (meta.dev(), meta.ino()) == self.identity);
        if same {
            // When: the file is still the `same` socket this process bound, it is removed.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
thread_local! {
    /// A test's endpoint directory, so a test that starts the real server binds in scratch space.
    pub(crate) static TEST_ENDPOINT_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Bind the socket in the per-user directory and start the listener thread.
pub(crate) fn start(shared: &Arc<McpShared>) -> io::Result<Endpoint> {
    #[cfg(test)]
    if let Some(dir) = TEST_ENDPOINT_DIR.with_borrow(Clone::clone) {
        // When: a test set `TEST_ENDPOINT_DIR`, the server binds there instead of the user's directory.
        return start_in(shared, &dir);
    }
    start_in(shared, &endpoint_dir())
}

/// A bound endpoint: its path and the guard that removes it.
pub(crate) struct Endpoint {
    /// The socket path published as `SONICTERM_CONTROL`.
    pub(crate) path: String,
    /// Removes the socket when the server is dropped, before the process can exit.
    pub(crate) guard: SocketGuard,
}

/// Bind the socket in `dir` and start the listener thread.
pub(crate) fn start_in(shared: &Arc<McpShared>, dir: &Path) -> io::Result<Endpoint> {
    let uid = current_uid();
    let dir = prepare_dir(dir, uid)?;
    remove_stale_sockets(&dir);
    let path = dir.join(format!("{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let meta = std::fs::symlink_metadata(&path)?;
    let guard = SocketGuard { path: path.clone(), identity: (meta.dev(), meta.ino()) };
    listener.set_nonblocking(true)?;
    let shared = Arc::clone(shared);
    std::thread::Builder::new().name("sonicterm-mcp-listen".into()).spawn(move || {
        accept_loop(&shared, &listener, uid);
    })?;
    Ok(Endpoint { path: path.to_string_lossy().into_owned(), guard })
}

/// Accept connections until the server stops, admitting only peers with our uid.
fn accept_loop(shared: &Arc<McpShared>, listener: &UnixListener, uid: u32) {
    while !shared.is_stopped() {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                // When: `accept` would block, no client waits; the stop flag is rechecked.
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
            Err(error) => {
                // When: `accept` failed otherwise, the `error` is logged and the loop retries.
                tracing::warn!(target: "mcp", %error, "accept failed");
                std::thread::sleep(ACCEPT_POLL);
                continue;
            }
        };
        if !admit_peer(peer_uid(&stream), uid) {
            // When: `admit_peer` refuses the peer, the connection is dropped before any byte is read.
            tracing::warn!(target: "mcp", "connection refused: another user");
            continue;
        }
        if let Some(transport) = transport(stream) {
            super::spawn_connection(shared, transport);
        }
    }
}

/// Whether a peer whose credential lookup gave `peer` may connect to a server running as `uid`:
/// only the same uid is admitted, and a failed lookup is refused.
pub(crate) fn admit_peer(peer: io::Result<u32>, uid: u32) -> bool {
    peer.is_ok_and(|peer| peer == uid)
}

/// Split an accepted stream into the reader and writer a connection uses.
fn transport(stream: UnixStream) -> Option<Transport> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(READ_POLL)).ok()?;
    stream.set_write_timeout(Some(WRITE_DEADLINE)).ok()?;
    let writer = stream.try_clone().ok()?;
    Some(Transport { reader: Box::new(stream), writer: Box::new(writer) })
}

#[cfg(test)]
#[path = "endpoint_unix_tests.rs"]
mod endpoint_unix_tests;
