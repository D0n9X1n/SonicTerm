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

    let resolved = app.cell_target_at(child, target, 0, 0).expect("OSC 8 target");
    assert!(matches!(
        resolved.target,
        super::path_target::ResolvedCellTarget::Uri(ref uri) if uri == "https://example.com"
    ));
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

/// Clear every pane's dirt in `window`, as a presented frame would.
fn clear_window_dirt(window: &WindowState) {
    for pane in window.panes.values() {
        pane.parser.lock().grid_mut().clear_dirty();
    }
}

/// Dirty-row count of each pane in `window`, keyed by pane id.
fn window_dirt(window: &WindowState) -> std::collections::BTreeMap<u64, usize> {
    window
        .panes
        .iter()
        .map(|(&pane_id, pane)| (pane_id, pane.parser.lock().grid().dirty_count()))
        .collect()
}

/// A pointer press that moves focus to another pane changes no cell and no geometry, so
/// no pane's rows are dirtied: selection and focus reach the renderer as window identity.
#[test]
fn pointer_focus_press_dirties_no_pane() {
    let (mut app, child, _source, target) = split_child();
    let window = app.windows.get_mut(&child).expect("seeded child window");
    clear_window_dirt(window);

    assert!(window.begin_local_selection(target, (0, 0), 1), "the press binds a selection");

    assert_eq!(window.tab_states[window.tabs.active_index()].active_pane, target);
    assert!(window_dirt(window).values().all(|&count| count == 0), "{:?}", window_dirt(window));
}

/// A child wheel scroll and a child scrollbar view-top write move one viewport, which the
/// pane identity carries; neither the scrolled pane nor its sibling gains dirty rows.
#[test]
fn child_scroll_and_view_top_dirty_no_pane() {
    let (mut app, child, active, sibling) = split_child();
    assert!(app.__test_advance_child_pane_parser(child, active, "line\r\n".repeat(80).as_bytes()));
    let mode = app.config.appearance.scrollbar;
    let window = app.windows.get_mut(&child).expect("seeded child window");
    clear_window_dirt(window);

    super::child_window::scroll_child_pane(window, active, -3, mode);

    assert!(window.panes[&active].viewport_top_abs.is_some(), "the wheel scrolled back");
    assert!(window_dirt(window).values().all(|&count| count == 0), "{:?}", window_dirt(window));

    let (live_top, at) = {
        let parser = window.panes[&active].parser.lock();
        let grid = parser.grid();
        (grid.scrollback_len() as u64, super::viewport_anchor::ViewportBaseline::of(grid))
    };
    app.set_child_pane_view_top(child, active, live_top / 2, live_top, at);
    let window = &app.windows[&child];
    assert_eq!(window.panes[&active].viewport_top_abs, Some(live_top / 2));
    assert!(window_dirt(window).values().all(|&count| count == 0), "{:?}", window_dirt(window));
    assert!(window.panes.contains_key(&sibling));
}

/// A main-window wheel scroll and a scrollbar view-top write dirty no pane in the window.
#[test]
fn main_scroll_and_view_top_dirty_no_pane() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    app.__test_split_active_right();
    assert!(app.__test_advance_pane_parser(pane_id, "line\r\n".repeat(80).as_bytes()));
    let main = app.main().expect("main window");
    assert_eq!(main.panes.len(), 2, "the split added a sibling");
    clear_window_dirt(main);

    app.scroll_pane(pane_id, -3);

    let main = app.main().expect("main window");
    assert!(main.panes[&pane_id].viewport_top_abs.is_some(), "the wheel scrolled back");
    assert!(window_dirt(main).values().all(|&count| count == 0), "{:?}", window_dirt(main));

    let active = main.tab_states[main.tabs.active_index()].active_pane;
    let (live_top, at) = {
        let parser = main.panes[&active].parser.lock();
        let grid = parser.grid();
        (grid.scrollback_len() as u64, super::viewport_anchor::ViewportBaseline::of(grid))
    };
    app.set_active_pane_view_top(live_top, live_top, at);
    let main = app.main().expect("main window");
    assert!(window_dirt(main).values().all(|&count| count == 0), "{:?}", window_dirt(main));
}

