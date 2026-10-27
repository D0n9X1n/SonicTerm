#[cfg(windows)]
use crate::app::{hovered_url::HoveredUrl, App};
#[cfg(windows)]
use sonicterm_cfg::{
    config::{Config, ScrollbarMode},
    keymap::Keymap,
    theme::Theme,
};
#[cfg(windows)]
use sonicterm_ui::selection::Selection;
#[cfg(windows)]
use winit::keyboard::ModifiersState;

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
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
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
                {
                    let mut parser = pane.parser.lock();
                    parser.advance(b"\x1b[?1003h\x1b[?1006h");
                    // Pointer handlers read the published byte, as the VT worker would leave it.
                    pane.__test_publish_input_modes(&parser);
                }
                self.progress.set("wheel", case, "tracked_wheel");
                ApplicationHandler::window_event(
                    &mut app,
                    event_loop,
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
                {
                    let pane = &app.windows[&window].panes[&pane_id];
                    let mut parser = pane.parser.lock();
                    parser.advance(b"\x1b[?1003l");
                    // The reset reaches the wheel handler only through the published byte.
                    pane.__test_publish_input_modes(&parser);
                }
                self.progress.set("wheel", case, "fallback_wheel");
                ApplicationHandler::window_event(
                    &mut app,
                    event_loop,
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
            self.selection_probe =
                Some(ModifierSelectionProbe::new(event_loop, self.progress.clone()));
        }
        // EventLoop<()> delivers proxy wakes here, not through the production App user-event type.
        fn user_event(&mut self, _: &ActiveEventLoop, (): ()) {
            self.proxy_user_event = true;
        }
        fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            if let Some(probe) = self.selection_probe.as_mut() {
                probe.event(event_loop, id, event, &mut self.failures);
            } else if let Some(probe) = self.hover_probe.as_mut() {
                probe.event(event_loop, id, event);
            }
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            self.progress.about_to_wait();
            self.proxy_about_to_wait |= self.proxy_user_event;
            if self.selection_probe.as_mut().is_some_and(|probe| probe.poll(&mut self.failures)) {
                self.progress.set("selection", "main/child", "teardown");
                self.selection_probe = None;
                self.hover_probe =
                    Some(HoverRetryProbe::new(event_loop, self.hover_case, self.progress.clone()));
            } else if self.hover_probe.as_mut().is_some_and(|probe| probe.poll(event_loop)) {
                let probe = self.hover_probe.as_ref().unwrap();
                self.progress.set("hover", probe.case, "teardown");
                self.hover_probe = None;
                self.hover_case += 1;
                if self.hover_case == 4 {
                    self.progress.set("fixture", "all", "exit_event_loop");
                    event_loop.exit();
                    return;
                }
                self.hover_probe =
                    Some(HoverRetryProbe::new(event_loop, self.hover_case, self.progress.clone()));
            }
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
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

#[test]
fn hover_moves_request_a_native_redraw_only_when_the_hovered_tab_changes() {
    // The renderer cannot be built off a native window, so the source pins the
    // callers: every hover update passes its own window's tab bar, and the main
    // and child move handlers ask for a native redraw only on its result, so a
    // sweep inside one tab issues none and a tab crossing issues one.
    let sources = [
        ("window_pointer.rs", include_str!("window_pointer.rs").replace("\r\n", "\n")),
        ("child_window_pointer.rs", include_str!("child_window_pointer.rs").replace("\r\n", "\n")),
        ("child_window.rs", include_str!("child_window.rs").replace("\r\n", "\n")),
    ];
    let mut call_count = 0;
    for (name, source) in &sources {
        assert!(!source.contains("muted ×"), "{name} keeps a stale close-button comment");
        for (offset, _) in source.match_indices("set_hover_cursor(") {
            let call = &source[offset..offset + source[offset..].find(')').unwrap() + 40];
            assert!(
                call.contains("&window.tabs)") || call.contains("&child.tabs)"),
                "{name}: hover update without its window's tab bar: {call}"
            );
            call_count += 1;
        }
    }
    assert_eq!(call_count, 4, "main move, main leave, child move and child leave");
    let main = &sources[0].1;
    let moved = main.find("fn handle_main_cursor_moved(").expect("main move handler");
    let moved = &main[moved..];
    let update = moved.find("hover_redraw = renderer.set_hover_cursor(").expect("hover update");
    let guard = moved.find("if hover_redraw {").expect("redraw guard");
    let request = moved.find("request_native_redraw(main_window)").expect("redraw request");
    assert!(update < guard && guard < request, "the main redraw follows the hover result");
    let child = &sources[1].1;
    assert!(child.contains(
        "if renderer.set_hover_cursor(Some((cursor_x, cursor_y)), &child.tabs) {\n            if let Some(window) = child.window.as_ref() {\n                crate::app::frame_counters::request_native_redraw(window);"
    ));
}

/// A pointer handler reads a pane's mouse modes while another thread holds that pane's
/// parser (as the VT worker does during a large parse) and returns without waiting.
#[test]
fn pointer_modes_read_while_the_parser_is_held_by_another_thread() {
    let mut parser = sonicterm_vt::vt::Parser::new(sonicterm_grid::grid::Grid::new(80, 24));
    parser.advance(b"\x1b[?1003h\x1b[?1006h");
    let pane =
        crate::app::PaneState::new(std::sync::Arc::new(parking_lot::Mutex::new(parser)), None);
    let parser = pane.parser.clone();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = parser.lock();
        held_tx.send(()).unwrap();
        // Hold the parser until the reader has finished or the test gives up.
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
    });
    held_rx.recv().unwrap();
    let (read_tx, read_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| read_tx.send(pane.pointer_modes()).unwrap());
        // A blocking read would wait for the holder; the published byte answers at once.
        let modes = read_rx.recv_timeout(std::time::Duration::from_secs(2));
        release_tx.send(()).unwrap();
        let modes = modes.expect("pointer mode read waited for the parser lock");
        assert_eq!(modes.tracking(), sonicterm_vt::vt::MouseTracking::AnyMotion);
        assert!(modes.sgr());
    });
    holder.join().unwrap();
}

