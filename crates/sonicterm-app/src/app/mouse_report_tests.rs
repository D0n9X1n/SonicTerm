//! Mouse report wire bytes for every encoding, and the pane-pixel and motion rules of 1016.

use super::*;
use sonicterm_vt::vt::MouseFormat;

/// A report of `button` with `modifiers` at zero-based cell (row, col) and pane pixel `pixel`.
fn report(
    button: u32,
    modifiers: u32,
    release: bool,
    cell: (u16, u16),
    pixel: (i32, i32),
) -> MouseReport {
    MouseReport { button, modifiers, release, cell, pixel }
}

/// Encode one report in `format`.
fn encode(format: MouseFormat, report: MouseReport) -> Vec<u8> {
    encode_mouse_report(format, report)
}

/// 1005 sends `CSI M`, the raw byte `Cb + 32`, then each coordinate as the UTF-8 encoding of its
/// zero-based index + 33: one byte up to index 94, two bytes from 95, and index 2015 or more
/// clamped to 2014 (`0x7FF`), the largest two-byte value.
#[test]
fn utf8_coordinates_are_index_plus_33_clamped_to_two_bytes() {
    let cases: [(u16, &[u8]); 5] = [
        (0, b"!"),
        (94, &[0x7f]),
        (95, &[0xc2, 0x80]),
        (2014, &[0xdf, 0xbf]),
        (2015, &[0xdf, 0xbf]),
    ];
    for (index, coordinate) in cases {
        let mut expected = b"\x1b[M ".to_vec();
        expected.extend_from_slice(coordinate);
        expected.extend_from_slice(coordinate);
        assert_eq!(
            encode(MouseFormat::Utf8, report(0, 0, false, (index, index), (0, 0))),
            expected,
            "{index}"
        );
    }
}

/// 1005's button byte is `Cb + 32` as one raw byte: a wheel with Shift, Alt and Control is
/// `64 + 28 + 32` up and one more down, and a release is legacy button 3 with its modifiers.
#[test]
fn utf8_button_byte_and_release() {
    assert_eq!(encode(MouseFormat::Utf8, report(64, 28, false, (0, 0), (0, 0))), b"\x1b[M|!!");
    assert_eq!(encode(MouseFormat::Utf8, report(65, 28, false, (0, 0), (0, 0))), b"\x1b[M}!!");
    assert_eq!(encode(MouseFormat::Utf8, report(0, 0, true, (2, 1), (0, 0))), b"\x1b[M#\"#");
    assert_eq!(encode(MouseFormat::Utf8, report(2, 4, true, (0, 0), (0, 0))), b"\x1b[M'!!");
}

/// 1015 is `CSI Cb+32 ; col+1 ; row+1 M` in decimal with no coordinate cap; a release is legacy
/// button 3 with its modifiers and still ends in uppercase `M`.
#[test]
fn urxvt_press_and_release() {
    assert_eq!(encode(MouseFormat::Urxvt, report(0, 0, false, (4, 4), (0, 0))), b"\x1b[32;5;5M");
    assert_eq!(encode(MouseFormat::Urxvt, report(0, 4, true, (4, 4), (0, 0))), b"\x1b[39;5;5M");
    assert_eq!(
        encode(MouseFormat::Urxvt, report(2, 16, false, (3000, 9000), (0, 0))),
        b"\x1b[50;9001;3001M"
    );
}

/// 1016 is the SGR form over zero-based pane pixels: signed and unclamped, so a pointer left of,
/// above or past the pane reports its true offset, and a release keeps its button with `m`.
#[test]
fn sgr_pixels_are_signed_and_unclamped() {
    assert_eq!(
        encode(MouseFormat::SgrPixels, report(0, 0, false, (0, 0), (-3, -7))),
        b"\x1b[<0;-3;-7M"
    );
    assert_eq!(
        encode(MouseFormat::SgrPixels, report(0, 0, true, (9, 9), (5000, 9000))),
        b"\x1b[<0;5000;9000m"
    );
    assert_eq!(
        encode(MouseFormat::SgrPixels, report(35, 8, false, (0, 0), (12, 34))),
        b"\x1b[<43;12;34M"
    );
}

