use crate::app::{window_event::begin_pointer_gesture, App, PointerCell};
use sonicterm_cfg::{
    config::Config,
    keymap::{Action, Keymap},
    theme::Theme,
};
use sonicterm_vt::vt::MouseTracking;
use winit::keyboard::ModifiersState;
#[cfg(windows)]
use winit::keyboard::{KeyCode, PhysicalKey};

fn pointer_cell(pane_id: u64, row: u16, col: u16) -> PointerCell {
    PointerCell { pane_id, row, col }
}

/// Native V events route configured paste to the source READONLY search without taking input from composition or Rename Window.
#[cfg(windows)]
#[test]
fn real_pty_search_paste_native_key_owner_matrix() {
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
fn run_search_paste_key_cases(native: &winit::event::KeyEvent) {
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
            for (id, _) in windows {
                if id != source_window {
                    assert_eq!(app.windows[&id].tab_states[0].search.as_ref().unwrap().query, "");
                    assert!(app.windows[&id].copy_mode.is_none());
                    assert!(app.windows[&id].notification.is_none());
                }
            }
            assert_eq!(app.frontmost_window, Some(other_window));
        }
    }
}

/// Native source dispatch retains accepted key routes after READONLY or search takes ownership, but new presses and orphan repeats do not inherit them.
#[cfg(windows)]
#[test]
fn real_pty_native_accepted_key_routes_survive_local_ownership() {
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
fn run_accepted_key_cases(native: &winit::event::KeyEvent) {
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

/// IME and clipboard gestures keep source-window ownership while AllTabs skips READONLY receivers and preserves writable peers.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_readonly_ime_paste_source_and_receiver_matrix() {
    use crate::app::{
        mod_tests::{input_test_windows, PtySubmissions},
        pty_test_support::{isolated, phase},
    };
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::{broadcast::BroadcastState, copy_mode::CopyModeState};
    use std::collections::BTreeSet;
    use winit::event::Ime;
    if isolated() {
        return;
    }
    for source_index in 0..3 {
        for source_readonly in [false, true] {
            for receiver_readonly in [false, true] {
                for ime in [false, true] {
                    let (mut app, windows) = input_test_windows();
                    let (source_window, source) = windows[source_index];
                    let (receiver_window, receiver) = windows[(source_index + 1) % 3];
                    let (_, peer) = windows[(source_index + 2) % 3];
                    app.frontmost_window = Some(receiver_window);
                    app.windows.get_mut(&source_window).unwrap().copy_mode =
                        source_readonly.then(|| CopyModeState::read_only_at((0, 0)));
                    app.windows.get_mut(&receiver_window).unwrap().copy_mode =
                        receiver_readonly.then(|| CopyModeState::read_only_at((0, 0)));
                    app.broadcast =
                        BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
                    app.__test_set_memory_clipboard("你好é");
                    app.wait_for_input_queues();
                    let submitted = PtySubmissions::start();
                    phase(source, if ime { "ime-commit" } else { "clipboard-paste" });
                    if ime {
                        app.handle_window_ime(source_window, Ime::Commit("你好é".into()));
                    } else {
                        assert!(
                            app.run_action_for_window(&Action::PasteFromClipboard, source_window)
                        );
                    }
                    let actual = submitted.take();
                    let expected = if source_readonly {
                        BTreeSet::new()
                    } else if receiver_readonly {
                        BTreeSet::from([source, peer])
                    } else {
                        BTreeSet::from([source, receiver, peer])
                    };
                    assert_eq!(actual.iter().map(|(pane, _)| *pane).collect::<BTreeSet<_>>(), expected, "source={source_index} source_readonly={source_readonly} receiver_readonly={receiver_readonly} ime={ime}");
                    assert_eq!(
                        actual.len(),
                        expected.len(),
                        "each destination receives exactly once"
                    );
                    assert!(actual.iter().all(|(_, bytes)| bytes == "你好é".as_bytes()));
                    assert_eq!(app.frontmost_window, Some(receiver_window));
                }
            }
        }
    }
}

#[test]
fn keyboard_owner_precedence_is_source_scoped_for_main_and_child() {
    // Composition and search precede READONLY in both window roles, without borrowing a sibling's owner.
    use super::WindowKeyOwner;
    use sonicterm_ui::{copy_mode::CopyModeState, search::SearchState};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    for (owner, other) in [(main, child), (child, main)] {
        app.frontmost_window = Some(other);
        assert_eq!(app.window_key_owner(owner), Some(WindowKeyOwner::Terminal));
        app.windows.get_mut(&owner).unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
        assert_eq!(app.window_key_owner(owner), Some(WindowKeyOwner::Copy));
        app.windows.get_mut(&owner).unwrap().tab_states[0].search = Some(SearchState::new());
        assert_eq!(app.window_key_owner(owner), Some(WindowKeyOwner::Search));
        app.windows.get_mut(&owner).unwrap().ime.handle_preedit("ni", None);
        assert_eq!(app.window_key_owner(owner), Some(WindowKeyOwner::Composition));
        app.run_action_for_window(&Action::OpenCommandPalette, owner);
        assert_eq!(app.window_key_owner(owner), Some(WindowKeyOwner::Palette));
        assert_eq!(app.window_key_owner(other), Some(WindowKeyOwner::Terminal));
        assert_eq!(app.frontmost_window, Some(other));
        app.command_palette.close();
        let window = app.windows.get_mut(&owner).unwrap();
        window.ime.cancel();
        window.tab_states[0].search = None;
        window.copy_mode = None;
    }
    assert_eq!(app.window_key_owner(winit::window::WindowId::from(0)), None);
}

#[test]
fn source_focus_cleanup_preserves_peer_state_and_report_destinations() {
    // Blur releases the latched pointer pane before reporting focus on the active pane and leaves peer composition intact.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main-pointer");
    app.__test_seed_tab("main-active");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child-pointer", "child-active"]);
    for (owner, other) in [(main, child), (child, main)] {
        let active = app.windows[&owner].tab_states[1].active_pane;
        let pointer_pane = app.windows[&owner].tab_states[0].active_pane;
        let window = app.windows.get_mut(&owner).unwrap();
        window.tabs.activate(1);
        window.modifiers = ModifiersState::ALT;
        window.ime.handle_preedit("source", None);
        window.mouse_down = true;
        window.test_renderer_focus_marker = Some(true);
        window.pointer_gesture = begin_pointer_gesture(
            pointer_cell(pointer_pane, 2, 3),
            MouseTracking::ButtonMotion,
            true,
            ModifiersState::empty(),
            false,
        );
        window.panes[&active].parser.lock().advance(b"\x1b[?1004h");
        app.windows.get_mut(&other).unwrap().ime.handle_preedit("peer", None);
        app.frontmost_window = Some(other);
        app.__test_enable_pty_write_log();

        app.handle_window_focus_changed(owner, false);

        assert_eq!(
            app.__test_drain_pty_writes(),
            vec![(pointer_pane, b"\x1b[<8;4;3m".to_vec()), (active, b"\x1b[O".to_vec())]
        );
        let window = &app.windows[&owner];
        assert!(!window.ime.is_composing());
        assert!(window.pointer_gesture.is_none());
        assert!(!window.mouse_down);
        assert_eq!(window.test_renderer_focus_marker, Some(false));
        assert_eq!(window.modifiers, ModifiersState::ALT);
        assert!(app.windows[&other].ime.is_composing());
        assert_eq!(app.frontmost_window, Some(other));

        app.handle_window_focus_changed(owner, true);
        assert_eq!(app.__test_drain_pty_writes(), vec![(active, b"\x1b[I".to_vec())]);
        assert_eq!(app.frontmost_window, Some(owner));
        assert_eq!(app.windows[&owner].test_renderer_focus_marker, Some(true));
        app.windows.get_mut(&other).unwrap().ime.cancel();
    }
}

#[test]
fn source_focus_keeps_main_only_compatibility_observations() {
    // Shared GUI cleanup must not turn the compatibility reducer into a second live child-focus owner.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    app.handle_window_focus_changed(main, true);
    assert_eq!(app.machine.state().focused_window, Some(sonicterm_types::WindowKey::new(0)));
    app.handle_window_focus_changed(child, true);
    assert_eq!(app.machine.state().focused_window, Some(sonicterm_types::WindowKey::new(0)));
    assert_eq!(app.frontmost_window, Some(child));
    app.handle_window_focus_changed(main, false);
    assert_eq!(app.machine.state().focused_window, None);
    assert_eq!(app.frontmost_window, Some(child));
}

#[test]
fn unknown_focus_and_modifiers_never_mutate_main_or_emit_input() {
    // Late events for a removed window cannot cancel main composition, alter modifiers, or write a focus report.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let removed = app.__test_seed_child_window(&["removed"]);
    app.windows.remove(&removed);
    app.frontmost_window = Some(main);
    app.windows.get_mut(&main).unwrap().ime.handle_preedit("keep", None);
    app.__test_enable_pty_write_log();
    app.handle_window_focus_changed(removed, false);
    app.handle_window_focus_changed(removed, true);
    app.handle_window_modifiers_changed(removed, ModifiersState::SUPER);
    assert_eq!(app.window_key_owner(removed), None);
    assert_eq!(app.windows[&main].ime.preedit(), "keep");
    assert_eq!(app.windows[&main].modifiers, ModifiersState::empty());
    assert_eq!(app.frontmost_window, Some(main));
    assert_eq!(app.machine.state().focused_window, None);
    assert!(app.__test_drain_pty_writes().is_empty());
}

#[test]
fn source_modifiers_do_not_follow_frontmost() {
    // Modifier updates belong only to their originating window, including when focus bookkeeping points elsewhere.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    app.frontmost_window = Some(child);
    app.handle_window_modifiers_changed(main, ModifiersState::SUPER);
    assert_eq!(app.windows[&main].modifiers, ModifiersState::SUPER);
    assert_eq!(app.windows[&child].modifiers, ModifiersState::empty());
    app.frontmost_window = Some(main);
    app.handle_window_modifiers_changed(child, ModifiersState::ALT);
    assert_eq!(app.windows[&main].modifiers, ModifiersState::SUPER);
    assert_eq!(app.windows[&child].modifiers, ModifiersState::ALT);
    assert_eq!(app.frontmost_window, Some(main));
}

#[test]
fn copy_navigation_keeps_unicode_and_missing_source_state() {
    // Shared copy navigation preserves Unicode extraction and restores its state when a tab or pane is temporarily absent.
    use sonicterm_ui::copy_mode::CopyModeState;
    use winit::keyboard::{Key, NamedKey};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    for (owner, other) in [(main, child), (child, main)] {
        app.__test_set_memory_clipboard("unchanged");
        let pane_id = app.windows[&owner].tab_states[0].active_pane;
        app.windows[&owner].panes[&pane_id].parser.lock().advance("é你".as_bytes());
        let mut copy = CopyModeState::new_at((0, 0));
        copy.start_select();
        copy.cursor = (2, 0);
        app.windows.get_mut(&owner).unwrap().copy_mode = Some(copy.clone());
        app.frontmost_window = Some(other);
        app.handle_window_copy_key(owner, &Key::Named(NamedKey::Enter));
        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("é你"));
        assert!(app.windows[&owner].copy_mode.is_none());
        assert_eq!(app.frontmost_window, Some(other));

        app.windows.get_mut(&owner).unwrap().copy_mode = Some(copy.clone());
        let tabs = std::mem::take(&mut app.windows.get_mut(&owner).unwrap().tab_states);
        app.handle_window_copy_key(owner, &Key::Named(NamedKey::ArrowLeft));
        assert_eq!(app.windows[&owner].copy_mode.as_ref(), Some(&copy));
        app.windows.get_mut(&owner).unwrap().tab_states = tabs;
        let pane = app.windows.get_mut(&owner).unwrap().panes.remove(&pane_id).unwrap();
        app.handle_window_copy_key(owner, &Key::Named(NamedKey::ArrowLeft));
        assert_eq!(app.windows[&owner].copy_mode.as_ref(), Some(&copy));
        app.windows.get_mut(&owner).unwrap().panes.insert(pane_id, pane);
    }
}

