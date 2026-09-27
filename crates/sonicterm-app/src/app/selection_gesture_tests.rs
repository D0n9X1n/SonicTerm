use super::*;
use crate::app::{next_pane_id, App, PaneState};
use parking_lot::Mutex;
use sonicterm_cfg::{
    config::Config,
    keymap::{Direction, Keymap},
    theme::Theme,
};
use sonicterm_vt::vt::Parser;
use std::sync::Arc;
use winit::window::WindowId;

/// A `cols`×`rows` pane layout of 10×20 px cells at `x`, `y`.
fn layout(id: u64, x: f32, y: f32, cols: u16, rows: u16) -> PaneLayoutSnapshot {
    PaneLayoutSnapshot {
        id,
        origin_x_logical: x,
        origin_y_logical: y,
        w_logical: f32::from(cols) * 10.0,
        h_logical: f32::from(rows) * 20.0,
        cell_w_logical: 10.0,
        cell_h_logical: 20.0,
        cols,
        rows,
    }
}

/// Text of absolute row `abs`, without trailing blank cells.
fn row_text(grid: &Grid, abs: u64) -> String {
    let row = grid.row_at_abs(abs);
    let text: String = row.map(|row| row.iter().map(|cell| cell.ch).collect()).unwrap_or_default();
    text.trim_end_matches([' ', '\0']).to_owned()
}

/// Resize `pane_id` to `cols`×`rows` and write `text` into it.
fn fill(state: &WindowState, pane_id: u64, cols: u16, rows: u16, text: &str) {
    let mut parser = state.panes[&pane_id].parser.lock();
    parser.resize(cols, rows);
    parser.advance(text.as_bytes());
}

/// Split `focus` in `window`'s first tab, putting a new empty pane `dir` of it.
fn split_pane(app: &mut App, window: WindowId, focus: u64, dir: Direction) -> u64 {
    let pane_id = next_pane_id();
    let pool = Arc::clone(&app.capture_staging_pool);
    let parser = Parser::new_with_staging_pool(Grid::new(20, 3), None, pool);
    let shared = Arc::new(Mutex::new(parser));
    let pane = PaneState::new_with_media_pool(shared, None, &app.inline_media_pool);
    let state = app.windows.get_mut(&window).unwrap();
    state.panes.insert(pane_id, pane);
    assert!(state.tab_states[0].tree.split(focus, dir, pane_id));
    pane_id
}

/// Split the first tab of `window` into `left | right` 20×3 panes with distinct text.
fn split_window(app: &mut App, window: WindowId) -> (u64, u64) {
    let left = app.windows[&window].tab_states[0].active_pane;
    let right = split_pane(app, window, left, Direction::Right);
    let state = &app.windows[&window];
    fill(state, left, 20, 3, "left one\r\nleft two");
    fill(state, right, 20, 3, "right one\r\nright two");
    (left, right)
}

/// Split `focus` toward `dir` with a 20×2 pane whose rows read `<name> one`, `<name> two`.
fn named_pane(app: &mut App, window: WindowId, focus: u64, dir: Direction, name: &str) -> u64 {
    let pane = split_pane(app, window, focus, dir);
    fill(&app.windows[&window], pane, 20, 2, &format!("{name} one\r\n{name} two"));
    pane
}

/// Pane arrangements a cross-pane drag is checked across.
#[derive(Debug, Clone, Copy)]
enum Topology {
    /// `west | east`; the drag starts in `west`.
    Horizontal,
    /// `west` over `down`; the drag starts in `west`.
    Vertical,
    /// `west | (east over down)`; the drag starts in `east`.
    Nested,
}

/// A press pane, the pane rectangles as drawn, and a pointer over another pane.
struct Crossing {
    press: u64,
    press_name: &'static str,
    layouts: Vec<PaneLayoutSnapshot>,
    pointer: (f32, f32),
}