/// Left, middle and right with every combination of Shift (4), Alt (8) and Control (16): SGR and
/// 1016 keep the button on release with `m`; X10, 1005 and 1015 release as button 3 with the same
/// modifier bits.
#[test]
fn every_button_and_modifier_combination_in_every_format() {
    for button in [0u32, 1, 2] {
        for modifiers in [0u32, 4, 8, 12, 16, 20, 24, 28] {
            let code = button + modifiers;
            let legacy_release = 3 + modifiers;
            let cell = (1, 2);
            let press = report(button, modifiers, false, cell, (7, 9));
            let release = report(button, modifiers, true, cell, (7, 9));
            assert_eq!(encode(MouseFormat::Sgr, press), format!("\x1b[<{code};3;2M").into_bytes());
            assert_eq!(
                encode(MouseFormat::Sgr, release),
                format!("\x1b[<{code};3;2m").into_bytes()
            );
            assert_eq!(
                encode(MouseFormat::SgrPixels, press),
                format!("\x1b[<{code};7;9M").into_bytes()
            );
            assert_eq!(
                encode(MouseFormat::SgrPixels, release),
                format!("\x1b[<{code};7;9m").into_bytes()
            );
            assert_eq!(
                encode(MouseFormat::Urxvt, press),
                format!("\x1b[{};3;2M", code + 32).into_bytes()
            );
            assert_eq!(
                encode(MouseFormat::Urxvt, release),
                format!("\x1b[{};3;2M", legacy_release + 32).into_bytes()
            );
            for format in [MouseFormat::X10, MouseFormat::Utf8] {
                assert_eq!(
                    encode(format, press),
                    vec![0x1b, b'[', b'M', (code + 32) as u8, 35, 34]
                );
                assert_eq!(
                    encode(format, release),
                    vec![0x1b, b'[', b'M', (legacy_release + 32) as u8, 35, 34]
                );
            }
        }
    }
}

/// X10 is unchanged: one-based coordinates plus 32, each clamped to 223.
#[test]
fn x10_is_unchanged() {
    assert_eq!(encode(MouseFormat::X10, report(0, 0, false, (2, 4), (0, 0))), b"\x1b[M %#");
    assert_eq!(
        encode(MouseFormat::X10, report(65, 0, false, (9999, 9999), (0, 0))),
        vec![0x1b, b'[', b'M', 97, 255, 255]
    );
}

use crate::app::secondary_button::SecondaryButton;
use crate::app::window_event::{pointer_route_bytes, take_pointer_release, PointerReportKind};
use crate::app::PointerCell;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_vt::vt::MouseTracking;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use winit::{
    dpi::PhysicalPosition,
    event::{ElementState, MouseScrollDelta},
    keyboard::ModifiersState,
};

/// A headless main window split into two panes on an 800x240 viewport of 10-pixel cells, with
/// the write ledger on; returns it with the main window and the left and right pane ids.
fn split_main() -> (App, WindowId, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let left = app.__test_seed_tab("main");
    app.__test_set_main_pane_viewport(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0);
    app.__test_split_active_right();
    let right = app
        .compute_active_pane_rects()
        .into_iter()
        .map(|(pane_id, _)| pane_id)
        .find(|pane_id| *pane_id != left)
        .expect("a second pane");
    app.__test_enable_pty_write_log();
    let main = app_main(&app);
    (app, main, left, right)
}

/// The main window's id.
fn app_main(app: &App) -> WindowId {
    app.registry.main_window_id.expect("main window")
}

/// Advance a pane's parser with `bytes` and publish its modes, as the VT worker does.
fn publish(app: &App, window_id: WindowId, pane_id: u64, bytes: &[u8]) {
    let pane = &app.registry.windows[&window_id].panes[&pane_id];
    let mut parser = pane.parser.lock();
    parser.advance(bytes);
    pane.__test_publish_input_modes(&parser);
}

/// The bytes written to every pane so far, in order.
fn writes(app: &App) -> Vec<Vec<u8>> {
    app.__test_pty_write_log().into_iter().map(|(_, bytes)| bytes).collect()
}

/// Give main-window pane `pane_id` a real shell PTY, so its input is admitted rather than refused.
fn attach_shell(app: &mut App, pane_id: u64) {
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/D".to_owned(), "/Q".to_owned()]);
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-s".to_owned()]);
    let pty =
        sonicterm_io::pty::PtyHandle::spawn_with_args(program, &args, 80, 24).expect("PTY fixture");
    assert!(app.__test_set_pane_pty(pane_id, Some(pty)));
}

