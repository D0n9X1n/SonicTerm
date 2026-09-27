use super::{
    begin_pointer_gesture, cancel_pointer_gesture, is_quit_chord, native_scrollbar_owns_pointer,
    no_button_motion_report, pointer_report_bytes, route_pressed_pointer_motion,
    take_focus_loss_pointer_release, take_pointer_release, terminal_repeat_targets,
    wheel_report_bytes, wheel_route, PointerCell, PointerGestureOwner, PointerMotionRoute,
    PointerReportKind, WheelRoute,
};
use crate::app::{child_window::child_no_button_motion_report, hovered_url::HoveredUrl, App};
use sonicterm_cfg::{
    config::{Config, ScrollbarMode},
    keymap::{Action, Keymap},
    theme::Theme,
};
use sonicterm_ui::{pane::SplitAxis, selection::Selection};
use sonicterm_vt::vt::MouseTracking;
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

/// The main dispatcher followed by the files that hold its handlers, so whole-file
/// absence and count checks cover every main-window event path.
const MAIN_SOURCES: &str = concat!(
    include_str!("window_event.rs"),
    include_str!("window_keyboard.rs"),
    include_str!("splitter_input.rs"),
    include_str!("window_pointer.rs")
);

/// Every source file that holds child-window code, so whole-file absence and count
/// checks cover all of it.
const CHILD_SOURCES: &str = concat!(
    include_str!("child_window.rs"),
    include_str!("child_tabs.rs"),
    include_str!("splitter_input.rs")
);

fn pointer_cell(pane_id: u64, row: u16, col: u16) -> PointerCell {
    PointerCell { pane_id, row, col }
}

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
        fn resumed(&mut self, el: &ActiveEventLoop) {
            run_readonly_native_matrix(el);
            self.ran = true;
            el.exit();
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

#[cfg(windows)]
fn run_readonly_native_matrix(el: &winit::event_loop::ActiveEventLoop) {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::phase,
    };
    use sonicterm_cfg::config::{BackdropKind, SoftwareRenderMode};
    use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
    use sonicterm_ui::copy_mode::CopyModeState;
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use winit::{
        dpi::{PhysicalPosition, PhysicalSize},
        event::{DeviceId, ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent},
        window::Window,
    };
    let (mut app, windows) = input_test_windows();
    app.config.appearance.scrollbar = ScrollbarMode::Never;
    app.tab_bar_visible = false;
    let native = Arc::new(
        el.create_window(
            Window::default_attributes()
                .with_visible(false)
                .with_active(false)
                .with_inner_size(PhysicalSize::new(640, 360)),
        )
        .unwrap(),
    );
    phase(0, "renderer-begin");
    let renderer = GpuRenderer::new(
        native.clone(),
        el,
        &app.theme,
        RendererSettings {
            font_family: &app.config.font.family,
            font_dirs: &[],
            font_size: 14.0,
            line_height_mult: 1.0,
            font_weight_scale: 1.0,
            subpixel_aa: app.config.font.subpixel_aa,
            padding: [0.0; 4],
            appearance: SurfaceAppearance {
                backdrop: BackdropKind::Opaque,
                opacity: 1.0,
                scrollbar: ScrollbarMode::Never,
                panel_padding: 0.0,
                software_render_mode: SoftwareRenderMode::Force,
            },
            role: "readonly-input-test",
        },
    )
    .unwrap();
    phase(0, "renderer-end");
    let mut renderer = Some(renderer);
    for (window, pane_id) in windows {
        let mut current = renderer.take().unwrap();
        current.set_tab_bar_visible(false);
        assert!(app.__test_attach_window_renderer(window, native.clone(), current));
        app.windows.get_mut(&window).unwrap().cursor_pos = (40.0, 80.0);
        app.__test_set_window_last_render(window, Instant::now() - Duration::from_secs(1));
        app.do_window_event(el, window, WindowEvent::RedrawRequested);
        let cell =
            app.windows[&window].renderer.as_ref().unwrap().pixel_to_pane_cell(40.0, 80.0).unwrap();
        assert_eq!(cell.0, pane_id, "rendered hit-test fixture must name the live pane");
        app.wait_for_input_queues();
        let submitted = PtySubmissions::start();
        for read_only in [false, true] {
            for tracked in [false, true] {
                for is_alt in [false, true] {
                    let ws = app.windows.get_mut(&window).unwrap();
                    ws.copy_mode = Some(if read_only {
                        CopyModeState::read_only_at((0, 0))
                    } else {
                        CopyModeState::new_at((0, 0))
                    });
                    ws.mouse_down = false;
                    ws.pointer_gesture = None;
                    let pane = ws.panes.get_mut(&pane_id).unwrap();
                    {
                        let mut parser = pane.parser.lock();
                        parser.advance(b"\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006h");
                        parser.advance("history\r\n".repeat(40).as_bytes());
                        if tracked {
                            parser.advance(b"\x1b[?1003h");
                        }
                        if is_alt {
                            parser.advance(b"\x1b[?1049h");
                        }
                    }
                    pane.viewport_top_abs = Some(10);
                    // Let the previous gesture leave the queue before testing this gesture's admission.
                    app.wait_for_input_queues();
                    phase(pane_id, "wheel");
                    app.do_window_event(
                        el,
                        window,
                        WindowEvent::MouseWheel {
                            device_id: DeviceId::dummy(),
                            delta: MouseScrollDelta::LineDelta(0.0, 1.0),
                            phase: TouchPhase::Moved,
                        },
                    );
                    let wheel = submitted.take();
                    let expected = if read_only || (!tracked && !is_alt) {
                        Vec::new()
                    } else if tracked {
                        vec![(
                            pane_id,
                            wheel_report_bytes(
                                true,
                                true,
                                u32::from(cell.2) + 1,
                                u32::from(cell.1) + 1,
                                3,
                            ),
                        )]
                    } else {
                        vec![(pane_id, b"\x1b[A\x1b[A\x1b[A".to_vec())]
                    };
                    assert_eq!(
                        wheel, expected,
                        "window={window:?} readonly={read_only} tracked={tracked} alt={is_alt}"
                    );
                    if !is_alt {
                        assert_eq!(
                            app.windows[&window].panes[&pane_id].viewport_top_abs,
                            if read_only || !tracked { Some(7) } else { Some(10) }
                        );
                    }
                    app.wait_for_input_queues();
                    phase(pane_id, "unheld-motion");
                    app.do_window_event(
                        el,
                        window,
                        WindowEvent::CursorMoved {
                            device_id: DeviceId::dummy(),
                            position: PhysicalPosition::new(40.0, 80.0),
                        },
                    );
                    app.flush_pointer_motion(Instant::now());
                    let motion = submitted.take();
                    assert_eq!(
                        motion,
                        if read_only || !tracked {
                            Vec::new()
                        } else {
                            vec![(
                                pane_id,
                                pointer_report_bytes(
                                    true,
                                    PointerReportKind::NoButtonMotion,
                                    ModifiersState::empty(),
                                    cell.1,
                                    cell.2,
                                ),
                            )]
                        },
                        "motion window={window:?} readonly={read_only} tracked={tracked}"
                    );
                    app.wait_for_input_queues();
                    phase(pane_id, "press");
                    app.do_window_event(
                        el,
                        window,
                        WindowEvent::MouseInput {
                            device_id: DeviceId::dummy(),
                            state: ElementState::Pressed,
                            button: MouseButton::Left,
                        },
                    );
                    assert_eq!(
                        submitted.take(),
                        if read_only || !tracked {
                            Vec::new()
                        } else {
                            vec![(
                                pane_id,
                                pointer_report_bytes(
                                    true,
                                    PointerReportKind::LeftPress,
                                    ModifiersState::empty(),
                                    cell.1,
                                    cell.2,
                                ),
                            )]
                        }
                    );
                    assert_eq!(
                        app.windows[&window].pointer_gesture.unwrap().owner
                            == PointerGestureOwner::Local,
                        read_only || !tracked
                    );
                    app.wait_for_input_queues();
                    app.do_window_event(
                        el,
                        window,
                        WindowEvent::MouseInput {
                            device_id: DeviceId::dummy(),
                            state: ElementState::Released,
                            button: MouseButton::Left,
                        },
                    );
                    submitted.take();
                }
            }
            // Windows OLE callbacks keep their registered window; writable cmd panes get double-quoted paths.
            app.wait_for_input_queues();
            phase(pane_id, "ole-drop");
            assert!(crate::os_drag_bridge::push_files(window, vec!["safe path".into()]));
            app.drain_os_drag();
            assert_eq!(
                submitted.take(),
                if read_only { Vec::new() } else { vec![(pane_id, b"\"safe path\"".to_vec())] },
                "OLE read_only={read_only}"
            );
            assert!(crate::os_drag_bridge::drain_file_drops().is_empty());
            app.wait_for_input_queues();
            phase(pane_id, "winit-drop");
            app.do_window_event(el, window, WindowEvent::DroppedFile("safe path".into()));
            assert!(submitted.take().is_empty(), "winit drops wait for the turn boundary");
            app.drain_winit_file_drops();
            assert_eq!(
                submitted.take(),
                if read_only { Vec::new() } else { vec![(pane_id, b"\"safe path\"".to_vec())] }
            );
        }
        renderer = app.windows.get_mut(&window).unwrap().renderer.take();
        app.windows.get_mut(&window).unwrap().window = None;
    }
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
                super::pointer_route_bytes(release, PointerReportKind::LeftRelease).unwrap();
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
                super::pointer_route_bytes(route, PointerReportKind::NoButtonMotion)
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
    assert!(route < main.find("self.handle_child_window_event(el, win_id, event)").unwrap());
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
    let split = dispatch.find("self.handle_child_window_event(el, win_id, event)").unwrap();
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
        let (tracking, sgr) = super::parser_mouse_profile(&parser);
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
                let (tracking, sgr) = super::parser_mouse_profile(&parser);
                assert_eq!(tracking, expected);
                assert_eq!(sgr, sgr_enabled);
                assert_eq!(wheel_route(tracking, parser.grid().is_alt()), WheelRoute::MouseReport);
                parser.advance(format!("\x1b[?{mode}l").as_bytes());
                let (tracking, sgr) = super::parser_mouse_profile(&parser);
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

/// A snapshot never borrows the parser or app, so a blocked handler cannot hide its last probe boundary.
struct NativeProbeStallState {
    probe: String,
    case: String,
    phase: String,
    parser_lock_held: bool,
    // Callback counts preserve delivery order even when adjacent clock readings are equal.
    heartbeat_count: u64,
    last_about_to_wait: Option<std::time::Instant>,
}

/// Heartbeats and disarm share a channel so neither watchdog wait needs polling or a sleep loop.
enum NativeWatchdogEvent {
    AboutToWait,
    Disarm,
}

/// Shares only short-lived diagnostic state locks with the watchdog, independently of the fixture's parser lock.
struct NativeProbeProgress {
    state: std::sync::Mutex<NativeProbeStallState>,
    events: std::sync::mpsc::Sender<NativeWatchdogEvent>,
}

impl NativeProbeProgress {
    /// Give the fixture shared progress and the watchdog sole ownership of its notification receiver.
    fn new() -> (std::sync::Arc<Self>, std::sync::mpsc::Receiver<NativeWatchdogEvent>) {
        let (events, receiver) = std::sync::mpsc::channel();
        (
            std::sync::Arc::new(Self {
                state: std::sync::Mutex::new(NativeProbeStallState {
                    probe: "fixture".into(),
                    case: "all".into(),
                    phase: "before_run_app".into(),
                    parser_lock_held: false,
                    heartbeat_count: 0,
                    last_about_to_wait: None,
                }),
                events,
            }),
            receiver,
        )
    }

    /// Publish the next operation before entering it, without keeping this lock during native or parser work.
    fn set(&self, probe: &str, case: &str, phase: impl Into<String>) {
        let mut state = self.state.lock().unwrap();
        state.probe = probe.into();
        state.case = case.into();
        state.phase = phase.into();
    }

    /// Bracket the deliberately held parser lock, including a stall while trying to acquire it.
    fn parser_lock(&self, held: bool) {
        self.state.lock().unwrap().parser_lock_held = held;
    }

    /// Mark callback entry before the loop can block in any of the fixture's probes.
    fn about_to_wait(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.heartbeat_count += 1;
            state.last_about_to_wait = Some(std::time::Instant::now());
        }
        // A finished watchdog no longer needs heartbeats; closed notification channels are harmless.
        let _ = self.events.send(NativeWatchdogEvent::AboutToWait);
    }
}