/// Every main and child pointer route reads the published byte: the blocking profile
/// helper is gone, and the only parser lock left in the child pointer file is the
/// scrollbar-drag viewport baseline, which needs grid state the byte does not carry.
#[test]
fn pointer_routes_read_published_modes_without_a_parser_lock() {
    let main = include_str!("window_pointer.rs").replace("\r\n", "\n");
    let child = include_str!("child_window_pointer.rs").replace("\r\n", "\n");
    let event = concat!(
        include_str!("window_event.rs"),
        "\n",
        include_str!("window_event_pointer.rs"),
        "\n",
        include_str!("window_event_redraw.rs")
    )
    .replace("\r\n", "\n");
    assert!(
        !event.contains("fn parser_mouse_profile"),
        "the blocking profile helper must stay deleted"
    );
    for (name, source, lock_count) in
        [("window_pointer.rs", &main, 0), ("child_window_pointer.rs", &child, 1)]
    {
        assert!(
            !source.contains("parser_mouse_profile"),
            "{name} still reads modes under the parser"
        );
        assert_eq!(
            source.matches(".pointer_modes()").count(),
            3,
            "{name}: motion, wheel and press"
        );
        assert_eq!(source.matches("lock_parser(").count(), lock_count, "{name} parser locks");
    }
    let retained = child.find("lock_parser(").unwrap();
    assert!(child[retained..retained + 200].contains("ViewportBaseline::of"));
}

/// Hold `parser` on another thread until the returned sender fires or three seconds pass.
fn hold_parser(
    parser: std::sync::Arc<parking_lot::Mutex<sonicterm_vt::vt::Parser>>,
) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Sender<()>) {
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let _guard = parser.lock();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(3));
    });
    held_rx.recv().unwrap();
    (holder, release_tx)
}