/// Flush every pane's coalesced motion until the writer has admitted or discarded it, as the
/// event loop's motion wake does; a full writer queue retries after a short wait.
fn flush_all(app: &mut App) {
    for _ in 0..200 {
        if app.flush_pointer_motion(std::time::Instant::now()).is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("pointer motion was never admitted");
}

/// The bytes the writer admitted since `submitted` started, in order.
fn admitted(submitted: &crate::app::mod_tests::PtySubmissions) -> Vec<Vec<u8>> {
    submitted.take().into_iter().map(|(_, bytes)| bytes).collect()
}

/// In 1016 two motions within one cell report their own pixels, a motion identical to the last
/// one the writer admitted is dropped, the same pixel with Shift held is sent, and the wheel
/// reports the pixel too. Each motion is flushed, so the writer admits it before the next one is
/// compared with it.
#[test]
fn pixel_motion_reports_each_pixel_and_drops_only_an_identical_report() {
    let (mut app, main, left, _right) = split_main();
    attach_shell(&mut app, left);
    publish(&app, main, left, b"\x1b[?1003h\x1b[?1016h");
    let submitted = crate::app::mod_tests::PtySubmissions::start();
    for (pointer_x, modifiers) in [
        (55.0, ModifiersState::empty()),
        (57.0, ModifiersState::empty()),
        (57.0, ModifiersState::empty()),
        (57.0, ModifiersState::SHIFT),
    ] {
        app.main_mut().unwrap().modifiers = modifiers;
        app.handle_main_cursor_moved(PhysicalPosition::new(pointer_x, 85.0));
        flush_all(&mut app);
    }
    app.main_mut().unwrap().modifiers = ModifiersState::empty();
    app.handle_main_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));
    assert_eq!(
        admitted(&submitted),
        [
            b"\x1b[<35;55;85M".to_vec(),
            b"\x1b[<35;57;85M".to_vec(),
            b"\x1b[<39;57;85M".to_vec(),
            b"\x1b[<64;57;85M".repeat(3),
        ]
    );
}

/// A motion encoded in a cell format from the published 1006 is never sent once the program has
/// switched to 1016 before the writer admits it: the queued report keeps the profile it was
/// encoded under, so admission drops it rather than sending cell bytes to a program reading pixels.
#[test]
fn a_cell_motion_queued_before_a_switch_to_pixels_is_not_sent() {
    let (mut app, main, left, _right) = split_main();
    attach_shell(&mut app, left);
    publish(&app, main, left, b"\x1b[?1003h\x1b[?1006h");
    // The parser has switched to 1016; the VT worker has not published the switch yet.
    app.registry.windows[&main].panes[&left].parser.lock().advance(b"\x1b[?1016h");
    let submitted = crate::app::mod_tests::PtySubmissions::start();
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    flush_all(&mut app);
    assert!(admitted(&submitted).is_empty(), "cell bytes are not sent under a pixel profile");
}

/// A 1016 motion counts as sent only when the writer admits it: a queued motion that a mode switch
/// discards before admission does not suppress the same pixel once the program returns to 1016.
#[test]
fn a_pixel_motion_discarded_before_admission_does_not_suppress_the_same_pixel() {
    let (mut app, main, left, _right) = split_main();
    attach_shell(&mut app, left);
    publish(&app, main, left, b"\x1b[?1003h\x1b[?1016h");
    let submitted = crate::app::mod_tests::PtySubmissions::start();
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    flush_all(&mut app);
    app.handle_main_cursor_moved(PhysicalPosition::new(57.0, 85.0));
    // The program leaves 1016 before the queued motion reaches the writer, which discards it.
    publish(&app, main, left, b"\x1b[?1016l");
    flush_all(&mut app);
    publish(&app, main, left, b"\x1b[?1016h");
    app.handle_main_cursor_moved(PhysicalPosition::new(57.0, 85.0));
    flush_all(&mut app);
    assert_eq!(admitted(&submitted), [b"\x1b[<35;55;85M".to_vec(), b"\x1b[<35;57;85M".to_vec()]);
}