/// The guard owns the worker only until run_app returns, including its unwind path.
struct NativeWatchdog {
    disarm: std::sync::mpsc::Sender<NativeWatchdogEvent>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl NativeWatchdog {
    /// Start the injected expiry actions on a separate thread before any native callback runs.
    fn start(
        progress: std::sync::Arc<NativeProbeProgress>,
        events: std::sync::mpsc::Receiver<NativeWatchdogEvent>,
        deadline: std::time::Instant,
        wake: impl FnOnce() -> bool + Send + 'static,
        mut report: impl std::io::Write + Send + 'static,
        exit: impl FnOnce(i32) + Send + 'static,
    ) -> Self {
        let disarm = progress.events.clone();
        let worker = std::thread::spawn(move || {
            run_native_watchdog(&progress, &events, deadline, wake, &mut report, exit);
        });
        Self { disarm, worker: Some(worker) }
    }
}

// Lifecycle: NativeWatchdog disarms its channel wait and joins the worker before the fixture leaves scope.
impl Drop for NativeWatchdog {
    fn drop(&mut self) {
        // An already expired worker may have closed its receiver; disarm still must not mask a fixture failure.
        let _ = self.disarm.send(NativeWatchdogEvent::Disarm);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("native watchdog thread");
        }
    }
}

/// Expiry reports through Write rather than libtest capture, tries one wake, and bounds the follow-up to two seconds.
fn run_native_watchdog(
    progress: &NativeProbeProgress,
    events: &std::sync::mpsc::Receiver<NativeWatchdogEvent>,
    deadline: std::time::Instant,
    wake: impl FnOnce() -> bool,
    report: &mut impl std::io::Write,
    exit: impl FnOnce(i32),
) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match events.recv_timeout(remaining) {
            Ok(NativeWatchdogEvent::AboutToWait) => {}
            Ok(NativeWatchdogEvent::Disarm) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => break,
        }
    }
    let initial = {
        let state = progress.state.lock().unwrap();
        format!(
            "native watchdog expired: probe={} case={} phase={} parser_lock_held={} last_about_to_wait={:?}\n",
            state.probe, state.case, state.phase, state.parser_lock_held, state.last_about_to_wait,
        )
    };
    // Reporting failure cannot prevent the wake or nonzero exit; stderr may already be closed.
    let _ = report.write_all(initial.as_bytes());
    let wake_started = Instant::now();
    // Snapshot callback order, not clock time: an immediate heartbeat need not advance Instant.
    let heartbeats_before_wake = progress.state.lock().unwrap().heartbeat_count;
    let wake_sent = wake();
    let wake_deadline = wake_started + Duration::from_secs(2);
    let after_wake = loop {
        let observed = progress.state.lock().unwrap().heartbeat_count;
        if observed > heartbeats_before_wake {
            break true;
        }
        let remaining = wake_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break false;
        }
        match events.recv_timeout(remaining) {
            Ok(NativeWatchdogEvent::AboutToWait | NativeWatchdogEvent::Disarm) => {}
            Err(RecvTimeoutError::Disconnected | RecvTimeoutError::Timeout) => {
                break progress.state.lock().unwrap().heartbeat_count > heartbeats_before_wake;
            }
        }
    };
    // Direct write_all also keeps this post-wake verdict visible when libtest captures the stalled test's output.
    let _ = report.write_all(
        format!("native watchdog wake_sent={wake_sent} about_to_wait_after_wake={after_wake}\n")
            .as_bytes(),
    );
    exit(1);
}

/// Expiry names the stalled native step and distinguishes a delivered wake from an unresponsive loop.
#[test]
fn native_watchdog_expiry_reports_phase_lock_and_wake_progress() {
    use std::time::{Duration, Instant};

    for reaches_about_to_wait in [false, true] {
        let (progress, events) = NativeProbeProgress::new();
        progress.set("hover", "child/OSC8", "PointerContendedActive");
        progress.parser_lock(true);
        let mut report = Vec::new();
        let mut wakes = 0;
        let mut exit_code = None;
        run_native_watchdog(
            &progress,
            &events,
            Instant::now() + Duration::from_millis(2),
            || {
                wakes += 1;
                if reaches_about_to_wait {
                    progress.about_to_wait();
                }
                true
            },
            &mut report,
            |code| exit_code = Some(code),
        );
        let report = String::from_utf8(report).unwrap();
        assert!(
            report.contains("probe=hover case=child/OSC8 phase=PointerContendedActive"),
            "{report}"
        );
        assert!(report.contains("parser_lock_held=true"), "{report}");
        assert!(
            report.contains(&format!("about_to_wait_after_wake={reaches_about_to_wait}")),
            "{report}"
        );
        assert_eq!(wakes, 1);
        assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
    }
}

/// A delivered heartbeat counts as progress even when the clock cannot order its timestamp after the wake.
#[test]
fn native_watchdog_detects_heartbeat_without_a_clock_tick() {
    use std::time::{Duration, Instant};

    let (progress, events) = NativeProbeProgress::new();
    let before_watchdog = Instant::now();
    let mut report = Vec::new();
    let mut wakes = 0;
    let mut exit_code = None;
    run_native_watchdog(
        &progress,
        &events,
        Instant::now() + Duration::from_millis(2),
        || {
            wakes += 1;
            progress.about_to_wait();
            // Freeze the recorded time before the watchdog so callback ordering cannot depend on clock resolution.
            progress.state.lock().unwrap().last_about_to_wait = Some(before_watchdog);
            true
        },
        &mut report,
        |code| exit_code = Some(code),
    );
    let report = String::from_utf8(report).unwrap();
    assert!(report.contains("about_to_wait_after_wake=true"), "{report}");
    assert_eq!(wakes, 1);
    assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
}

/// A heartbeat recorded before the wake is not a response to it, even when it arrives after the watchdog starts.
#[test]
fn native_watchdog_does_not_count_pre_wake_heartbeat_as_response() {
    use std::time::{Duration, Instant};

    // The first report write runs after the deadline wait and before the wake baseline is sampled.
    struct PreWakeHeartbeat<'a> {
        first_write: Option<&'a NativeProbeProgress>,
        bytes: Vec<u8>,
    }
    impl std::io::Write for PreWakeHeartbeat<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(progress) = self.first_write.take() {
                progress.about_to_wait();
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (progress, events) = NativeProbeProgress::new();
    let mut report = PreWakeHeartbeat { first_write: Some(&progress), bytes: Vec::new() };
    let mut wakes = 0;
    let mut exit_code = None;
    run_native_watchdog(
        &progress,
        &events,
        Instant::now() + Duration::from_millis(2),
        || {
            wakes += 1;
            assert_eq!(
                progress.state.lock().unwrap().heartbeat_count,
                1,
                "heartbeat precedes wake"
            );
            true
        },
        &mut report,
        |code| exit_code = Some(code),
    );
    let report = String::from_utf8(report.bytes).unwrap();
    assert!(report.contains("about_to_wait_after_wake=false"), "{report}");
    assert_eq!(wakes, 1);
    assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
}

