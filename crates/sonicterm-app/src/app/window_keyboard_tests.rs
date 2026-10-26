pub(super) use crate::app::{window_event::begin_pointer_gesture, App, PointerCell};
pub(super) use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
pub(super) use sonicterm_vt::vt::MouseTracking;
#[cfg(windows)]
pub(super) use winit::keyboard::{KeyCode, PhysicalKey};

pub(super) fn pointer_cell(pane_id: u64, row: u16, col: u16) -> PointerCell {
    PointerCell { pane_id, row, col }
}

/// Native V events route configured paste to the source READONLY search without taking input from composition or Rename Window.
#[cfg(windows)]
#[test]
pub(super) fn real_pty_search_paste_native_key_owner_matrix() {
    use crate::app::pty_test_support::isolated;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN},
    };
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
        window::Window,
    };
    if isolated() {
        return;
    }
    struct Probe {
        window: Option<Window>,
        ran: bool,
        deadline: std::time::Instant,
    }
    impl ApplicationHandler for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let window = event_loop
                .create_window(Window::default_attributes().with_visible(false).with_active(false))
                .unwrap();
            let RawWindowHandle::Win32(handle) = window.window_handle().unwrap().as_raw() else {
                panic!("Windows handle")
            };
            // SAFETY: the test owns this HWND; posting one V key does not inject keyboard input into another application.
            unsafe {
                PostMessageW(
                    Some(HWND(handle.hwnd.get() as *mut _)),
                    WM_KEYDOWN,
                    WPARAM(0x56),
                    LPARAM(1 | (0x2f << 16)),
                )
                .unwrap();
            }
            self.window = Some(window);
        }
        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            id: winit::window::WindowId,
            event: WindowEvent,
        ) {
            if Some(id) != self.window.as_ref().map(Window::id) || self.ran {
                return;
            }
            if let WindowEvent::KeyboardInput { event: key, is_synthetic: false, .. } = event {
                assert_eq!(key.physical_key, PhysicalKey::Code(KeyCode::KeyV));
                run_search_paste_key_cases(&key);
                self.ran = true;
                event_loop.exit();
            }
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            assert!(std::time::Instant::now() < self.deadline, "native V delivery timed out");
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(10),
            ));
        }
    }
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let mut probe = Probe {
        window: None,
        ran: false,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(20),
    };
    event_loop.run_app(&mut probe).unwrap();
    assert!(probe.ran);
}