/// Published modes paired with what they must produce: the parser itself keeps every mode
/// off on the primary screen, so only the published byte can route these.
fn published_cases() -> [(sonicterm_vt::vt::PointerModes, Vec<u8>); 2] {
    use sonicterm_vt::vt::{MouseTracking, PointerModes};
    [
        // Any-motion SGR tracking: no-button motion at column 3, row 2 reports button 35.
        (
            PointerModes::new(MouseTracking::AnyMotion, true, false, false),
            b"\x1b[<35;3;2M".to_vec(),
        ),
        // Untracked alternate screen with DECCKM: motion reports nothing.
        (PointerModes::new(MouseTracking::Off, false, true, true), Vec::new()),
    ]
}

/// Unheld motion and the wheel go through the real main-window handlers while another thread
/// holds the pane's parser (as the VT worker does during a large parse): they return within a
/// bound, and the bytes follow the published modes, not the parser's own state.
// Ordering: pointer_input stores Relaxed; this thread is the handler's only reader.
#[test]
fn main_pointer_handlers_route_by_published_modes_while_the_parser_is_held() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use winit::{dpi::PhysicalPosition, event::MouseScrollDelta};
    let wheel_bytes = [b"\x1b[<64;3;2M".repeat(3), b"\x1bOA".repeat(3)];
    for ((modes, motion), wheel) in published_cases().into_iter().zip(wheel_bytes) {
        let mut app = crate::app::App::new(Theme::default(), Config::default(), Keymap::default());
        let pane_id = app.__test_seed_tab("main");
        app.test_viewport_override =
            Some((sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0));
        app.__test_enable_pty_write_log();
        let pane = &app.main().unwrap().panes[&pane_id];
        pane.pointer_input.store(modes.bits(), std::sync::atomic::Ordering::Relaxed);
        let (holder, release) = hold_parser(pane.parser.clone());

        let started = std::time::Instant::now();
        app.handle_main_cursor_moved(PhysicalPosition::new(25.0, 15.0));
        app.handle_main_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));
        let elapsed = started.elapsed();
        let _ = release.send(());
        holder.join().unwrap();

        assert!(elapsed < std::time::Duration::from_secs(1), "handlers waited {elapsed:?}");
        let expected: Vec<(u64, Vec<u8>)> = [motion, wheel]
            .into_iter()
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| (pane_id, bytes))
            .collect();
        assert_eq!(app.__test_pty_write_log(), expected, "{modes:?}");
    }
}

/// Unheld motion and the wheel go through the real child-window handlers while the parser is
/// held: they return within a bound, motion reports follow the published modes, and the wheel
/// never falls back to local scrollback (which would lock the parser) when the published
/// modes route it to the terminal.
// Ordering: pointer_input stores Relaxed; this thread is the handler's only reader.
#[test]
fn child_pointer_handlers_route_by_published_modes_while_the_parser_is_held() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use winit::{dpi::PhysicalPosition, event::MouseScrollDelta};
    for (modes, motion) in published_cases() {
        let mut app = crate::app::App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        let child = app.__test_seed_child_window(&["child"]);
        assert!(app.__test_set_child_pane_viewport(
            child,
            sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
            10.0,
            10.0,
        ));
        let pane_id = app.__test_child_active_pane(child).unwrap();
        app.__test_enable_pty_write_log();
        let pane = &app.windows[&child].panes[&pane_id];
        pane.pointer_input.store(modes.bits(), std::sync::atomic::Ordering::Relaxed);
        let (holder, release) = hold_parser(pane.parser.clone());
        let config = app.config.clone();
        let position = PhysicalPosition::new(25.0, 15.0);

        let started = std::time::Instant::now();
        if !app.handle_child_cursor_moved_chrome(child, &position) {
            app.handle_child_cursor_moved(child, position, &config);
        }
        crate::app::App::handle_child_mouse_wheel(
            app.windows.get_mut(&child).unwrap(),
            MouseScrollDelta::LineDelta(0.0, 1.0),
            &None,
            config.appearance.scrollbar,
        );
        let elapsed = started.elapsed();
        let _ = release.send(());
        holder.join().unwrap();

        assert!(elapsed < std::time::Duration::from_secs(1), "handlers waited {elapsed:?}");
        let expected: Vec<(u64, Vec<u8>)> = [motion]
            .into_iter()
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| (pane_id, bytes))
            .collect();
        assert_eq!(app.__test_pty_write_log(), expected, "{modes:?}");
        assert_eq!(app.windows[&child].panes[&pane_id].viewport_top_abs, None, "no local scroll");
    }
}