/// Once the deadline expires, a loop that returns during the wake grace period still fails instead of hiding the stall.
#[test]
fn native_watchdog_expiry_cannot_be_erased_by_a_late_disarm() {
    use std::time::{Duration, Instant};

    let (progress, events) = NativeProbeProgress::new();
    let mut report = Vec::new();
    let mut exit_code = None;
    run_native_watchdog(
        &progress,
        &events,
        Instant::now() + Duration::from_millis(2),
        || {
            progress.events.send(NativeWatchdogEvent::Disarm).unwrap();
            true
        },
        &mut report,
        |code| exit_code = Some(code),
    );
    assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
    assert!(String::from_utf8(report).unwrap().contains("about_to_wait_after_wake=false"));
}

/// Dropping the guard cancels its channel wait without waking, reporting, or invoking the exit action.
#[test]
fn native_watchdog_guard_disarms_before_deadline() {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    // Count writes rather than their content: a passing fixture must not produce even a partial report.
    struct ReportWrites(Arc<Mutex<usize>>);
    impl std::io::Write for ReportWrites {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            *self.0.lock().unwrap() += 1;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let writes = Arc::new(Mutex::new(0));
    let exit_code = Arc::new(Mutex::new(None));
    let exit_result = exit_code.clone();
    let (progress, events) = NativeProbeProgress::new();
    let guard = NativeWatchdog::start(
        progress,
        events,
        Instant::now() + Duration::from_secs(180),
        || panic!("a disarmed watchdog must not wake the event loop"),
        ReportWrites(writes.clone()),
        move |code| *exit_result.lock().unwrap() = Some(code),
    );
    drop(guard);
    assert_eq!(*writes.lock().unwrap(), 0);
    assert_eq!(*exit_code.lock().unwrap(), None);
}

#[cfg(windows)]
#[test]
fn native_main_and_child_handlers_preserve_wheel_and_modifier_selection_contracts() {
    // One native loop preserves wheel/selection ownership and proves quiet Ctrl-hover refreshes after parser contention.
    use std::time::{Duration, Instant};
    use winit::{
        application::ApplicationHandler,
        event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent},
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
        window::WindowId,
    };
    struct Probe {
        failures: Vec<String>,
        ran: bool,
        selection_probe: Option<ModifierSelectionProbe>,
        hover_probe: Option<HoverRetryProbe>,
        hover_case: usize,
        progress: std::sync::Arc<NativeProbeProgress>,
        proxy_user_event: bool,
        proxy_about_to_wait: bool,
    }
    impl ApplicationHandler for Probe {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            for child in [false, true] {
                let case = if child { "child" } else { "main" };
                self.progress.set("wheel", case, "setup");
                let mut app = App::new(Default::default(), Default::default(), Default::default());
                let main_pane = app.__test_seed_tab("wheel-main");
                let (window, pane_id) = if child {
                    let window = app.__test_seed_child_window(&["wheel-child"]);
                    let pane = app.__test_child_pane_ids(window).unwrap()[0];
                    app.__test_set_child_pane_viewport(
                        window,
                        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
                        10.0,
                        10.0,
                    );
                    (window, pane)
                } else {
                    app.__test_set_main_pane_viewport(
                        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
                        10.0,
                        10.0,
                    );
                    (app.main_window_id.unwrap(), main_pane)
                };
                self.progress.set("wheel", case, "spawn_pty");
                let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(
                    "cmd.exe",
                    &["/D".into(), "/Q".into()],
                    80,
                    24,
                )
                .unwrap();
                let input = pty.input_sender();
                let window_state = app.windows.get_mut(&window).unwrap();
                window_state.cursor_pos = (40.0, 40.0);
                let pane = window_state.panes.get_mut(&pane_id).unwrap();
                pane.pty = Some(pty);
                self.progress.set("wheel", case, "prepare_tracking");
                pane.parser.lock().advance("history\r\n".repeat(60).as_bytes());
                pane.viewport_top_abs = Some(10);
                pane.parser.lock().advance(b"\x1b[?1003h\x1b[?1006h");
                self.progress.set("wheel", case, "tracked_wheel");
                ApplicationHandler::window_event(
                    &mut app,
                    el,
                    window,
                    WindowEvent::MouseWheel {
                        device_id: DeviceId::dummy(),
                        delta: MouseScrollDelta::LineDelta(0.0, 1.0),
                        phase: TouchPhase::Moved,
                    },
                );
                self.progress.set("wheel", case, "wait_for_input");
                let deadline = Instant::now() + Duration::from_secs(1);
                while input.diagnostics().completed_messages == 0 && Instant::now() < deadline {
                    std::thread::yield_now();
                }
                self.progress.set("wheel", case, "check_tracked_wheel");
                let completed = input.diagnostics().completed_messages;
                let viewport = app.windows[&window].panes[&pane_id].viewport_top_abs;
                if completed != 1 || viewport != Some(10) {
                    self.failures.push(format!(
                        "child={child}: accepted/completed={completed}, viewport={viewport:?}"
                    ));
                }
                self.progress.set("wheel", case, "reset_tracking");
                app.windows
                    .get_mut(&window)
                    .unwrap()
                    .panes
                    .get_mut(&pane_id)
                    .unwrap()
                    .parser
                    .lock()
                    .advance(b"\x1b[?1003l");
                self.progress.set("wheel", case, "fallback_wheel");
                ApplicationHandler::window_event(
                    &mut app,
                    el,
                    window,
                    WindowEvent::MouseWheel {
                        device_id: DeviceId::dummy(),
                        delta: MouseScrollDelta::LineDelta(0.0, 1.0),
                        phase: TouchPhase::Moved,
                    },
                );
                self.progress.set("wheel", case, "check_fallback");
                let fallback = app.windows[&window].panes[&pane_id].viewport_top_abs;
                if fallback != viewport.map(|top| top.saturating_sub(3)) {
                    self.failures
                        .push(format!("child={child}: reset fallback viewport={fallback:?}"));
                }
                self.progress.set("wheel", case, "teardown");
            }
            self.ran = true;
            self.selection_probe = Some(ModifierSelectionProbe::new(el, self.progress.clone()));
        }
        // EventLoop<()> delivers proxy wakes here, not through the production App user-event type.
        fn user_event(&mut self, _: &ActiveEventLoop, (): ()) {
            self.proxy_user_event = true;
        }
        fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            if let Some(probe) = self.selection_probe.as_mut() {
                probe.event(el, id, event, &mut self.failures);
            } else if let Some(probe) = self.hover_probe.as_mut() {
                probe.event(el, id, event);
            }
        }
        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            self.progress.about_to_wait();
            self.proxy_about_to_wait |= self.proxy_user_event;
            if self.selection_probe.as_mut().is_some_and(|probe| probe.poll(&mut self.failures)) {
                self.progress.set("selection", "main/child", "teardown");
                self.selection_probe = None;
                self.hover_probe =
                    Some(HoverRetryProbe::new(el, self.hover_case, self.progress.clone()));
            } else if self.hover_probe.as_mut().is_some_and(|probe| probe.poll(el)) {
                let probe = self.hover_probe.as_ref().unwrap();
                self.progress.set("hover", probe.case, "teardown");
                self.hover_probe = None;
                self.hover_case += 1;
                if self.hover_case == 4 {
                    self.progress.set("fixture", "all", "exit_event_loop");
                    el.exit();
                    return;
                }
                self.hover_probe =
                    Some(HoverRetryProbe::new(el, self.hover_case, self.progress.clone()));
            }
            el.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(5),
            ));
        }
    }
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let (progress, events) = NativeProbeProgress::new();
    let proxy = event_loop.create_proxy();
    let watchdog = NativeWatchdog::start(
        progress.clone(),
        events,
        Instant::now() + Duration::from_secs(180),
        move || proxy.send_event(()).is_ok(),
        std::io::stderr(),
        |code| sonicterm_logging::exit_with(code, "native wheel/selection/hover watchdog expired"),
    );
    let mut probe = Probe {
        failures: Vec::new(),
        ran: false,
        selection_probe: None,
        hover_probe: None,
        hover_case: 0,
        progress,
        proxy_user_event: false,
        proxy_about_to_wait: false,
    };
    // A real proxy event must pass user_event and then about_to_wait; polling alone cannot satisfy this control.
    event_loop.create_proxy().send_event(()).expect("native watchdog proxy control");
    let result = event_loop.run_app(&mut probe);
    drop(watchdog);
    result.unwrap();
    assert!(probe.ran);
    assert!(probe.proxy_user_event, "proxy wake must reach user_event");
    assert!(probe.proxy_about_to_wait, "about_to_wait must follow the proxy user_event");
    assert!(probe.failures.is_empty(), "{}", probe.failures.join("; "));
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
enum HoverRetryPhase {
    Setup,
    BaselineActive,
    BaselineInactive,
    ContendedActive,
    ContendedInactive,
    PointerReady,
    PointerBaselineBlank,
    PointerBaselineActive,
    PointerContendedBlank,
    PointerContendedActive,
}

#[cfg(windows)]
struct HoverRetryProbe {
    app: App,
    native_id: winit::window::WindowId,
    tracked_id: winit::window::WindowId,
    pane_id: u64,
    case: &'static str,
    progress: std::sync::Arc<NativeProbeProgress>,
    phase: HoverRetryPhase,
    cycle: usize,
    deadline: std::time::Instant,
    phase_started: std::time::Instant,
    last_native_frame: std::time::Instant,
    native_frames: u64,
    phase_native_start: u64,
    phase_present_start: u64,
    active_pixels: Vec<[u8; 4]>,
    inactive_pixels: Vec<[u8; 4]>,
    blank_pixels: Vec<[u8; 4]>,
}

#[cfg(windows)]
impl HoverRetryProbe {
    const URI: &'static str = "https://example.com/docs";
    const QUIET: std::time::Duration = std::time::Duration::from_millis(200);

