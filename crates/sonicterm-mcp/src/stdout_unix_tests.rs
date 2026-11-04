//! Pins the nonblocking stdout: a full pipe is retried rather than blocking, a write after a small
//! read takes the room that read freed, and a non-pipe keeps blocking writes.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::time::Duration;

use super::*;

/// A fresh pipe as (read end, write end).
fn pipe() -> (File, File) {
    let mut ends = [0i32; 2];
    let made =
        // SAFETY: `ends` is a writable two-element array, as `pipe` requires.
        unsafe { libc::pipe(ends.as_mut_ptr()) };
    assert_eq!(made, 0);
    let read_end =
        // SAFETY: `pipe` returned `ends[0]`, and this `File` takes sole ownership of it.
        unsafe { File::from_raw_fd(ends[0]) };
    let write_end =
        // SAFETY: `pipe` returned `ends[1]`, and this `File` takes sole ownership of it.
        unsafe { File::from_raw_fd(ends[1]) };
    (read_end, write_end)
}

/// A pipe and a socket are detected and switched to nonblocking; a regular file is neither.
#[test]
fn only_a_pipe_becomes_nonblocking() {
    let (_read_end, write_end) = pipe();
    assert!(is_stream(write_end.as_raw_fd()));
    let _stdout = Stdout::on(write_end.as_raw_fd());
    let flags =
        // SAFETY: `F_GETFL` only reads the open descriptor's flags.
        unsafe { libc::fcntl(write_end.as_raw_fd(), libc::F_GETFL) };
    assert_ne!(flags & libc::O_NONBLOCK, 0);
    let (_peer, socket) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(is_stream(socket.as_raw_fd()));
    let file = tempfile::tempfile().unwrap();
    assert!(!is_stream(file.as_raw_fd()));
}

/// The smallest read that frees room in a full pipe: any read on macOS, a whole page on Linux.
#[cfg(target_os = "linux")]
const FREEING_READ: usize = 4096;
/// The smallest read that frees room in a full pipe: any read on macOS, a whole page on Linux.
#[cfg(not(target_os = "linux"))]
const FREEING_READ: usize = 256;

/// Fill the nonblocking pipe behind `write_end` until it refuses a write.
fn fill(write_end: &File) {
    let mut filled = 0;
    while filled < 1 << 20 {
        let probe =
            // SAFETY: the buffer is a live 4096-byte slice and the descriptor is open.
            unsafe { libc::write(write_end.as_raw_fd(), [0u8; 4096].as_ptr().cast(), 4096) };
        if probe <= 0 {
            break;
        }
        filled += probe as usize;
    }
}

/// With the pipe full, a write waits in short retries and takes the room each read frees, at the
/// platform's granularity. The write runs on its own thread with a bounded wait, and the reader
/// closes its end when done, so a write that never progresses fails the test instead of hanging.
#[test]
fn a_full_pipe_takes_what_each_small_read_frees() {
    let (mut read_end, write_end) = pipe();
    // Nonblocking first, so filling stops when the pipe is full instead of waiting.
    let mut stdout = Stdout::on(write_end.as_raw_fd());
    fill(&write_end);
    let reader = std::thread::spawn(move || {
        let mut chunk = vec![0u8; FREEING_READ];
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(200));
            read_end.read_exact(&mut chunk).unwrap();
        }
        // Dropping the read end here ends a writer that would otherwise wait for good.
    });
    let (took_some, outcome) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let _ = took_some.send(stdout.write(&[1u8; 64 * 1024]));
        drop(write_end);
    });
    let took = outcome.recv_timeout(Duration::from_secs(5)).expect("the write made progress");
    assert!(took.unwrap() >= 1);
    reader.join().unwrap();
    writer.join().unwrap();
}