#[test]
fn copy_quick_select_owns_hints_before_safe_bindings() {
    // A quick-select label remains local even when the same key is bound to a READONLY-safe action.
    use sonicterm_ui::copy_mode::{CopyModeState, QuickSelectHint, QuickSelectState};
    use winit::keyboard::Key;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    app.keymap.bindings.push(sonicterm_cfg::keymap::Binding {
        keys: "f".into(),
        action: sonicterm_cfg::keymap::ActionWrapper(Action::OpenSearch),
    });
    for owner in [main, child] {
        app.__test_set_memory_clipboard("unchanged");
        let mut copy = CopyModeState::new_at((0, 0));
        copy.quick_select = Some(QuickSelectState {
            hints: vec![QuickSelectHint {
                hint: 'f',
                row: 0,
                col_start: 0,
                col_end: 3,
                text: "hint".into(),
            }],
        });
        app.windows.get_mut(&owner).unwrap().copy_mode = Some(copy);
        assert!(!app.copy_mode_allows_keymap(owner));
        app.handle_window_copy_key(owner, &Key::Character("f".into()));
        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("hint"));
        assert!(app.windows[&owner].copy_mode.is_none());
        app.windows.get_mut(&owner).unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
        assert!(app.copy_mode_allows_keymap(owner));
        app.windows.get_mut(&owner).unwrap().copy_mode = Some(CopyModeState::new_at((0, 0)));
        assert!(app.copy_mode_allows_keymap(owner));
    }
}