/// Native key metadata stays intact; only the source window's stored modifiers select the configured paste chord.
#[cfg(windows)]
pub(super) fn run_search_paste_key_cases(native: &winit::event::KeyEvent) {
    use crate::app::{
        key_encoding::key_event_to_string,
        mod_tests::{input_test_windows, PtySubmissions},
        text_edit::{printable_event_text, search_text_edit_for_event},
    };
    use sonicterm_cfg::keymap::{ActionWrapper, Binding, BroadcastScope};
    use sonicterm_ui::{broadcast::BroadcastState, copy_mode::CopyModeState, search::SearchState};
    for target in 0..2 {
        let (mut app, windows) = input_test_windows();
        let (source_window, source) = windows[target];
        let (other_window, bracketed) = windows[(target + 1) % 3];
        for (id, _) in windows {
            app.windows.get_mut(&id).unwrap().tab_states[0].search = Some(SearchState::new());
        }
        app.pane_by_id(bracketed).unwrap().parser.lock().advance(b"\x1b[?2004h");
        app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
        app.frontmost_window = Some(other_window);
        app.__test_enable_pty_write_log();
        let submitted = PtySubmissions::start();
        for (chord, modifiers) in [
            ("ctrl+shift+v", ModifiersState::CONTROL | ModifiersState::SHIFT),
            (
                "ctrl+alt+shift+v",
                ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SHIFT,
            ),
        ] {
            app.keymap.bindings = vec![Binding {
                keys: chord.into(),
                action: ActionWrapper(Action::PasteFromClipboard),
            }];
            app.windows.get_mut(&source_window).unwrap().modifiers = modifiers;
            app.windows.get_mut(&other_window).unwrap().modifiers = ModifiersState::empty();
            assert_eq!(key_event_to_string(native, modifiers).as_deref(), Some(chord));
            assert!(search_text_edit_for_event(native, modifiers).is_none());
            assert!(printable_event_text(native, modifiers).is_none());
            for (read_only, search_open) in
                [(false, true), (true, false), (false, false), (true, true)]
            {
                let window = app.windows.get_mut(&source_window).unwrap();
                window.copy_mode = read_only.then(|| CopyModeState::read_only_at((0, 0)));
                window.tab_states[0].search = search_open.then(SearchState::new);
                app.__test_set_memory_clipboard("paste");
                app.wait_for_input_queues();
                app.handle_window_keyboard(source_window, native, false);
                let attempts = app.__test_drain_pty_writes();
                let accepted = submitted.take();
                assert_eq!(
                    app.windows[&source_window].tab_states[0]
                        .search
                        .as_ref()
                        .map(|search| search.query.as_str()),
                    search_open.then_some("paste"),
                    "target={target} chord={chord} read_only={read_only}"
                );
                if read_only || search_open {
                    assert_eq!((attempts, accepted), (Vec::new(), Vec::new()));
                } else {
                    let peers: std::collections::BTreeSet<_> = windows
                        .iter()
                        .map(|(_, pane)| *pane)
                        .filter(|pane| *pane != source)
                        .collect();
                    let expected: Vec<_> = std::iter::once(source)
                        .chain(peers)
                        .map(|pane| {
                            (
                                pane,
                                if pane == bracketed {
                                    b"\x1b[200~paste\x1b[201~".to_vec()
                                } else {
                                    b"paste".to_vec()
                                },
                            )
                        })
                        .collect();
                    assert_eq!(attempts, expected);
                    assert_eq!(accepted, expected);
                }
                assert!(app.windows[&source_window].pty_pressed_keys.is_empty());
                assert_eq!(
                    app.windows[&source_window]
                        .copy_mode
                        .as_ref()
                        .is_some_and(CopyModeState::is_read_only),
                    read_only
                );
                assert!(app.windows[&source_window].notification.is_none());
            }
            // IME composition consumes the chord even though search is open and READONLY permits local editing.
            app.windows.get_mut(&source_window).unwrap().ime.handle_preedit("compose", None);
            app.__test_set_memory_clipboard("blocked");
            app.handle_window_keyboard(source_window, native, false);
            assert_eq!(
                app.windows[&source_window].tab_states[0].search.as_ref().unwrap().query,
                "paste"
            );
            assert!(app.windows[&source_window].ime.is_composing());
            assert_eq!((app.__test_drain_pty_writes(), submitted.take()), (Vec::new(), Vec::new()));
            app.windows.get_mut(&source_window).unwrap().ime.cancel();
            app.start_rename_window(source_window);
            app.__test_set_memory_clipboard("renamed");
            app.handle_window_keyboard(source_window, native, false);
            assert_eq!(app.command_palette.query(), "renamed");
            assert_eq!(
                app.windows[&source_window].tab_states[0].search.as_ref().unwrap().query,
                "paste"
            );
            assert_eq!((app.__test_drain_pty_writes(), submitted.take()), (Vec::new(), Vec::new()));
            app.cancel_window_rename(source_window);
            // Commands and RenameTab fields own the same configured chord ahead of READONLY, search, and terminal delivery.
            for rename_tab in [false, true] {
                app.windows.get_mut(&source_window).unwrap().copy_mode = None;
                assert!(app.run_action_for_window(&Action::OpenCommandPalette, source_window));
                if rename_tab {
                    assert!(app.run_action_for_window(&Action::RenameTab, source_window));
                }
                app.command_palette.set_query("");
                app.windows.get_mut(&source_window).unwrap().copy_mode =
                    Some(CopyModeState::read_only_at((0, 0)));
                app.__test_set_memory_clipboard("field");
                app.wait_for_input_queues();
                app.handle_window_keyboard(source_window, native, false);
                assert_eq!(
                    app.command_palette.query(),
                    "field",
                    "target={target} chord={chord} rename_tab={rename_tab}"
                );
                assert_eq!(
                    app.windows[&source_window].tab_states[0].search.as_ref().unwrap().query,
                    "paste"
                );
                assert_eq!(
                    (app.__test_drain_pty_writes(), submitted.take()),
                    (Vec::new(), Vec::new())
                );
                app.command_palette.close();
                app.palette_attached_window = None;
                app.tab_edit_target = None;
            }
            app.windows.get_mut(&source_window).unwrap().copy_mode = None;
            for (id, _) in windows {
                if id != source_window {
                    assert_eq!(app.windows[&id].tab_states[0].search.as_ref().unwrap().query, "");
                    assert!(app.windows[&id].copy_mode.is_none());
                    assert!(app.windows[&id].notification.is_none());
                }
            }
            assert_eq!(app.frontmost_window, Some(other_window));
        }
        // Windows Alt+V stays a terminal passthrough even when configured as Paste, so an open field consumes it without text.
        app.keymap.bindings = vec![Binding {
            keys: "alt+v".into(),
            action: ActionWrapper(Action::PasteFromClipboard),
        }];
        app.windows.get_mut(&source_window).unwrap().modifiers = ModifiersState::ALT;
        assert_eq!(key_event_to_string(native, ModifiersState::ALT).as_deref(), Some("alt+v"));
        assert!(printable_event_text(native, ModifiersState::ALT).is_none());
        for palette in [false, true] {
            let window = app.windows.get_mut(&source_window).unwrap();
            window.copy_mode = None;
            window.tab_states[0].search = Some(SearchState::new());
            if palette {
                assert!(app.run_action_for_window(&Action::OpenCommandPalette, source_window));
            }
            app.__test_set_memory_clipboard("alt");
            app.wait_for_input_queues();
            app.handle_window_keyboard(source_window, native, false);
            let case = format!("target={target} palette={palette}");
            assert_eq!(
                app.windows[&source_window].tab_states[0].search.as_ref().unwrap().query,
                "",
                "{case}"
            );
            assert_eq!(app.command_palette.query(), "", "{case}");
            assert_eq!(
                (app.__test_drain_pty_writes(), submitted.take()),
                (Vec::new(), Vec::new()),
                "{case}"
            );
            assert!(app.windows[&source_window].pty_pressed_keys.is_empty(), "{case}");
            app.command_palette.close();
            app.palette_attached_window = None;
        }
    }
}