/// A 1016 motion the pane refuses (here it has no PTY) is not recorded as sent, so the next
/// identical motion is still offered to the pane rather than dropped as a repeat.
#[test]
fn a_refused_pixel_motion_does_not_suppress_the_next_identical_one() {
    let (mut app, main, left, _right) = split_main();
    publish(&app, main, left, b"\x1b[?1003h\x1b[?1016h");
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    assert_eq!(writes(&app), [b"\x1b[<35;55;85M".to_vec(), b"\x1b[<35;55;85M".to_vec()]);
}

/// The cell formats keep today's motion bytes: under 1006 every motion is sent, an identical one
/// included, and X10 sends `CSI M` with biased bytes.
#[test]
fn cell_formats_keep_reporting_every_motion() {
    let (mut app, main, left, _right) = split_main();
    publish(&app, main, left, b"\x1b[?1003h\x1b[?1006h");
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    app.handle_main_cursor_moved(PhysicalPosition::new(57.0, 85.0));
    app.handle_main_cursor_moved(PhysicalPosition::new(57.0, 85.0));
    publish(&app, main, left, b"\x1b[?1006l");
    app.handle_main_cursor_moved(PhysicalPosition::new(57.0, 85.0));
    assert_eq!(
        writes(&app),
        [
            b"\x1b[<35;6;9M".to_vec(),
            b"\x1b[<35;6;9M".to_vec(),
            b"\x1b[<35;6;9M".to_vec(),
            vec![0x1b, b'[', b'M', 67, 38, 41],
        ]
    );
}

/// A left press in the right pane measures pixels from that pane's own origin, and its release
/// keeps the press-time 1016 with the pointer's negative pixel after a drag out of the pane; a held
/// motion after the program switched to 1006 is dropped, since neither format is now the one the
/// program reads for new motion.
#[test]
fn a_held_press_keeps_its_pane_origin_and_press_time_format() {
    let (mut app, main, _left, right) = split_main();
    let origin_x = app
        .compute_active_pane_rects()
        .into_iter()
        .find(|(pane_id, _)| *pane_id == right)
        .map(|(_, rect)| f64::from(rect.x))
        .unwrap();
    publish(&app, main, right, b"\x1b[?1002h\x1b[?1016h");
    app.handle_main_cursor_moved(PhysicalPosition::new(origin_x + 5.0, 85.0));
    let pixel = app.pane_pixel(main, right);
    assert_eq!(pixel, (5, 85));
    let cell = PointerCell { pane_id: right, row: 8, col: 0, pixel };
    let window = app.main_mut().unwrap();
    let press = window.begin_pointer_press(
        cell,
        MouseTracking::ButtonMotion,
        MouseFormat::SgrPixels,
        false,
    );
    assert_eq!(press.unwrap(), b"\x1b[<0;5;85M");
    window.mouse_down = true;
    publish(&app, main, right, b"\x1b[?1006h");
    app.handle_main_cursor_moved(PhysicalPosition::new(origin_x - 20.0, 85.0));
    assert!(writes(&app).is_empty(), "held motion after a format change is dropped");
    let route =
        take_pointer_release(&mut app.main_mut().unwrap().pointer_gesture, ModifiersState::empty());
    let (_, release) = pointer_route_bytes(route.unwrap(), PointerReportKind::LeftRelease).unwrap();
    assert_eq!(release, b"\x1b[<0;-20;85m");
}

/// A right press keeps the format it was reported in for its release: under 1015 its press and its
/// legacy release are urxvt, and a held motion after the program switched to 1016 is dropped.
#[test]
fn a_secondary_press_keeps_its_press_time_format() {
    let (mut app, main, left, _right) = split_main();
    publish(&app, main, left, b"\x1b[?1002h\x1b[?1015h");
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    app.route_secondary_button(main, SecondaryButton::Right, ElementState::Pressed);
    publish(&app, main, left, b"\x1b[?1016h");
    app.handle_main_cursor_moved(PhysicalPosition::new(57.0, 85.0));
    app.route_secondary_button(main, SecondaryButton::Right, ElementState::Released);
    assert_eq!(writes(&app), [b"\x1b[34;6;9M".to_vec(), b"\x1b[35;6;9M".to_vec()]);
}