    /// Publish setup milestones before native window and renderer construction can block.
    fn new(
        el: &winit::event_loop::ActiveEventLoop,
        case_index: usize,
        progress: std::sync::Arc<NativeProbeProgress>,
    ) -> Self {
        use sonicterm_cfg::config::{BackdropKind, SoftwareRenderMode, SubpixelAaMode};
        use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
        use std::{sync::Arc, time::Instant};
        use winit::{dpi::PhysicalSize, window::Window};
        let case = ["main/plain", "main/OSC8", "child/plain", "child/OSC8"][case_index];
        progress.set("hover", case, "Setup/create_window");
        let window = Arc::new(
            el.create_window(
                Window::default_attributes()
                    .with_inner_size(PhysicalSize::new(640, 360))
                    .with_active(false)
                    .with_title("SonicTerm quiet hover regression"),
            )
            .unwrap(),
        );
        let native_id = window.id();
        let theme = Theme::default();
        let mut config = Config::default();
        config.font.size = 14.0;
        config.font.subpixel_aa = SubpixelAaMode::Rgb;
        config.appearance.backdrop = BackdropKind::Opaque;
        config.appearance.opacity = 1.0;
        config.appearance.software_render_mode = SoftwareRenderMode::Force;
        config.appearance.scrollbar = ScrollbarMode::Never;
        config.window.padding_left = 0.0;
        config.window.padding_right = 0.0;
        config.window.padding_top = 0.0;
        config.window.padding_bottom = 0.0;
        progress.set("hover", case, "Setup/create_renderer");
        let mut renderer = GpuRenderer::new(
            window.clone(),
            el,
            &theme,
            RendererSettings {
                font_family: &config.font.family,
                font_dirs: &[],
                font_size: config.font.size,
                line_height_mult: config.font.line_height,
                font_weight_scale: config.font.effective_weight_scale(),
                subpixel_aa: config.font.subpixel_aa,
                padding: [0.0; 4],
                appearance: SurfaceAppearance {
                    backdrop: config.appearance.backdrop,
                    opacity: 1.0,
                    scrollbar: config.appearance.scrollbar,
                    panel_padding: 0.0,
                    software_render_mode: SoftwareRenderMode::Force,
                },
                role: "quiet-hover-test",
            },
        )
        .unwrap();
        renderer.set_tab_bar_visible(false);
        renderer.set_cursor_blink(false);
        progress.set("hover", case, "Setup/create_pane");
        let mut app = App::new(theme, config, Keymap::default());
        app.__test_set_software_render_degrade(true);
        let (tracked_id, pane_id) = if case_index < 2 {
            let pane = app.__test_seed_tab("hover-main");
            (app.main_window_id.unwrap(), pane)
        } else {
            let id = app.__test_seed_child_window(&["hover-child"]);
            (id, app.windows[&id].tab_states[0].active_pane)
        };
        assert!(app.__test_attach_window_renderer(tracked_id, window, renderer));
        app.windows.get_mut(&tracked_id).unwrap().cursor_pos = (-100.0, -100.0);
        let label = if case_index.is_multiple_of(2) {
            Self::URI.to_owned()
        } else {
            format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", Self::URI, Self::URI)
        };
        // Identical terminal cells isolate OSC 8 metadata; alternate-screen mouse reporting stays enabled without a PTY.
        app.windows[&tracked_id].panes[&pane_id].parser.lock().advance(
            format!("\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?25l\x1b[2;2H{label}\x1b[6;1H")
                .as_bytes(),
        );
        assert!(app.path_workers.is_none());
        assert!(app.windows[&tracked_id].panes[&pane_id].pty.is_none());
        let now = Instant::now();
        let mut probe = Self {
            app,
            native_id,
            tracked_id,
            pane_id,
            case,
            progress,
            phase: HoverRetryPhase::Setup,
            cycle: 0,
            deadline: now + std::time::Duration::from_secs(20),
            phase_started: now,
            last_native_frame: now,
            native_frames: 0,
            phase_native_start: 0,
            phase_present_start: 0,
            active_pixels: Vec::new(),
            inactive_pixels: Vec::new(),
            blank_pixels: Vec::new(),
        };
        // Focus supplies initial layout independently of the modifier scheduling contract under test.
        probe.progress.set("hover", case, "Setup/focus");
        winit::application::ApplicationHandler::window_event(
            &mut probe.app,
            el,
            tracked_id,
            winit::event::WindowEvent::Focused(true),
        );
        probe.progress.set("hover", case, "Setup");
        probe
    }

    fn assert_unscheduled(&self) {
        assert!(
            !self.app.pending_redraw
                && self.app.pending_redraw_windows.is_empty()
                && self.app.windows[&self.tracked_id].retry_not_before.is_none(),
            "INVALID {} {:?}: pacing or parser retry contaminated the native observation",
            self.case,
            self.phase,
        );
    }

    fn event(
        &mut self,
        el: &winit::event_loop::ActiveEventLoop,
        id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        if id != self.native_id || !matches!(event, winit::event::WindowEvent::RedrawRequested) {
            return;
        }
        self.progress.set(
            "hover",
            self.case,
            format!("{:?}/render cycle={}", self.phase, self.cycle),
        );
        assert!(
            self.app.windows[&self.tracked_id].panes[&self.pane_id].parser.try_lock().is_some(),
            "INVALID {} {:?}: native frame arrived while parser lock was held",
            self.case,
            self.phase,
        );
        self.assert_unscheduled();
        // Backdate pacing only after native delivery; frame admission cannot supply a missing native redraw request.
        assert!(self.app.__test_set_window_last_render(
            self.tracked_id,
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        ));
        self.native_frames += 1;
        winit::application::ApplicationHandler::window_event(
            &mut self.app,
            el,
            self.tracked_id,
            event,
        );
        self.last_native_frame = std::time::Instant::now();
        self.assert_unscheduled();
    }

    fn row_pixels(&self) -> Vec<[u8; 4]> {
        let renderer = self.app.windows[&self.tracked_id].renderer.as_ref().unwrap();
        let [x, y] = renderer.pane_grid_origin(self.pane_id).expect("native layout must exist");
        let (cw, ch) = renderer.cell_size();
        // Restrict readback to the URI row and prove the real tooltip geometry cannot cover its pixels.
        if let Some(preview) = self.app.windows[&self.tracked_id].link_preview.as_ref() {
            let font_size = renderer.font_size().max(1.0) * renderer.scale_factor();
            let layout = sonicterm_ui::overlays::LinkPreviewLayout::compute(
                &sonicterm_ui::overlays::link_preview_text(&preview.uri),
                preview.pointer,
                renderer.logical_size(),
                font_size * 1.4,
                renderer.scale_factor(),
                |text| renderer.measure_overlay_text_width(text, font_size),
            )
            .expect("baseline preview must fit the native window");
            assert!(
                layout.border.y >= (y + 2.0 * ch).ceil()
                    || layout.border.y + layout.border.h <= (y + ch).floor(),
                "INVALID {}: link preview overlaps target-row readback",
                self.case,
            );
        }
        ((y + ch).floor() as u32..(y + 2.0 * ch).ceil() as u32)
            .flat_map(|py| {
                ((x + cw).floor() as u32..(x + (1 + Self::URI.len()) as f32 * cw).ceil() as u32)
                    .map(move |px| (px, py))
            })
            .map(|(x, y)| {
                self.app
                    .__test_window_software_frame_pixel_bgra(self.tracked_id, x, y)
                    .expect("forced software frame must expose target-row pixels")
            })
            .collect()
    }

    fn assert_hover(&self, active: bool) {
        let hover = self.app.windows[&self.tracked_id]
            .hovered_url
            .as_ref()
            .expect("stationary URI must retain its hover identity");
        assert_eq!(hover.url, Self::URI, "{} {:?}", self.case, self.phase);
        assert_eq!(hover.active(), active, "{} {:?}", self.case, self.phase);
        assert_eq!(self.app.frontmost_window, Some(self.tracked_id));
    }

    /// The watchdog sees every modifier and pointer phase, including its repeated cycle, before the stimulus runs.
    fn begin_phase(&mut self, phase: HoverRetryPhase) {
        self.progress.set("hover", self.case, format!("{phase:?} cycle={}", self.cycle));
        self.assert_unscheduled();
        let period = crate::app::effective_frame_period(true, false, self.app.frame_period);
        assert!(self.app.windows[&self.tracked_id].last_render.elapsed() > period);
        self.phase = phase;
        self.phase_started = std::time::Instant::now();
        self.phase_native_start = self.native_frames;
        self.phase_present_start =
            self.app.windows[&self.tracked_id].renderer.as_ref().unwrap().successful_frame_count();
    }

    /// Keep the fixture's lock flag visible throughout the real modifier lookup, but clear it before rendering.
    fn modifiers(
        &mut self,
        el: &winit::event_loop::ActiveEventLoop,
        active: bool,
        contended: bool,
        phase: HoverRetryPhase,
    ) {
        self.begin_phase(phase);
        let parser = self.app.windows[&self.tracked_id].panes[&self.pane_id].parser.clone();
        let previous = self.app.windows[&self.tracked_id].hovered_url.clone();
        self.progress.parser_lock(contended);
        let guard = contended.then(|| parser.lock());
        winit::application::ApplicationHandler::window_event(
            &mut self.app,
            el,
            self.tracked_id,
            winit::event::WindowEvent::ModifiersChanged(
                if active { ModifiersState::CONTROL } else { ModifiersState::empty() }.into(),
            ),
        );
        if contended && active {
            assert_eq!(self.app.windows[&self.tracked_id].hovered_url, previous);
            self.assert_hover(false);
            assert!(self.app.windows[&self.tracked_id].link_preview.is_none());
        } else {
            self.assert_hover(active);
        }
        if contended && !active {
            assert!(!self.app.windows[&self.tracked_id].hover_link);
            assert!(self.app.windows[&self.tracked_id].link_preview.is_none());
        }
        // The lock covers modifier lookup only; rendering with it held would test a different retry mechanism.
        drop(guard);
        self.progress.parser_lock(false);
        self.assert_unscheduled();
    }

