//! Native dispatch and source-frame collection contracts.

use super::super::window_event_tests::*;
use super::*;

/// READONLY consumes new presses in every window while ordinary copy mode keeps terminal mouse ownership.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_readonly_pointer_press_latches_local() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::isolated,
        PtyInputSource,
    };
    use sonicterm_ui::copy_mode::CopyModeState;
    if isolated() {
        return;
    }
    for target in 0..3 {
        for read_only in [false, true] {
            let (mut app, windows) = input_test_windows();
            let (window, pane) = windows[target];
            app.windows.get_mut(&window).unwrap().copy_mode = Some(if read_only {
                CopyModeState::read_only_at((0, 0))
            } else {
                CopyModeState::new_at((0, 0))
            });
            app.wait_for_input_queues();
            let submitted = PtySubmissions::start();
            let bytes = app.windows.get_mut(&window).unwrap().begin_pointer_press(
                pointer_cell(pane, 2, 3),
                MouseTracking::AnyMotion,
                true,
            );
            if let Some(bytes) = bytes {
                app.write_to_pane(pane, bytes, PtyInputSource::PointerButton);
            }
            assert_eq!(
                submitted.take(),
                if read_only { Vec::new() } else { vec![(pane, b"\x1b[<0;4;3M".to_vec())] },
                "target={target} read_only={read_only}"
            );
            assert_eq!(
                app.windows[&window].pointer_gesture.unwrap().owner == PointerGestureOwner::Local,
                read_only
            );
        }
    }
}

/// Winit and OLE file-drop callers share one READONLY guard before input or AllTabs fan-out.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_readonly_shared_drop_is_consumed() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::isolated,
    };
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::{broadcast::BroadcastState, copy_mode::CopyModeState};
    if isolated() {
        return;
    }
    for target in 0..3 {
        for read_only in [false, true] {
            let (mut app, windows) = input_test_windows();
            let (window, pane) = windows[target];
            app.windows.get_mut(&window).unwrap().copy_mode = Some(if read_only {
                CopyModeState::read_only_at((0, 0))
            } else {
                CopyModeState::new_at((0, 0))
            });
            app.broadcast =
                BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: pane };
            app.wait_for_input_queues();
            let submitted = PtySubmissions::start();
            app.paste_file_paths_in_window(window, vec![std::path::PathBuf::from("safe path")]);
            let writes = submitted.take();
            if read_only {
                assert!(writes.is_empty(), "target={target}: {writes:?}");
            } else {
                assert_eq!(writes.len(), 3);
                // The Windows fixture launches cmd; Unix launches sh, so accepted paths use their own shell syntax.
                let expected: &[u8] = if cfg!(windows) { b"\"safe path\"" } else { b"'safe path'" };
                assert!(writes.iter().all(|(_, bytes)| bytes == expected));
            }
        }
    }
}

/// Real main/child event handlers must keep READONLY wheel local and consume new mouse/drop input.
#[cfg(windows)]
#[test]
fn real_pty_readonly_native_pointer_wheel_drop_matrix() {
    use crate::app::pty_test_support::isolated;
    use winit::{
        application::ApplicationHandler,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
    };
    if isolated() {
        return;
    }
    struct Probe {
        ran: bool,
    }
    impl ApplicationHandler<crate::app::UserEvent> for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            run_readonly_native_matrix(event_loop);
            self.ran = true;
            event_loop.exit();
        }
        fn window_event(
            &mut self,
            _: &ActiveEventLoop,
            _: winit::window::WindowId,
            _: winit::event::WindowEvent,
        ) {
        }
    }
    let event_loop = EventLoop::<crate::app::UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .unwrap();
    crate::os_drag_bridge::install_proxy(event_loop.create_proxy());
    let mut probe = Probe { ran: false };
    event_loop.run_app(&mut probe).unwrap();
    assert!(probe.ran);
}