/// Build `topology` in `window`'s first tab from 20×2 panes named by their text.
fn build(app: &mut App, window: WindowId, topology: Topology) -> Crossing {
    let west = app.windows[&window].tab_states[0].active_pane;
    fill(&app.windows[&window], west, 20, 2, "west one\r\nwest two");
    let west_layout = layout(west, 0.0, 0.0, 20, 2);
    match topology {
        Topology::Horizontal => {
            let east = named_pane(app, window, west, Direction::Right, "east");
            let layouts = vec![west_layout, layout(east, 210.0, 0.0, 20, 2)];
            Crossing { press: west, press_name: "west", layouts, pointer: (215.0, 30.0) }
        }
        Topology::Vertical => {
            let down = named_pane(app, window, west, Direction::Down, "down");
            let layouts = vec![west_layout, layout(down, 0.0, 50.0, 20, 2)];
            Crossing { press: west, press_name: "west", layouts, pointer: (195.0, 65.0) }
        }
        Topology::Nested => {
            let east = named_pane(app, window, west, Direction::Right, "east");
            let down = named_pane(app, window, east, Direction::Down, "down");
            let layouts = vec![
                west_layout,
                layout(east, 210.0, 0.0, 20, 2),
                layout(down, 210.0, 50.0, 20, 2),
            ];
            Crossing { press: east, press_name: "east", layouts, pointer: (405.0, 65.0) }
        }
    }
}

/// Activate tab `idx` of `window` through the production tab-activation path.
fn activate(app: &mut App, window: WindowId, idx: usize) {
    if Some(window) == app.main_window_id {
        app.__test_invoke_activate_main_tab(idx);
    } else {
        // When: `window` is a child, its own tab bar takes the activation.
        app.__test_invoke_activate_tab_in_child(window, idx);
    }
}

/// A main or child window with tabs `a` and `b`, `a` active and split `left | right`.
fn two_tab_window(app: &mut App, in_child: bool) -> (WindowId, u64) {
    app.__test_seed_tab("a");
    let window = if in_child {
        app.__test_seed_child_window(&["a", "b"])
    } else {
        // When: `in_child` is false, the main window takes the second tab.
        app.__test_seed_tab("b");
        app.main_window_id.unwrap()
    };
    activate(app, window, 0);
    let (left, _) = split_window(app, window);
    (window, left)
}

/// A fresh app and the main or child window a test runs in.
fn app_window(in_child: bool) -> (App, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let window = if in_child {
        app.__test_seed_child_window(&["child"])
    } else {
        // When: `in_child` is false, the seeded main window is the test window.
        app.main_window_id.unwrap()
    };
    (app, window)
}

#[test]
fn foreign_pane_gap_and_outside_motion_clamp_into_the_press_pane() {
    // A pointer over another pane, a gap, or outside every pane resolves through the press pane.
    let (left, right) = (layout(1, 0.0, 0.0, 20, 5), layout(2, 210.0, 0.0, 20, 5));
    assert_eq!(press_pane_cell(left, 215.0, 45.0), (2, 19), "horizontal split");
    assert_eq!(press_pane_cell(right, 195.0, 45.0), (2, 0));
    assert_eq!(press_pane_cell(left, 205.0, 45.0), (2, 19), "gap between panes");
    assert_eq!(press_pane_cell(left, -40.0, -40.0), (0, 0), "outside the window");
    assert_eq!(press_pane_cell(right, 900.0, 900.0), (4, 19));
    let (top, bottom) = (layout(1, 0.0, 0.0, 20, 5), layout(2, 0.0, 110.0, 20, 5));
    assert_eq!(press_pane_cell(top, 55.0, 115.0), (4, 5), "vertical split");
    assert_eq!(press_pane_cell(bottom, 55.0, 95.0), (0, 5));
    let nested =
        [layout(1, 0.0, 0.0, 20, 5), layout(2, 210.0, 0.0, 20, 5), layout(3, 210.0, 110.0, 20, 5)];
    assert_eq!(press_pane_cell(nested[1], 250.0, 150.0), (4, 4), "nested split");
    assert_eq!(press_pane_cell(nested[2], 250.0, 50.0), (0, 4));
    assert_eq!(press_pane_cell(nested[0], 250.0, 150.0), (4, 19));
}