/// Native A, Left, and C events select, extend, and copy inside owned fields without a PTY; Ctrl+A outside a field stays terminal input.
#[cfg(windows)]
#[test]
pub(super) fn real_pty_field_selection_native_key_matrix() {
    use crate::app::pty_test_support::isolated;
    if isolated() {
        return;
    }
    // VK_A, extended VK_LEFT, and VK_C, each with its scan code in the WM_KEYDOWN lParam.
    let events = native_key_downs(&[
        (0x41, 1 | (0x1e << 16)),
        (0x25, 1 | (0x4b << 16) | (1 << 24)),
        (0x43, 1 | (0x2e << 16)),
    ]);
    assert_eq!(events[0].physical_key, PhysicalKey::Code(KeyCode::KeyA));
    assert_eq!(events[1].physical_key, PhysicalKey::Code(KeyCode::ArrowLeft));
    assert_eq!(events[2].physical_key, PhysicalKey::Code(KeyCode::KeyC));
    run_field_selection_key_cases(&events[0], &events[1], &events[2]);
}

/// Deliver owned WM_KEYDOWN messages through a hidden winit window and return the pressed key events in order.
#[cfg(windows)]
pub(super) fn native_key_downs(keys: &[(usize, isize)]) -> Vec<winit::event::KeyEvent> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN},
    };
    use winit::{
        application::ApplicationHandler,
        event::{ElementState, KeyEvent, WindowEvent},
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
        window::Window,
    };
    struct Probe<'keys> {
        keys: &'keys [(usize, isize)],
        window: Option<Window>,
        events: Vec<KeyEvent>,
        deadline: std::time::Instant,
    }
    impl ApplicationHandler for Probe<'_> {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() {
                return;
            }
            let window = event_loop
                .create_window(Window::default_attributes().with_visible(false).with_active(false))
                .unwrap();
            let RawWindowHandle::Win32(handle) = window.window_handle().unwrap().as_raw() else {
                panic!("Windows handle")
            };
            for (virtual_key, key_data) in self.keys {
                // SAFETY: the test owns this HWND; posting its own key messages injects no input into another application.
                unsafe {
                    PostMessageW(
                        Some(HWND(handle.hwnd.get() as *mut _)),
                        WM_KEYDOWN,
                        WPARAM(*virtual_key),
                        LPARAM(*key_data),
                    )
                    .unwrap();
                }
            }
            self.window = Some(window);
        }
        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            id: winit::window::WindowId,
            event: WindowEvent,
        ) {
            if Some(id) != self.window.as_ref().map(Window::id) {
                return;
            }
            if let WindowEvent::KeyboardInput { event: key, is_synthetic: false, .. } = event {
                if key.state == ElementState::Pressed {
                    self.events.push(key);
                }
                if self.events.len() == self.keys.len() {
                    event_loop.exit();
                }
            }
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            assert!(std::time::Instant::now() < self.deadline, "native key delivery timed out");
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(10),
            ));
        }
    }
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let mut probe = Probe {
        keys,
        window: None,
        events: Vec::new(),
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(20),
    };
    event_loop.run_app(&mut probe).unwrap();
    assert_eq!(probe.events.len(), keys.len());
    probe.events
}

