use super::*;
use crate::app::App;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::cursor::{BLINK_PHASE, BLINK_REST_AFTER};
use std::time::Duration;
use winit::window::WindowId;

/// An app with a seeded main window and one child window, as the redraw tests build them.
fn owners() -> (App, WindowId, WindowId) {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    app.__test_seed_tab("main");
    let main = app.registry.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    (app, main, child)
}

/// A clock samples on first, off in its second phase, and back on at the rest; a frame that finds
/// the cursor somewhere new restarts it, so output that moves the cursor shows it at once.
#[test]
fn a_moved_cursor_restarts_the_blink() {
    let start = Instant::now();
    let mut blink = CursorBlink::new(start);
    assert!(!blink.sample(start, (1, 0, 0), true), "the first phase is on");
    assert!(blink.sample(start + BLINK_PHASE, (1, 0, 0), true), "the second phase is off");
    // The cursor moved a column: the blink restarts on, mid-phase.
    assert!(!blink.sample(start + BLINK_PHASE, (1, 0, 1), true));
    // Another pane is another cursor, even at the same cell.
    assert!(blink.sample(start + BLINK_PHASE * 2, (1, 0, 1), true));
    assert!(!blink.sample(start + BLINK_PHASE * 2, (2, 0, 1), true));
    assert!(!blink.sample(start + BLINK_PHASE * 2 + BLINK_REST_AFTER, (2, 0, 1), true));
}

/// A frame that does not blink always shows the cursor, whatever the phase, and records that, so a
/// later activity does not ask for a redundant frame.
#[test]
fn a_cursor_that_does_not_blink_is_always_on() {
    let start = Instant::now();
    let mut blink = CursorBlink::new(start);
    // The first frame records the cursor, so the next one, in an off phase, keeps the clock.
    assert!(!blink.sample(start, (1, 0, 0), false));
    assert!(!blink.sample(start + BLINK_PHASE, (1, 0, 0), false), "off phase, not blinking");
    assert!(!blink.activity(start + BLINK_PHASE), "no off frame was drawn");
}

/// Activity during an off phase reports that the last frame hid the cursor, so the caller shows it
/// at once; activity restarts the blink on, and the next deadline is a whole phase later.
#[test]
fn activity_in_an_off_phase_asks_for_a_frame() {
    let start = Instant::now();
    let mut blink = CursorBlink::new(start);
    // The first frame records where the cursor is; a later frame at the same cell keeps the clock.
    assert!(!blink.sample(start, (1, 0, 0), true));
    let off_at = start + BLINK_PHASE + Duration::from_millis(10);
    assert!(blink.sample(off_at, (1, 0, 0), true));
    assert!(blink.activity(off_at), "the cursor was hidden, so a frame is owed now");
    assert!(!blink.sample(off_at, (1, 0, 0), true), "the restarted blink shows the cursor");
    assert_eq!(blink.deadline(off_at), Some(off_at + BLINK_PHASE));
}

/// About 28 blink frames are due in the 15 s after activity, then none, however long the window
/// stays idle; the last due frame shows the cursor.
#[test]
fn an_idle_window_stops_scheduling_blink_frames() {
    let start = Instant::now();
    let mut blink = CursorBlink::new(start);
    // The first frame records where the cursor is, so the frames below find it unmoved.
    assert!(!blink.sample(start, (1, 0, 0), true));
    let mut now = start;
    let mut frames = 0;
    let mut last_off = true;
    while let Some(deadline) = blink.deadline(now) {
        now = deadline;
        last_off = blink.sample(now, (1, 0, 0), true);
        frames += 1;
    }
    assert_eq!(frames, 29);
    assert!(!last_off, "the last frame shows the cursor");
    assert_eq!(blink.deadline(now + Duration::from_secs(3_600)), None);
}