#[test]
fn fractional_origin_maps_through_the_renderer_column_edges() {
    // With a fractional origin and cell width, a point on a column edge takes the renderer's own
    // edge lookup, and padding below the last text row clamps to that row, not a divided guess.
    let press = PaneLayoutSnapshot {
        id: 1,
        origin_x_logical: 0.37,
        origin_y_logical: 11.25,
        w_logical: 200.5,
        h_logical: 100.5,
        cell_w_logical: 13.9,
        cell_h_logical: 20.0,
        cols: 14,
        rows: 5,
    };
    let edges = build_snapped_cell_x(press.origin_x_logical, press.cell_w_logical, press.cols);
    // Dividing the offset by the cell width floors this edge to column 2.
    assert_eq!(press_pane_cell(press, edges[3], 120.0), (4, 3), "edge of column 3, in padding");
    assert_eq!(press_pane_cell(press, edges[6], 50.0), (1, 6));
    for col in 0..press.cols {
        let x = edges[usize::from(col)];
        assert_eq!(press_pane_cell(press, x, 30.0).1, col, "the renderer's column at edge {col}");
    }
    let reviewed =
        PaneLayoutSnapshot { origin_x_logical: 6.25, cell_w_logical: 10.0, cols: 20, ..press };
    assert_eq!(press_pane_cell(reviewed, 16.0, 110.0), (4, 0));
    assert_eq!(press_pane_cell(reviewed, 16.0, 120.0), (4, 0), "padding clamps to row 4");
}

#[test]
fn anchor_rebases_surviving_rows_and_cancels_lost_content() {
    // Eviction shifts a surviving anchor with its text; an evicted row or screen switch cancels.
    let mut parser = Parser::new(Grid::new(20, 3));
    parser.grid_mut().set_scrollback_limit(10);
    for line in 0..18 {
        parser.advance(format!("line {line:03}\r\n").as_bytes());
    }
    let tab = (TabId(1), 0);
    let (anchor, _) = capture_press(parser.grid(), 7, tab, 4, (1, 2), SelectMode::Cell).unwrap();
    assert_eq!(anchor.cell, (5, 2));
    assert_eq!(row_text(parser.grid(), 5), "line 011");
    for line in 18..21 {
        parser.advance(format!("line {line:03}\r\n").as_bytes());
    }
    let rebased = anchor.rebase(parser.grid()).expect("the anchored row survives");
    assert_eq!(rebased.cell, (2, 2));
    assert_eq!(row_text(parser.grid(), 2), "line 011");
    assert_eq!(rebased.rebase(parser.grid()), Some(rebased), "rebasing again is a no-op");
    for line in 21..25 {
        parser.advance(format!("line {line:03}\r\n").as_bytes());
    }
    assert_eq!(rebased.rebase(parser.grid()), None, "the anchored row was evicted");
    let (fresh, _) = capture_press(parser.grid(), 7, tab, 0, (0, 0), SelectMode::Cell).unwrap();
    parser.advance(b"\x1b[?1049h");
    assert_eq!(fresh.rebase(parser.grid()), None, "the screen changed under the drag");
}

#[test]
fn resize_cancels_an_anchor_whose_address_still_resolves() {
    // A same-size resize keeps the anchor; a real one cancels it even though the pressed
    // address still names a row, because that row can now hold other output.
    let mut parser = Parser::new(Grid::new(20, 3));
    parser.advance(b"one\r\ntwo\r\nthree");
    let tab = (TabId(1), 0);
    let (anchor, _) = capture_press(parser.grid(), 7, tab, 0, (0, 1), SelectMode::Cell).unwrap();
    parser.resize(20, 3);
    assert_eq!(anchor.rebase(parser.grid()), Some(anchor), "same bounded size");
    parser.resize(21, 3);
    assert!(parser.grid().row_at_abs(0).is_some(), "the pressed address still resolves");
    assert_eq!(anchor.rebase(parser.grid()), None, "a real resize cancels");
}

#[test]
fn press_outside_the_held_grid_captures_nothing() {
    // A row or column past the held grid, or a view top past its retained rows, binds no anchor
    // at any granularity, so a stale press cannot leave an unfingerprinted selection behind.
    let mut parser = Parser::new(Grid::new(20, 3));
    parser.advance(b"one\r\ntwo\r\nthree");
    let grid = parser.grid();
    let tab = (TabId(1), 0);
    for mode in [SelectMode::Cell, SelectMode::Word, SelectMode::Line] {
        assert!(capture_press(grid, 7, tab, 0, (3, 0), mode).is_none(), "row past {mode:?}");
        assert!(capture_press(grid, 7, tab, 0, (0, 20), mode).is_none(), "col past {mode:?}");
        assert!(capture_press(grid, 7, tab, 1, (2, 0), mode).is_none(), "view top past {mode:?}");
        assert!(capture_press(grid, 7, tab, 0, (2, 19), mode).is_some(), "last cell {mode:?}");
    }
}

