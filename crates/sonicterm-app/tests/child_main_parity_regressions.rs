//! v0.10.1 main/child parity regression tests.
//!
//! These are intentionally narrow, headless tests for child-window behavior that
//! already exists in the main window path. They use small `__test_*` seams so the
//! eventual fixes can land in the production child helpers without requiring a
//! live winit window or GPU renderer.

use sonicterm_app::app::App;
use sonicterm_cfg::{config::Config, keymap::Action, keymap::Keymap, theme::Theme};
use sonicterm_ui::{pane::Rect, pane::SplitAxis, selection::Selection};

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

fn main_and_child() -> (App, u64, winit::window::WindowId, u64) {
    let mut app = app();
    let main_pane = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let child_pane = app.__test_child_active_pane(child).expect("seeded child has active pane");
    (app, main_pane, child, child_pane)
}

fn selected_row(cols_inclusive: u16) -> Selection {
    Selection { start: (0, 0), end: (0, cols_inclusive), anchored: true }
}

#[test]
fn child_copy_action_uses_child_selection_not_main_selection() {
    let (mut app, main_pane, child, child_pane) = main_and_child();
    assert!(app.__test_advance_pane_parser(main_pane, b"main"));
    assert!(app.__test_advance_child_pane_parser(child, child_pane, b"child"));
    assert!(app.__test_set_main_selection(Some(selected_row(3))));
    assert!(app.__test_set_child_selection(child, Some(selected_row(4))));
    app.__test_set_memory_clipboard("unchanged");

    assert!(app.run_action_for_window(&Action::CopyToClipboard, child));

    assert_eq!(
        app.__test_memory_clipboard().as_deref(),
        Some("child"),
        "CopyToClipboard dispatched from a child window must copy the child selection, not the main window selection"
    );
}

#[test]
fn child_paste_action_writes_to_child_active_pane_not_main_pty() {
    let (mut app, main_pane, child, child_pane) = main_and_child();
    app.__test_set_memory_clipboard("paste-child");
    let _ = app.__test_drain_pty_writes();

    assert!(app.run_action_for_window(&Action::PasteFromClipboard, child));

    assert_eq!(
        app.__test_drain_pty_writes(),
        vec![(child_pane, b"paste-child".to_vec())],
        "PasteFromClipboard dispatched from a child window must target the child active pane; main pane was {main_pane}"
    );
}

#[test]
fn child_focus_blur_cancels_child_ime_preedit() {
    let (mut app, _main_pane, child, _child_pane) = main_and_child();
    assert!(app.__test_set_child_ime_preedit(child, "nihao"));
    assert_eq!(app.__test_child_ime_composing(child), Some(true));

    app.__test_handle_child_focus_changed(child, false);

    assert_eq!(
        app.__test_child_ime_composing(child),
        Some(false),
        "child focus loss must cancel in-flight IME preedit like the main window path"
    );
}

#[test]
fn child_focus_blur_updates_renderer_focus_state() {
    let (mut app, _main_pane, child, _child_pane) = main_and_child();
    assert!(app.__test_set_child_renderer_focus_marker(child, true));

    app.__test_handle_child_focus_changed(child, false);

    assert_eq!(
        app.__test_child_renderer_focus_marker(child),
        Some(false),
        "child focus loss must propagate to the child renderer so cursor focus state matches main"
    );
}

#[test]
fn child_focus_blur_marks_child_panes_dirty_for_redraw() {
    let (mut app, _main_pane, child, child_pane) = main_and_child();
    assert!(app.__test_clear_child_pane_dirty(child, child_pane));
    assert_eq!(app.__test_child_pane_dirty_count(child, child_pane), Some(0));

    app.__test_handle_child_focus_changed(child, false);

    assert!(
        app.__test_child_pane_dirty_count(child, child_pane).unwrap_or(0) > 0,
        "child focus transition must mark panes dirty so the cursor/focus repaint is not deferred indefinitely"
    );
}

#[test]
fn child_focus_reports_decset_focus_to_child_active_pane() {
    let (mut app, _main_pane, child, child_pane) = main_and_child();
    assert!(app.__test_advance_child_pane_parser(child, child_pane, b"\x1b[?1004h"));
    let _ = app.__test_drain_pty_writes();

    app.__test_handle_child_focus_changed(child, false);

    assert_eq!(
        app.__test_drain_pty_writes(),
        vec![(child_pane, b"\x1b[O".to_vec())],
        "child focus loss must send DECSET ?1004 focus-out to the child active pane"
    );
}

#[test]
fn child_search_ime_candidate_anchor_uses_search_box_not_terminal_cursor() {
    let (mut app, _main_pane, child, _child_pane) = main_and_child();
    assert!(app.__test_invoke_open_search_in_child(child));

    assert_eq!(
        app.__test_child_ime_candidate_anchor_kind(child),
        Some("search"),
        "when child search is open, IME candidate area should anchor to the search query caret like main, not terminal cursor"
    );
}

#[test]
fn child_splitter_hover_sets_resize_cursor_without_active_drag() {
    let mut app = app();
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.__test_set_child_pane_viewport(
        child,
        Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0
    ));
    assert!(app.__test_child_split_active_right(child));

    assert_eq!(
        app.__test_child_splitter_hit_axis(child, 400.0, 20.0),
        Some(SplitAxis::Vertical),
        "test setup should place the cursor on the vertical child splitter"
    );

    assert!(
        app.__test_refresh_child_splitter_hover(child, 400.0, 20.0),
        "hovering a child splitter without dragging should be handled, just like main"
    );
    assert_eq!(
        app.__test_child_splitter_hover(child),
        Some(SplitAxis::Vertical),
        "child splitter hover state should remember the resize axis so cursor can be restored when leaving"
    );
}