/// Accepted pointer releases keep their press owner across READONLY or a later local overlay; focus reports remain exempt.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_accepted_pointer_and_focus_routes_survive_readonly() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::isolated,
        PtyInputSource,
    };
    use sonicterm_ui::copy_mode::CopyModeState;
    if isolated() {
        return;
    }
    for target in 0..3 {
        for local_overlay in [false, true] {
            let (mut app, windows) = input_test_windows();
            let (window, pane) = windows[target];
            app.wait_for_input_queues();
            let submitted = PtySubmissions::start();
            let press = app
                .windows
                .get_mut(&window)
                .unwrap()
                .begin_pointer_press(pointer_cell(pane, 2, 3), MouseTracking::ButtonMotion, true)
                .unwrap();
            assert!(app.write_to_pane(pane, press, PtyInputSource::PointerButton));
            assert_eq!(submitted.take(), vec![(pane, b"\x1b[<0;4;3M".to_vec())]);
            app.windows.get_mut(&window).unwrap().copy_mode =
                Some(CopyModeState::read_only_at((0, 0)));
            if local_overlay {
                app.run_action_for_window(&Action::OpenCommandPalette, window);
            }
            let release = take_pointer_release(
                &mut app.windows.get_mut(&window).unwrap().pointer_gesture,
                ModifiersState::SHIFT,
            )
            .unwrap();
            let (destination, bytes) =
                super::super::pointer_route_bytes(release, PointerReportKind::LeftRelease).unwrap();
            assert_eq!(destination, pane);
            app.wait_for_input_queues();
            assert!(app.write_to_pane(destination, bytes, PtyInputSource::PointerButton));
            assert_eq!(submitted.take(), vec![(pane, b"\x1b[<4;4;3m".to_vec())]);
            assert!(app.windows[&window].pointer_gesture.is_none());
            app.command_palette.close();
            app.windows[&window].panes[&pane].parser.lock().advance(b"\x1b[?1004h");
            app.wait_for_input_queues();
            app.handle_window_focus_changed(window, false);
            app.wait_for_input_queues();
            app.handle_window_focus_changed(window, true);
            assert_eq!(
                submitted.take(),
                vec![(pane, b"\x1b[O".to_vec()), (pane, b"\x1b[I".to_vec())]
            );
        }
    }
}

/// On POSIX, an accepted routed hold still writes to READONLY peers and an orphan repeat acquires no route.
#[cfg(unix)]
#[test]
fn real_pty_posix_accepted_key_routes_survive_readonly() {
    use crate::app::{
        keyboard_protocol::{EncodedKey, HeldKey},
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::isolated,
    };
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::{broadcast::BroadcastState, copy_mode::CopyModeState};
    use std::collections::BTreeSet;
    if isolated() {
        return;
    }
    let (mut app, windows) = input_test_windows();
    let (source_window, source) = windows[0];
    app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
    app.wait_for_input_queues();
    let submitted = PtySubmissions::start();
    let encode = |targets: BTreeSet<u64>, bytes: &[u8]| {
        targets
            .into_iter()
            .map(|pane| (pane, EncodedKey { bytes: bytes.to_vec(), held: HeldKey::Legacy }))
            .collect()
    };
    let writes = encode(app.terminal_key_targets(source), b"press");
    let accepted = app.dispatch_terminal_key_writes(writes);
    let key = PhysicalKey::Code(KeyCode::ArrowUp);
    app.windows.get_mut(&source_window).unwrap().pty_pressed_keys.insert(key, accepted.clone());
    submitted.take();
    for (window, _) in windows {
        app.windows.get_mut(&window).unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
    }
    let repeat =
        terminal_repeat_targets(&app.windows[&source_window].pty_pressed_keys, key, true).unwrap();
    app.wait_for_input_queues();
    app.dispatch_terminal_key_writes(encode(repeat.keys().copied().collect(), b"repeat"));
    assert_eq!(
        submitted.take().into_iter().map(|(pane, _)| pane).collect::<BTreeSet<_>>(),
        accepted.keys().copied().collect()
    );
    assert!(terminal_repeat_targets(
        &app.windows[&source_window].pty_pressed_keys,
        PhysicalKey::Code(KeyCode::ArrowLeft),
        true
    )
    .is_none());
}

