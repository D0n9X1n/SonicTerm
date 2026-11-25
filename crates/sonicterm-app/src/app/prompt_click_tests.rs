//! Pins prompt click-to-move: a plain left click inside an OSC 133 prompt becomes a click report
//! or cursor keys for the shell, written only to the press pane and only when the press identity,
//! the semantic anchor, the layout and the input owner still hold at release.

use super::*;
use crate::app::selection_gesture::PointerCell;
use crate::app::window_event::{pointer_route_bytes, take_pointer_release, PointerReportKind};
use sonicterm_cfg::config::ClickToMove;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;
use winit::keyboard::ModifiersState;

/// An App with one seeded 80x24 pane fed `bytes`, its modes published, and the write log on.
fn app_with(bytes: &[u8]) -> (App, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    app.__test_enable_pty_write_log();
    feed(&app, pane_id, bytes);
    (app, pane_id)
}

/// Advance `pane_id`'s parser with `bytes` and publish its modes, as the VT worker does.
fn feed(app: &App, pane_id: u64, bytes: &[u8]) {
    let pane = &app.main().unwrap().panes[&pane_id];
    let mut parser = pane.parser.lock();
    parser.advance(bytes);
    pane.__test_publish_input_modes(&parser);
}

/// Press the left button at viewport `cell` of `pane_id` with `modifiers`, as both pointer
/// handlers do after chrome, links and the scrollbar miss: a terminal-owned press writes its
/// report, any other press binds a local selection.
fn press(app: &mut App, pane_id: u64, cell: (u16, u16), modifiers: ModifiersState) {
    let window = app.main_mut().unwrap();
    window.modifiers = modifiers;
    let modes = window.panes[&pane_id].pointer_modes();
    let pointer = PointerCell { pane_id, row: cell.0, col: cell.1, pixel: (0, 0) };
    if let Some(bytes) =
        window.begin_pointer_press(pointer, modes.tracking(), modes.format(), modes.is_alt())
    {
        app.write_to_pane(pane_id, bytes, PtyInputSource::PointerButton);
        return;
    }
    let count = window.register_click(cell.0, cell.1);
    window.begin_local_selection(pane_id, cell, count);
}

/// Release the left button as both pointer handlers do: the terminal release report, then the
/// empty-selection clear, then the prompt click.
fn release(app: &mut App) {
    let window = app.main_mut().unwrap();
    let gesture = window.pointer_gesture;
    let modifiers = window.modifiers;
    let route = take_pointer_release(&mut window.pointer_gesture, modifiers);
    if window.selection.as_ref().is_some_and(|selection| selection.is_empty()) {
        window.selection = None;
    }
    if let Some((pane_id, bytes)) =
        route.and_then(|route| pointer_route_bytes(route, PointerReportKind::LeftRelease))
    {
        app.write_to_pane(pane_id, bytes, PtyInputSource::PointerButton);
    }
    let window_id = app.registry.main_window_id.unwrap();
    app.try_prompt_click(window_id, gesture);
}

/// One plain single click at `cell` with `modifiers`; returns and clears the write log. The click
/// streak is reset first, so repeated clicks on one cell stay single clicks.
fn click_with(app: &mut App, pane_id: u64, cell: (u16, u16), modifiers: ModifiersState) -> Vec<u8> {
    app.main_mut().unwrap().last_click_time = None;
    press(app, pane_id, cell, modifiers);
    release(app);
    take_log(app)
}

/// One unmodified click at `cell`.
fn click(app: &mut App, pane_id: u64, cell: (u16, u16)) -> Vec<u8> {
    click_with(app, pane_id, cell, ModifiersState::empty())
}

/// Every byte written since the last call, concatenated, and the log cleared.
fn take_log(app: &App) -> Vec<u8> {
    let bytes = app.__test_pty_write_log().into_iter().flat_map(|(_, bytes)| bytes).collect();
    app.testing.test_pty_writes.lock().clear();
    bytes
}

/// OSC 133 `kind` with `options`, BEL-terminated.
fn mark(kind: &str, options: &str) -> String {
    if options.is_empty() {
        format!("\x1b]133;{kind}\x07")
    } else {
        format!("\x1b]133;{kind};{options}\x07")
    }
}

