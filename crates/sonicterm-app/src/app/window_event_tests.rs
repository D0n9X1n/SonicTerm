pub(super) use super::{is_quit_chord, PointerCell};
pub(super) use crate::app::{child_window::child_no_button_motion_report, hovered_url::HoveredUrl};
pub(super) use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
pub(super) use sonicterm_ui::pane::SplitAxis;
pub(super) use winit::keyboard::KeyCode;

/// The main dispatcher followed by the files that hold its handlers, so whole-file
/// absence and count checks cover every main-window event path.
pub(super) const MAIN_SOURCES: &str = concat!(
    concat!(
        include_str!("window_event.rs"),
        "\n",
        include_str!("window_event_pointer.rs"),
        "\n",
        include_str!("window_event_redraw.rs")
    ),
    concat!(include_str!("window_keyboard.rs"), "\n", include_str!("window_keyboard_ime.rs")),
    include_str!("splitter_input.rs"),
    include_str!("window_pointer.rs")
);

/// Every source file that holds child-window code, so whole-file absence and count
/// checks cover all of it.
pub(super) const CHILD_SOURCES: &str = concat!(
    include_str!("child_window.rs"),
    include_str!("child_tabs.rs"),
    include_str!("splitter_input.rs"),
    include_str!("child_window_pointer.rs"),
    include_str!("child_window_redraw.rs")
);

pub(super) fn pointer_cell(pane_id: u64, row: u16, col: u16) -> PointerCell {
    PointerCell { pane_id, row, col }
}

#[cfg(windows)]
pub(super) fn run_readonly_native_matrix(event_loop: &winit::event_loop::ActiveEventLoop) {
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
        event_loop
            .create_window(
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
        event_loop,
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
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
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
        app.do_window_event(event_loop, window, WindowEvent::RedrawRequested);
        let cell =
            app.windows[&window].renderer.as_ref().unwrap().pixel_to_pane_cell(40.0, 80.0).unwrap();
        assert_eq!(cell.0, pane_id, "rendered hit-test fixture must name the live pane");
        app.wait_for_input_queues();
        let submitted = PtySubmissions::start();
        for read_only in [false, true] {
            for tracked in [false, true] {
                for is_alt in [false, true] {
                    let window_state = app.windows.get_mut(&window).unwrap();
                    window_state.copy_mode = Some(if read_only {
                        CopyModeState::read_only_at((0, 0))
                    } else {
                        CopyModeState::new_at((0, 0))
                    });
                    window_state.mouse_down = false;
                    window_state.pointer_gesture = None;
                    let pane = window_state.panes.get_mut(&pane_id).unwrap();
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
                        // No VT worker runs here; handlers route by the byte it would publish.
                        pane.__test_publish_input_modes(&parser);
                    }
                    pane.viewport_top_abs = Some(10);
                    // Let the previous gesture leave the queue before testing this gesture's admission.
                    app.wait_for_input_queues();
                    phase(pane_id, "wheel");
                    app.do_window_event(
                        event_loop,
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
                        event_loop,
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
                        event_loop,
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
                        event_loop,
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
            app.do_window_event(event_loop, window, WindowEvent::DroppedFile("safe path".into()));
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