#[test]
fn cross_pane_drags_keep_the_press_pane_text_for_every_split_and_click_count() {
    // Horizontal, vertical, and nested splits at cell, word, and line granularity, in main and
    // child windows: a drag into another pane selects only the press pane's text, bound to it.
    for topology in [Topology::Horizontal, Topology::Vertical, Topology::Nested] {
        for click_count in 1..=3u8 {
            for in_child in [false, true] {
                let (mut app, window) = app_window(in_child);
                let crossing = build(&mut app, window, topology);
                let case = format!("{topology:?} x{click_count} child={in_child}");
                let state = app.windows.get_mut(&window).unwrap();
                assert!(state.begin_local_selection(crossing.press, (0, 5), click_count));
                // Focus moving to another pane must not retarget the held gesture.
                let other = crossing.layouts.iter().find(|pane| pane.id != crossing.press);
                state.tab_states[0].active_pane = other.expect("another pane").id;
                let layouts = &crossing.layouts;
                let found = |_: &WindowState, id: u64| layouts.iter().find(|p| p.id == id).copied();
                let (x, y) = crossing.pointer;
                assert!(state.extend_local_selection_with(x, y, found), "{case}");
                let selection = state.selection.expect("drag selection");
                assert_eq!(selection.pane_id, Some(crossing.press), "{case}");
                let text = selection.as_text(state.panes[&crossing.press].parser.lock().grid());
                let name = crossing.press_name;
                let expected = if click_count == 3 {
                    format!("{name} one\n{name} two")
                } else {
                    // When: cell and word drags start inside `one`, at column 5.
                    format!("one\n{name} two")
                };
                assert_eq!(text, expected, "{case}");
            }
        }
    }
}

#[test]
fn resize_that_removes_the_pressed_cell_cancels_before_the_first_extension() {
    // A row or a column shrink after the press leaves the empty press selection in place, but
    // the anchored cell is gone, so the first move cancels instead of spanning unrelated text.
    for in_child in [false, true] {
        for (press, shrunk) in [((2, 3), (20, 2)), ((0, 15), (10, 3))] {
            let (mut app, window) = app_window(in_child);
            let (left, _) = split_window(&mut app, window);
            // The layout was drawn before the resize, so it still reports the old size.
            let drawn = [layout(left, 0.0, 0.0, 20, 3)];
            let state = app.windows.get_mut(&window).unwrap();
            assert!(state.begin_local_selection(left, press, 1));
            let pressed = state.selection;
            state.panes[&left].parser.lock().resize(shrunk.0, shrunk.1);
            let found = |_: &WindowState, id: u64| drawn.iter().find(|p| p.id == id).copied();
            assert!(!state.extend_local_selection_with(195.0, 50.0, found), "{shrunk:?}");
            assert_eq!(state.pointer_gesture, None, "a removed anchor cell cancels {shrunk:?}");
            assert_eq!(state.selection, pressed);
        }
    }
}

#[test]
fn motion_past_a_shrunk_grid_clamps_to_its_last_cell() {
    // A layout drawn before a resize can name rows the grid no longer has; when the resize
    // precedes the press, the drag end is clamped to the held grid's addressable cells.
    for in_child in [false, true] {
        let (mut app, window) = app_window(in_child);
        let (left, _) = split_window(&mut app, window);
        let drawn = [layout(left, 0.0, 0.0, 20, 3)];
        let state = app.windows.get_mut(&window).unwrap();
        state.panes[&left].parser.lock().resize(20, 2);
        assert!(state.begin_local_selection(left, (0, 0), 1));
        let found = |_: &WindowState, id: u64| drawn.iter().find(|p| p.id == id).copied();
        assert!(state.extend_local_selection_with(195.0, 50.0, found));
        let selection = state.selection.expect("clamped drag");
        assert_eq!(selection.normalized().1, (1, 19), "row 2 of the old layout clamps to row 1");
    }
}