/// A prompt `$ ` with A `options`, then B and typed `input`; the cursor ends after the input.
fn prompt_with_input(options: &str, input: &str) -> String {
    format!("{}$ {}{input}", mark("A", options), mark("B", ""))
}

/// `count` presses of the plain (DECCKM off) arrow `letter`.
fn arrows(letter: char, count: usize) -> Vec<u8> {
    format!("\x1b[{letter}").repeat(count).into_bytes()
}

/// `click_events=1` sends one SGR press at the viewport row (absolute) and `click_events=2` one
/// at the row relative to the prompt; neither sends a release.
#[test]
fn abs_and_rel_events_press_only() {
    let screen = format!("out\r\n{}", prompt_with_input("click_events=1", "ls"));
    let (mut app, pane_id) = app_with(screen.as_bytes());
    assert_eq!(click(&mut app, pane_id, (1, 4)), b"\x1b[<0;5;2M");
    let screen = format!("out\r\n{}", prompt_with_input("click_events=2", "ls"));
    let (mut app, pane_id) = app_with(screen.as_bytes());
    assert_eq!(click(&mut app, pane_id, (1, 4)), b"\x1b[<0;5;1M");
}

/// In relative mode a secondary prompt at or above the click anchors the row; a click above it
/// anchors on the primary prompt.
#[test]
fn rel_anchor_secondary_prompt() {
    let screen = format!(
        "{}$ {}if x\r\n{}> {}y",
        mark("A", "click_events=2"),
        mark("B", ""),
        mark("P", "k=s"),
        mark("B", "")
    );
    let (mut app, pane_id) = app_with(screen.as_bytes());
    assert_eq!(click(&mut app, pane_id, (1, 2)), b"\x1b[<0;3;1M", "on the secondary prompt");
    assert_eq!(click(&mut app, pane_id, (0, 3)), b"\x1b[<0;4;1M", "above it: the primary prompt");
}

/// A click below the input is still reported (the shell validates it); a click above the anchor
/// is not.
#[test]
fn below_input_sent_for_events() {
    let screen = format!("out\r\n{}", prompt_with_input("click_events=1", "ls"));
    let (mut app, pane_id) = app_with(screen.as_bytes());
    assert_eq!(click(&mut app, pane_id, (5, 9)), b"\x1b[<0;10;6M");
}

/// A click on a row above the prompt's anchor sends nothing.
#[test]
fn above_anchor_ignored() {
    let screen = format!("out\r\n{}", prompt_with_input("click_events=1", "ls"));
    let (mut app, pane_id) = app_with(screen.as_bytes());
    assert_eq!(click(&mut app, pane_id, (0, 1)), b"");
}

/// With input cells, arrows count typed input only: leftward clamps at the input's start, and a
/// click past the end goes to the end, plus one when the cursor sits on an input cell.
#[test]
fn g_count_end_plus_one_and_left_clamp() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    assert_eq!(click(&mut app, pane_id, (0, 3)), arrows('D', 2), "onto b");
    assert_eq!(click(&mut app, pane_id, (0, 0)), arrows('D', 3), "on the prompt: clamp");
    assert_eq!(click(&mut app, pane_id, (0, 20)), b"", "past the end, already at the end");
    // The cursor on b: c, then one past it.
    feed(&app, pane_id, b"\x1b[2D");
    assert_eq!(click(&mut app, pane_id, (0, 20)), arrows('C', 2), "end plus one");
    assert_eq!(click(&mut app, pane_id, (0, 3)), b"", "on the cursor's own cell");
}

/// Without input marks, arrows count text cells (kitty): leading blanks are skipped, a trailing
/// blank run counts one and ends the row, and a row with no text counts one.
#[test]
fn k_count_table() {
    let screen = format!("{}$ a b", mark("A", ""));
    let (mut app, pane_id) = app_with(screen.as_bytes());
    // Cursor at column 5, after "$ a b".
    let cases: [((u16, u16), Vec<u8>); 5] = [
        ((0, 4), arrows('D', 1)),
        ((0, 2), arrows('D', 3)),
        ((0, 0), arrows('D', 5)),
        ((0, 30), arrows('C', 1)),
        ((2, 3), arrows('C', 3)),
    ];
    for (cell, expected) in cases {
        assert_eq!(click(&mut app, pane_id, cell), expected, "{cell:?}");
    }
}

