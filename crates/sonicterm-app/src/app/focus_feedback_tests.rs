use super::*;
use sonicterm_cfg::{config::Config, keymap::Direction, keymap::Keymap, theme::Theme};
use sonicterm_ui::{pane::Rect, selection::Selection};

fn split_child() -> (App, WindowId, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["child"]);
    let original = app.__test_child_active_pane(child).expect("seeded child pane");
    assert!(app.__test_set_child_pane_viewport(
        child,
        Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0,
    ));
    assert!(app.__test_child_split_active_right(child));
    let active = app.__test_child_active_pane(child).expect("split child active pane");
    assert_ne!(active, original, "split must focus the new right pane");
    (app, child, active, original)
}

/// Pointer focus clears source selection once while same-pane requests preserve destination state.
#[test]
fn pointer_focus_transition_is_one_shot_and_clears_stale_selection() {
    let (mut app, child, source, target) = split_child();
    let stale = Selection::new(0, 0).with_content_state(source, 0, false, 0);
    let window = app.windows.get_mut(&child).expect("seeded child window");
    window.selection = Some(stale);

    let change = window.begin_pointer_pane_focus_change(target).expect("inactive pane transition");

    assert_eq!(change.pane_id, target);
    assert_eq!(window.tab_states[window.tabs.active_index()].active_pane, target);
    assert_eq!(window.selection, None, "source-pane selection must not move to the target");
    let target_selection = Selection::new(1, 1).with_content_state(target, 0, false, 0);
    window.selection = Some(target_selection);
    assert!(
        window.begin_pointer_pane_focus_change(target).is_none(),
        "focusing the already-active pane must not replay feedback"
    );
    assert_eq!(
        window.selection,
        Some(target_selection),
        "an already-active request must preserve destination selection"
    );
}

/// A target outside the active tab is rejected without changing focus or selection.
#[test]
fn pane_focus_transition_rejects_non_leaf_targets() {
    let (mut app, child, source, _target) = split_child();
    let selection = Selection::new(0, 0).with_content_state(source, 0, false, 0);
    let window = app.windows.get_mut(&child).expect("seeded child window");
    window.selection = Some(selection);

    assert!(window.begin_pane_focus_change(u64::MAX).is_none());
    assert_eq!(window.tab_states[window.tabs.active_index()].active_pane, source);
    assert_eq!(window.selection, Some(selection));
}

/// Finishing feedback dirties every pane after selection work while preserving the target pane.
#[test]
fn pane_focus_feedback_finishes_with_dirty_target_frame() {
    let (mut app, child, _source, target) = split_child();
    let window = app.windows.get_mut(&child).expect("seeded child window");
    for pane in window.panes.values() {
        pane.parser.lock().grid_mut().clear_dirty();
    }
    let change = window.begin_pane_focus_change(target).expect("inactive pane transition");
    window.selection = Some(Selection::new(1, 1).with_content_state(target, 0, false, 0));

    window.finish_pane_focus_change(change);

    assert_eq!(window.selection.and_then(|selection| selection.pane_id), Some(target));
    assert!(
        window.panes.values().all(|pane| pane.parser.lock().grid().dirty_rows().count() > 0),
        "the feedback frame must rebuild every pane after focus changes"
    );
}

/// Main and child directional routes select the same neighboring leaf without replaying at an edge.
#[test]
fn directional_focus_routes_preserve_main_child_parity() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main_left = app.__test_seed_tab("main");
    assert!(app.__test_set_main_pane_viewport(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0,));
    app.__test_split_active_right();
    let main_right = app.__test_active_pane_in_tab(0).expect("split main active pane");
    let main_selection = Selection::new(1, 2).with_content_state(main_right, 0, false, 0);
    assert!(app.__test_set_main_selection(Some(main_selection)));

    let child = app.__test_seed_child_window(&["child"]);
    let child_left = app.__test_child_active_pane(child).expect("seeded child pane");
    assert!(app.__test_set_child_pane_viewport(
        child,
        Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0,
    ));
    assert!(app.__test_child_split_active_right(child));
    let child_right = app.__test_child_active_pane(child).expect("split child active pane");
    let child_selection = Selection::new(1, 2).with_content_state(child_right, 0, false, 0);
    assert!(app.__test_set_child_selection(child, Some(child_selection)));

    app.focus_pane_dir(Direction::Left);
    assert!(app.focus_pane_dir_in_child(child, Direction::Left));

    assert_eq!(app.__test_active_pane_in_tab(0), Some(main_left));
    assert_eq!(app.__test_child_active_pane(child), Some(child_left));
    assert_eq!(app.main_selection().copied().flatten(), Some(main_selection));
    assert_eq!(app.__test_window_selection(child).flatten(), Some(child_selection));
    assert_ne!(main_left, main_right);
    assert_ne!(child_left, child_right);

    app.focus_pane_dir(Direction::Left);
    assert!(app.focus_pane_dir_in_child(child, Direction::Left));
    assert_eq!(app.__test_active_pane_in_tab(0), Some(main_left));
    assert_eq!(app.__test_child_active_pane(child), Some(child_left));
    assert_eq!(app.main_selection().copied().flatten(), Some(main_selection));
    assert_eq!(app.__test_window_selection(child).flatten(), Some(child_selection));
}

/// Child URL lookup reads the explicitly clicked pane without preempting its focus transition.
#[test]
fn child_url_attribution_does_not_mutate_focus() {
    let (app, child, active, target) = split_child();
    assert!(app.__test_advance_child_pane_parser(
        child,
        target,
        b"\x1b]8;;https://example.com\x1b\\A\x1b]8;;\x1b\\",
    ));

    assert_eq!(
        app.child_hyperlink_uri_at(child, target, 0, 0).as_deref(),
        Some("https://example.com")
    );
    assert_eq!(app.__test_child_active_pane(child), Some(active));
}

/// Pane attribution identifies one half-open split rectangle without mutating focus state.
#[test]
fn clicked_pane_attribution_uses_half_open_rectangles() {
    let rects =
        [(11, Rect::new(0.0, 0.0, 400.0, 200.0)), (22, Rect::new(400.0, 0.0, 400.0, 200.0))];

    assert_eq!(pane_id_at_point(&rects, 399.9, 20.0), Some(11));
    assert_eq!(pane_id_at_point(&rects, 400.0, 20.0), Some(22));
    assert_eq!(pane_id_at_point(&rects, 800.0, 20.0), None);
}