/// Every owned field applies select-all, Shift extension, and configured copy from native keys, even in READONLY.
#[cfg(windows)]
pub(super) fn run_field_selection_key_cases(
    key_a: &winit::event::KeyEvent,
    left: &winit::event::KeyEvent,
    key_c: &winit::event::KeyEvent,
) {
    use crate::app::mod_tests::{input_test_windows, PtySubmissions};
    use sonicterm_cfg::keymap::{ActionWrapper, Binding};
    use sonicterm_ui::{copy_mode::CopyModeState, search::SearchState};
    let text = "你é好";
    for target in 0..2 {
        let (mut app, windows) = input_test_windows();
        let (source_window, source) = windows[target];
        let (other_window, _) = windows[(target + 1) % 3];
        app.keymap.bindings = vec![Binding {
            keys: "ctrl+shift+c".into(),
            action: ActionWrapper(Action::CopyToClipboard),
        }];
        app.__test_enable_pty_write_log();
        let submitted = PtySubmissions::start();
        for field in ["search", "commands", "rename-tab"] {
            let case = format!("target={target} field={field}");
            app.command_palette.close();
            app.palette_attached_window = None;
            app.tab_edit_target = None;
            let window = app.windows.get_mut(&source_window).unwrap();
            window.copy_mode = None;
            window.tab_states[0].search = None;
            if field == "search" {
                let mut search = SearchState::new();
                search.set_query(text, window.panes[&source].parser.lock().grid());
                window.tab_states[0].search = Some(search);
            } else {
                // When: a palette field is under test, open it on the source through its public action route.
                assert!(app.run_action_for_window(&Action::OpenCommandPalette, source_window));
                if field == "rename-tab" {
                    assert!(app.run_action_for_window(&Action::RenameTab, source_window));
                }
                app.command_palette.set_query(text);
            }
            app.frontmost_window = Some(other_window);
            // READONLY still permits local field editing and copying.
            app.windows.get_mut(&source_window).unwrap().copy_mode =
                Some(CopyModeState::read_only_at((0, 0)));
            let selected = |app: &App| -> Option<String> {
                if field == "search" {
                    let search = app.windows[&source_window].tab_states[0].search.as_ref()?;
                    search.selected_text().map(str::to_owned)
                } else {
                    // When: a palette field is under test, its selection lives on the shared palette.
                    app.command_palette.selected_text().map(str::to_owned)
                }
            };
            let search_state = |app: &App| {
                app.windows[&source_window].tab_states[0].search.as_ref().map(|search| {
                    (search.matches.len(), search.current, search.requested_scroll_row)
                })
            };
            let before = search_state(&app);
            app.wait_for_input_queues();

            app.windows.get_mut(&source_window).unwrap().modifiers = ModifiersState::CONTROL;
            app.handle_window_keyboard(source_window, key_a, false);
            assert_eq!(selected(&app).as_deref(), Some(text), "{case}");

            // Collapse to the end, then extend one scalar left over the final wide character.
            if field == "search" {
                let window = app.windows.get_mut(&source_window).unwrap();
                window.tab_states[0].search.as_mut().unwrap().set_cursor(text.len());
            } else {
                // When: a palette field is under test, collapse the shared palette selection.
                app.command_palette.set_cursor(text.len());
            }
            app.windows.get_mut(&source_window).unwrap().modifiers = ModifiersState::SHIFT;
            app.handle_window_keyboard(source_window, left, false);
            assert_eq!(selected(&app).as_deref(), Some("好"), "{case}");

            app.__test_set_memory_clipboard("sentinel");
            app.windows.get_mut(&source_window).unwrap().modifiers =
                ModifiersState::CONTROL | ModifiersState::SHIFT;
            app.handle_window_keyboard(source_window, key_c, false);
            assert_eq!(app.__test_memory_clipboard().as_deref(), Some("好"), "{case}");
            assert_eq!(selected(&app).as_deref(), Some("好"), "{case}");
            // Selection-only keys neither rescan search nor move its focused match or viewport.
            assert_eq!(search_state(&app), before, "{case}");
            assert_eq!(
                (app.__test_drain_pty_writes(), submitted.take()),
                (Vec::new(), Vec::new()),
                "{case}"
            );
            assert!(app.windows[&source_window].pty_pressed_keys.is_empty(), "{case}");
        }

        // Outside a field, Ctrl+A is not select-all: it reaches the terminal owner's configured binding.
        // Posted messages hold no real Control state, so raw chord encoding is not observable here.
        app.command_palette.close();
        app.palette_attached_window = None;
        app.tab_edit_target = None;
        app.keymap.bindings = vec![Binding {
            keys: "ctrl+a".into(),
            action: ActionWrapper(Action::PasteFromClipboard),
        }];
        let window = app.windows.get_mut(&source_window).unwrap();
        window.copy_mode = None;
        window.tab_states[0].search = None;
        window.modifiers = ModifiersState::CONTROL;
        app.__test_set_memory_clipboard("ctl");
        app.wait_for_input_queues();
        app.handle_window_keyboard(source_window, key_a, false);
        let expected = vec![(source, b"ctl".to_vec())];
        assert_eq!(app.__test_drain_pty_writes(), expected, "target={target}");
        assert_eq!(submitted.take(), expected, "target={target}");
    }
}