    /// Diagnose a held-lock pointer lookup without turning its subsequent redraw into a parser-contention test.
    fn pointer_refresh(&mut self, on_uri: bool, contended: bool, phase: HoverRetryPhase) {
        self.begin_phase(phase);
        assert_eq!(self.app.windows[&self.tracked_id].modifiers, ModifiersState::CONTROL);
        let window = self.app.windows.get_mut(&self.tracked_id).unwrap();
        let renderer = window.renderer.as_ref().unwrap();
        let [x, y] = renderer.pane_grid_origin(self.pane_id).unwrap();
        let (cw, ch) = renderer.cell_size();
        window.cursor_pos =
            ((x + 4.5 * cw) as f64, (y + if on_uri { 1.5 } else { 5.5 } * ch) as f64);
        let parser = window.panes[&self.pane_id].parser.clone();
        let previous = window.hovered_url.clone();
        self.progress.parser_lock(contended);
        let guard = contended.then(|| parser.lock());
        // This is the shared pointer-refresh seam, not CursorMoved: its later mouse-report lock would block this fixture.
        self.app.refresh_target_hover(self.tracked_id);
        if contended {
            assert_eq!(self.app.windows[&self.tracked_id].hovered_url, previous);
            assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
            assert!(self.app.windows[&self.tracked_id].link_preview.is_none());
        } else if on_uri {
            self.assert_hover(true);
        } else {
            assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
        }
        drop(guard);
        self.progress.parser_lock(false);
        self.assert_unscheduled();
    }

    fn poll(&mut self, el: &winit::event_loop::ActiveEventLoop) -> bool {
        use HoverRetryPhase::{
            BaselineActive, BaselineInactive, ContendedActive, ContendedInactive,
            PointerBaselineActive, PointerBaselineBlank, PointerContendedActive,
            PointerContendedBlank, PointerReady, Setup,
        };
        self.progress.set(
            "hover",
            self.case,
            format!("{:?}/poll cycle={}", self.phase, self.cycle),
        );
        let now = std::time::Instant::now();
        assert!(
            now < self.deadline,
            "INVALID {} {:?}: hover watchdog expired",
            self.case,
            self.phase
        );
        if now.duration_since(self.phase_started.max(self.last_native_frame)) < Self::QUIET {
            return false;
        }
        self.assert_unscheduled();
        let frames = self.native_frames - self.phase_native_start;
        let presents =
            self.app.windows[&self.tracked_id].renderer.as_ref().unwrap().successful_frame_count()
                - self.phase_present_start;
        let hover = self.app.windows[&self.tracked_id].hovered_url.as_ref();
        eprintln!(
            "hover case={} phase={:?} cycle={} native_frames={frames} presents={presents} uri={:?} active={:?}",
            self.case,
            self.phase,
            self.cycle,
            hover.map(|hover| hover.url.as_str()),
            hover.map(HoveredUrl::active),
        );
        if matches!(self.phase, ContendedActive | ContendedInactive | PointerContendedActive) {
            assert!(
                frames > 0,
                "{} {:?} cycle={}: 0 native frames in the quiet window",
                self.case,
                self.phase,
                self.cycle
            );
        } else {
            assert!(
                frames > 0,
                "INVALID {} {:?}: baseline received 0 native frames",
                self.case,
                self.phase
            );
        }
        assert!(
            presents > 0,
            "INVALID {} {:?}: native redraw produced no presentation",
            self.case,
            self.phase
        );
        match self.phase {
            Setup => {
                let window = self.app.windows.get_mut(&self.tracked_id).unwrap();
                let renderer = window.renderer.as_ref().unwrap();
                assert!(
                    renderer.__test_pane_focus_flash_target().is_none(),
                    "INVALID: setup focus flash must not affect the baseline"
                );
                let [x, y] = renderer.pane_grid_origin(self.pane_id).expect("setup layout");
                let (cw, ch) = renderer.cell_size();
                window.cursor_pos = ((x + 4.5 * cw) as f64, (y + 1.5 * ch) as f64);
                self.modifiers(el, true, false, BaselineActive);
            }
            BaselineActive => {
                self.assert_hover(true);
                self.active_pixels = self.row_pixels();
                self.modifiers(el, false, false, BaselineInactive);
            }
            BaselineInactive => {
                self.assert_hover(false);
                self.inactive_pixels = self.row_pixels();
                assert_ne!(
                    self.active_pixels, self.inactive_pixels,
                    "INVALID {}: baseline target-row pixels do not distinguish Ctrl",
                    self.case
                );
                self.modifiers(el, true, true, ContendedActive);
            }
            ContendedActive => {
                self.assert_hover(true);
                assert_eq!(
                    self.row_pixels(),
                    self.active_pixels,
                    "{} cycle={}: contended Ctrl must paint the free-lock active row",
                    self.case,
                    self.cycle
                );
                self.modifiers(el, false, true, ContendedInactive);
            }
            ContendedInactive => {
                self.assert_hover(false);
                assert_eq!(
                    self.row_pixels(),
                    self.inactive_pixels,
                    "{} cycle={}: contended release must restore the free-lock inactive row",
                    self.case,
                    self.cycle
                );
                self.cycle += 1;
                if self.cycle == 3 {
                    self.cycle = 0;
                    self.modifiers(el, true, false, PointerReady);
                } else {
                    self.modifiers(el, true, true, ContendedActive);
                }
            }
            PointerReady => {
                self.assert_hover(true);
                self.pointer_refresh(false, false, PointerBaselineBlank);
            }
            PointerBaselineBlank => {
                assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
                self.blank_pixels = self.row_pixels();
                self.pointer_refresh(true, false, PointerBaselineActive);
            }
            PointerBaselineActive => {
                self.assert_hover(true);
                assert_eq!(
                    self.row_pixels(),
                    self.active_pixels,
                    "{}: free pointer refresh must match the modifier baseline",
                    self.case
                );
                assert_ne!(
                    self.row_pixels(),
                    self.blank_pixels,
                    "INVALID {}: blank and active pointer baselines must differ",
                    self.case
                );
                self.pointer_refresh(false, false, PointerContendedBlank);
            }
            PointerContendedBlank => {
                assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
                assert_eq!(
                    self.row_pixels(),
                    self.blank_pixels,
                    "{}: blank row must settle before the held-lock pointer refresh",
                    self.case
                );
                self.pointer_refresh(true, true, PointerContendedActive);
            }
            PointerContendedActive => {
                self.assert_hover(true);
                assert_eq!(self.row_pixels(), self.active_pixels, "{} cycle={}: contended pointer-refresh seam must paint the free-lock active row", self.case, self.cycle);
                self.cycle += 1;
                if self.cycle == 3 {
                    return true;
                }
                self.pointer_refresh(false, false, PointerContendedBlank);
            }
        }
        false
    }
}

#[cfg(windows)]
struct ModifierSelectionProbe {
    app: App,
    progress: std::sync::Arc<NativeProbeProgress>,
    window: winit::window::Window,
    targets: Vec<(winit::window::WindowId, u64, sonicterm_io::pty::PtyInputSender)>,
    steps: std::collections::VecDeque<(bool, u16, u16, bool, bool)>,
    in_flight: Option<(bool, u16, u16, bool, bool)>,
    completed: u64,
    deadline: std::time::Instant,
}

#[cfg(windows)]
impl ModifierSelectionProbe {
    /// Name native construction and per-window PTY setup separately from the subsequent key-delivery phases.
    fn new(
        el: &winit::event_loop::ActiveEventLoop,
        progress: std::sync::Arc<NativeProbeProgress>,
    ) -> Self {
        use winit::window::Window;
        progress.set("selection", "main/child", "create_window");
        let window = el
            .create_window(Window::default_attributes().with_visible(false).with_active(false))
            .unwrap();
        let mut app = App::new(Default::default(), Default::default(), Default::default());
        app.clipboard = None;
        app.test_clipboard_text = Some("clipboard sentinel".into());
        app.keymap = Keymap::parse_resilient("[meta]\nname = \"selection\"\nversion = \"1.0\"\n[[binding]]\nkeys = \"ctrl+shift+c\"\naction = \"copy_to_clipboard\"\n", "native selection fixture").unwrap();
        app.__test_enable_pty_write_log();
        let main_pane = app.__test_seed_tab("modifier-main");
        let main = app.main_window_id.unwrap();
        let child = app.__test_seed_child_window(&["modifier-child"]);
        let child_pane = app.__test_child_pane_ids(child).unwrap()[0];
        let mut targets = Vec::new();
        for (window_id, pane_id) in [(main, main_pane), (child, child_pane)] {
            let case = if window_id == main { "main" } else { "child" };
            progress.set("selection", case, "spawn_pty");
            let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(
                "cmd.exe",
                &["/D".into(), "/Q".into()],
                80,
                24,
            )
            .unwrap();
            let input = pty.input_sender();
            let pane = app.windows.get_mut(&window_id).unwrap().panes.get_mut(&pane_id).unwrap();
            pane.pty = Some(pty);
            progress.set("selection", case, "prepare_parser");
            let mut parser = pane.parser.lock();
            parser.advance(b"selected text\x1b[?9001h");
            pane.keyboard_input
                .store(parser.keyboard_input_snapshot(), std::sync::atomic::Ordering::Relaxed);
            targets.push((window_id, pane_id, input));
        }
        let mut steps = std::collections::VecDeque::new();
        for kitty in [false, true] {
            for (vk, scan) in [(0x11, 0x1d), (0x10, 0x2a), (0x43, 0x2e), (0x58, 0x2d), (0x59, 0x15)]
            {
                steps.extend([
                    (kitty, vk, scan, true, false),
                    (kitty, vk, scan, true, true),
                    (kitty, vk, scan, false, false),
                ]);
            }
        }
        progress.set("selection", "main/child", "ready");
        Self {
            app,
            progress,
            window,
            targets,
            steps,
            in_flight: None,
            completed: 0,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(15),
        }
    }