/// A child splitter drag resizes only the two panes beside the divider: each resized grid
/// is dirty on every row, and a pane whose size did not change stays clean.
#[test]
fn child_splitter_drag_dirties_only_resized_panes() {
    let (mut app, child, middle, left) = split_child();
    assert!(app.__test_child_split_active_right(child));
    let right = app.__test_child_active_pane(child).expect("second split pane");
    let outer = Rect::new(0.0, 0.0, 800.0, 240.0);
    let window = app.windows.get_mut(&child).expect("seeded child window");
    let rects = window.tab_states[0].tree.layout(outer);
    let right_rect = rects.iter().find(|(id, _)| *id == right).map(|(_, rect)| *rect).unwrap();
    let seam = (right_rect.x, right_rect.y + 10.0);
    let hit = window.tab_states[0].tree.hit_splitter(outer, 8.0, seam.0, seam.1).expect("seam");
    window.splitter_drag =
        Some(SplitterDragState { splitter: hit.id, axis: hit.axis, last_pos: seam });
    clear_window_dirt(window);
    let before = app.__test_child_pane_grid_size(child, left);

    assert!(app.apply_splitter_drag_in_child(child, seam.0 + 40.0, seam.1));

    let window = &app.windows[&child];
    assert_eq!(app.__test_child_pane_grid_size(child, left), before, "the far pane kept its size");
    let dirt = window_dirt(window);
    assert_eq!(dirt[&left], 0, "an unresized pane gains no dirt: {dirt:?}");
    for pane_id in [middle, right] {
        let rows = usize::from(window.panes[&pane_id].parser.lock().grid().rows);
        assert_eq!(dirt[&pane_id], rows, "a resized pane is dirty on every row: {dirt:?}");
    }
}

/// Tab reorder keeps window-wide dirt: it is a retained exception, not a pointer operation.
#[test]
fn tab_reorder_keeps_window_wide_dirt() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["first", "second"]);
    let window = app.windows.get_mut(&child).expect("seeded child window");
    clear_window_dirt(window);

    assert!(window.reorder_tab(0, 1));

    assert!(window_dirt(window).values().all(|&count| count > 0), "{:?}", window_dirt(window));
}

/// The pointer files mark no window-wide dirt, and the remaining callers are exactly the
/// once-per-state-change paths (config, theme, keyboard, search, redraw, window topology).
#[test]
fn window_wide_dirt_callers_inventory() {
    let call = "mark_all_panes_dirty(";
    let count = |source: &str| {
        source
            .replace("\r\n", "\n")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//") && line.contains(call))
            .filter(|line| !line.contains("fn mark_all_panes_dirty"))
            .count()
    };
    for (name, source) in [
        ("window_pointer.rs", include_str!("window_pointer.rs")),
        ("child_window_pointer.rs", include_str!("child_window_pointer.rs")),
        ("scroll.rs", include_str!("scroll.rs")),
        ("scrollbar_input.rs", include_str!("scrollbar_input.rs")),
        ("splitter_input.rs", include_str!("splitter_input.rs")),
        ("child_window.rs", include_str!("child_window.rs")),
        ("selection_gesture.rs", include_str!("selection_gesture.rs")),
        ("tab_gesture.rs", include_str!("tab_gesture.rs")),
    ] {
        assert_eq!(count(source), 0, "{name} marks window-wide dirt");
    }
    for (name, source, expected) in [
        ("config_apply.rs", include_str!("config_apply.rs"), 3),
        ("misc.rs", include_str!("misc.rs"), 4),
        ("redraw.rs", include_str!("redraw.rs"), 1),
        ("search_handle.rs", include_str!("search_handle.rs"), 1),
        ("window_keyboard.rs", include_str!("window_keyboard.rs"), 3),
        ("window_state.rs", include_str!("window_state.rs"), 1),
    ] {
        assert_eq!(count(source), expected, "{name} window-wide dirt callers");
    }
    let state = include_str!("window_state.rs").replace("\r\n", "\n");
    let mark = state.find("mark_all_panes_dirty(&self.panes)").expect("window branch");
    assert!(
        state[mark.saturating_sub(120)..mark].contains("TopologyDirt::Window"),
        "the remaining window_state caller is the TopologyDirt::Window branch"
    );
}