/// A native `[` whose logical key reads `{` reaches field copy and paste configured only on its later alias, on main and child.
#[cfg(windows)]
#[test]
pub(super) fn real_pty_field_clipboard_native_key_aliases() {
    use crate::app::pty_test_support::isolated;
    if isolated() {
        return;
    }
    // VK_OEM_4 with its US-layout scan code; posted messages carry no real Shift or Control state.
    let native = native_key_downs(&[(0xdb, 1 | (0x1a << 16))]).remove(0);
    assert_eq!(native.physical_key, PhysicalKey::Code(KeyCode::BracketLeft));
    run_field_clipboard_alias_cases(&native);
}

/// Configured clipboard aliases reach search and palette fields, while an earlier non-clipboard alias keeps precedence.
#[cfg(windows)]
pub(super) fn run_field_clipboard_alias_cases(native: &winit::event::KeyEvent) {
    use crate::app::{
        key_encoding::key_event_to_strings,
        mod_tests::{input_test_windows, PtySubmissions},
    };
    use sonicterm_cfg::keymap::{ActionWrapper, Binding};
    use sonicterm_ui::search::SearchState;
    use winit::{keyboard::Key, platform::modifier_supplement::KeyEventExtModifierSupplement};
    assert_eq!(native.key_without_modifiers(), Key::Character("[".into()), "US layout OEM_4");
    // Synthetic logical metadata: Shift+[ reports `{` on a US layout; the private unmodified `[` stays native.
    let mut aliased = native.clone();
    aliased.logical_key = Key::Character("{".into());
    aliased.text = None;
    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    assert_eq!(key_event_to_strings(&aliased, mods), ["ctrl+shift+[", "ctrl+shift+{"]);
    let bind =
        |keys: &str, action: Action| Binding { keys: keys.into(), action: ActionWrapper(action) };
    for target in 0..2 {
        let (mut app, windows) = input_test_windows();
        let (source_window, source) = windows[target];
        let (other_window, _) = windows[(target + 1) % 3];
        app.__test_enable_pty_write_log();
        let submitted = PtySubmissions::start();
        app.frontmost_window = Some(other_window);
        app.windows.get_mut(&source_window).unwrap().modifiers = mods;
        // Reset the source to a `needle` search, with the palette over it for the commands field.
        let open_field = |app: &mut App, field: &str, select_all: bool| {
            app.command_palette.close();
            app.palette_attached_window = None;
            let window = app.windows.get_mut(&source_window).unwrap();
            let mut search = SearchState::new();
            search.set_query("needle", window.panes[&source].parser.lock().grid());
            if select_all {
                search.select_all();
            }
            window.tab_states[0].search = Some(search);
            if field == "commands" {
                // When: the palette field is under test, open it on the source through its public action route.
                assert!(app.run_action_for_window(&Action::OpenCommandPalette, source_window));
                app.command_palette.set_query("needle");
                if select_all {
                    app.command_palette.select_all();
                }
            }
        };
        let field_state = |app: &App, field: &str| -> (String, Option<String>) {
            if field == "search" {
                let search = app.windows[&source_window].tab_states[0].search.as_ref().unwrap();
                (search.query.clone(), search.selected_text().map(str::to_owned))
            } else {
                // When: the palette field is under test, its text and selection live on the shared palette.
                let palette = &app.command_palette;
                (palette.query().to_owned(), palette.selected_text().map(str::to_owned))
            }
        };
        for field in ["search", "commands"] {
            let case = format!("target={target} field={field}");
            // Copy bound only on the later `{` alias copies the field's own selection.
            app.keymap.bindings = vec![bind("ctrl+shift+{", Action::CopyToClipboard)];
            open_field(&mut app, field, true);
            app.__test_set_memory_clipboard("sentinel");
            app.wait_for_input_queues();
            app.handle_window_keyboard(source_window, &aliased, false);
            assert_eq!(app.__test_memory_clipboard().as_deref(), Some("needle"), "{case}");
            assert_eq!(
                field_state(&app, field),
                ("needle".into(), Some("needle".into())),
                "{case}"
            );

            // Paste bound only on the later alias inserts at the caret, which open_field leaves at the end.
            app.keymap.bindings = vec![bind("ctrl+shift+{", Action::PasteFromClipboard)];
            open_field(&mut app, field, false);
            app.__test_set_memory_clipboard("paste");
            app.handle_window_keyboard(source_window, &aliased, false);
            assert_eq!(field_state(&app, field).0, "needlepaste", "{case}");

            // The earlier `[` alias matches first, so its non-clipboard action wins even though the
            // clipboard binding is listed first; the palette's toggle check would close it, so it uses ToggleTabBar.
            let earlier =
                if field == "search" { Action::OpenCommandPalette } else { Action::ToggleTabBar };
            app.keymap.bindings =
                vec![bind("ctrl+shift+{", Action::CopyToClipboard), bind("ctrl+shift+[", earlier)];
            open_field(&mut app, field, true);
            app.__test_set_memory_clipboard("sentinel");
            app.handle_window_keyboard(source_window, &aliased, false);
            assert_eq!(app.__test_memory_clipboard().as_deref(), Some("sentinel"), "{case}");
            if field == "search" {
                // When: search owned the key, its earlier binding opened the palette on the source.
                assert!(app.command_palette_owns_input(source_window), "{case}");
                assert_eq!(
                    field_state(&app, field),
                    ("needle".into(), Some("needle".into())),
                    "{case}"
                );
            }
            assert_eq!(
                (app.__test_drain_pty_writes(), submitted.take()),
                (Vec::new(), Vec::new()),
                "{case}"
            );
            assert!(app.windows[&source_window].pty_pressed_keys.is_empty(), "{case}");
            app.command_palette.close();
            app.palette_attached_window = None;
        }
        assert_eq!(app.frontmost_window, Some(other_window));
    }
}