/// Advance `pane`'s parser with each batch and publish its modes after every batch, as the VT
/// worker does; a fixture without a VT worker must do the same before driving a handler.
fn advance_published(pane: &crate::app::PaneState, batches: &[&[u8]]) {
    let mut parser = pane.parser.lock();
    for batch in batches {
        parser.advance(batch);
        pane.__test_publish_input_modes(&parser);
    }
}

/// Mode batches the Windows native fixtures feed through the parser, with the wheel bytes and
/// viewport each must leave: untracked alternate screen sends arrows, any-motion SGR tracking
/// sends a report at row 2 column 3, and a published tracking reset falls back to local scroll.
fn parser_mode_cases() -> [(&'static str, Vec<&'static [u8]>, Vec<u8>, Option<u64>); 3] {
    let reset: &[u8] = b"\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006h";
    [
        ("alt", vec![reset, b"\x1b[?1049h"], b"\x1b[A".repeat(3), Some(10)),
        ("tracked", vec![reset, b"\x1b[?1003h"], b"\x1b[<64;3;2M".repeat(3), Some(10)),
        ("reset", vec![reset, b"\x1b[?1003h", b"\x1b[?1003l"], Vec::new(), Some(7)),
    ]
}

/// Modes set through the parser reach the real main wheel handler once published: alternate
/// screen, tracking and a tracking reset each route the wheel as the Windows native matrix expects.
#[test]
fn main_wheel_follows_modes_published_from_parser_batches() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use winit::event::MouseScrollDelta;
    for (name, batches, wheel, viewport) in parser_mode_cases() {
        let mut app = crate::app::App::new(Theme::default(), Config::default(), Keymap::default());
        let pane_id = app.__test_seed_tab("main");
        app.test_viewport_override =
            Some((sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0));
        app.__test_enable_pty_write_log();
        let window = app.main_mut().unwrap();
        // Column 3, row 2 of the 10-pixel cells, matching the report in the tracked case.
        window.cursor_pos = (25.0, 15.0);
        let pane = window.panes.get_mut(&pane_id).unwrap();
        pane.parser.lock().advance("history\r\n".repeat(60).as_bytes());
        advance_published(pane, &batches);
        pane.viewport_top_abs = Some(10);

        app.handle_main_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));

        let expected: Vec<(u64, Vec<u8>)> = [wheel]
            .into_iter()
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| (pane_id, bytes))
            .collect();
        assert_eq!(app.__test_pty_write_log(), expected, "{name}");
        assert_eq!(app.main().unwrap().panes[&pane_id].viewport_top_abs, viewport, "{name}");
    }
}

/// The child wheel handler reads the same published byte: only a published tracking reset lets
/// the wheel scroll the child pane locally; alternate screen and tracking keep the viewport.
#[test]
fn child_wheel_follows_modes_published_from_parser_batches() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use winit::event::MouseScrollDelta;
    for (name, batches, _, viewport) in parser_mode_cases() {
        let mut app = crate::app::App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        let child = app.__test_seed_child_window(&["child"]);
        assert!(app.__test_set_child_pane_viewport(
            child,
            sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
            10.0,
            10.0,
        ));
        let pane_id = app.__test_child_active_pane(child).unwrap();
        let config = app.config.clone();
        let window = app.windows.get_mut(&child).unwrap();
        window.cursor_pos = (25.0, 15.0);
        let pane = window.panes.get_mut(&pane_id).unwrap();
        pane.parser.lock().advance("history\r\n".repeat(60).as_bytes());
        advance_published(pane, &batches);
        pane.viewport_top_abs = Some(10);

        crate::app::App::handle_child_mouse_wheel(
            window,
            MouseScrollDelta::LineDelta(0.0, 1.0),
            &None,
            config.appearance.scrollbar,
        );

        assert_eq!(app.windows[&child].panes[&pane_id].viewport_top_abs, viewport, "{name}");
    }
}

