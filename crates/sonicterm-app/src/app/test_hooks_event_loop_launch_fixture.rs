//! Launch-request fixtures and explicit-source user-event contracts.

use super::*;

/// A child window with a tab, seeded headless, plus the main window it belongs beside.
pub(super) fn app_with_child_window() -> (App, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    (app, child)
}

/// Captured log text, shared with the subscriber's writer.
#[cfg(unix)]
#[derive(Clone, Default)]
pub(super) struct LaunchLog(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(unix)]
impl std::io::Write for LaunchLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `action` with every level captured and return the log text.
#[cfg(unix)]
pub(super) fn capture_all_levels(action: impl FnOnce()) -> String {
    let log = LaunchLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, action);
    let bytes = log.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

/// Every visible character of the main window's tab `index`, row by row.
#[cfg(unix)]
pub(super) fn main_tab_text(app: &App, index: usize) -> String {
    let window = app.main().unwrap();
    let pane_id = window.tab_states[index].active_pane;
    let parser = window.panes[&pane_id].parser.lock();
    parser
        .grid()
        .rows_iter()
        .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "test_hooks_event_loop_launch_fixture_tests.rs"]
mod test_hooks_event_loop_launch_fixture_tests;