/// A child window measures from its own pane's origin and rounds a fractional pointer position:
/// a split pane from its own left edge, a zoomed pane from the window's pane area.
#[test]
fn child_split_and_zoomed_panes_measure_from_their_own_origin() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let outer = Rect::new(20.0, 30.0, 800.0, 240.0);
    assert!(app.__test_set_child_pane_viewport(child, outer, 10.0, 10.0));
    let first = app.__test_child_active_pane(child).unwrap();
    publish(&app, child, first, b"\x1b[?1003h\x1b[?1016h");
    app.__test_enable_pty_write_log();
    let config = app.config.clone();
    app.handle_child_cursor_moved(child, PhysicalPosition::new(75.4, 115.6), &config);
    assert_eq!(writes(&app), [b"\x1b[<35;55;86M".to_vec()]);

    assert!(app
        .__test_invoke_split_active_pane_in_child(child, sonicterm_cfg::keymap::Direction::Right));
    let second = app.__test_child_active_pane(child).unwrap();
    let second_x = App::compute_pane_rects_for(&app.registry.windows[&child])
        .into_iter()
        .find(|(pane_id, _)| *pane_id == second)
        .map(|(_, rect)| f64::from(rect.x))
        .unwrap();
    assert!(second_x > 400.0, "the split pane starts right of the window's middle");
    publish(&app, child, second, b"\x1b[?1003h\x1b[?1016h");
    app.handle_child_cursor_moved(child, PhysicalPosition::new(second_x + 3.4, 115.6), &config);
    assert!(app.__test_invoke_toggle_active_pane_zoom_in_child(child));
    app.handle_child_cursor_moved(child, PhysicalPosition::new(75.4, 115.6), &config);
    assert_eq!(writes(&app)[1..], [b"\x1b[<35;3;86M".to_vec(), b"\x1b[<35;55;86M".to_vec()]);
}

/// Arm the queue hook to switch `pane_id` to the alternate screen and publish it, as the VT worker
/// does, after the motion route has read the modes it encodes under and before the report queues.
fn switch_screen_before_queueing(app: &App, window_id: WindowId, pane_id: u64) {
    let pane = &app.registry.windows[&window_id].panes[&pane_id];
    let (parser, published) = (Arc::clone(&pane.parser), Arc::clone(&pane.pointer_input));
    MOTION_QUEUE_HOOK.set(Some(Box::new(move || {
        let mut parser = parser.lock();
        parser.advance(b"\x1b[?1049h");
        // Ordering: pointer_input stores Relaxed; this thread is the handler's only reader.
        published.store(parser.pointer_input_snapshot(), Ordering::Relaxed);
    })));
}

/// A main-window no-button motion is queued under the modes it was encoded with: a screen switch
/// published between encoding and queueing, with the same tracking and format, does not retag it,
/// so the writer drops a primary-screen report instead of sending it to the alternate screen.
#[test]
fn a_main_motion_keeps_its_encode_time_screen() {
    let (mut app, main, left, _right) = split_main();
    attach_shell(&mut app, left);
    publish(&app, main, left, b"\x1b[?1003h\x1b[?1006h");
    let submitted = crate::app::mod_tests::PtySubmissions::start();
    switch_screen_before_queueing(&app, main, left);
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    flush_all(&mut app);
    assert!(
        admitted(&submitted).is_empty(),
        "a primary-screen report reached the alternate screen"
    );
}

/// A headless child window on an 800x240 viewport of 10-pixel cells whose one pane has a real
/// shell PTY; returns it with the child window and its pane id.
fn child_with_shell() -> (App, WindowId, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.__test_set_child_pane_viewport(
        child,
        Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0
    ));
    let pane_id = app.__test_child_active_pane(child).unwrap();
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/D".to_owned(), "/Q".to_owned()]);
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-s".to_owned()]);
    let pty =
        sonicterm_io::pty::PtyHandle::spawn_with_args(program, &args, 80, 24).expect("PTY fixture");
    app.registry.windows.get_mut(&child).unwrap().panes.get_mut(&pane_id).unwrap().pty = Some(pty);
    (app, child, pane_id)
}

/// A child-window no-button motion keeps its encode-time screen the same way as the main window's.
#[test]
fn a_child_motion_keeps_its_encode_time_screen() {
    let (mut app, child, pane_id) = child_with_shell();
    publish(&app, child, pane_id, b"\x1b[?1003h\x1b[?1006h");
    let submitted = crate::app::mod_tests::PtySubmissions::start();
    switch_screen_before_queueing(&app, child, pane_id);
    let config = app.config.clone();
    app.handle_child_cursor_moved(child, PhysicalPosition::new(55.0, 85.0), &config);
    flush_all(&mut app);
    assert!(
        admitted(&submitted).is_empty(),
        "a primary-screen report reached the alternate screen"
    );
}