/// A child's live no-button route respects READONLY before staging, while regular copy mode leaves terminal tracking unchanged.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_readonly_unheld_motion_shared_window_matrix() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::isolated,
        PtyInputSource,
    };
    use sonicterm_ui::copy_mode::CopyModeState;
    if isolated() {
        return;
    }
    for target in 0..3 {
        for read_only in [false, true] {
            let (mut app, windows) = input_test_windows();
            let (window, pane) = windows[target];
            app.windows.get_mut(&window).unwrap().copy_mode = Some(if read_only {
                CopyModeState::read_only_at((0, 0))
            } else {
                CopyModeState::new_at((0, 0))
            });
            app.windows[&window].panes[&pane].parser.lock().advance(b"\x1b[?1003h\x1b[?1006h");
            app.wait_for_input_queues();
            let submitted = PtySubmissions::start();
            let route = child_no_button_motion_report(
                &app.windows[&window],
                pointer_cell(pane, 2, 3),
                MouseTracking::AnyMotion,
                true,
                false,
            );
            if let Some((pane, bytes)) = route.and_then(|route| {
                super::super::pointer_route_bytes(route, PointerReportKind::NoButtonMotion)
            }) {
                app.write_to_pane(pane, bytes, PtyInputSource::PointerMotion);
            }
            assert!(submitted.take().is_empty(), "staging is not submission");
            app.flush_pointer_motion(std::time::Instant::now());
            assert_eq!(
                submitted.take(),
                if read_only { Vec::new() } else { vec![(pane, b"\x1b[<35;4;3M".to_vec())] },
                "target={target} readonly={read_only}"
            );
        }
    }
}

#[test]
fn ime_and_search_dispatch_have_one_window_scoped_owner() {
    // Native IME must take one source-window route before main/child dispatch can diverge.
    let main = MAIN_SOURCES;
    let child = CHILD_SOURCES;
    let search = include_str!("search_handle.rs");
    let route = main
        .find("self.handle_window_ime(win_id, ime_event)")
        .expect("IME must route through the shared source-window handler");
    assert!(main.find("self.is_warm_window_id(win_id)").unwrap() < route);
    assert!(
        route < main.find("self.handle_child_window_event(event_loop, win_id, event)").unwrap()
    );
    assert_eq!(main.matches("WindowEvent::Ime(ime_event)").count(), 1);
    assert!(!child.contains("WindowEvent::Ime"));
    assert_eq!(search.matches("fn search_handle_ime_commit(").count(), 1);
    assert_eq!(search.matches("fn search_handle_key(").count(), 1);
    assert!(!search.contains("search_handle_ime_commit_in_child"));
    assert!(!search.contains("search_handle_key_in_child"));
}

#[test]
fn native_input_dispatch_has_one_source_window_boundary() {
    // Every native input path rejects stale/warm targets before a shared handler can reach main fallback.
    let source = MAIN_SOURCES;
    let (_, dispatch) = source.split_once("pub(super) fn do_window_event(").unwrap();
    let child = CHILD_SOURCES;
    let warm = dispatch.find("self.is_warm_window_id(win_id)").unwrap();
    let live = dispatch.find("!self.windows.contains_key(&win_id)").unwrap();
    let split = dispatch.find("self.handle_child_window_event(event_loop, win_id, event)").unwrap();
    for call in [
        "self.handle_window_keyboard(win_id, &event, is_synthetic)",
        "self.handle_window_focus_changed(win_id, focused)",
        "self.handle_window_modifiers_changed(win_id, modifiers.state())",
    ] {
        let position = dispatch.find(call).expect("shared native input handler");
        assert!(warm < position && live < position && position < split);
    }
    for event in [
        "WindowEvent::KeyboardInput { event, is_synthetic, .. } =>",
        "WindowEvent::Focused(focused) =>",
        "WindowEvent::ModifiersChanged(modifiers) =>",
    ] {
        assert_eq!(dispatch.matches(event).count(), 1);
    }
    for duplicate in [
        "WindowEvent::KeyboardInput",
        "WindowEvent::Focused",
        "WindowEvent::ModifiersChanged",
        "fn child_copy_mode_handle_key",
        "fn child_enter_copy_mode",
        "fn child_enter_quick_select",
    ] {
        assert!(!child.contains(duplicate), "duplicate input path: {duplicate}");
    }
}