    fn poll(&mut self, failures: &mut Vec<String>) -> bool {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::{
            Foundation::{HWND, LPARAM, WPARAM},
            UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP},
        };
        self.progress.set(
            "selection",
            "main/child",
            format!("poll in_flight={:?}", self.in_flight),
        );
        if std::time::Instant::now() >= self.deadline {
            failures.push(format!("modifier input deadline: {:?}", self.in_flight));
            return true;
        }
        if self.in_flight.is_some()
            || self
                .targets
                .iter()
                .any(|(_, _, input)| input.diagnostics().completed_messages < self.completed)
        {
            return false;
        }
        let Some(stroke @ (kitty, vk, scan, down, repeat)) = self.steps.pop_front() else {
            return true;
        };
        if down && !repeat {
            for (window_id, pane_id, _) in &self.targets {
                let case =
                    if Some(*window_id) == self.app.main_window_id { "main" } else { "child" };
                self.progress.set(
                    "selection",
                    case,
                    format!("prepare kitty={kitty} vk={vk} down={down} repeat={repeat}"),
                );
                let pane = &self.app.windows[window_id].panes[pane_id];
                let mut parser = pane.parser.lock();
                parser.advance(if kitty { b"\x1b[=10u" } else { b"\x1b[=0u" });
                pane.keyboard_input
                    .store(parser.keyboard_input_snapshot(), std::sync::atomic::Ordering::Relaxed);
                drop(parser);
                if vk == 0x43 {
                    // Copy must use the selection preserved across the preceding Shift lifecycle, not a fresh fixture range.
                    continue;
                }
                let mut selection = Selection::new(0, 0);
                selection.end = (0, 8);
                if Some(*window_id) == self.app.main_window_id {
                    self.app.__test_set_main_selection(Some(selection));
                } else {
                    self.app.__test_set_child_selection(*window_id, Some(selection));
                }
            }
        }
        let RawWindowHandle::Win32(handle) = self.window.window_handle().unwrap().as_raw() else {
            panic!("Windows handle");
        };
        let bits = 1
            | (u32::from(scan) << 16)
            | (u32::from(repeat || !down) << 30)
            | (u32::from(!down) << 31);
        self.in_flight = Some(stroke);
        self.progress.set(
            "selection",
            "main/child",
            format!("post kitty={kitty} vk={vk} down={down} repeat={repeat}"),
        );
        // SAFETY: this test owns the destination HWND; only scalar key metadata is posted to its message queue.
        unsafe {
            PostMessageW(
                Some(HWND(handle.hwnd.get() as *mut _)),
                if down { WM_KEYDOWN } else { WM_KEYUP },
                WPARAM(usize::from(vk)),
                LPARAM(bits as isize),
            )
        }
        .unwrap();
        false
    }

    fn event(
        &mut self,
        el: &winit::event_loop::ActiveEventLoop,
        id: winit::window::WindowId,
        event: winit::event::WindowEvent,
        failures: &mut Vec<String>,
    ) {
        use winit::{event::WindowEvent, platform::windows::KeyEventExtWindows};
        if id != self.window.id() {
            return;
        }
        let WindowEvent::KeyboardInput { event: key, is_synthetic: false, .. } = &event else {
            return;
        };
        let Some((kitty, vk, scan, down, repeat)) = self.in_flight.take() else {
            failures.push("unrequested native key".into());
            return;
        };
        self.progress.set(
            "selection",
            "main/child",
            format!("native_key kitty={kitty} vk={vk} down={down} repeat={repeat}"),
        );
        let native = key.native_key_event().expect("posted key must carry native metadata");
        assert_eq!((native.virtual_key, native.scan_code, native.key_down), (vk, scan, down));
        assert_eq!(key.repeat, repeat);
        let physical = key.physical_key;
        let modifier = vk == 0x11 || vk == 0x10;
        if !modifier {
            assert!(matches!(key.logical_key, winit::keyboard::Key::Character(_)));
        }
        let copy_chord = vk == 0x43;
        for (window_id, pane_id, _) in &self.targets {
            let case = if Some(*window_id) == self.app.main_window_id { "main" } else { "child" };
            self.progress.set(
                "selection",
                case,
                format!("dispatch kitty={kitty} vk={vk} down={down} repeat={repeat}"),
            );
            // The native event is retained; only aggregate modifiers are controlled for copy and AltGr policy checks.
            let mods = if modifier {
                ModifiersState::empty()
            } else if copy_chord {
                ModifiersState::CONTROL | ModifiersState::SHIFT
            } else if vk == 0x58 {
                ModifiersState::CONTROL | ModifiersState::ALT
            } else {
                ModifiersState::empty()
            };
            self.app.windows.get_mut(window_id).unwrap().modifiers = mods;
            let writes_before = self.app.__test_pty_write_log().len();
            winit::application::ApplicationHandler::window_event(
                &mut self.app,
                el,
                *window_id,
                event.clone(),
            );
            self.progress.set(
                "selection",
                case,
                format!("check kitty={kitty} vk={vk} down={down} repeat={repeat}"),
            );
            let state = &self.app.windows[window_id];
            if state.selection.is_some() != (modifier || copy_chord) {
                failures.push(format!("window={window_id:?} kitty={kitty} vk={vk} down={down} repeat={repeat}: selection_present={}", state.selection.is_some()));
            }
            let held = state.pty_pressed_keys.get(&physical).and_then(|routes| routes.get(pane_id));
            assert_eq!(
                held.is_some(),
                down && !copy_chord,
                "only admitted press/repeat owns a matching release"
            );
            if down && !copy_chord {
                assert_eq!(matches!(held, Some(crate::app::HeldKey::Legacy)), kitty);
            }
            let writes = self.app.__test_pty_write_log();
            if copy_chord {
                assert_eq!(writes.len(), writes_before, "copy shortcut never leaks terminal input");
                if down {
                    assert_eq!(self.app.test_clipboard_text.as_deref(), Some("selected"));
                    self.app.test_clipboard_text = Some("clipboard sentinel".into());
                }
            } else {
                assert_eq!(writes.len(), writes_before + 1);
                let bytes = &writes.last().unwrap().1;
                if kitty {
                    assert!(
                        bytes.starts_with(b"\x1b[") && bytes.ends_with(b"u"),
                        "Kitty report-all record: {bytes:?}"
                    );
                } else {
                    assert_eq!(
                        *bytes,
                        crate::app::key_encoding::encode_win32_key(
                            crate::app::key_encoding::Win32KeyEvent {
                                virtual_key: native.virtual_key,
                                scan_code: native.scan_code,
                                unicode: &native.unicode,
                                key_down: native.key_down,
                                control_key_state: native.control_key_state,
                                repeat_count: native.repeat_count
                            }
                        )
                    );
                }
            }
        }
        if !copy_chord {
            self.completed += 1;
        }
        assert_eq!(self.app.test_clipboard_text.as_deref(), Some("clipboard sentinel"));
    }
}

#[test]
fn sgr_wheel_up_is_button_64() {
    // col=5, row=3, one tick up → ESC[<64;5;3M
    assert_eq!(wheel_report_bytes(true, true, 5, 3, 1), b"\x1b[<64;5;3M".to_vec());
}

#[test]
fn sgr_wheel_down_is_button_65() {
    assert_eq!(wheel_report_bytes(true, false, 5, 3, 1), b"\x1b[<65;5;3M".to_vec());
}

#[test]
fn sgr_emits_one_report_per_line() {
    // 3 ticks → three concatenated reports.
    assert_eq!(
        wheel_report_bytes(true, true, 1, 1, 3),
        b"\x1b[<64;1;1M\x1b[<64;1;1M\x1b[<64;1;1M".to_vec()
    );
}

#[test]
fn legacy_x10_encodes_button_and_coords_plus_32() {
    // up=button 64 → 64+32=96 ('`'); col 5 → 37 ('%'); row 3 → 35 ('#').
    assert_eq!(wheel_report_bytes(false, true, 5, 3, 1), vec![0x1b, b'[', b'M', 96, 37, 35]);
}

#[test]
fn legacy_x10_clamps_large_coords() {
    // col/row clamp to 223 so +32 stays within a byte (255).
    let out = wheel_report_bytes(false, false, 9999, 9999, 1);
    assert_eq!(out, vec![0x1b, b'[', b'M', 97, 255, 255]); // 65+32=97
}

#[test]
fn sgr_pointer_reports_encode_press_release_and_motion_modifiers() {
    // SGR keeps left-button Cb on release, uses lowercase `m`, and adds the
    // current modifier and motion bits without changing one-based coordinates.
    assert_eq!(
        pointer_report_bytes(true, PointerReportKind::LeftPress, ModifiersState::empty(), 0, 0,),
        b"\x1b[<0;1;1M".to_vec()
    );
    assert_eq!(
        pointer_report_bytes(true, PointerReportKind::LeftRelease, ModifiersState::empty(), 0, 0,),
        b"\x1b[<0;1;1m".to_vec()
    );
    assert_eq!(
        pointer_report_bytes(true, PointerReportKind::HeldLeftMotion, ModifiersState::ALT, 0, 0),
        b"\x1b[<40;1;1M".to_vec()
    );
    assert_eq!(
        pointer_report_bytes(
            true,
            PointerReportKind::NoButtonMotion,
            ModifiersState::SHIFT | ModifiersState::CONTROL,
            0,
            0,
        ),
        b"\x1b[<55;1;1M".to_vec()
    );
    assert_eq!(
        pointer_report_bytes(true, PointerReportKind::LeftPress, ModifiersState::SUPER, 0, 0,),
        b"\x1b[<8;1;1M".to_vec()
    );
}

