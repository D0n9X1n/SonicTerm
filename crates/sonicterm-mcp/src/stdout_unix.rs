//! stdout as the relay writes it on macOS and Linux.
//!
//! When stdout is a pipe or a socket (Node's `child_process` pipes child stdio through Unix
//! sockets on macOS), the relay writes it in nonblocking mode and retries every [`RETRY`] while
//! it is full. A blocking write reports nothing until the kernel wakes the
//! writer, which can take several of a slow reader's reads; a nonblocking write accepts whatever
//! room the reader has freed, so each read shows up as progress within [`RETRY`]. The pipe's own
//! granularity still applies: Linux frees room a page at a time, macOS by whatever was read.

use std::io::{self, Write};
use std::os::unix::io::RawFd;
use std::time::Duration;

/// How long a full pipe waits before the next attempt.
pub const RETRY: Duration = Duration::from_millis(50);

/// The relay's stdout: the raw descriptor, nonblocking when it is a pipe.
pub struct Stdout {
    /// The descriptor written to; owned by the process, never closed here.
    fd: RawFd,
}

impl Stdout {
    /// stdout, switched to nonblocking writes when it is a pipe or a socket. The switch only
    /// affects this process's end; a terminal or file keeps blocking writes.
    pub fn new() -> Self {
        Self::on(libc::STDOUT_FILENO)
    }

    /// The writer for `fd`, as [`Stdout::new`] sets it up.
    pub fn on(fd: RawFd) -> Self {
        // A full pipe or socket should report no room rather than block; other kinds keep
        // blocking writes.
        if is_stream(fd) {
            set_nonblocking(fd);
        }
        Self { fd }
    }
}

/// Whether `fd` is a pipe, FIFO or socket: a stream whose reader frees room as it reads.
fn is_stream(fd: RawFd) -> bool {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result =
        // SAFETY: `stat` is a writable `libc::stat` that `fstat` fills for the open `fd`.
        unsafe { libc::fstat(fd, stat.as_mut_ptr()) };
    if result != 0 {
        // When: `fstat` returned a nonzero `result`, the kind is unknown and writes stay blocking.
        return false;
    }
    let filled =
        // SAFETY: `fstat` returned 0, so it initialized `stat`.
        unsafe { stat.assume_init() };
    let mode = filled.st_mode;
    let kind = mode & libc::S_IFMT;
    kind == libc::S_IFIFO || kind == libc::S_IFSOCK
}

/// Add `O_NONBLOCK` to `fd`'s flags, keeping the rest; a failure leaves writes blocking.
fn set_nonblocking(fd: RawFd) {
    let flags =
        // SAFETY: `F_GETFL` only reads the open `fd`'s status flags.
        unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        // When: `flags` were read; a failed `F_SETFL` leaves blocking writes, which still work.
        let _ =
            // SAFETY: `F_SETFL` sets status flags on the open `fd`, adding only `O_NONBLOCK`.
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    }
}

impl Write for Stdout {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            let written =
                // SAFETY: `buffer` is a live slice of `buffer.len()` bytes and `fd` is open.
                unsafe { libc::write(self.fd, buffer.as_ptr().cast(), buffer.len()) };
            if written >= 0 {
                // When: `written` is not negative, the pipe took that many bytes.
                return Ok(written as usize);
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::WouldBlock => {
                    // The pipe is full; try again once the reader has had time.
                    std::thread::sleep(RETRY);
                }
                io::ErrorKind::Interrupted => {
                    // When: a signal `Interrupted` the call, it is retried at once.
                    continue;
                }
                _ => {
                    // When: any other `error`, stdout is gone or unusable.
                    return Err(error);
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "stdout_unix_tests.rs"]
mod stdout_unix_tests;