/// A wide character is one key, wherever on its two cells the click lands.
#[test]
fn wide_char_one_key() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "a\u{4e2d}b").as_bytes());
    // Input cells: a at 2, the wide lead at 3, its continuation at 4, b at 5; cursor at 6.
    assert_eq!(click(&mut app, pane_id, (0, 4)), arrows('D', 2), "input count, right half");
    let screen = format!("{}$ a\u{4e2d}b", mark("A", ""));
    let (mut app, pane_id) = app_with(screen.as_bytes());
    assert_eq!(click(&mut app, pane_id, (0, 4)), arrows('D', 2), "text count, right half");
}

/// A combining mark lives in its base cell's extras and adds no key.
#[test]
fn combining_adds_none() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "e\u{301}x").as_bytes());
    assert_eq!(click(&mut app, pane_id, (0, 2)), arrows('D', 2));
}

/// Input that soft-wraps is one line: arrows follow the wrap chain both ways and stop at its end.
#[test]
fn soft_wrap_chain() {
    let (mut app, pane_id) = app_with(b"");
    app.main().unwrap().panes[&pane_id].parser.lock().resize(10, 4);
    feed(&app, pane_id, prompt_with_input("", "abcdefghijkl").as_bytes());
    // Row 0 "$ abcdefgh", row 1 "ijkl"; the cursor is at row 1, column 4.
    assert_eq!(click(&mut app, pane_id, (0, 4)), arrows('D', 10), "onto c across the wrap");
    // CUP puts the cursor on c (row 0, column 4); CUB would stop at row 1's first column.
    feed(&app, pane_id, b"\x1b[1;5H");
    assert_eq!(click(&mut app, pane_id, (1, 2)), arrows('C', 8), "back across the wrap onto k");
    assert_eq!(click(&mut app, pane_id, (3, 0)), arrows('C', 10), "below: to the end plus one");
}

/// Keys come from SonicTerm's key encoder: DECCKM selects SS3 arrows, and kitty's report-events
/// flag adds each key's release.
#[test]
fn decckm_and_kitty_flags() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    feed(&app, pane_id, b"\x1b[?1h");
    assert_eq!(click(&mut app, pane_id, (0, 3)), b"\x1bOD\x1bOD");
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    feed(&app, pane_id, b"\x1b[>3u");
    assert_eq!(click(&mut app, pane_id, (0, 4)), b"\x1b[D\x1b[1;1:3D");
}

/// `special_key=1` turns each key into kitty's special key: `CSI 1;1u` left, `CSI 1u` right.
#[test]
fn special_key_csi_u() {
    let (mut app, pane_id) = app_with(prompt_with_input("special_key=1", "abc").as_bytes());
    assert_eq!(click(&mut app, pane_id, (0, 3)), b"\x1b[1;1u\x1b[1;1u");
    feed(&app, pane_id, b"\x1b[2D");
    assert_eq!(click(&mut app, pane_id, (0, 4)), b"\x1b[1u");
}

/// `declared` moves only at prompts that ask for it (`click_events` or `cl`); `off` never moves.
#[test]
fn declared_requires_option() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    app.config.terminal.click_to_move = ClickToMove::Declared;
    assert_eq!(click(&mut app, pane_id, (0, 3)), b"", "an undeclared prompt");
    let (mut app, pane_id) = app_with(prompt_with_input("cl=line", "abc").as_bytes());
    app.config.terminal.click_to_move = ClickToMove::Declared;
    assert_eq!(click(&mut app, pane_id, (0, 3)), arrows('D', 2), "cl=line");
    let (mut app, pane_id) = app_with(prompt_with_input("click_events=1", "abc").as_bytes());
    app.config.terminal.click_to_move = ClickToMove::Off;
    assert_eq!(click(&mut app, pane_id, (0, 3)), b"", "off");
}

/// Under mouse tracking a plain click is the program's (its press and release reports only);
/// Shift-click is local, so it moves the cursor. Ctrl, Alt and Super never move it.
#[test]
fn shift_click_under_tracking() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    feed(&app, pane_id, b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(click(&mut app, pane_id, (0, 3)), b"\x1b[<0;4;1M\x1b[<0;4;1m");
    assert_eq!(click_with(&mut app, pane_id, (0, 3), ModifiersState::SHIFT), arrows('D', 2));
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    for modifiers in [ModifiersState::CONTROL, ModifiersState::ALT, ModifiersState::SUPER] {
        assert_eq!(click_with(&mut app, pane_id, (0, 3), modifiers), b"", "{modifiers:?}");
    }
}