#[test]
fn window_ime_commit_keeps_its_source_through_focus_changes() {
    // Main, child, and sibling composition must never follow the frontmost window or leak preedit bytes.
    use winit::event::Ime;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    let sibling = app.__test_seed_child_window(&["sibling"]);
    for (owner, other) in [(main, child), (child, main), (sibling, child)] {
        let pane = app.windows[&owner].tab_states[0].active_pane;
        app.frontmost_window = Some(other);
        app.__test_enable_pty_write_log();
        app.handle_window_ime(owner, Ime::Enabled);
        app.handle_window_ime(owner, Ime::Preedit("ni".into(), Some((0, 2))));
        assert_eq!(app.windows[&owner].ime.preedit(), "ni");
        assert_eq!(app.windows[&owner].ime.cursor(), Some((0, 2)));
        assert!(app.windows[&owner].ime.is_composing());
        assert!(!app.windows[&other].ime.is_composing());
        assert!(app.__test_drain_pty_writes().is_empty());
        app.handle_window_ime(owner, Ime::Commit("你好é".into()));
        assert_eq!(app.__test_drain_pty_writes(), vec![(pane, "你好é".as_bytes().to_vec())]);
        assert!(!app.windows[&owner].ime.is_composing());
        assert!(app.windows.get_mut(&owner).unwrap().ime.take_commits().is_empty());
        app.handle_window_ime(owner, Ime::Commit(String::new()));
        app.handle_window_ime(owner, Ime::Preedit("discard".into(), None));
        app.handle_window_ime(owner, Ime::Disabled);
        assert!(app.windows[&owner].ime.preedit().is_empty());
        assert!(!app.windows[&owner].ime.is_composing());
        assert!(app.__test_drain_pty_writes().is_empty());
        assert_eq!(app.frontmost_window, Some(other));
    }
}

