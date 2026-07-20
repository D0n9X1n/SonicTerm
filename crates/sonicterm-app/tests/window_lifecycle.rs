//! process lifecycle follows visible/active terminal windows.
//!
//! Closing the final child after the main window has already been hidden must
//! request process exit. Likewise, closing the last tab in a child via the
//! keymap/tab-close path should not leave a hidden-main/no-child process alive.

use sonicterm_app::app::{
    os_drag::{AppHandle, OsTabDragBackend, TabBarSnapshot},
    App,
};
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct UnregisterTrackingBackend {
    calls: Arc<AtomicUsize>,
}

impl OsTabDragBackend for UnregisterTrackingBackend {
    fn begin_session(
        &mut self,
        _handle: AppHandle,
        _source_window: winit::window::WindowId,
        _source_tab_idx: usize,
        _payload_json: String,
        _drag_image_png: Vec<u8>,
    ) {
    }

    fn unregister_window(&mut self, _window_id: winit::window::WindowId) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }
}

fn app_with_hidden_main_and_child() -> (App, winit::window::WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_hide_main_window();
    assert!(app.__test_main_hidden(), "setup hides the drained main window");
    assert_eq!(app.__test_windows_len(), 1, "setup keeps one active child window");
    assert!(!app.__test_pending_exit(), "setup must not start with a pending exit");
    (app, child)
}

#[test]
fn closing_last_child_tab_after_main_hidden_requests_process_exit() {
    let (mut app, child) = app_with_hidden_main_and_child();

    assert!(app.__test_invoke_close_active_tab_in_child(child));

    assert_eq!(app.__test_windows_len(), 0, "the last child window is reaped");
    assert!(app.__test_main_hidden(), "main stays hidden once no terminal windows remain");
    assert!(app.__test_pending_exit(), "no active terminal window left must quit the process");
}

#[test]
fn closing_last_child_pane_or_tab_after_main_hidden_requests_process_exit() {
    let (mut app, child) = app_with_hidden_main_and_child();

    assert!(app.__test_invoke_close_active_pane_or_tab_in_child(child));

    assert_eq!(app.__test_windows_len(), 0, "the last child window is reaped");
    assert!(app.__test_pending_exit(), "CloseActivePaneOrTab on the final child must quit");
}

#[test]
fn closing_one_child_keeps_running_when_another_child_remains() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let first = app.__test_seed_child_window(&["first"]);
    let _second = app.__test_seed_child_window(&["second"]);
    app.__test_hide_main_window();

    assert!(app.__test_invoke_close_active_tab_in_child(first));

    assert_eq!(app.__test_windows_len(), 1, "one active child remains");
    assert!(!app.__test_pending_exit(), "a remaining child window keeps the process alive");
}

#[test]
fn visible_main_with_tabs_keeps_running_when_child_closes() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);

    assert!(app.__test_invoke_close_active_tab_in_child(child));

    assert_eq!(app.__test_windows_len(), 0, "child is reaped");
    assert!(!app.__test_main_hidden(), "main remains active");
    assert!(!app.__test_pending_exit(), "visible main tabs keep the process alive");
}

#[test]
fn child_reap_purges_drag_snapshot_and_backend_registration() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let registry = app.os_drag_bar_registry();
    registry.publish(TabBarSnapshot {
        window: Some(child),
        window_rect: (0, 0, 100, 100),
        bar_rect: (0, 0, 100, 20),
        tab_lefts: vec![0],
        tab_rights: vec![100],
    });
    let unregister_calls = Arc::new(AtomicUsize::new(0));
    app.__test_set_os_drag_backend(Box::new(UnregisterTrackingBackend {
        calls: unregister_calls.clone(),
    }));

    assert!(app.__test_invoke_close_active_tab_in_child(child));

    assert!(registry.is_empty(), "reaped child must not retain tab-bar snapshots");
    assert_eq!(
        unregister_calls.load(Ordering::Relaxed),
        1,
        "reaped child must unregister its platform drag target"
    );
}

#[test]
fn failed_child_tear_out_source_cleanup_releases_empty_window() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&[]);
    let registry = app.os_drag_bar_registry();
    registry.publish(TabBarSnapshot {
        window: Some(child),
        window_rect: (0, 0, 100, 100),
        bar_rect: (0, 0, 100, 20),
        tab_lefts: Vec::new(),
        tab_rights: Vec::new(),
    });
    let unregister_calls = Arc::new(AtomicUsize::new(0));
    app.__test_set_os_drag_backend(Box::new(UnregisterTrackingBackend {
        calls: unregister_calls.clone(),
    }));

    app.tear_out_apply_child_source_side(child, 0);

    assert_eq!(app.__test_windows_len(), 0);
    assert!(registry.is_empty());
    assert_eq!(unregister_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn ready_no_active_windows_means_exit_even_if_main_is_hidden() {
    assert!(
        App::should_exit_pure(1, true, 0),
        "a hidden main window is not an active terminal window for lifecycle purposes"
    );
    assert!(App::should_exit_pure(0, true, 0));
    assert!(!App::should_exit_pure(1, false, 0));
    assert!(!App::should_exit_pure(0, true, 1));
}
