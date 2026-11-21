//! Launch a helper program the app does not wait for (a URL or file opener, a notification
//! script) without leaving it a zombie on Unix. Every such child goes to one reaper thread, which
//! owns them all and collects each one's exit status once it exits. The reaper waits only on its
//! own children by pid, never on any child, so the PTY teardown's own waits are untouched.

use std::io;
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Spawn `command` and return once it has started; its exit is collected off the caller's thread.
pub fn spawn_detached(command: &mut Command) -> io::Result<()> {
    let child = command.spawn()?;
    adopt(child);
    Ok(())
}

/// The first and the longest wait between checks of children that are still running. A child that
/// has not exited is checked again after a wait that doubles up to the longest, so a helper that
/// keeps running (an editor `xdg-open` handed the file to) costs one check a second, and an idle
/// reaper, holding none, blocks without waking.
const FIRST_CHECK: Duration = Duration::from_millis(50);
const LONGEST_CHECK: Duration = Duration::from_secs(1);

/// Hand `child` to the reaper, starting it on first use. When no reaper can start, the child is
/// checked here for up to [`LONGEST_CHECK`] and then left: never killed, since it may be a program
/// the user is using, and at worst a zombie until the app exits.
fn adopt(child: Child) {
    static REAPER: OnceLock<Option<Mutex<Sender<Child>>>> = OnceLock::new();
    let reaper = REAPER.get_or_init(|| {
        let (handoff, children) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("sonicterm-helper-reaper".into())
            .spawn(move || run_reaper(children));
        match spawned {
            Ok(_) => Some(Mutex::new(handoff)),
            Err(error) => {
                tracing::warn!(target: "sonicterm_cfg::detached_spawn", %error, "helper reaper did not start");
                None
            }
        }
    });
    let Some(handoff) = reaper else {
        // When: no reaper started, this child is checked here instead.
        collect_briefly(child);
        return;
    };
    // A poisoned lock still guards a working sender: only a panic elsewhere poisoned it.
    let sender = handoff.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Err(returned) = sender.send(child) {
        // The reaper exited, so the returned child is checked here instead.
        drop(sender);
        collect_briefly(returned.0);
    }
}

/// Owns every adopted child: blocks for the next one while none is running, and otherwise checks
/// each running child with backoff, collecting every one that has exited.
fn run_reaper(children: Receiver<Child>) {
    let mut running: Vec<Child> = Vec::new();
    let mut wait = FIRST_CHECK;
    loop {
        // With nothing running the reaper sleeps until a child comes; otherwise the next check is due
        // after `wait`, or sooner for a new child.
        let next = if running.is_empty() {
            children.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            // When: `running` holds a child, the next check is due after `wait`.
            children.recv_timeout(wait)
        };
        match next {
            Ok(child) => {
                running.push(child);
                wait = FIRST_CHECK;
            }
            Err(RecvTimeoutError::Timeout) => {
                // No new child within the wait: the next wait doubles, up to the longest.
                wait = (wait * 2).min(LONGEST_CHECK);
            }
            Err(RecvTimeoutError::Disconnected) if running.is_empty() => {
                // When: every sender is gone (`Disconnected`) and nothing is `running`, the reaper ends.
                return;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Every sender is gone, but running children are still checked.
                std::thread::sleep(wait);
            }
        }
        // `try_wait` collects an exited child's status; one that cannot be read is dropped as
        // collected, since asking again would fail the same way.
        running.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
    }
}

/// Check `child` for up to [`LONGEST_CHECK`], collecting it if it exits in that time.
fn collect_briefly(mut child: Child) {
    let mut waited = Duration::ZERO;
    while waited < LONGEST_CHECK {
        let still_running = matches!(child.try_wait(), Ok(None));
        if !still_running {
            // When: not `still_running`, the child was collected or cannot be read: nothing is left.
            return;
        }
        std::thread::sleep(FIRST_CHECK);
        waited += FIRST_CHECK;
    }
}

#[cfg(test)]
#[path = "detached_spawn_tests.rs"]
mod detached_spawn_tests;
