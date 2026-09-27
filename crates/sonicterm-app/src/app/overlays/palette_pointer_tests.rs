use super::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use winit::keyboard::{Key, NamedKey};

fn pointer_target(app: &App, index: usize) -> super::PalettePointerHit {
    super::PalettePointerHit::Row { index, entry: app.command_palette.visible()[index].clone() }
}

fn press_palette_target(app: &mut App, window_id: winit::window::WindowId, index: usize) {
    let entry = app.command_palette.visible()[index].clone();
    assert!(app.command_palette.select_visible_index(index));
    app.palette_pointer_capture = Some(super::PalettePointerCapture {
        window_id,
        target: super::PalettePointerTarget::Entry(entry),
    });
}

/// Release resolves the pressed child target by ID even after a reorder without an intervening redraw.
#[test]
fn palette_pointer_release_revalidates_attached_tab_identity() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["same", "same", "peer"]);
    let target = app.windows[&child].tabs.tabs()[0].id;
    app.open_tab_selector(child);
    press_palette_target(&mut app, child, 0);
    let release = pointer_target(&app, 0);
    let window = app.windows.get_mut(&child).unwrap();
    window.tabs.reorder(0, 1);
    window.tab_states.swap(0, 1);
    window.tabs.activate(2);
    app.__test_set_frontmost_window(app.main_window_id);
    app.release_command_palette_pointer(child, Some(release));
    assert_eq!(app.windows[&child].tabs.active().unwrap().id, target);
    assert!(!app.command_palette.is_open());
    assert!(app.palette_pointer_capture.is_none());
}

/// A redraw that moves another row under the held pointer cancels activation instead of retargeting it.
#[test]
fn palette_pointer_release_rejects_a_different_displayed_row() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("first");
    app.__test_seed_tab("second");
    app.__test_seed_tab("active");
    let window_id = app.main_window_id.unwrap();
    let active = app.main_tabs().unwrap().active().unwrap().id;
    app.open_tab_selector(window_id);
    press_palette_target(&mut app, window_id, 0);
    let window = app.windows.get_mut(&window_id).unwrap();
    window.tabs.reorder(0, 1);
    window.tab_states.swap(0, 1);
    app.refresh_command_palette_context();
    let release = pointer_target(&app, 0);
    app.release_command_palette_pointer(window_id, Some(release));
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, active);
    assert!(app.command_palette.is_open());
}

/// Closed targets cannot lend their pointer capture to a same-title replacement at the old slot.
#[test]
fn palette_pointer_release_rejects_a_closed_target() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("same");
    app.__test_seed_tab("active");
    let window_id = app.main_window_id.unwrap();
    let target = app.main_tabs().unwrap().tabs()[0].id;
    let active = app.main_tabs().unwrap().active().unwrap().id;
    app.open_tab_selector(window_id);
    press_palette_target(&mut app, window_id, 0);
    let release = pointer_target(&app, 0);
    let window = app.windows.get_mut(&window_id).unwrap();
    window.tabs.close(target);
    window.tab_states.remove(0);
    app.__test_seed_tab("same");
    let window = app.windows.get_mut(&window_id).unwrap();
    window.tabs.activate(0);
    app.release_command_palette_pointer(window_id, Some(release));
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, active);
    assert!(app.command_palette.is_open());
    assert!(app.command_palette.current().is_none());
}

/// Disabled clicks keep the modal open, while outside dismissal consumes its own paired release.
#[test]
fn palette_pointer_disabled_and_outside_clicks_never_dispatch() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let window_id = app.main_window_id.unwrap();
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_palette_query("Copy to Clipboard");
    app.__test_set_memory_clipboard("unchanged");
    press_palette_target(&mut app, window_id, 0);
    let release = pointer_target(&app, 0);
    app.release_command_palette_pointer(window_id, Some(release));
    assert!(app.command_palette.is_open());
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("unchanged"));
    app.palette_pointer_capture = Some(super::PalettePointerCapture {
        window_id,
        target: super::PalettePointerTarget::Outside,
    });
    app.release_command_palette_pointer(window_id, Some(super::PalettePointerHit::Outside));
    assert!(!app.command_palette.is_open());
    assert!(app.__test_pty_write_log().is_empty());
}

/// Keyboard and IME edits revoke a held row so its later release cannot run a newly selected entry.
#[test]
fn palette_pointer_capture_is_cleared_by_keyboard_ime_and_reopen() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("first");
    app.__test_seed_tab("active");
    let window_id = app.main_window_id.unwrap();
    for edit in 0..3 {
        app.open_tab_selector(window_id);
        press_palette_target(&mut app, window_id, 0);
        match edit {
            0 => {
                app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowDown));
            }
            1 => {
                app.command_palette_handle_ime(&winit::event::Ime::Preedit(
                    "composition".into(),
                    None,
                ));
            }
            _ => {
                app.open_tab_selector(window_id);
            }
        }
        assert!(app.palette_pointer_capture.is_none());
        app.main_mut().unwrap().ime.cancel();
    }
}

/// Wheel input stays bounded at the list ends and never reaches a PTY or a noncommand picker.
#[test]
fn palette_pointer_wheel_is_bounded_and_modal() {
    use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("first");
    app.__test_seed_tab("second");
    app.__test_seed_tab("last");
    let window_id = app.main_window_id.unwrap();
    app.open_tab_selector(window_id);
    let wheel = |y| WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(0.0, y),
        phase: TouchPhase::Moved,
    };
    assert!(app.command_palette_handle_pointer_event(window_id, &wheel(f32::MAX)));
    assert_eq!(app.command_palette.selected(), 0);
    assert!(app.command_palette_handle_pointer_event(window_id, &wheel(-f32::MAX)));
    assert_eq!(app.command_palette.selected(), 1);
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0] {
        assert!(app.command_palette_handle_pointer_event(window_id, &wheel(value)));
        assert_eq!(app.command_palette.selected(), 1);
    }
    for _ in 0..4 {
        app.command_palette_handle_pointer_event(window_id, &wheel(-1.0));
    }
    assert_eq!(app.command_palette.selected(), 2);
    app.command_palette.start_rename_tab("literal");
    assert!(app.command_palette_handle_pointer_event(window_id, &wheel(-1.0)));
    assert_eq!(app.command_palette.query(), "literal");
    assert!(app.__test_pty_write_log().is_empty());
}

/// Closing the host or changing attachment cannot send a captured click into another window.
#[test]
fn palette_pointer_capture_does_not_cross_window_ownership() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("same");
    let main_id = app.main_window_id.unwrap();
    let main_tab = app.main_tabs().unwrap().active().unwrap().id;
    let child = app.__test_seed_child_window(&["same", "active"]);
    app.open_tab_selector(child);
    press_palette_target(&mut app, child, 0);
    let release = pointer_target(&app, 0);
    app.__test_remove_window(child);
    app.release_command_palette_pointer(child, Some(release));
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, main_tab);
    assert!(app.command_palette.is_open());
    app.open_tab_selector(main_id);
    press_palette_target(&mut app, main_id, 0);
    let release = pointer_target(&app, 0);
    app.release_command_palette_pointer(child, Some(release));
    assert!(app.command_palette.is_open());
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, main_tab);
}