/// Every constructor starts both pacing clocks at one instant, so neither paces a new window.
#[test]
fn every_window_constructor_starts_both_clocks_together() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    let child = app.__test_seed_child_window(&["child"]);
    for window in app.windows.values() {
        assert_eq!(window.stream_clock, window.last_render);
    }
    assert!(app.windows.contains_key(&child));
    // The native constructors cannot run headless, so their literals are pinned by source.
    for (name, source) in [
        ("event_loop.rs", include_str!("event_loop.rs")),
        ("misc.rs", include_str!("misc.rs")),
        ("tear_out.rs", include_str!("tear_out.rs")),
        ("test_hooks_windows.rs", include_str!("test_hooks_windows.rs")),
    ] {
        let source = source.replace("\r\n", "\n");
        let literals = source.matches("last_render: created_at,\n").count();
        assert!(literals > 0, "{name} builds a window");
        assert_eq!(
            source
                .matches("last_render: created_at,\n            stream_clock: created_at,")
                .count(),
            literals,
            "{name} sets both clocks from one instant"
        );
        assert!(!source.contains("last_render: Instant::now()"), "{name}");
        assert!(!source.contains("last_render: std::time::Instant::now()"), "{name}");
    }
}

/// The test hook that backdates a window moves both clocks, so native tests still cross a frame.
#[test]
fn backdating_hook_moves_both_clocks() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["child"]);
    let backdated = Instant::now() - Duration::from_millis(500);
    assert!(app.__test_set_window_last_render(child, backdated));
    assert_eq!(app.windows[&child].last_render, backdated);
    assert_eq!(app.windows[&child].stream_clock, backdated);
}

/// A local selection drag reads only its press pane: with the sibling pane's parser held by
/// another thread, the press and each extension complete promptly, take no counted parser lock,
/// and dirty no pane, so selection reaches the renderer as window identity alone.
#[test]
fn selection_drag_locks_only_the_selecting_pane() {
    use crate::app::frame_counters::{DispatchScope, DispatchTotals};
    use std::sync::{atomic::Ordering, mpsc, Arc};
    use std::time::{Duration, Instant};
    let (mut app, child, active, sibling) = split_child();
    let window = app.windows.get_mut(&child).expect("seeded child window");
    clear_window_dirt(window);
    let before = window_dirt(window);

    // The helper holds the sibling's parser until released, or for at most five seconds, so a
    // drag that locks the sibling is detected by its delay instead of hanging the suite.
    let sibling_parser = Arc::clone(&window.panes[&sibling].parser);
    let (held_sender, held) = mpsc::channel();
    let (release, release_receiver) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = sibling_parser.lock();
        held_sender.send(()).expect("the test waits for the hold");
        let _ = release_receiver.recv_timeout(Duration::from_secs(5));
    });
    held.recv().expect("the sibling parser is held");

    let totals = Arc::new(DispatchTotals::default());
    let started = Instant::now();
    {
        let _scope = DispatchScope::enter(Some(Arc::clone(&totals)));
        assert!(window.begin_local_selection(active, (0, 0), 1), "the press binds a selection");
        for col in 1..4 {
            assert!(window.extend_local_selection_to_cell((1, col)), "the drag extends");
        }
    }
    let elapsed = started.elapsed();
    // A holder that timed out has dropped its receiver; the elapsed check below reports why.
    let _ = release.send(());
    holder.join().expect("the holder exits");
    assert!(elapsed < Duration::from_secs(2), "the drag blocked on the sibling for {elapsed:?}");
    assert_eq!(totals.locks.load(Ordering::Relaxed), 0, "the drag only try-locks its pane");
    assert_eq!(window_dirt(window), before, "the drag dirtied a pane");
    assert_eq!(window.selection.and_then(|selection| selection.pane_id), Some(active));
}