#[test]
fn resize_before_the_first_move_cancels_even_when_the_pressed_address_resolves() {
    // Shrinking the press pane and then printing, or shrinking and growing back, can leave the
    // pressed address valid while it names fresh output or a blank row. History, eviction, and
    // screen identity are unchanged, so only the size generation shows the resize.
    for in_child in [false, true] {
        for grow_back in [false, true] {
            let (mut app, window) = app_window(in_child);
            let pane = app.windows[&window].tab_states[0].active_pane;
            let parser = app.windows[&window].panes[&pane].parser.clone();
            parser.lock().grid_mut().set_scrollback_limit(100);
            fill(&app.windows[&window], pane, 20, 5, "row 0\r\nrow 1\r\nrow 2\r\nrow 3\r\nrow 4");
            let identity = |grid: &Grid| (grid.scrollback_evicted(), grid.screen_epoch());
            let before = identity(parser.lock().grid());
            let drawn = [layout(pane, 0.0, 0.0, 20, 5)];
            let state = app.windows.get_mut(&window).unwrap();
            assert!(state.begin_local_selection(pane, (4, 2), 1));
            let pressed = state.selection;
            {
                let mut parser = parser.lock();
                parser.resize(20, 3);
                if grow_back {
                    parser.resize(20, 5);
                } else {
                    // When: `grow_back` is false, output scrolls fresh rows under the address.
                    parser.advance(b"\r\nfresh 5\r\nfresh 6");
                }
                assert!(parser.grid().row_at_abs(4).is_some(), "the address resolves again");
                assert_eq!(identity(parser.grid()), before, "history and screen are unchanged");
                assert_ne!(row_text(parser.grid(), 4), "row 4");
            }
            let found = |_: &WindowState, id: u64| drawn.iter().find(|p| p.id == id).copied();
            let case = format!("grow_back={grow_back} child={in_child}");
            assert!(!state.extend_local_selection_with(15.0, 10.0, found), "{case}");
            assert_eq!(state.pointer_gesture, None, "a real resize cancels {case}");
            assert_eq!(state.selection, pressed, "{case}");
        }
    }
}

#[test]
fn press_through_a_stale_layout_keeps_the_previous_selection() {
    // A keyboard split resize can shrink a pane before its next frame, so a press mapped through
    // the drawn layout names a row or column the grid lost. At every click count it binds no
    // gesture and keeps the previous selection instead of an unfingerprinted one for Copy.
    for in_child in [false, true] {
        for (shrunk, pointer) in [((20, 2), (55.0, 50.0)), ((10, 3), (155.0, 10.0))] {
            for click_count in 1..=3u8 {
                let (mut app, window) = app_window(in_child);
                let (left, _) = split_window(&mut app, window);
                let drawn = layout(left, 0.0, 0.0, 20, 3);
                let state = app.windows.get_mut(&window).unwrap();
                assert!(state.begin_local_selection(left, (0, 0), 1));
                assert!(state.extend_local_selection_to_cell((1, 4)));
                // Releasing the button ends the gesture; the finished selection remains.
                state.pointer_gesture = None;
                let previous = state.selection;
                state.panes[&left].parser.lock().resize(shrunk.0, shrunk.1);
                let cell = press_pane_cell(drawn, pointer.0, pointer.1);
                let case = format!("{shrunk:?} {cell:?} x{click_count} child={in_child}");
                assert!(!state.begin_local_selection(left, cell, click_count), "{case}");
                assert_eq!(state.pointer_gesture, None, "{case}");
                assert_eq!(state.selection, previous, "{case}");
            }
        }
    }
}

#[test]
fn busy_parser_at_press_installs_no_gesture_and_keeps_the_selection() {
    // A contended press binds nothing, keeps a valid selection, and later motion invents nothing.
    for in_child in [false, true] {
        let (mut app, window) = app_window(in_child);
        let (left, _) = split_window(&mut app, window);
        let parser = app.windows[&window].panes[&left].parser.clone();
        let state = app.windows.get_mut(&window).unwrap();
        assert!(state.begin_local_selection(left, (0, 0), 1));
        let existing = state.selection;
        // The press path latches an anchor-less local gesture before the anchor is bound.
        let latched = state.pointer_gesture.expect("local gesture");
        state.pointer_gesture = Some(PointerGesture { anchor: None, ..latched });
        let held = parser.lock();
        assert!(!state.begin_local_selection(left, (1, 3), 1));
        assert_eq!(state.pointer_gesture, None, "no local gesture from a fabricated anchor");
        assert_eq!(state.selection, existing);
        drop(held);
        assert!(!state.extend_local_selection_to_cell((1, 5)));
        assert_eq!(state.selection, existing);
    }
}

