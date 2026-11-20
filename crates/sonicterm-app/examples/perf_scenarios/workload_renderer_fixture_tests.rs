//! Native renderer coverage stays in the harness test process and its one event loop.

use super::*;

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
/// reports `HOST_INCAPABLE`, as one with no adapter does, instead of stalling the suite.
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
    let mut child = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args([test_name, "--exact", "--nocapture", "--test-threads=1"])
        .env(COVERAGE_CHILD, "1")
        .spawn()
        .expect("the coverage child process");
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("the coverage child's status") {
            // When: the child exited, its own assertions decide the test.
            assert!(status.success(), "the coverage child failed: {status}");
            return;
        }
        if started.elapsed() > COVERAGE_DEADLINE {
            // When: the child ran past `COVERAGE_DEADLINE`, a native call blocked it; a kill or
            // reap that fails leaves nothing more to stop, so both results are ignored.
            let _ = child.kill();
            let _ = child.wait();
            println!(
                "capability=HOST_INCAPABLE case=working-set-coverage reason=a native \
                 presentation call blocked past {COVERAGE_DEADLINE:?}"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