/// The 1016 repeat rule compares the whole profile a report was admitted under: the first motion
/// on the alternate screen is sent even when its bytes equal the primary screen's last report.
#[test]
fn the_first_alternate_screen_pixel_motion_is_not_a_repeat() {
    let (mut app, main, left, _right) = split_main();
    attach_shell(&mut app, left);
    publish(&app, main, left, b"\x1b[?1003h\x1b[?1016h");
    let submitted = crate::app::mod_tests::PtySubmissions::start();
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    flush_all(&mut app);
    publish(&app, main, left, b"\x1b[?1049h");
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    flush_all(&mut app);
    assert_eq!(admitted(&submitted), [b"\x1b[<35;55;85M".to_vec(), b"\x1b[<35;55;85M".to_vec()]);
}

/// A held left press is stamped with its press-time profile: after the program switches to the
/// alternate screen with the same tracking and format, held motion is dropped rather than sent to
/// a screen the press never saw.
#[test]
fn a_held_left_motion_keeps_its_press_time_screen() {
    let (mut app, main, left, _right) = split_main();
    attach_shell(&mut app, left);
    publish(&app, main, left, b"\x1b[?1002h\x1b[?1006h");
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    let cell = PointerCell { pane_id: left, row: 8, col: 5, pixel: app.pane_pixel(main, left) };
    let window = app.main_mut().unwrap();
    let press =
        window.begin_pointer_press(cell, MouseTracking::ButtonMotion, MouseFormat::Sgr, false);
    assert!(press.is_some(), "the press is the terminal's");
    window.mouse_down = true;
    publish(&app, main, left, b"\x1b[?1049h");
    let submitted = crate::app::mod_tests::PtySubmissions::start();
    app.handle_main_cursor_moved(PhysicalPosition::new(75.0, 85.0));
    flush_all(&mut app);
    assert!(admitted(&submitted).is_empty(), "a primary-screen drag reached the alternate screen");
}

/// A held right press is stamped with its press-time profile the same way: held motion after a
/// switch to the alternate screen is dropped.
#[test]
fn a_held_secondary_motion_keeps_its_press_time_screen() {
    let (mut app, main, left, _right) = split_main();
    attach_shell(&mut app, left);
    publish(&app, main, left, b"\x1b[?1002h\x1b[?1006h");
    app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
    app.route_secondary_button(main, SecondaryButton::Right, ElementState::Pressed);
    publish(&app, main, left, b"\x1b[?1049h");
    let submitted = crate::app::mod_tests::PtySubmissions::start();
    app.handle_main_cursor_moved(PhysicalPosition::new(75.0, 85.0));
    flush_all(&mut app);
    assert!(admitted(&submitted).is_empty(), "a primary-screen drag reached the alternate screen");
}

/// A child window's held left press keeps its press-time profile as the main window's does.
#[test]
fn a_child_held_left_motion_keeps_its_press_time_screen() {
    let (mut app, child, pane_id) = child_with_shell();
    publish(&app, child, pane_id, b"\x1b[?1002h\x1b[?1006h");
    let config = app.config.clone();
    app.handle_child_cursor_moved(child, PhysicalPosition::new(55.0, 85.0), &config);
    let cell = PointerCell { pane_id, row: 8, col: 5, pixel: app.pane_pixel(child, pane_id) };
    let window = app.registry.windows.get_mut(&child).unwrap();
    let press =
        window.begin_pointer_press(cell, MouseTracking::ButtonMotion, MouseFormat::Sgr, false);
    assert!(press.is_some(), "the press is the terminal's");
    window.mouse_down = true;
    publish(&app, child, pane_id, b"\x1b[?1049h");
    let submitted = crate::app::mod_tests::PtySubmissions::start();
    app.handle_child_cursor_moved(child, PhysicalPosition::new(75.0, 85.0), &config);
    flush_all(&mut app);
    assert!(admitted(&submitted).is_empty(), "a primary-screen drag reached the alternate screen");
}