/// Every `.rs` file under `dir`, recursively.
fn rust_sources(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            // When: a subdirectory holds more modules, so its sources are scanned too.
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// Offsets in `bare` where `needle` starts and is not the tail of a longer identifier.
fn token_offsets<'a>(bare: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    bare.match_indices(needle).map(|(offset, _)| offset).filter(move |offset| {
        *offset == 0
            || !crate::app::source_scan_support::is_ident_byte(bare.as_bytes()[*offset - 1])
            || needle.starts_with('.')
    })
}

/// The method receiver that ends at `dot` in `bare`, with whitespace removed: the expression
/// back to the enclosing statement, argument or block boundary.
fn receiver_before(bare: &str, dot: usize) -> String {
    let bytes = bare.as_bytes();
    let mut cursor = dot;
    let mut depth = 0usize;
    while cursor > 0 {
        match bytes[cursor - 1] {
            b')' | b']' => depth += 1,
            b'(' | b'[' if depth == 0 => break,
            b'(' | b'[' => depth -= 1,
            b';' | b'{' | b'}' | b'=' | b',' if depth == 0 => break,
            _ => {}
        }
        cursor -= 1;
    }
    bare[cursor..dot].split_whitespace().collect::<String>().trim_start_matches('&').to_owned()
}

/// The text between the parenthesis at `open` in `bare` and its match, as a byte range.
fn call_arguments(bare: &str, open: usize) -> std::ops::Range<usize> {
    let mut depth = 0usize;
    for (offset, byte) in bare.bytes().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return open + 1..offset;
                }
            }
            _ => {}
        }
    }
    open + 1..bare.len()
}

/// One code event the publication scan orders by offset.
enum ScanEvent {
    /// A function starts: bindings and unpublished changes from the previous one end.
    FunctionStart,
    /// `let <name> = <pane>.parser.lock();` binds a parser guard to a pane.
    GuardBinding { name: String, pane: String },
    /// A raw `advance` whose argument carries pointer-mode bytes, on this receiver.
    ModeChange { receiver: String },
    /// `<pane>.__test_publish_input_modes(&<guard>)`.
    Publish { pane: String, guard: String },
    /// A native pointer event or a direct pointer-handler call.
    Handler,
}

