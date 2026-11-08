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