/// A release whose gesture the terminal captured is never turned into a prompt click, even when
/// the prompt asks for click events.
#[test]
fn terminal_owned_release_not_intercepted() {
    let (mut app, pane_id) = app_with(prompt_with_input("click_events=1", "abc").as_bytes());
    feed(&app, pane_id, b"\x1b[?1002h\x1b[?1006h");
    assert_eq!(click(&mut app, pane_id, (0, 3)), b"\x1b[<0;4;1M\x1b[<0;4;1m");
}

/// A standalone P prompt (no A, so no prompt region) still anchors the semantic state, so a click
/// in it moves the cursor.
#[test]
fn standalone_p_prompt_without_region_moves() {
    let screen = format!("{}$ {}abc", mark("P", ""), mark("B", ""));
    let (mut app, pane_id) = app_with(screen.as_bytes());
    assert_eq!(app.main().unwrap().panes[&pane_id].parser.lock().grid().prompts().count(), 0);
    assert_eq!(click(&mut app, pane_id, (0, 3)), arrows('D', 2));
}

/// A new semantic generation after the press (a P at the same row changes no region) cancels the
/// click at release.
#[test]
fn semantic_replacement_after_press_without_region_change_skips() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    let regions = app.main().unwrap().panes[&pane_id].parser.lock().grid().prompts().count();
    press(&mut app, pane_id, (0, 3), ModifiersState::empty());
    feed(&app, pane_id, format!("\r{}", mark("P", "k=i")).as_bytes());
    assert_eq!(
        app.main().unwrap().panes[&pane_id].parser.lock().grid().prompts().count(),
        regions,
        "no region changed"
    );
    release(&mut app);
    assert_eq!(take_log(&app), b"");
}

/// A resize, a tab switch away and back, or a viewport scroll between press and release cancels
/// the click.
#[test]
fn stale_press_after_resize_tab_switch_or_scroll_skips() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    press(&mut app, pane_id, (0, 3), ModifiersState::empty());
    app.main().unwrap().panes[&pane_id].parser.lock().resize(79, 24);
    release(&mut app);
    assert_eq!(take_log(&app), b"", "resize");

    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    app.__test_seed_tab("second");
    app.main_mut().unwrap().tabs.activate(0);
    press(&mut app, pane_id, (0, 3), ModifiersState::empty());
    app.main_mut().unwrap().tabs.activate(1);
    app.main_mut().unwrap().tabs.activate(0);
    release(&mut app);
    assert_eq!(take_log(&app), b"", "tab switch");

    let (mut app, pane_id) = app_with(&b"line\r\n".repeat(40));
    feed(&app, pane_id, prompt_with_input("", "abc").as_bytes());
    press(&mut app, pane_id, (23, 3), ModifiersState::empty());
    app.main_mut().unwrap().panes.get_mut(&pane_id).unwrap().pin_viewport_top(Some(0));
    release(&mut app);
    assert_eq!(take_log(&app), b"", "scroll");
}

/// A drag that leaves a selection, a double click's word selection, and a link-chord click never
/// move the cursor; selection and links keep precedence.
#[test]
fn selection_and_link_precedence() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc def").as_bytes());
    press(&mut app, pane_id, (0, 3), ModifiersState::empty());
    assert!(app.main_mut().unwrap().extend_local_selection_to_cell((0, 5)));
    release(&mut app);
    assert_eq!(take_log(&app), b"", "drag");
    assert!(app.main().unwrap().selection.is_some());

    let (mut app, pane_id) = app_with(prompt_with_input("", "abc def").as_bytes());
    let first = click(&mut app, pane_id, (0, 3));
    press(&mut app, pane_id, (0, 3), ModifiersState::empty());
    release(&mut app);
    assert_eq!(first, arrows('D', 6), "the first click moves");
    assert_eq!(take_log(&app), b"", "the double click selects a word instead");

    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    assert_eq!(click_with(&mut app, pane_id, (0, 3), ModifiersState::SUPER), b"", "link chord");
}