#[test]
fn legacy_pointer_reports_encode_exact_codes_and_zero_cell() {
    // Legacy pointer reports retain the X10 byte layout: Cb+32 followed by
    // one-based coordinates+32, so pane-local cell zero is byte 33 on both axes.
    let cases = [
        (PointerReportKind::LeftPress, 32),
        (PointerReportKind::LeftRelease, 35),
        (PointerReportKind::HeldLeftMotion, 64),
        (PointerReportKind::NoButtonMotion, 67),
    ];
    for (kind, cb) in cases {
        assert_eq!(
            pointer_report_bytes(false, kind, ModifiersState::empty(), 0, 0),
            vec![0x1b, b'[', b'M', cb, 33, 33]
        );
    }
    assert_eq!(
        pointer_report_bytes(false, PointerReportKind::LeftRelease, ModifiersState::ALT, 0, 0),
        vec![0x1b, b'[', b'M', 43, 33, 33]
    );
}

#[test]
fn legacy_pointer_reports_clamp_large_coordinates() {
    // The current legacy profile caps pane-local zero-based coordinates at 222,
    // yielding protocol coordinate 223 and a final encoded byte of 255.
    assert_eq!(
        pointer_report_bytes(
            false,
            PointerReportKind::LeftPress,
            ModifiersState::empty(),
            u16::MAX,
            u16::MAX,
        ),
        vec![0x1b, b'[', b'M', 32, 255, 255]
    );
}

#[test]
fn window_press_uses_live_shift_and_latches_the_real_owner() {
    // This is the transition called by both main and child handlers. Reading
    // `window.modifiers` here prevents either call site from substituting stale state.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window_id = app.__test_seed_child_window(&["child"]);
    let pane = app.__test_child_pane_ids(window_id).expect("seeded child window")[0];
    let cell = pointer_cell(pane, 2, 3);
    let window = app.windows.get_mut(&window_id).unwrap();
    window.selection = Some(Selection::new(2, 3));
    window.modifiers = ModifiersState::SHIFT;

    assert_eq!(window.begin_pointer_press(cell, MouseTracking::ButtonMotion, true), None);
    assert!(matches!(
        window.pointer_gesture.map(|gesture| gesture.owner),
        Some(PointerGestureOwner::Local)
    ));
    assert!(window.selection.is_some());
    assert_eq!(take_pointer_release(&mut window.pointer_gesture, ModifiersState::empty()), None);

    window.selection = Some(Selection::new(2, 3));
    window.modifiers = ModifiersState::empty();
    assert_eq!(
        window.begin_pointer_press(cell, MouseTracking::ButtonMotion, true),
        Some(b"\x1b[<0;4;3M".to_vec())
    );
    assert!(matches!(
        window.pointer_gesture.map(|gesture| gesture.owner),
        Some(PointerGestureOwner::Terminal { tracking: MouseTracking::ButtonMotion, sgr: true })
    ));
    assert!(window.selection.is_none());
}

#[test]
fn tracked_press_chooses_terminal_unless_shift_or_tracking_off() {
    // Press-time Shift overrides every active tracking mode, while an
    // unmodified active mode latches terminal ownership and Off stays local.
    let cell = pointer_cell(7, 3, 4);
    for tracking in [MouseTracking::Button, MouseTracking::ButtonMotion, MouseTracking::AnyMotion] {
        let terminal = begin_pointer_gesture(cell, tracking, true, ModifiersState::empty(), false)
            .expect("grid press must create a gesture");
        assert!(matches!(terminal.owner, PointerGestureOwner::Terminal { .. }));
    }

    let shifted =
        begin_pointer_gesture(cell, MouseTracking::AnyMotion, true, ModifiersState::SHIFT, false)
            .expect("shifted grid press must create a local gesture");
    assert_eq!(shifted.owner, PointerGestureOwner::Local);

    let off = begin_pointer_gesture(cell, MouseTracking::Off, true, ModifiersState::empty(), false)
        .expect("untracked grid press must create a local gesture");
    assert_eq!(off.owner, PointerGestureOwner::Local);
}

#[test]
fn consumed_ui_press_creates_no_terminal_gesture_or_report() {
    // A press consumed by SonicTerm chrome never latches a grid owner and emits
    // no bytes even when the pane beneath it requested mouse tracking.
    assert_eq!(
        begin_pointer_gesture(
            pointer_cell(7, 0, 0),
            MouseTracking::AnyMotion,
            true,
            ModifiersState::empty(),
            true,
        ),
        None
    );
}

#[test]
fn terminal_owner_latches_press_pane_mode_profile_and_last_cell() {
    // Current Shift, parser mode, and profile changes cannot steal a terminal
    // gesture; motion remains pinned to the press pane and its last valid cell.
    let mut gesture = begin_pointer_gesture(
        pointer_cell(7, 3, 4),
        MouseTracking::ButtonMotion,
        true,
        ModifiersState::empty(),
        false,
    )
    .expect("tracked press must create a terminal gesture");

    let route = route_pressed_pointer_motion(
        &mut gesture,
        Some(pointer_cell(8, 9, 9)),
        ModifiersState::SHIFT,
    );
    assert_eq!(
        route,
        PointerMotionRoute::Report {
            pane_id: 7,
            sgr: true,
            row: 3,
            col: 4,
            modifiers: ModifiersState::SHIFT,
        }
    );
    assert!(matches!(gesture.owner, PointerGestureOwner::Terminal { .. }));
}

#[test]
fn local_owner_survives_shift_release() {
    // Once Shift gives the press to local selection, later modifier state does
    // not promote the held gesture into terminal reporting.
    let mut gesture = begin_pointer_gesture(
        pointer_cell(7, 1, 2),
        MouseTracking::AnyMotion,
        true,
        ModifiersState::SHIFT,
        false,
    )
    .expect("shifted press must create a local gesture");
    assert_eq!(
        route_pressed_pointer_motion(
            &mut gesture,
            Some(pointer_cell(7, 2, 3)),
            ModifiersState::empty(),
        ),
        PointerMotionRoute::Local
    );
    assert_eq!(gesture.owner, PointerGestureOwner::Local);
}

#[test]
fn terminal_motion_obeys_latched_tracking_mode() {
    // Button suppresses held motion, whereas ButtonMotion and AnyMotion both
    // emit held-left motion through the same terminal route.
    for (tracking, expected_report) in [
        (MouseTracking::Button, false),
        (MouseTracking::ButtonMotion, true),
        (MouseTracking::AnyMotion, true),
    ] {
        let mut gesture = begin_pointer_gesture(
            pointer_cell(7, 1, 2),
            tracking,
            false,
            ModifiersState::empty(),
            false,
        )
        .expect("active tracking must create a terminal gesture");
        assert_eq!(
            matches!(
                route_pressed_pointer_motion(
                    &mut gesture,
                    Some(pointer_cell(7, 4, 5)),
                    ModifiersState::ALT,
                ),
                PointerMotionRoute::Report { .. }
            ),
            expected_report
        );
    }
}

#[test]
fn native_scrollbar_owns_only_its_right_gutter() {
    // Always mode owns the drawn eight-pixel gutter even without an Auto hover
    // latch, while center cells and Never mode stay available to terminal motion.
    let pane = sonicterm_ui::pane::Rect::new(10.0, 20.0, 200.0, 120.0);
    assert!(native_scrollbar_owns_pointer(
        ScrollbarMode::Always,
        pane,
        205.0,
        60.0,
        8.0,
        false,
        false,
    ));
    assert!(!native_scrollbar_owns_pointer(
        ScrollbarMode::Always,
        pane,
        100.0,
        60.0,
        8.0,
        false,
        false,
    ));
    assert!(!native_scrollbar_owns_pointer(
        ScrollbarMode::Never,
        pane,
        205.0,
        60.0,
        8.0,
        true,
        true,
    ));
    assert!(native_scrollbar_owns_pointer(
        ScrollbarMode::Auto,
        pane,
        205.0,
        60.0,
        8.0,
        true,
        false,
    ));
    assert!(native_scrollbar_owns_pointer(
        ScrollbarMode::Auto,
        pane,
        205.0,
        60.0,
        8.0,
        false,
        true,
    ));
    assert!(!native_scrollbar_owns_pointer(
        ScrollbarMode::Auto,
        pane,
        205.0,
        60.0,
        8.0,
        false,
        false,
    ));
}

#[test]
fn keyboard_and_modifier_transitions_preserve_path_probe_authorization() {
    // Holding Ctrl can emit repeated keyboard/modifier events; neither changes
    // target identity, so main and child paths must retain the accepted probe.
    let main_source = MAIN_SOURCES.replace("\r\n", "\n");
    let invalidation_start = main_source
        .find("if matches!(\n            &event,")
        .expect("path-hover invalidation match");
    let invalidation_end = main_source[invalidation_start..]
        .find("if self.command_palette_handle_pointer_event")
        .map(|offset| invalidation_start + offset)
        .expect("end of path-hover invalidation match");
    let invalidation_match = &main_source[invalidation_start..invalidation_end];
    assert!(!invalidation_match.contains("WindowEvent::KeyboardInput"));
    assert!(!main_source.contains("ws.path_probe.invalidate();"));
    assert!(!CHILD_SOURCES.contains("c.path_probe.invalidate();"));
}

#[test]
fn main_and_child_no_button_paths_share_scrollbar_ownership() {
    // Both runtime paths must call the same gutter predicate so Always and Auto
    // scrollbar ownership cannot drift between main and torn-out windows.
    assert_eq!(
        MAIN_SOURCES.matches("native_scrollbar_owns_pointer(").count(),
        2,
        "main source must define and call the shared ownership helper",
    );
    assert_eq!(
        CHILD_SOURCES.matches("native_scrollbar_owns_pointer(").count(),
        1,
        "child source must call the shared ownership helper once",
    );
}

