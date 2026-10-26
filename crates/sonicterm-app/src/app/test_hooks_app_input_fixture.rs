//! Native input submission observations and source-window fixtures.

use super::*;

pub(in crate::app) struct RetiringSink;

impl crate::os_drag::OsDragSink for RetiringSink {
    fn begin_drag(&self, _payload: &crate::os_drag::TabPayload) -> crate::os_drag::DragAck {
        crate::os_drag::DragAck::Accepted
    }
}

pub(super) type SubmittedInput = Vec<(u64, Vec<u8>)>;

#[cfg(any(windows, unix))]
use super::super::pty_test_support::{isolated, phase as input_phase, record_process};

/// Keep PTY teardown inside the isolated test's deadline, including assertion unwinding.
#[cfg(any(windows, unix))]
pub(in crate::app) struct InputTestApp(App);

#[cfg(any(windows, unix))]
impl std::ops::Deref for InputTestApp {
    type Target = App;
    fn deref(&self) -> &App {
        &self.0
    }
}

#[cfg(any(windows, unix))]
impl std::ops::DerefMut for InputTestApp {
    fn deref_mut(&mut self) -> &mut App {
        &mut self.0
    }
}

#[cfg(any(windows, unix))]
impl InputTestApp {
    /// Wait for queue capacity before a delivery assertion; native completion is a separate observation.
    pub(in crate::app) fn wait_for_input_queues(&self) {
        for window in self.windows.values() {
            for (pane_id, pane) in &window.panes {
                let Some(pty) = pane.pty.as_ref() else { continue };
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let diagnostics = pty.input_diagnostics();
                    if diagnostics.queued_messages == 0 {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "pane {pane_id} input queue did not drain: {diagnostics:?}"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}

// Lifecycle: InputTestApp drops each live PTY while the parent still retains its PID for timeout cleanup.
#[cfg(any(windows, unix))]
impl Drop for InputTestApp {
    fn drop(&mut self) {
        for window in self.0.windows.values_mut() {
            for (pane_id, pane) in &mut window.panes {
                input_phase(*pane_id, "drop-begin");
                let pid = pane.pty.as_ref().and_then(PtyHandle::pid);
                drop(pane.pty.take());
                if let Some(pid) = pid {
                    record_process(pid, false);
                }
                input_phase(*pane_id, "drop-end");
            }
        }
    }
}

pub(in crate::app) fn submission_snapshot(bytes: &[u8]) -> Option<Vec<u8>> {
    PTY_SUBMISSIONS.with_borrow(|slot| {
        slot.as_ref().map(|_| {
            #[cfg(any(windows, unix))]
            input_phase(0, "submit-begin");
            bytes.to_vec()
        })
    })
}

pub(in crate::app) fn record_submission(pane: u64, bytes: Vec<u8>) {
    PTY_SUBMISSIONS.with_borrow_mut(|slot| slot.as_mut().unwrap().push((pane, bytes)));
    #[cfg(any(windows, unix))]
    input_phase(pane, "submit-end");
}

thread_local! {
    static PTY_SUBMISSIONS: std::cell::RefCell<Option<SubmittedInput>> = const { std::cell::RefCell::new(None) };
}

/// Scope successful-submission evidence to one test's event-loop thread.
pub(in crate::app) struct PtySubmissions;

impl PtySubmissions {
    pub(in crate::app) fn start() -> Self {
        PTY_SUBMISSIONS.with_borrow_mut(|slot| {
            assert!(slot.is_none(), "submission observation cannot nest");
            *slot = Some(Vec::new());
        });
        Self
    }

    pub(in crate::app) fn take(&self) -> SubmittedInput {
        PTY_SUBMISSIONS.with_borrow_mut(|slot| std::mem::take(slot.as_mut().unwrap()))
    }
}

impl Drop for PtySubmissions {
    // Lifecycle: every test releases its thread-local observer, including assertion unwinding.
    fn drop(&mut self) {
        PTY_SUBMISSIONS.with_borrow_mut(|slot| *slot = None);
    }
}

/// Install an idle real PTY without a VT worker that could change the test's negotiated modes.
#[cfg(any(windows, unix))]
pub(in crate::app) fn attach_idle_pty(app: &mut App, window: WindowId, pane: u64) {
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/D".into(), "/Q".into()]);
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-s".into()]);
    input_phase(pane, "spawn-begin");
    app.windows.get_mut(&window).unwrap().panes.get_mut(&pane).unwrap().pty =
        Some(PtyHandle::spawn_with_args(program, &args, 80, 24).expect("idle input PTY"));
    record_process(app.windows[&window].panes[&pane].pty.as_ref().unwrap().pid().unwrap(), true);
    input_phase(pane, "spawn-end");
}

#[cfg(any(windows, unix))]
pub(in crate::app) fn input_test_app() -> (InputTestApp, WindowId, u64) {
    let mut app = InputTestApp(App::new(Theme::default(), Config::default(), Keymap::default()));
    let pane = app.__test_seed_tab("input");
    let window = app.main_window_id.unwrap();
    attach_idle_pty(&mut app, window, pane);
    (app, window, pane)
}

/// Main, child, and sibling windows retain independent real PTYs for admission assertions.
#[cfg(any(windows, unix))]
pub(in crate::app) fn input_test_windows() -> (InputTestApp, [(WindowId, u64); 3]) {
    let (mut app, main, main_pane) = input_test_app();
    let child = app.__test_seed_child_window(&["child"]);
    let sibling = app.__test_seed_child_window(&["sibling"]);
    let child_pane = app.windows[&child].tab_states[0].active_pane;
    let sibling_pane = app.windows[&sibling].tab_states[0].active_pane;
    attach_idle_pty(&mut app, child, child_pane);
    attach_idle_pty(&mut app, sibling, sibling_pane);
    (app, [(main, main_pane), (child, child_pane), (sibling, sibling_pane)])
}

#[cfg(test)]
#[path = "test_hooks_app_input_fixture_tests.rs"]
mod test_hooks_app_input_fixture_tests;