#[test]
fn window_ime_ignores_other_window_search_and_readonly_state() {
    // A sibling's input owner cannot swallow a terminal commit in either routing direction.
    use sonicterm_ui::{copy_mode::CopyModeState, search::SearchState};
    use winit::event::Ime;
    for owner_is_child in [false, true] {
        for other_has_search in [false, true] {
            let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
            app.__test_seed_tab("main");
            let main = app.main_window_id.unwrap();
            let child = app.__test_seed_child_window(&["child"]);
            let (owner, other) = if owner_is_child { (child, main) } else { (main, child) };
            let pane = app.windows[&owner].tab_states[0].active_pane;
            let other_window = app.windows.get_mut(&other).unwrap();
            if other_has_search {
                other_window.tab_states[0].search = Some(SearchState::new());
            } else {
                other_window.copy_mode = Some(CopyModeState::read_only_at((0, 0)));
            }
            app.frontmost_window = Some(other);
            app.__test_enable_pty_write_log();
            app.handle_window_ime(owner, Ime::Commit("source".into()));
            assert_eq!(app.__test_drain_pty_writes(), vec![(pane, b"source".to_vec())]);
            assert_eq!(app.frontmost_window, Some(other));
            if other_has_search {
                assert!(app.windows[&other].tab_states[0]
                    .search
                    .as_ref()
                    .unwrap()
                    .query
                    .is_empty());
            }
        }
    }
}

#[test]
fn window_ime_overlay_owners_take_precedence_over_readonly_and_broadcast() {
    // Palette precedes search; search precedes READONLY; none of these commits may reach any PTY.
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::{broadcast::BroadcastState, copy_mode::CopyModeState, search::SearchState};
    use winit::event::Ime;
    for owner_is_child in [false, true] {
        for owner_kind in ["palette", "search", "readonly"] {
            let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
            app.__test_seed_tab("main");
            let main = app.main_window_id.unwrap();
            let child = app.__test_seed_child_window(&["child"]);
            let (owner, other) = if owner_is_child { (child, main) } else { (main, child) };
            let pane = app.windows[&owner].tab_states[0].active_pane;
            app.broadcast =
                BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: pane };
            app.windows.get_mut(&owner).unwrap().copy_mode =
                Some(CopyModeState::read_only_at((0, 0)));
            if owner_kind != "readonly" {
                app.windows.get_mut(&owner).unwrap().tab_states[0].search =
                    Some(SearchState::new());
            }
            if owner_kind == "palette" {
                app.run_action_for_window(&Action::OpenCommandPalette, owner);
            }
            app.frontmost_window = Some(other);
            app.__test_enable_pty_write_log();
            app.handle_window_ime(owner, Ime::Preedit("ni".into(), Some((0, 2))));
            app.handle_window_ime(owner, Ime::Commit("你好".into()));
            assert!(app.__test_drain_pty_writes().is_empty(), "{owner_kind}");
            assert!(app.windows.get_mut(&owner).unwrap().ime.take_commits().is_empty());
            assert!(!app.windows[&owner].ime.is_composing());
            assert!(app.windows[&owner].copy_mode.is_some());
            match owner_kind {
                "palette" => {
                    assert_eq!(app.command_palette.query(), "你好");
                    assert!(app.windows[&owner].tab_states[0]
                        .search
                        .as_ref()
                        .unwrap()
                        .query
                        .is_empty());
                }
                "search" => {
                    assert_eq!(
                        app.windows[&owner].tab_states[0].search.as_ref().unwrap().query,
                        "你好"
                    );
                }
                _ => assert!(app.windows[&owner].tab_states[0].search.is_none()),
            }
            app.command_palette.close();
            app.windows.get_mut(&owner).unwrap().tab_states[0].search = None;
            app.windows.get_mut(&owner).unwrap().copy_mode = None;
            app.broadcast = BroadcastState::Off;
            app.handle_window_ime(owner, Ime::Commit("next".into()));
            assert_eq!(app.__test_drain_pty_writes(), vec![(pane, b"next".to_vec())]);
        }
    }
}