#[test]
fn inactive_pane_local_press_commits_focus_only_after_snapshot_admission() {
    // Main and child local presses must admit the target snapshot before replacing focus or text.
    // Contention starts after the earlier mouse-profile read; that blocking read is not under test.
    for in_child in [false, true] {
        let (mut app, window) = app_window(in_child);
        let (left, right) = split_window(&mut app, window);
        let state = app.windows.get_mut(&window).unwrap();
        assert_eq!(state.tab_states[0].active_pane, left);
        assert!(state.begin_local_selection(left, (0, 0), 3));
        state.pointer_gesture = None;
        let previous = state.selection.expect("completed left-pane selection");
        let left_parser = state.panes[&left].parser.clone();
        assert_eq!(previous.as_text(left_parser.lock().grid()), "left one");

        let right_parser = state.panes[&right].parser.clone();
        let profile = {
            let parser = right_parser.lock();
            crate::app::window_event::parser_mouse_profile(&parser)
        };
        let cell = PointerCell { pane_id: right, row: 0, col: 0 };
        assert!(state.begin_pointer_press(cell, profile.0, profile.1).is_none());
        // Model the parser becoming busy between profile lookup and the local snapshot attempt.
        let held = right_parser.lock();
        let admitted = state.begin_local_selection(right, (0, 0), 3);
        drop(held);
        assert!(!admitted, "contended snapshot must refuse child={in_child}");
        assert_eq!(state.tab_states[0].active_pane, left, "failed press preserves focus");
        assert_eq!(state.selection, Some(previous), "failed press preserves selection identity");
        assert_eq!(state.pointer_gesture, None, "failed press installs no local gesture");
        assert_eq!(state.selection.unwrap().as_text(left_parser.lock().grid()), "left one");
        assert!(!state.extend_local_selection_to_cell((1, 3)));
        assert_eq!(state.selection, Some(previous), "later motion cannot invent the refused press");

        // The same transaction must focus the target on success without clearing its new selection.
        assert!(state.begin_pointer_press(cell, profile.0, profile.1).is_none());
        assert!(state.begin_local_selection(right, (0, 0), 3));
        assert_eq!(
            state.tab_states[0].active_pane, right,
            "admitted local press must commit target focus child={in_child}"
        );
        let selected = state.selection.expect("admitted right-pane selection");
        assert_eq!(selected.pane_id, Some(right));
        assert_eq!(selected.as_text(right_parser.lock().grid()), "right one");
        let gesture = state.pointer_gesture.expect("admitted local gesture");
        assert_eq!(gesture.press_pane, right);
        assert_eq!(gesture.anchor.unwrap().pane_id, right);
    }
}

#[test]
fn grid_press_routes_do_not_focus_before_ownership_admission() {
    // Structural wiring complements the transaction test: neither native event route may clear selection before admission.
    let main = include_str!("window_event.rs")
        .split("let clicked_pane = pixel_target")
        .nth(1)
        .expect("main rendered grid press");
    let child = include_str!("child_window.rs")
        .split("let pointer_cell = pixel_target.and_then")
        .nth(1)
        .expect("child rendered grid press");
    for (name, source, decision) in
        [("main", main, "if opened {"), ("child", child, "if let Some(bytes) = terminal_press {")]
    {
        let first_focus = source.find("begin_pointer_pane_focus_change").unwrap();
        let admitted = source.find(decision).unwrap();
        assert!(first_focus > admitted, "{name} mutates focus before press ownership admission");
        let local = source.split("begin_local_selection(").nth(1).expect("local transaction");
        let release = local.find("ElementState::Released").expect("release event arm");
        assert!(
            !local[..release].contains("begin_pointer_pane_focus_change"),
            "{name} clears the selection after committing the local transaction"
        );
    }
}

#[test]
fn local_press_rejects_a_zoomed_away_pane_without_changing_focus() {
    // A stale rendered hit cannot install a selection in a pane that zoom hides.
    for in_child in [false, true] {
        let (mut app, window) = app_window(in_child);
        let (left, right) = split_window(&mut app, window);
        let state = app.windows.get_mut(&window).unwrap();
        assert!(state.begin_local_selection(left, (0, 0), 3));
        let previous = state.selection;
        state.tab_states[0].tree.toggle_zoom(left);
        assert!(!state.begin_local_selection(right, (0, 0), 3));
        assert_eq!(state.tab_states[0].active_pane, left);
        assert_eq!(state.selection, previous);
        assert!(state.pointer_gesture.is_none());
    }
}