/// Line pairs `(mode change, handler)` where a function in `text` changes pointer modes with a
/// raw `advance` and then drives a pointer handler before publishing that pane's modes.
///
/// Comments and the contents of string and char literals never count, and events are ordered by
/// byte offset, so a publish later on the handler's line is too late. A publish counts only when
/// its guard argument is a `let` binding of `<pane>.parser.lock()` and its receiver is that same
/// `<pane>` text as the advanced parser's. The scan does not prove that the handler's cursor
/// targets that pane, and a nested `fn` ends the enclosing function's pending changes.
fn unpublished_mode_changes(text: &str) -> Vec<(usize, usize)> {
    let mode_needles: Vec<String> = ["1049", "1047", "47", "1000", "1002", "1003", "1006", "1"]
        .iter()
        .flat_map(|mode| [format!("[?{mode}h"), format!("[?{mode}l")])
        .chain([format!("{}x1bc", '\\')])
        .collect();
    let (code, bare) = crate::app::source_scan_support::code_views(text);
    let mut events: Vec<(usize, ScanEvent)> = Vec::new();
    events.extend(token_offsets(&bare, "fn ").map(|offset| (offset, ScanEvent::FunctionStart)));
    for offset in token_offsets(&bare, "let ") {
        let statement_end = bare[offset..].find(';').map_or(bare.len(), |len| offset + len);
        let Some((left, right)) = bare[offset + 4..statement_end].split_once('=') else {
            // When: a `let` without `=` binds nothing the scan can resolve.
            continue;
        };
        let name = left.trim().trim_start_matches("mut ").trim();
        let right: String = right.split_whitespace().collect();
        if let Some(pane) = right.strip_suffix(".parser.lock()") {
            if !name.is_empty() && name.bytes().all(crate::app::source_scan_support::is_ident_byte)
            {
                let (name, pane) = (name.to_owned(), pane.trim_start_matches('&').to_owned());
                events.push((offset, ScanEvent::GuardBinding { name, pane }));
            }
        }
    }
    for offset in token_offsets(&bare, ".advance(") {
        let arguments = call_arguments(&bare, offset + ".advance".len());
        if mode_needles.iter().any(|needle| code[arguments.clone()].contains(needle.as_str())) {
            let receiver = receiver_before(&bare, offset);
            events.push((offset, ScanEvent::ModeChange { receiver }));
        }
    }
    let publish_call = ".__test_publish_input_modes(";
    for offset in token_offsets(&bare, publish_call) {
        let arguments = call_arguments(&bare, offset + publish_call.len() - 1);
        let guard = bare[arguments].split_whitespace().collect::<String>();
        let guard = guard.trim_start_matches('&').to_owned();
        events.push((offset, ScanEvent::Publish { pane: receiver_before(&bare, offset), guard }));
    }
    for handler in POINTER_HANDLERS {
        events.extend(token_offsets(&bare, handler).map(|offset| (offset, ScanEvent::Handler)));
    }
    events.sort_by_key(|(offset, _)| *offset);

    let line_of = |offset: usize| bare[..offset].matches('\n').count() + 1;
    let mut found = Vec::new();
    // Guard name -> pane, and pane key -> line of its newest unpublished mode change.
    let mut guards: std::collections::HashMap<String, String> = Default::default();
    let mut unpublished: std::collections::BTreeMap<String, usize> = Default::default();
    for (offset, event) in events {
        match event {
            ScanEvent::FunctionStart => {
                guards.clear();
                unpublished.clear();
            }
            ScanEvent::GuardBinding { name, pane } => {
                guards.insert(name, pane);
            }
            ScanEvent::ModeChange { receiver } => {
                // An unresolvable receiver gets a key no publish can match, so it is never cleared.
                let pane = receiver
                    .strip_suffix(".parser.lock()")
                    .map(str::to_owned)
                    .or_else(|| guards.get(&receiver).cloned())
                    .unwrap_or_else(|| format!("unresolved {receiver}"));
                unpublished.insert(pane, line_of(offset));
            }
            ScanEvent::Publish { pane, guard } => {
                if guards.get(&guard) == Some(&pane) {
                    unpublished.remove(&pane);
                }
            }
            ScanEvent::Handler => {
                if let Some(changed) = unpublished.values().min() {
                    found.push((*changed, line_of(offset)));
                    unpublished.clear();
                }
            }
        }
    }
    found
}

/// Tokens that drive a real pointer handler: a native event or a direct handler call.
const POINTER_HANDLERS: [&str; 7] = [
    "MouseWheel {",
    "MouseInput {",
    "CursorMoved {",
    "handle_main_mouse_wheel(",
    "handle_child_mouse_wheel(",
    "handle_main_cursor_moved(",
    "handle_child_cursor_moved(",
];

