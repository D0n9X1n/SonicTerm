use super::*;
use crate::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// Parser with a 20×3 screen and `limit` history rows.
fn bounded_parser(limit: usize) -> Parser {
    let mut parser = Parser::new(Grid::new(20, 3));
    parser.grid_mut().set_scrollback_limit(limit);
    parser
}

/// Print one `line NNN` row per number, each ending in CRLF.
fn print_lines(parser: &mut Parser, lines: std::ops::Range<u32>) {
    for line in lines {
        parser.advance(format!("line {line:03}\r\n").as_bytes());
    }
}

/// Text of absolute row `abs`, without trailing blank cells.
fn text_at(grid: &Grid, abs: u64) -> String {
    let text = grid.row_at_abs(abs).map(|row| row.iter().map(|cell| cell.ch).collect::<String>());
    text.unwrap_or_default().trim_end_matches([' ', '\0']).to_owned()
}

/// A fresh anchor pinned to `row`, measured against `parser`'s current grid.
fn pinned(parser: &Parser, row: u64) -> (ViewportAnchor, Option<u64>) {
    let mut anchor = ViewportAnchor::default();
    let mut field = None;
    anchor.set(&mut field, Some(row), ViewportBaseline::of(parser.grid()));
    (anchor, field)
}

/// Reconcile `anchor` against `parser`'s current grid.
fn resolve_on(
    anchor: &mut ViewportAnchor,
    field: &mut Option<u64>,
    parser: &Parser,
) -> Option<u64> {
    anchor.reconcile(field, ViewportBaseline::of(parser.grid()))
}

/// Give `pane_id` a 20×3 screen, `limit` history rows, and `line NNN` output.
fn seed_history(app: &App, pane_id: u64, limit: usize, lines: std::ops::Range<u32>) {
    let pane = app.pane_by_id(pane_id).expect("seeded pane");
    let mut parser = pane.parser.lock();
    parser.resize(20, 3);
    parser.grid_mut().set_scrollback_limit(limit);
    print_lines(&mut parser, lines);
}

/// Text of absolute row `abs` in `pane_id`'s grid.
fn pane_text_at(app: &App, pane_id: u64, abs: u64) -> String {
    let parser = app.pane_by_id(pane_id).expect("seeded pane").parser.lock();
    text_at(parser.grid(), abs)
}

#[test]
fn full_history_append_keeps_the_pinned_text_on_top() {
    // At the history cap one appended line evicts the oldest row; the view keeps its text.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    assert_eq!(parser.grid().scrollback_evicted(), 6);
    let (mut anchor, mut field) = pinned(&parser, 4);
    let pinned_text = text_at(parser.grid(), 4);
    assert_eq!(pinned_text, "line 010");
    print_lines(&mut parser, 18..19);
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), Some(3));
    assert_eq!(field, Some(3), "the compatibility field carries the rebased row");
    assert_eq!(text_at(parser.grid(), 3), pinned_text);
}

#[test]
fn one_row_evictions_between_frames_shift_the_pin_by_their_total() {
    // Several one-row FIFO evictions between two frames move the pin by their total, not by one.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut anchor, mut field) = pinned(&parser, 7);
    let pinned_text = text_at(parser.grid(), 7);
    print_lines(&mut parser, 18..23);
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), Some(2));
    assert_eq!(text_at(parser.grid(), 2), pinned_text);
}

#[test]
fn cell_budget_trim_rebases_the_pin_by_the_rows_it_drains_at_once() {
    // A resize that shrinks the history cell budget drains many rows in one grid operation; the
    // pin rebases by exactly that delta and keeps its text. Blank rows stay compressed.
    let mut parser = bounded_parser(2_000);
    for line in 0..1_200u32 {
        let text = if line == 1_100 { "pinned history row" } else { "" };
        parser.advance(format!("{text}\r\n").as_bytes());
    }
    assert_eq!(parser.grid().scrollback_evicted(), 0);
    assert_eq!(text_at(parser.grid(), 1_100), "pinned history row");
    let (mut anchor, mut field) = pinned(&parser, 1_100);
    parser.resize(1_000, 3);
    let delta = parser.grid().scrollback_evicted();
    assert!(delta > 1, "one resize drained {delta} rows at once");
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), Some(1_100 - delta));
    assert_eq!(text_at(parser.grid(), 1_100 - delta), "pinned history row");
}

#[test]
fn evicted_pin_clamps_to_the_oldest_retained_row() {
    // A pinned row that left history clamps to the oldest retained row and stays there.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut anchor, mut field) = pinned(&parser, 1);
    print_lines(&mut parser, 18..23);
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), Some(0));
    assert_eq!(text_at(parser.grid(), 0), "line 011");
    print_lines(&mut parser, 23..25);
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), Some(0));
}

