//! Native renderer coverage stays in the harness test process and its one event loop.

use super::super::coverage_watchdog::{
    supervise, ChildExit, CoverageVerdict, Supervised, WatchBounds, WatchClock,
};
use super::*;

/// A real child process under the watchdog.
struct StdChild(std::process::Child);

impl Supervised for StdChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ChildExit>> {
        Ok(self
            .0
            .try_wait()?
            .map(|status| ChildExit { success: status.success(), status: status.to_string() }))
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.0.kill()
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

/// Wall-clock time since the child started.
struct RealClock {
    started: Instant,
}

impl WatchClock for RealClock {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn sleep(&mut self, step: Duration) {
        std::thread::sleep(step);
    }
}

/// How long the real-renderer coverage may run in its child process before a native
/// presentation call counts as blocked: the coverage itself finishes in seconds.
const COVERAGE_DEADLINE: Duration = Duration::from_secs(90);

/// Set in the child process this test starts, which runs the coverage on its own event loop.
const COVERAGE_CHILD: &str = "SONICTERM_RENDERER_COVERAGE_CHILD";

/// Once fallback completes on a presented frame, the helper's identity set covers every tile
/// the real renderer holds for S9 and S12 with the tab bar, titles, cursor and an open
/// palette, at scale 1 and 2, with the same raster sizes, and S9's representative emoji and CJK
/// are resident real tiles; the module comment states what this proves and what it does not.
/// The coverage runs in a child process the test can stop: a host whose software adapter
/// blocks a native presentation call cannot be interrupted inside this process, so such a host
/// reports `HOST_INCAPABLE`, as one with no adapter does, instead of stalling the suite. A child
/// that cannot be stopped and reaped fails the test, since it is left running.
#[test]
fn the_helper_covers_the_real_renderer() {
    if std::env::var_os(COVERAGE_CHILD).is_some() {
        // When: this process is the `COVERAGE_CHILD`, it runs the cases on its one event loop.
        let event_loop =
            EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
        let mut probe = Probe { outcome: None };
        event_loop.run_app(&mut probe).expect("working-set coverage event loop");
        probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
        return;
    }
    // libtest names a test by its module path without the crate name.
    let path = concat!(module_path!(), "::the_helper_covers_the_real_renderer");
    let test_name = path.split_once("::").map_or(path, |(_, rest)| rest);
    let child = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args([test_name, "--exact", "--test-threads=1"])
        .env(COVERAGE_CHILD, "1")
        .spawn()
        .expect("the coverage child process");
    let mut clock = RealClock { started: Instant::now() };
    let bounds = WatchBounds {
        deadline: COVERAGE_DEADLINE,
        reap: Duration::from_secs(10),
        poll: Duration::from_millis(100),
    };
    match supervise(&mut StdChild(child), &mut clock, bounds) {
        CoverageVerdict::Passed => {}
        CoverageVerdict::HostIncapable => println!(
            "capability=HOST_INCAPABLE case=working-set-coverage reason=a native \
             presentation call blocked past {COVERAGE_DEADLINE:?}"
        ),
        CoverageVerdict::Failed(reason) => panic!("{reason}"),
    }
}
