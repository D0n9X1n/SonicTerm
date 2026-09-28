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
    include_str!("splitter_input.rs"),
    include_str!("child_window_pointer.rs"),
    include_str!("child_window_redraw.rs")
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