/// Snippets the scan must reject: each changes modes and then drives a handler while the only
/// publication is commented out, after the handler, of another pane or with another pane's
/// guard, or inside a string.
const UNPUBLISHED_FIXTURES: [(&str, &str); 6] = [
    (
        "line-commented publish",
        r#"fn case() {
    let pane = &window.panes[&pane_id];
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1b[?1049h");
    // pane.__test_publish_input_modes(&parser);
    drop(parser);
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "publish after the handler on one line",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1b[?1049h");
    app.handle_main_mouse_wheel(delta); pane.__test_publish_input_modes(&parser);
}"#,
    ),
    (
        "publish of another pane",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1b[?1003l");
    let other_guard = other.parser.lock();
    other.__test_publish_input_modes(&other_guard);
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "publish with another pane's guard",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    let other_guard = other.parser.lock();
    parser.advance(b"\x1b[?1049h");
    pane.__test_publish_input_modes(&other_guard);
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "block-commented publish",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1b[?1003l");
    /* pane.__test_publish_input_modes(&parser); */
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "publish named in a string literal",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1bc");
    let note = "pane.__test_publish_input_modes(&parser);";
    app.handle_child_mouse_wheel(window, delta, &None, mode);
}"#,
    ),
];

/// Snippets the scan must accept: a publication of the advanced pane before the handler, a
/// rustfmt-wrapped `advance`, and mode bytes that appear only inside a string literal.
const PUBLISHED_FIXTURES: [(&str, &str); 3] = [
    (
        "publish then handler",
        r#"fn case() {
    let pane = &window.panes[&pane_id];
    let mut parser = pane.parser.lock();
    parser.advance(b"\x1b[?1049h");
    pane.__test_publish_input_modes(&parser);
    drop(parser);
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "multi-line advance then publish",
        r#"fn case() {
    let mut parser = pane.parser.lock();
    parser.advance(
        b"\x1b[?1049h\x1b[?1003h\x1b[?1006h",
    );
    pane.__test_publish_input_modes(&parser);
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
    (
        "mode bytes only in a string",
        r#"fn case() {
    let snippet = "parser.advance(b\"\\x1b[?1049h\");";
    app.handle_main_mouse_wheel(delta);
}"#,
    ),
];

/// The scan rejects every unpublished fixture and accepts every published one, using in-memory
/// sources so its comment, string, order and receiver rules are checked without the filesystem.
#[test]
fn mode_publication_scan_rejects_unpublished_and_accepts_published_fixtures() {
    let mut wrong = Vec::new();
    for (name, source) in UNPUBLISHED_FIXTURES {
        if unpublished_mode_changes(source).is_empty() {
            wrong.push(format!("accepted {name}"));
        }
    }
    for (name, source) in PUBLISHED_FIXTURES {
        let found = unpublished_mode_changes(source);
        if !found.is_empty() {
            wrong.push(format!("rejected {name}: {found:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Handlers read the published byte, and only the VT worker publishes it, so a fixture that
/// changes pointer modes with a raw `advance` must publish before it drives a pointer handler.
/// The scan reads Windows-gated fixtures as text, so a macOS run catches one that does not.
#[test]
fn fixtures_publish_parser_pointer_modes_before_driving_a_handler() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    rust_sources(&root.join("src"), &mut sources);
    rust_sources(&root.join("tests"), &mut sources);
    let mut failures = Vec::new();
    for path in sources {
        let text = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
        for (changed, handler) in unpublished_mode_changes(&text) {
            failures.push(format!(
                "{}:{changed} changes modes, line {handler} drives a handler unpublished",
                path.strip_prefix(root).unwrap().display()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A real event-loop proxy delivers output events into `App`: background-pane output makes no
/// native frame request, visible-pane output makes one, an explicit request for a settled window
/// makes one, and each serviced event arms the foreground probe once.
#[cfg(windows)]
#[test]
fn native_output_events_request_frames_only_for_visible_output_or_explicit_requests() {
    use crate::app::{pty_test_support::isolated, redraw::FrameSettlement, UserEvent};
    use std::{
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
        window::WindowId,
    };
    // winit allows one event loop per process, so the body runs in its own child process.
    if isolated() {
        return;
    }

    /// Services each queued output event through `App` and records its native requests and arm.
    struct Probe {
        app: App,
        child: WindowId,
        hidden_pane: u64,
        visible_pane: u64,
        serviced: usize,
        results: Vec<(u64, bool)>,
        progress: std::sync::Arc<NativeProbeProgress>,
    }
    impl ApplicationHandler<UserEvent> for Probe {
        fn resumed(&mut self, _: &ActiveEventLoop) {}
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
            if !matches!(event, UserEvent::PaneOutput { .. } | UserEvent::RequestRedraw(_)) {
                // The watchdog's wake carries no output to service.
                return;
            }
            let case = ["hidden", "visible", "explicit"][self.serviced];
            self.progress.set("output", case, "service");
            // Output reaches a pane only now, so earlier events cannot see it.
            let published = match self.serviced {
                0 => Some(self.hidden_pane),
                1 => Some(self.visible_pane),
                _ => None,
            };
            if let Some(pane) = published {
                let pane = &self.app.windows[&self.child].panes[&pane];
                pane.output_generation.fetch_add(1, Ordering::Release);
                pane.output_outstanding.store(true, Ordering::Release);
            }
            self.app.foreground_schedule.activity_wake = None;
            let before = crate::app::window_state::window_redraw_requests();
            ApplicationHandler::user_event(&mut self.app, event_loop, event);
            let requests = crate::app::window_state::window_redraw_requests() - before;
            self.results.push((requests, self.app.foreground_schedule.activity_wake.is_some()));
            // Settle the frame so the next request is not absorbed by one still in flight.
            if let Some(snapshot) = self.app.snapshot_window_redraw(self.child) {
                self.app.finish_window_redraw(
                    self.child,
                    &snapshot,
                    FrameSettlement::Presented,
                    Instant::now(),
                );
            }
            self.serviced += 1;
            if self.serviced == 3 {
                self.progress.set("output", "all", "exit_event_loop");
                event_loop.exit();
            }
        }
        fn about_to_wait(&mut self, _: &ActiveEventLoop) {
            self.progress.about_to_wait();
        }
    }

    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["visible", "background"]);
    app.windows.get_mut(&child).unwrap().tabs.activate(0);
    let visible_pane = app.windows[&child].tab_states[0].active_pane;
    let hidden_pane = app.windows[&child].tab_states[1].active_pane;
    let event_loop =
        EventLoop::<UserEvent>::with_user_event().with_any_thread(true).build().unwrap();
    let proxy = event_loop.create_proxy();
    for event in [
        UserEvent::PaneOutput { window_id: child, pane_id: hidden_pane },
        UserEvent::PaneOutput { window_id: child, pane_id: visible_pane },
        UserEvent::RequestRedraw(child),
    ] {
        proxy.send_event(event).expect("queue output event");
    }
    let (progress, events) = NativeProbeProgress::new();
    let watchdog = NativeWatchdog::start(
        progress.clone(),
        events,
        // The isolated child is killed at 60 s; 45 s plus the 2 s follow-up reports first.
        Instant::now() + Duration::from_secs(45),
        move || proxy.send_event(UserEvent::ClearShapeCache).is_ok(),
        std::io::stderr(),
        |code| sonicterm_logging::exit_with(code, "native output-event watchdog expired"),
    );
    let mut probe =
        Probe { app, child, hidden_pane, visible_pane, serviced: 0, results: Vec::new(), progress };
    let result = event_loop.run_app(&mut probe);
    drop(watchdog);
    result.unwrap();
    assert_eq!(probe.results, [(0, true), (1, true), (1, true)]);
}

#[cfg(windows)]
#[path = "test_hooks_window_pointer_native_fixture.rs"]
mod test_hooks_window_pointer_native_fixture;
#[cfg(windows)]
use test_hooks_window_pointer_native_fixture::*;

#[path = "test_hooks_window_pointer_watchdog_fixture.rs"]
mod test_hooks_window_pointer_watchdog_fixture;
#[cfg(windows)]
use test_hooks_window_pointer_watchdog_fixture::*;