/// A window blinks only with blinking on, focus, a visible cursor at the live view and no read-only
/// copy mode; the main and a child window answer alike, so both schedule blink deadlines.
#[test]
fn only_a_focused_live_visible_cursor_blinks_in_either_window() {
    let (mut app, main, child) = owners();
    for window_id in [main, child] {
        let window = app.registry.windows.get_mut(&window_id).unwrap();
        assert!(blinks(window, true, true), "a focused live cursor blinks");
        assert!(!blinks(window, false, true), "cursor_blink off");
        assert!(!blinks(window, true, false), "an unfocused window");
        let pane_id = window.tab_states[window.tabs.active_index()].active_pane;
        let pane = window.panes.get_mut(&pane_id).unwrap();
        pane.cursor_visible.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!blinks(window, true, true), "a cursor the program hid");
        let pane = window.panes.get_mut(&pane_id).unwrap();
        pane.cursor_visible.store(true, std::sync::atomic::Ordering::Relaxed);
        pane.viewport_top_abs = Some(0);
        assert!(!blinks(window, true, true), "a view scrolled back");
        window.panes.get_mut(&pane_id).unwrap().viewport_top_abs = None;
        window.copy_mode = Some(CopyModeState::read_only_at((0, 0)));
        assert!(!blinks(window, true, true), "read-only mode");
        window.copy_mode = Some(CopyModeState::new_at((0, 0)));
        assert!(blinks(window, true, true), "ordinary copy mode keeps the blink");
    }
}

/// A headless window has no renderer, so it never blinks and schedules no blink deadline.
#[test]
fn a_window_without_a_renderer_never_blinks() {
    let (app, main, child) = owners();
    for window_id in [main, child] {
        assert!(!window_blinks(&app.registry.windows[&window_id]));
    }
    let now = Instant::now();
    assert!(
        app.frame_due_work_at(now)
            .iter()
            .all(|work| work.cause != crate::app::redraw::DueCause::Cursor),
        "no window without a renderer schedules a blink frame"
    );
}

/// Output that advances in at least 4 of the last 6 collections is sustained, so the cursor does
/// not glide through a flood; output right after input is its echo and is never sustained, so a
/// typed character or an arrow key still glides; a pane switch is not an advance.
#[test]
fn sustained_output_is_told_from_an_echo() {
    let start = Instant::now();
    let frame = Duration::from_millis(16);
    let mut clock = CursorBlink::new(start);
    let mut generation = 0;
    let mut sustained = false;
    for step in 0..6u32 {
        generation += 1;
        sustained = clock.note_output(start + frame * step, 1, generation);
    }
    assert!(sustained, "six advancing collections with no input are a flood");

    // A keystroke, then its echo: not sustained, even after a flood.
    let typed = start + frame * 7;
    clock.activity(typed);
    generation += 1;
    assert!(!clock.note_output(typed + frame, 1, generation), "the echo of a key glides");

    // Quiet output: one batch among idle collections is not sustained.
    let mut quiet = CursorBlink::new(start);
    for step in 0..6u32 {
        let advanced = u64::from(step == 3);
        assert!(!quiet.note_output(start + frame * step, 1, advanced), "step {step}");
    }

    // Switching panes is not an advance, whatever the generations.
    let mut switching = CursorBlink::new(start);
    for step in 0..6u32 {
        let pane = u64::from(step % 2);
        assert!(!switching.note_output(start + frame * step, pane, u64::from(step)));
    }
}

/// A glide deadline is armed once and kept: unrelated turns before it never push it later, a turn
/// after it passes without a collected frame arms the next one a whole period on, and a collected
/// frame clears it.
#[test]
fn the_glide_deadline_holds_still_under_unrelated_turns() {
    let start = Instant::now();
    let period = Duration::from_millis(16);
    let blink = CursorBlink::new(start);
    let armed = blink.glide_deadline(start, start, period);
    assert_eq!(armed, start + period);
    for turn in 1..=3u32 {
        let now = start + Duration::from_millis(u64::from(turn) * 5);
        assert_eq!(blink.glide_deadline(now, start, period), armed, "turn {turn} slid it");
    }
    // Past it with no frame collected (a deferred frame): the next one is a whole period away.
    let late = start + Duration::from_millis(20);
    assert_eq!(blink.glide_deadline(late, start, period), late + period);
    // A collected frame services it; the next is armed from that frame.
    let mut blink = blink;
    let _ = blink.sample(late, (1, 0, 0), false);
    assert_eq!(blink.glide_deadline(late, late, period), late + period);
}