#[test]
fn eviction_during_a_drag_rebases_or_cancels_the_gesture() {
    // Output that evicts rows mid-drag rebases a surviving anchor and cancels a lost one.
    for in_child in [false, true] {
        let (mut app, window) = app_window(in_child);
        let (left, _) = split_window(&mut app, window);
        let parser = app.windows[&window].panes[&left].parser.clone();
        {
            let mut parser = parser.lock();
            parser.grid_mut().set_scrollback_limit(10);
            parser.advance(b"\r\n");
            for line in 0..16 {
                parser.advance(format!("line {line:03}\r\n").as_bytes());
            }
        }
        let pressed_text = row_text(parser.lock().grid(), 10);
        let state = app.windows.get_mut(&window).unwrap();
        assert!(state.begin_local_selection(left, (0, 0), 1));
        for line in 16..18 {
            parser.lock().advance(format!("line {line:03}\r\n").as_bytes());
        }
        assert!(state.extend_local_selection_to_cell((1, 3)));
        let selection = state.selection.expect("rebased selection");
        let text = selection.as_text(parser.lock().grid());
        assert!(text.starts_with(&pressed_text), "{text:?} keeps {pressed_text:?}");
        for line in 18..27 {
            parser.lock().advance(format!("line {line:03}\r\n").as_bytes());
        }
        assert!(!state.extend_local_selection_to_cell((1, 5)));
        assert_eq!(state.pointer_gesture, None, "the anchored row was evicted");
        assert_eq!(state.selection, Some(selection));
    }
}

#[test]
fn production_motion_cancels_a_gesture_whose_pane_is_gone_without_a_layout() {
    // Once frames stop drawing a closed press pane there is no layout for it; the pixel
    // entrypoint checks ownership first, so the stale gesture is cancelled, not skipped.
    for in_child in [false, true] {
        let (mut app, window) = app_window(in_child);
        let (left, _) = split_window(&mut app, window);
        let state = app.windows.get_mut(&window).unwrap();
        assert!(state.begin_local_selection(left, (0, 0), 1));
        assert!(!state.extend_local_selection(10.0, 10.0), "no renderer has drawn yet");
        assert!(state.pointer_gesture.is_some(), "a missing layout alone only skips the move");
        drop(state.panes.remove(&left));
        assert!(!state.extend_local_selection(10.0, 10.0));
        assert_eq!(state.pointer_gesture, None, "a gone press pane cancels without a layout");
    }
}

#[test]
fn tab_round_trip_without_motion_cancels_the_held_gesture() {
    // Switching to another tab and back while the button is held, with no move in between,
    // ends the drag: the tab bar's activation count changed even though the press tab is active.
    for in_child in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, left) = two_tab_window(&mut app, in_child);
        assert!(app.windows.get_mut(&window).unwrap().begin_local_selection(left, (0, 0), 1));
        activate(&mut app, window, 1);
        activate(&mut app, window, 0);
        let state = app.windows.get_mut(&window).unwrap();
        assert!(!state.extend_local_selection(10.0, 10.0));
        assert_eq!(state.pointer_gesture, None, "A to B and back to A cancels the gesture");
    }
}

#[test]
fn closing_the_pressed_tab_while_the_button_is_held_cancels_the_drag() {
    // The production close path removes the press pane's tab, and its topology cleanup clears
    // that pane's selection; the next move cancels the drag instead of retargeting a survivor.
    for in_child in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let (window, left) = two_tab_window(&mut app, in_child);
        assert!(app.windows.get_mut(&window).unwrap().begin_local_selection(left, (0, 0), 1));
        if in_child {
            assert!(app.__test_invoke_close_tab_at_in_child(window, 0));
        } else {
            // When: `in_child` is false, the main window closes the tab through `close_tab_at`.
            app.close_tab_at(0);
        }
        let state = app.windows.get_mut(&window).unwrap();
        assert!(!state.extend_local_selection(10.0, 10.0));
        assert_eq!(state.pointer_gesture, None, "closing the press tab cancels the gesture");
        let selected_pane = state.selection.and_then(|selection| selection.pane_id);
        assert_eq!(selected_pane, None, "the closed pane's selection is cleared");
    }
}