#[test]
fn repeated_resolution_never_double_subtracts() {
    // Resolving again against an unchanged eviction count is a no-op, including at row zero.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut anchor, mut field) = pinned(&parser, 6);
    let pinned_text = text_at(parser.grid(), 6);
    print_lines(&mut parser, 18..21);
    let now = ViewportBaseline::of(parser.grid());
    assert_eq!(anchor.resolve(field, now), Some(3));
    assert_eq!(anchor.resolve(field, now), Some(3), "a preview commits nothing");
    assert_eq!(anchor.reconcile(&mut field, now), Some(3));
    assert_eq!(anchor.reconcile(&mut field, now), Some(3));
    assert_eq!(text_at(parser.grid(), 3), pinned_text);
    print_lines(&mut parser, 21..24);
    let now = ViewportBaseline::of(parser.grid());
    assert_eq!(anchor.reconcile(&mut field, now), Some(0));
    assert_eq!(anchor.reconcile(&mut field, now), Some(0), "row zero is a real pin");
    assert_eq!(text_at(parser.grid(), 0), pinned_text);
}

#[test]
fn lowered_history_limit_rebases_or_clamps_the_pin() {
    // Trimming history keeps a surviving pin's text and clamps one whose row was trimmed.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut kept, mut kept_field) = pinned(&parser, 8);
    let (mut lost, mut lost_field) = pinned(&parser, 2);
    let kept_text = text_at(parser.grid(), 8);
    parser.grid_mut().set_scrollback_limit(4);
    assert_eq!(resolve_on(&mut kept, &mut kept_field, &parser), Some(2));
    assert_eq!(text_at(parser.grid(), 2), kept_text);
    assert_eq!(resolve_on(&mut lost, &mut lost_field, &parser), Some(0));
}

#[test]
fn erased_history_follows_the_tail_and_keeps_a_screen_pin() {
    // CSI 3 J erases history: a history pin follows the tail, a live-screen pin keeps its row.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut history, mut history_field) = pinned(&parser, 4);
    let (mut screen, mut screen_field) = pinned(&parser, 10);
    let screen_text = text_at(parser.grid(), 10);
    parser.advance(b"\x1b[3J");
    assert_eq!(parser.grid().scrollback_len(), 0);
    assert_eq!(resolve_on(&mut history, &mut history_field, &parser), None);
    assert_eq!(resolve_on(&mut screen, &mut screen_field, &parser), Some(0));
    assert_eq!(text_at(parser.grid(), 0), screen_text);
}

#[test]
fn alternate_round_trip_rebases_the_suspended_primary_pin() {
    // The alternate screen suspends the primary pin; returning rebases it by the rows trimmed.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut anchor, mut field) = pinned(&parser, 6);
    let pinned_text = text_at(parser.grid(), 6);
    parser.advance(b"\x1b[?1049h");
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), None, "alternate shows its tail");
    parser.advance(b"alt output\r\n");
    parser.grid_mut().set_scrollback_limit(7);
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), None);
    parser.advance(b"\x1b[?1049l");
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), Some(3));
    assert_eq!(text_at(parser.grid(), 3), pinned_text);
}

#[test]
fn unobserved_alternate_round_trip_still_rebases_the_pin() {
    // A round trip no frame observed keeps primary history continuous, so the full delta applies.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut anchor, mut field) = pinned(&parser, 6);
    let pinned_text = text_at(parser.grid(), 6);
    parser.advance(b"\x1b[?1049h");
    parser.grid_mut().set_scrollback_limit(7);
    parser.advance(b"\x1b[?1049l");
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), Some(3));
    assert_eq!(text_at(parser.grid(), 3), pinned_text);
}

#[test]
fn replaced_alternate_screen_drops_its_pin_but_keeps_the_primary() {
    // A different alternate incarnation resets its own pin; the suspended primary pin survives.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut anchor, mut field) = pinned(&parser, 6);
    let pinned_text = text_at(parser.grid(), 6);
    parser.advance(b"\x1b[?1049h");
    anchor.set(&mut field, Some(0), ViewportBaseline::of(parser.grid()));
    assert_eq!(field, Some(0));
    parser.advance(b"\x1b[?1049l\x1b[?1049h");
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), None);
    parser.advance(b"\x1b[?1049l");
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), Some(6));
    assert_eq!(text_at(parser.grid(), 6), pinned_text);
}

