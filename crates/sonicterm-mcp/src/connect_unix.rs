//! Connecting on macOS and Linux: a Unix socket whose server must run as the caller's uid.

use std::io;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// The per-user directory SonicTerm binds its sockets in.
pub fn endpoint_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        Some(runtime) => PathBuf::from(runtime).join("sonicterm"),
        None => std::env::temp_dir().join(format!("sonicterm-{}", current_uid())),
    }
}

/// The caller's effective uid.
pub fn current_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, cannot fail and touches no memory we own.
    unsafe { libc::geteuid() }
}

/// Whether process `pid` exists.
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        // When: the `pid` does not fit a `pid_t`, no such process exists.
        return false;
    };
    let result =
        // SAFETY: signal 0 only checks that `pid` exists; nothing is delivered.
        unsafe { libc::kill(pid, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether a server is listening on the socket at `address`. The probe connection closes at
/// once without a byte; the server reads that as a client that left.
pub fn listening(address: &str) -> bool {
    UnixStream::connect(address).is_ok()
}

/// The uid of the process at the other end of `stream`.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "macos")]
    {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        let result =
            // SAFETY: `fd` is a live connected socket owned by `stream`; `uid` and `gid` are locals.
            unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
        if result != 0 {
            // When: `getpeereid` returned a nonzero `result`, the server cannot be identified.
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let result =
            // SAFETY: `fd` is a live connected socket; `cred` and `len` are locals sized for
            // `SO_PEERCRED`, and the kernel writes at most `len` bytes.
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
            // When: `getsockopt` returned a nonzero `result`, the server cannot be identified.
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
}

/// Whether a server whose credential lookup gave `peer` may be talked to by a caller running as
/// `uid`: only the same uid, and a failed lookup is refused.
pub fn trust_server(peer: io::Result<u32>, uid: u32) -> bool {
    peer.is_ok_and(|peer| peer == uid)
}

/// Connect to `address` and check the server runs as the caller before any byte is sent.
pub fn connect(address: &str) -> io::Result<(UnixStream, UnixStream)> {
    let stream = UnixStream::connect(address)?;
    if !trust_server(peer_uid(&stream), current_uid()) {
        // When: `trust_server` refused the peer, nothing is sent to it.
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the endpoint belongs to another user",
        ));
    }
    let writer = stream.try_clone()?;
    Ok((stream, writer))
}

#[cfg(test)]
#[path = "connect_unix_tests.rs"]
mod connect_unix_tests;