/// A parser held by another thread at release skips the click without waiting.
#[test]
fn contended_parser_skips() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    press(&mut app, pane_id, (0, 3), ModifiersState::empty());
    let parser = app.main().unwrap().panes[&pane_id].parser.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = parser.lock();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(3));
    });
    held_rx.recv().unwrap();
    let started = std::time::Instant::now();
    release(&mut app);
    let elapsed = started.elapsed();
    let _ = release_tx.send(());
    holder.join().unwrap();
    assert!(elapsed < std::time::Duration::from_secs(1), "release waited {elapsed:?}");
    assert_eq!(take_log(&app), b"");
}

/// A grid holding `screen` with a 200x100 or `cols`-wide layout, for the pure byte builder.
fn grid_parser(cols: u16, rows: u16, screen: &[u8]) -> Parser {
    let mut parser = Parser::new(Grid::new(cols, rows));
    parser.advance(screen);
    parser
}

/// The walk visits at most 16384 cells and the keys stay within 4096 keys and 12 KiB, all or
/// nothing: past any bound the click is skipped, never cut short.
#[test]
fn cells_visited_cap_and_byte_cap() {
    let parser = grid_parser(200, 100, format!("{}$ ", mark("A", "")).as_bytes());
    let snapshot = KeyboardSnapshot::from_bits(parser.keyboard_input_snapshot());
    let build = |parser: &Parser, cell: (u64, u16)| {
        prompt_click_bytes(parser.grid(), cell, cell.0 as u16, ClickToMove::AnyPrompt, snapshot)
    };
    assert!(build(&parser, (10, 0)).is_some(), "a near click");
    assert_eq!(build(&parser, (99, 199)), None, "past 16384 visited cells");

    let input = |count: usize| {
        let screen = format!("{}${}{}", mark("A", ""), mark("B", ""), "x".repeat(count));
        grid_parser(5000, 2, screen.as_bytes())
    };
    let at_cap = input(4096);
    assert_eq!(build(&at_cap, (0, 1)).map(|bytes| bytes.len()), Some(MAX_MOVE_BYTES));
    assert_eq!(build(&input(4097), (0, 1)), None, "past 4096 keys");
    let mut released = input(2000);
    released.advance(b"\x1b[>3u");
    let snapshot = KeyboardSnapshot::from_bits(released.keyboard_input_snapshot());
    let bytes = prompt_click_bytes(released.grid(), (0, 1), 0, ClickToMove::AnyPrompt, snapshot);
    assert_eq!(bytes, None, "2000 press-release pairs pass 12 KiB");
}

/// The click is written to the press pane only, as keyboard input, even with broadcast on.
#[test]
fn source_only_no_broadcast() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    app.__test_seed_tab("second");
    app.main_mut().unwrap().tabs.activate(0);
    app.toggle_broadcast(sonicterm_types::action::BroadcastScope::AllTabs);
    assert!(!app.__test_broadcast_receivers().is_empty(), "broadcast reaches the other pane");
    press(&mut app, pane_id, (0, 3), ModifiersState::empty());
    release(&mut app);
    assert_eq!(app.__test_pty_write_log(), vec![(pane_id, arrows('D', 2))]);
    assert_eq!(
        app.testing.test_pty_write_sources.borrow().last(),
        Some(&(pane_id, PtyInputSource::Keyboard))
    );
}

/// An open palette and READONLY copy mode own input, so a click moves nothing.
#[test]
fn overlays_and_readonly_skip() {
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    app.interaction.command_palette.open();
    assert_eq!(click(&mut app, pane_id, (0, 3)), b"", "palette");
    let (mut app, pane_id) = app_with(prompt_with_input("", "abc").as_bytes());
    app.main_mut().unwrap().copy_mode =
        Some(sonicterm_ui::copy_mode::CopyModeState::read_only_at((0, 0)));
    assert_eq!(click(&mut app, pane_id, (0, 3)), b"", "READONLY");
}

/// fish's prompt start exactly as it emits it (`133;A;click_events=1`, ST-terminated) gets one
/// absolute click report.
#[test]
fn fish_click_events_fixture() {
    let screen = b"\x1b]133;A;click_events=1\x1b\\> \x1b]133;B\x1b\\echo hi";
    let (mut app, pane_id) = app_with(screen);
    assert_eq!(click(&mut app, pane_id, (0, 4)), b"\x1b[<0;5;1M");
}