#[test]
fn wheel_route_tracking_takes_precedence_on_both_screens() {
    // Every tracking mode owns wheel input on primary and alternate screens; screen alone selects only the fallback.
    for tracking in [MouseTracking::Button, MouseTracking::ButtonMotion, MouseTracking::AnyMotion] {
        for is_alt in [false, true] {
            assert_eq!(
                wheel_route(tracking, is_alt),
                WheelRoute::MouseReport,
                "{tracking:?}, alt={is_alt}"
            );
        }
    }
    assert_eq!(wheel_route(MouseTracking::Off, false), WheelRoute::LocalScrollback);
    assert_eq!(wheel_route(MouseTracking::Off, true), WheelRoute::CursorKeys);
}

#[test]
fn wheel_route_sgr_encoding_alone_does_not_enable_tracking() {
    // DEC1006 selects mouse encoding only, so it must not suppress either screen's untracked fallback.
    use sonicterm_grid::grid::Grid;
    use sonicterm_vt::vt::Parser;
    for is_alt in [false, true] {
        let mut parser = Parser::new(Grid::new(80, 24));
        if is_alt {
            parser.advance(b"\x1b[?1049h");
        }
        parser.advance(b"\x1b[?1006h");
        let modes = sonicterm_vt::vt::PointerModes::from_bits(parser.pointer_input_snapshot());
        let (tracking, sgr) = (modes.tracking(), modes.sgr());
        assert!(sgr);
        assert_eq!(tracking, MouseTracking::Off);
        assert_eq!(
            wheel_route(tracking, parser.grid().is_alt()),
            if is_alt { WheelRoute::CursorKeys } else { WheelRoute::LocalScrollback }
        );
    }
}

#[test]
fn wheel_route_parser_tracking_resets_restore_screen_fallback() {
    // Real DEC mode transitions own wheel routing independently of SGR and restore the correct fallback on reset.
    use sonicterm_grid::grid::Grid;
    use sonicterm_vt::vt::Parser;
    for (mode, expected) in [
        (1000, MouseTracking::Button),
        (1002, MouseTracking::ButtonMotion),
        (1003, MouseTracking::AnyMotion),
    ] {
        for is_alt in [false, true] {
            for sgr_enabled in [false, true] {
                let mut parser = Parser::new(Grid::new(80, 24));
                if is_alt {
                    parser.advance(b"\x1b[?1049h");
                }
                if sgr_enabled {
                    parser.advance(b"\x1b[?1006h");
                }
                parser.advance(format!("\x1b[?{mode}h").as_bytes());
                let modes =
                    sonicterm_vt::vt::PointerModes::from_bits(parser.pointer_input_snapshot());
                let (tracking, sgr) = (modes.tracking(), modes.sgr());
                assert_eq!(tracking, expected);
                assert_eq!(sgr, sgr_enabled);
                assert_eq!(wheel_route(tracking, parser.grid().is_alt()), WheelRoute::MouseReport);
                parser.advance(format!("\x1b[?{mode}l").as_bytes());
                let modes =
                    sonicterm_vt::vt::PointerModes::from_bits(parser.pointer_input_snapshot());
                let (tracking, sgr) = (modes.tracking(), modes.sgr());
                assert_eq!(tracking, MouseTracking::Off);
                assert_eq!(sgr, sgr_enabled);
                assert_eq!(
                    wheel_route(tracking, parser.grid().is_alt()),
                    if is_alt { WheelRoute::CursorKeys } else { WheelRoute::LocalScrollback }
                );
            }
        }
    }
}

#[test]
fn sgr_wheel_up_is_button_64() {
    // col=5, row=3, one tick up → ESC[<64;5;3M
    assert_eq!(wheel_report_bytes(true, true, 5, 3, 1), b"\x1b[<64;5;3M".to_vec());
}