/// A paste into a torn-out window whose cursor the last frame hid wakes that child window, not the
/// main one; with the cursor already shown no window is woken.
#[test]
fn a_paste_into_a_child_window_wakes_that_child() {
    let (mut app, _main, child) = owners();
    let window = app.registry.windows.get_mut(&child).unwrap();
    let pane = window.tab_states[window.tabs.active_index()].active_pane;
    let start = Instant::now();
    window.cursor_blink = CursorBlink::new(start);
    let _ = window.cursor_blink.sample(start, (pane, 0, 0), true);
    assert!(window.cursor_blink.sample(start + BLINK_PHASE, (pane, 0, 0), true), "hidden");
    assert_eq!(app.note_cursor_activity(pane), Some(child));
    assert_eq!(app.note_cursor_activity(pane), None, "already shown: nothing to wake");
}

/// The sustained-output rule on real programs: a typed character echoed by `cat` is one output
/// batch right after input, so the cursor may still glide; a `yes` flood, with no input, is
/// sustained within a few collections and stops the glide. Collections are taken every 16 ms from
/// the pane's real output generation, as a 60 Hz frame loop would.
#[cfg(unix)]
#[test]
fn a_real_echo_glides_and_a_real_flood_does_not() {
    use sonicterm_types::{CommandLaunch, TerminalLaunch};
    if crate::app::pty_test_support::isolated() {
        return;
    }
    let launch = |program: &str| {
        crate::app::pane_launch::PaneLaunch::for_terminal(TerminalLaunch {
            command: Some(CommandLaunch { program: program.into(), args: Vec::new() }),
            ..TerminalLaunch::default()
        })
    };
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    app.__test_synthetic_main();
    let main = app.registry.main_window_id.unwrap();
    let generation = |app: &App, pane: u64| {
        app.registry.windows[&main].panes[&pane]
            .output_generation
            .load(std::sync::atomic::Ordering::Acquire)
    };
    let frame = Duration::from_millis(16);

    // Echo: `cat` prints each line it reads; one key stroke and its echo.
    app.new_tab_with_launch("echo", launch("cat"));
    let echo = app.registry.windows[&main].tab_states[0].active_pane;
    let mut clock = CursorBlink::new(Instant::now());
    for _ in 0..6 {
        std::thread::sleep(frame);
        let now = Instant::now();
        assert!(!clock.note_output(now, echo, generation(&app, echo)), "an idle pane is quiet");
    }
    clock.activity(Instant::now());
    app.__test_write_to_pane_with_broadcast(echo, b"x\n".to_vec());
    let mut echoed = false;
    for _ in 0..6 {
        std::thread::sleep(frame);
        let before = generation(&app, echo);
        echoed |= before > 0;
        assert!(!clock.note_output(Instant::now(), echo, before), "an echo is never sustained");
    }
    assert!(echoed, "cat echoed the line");

    // Flood: `yes` writes without input; once its output has run 250 ms it is sustained.
    app.new_tab_with_launch("flood", launch("yes"));
    let flood = app.registry.windows[&main].tab_states[1].active_pane;
    let mut clock = CursorBlink::new(Instant::now());
    let started = Instant::now();
    let mut sustained = false;
    while started.elapsed() < Duration::from_secs(3) && !sustained {
        std::thread::sleep(frame);
        sustained = clock.note_output(Instant::now(), flood, generation(&app, flood));
    }
    assert!(sustained, "a yes flood became sustained within 3 s");
}