/// Native source dispatch retains accepted key routes after READONLY or search takes ownership, but new presses and orphan repeats do not inherit them.
#[cfg(windows)]
#[test]
pub(super) fn real_pty_native_accepted_key_routes_survive_local_ownership() {
    use crate::app::pty_test_support::isolated;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN},
    };
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
        window::Window,
    };
    if isolated() {
        return;
    }
    struct Probe {
        window: Option<Window>,
        ran: bool,
        deadline: std::time::Instant,
    }
    impl ApplicationHandler for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let window = event_loop
                .create_window(Window::default_attributes().with_visible(false).with_active(false))
                .unwrap();
            let RawWindowHandle::Win32(handle) = window.window_handle().unwrap().as_raw() else {
                panic!("Windows handle")
            };
            // SAFETY: this test owns the HWND; scalar metadata requests one extended ArrowUp key without global input injection.
            unsafe {
                PostMessageW(
                    Some(HWND(handle.hwnd.get() as *mut _)),
                    WM_KEYDOWN,
                    WPARAM(0x26),
                    LPARAM(1 | (0x48 << 16) | (1 << 24)),
                )
                .unwrap();
            }
            self.window = Some(window);
        }
        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            id: winit::window::WindowId,
            event: WindowEvent,
        ) {
            if Some(id) != self.window.as_ref().map(Window::id) || self.ran {
                return;
            }
            if let WindowEvent::KeyboardInput { event: key, is_synthetic: false, .. } = event {
                assert_eq!(key.physical_key, PhysicalKey::Code(KeyCode::ArrowUp));
                run_accepted_key_cases(&key);
                self.ran = true;
                event_loop.exit();
            }
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            assert!(std::time::Instant::now() < self.deadline, "native ArrowUp delivery timed out");
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(10),
            ));
        }
    }
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let mut probe = Probe {
        window: None,
        ran: false,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(20),
    };
    event_loop.run_app(&mut probe).unwrap();
    assert!(probe.ran);
}

