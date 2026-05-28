//! Regression test for CLAUDE.md §4 land-mine:
//! "`PtyHandle::Drop` kills the child explicitly."
//!
//! Just dropping a `Box<dyn portable_pty::Child>` is not enough to terminate
//! the spawned shell — the kernel keeps it alive (now an orphan reparented
//! to init) until it tries to write to the closed pty and gets SIGPIPE,
//! which can take indefinitely long for an interactive shell that's idle.
//! `PtyHandle::Drop` therefore calls `.kill()` explicitly. This test
//! pins that behaviour so a future refactor cannot silently regress it.
//!
//! Unix-only; Windows ConPTY has different lifecycle semantics and warrants
//! its own dedicated test if/when needed.

#![cfg(unix)]

use std::{thread, time::Duration};

use sonic_io::pty::PtyHandle;

/// Spawn a long-lived shell over a PTY, capture its pid, drop the handle,
/// and assert the kernel no longer knows about the pid.
#[test]
fn pty_drop_kills_child() {
    // `/bin/sh` with no script and no tty input will sit blocked on stdin
    // (which is our pty slave) effectively forever — perfect long-lived
    // child for the test.
    let handle = PtyHandle::spawn("/bin/sh", 80, 24).expect("spawn shell over pty");

    let pid = handle.pid().expect("portable-pty must expose a child pid on unix") as i32;
    assert!(pid > 1, "implausible pid {pid}");

    // Cleanup guard: if any assertion below panics, ensure the spawned child
    // is killed during unwind so the test never leaks a real process. When
    // the bug under test is absent the pid is already dead by the time the
    // guard runs and SIGKILL just returns ESRCH — harmless.
    struct PidGuard(i32);
    impl Drop for PidGuard {
        fn drop(&mut self) {
            if self.0 > 1 {
                unsafe {
                    libc::kill(self.0, libc::SIGKILL);
                }
            }
        }
    }
    let _guard = PidGuard(pid);

    // Sanity: the child is alive right now.
    // `kill(pid, 0)` is the canonical "does this pid exist & am I allowed to
    // signal it" probe — no signal is actually delivered.
    let alive = unsafe { libc::kill(pid, 0) };
    assert_eq!(alive, 0, "child pid {pid} should be alive immediately after spawn");

    drop(handle);

    // Give the OS a moment to deliver SIGKILL and reap the zombie. portable-pty
    // does a blocking `wait()` inside `Child::kill` on most platforms, but be
    // generous — CI runners can be slow under load.
    let mut gone = false;
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(25));
        let rc = unsafe { libc::kill(pid, 0) };
        if rc == -1 {
            let errno = errno();
            // ESRCH = no such process. EPERM would mean it's alive but owned
            // by someone else, which can't happen for a child we just spawned.
            if errno == libc::ESRCH {
                gone = true;
                break;
            }
        }
    }

    assert!(
        gone,
        "child pid {pid} still alive 1s after PtyHandle drop — \
         PtyHandle::Drop is no longer killing the child (CLAUDE.md §4 land-mine regression)",
    );
}

/// Portable errno read. `libc::__errno_location` on Linux, `libc::__error`
/// on macOS / BSDs. We avoid pulling in the `errno` crate just for this.
fn errno() -> i32 {
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location()
    }
    #[cfg(not(target_os = "linux"))]
    unsafe {
        *libc::__error()
    }
}