#[test]
fn any_motion_without_pressed_gesture_uses_current_pane_profile_and_modifiers() {
    // Hover motion is live rather than latched: only current AnyMotion emits,
    // carrying the current pane, profile, coordinates, and modifiers.
    assert_eq!(
        no_button_motion_report(
            pointer_cell(9, 2, 5),
            MouseTracking::AnyMotion,
            false,
            ModifiersState::SHIFT | ModifiersState::CONTROL,
            false,
        ),
        Some(PointerMotionRoute::Report {
            pane_id: 9,
            sgr: false,
            row: 2,
            col: 5,
            modifiers: ModifiersState::SHIFT | ModifiersState::CONTROL,
        })
    );
    assert_eq!(
        no_button_motion_report(
            pointer_cell(9, 2, 5),
            MouseTracking::ButtonMotion,
            true,
            ModifiersState::empty(),
            false,
        ),
        None
    );
    assert_eq!(
        no_button_motion_report(
            pointer_cell(9, 2, 5),
            MouseTracking::AnyMotion,
            true,
            ModifiersState::empty(),
            true,
        ),
        None
    );
}

#[test]
fn terminal_release_uses_press_pane_profile_and_last_same_pane_cell() {
    // Same-pane motion advances the retained cell; crossing panes or leaving the
    // grid does not, and release consumes the latched press pane/profile.
    let mut gesture = begin_pointer_gesture(
        pointer_cell(7, 1, 2),
        MouseTracking::AnyMotion,
        false,
        ModifiersState::empty(),
        false,
    )
    .expect("tracked press must create a terminal gesture");
    let _ = route_pressed_pointer_motion(
        &mut gesture,
        Some(pointer_cell(7, 4, 5)),
        ModifiersState::empty(),
    );
    let _ = route_pressed_pointer_motion(
        &mut gesture,
        Some(pointer_cell(8, 9, 10)),
        ModifiersState::empty(),
    );
    let _ = route_pressed_pointer_motion(&mut gesture, None, ModifiersState::empty());

    assert_eq!(
        take_pointer_release(&mut Some(gesture), ModifiersState::ALT),
        Some(PointerMotionRoute::Report {
            pane_id: 7,
            sgr: false,
            row: 4,
            col: 5,
            modifiers: ModifiersState::ALT,
        })
    );
}

#[test]
fn focus_loss_releases_terminal_owner_and_silently_clears_local_owner() {
    // Focus loss consumes both owners, but only a terminal-owned gesture emits
    // a release from its latched pane/profile/cell with current modifiers.
    let mut terminal = begin_pointer_gesture(
        pointer_cell(7, 1, 2),
        MouseTracking::ButtonMotion,
        false,
        ModifiersState::empty(),
        false,
    );
    if let Some(gesture) = terminal.as_mut() {
        let _ = route_pressed_pointer_motion(
            gesture,
            Some(pointer_cell(7, 4, 5)),
            ModifiersState::empty(),
        );
    }
    assert_eq!(
        take_focus_loss_pointer_release(&mut terminal, ModifiersState::ALT),
        Some(PointerMotionRoute::Report {
            pane_id: 7,
            sgr: false,
            row: 4,
            col: 5,
            modifiers: ModifiersState::ALT,
        })
    );
    assert_eq!(terminal, None);

    let mut local = begin_pointer_gesture(
        pointer_cell(9, 3, 6),
        MouseTracking::AnyMotion,
        true,
        ModifiersState::SHIFT,
        false,
    );
    assert_eq!(take_focus_loss_pointer_release(&mut local, ModifiersState::empty()), None);
    assert_eq!(local, None);
}

#[test]
fn main_and_child_focus_loss_share_release_helper() {
    // One native focus route releases recorded pointer ownership before its bounded source-pane write.
    let source = include_str!("window_keyboard.rs");
    let (_, focus) = source.split_once("pub(super) fn handle_window_focus_changed(").unwrap();
    let focus = focus.split("pub(super) fn handle_window_ime(").next().unwrap();
    assert!(
        focus.find("self.release_window_native_keys(win_id)").unwrap()
            < focus.find("take_focus_loss_pointer_release(").unwrap()
    );
    assert!(
        focus.find("take_focus_loss_pointer_release(").unwrap()
            < focus
                .find("self.write_to_pane(pane_id, bytes, PtyInputSource::PointerButton)")
                .unwrap()
    );
    assert!(!CHILD_SOURCES.contains("take_focus_loss_pointer_release("));
}

/// A repeated key keeps its terminal owner even if local UI opens after the press.
#[test]
fn terminal_repeat_owner_is_resolved_before_local_input_owners() {
    let key = PhysicalKey::Code(KeyCode::KeyA);
    let panes = std::collections::BTreeMap::from([
        (7, crate::app::HeldKey::Legacy),
        (11, crate::app::HeldKey::Legacy),
    ]);
    let mut pressed = std::collections::HashMap::from([(key, panes.clone())]);

    assert_eq!(terminal_repeat_targets(&pressed, key, true), Some(panes.clone()));
    assert_eq!(terminal_repeat_targets(&pressed, key, false), None);
    assert_eq!(pressed.remove(&key), Some(panes));

    let source = include_str!("window_keyboard.rs");
    let (_, keyboard) = source.split_once("pub(super) fn handle_window_keyboard(").unwrap();
    let keyboard = keyboard.split("pub(super) fn handle_window_modifiers_changed(").next().unwrap();
    let release = keyboard.find("take_release_routes(").expect("release ownership lookup");
    let repeat = keyboard.find("terminal_repeat_targets(").expect("repeat ownership lookup");
    let quit = keyboard.find("self.on_quit_chord_pressed(win_id, event.repeat)").unwrap();
    let owners = keyboard
        .find("match self.window_key_owner(win_id)")
        .expect("source-window owner selection");
    assert!(release < repeat && repeat < quit && quit < owners);
    assert!(!keyboard.contains("self.frontmost_window ="));
}

#[test]
fn child_no_button_motion_respects_each_child_ui_owner() {
    // Production child routing computes `ui_consumed` from these exact fields;
    // each owner must suppress AnyMotion while an otherwise identical cell reports.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["child"]);
    let pane = app.__test_child_pane_ids(window).expect("seeded child window")[0];
    let cell = pointer_cell(pane, 6, 7);

    let child = app.windows.get(&window).expect("seeded child state");
    assert!(matches!(
        child_no_button_motion_report(child, cell, MouseTracking::AnyMotion, true, false),
        Some(PointerMotionRoute::Report { pane_id, .. }) if pane_id == pane
    ));

    app.windows.get_mut(&window).unwrap().splitter_hover = Some(SplitAxis::Vertical);
    assert_eq!(
        child_no_button_motion_report(
            app.windows.get(&window).unwrap(),
            cell,
            MouseTracking::AnyMotion,
            true,
            false,
        ),
        None
    );
    app.windows.get_mut(&window).unwrap().splitter_hover = None;

    app.windows.get_mut(&window).unwrap().hovered_url = Some(HoveredUrl {
        cells: sonicterm_render_model::inputs::HoveredUrlCells::single(pane, 6, 7, 8, true)
            .unwrap(),
        url: "https://example.com".into(),
    });
    assert_eq!(
        child_no_button_motion_report(
            app.windows.get(&window).unwrap(),
            cell,
            MouseTracking::AnyMotion,
            true,
            false,
        ),
        None
    );
    app.windows.get_mut(&window).unwrap().hovered_url = None;

    app.windows.get_mut(&window).unwrap().hover_link = true;
    assert_eq!(
        child_no_button_motion_report(
            app.windows.get(&window).unwrap(),
            cell,
            MouseTracking::AnyMotion,
            true,
            false,
        ),
        None
    );
    app.windows.get_mut(&window).unwrap().hover_link = false;

    assert_eq!(
        child_no_button_motion_report(
            app.windows.get(&window).unwrap(),
            cell,
            MouseTracking::AnyMotion,
            true,
            true,
        ),
        None
    );
}

#[test]
fn pointer_cleanup_clears_latched_gesture() {
    // Focus-loss and global drag cleanup share this primitive, preventing a
    // gesture whose release was lost from resuming on a later cursor event.
    let mut gesture = begin_pointer_gesture(
        pointer_cell(7, 0, 0),
        MouseTracking::Button,
        true,
        ModifiersState::empty(),
        false,
    );
    assert!(cancel_pointer_gesture(&mut gesture));
    assert_eq!(gesture, None);
    assert!(!cancel_pointer_gesture(&mut gesture));
}

#[test]
fn explicit_quit_app_binding_is_quit_chord_any_key() {
    // An explicit `quit_app` binding fires the guard regardless of chord or
    // platform — this is the cross-platform / user-rebind path.
    assert!(is_quit_chord("super+q", Some(&Action::QuitApp)));
    assert!(is_quit_chord("ctrl+shift+q", Some(&Action::QuitApp)));
}

#[test]
fn super_q_bound_elsewhere_is_not_quit_chord() {
    // If the user deliberately rebound super+q to another action, respect it.
    assert!(!is_quit_chord("super+q", Some(&Action::CloseActivePaneOrTab)));
    assert!(!is_quit_chord("super+q", Some(&Action::NewTab)));
}

#[test]
fn other_chords_unbound_are_not_quit_chord() {
    // Unbound non-quit chords must never trigger quit (they fall through to
    // the PTY as normal input).
    assert!(!is_quit_chord("super+w", None));
    assert!(!is_quit_chord("q", None));
    assert!(!is_quit_chord("super+shift+q", None));
}

#[cfg(target_os = "macos")]
#[test]
fn unbound_super_q_is_quit_chord_on_macos() {
    // The reported bug: a user keymap with no `super+q` binding must still
    // quit on macOS (Cmd+Q is a system chord) instead of typing a literal q.
    assert!(is_quit_chord("super+q", None));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unbound_super_q_is_not_quit_chord_off_macos() {
    // Off macOS, Cmd+Q is not a system quit chord; only an explicit binding
    // quits.
    assert!(!is_quit_chord("super+q", None));
}