#[test]
fn window_ime_broadcasts_only_from_its_recorded_source_once() {
    // IME fan-out retains source identity, excludes a duplicate source write, and ignores unrelated focus.
    use sonicterm_cfg::keymap::BroadcastScope;
    use sonicterm_ui::broadcast::BroadcastState;
    use winit::event::Ime;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main_pane = app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    let child_pane = app.windows[&child].tab_states[0].active_pane;
    let sibling = app.__test_seed_child_window(&["sibling"]);
    let sibling_pane = app.windows[&sibling].tab_states[0].active_pane;
    for (owner, pane, other) in [(main, main_pane, child), (child, child_pane, main)] {
        app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: pane };
        app.frontmost_window = Some(other);
        app.__test_enable_pty_write_log();
        app.handle_window_ime(owner, Ime::Commit("é".into()));
        let mut writes = app.__test_drain_pty_writes();
        writes.sort_by_key(|(id, _)| *id);
        let mut expected: Vec<_> = [main_pane, child_pane, sibling_pane]
            .into_iter()
            .map(|id| (id, "é".as_bytes().to_vec()))
            .collect();
        expected.sort_by_key(|(id, _)| *id);
        assert_eq!(writes, expected);
        let other_pane = app.windows[&other].tab_states[0].active_pane;
        app.handle_window_ime(other, Ime::Commit("local".into()));
        assert_eq!(app.__test_drain_pty_writes(), vec![(other_pane, b"local".to_vec())]);
    }
}

#[test]
fn window_ime_missing_owner_or_search_pane_never_falls_back() {
    // Stale window events do nothing, while an open search retains ownership even after its pane disappears.
    use sonicterm_ui::search::SearchState;
    use winit::{event::Ime, window::WindowId};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let removed = app.__test_seed_child_window(&["removed"]);
    app.windows.remove(&removed);
    app.run_action_for_window(&Action::OpenCommandPalette, main);
    app.__test_enable_pty_write_log();
    for target in [removed, WindowId::from(0)] {
        app.handle_window_ime(target, Ime::Preedit("ignored".into(), None));
        app.handle_window_ime(target, Ime::Commit("ignored".into()));
    }
    assert!(app.command_palette.query().is_empty());
    assert!(app.windows[&main].ime.preedit().is_empty());
    assert!(app.__test_drain_pty_writes().is_empty());
    app.command_palette.close();
    let child = app.__test_seed_child_window(&["child"]);
    for owner in [main, child] {
        let window = app.windows.get_mut(&owner).unwrap();
        window.tab_states[0].search = Some(SearchState::new());
        let pane_id = window.tab_states[0].active_pane;
        let pane = window.panes.remove(&pane_id).unwrap();
        app.handle_window_ime(owner, Ime::Commit("retain".into()));
        assert!(app.windows[&owner].tab_states[0].search.as_ref().unwrap().query.is_empty());
        assert!(app.windows.get_mut(&owner).unwrap().ime.take_commits().is_empty());
        assert!(app.__test_drain_pty_writes().is_empty());
        app.windows.get_mut(&owner).unwrap().panes.insert(pane_id, pane);
        app.handle_window_ime(owner, Ime::Commit("next".into()));
        assert_eq!(app.windows[&owner].tab_states[0].search.as_ref().unwrap().query, "next");
        assert!(app.__test_drain_pty_writes().is_empty());
    }
}