#[test]
fn release_follows_the_tail_on_every_screen() {
    // Submitting input returns the whole pane to live output, including a suspended primary pin.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut anchor, mut field) = pinned(&parser, 6);
    parser.advance(b"\x1b[?1049h");
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), None);
    assert!(!anchor.release(&mut field), "the alternate view already shows its tail");
    parser.advance(b"\x1b[?1049l");
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), None, "no primary pin returns");
    let (mut anchor, mut field) = pinned(&parser, 6);
    assert!(anchor.release(&mut field));
    assert_eq!(field, None);
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), None);
}

#[test]
fn external_assignment_is_adopted_at_the_current_count() {
    // A legacy field write pins at the eviction count it is first seen at, then rebases normally.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut anchor, mut field) = pinned(&parser, 2);
    assert_eq!(field, Some(2));
    print_lines(&mut parser, 18..20);
    field = Some(5);
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), Some(5), "adopted, not rebased");
    let adopted_text = text_at(parser.grid(), 5);
    print_lines(&mut parser, 20..22);
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), Some(3));
    assert_eq!(text_at(parser.grid(), 3), adopted_text);
    field = None;
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), None, "None follows the tail");
    print_lines(&mut parser, 22..23);
    assert_eq!(resolve_on(&mut anchor, &mut field, &parser), None);
}

#[test]
fn identical_assignment_is_not_a_repin_but_the_setter_is() {
    // Assigning the value the field already holds is unobservable; the explicit setter repins.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut assigned, mut assigned_field) = pinned(&parser, 5);
    let (mut repinned, mut repinned_field) = pinned(&parser, 5);
    print_lines(&mut parser, 18..20);
    assert_eq!(assigned_field, Some(5));
    assigned_field = Some(5);
    let now = ViewportBaseline::of(parser.grid());
    repinned.set(&mut repinned_field, Some(5), now);
    assert_eq!(assigned.reconcile(&mut assigned_field, now), Some(3), "still the first row");
    assert_eq!(repinned.reconcile(&mut repinned_field, now), Some(5), "repinned at the count");
}

#[test]
fn replaced_grid_resets_the_anchor() {
    // A backwards eviction count means a different grid; no recorded row rebases onto it.
    let mut parser = bounded_parser(10);
    print_lines(&mut parser, 0..18);
    let (mut anchor, mut field) = pinned(&parser, 4);
    let mut fresh = bounded_parser(10);
    print_lines(&mut fresh, 0..12);
    assert!(fresh.grid().scrollback_evicted() < parser.grid().scrollback_evicted());
    assert_eq!(resolve_on(&mut anchor, &mut field, &fresh), None);
}

#[test]
fn inactive_tab_output_keeps_the_pinned_row() {
    // Output into a background tab rebases its pane, so reactivating the tab shows the same text.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("reading");
    seed_history(&app, pane_id, 10, 0..18);
    app.scroll_pane(pane_id, -6);
    let pinned_text = pane_text_at(&app, pane_id, 4);
    app.__test_seed_tab("busy");
    assert_eq!(app.main().unwrap().tabs.active_index(), 1);
    let parser = app.pane_by_id(pane_id).unwrap().parser.clone();
    print_lines(&mut parser.lock(), 18..21);
    app.__test_invoke_activate_main_tab(0);
    assert_eq!(app.main().unwrap().tabs.active_index(), 0);
    let window = app.main().expect("main window");
    let (view_top, ..) = window.viewport_row_selection_state(0).expect("active pane");
    assert_eq!(view_top, 1);
    assert_eq!(pane_text_at(&app, pane_id, view_top), pinned_text);
}

#[test]
fn transferred_tab_keeps_its_pinned_row_after_activation() {
    // A real tab transfer carries the pane's anchor; after activating the destination tab and
    // more output, the destination window's hit-test top shows the pinned history text.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("moved");
    app.__test_seed_tab("remaining");
    let child = app.__test_seed_child_window(&["destination"]);
    app.windows.get_mut(&child).unwrap().test_pane_viewport =
        Some((sonicterm_ui::pane::Rect { x: 0.0, y: 0.0, w: 200.0, h: 60.0 }, 10.0, 20.0));
    seed_history(&app, pane_id, 10, 0..18);
    app.scroll_pane(pane_id, -6);
    let pinned_text = pane_text_at(&app, pane_id, 4);
    app.transfer_tab(None, 0, Some(child), 1).expect("tab transfer");
    app.__test_invoke_activate_tab_in_child(child, 1);
    assert_eq!(app.windows[&child].tabs.active_index(), 1);
    assert_eq!(app.windows[&child].tab_states[1].active_pane, pane_id);
    let parser = app.pane_by_id(pane_id).unwrap().parser.clone();
    // Whatever size the destination gives the pane, print until two more rows are evicted.
    let start = parser.lock().grid().scrollback_evicted();
    let mut line = 18u32;
    while parser.lock().grid().scrollback_evicted() < start + 2 && line < 80 {
        parser.lock().advance(format!("line {line:03}\r\n").as_bytes());
        line += 1;
    }
    let evicted = parser.lock().grid().scrollback_evicted();
    assert_eq!(evicted, 8, "the pin was measured at 6 evictions and output evicted two more");
    let destination = &app.windows[&child];
    let (view_top, ..) = destination.viewport_row_selection_state(0).expect("active pane");
    assert_eq!(view_top, 2);
    assert_eq!(pane_text_at(&app, pane_id, view_top), pinned_text);
}