#[cfg(windows)]
pub(super) fn run_accepted_key_cases(native: &winit::event::KeyEvent) {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::phase,
    };
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::{broadcast::BroadcastState, copy_mode::CopyModeState, search::SearchState};
    use std::collections::BTreeSet;
    use winit::{
        event::ElementState,
        keyboard::{Key, NamedKey},
    };
    for target in 0..3 {
        for local_overlay in [false, true] {
            let (mut app, windows) = input_test_windows();
            let (source_window, source) = windows[target];
            let (protected_window, protected) = windows[(target + 1) % 3];
            let (_, peer) = windows[(target + 2) % 3];
            for (window, pane) in windows {
                let pane = &app.windows[&window].panes[&pane];
                let mut parser = pane.parser.lock();
                parser.advance(b"\x1b[=10u");
                pane.keyboard_input
                    .store(parser.keyboard_input_snapshot(), std::sync::atomic::Ordering::Relaxed);
            }
            app.broadcast =
                BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
            app.wait_for_input_queues();
            let submitted = PtySubmissions::start();
            phase(source, "accepted-key-press");
            app.handle_window_keyboard(source_window, native, false);
            let all = BTreeSet::from([source, protected, peer]);
            let writes = submitted.take();
            assert_eq!(writes.iter().map(|(pane, _)| *pane).collect::<BTreeSet<_>>(), all);
            assert!(writes.iter().all(|(_, bytes)| bytes == b"\x1b[A"));
            app.windows.get_mut(&protected_window).unwrap().copy_mode =
                Some(CopyModeState::read_only_at((0, 0)));
            // Only Kitty event state is varied on these native-backed events; native Win32 metadata is not consumed in this mode.
            let mut fresh = native.clone();
            fresh.physical_key = PhysicalKey::Code(KeyCode::ArrowDown);
            fresh.logical_key = Key::Named(NamedKey::ArrowDown);
            app.wait_for_input_queues();
            phase(source, "new-key-filtered");
            app.handle_window_keyboard(source_window, &fresh, false);
            let writes = submitted.take();
            assert_eq!(
                writes.iter().map(|(pane, _)| *pane).collect::<BTreeSet<_>>(),
                BTreeSet::from([source, peer])
            );
            assert!(writes.iter().all(|(_, bytes)| bytes == b"\x1b[B"));
            let source_state = app.windows.get_mut(&source_window).unwrap();
            if local_overlay {
                source_state.tab_states[0].search = Some(SearchState::new());
            } else {
                source_state.copy_mode = Some(CopyModeState::read_only_at((0, 0)));
            }
            let mut repeated = native.clone();
            repeated.repeat = true;
            app.wait_for_input_queues();
            phase(source, "accepted-key-repeat");
            app.handle_window_keyboard(source_window, &repeated, false);
            let writes = submitted.take();
            assert_eq!(writes.iter().map(|(pane, _)| *pane).collect::<BTreeSet<_>>(), all);
            assert!(writes.iter().all(|(_, bytes)| bytes == b"\x1b[1;1:2A"));
            let mut release = native.clone();
            release.state = ElementState::Released;
            release.repeat = false;
            app.wait_for_input_queues();
            phase(source, "accepted-key-release");
            app.handle_window_keyboard(source_window, &release, false);
            let writes = submitted.take();
            assert_eq!(writes.iter().map(|(pane, _)| *pane).collect::<BTreeSet<_>>(), all);
            assert!(writes.iter().all(|(_, bytes)| bytes == b"\x1b[1;1:3A"));
            assert!(!app.windows[&source_window]
                .pty_pressed_keys
                .contains_key(&native.physical_key));
            let mut local_press = native.clone();
            local_press.physical_key = PhysicalKey::Code(KeyCode::ArrowLeft);
            local_press.logical_key = Key::Named(NamedKey::ArrowLeft);
            app.handle_window_keyboard(source_window, &local_press, false);
            assert!(submitted.take().is_empty());
            app.windows.get_mut(&source_window).unwrap().copy_mode = None;
            app.windows.get_mut(&source_window).unwrap().tab_states[0].search = None;
            // No accepted ArrowLeft hold exists, even though the source now accepts ordinary terminal input.
            local_press.repeat = true;
            app.handle_window_keyboard(source_window, &local_press, false);
            assert!(submitted.take().is_empty());
        }
    }
}

/// A window with two split panes (main or child), returning the window, its active pane and
/// the peer pane beside it.
pub(super) fn split_focus_window(
    app: &mut App,
    in_child: bool,
) -> (winit::window::WindowId, u64, u64) {
    let viewport = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0);
    if in_child {
        let child = app.__test_seed_child_window(&["child"]);
        let peer = app.__test_child_active_pane(child).expect("seeded child pane");
        assert!(app.__test_set_child_pane_viewport(child, viewport, 10.0, 10.0));
        assert!(app.__test_child_split_active_right(child));
        let active = app.__test_child_active_pane(child).expect("split child pane");
        (child, active, peer)
    } else {
        let peer = app.__test_seed_tab("main");
        assert!(app.__test_set_main_pane_viewport(viewport, 10.0, 10.0));
        app.__test_split_active_right();
        let active = app.__test_active_pane_in_tab(0).expect("split main pane");
        (app.main_window_id.expect("main window"), active, peer)
    }
}
