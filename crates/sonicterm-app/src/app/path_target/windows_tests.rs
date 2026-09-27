//! Windows-only path-target tests: ignored native probes that an external driver runs to check
//! Explorer selection and structural-path interaction in real windows.

#![cfg(target_os = "windows")]

use super::*;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// Native manual probe uses production validation/dispatch; Explorer selection is inspected by the caller.
#[cfg(target_os = "windows")]
#[test]
#[ignore = "opens Explorer for an explicitly supplied native test fixture"]
fn reveal_file_in_explorer_native_probe() {
    let path =
        PathBuf::from(std::env::var_os("SONICTERM_REVEAL_PROBE_FILE").expect("explicit fixture"));
    let decision = classify_local_target(&path);
    assert_eq!(decision, PathOpenDecision::Openable(PathKind::File));
    open_path(&path, decision).expect("native Explorer selection request");
}

/// A dedicated process runs real native input and path workers with scratch state; the driver verifies Explorer selection.
#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires an external native pointer driver and explicit scratch directory"]
fn structural_paths_native_interaction() {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
    };
    struct NativeProbe {
        app: App,
        root: PathBuf,
        started: std::time::Instant,
        sequence: u64,
        input_sequence: u64,
        last_pointer_event: serde_json::Value,
    }
    impl ApplicationHandler<super::super::super::UserEvent> for NativeProbe {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            self.app.resumed(el);
        }
        fn new_events(&mut self, el: &ActiveEventLoop, cause: winit::event::StartCause) {
            self.app.new_events(el, cause);
        }
        fn user_event(&mut self, el: &ActiveEventLoop, event: super::super::super::UserEvent) {
            self.app.user_event(el, event);
        }
        fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
            if matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::CursorEntered { .. }
                    | WindowEvent::CursorLeft { .. }
                    | WindowEvent::ModifiersChanged(_)
                    | WindowEvent::Focused(_)
                    | WindowEvent::MouseInput { .. }
            ) {
                self.input_sequence += 1;
                self.last_pointer_event = serde_json::json!({
                    "window": format!("{id:?}"),
                    "event": format!("{event:?}"),
                    "sequence": self.input_sequence,
                });
            }
            self.app.window_event(el, id, event);
        }
        fn device_event(
            &mut self,
            el: &ActiveEventLoop,
            id: winit::event::DeviceId,
            event: winit::event::DeviceEvent,
        ) {
            self.app.device_event(el, id, event);
        }
        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            self.app.about_to_wait(el);
            let native_windows = self.app.windows.iter().filter_map(|(id, state)| {
                let window = state.window.as_ref()?;
                let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else { return None };
                let active_pane = state.tab_states.get(state.tabs.active_index())?.active_pane;
                let pane = state.panes.get(&active_pane)?;
                let parser = pane.parser.try_lock()?;
                let rows = parser.grid().rows_iter().map(|row| row.iter().map(|c| c.ch).collect::<String>()).collect::<Vec<_>>();
                Some(serde_json::json!({"hwnd":handle.hwnd.get(),"main":Some(*id)==self.app.main_window_id,"width":window.inner_size().width,"height":window.inner_size().height,"scale":window.scale_factor(),"tabs":state.tabs.len(),"rows":rows,"hidden":state.hidden}))
            }).collect::<Vec<_>>();
            std::fs::write(
                self.root.join("windows.json"),
                serde_json::to_vec(&native_windows).unwrap(),
            )
            .unwrap();
            if let Some(id) = self.app.main_window_id {
                let window = &self.app.windows[&id];
                let tab = &window.tab_states[window.tabs.active_index()];
                if let (Some(native), Some(renderer), Some(pane)) =
                    (&window.window, &window.renderer, window.panes.get(&tab.active_pane))
                {
                    if let Some(parser) = pane.parser.try_lock() {
                        let RawWindowHandle::Win32(handle) =
                            native.window_handle().unwrap().as_raw()
                        else {
                            panic!("Windows handle")
                        };
                        let rows = parser
                            .grid()
                            .rows_iter()
                            .map(|row| row.iter().map(|c| c.ch).collect::<String>())
                            .collect::<Vec<_>>();
                        let (cw, ch) = renderer.cell_size();
                        let pane_id = window.tab_states[window.tabs.active_index()].active_pane;
                        let origin = renderer.pane_grid_origin(pane_id);
                        let pointer_cell = renderer.pixel_to_pane_cell(
                            window.cursor_pos.0 as f32,
                            window.cursor_pos.1 as f32,
                        );
                        // Observe the held parser without advancing probes or granting fresh authorization.
                        let fresh = pointer_cell
                            .filter(|(pointed_pane, _, _)| *pointed_pane == pane_id)
                            .and_then(|(_, row, col)| {
                                self.app.cell_target_from_parser(
                                    id,
                                    pane_id,
                                    row,
                                    col,
                                    &parser,
                                    pane.viewport_top_abs,
                                )
                            });
                        let probe = &window.path_probe;
                        let fresh_key = fresh.as_ref().and_then(|target| match &target.target {
                            ResolvedCellTarget::Path(key) => Some(key),
                            _ => None,
                        });
                        let current_matches =
                            fresh_key.is_some_and(|key| probe.current.as_ref() == Some(key));
                        let settled = if fresh_key.is_some() {
                            current_matches
                                && probe.pending_result.is_none()
                                && (probe.selection.is_some() || probe.failure.is_some())
                        } else {
                            fresh.is_none()
                                && pointer_cell.is_some_and(|(pane, _, _)| pane == pane_id)
                                && probe.current.is_none()
                                && probe.pending_result.is_none()
                        };
                        let mut native_cursor = windows::Win32::Foundation::POINT::default();
                        let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _);
                        let (native_cursor_screen, native_cursor_client, foreground) =
                            // SAFETY: native keeps hwnd live; native_cursor is writable and other handles are only compared.
                            unsafe {
                                use windows::Win32::{
                                    Graphics::Gdi::ScreenToClient,
                                    UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow},
                                };
                                let screen = GetCursorPos(&mut native_cursor)
                                    .ok()
                                    .map(|()| [native_cursor.x, native_cursor.y]);
                                let client = screen.and_then(|_| {
                                    ScreenToClient(hwnd, &mut native_cursor)
                                        .as_bool()
                                        .then_some([native_cursor.x, native_cursor.y])
                                });
                                (screen, client, GetForegroundWindow() == hwnd)
                            };
                        self.sequence += 1;
                        let report = serde_json::json!({
                            "sequence": self.sequence,
                            "input_sequence": self.input_sequence,
                            "last_pointer_event": self.last_pointer_event,
                            "cursor_pos": [window.cursor_pos.0, window.cursor_pos.1],
                            "native_cursor_screen": native_cursor_screen,
                            "native_cursor_client": native_cursor_client,
                            "foreground": foreground,
                            "open_modifier": self.app.open_modifier_held(id),
                            "probe": {
                                "epoch": probe.epoch.0,
                                "pointed": probe.current.as_ref().map(|key| [key.pointed.row, u64::from(key.pointed.col)]),
                                "current_matches": current_matches,
                                "settled": settled,
                                "pending_result": probe.pending_result.is_some(),
                                "failure": probe.failure,
                                "selection": probe.selection.as_ref().map(|selection| selection.candidate.resolved_path.to_string_lossy()),
                                "fresh_kind": match fresh.as_ref().map(|target| &target.target) {
                                    None => "none",
                                    Some(ResolvedCellTarget::Path(_)) => "path",
                                    Some(ResolvedCellTarget::Uri(_)) => "uri",
                                    Some(ResolvedCellTarget::Rejected(_)) => "rejected",
                                },
                            },
                            "hwnd": handle.hwnd.get(), "rows": rows, "cw": cw, "ch": ch,
                            "top": origin.map(|p| p[1]), "tab_bar_top": renderer.tab_bar_y_offset(),
                            "surface_height": renderer.height(), "padding_bottom": renderer.padding_bottom_px(),
                            "view_top": GpuRenderer::resolved_view_top_abs_legacy(parser.grid(), pane.viewport_top_abs),
                            "search_current": tab.search.as_ref().and_then(|s| s.current),
                            "search_total": tab.search.as_ref().map(|s| s.matches.len()),
                            "pointer_cell": renderer.pixel_to_pane_cell(window.cursor_pos.0 as f32, window.cursor_pos.1 as f32),
                            "selection_rows": window.selection.as_ref().map(|s| {let (a,b)=s.normalized(); [a.0,b.0]}),
                            "padding_left": self.app.config.window.padding_left,
                            "preview": window.link_preview.as_ref().map(|p| &p.uri),
                            "notification": window.notification.as_ref().map(|n| &n.message),
                            "links": parser.grid().rows_iter().enumerate().flat_map(|(row, cells)| cells.iter().enumerate().filter_map(move |(col, cell)| cell.hyperlink().map(|id| (row,col,id)))).filter_map(|(row,col,id)| parser.hyperlinks().lookup(id).map(|link| serde_json::json!({"row":row,"col":col,"uri":link.uri}))).collect::<Vec<_>>()});
                        std::fs::write(
                            self.root.join("window.json"),
                            serde_json::to_vec(&report).unwrap(),
                        )
                        .unwrap();
                    }
                }
            }
            if self.root.join("done").exists() {
                el.exit();
            }
            assert!(
                self.started.elapsed() < std::time::Duration::from_secs(180),
                "native driver deadline"
            );
            el.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(50),
            ));
        }
    }
    assert!(std::env::var_os("NO_COLOR").is_none(), "native color profile required");
    let root = PathBuf::from(
        std::env::var_os("SONICTERM_PATH_INTERACTION_DIR").expect("explicit scratch directory"),
    );
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::create_dir_all(root.join("logs")).unwrap();
    let mut config = Config::default();
    config.terminal.shell = Some("cmd.exe".into());
    if root.join("cold").exists() {
        config.window.warm_window_pool = 0;
    }
    config.window.cols = 110;
    config.window.rows = 28;
    config.logging.level = sonicterm_logging::LogLevel::Debug;
    let _log = sonicterm_logging::init_in(&config.logging, &root.join("logs")).unwrap();
    tracing::warn!(
        target: "sonicterm_app::app::path_target::path_target_tests",
        "native path interaction started"
    );
    let event_loop = EventLoop::<super::super::super::UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .unwrap();
    let mut app = App::new_with_proxy(
        Theme::default(),
        config,
        Keymap::parse_resilient(
            &format!(
                "{}\n[[binding]]\nkeys = \"alt+shift+x\"\naction = \"move_tab_to_new_window\"\n",
                include_str!("../../../../../assets/keymaps/sonicterm-windows.toml")
            ),
            "native fixture",
        )
        .unwrap(),
        Some(event_loop.create_proxy()),
    );
    // Inject only fixture path resolution; the child shell must keep the user's real HOME.
    if root.join("home").is_dir() {
        app.home_dir = Some(root.join("home"));
    }
    app.runtime_config_path = Some(root.join("config/sonicterm.toml"));
    let mut probe = NativeProbe {
        app,
        root,
        started: std::time::Instant::now(),
        sequence: 0,
        input_sequence: 0,
        last_pointer_event: serde_json::Value::Null,
    };
    event_loop.run_app(&mut probe).unwrap();
    assert!(
        probe.root.join("done").exists(),
        "driver must verify native outcomes before completion"
    );
}