#[test]
fn one_reconcile_pass_feeds_frame_selection_search_and_scrollbar_one_top() {
    // One held-frame reconcile returns the per-pane and window frame projections; hit-testing,
    // a search committed afterwards, and scrollbar paging read the same rebased row.
    for child in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        let window = if child {
            app.__test_seed_child_window(&["child"])
        } else {
            app.main_window_id.unwrap()
        };
        let pane_id = app.windows[&window].tab_states[0].active_pane;
        let parser = app.windows[&window].panes[&pane_id].parser.clone();
        {
            let mut parser = parser.lock();
            parser.resize(20, 3);
            parser.grid_mut().set_scrollback_limit(10);
            for line in 0..18u32 {
                let word = if line % 3 == 0 { "needle" } else { "other" };
                parser.advance(format!("{word} {line:03}\r\n").as_bytes());
            }
        }
        let pane = app.windows.get_mut(&window).unwrap().panes.get_mut(&pane_id).unwrap();
        pane.pin_viewport_top(Some(4));
        for line in 18..21u32 {
            parser.lock().advance(format!("other {line:03}\r\n").as_bytes());
        }
        let frame = {
            let held = parser.lock();
            let state = app.windows.get_mut(&window).unwrap();
            reconcile_held_viewports(&mut state.panes, [(pane_id, &*held)], pane_id)
        };
        assert_eq!(frame.of(pane_id), Some(1), "per-pane frame projection");
        assert_eq!(frame.active, Some(1), "window frame projection");
        assert_eq!(app.windows[&window].panes[&pane_id].viewport_top_abs, Some(1));
        let state = &app.windows[&window];
        let (view_top, ..) = state.viewport_row_selection_state(0).expect("active pane");
        assert_eq!(Some(view_top), frame.active, "hit-testing reads the frame's top");
        if child {
            assert!(app.open_search_in_child(window));
        } else {
            app.open_search();
        }
        assert!(app.search_handle_ime_commit(window, "needle"));
        let search = app.windows[&window].tab_states[0].search.as_ref().expect("search");
        let current = search.current.map(|index| search.matches[index].row);
        // Needles sit at rows 0, 3 and 6; the rebased view shows rows 1-3, a stale one 4-6.
        assert_eq!(current, Some(3), "search anchors in the rebased viewport");
        if child {
            app.scrollbar_track_page_in_child(window, false);
        } else {
            app.scrollbar_track_page(false);
        }
        // A page up from row 1 reaches row 0; one from the stale row 4 would stop at row 1.
        assert_eq!(app.windows[&window].panes[&pane_id].viewport_top_abs, Some(0));
    }
}

#[test]
fn config_reload_trim_rebases_the_pinned_row() {
    // Lowering `terminal.scrollback` trims history and rebases the view in the same reload.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    seed_history(&app, pane_id, 10, 0..18);
    app.scroll_pane(pane_id, -2);
    let pinned_text = pane_text_at(&app, pane_id, 8);
    let mut reloaded = app.config.clone();
    reloaded.terminal.scrollback = 4;
    app.apply_new_config(reloaded);
    assert_eq!(app.pane_by_id(pane_id).unwrap().viewport_top_abs, Some(2));
    assert_eq!(pane_text_at(&app, pane_id, 2), pinned_text);
}

#[test]
fn pin_viewport_top_repins_the_same_index() {
    // The public setter measures an unchanged index against the current count; assignment cannot.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    seed_history(&app, pane_id, 10, 0..18);
    app.scroll_pane(pane_id, -6);
    let parser = app.pane_by_id(pane_id).unwrap().parser.clone();
    print_lines(&mut parser.lock(), 18..20);
    let pane = app.main_mut().unwrap().panes.get_mut(&pane_id).unwrap();
    assert_eq!(pane.viewport_top_abs, Some(4), "not reconciled yet");
    pane.pin_viewport_top(Some(4));
    let repinned_text = text_at(parser.lock().grid(), 4);
    print_lines(&mut parser.lock(), 20..21);
    assert_eq!(pane.reconcile_viewport(parser.lock().grid()), Some(3));
    assert_eq!(text_at(parser.lock().grid(), 3), repinned_text);
}
