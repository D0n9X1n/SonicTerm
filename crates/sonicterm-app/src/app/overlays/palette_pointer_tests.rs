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
    let wheel = |vertical_lines| WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(0.0, vertical_lines),
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

/// A click on a checklist row toggles it on release over the same row and keeps the page open;
/// a release over another row toggles nothing.
#[test]
fn palette_pointer_click_toggles_a_checklist_row_and_keeps_it_open() {
    use sonicterm_cfg::status_line::StatusSegmentId as Seg;
    use sonicterm_ui::command_palette::StatusSegmentRowKind as Kind;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["work"]);
    app.open_status_segments(Some(child), Some(Seg::Cpu));
    let index = app.command_palette.selected();
    let capture = |kind| super::PalettePointerCapture {
        window_id: child,
        target: super::PalettePointerTarget::StatusRow(kind),
    };
    app.palette_pointer_capture = Some(capture(Kind::Segment(Seg::Cpu)));
    let other = super::PalettePointerHit::StatusRow { index: 0, kind: Kind::ShowStrip };
    app.release_command_palette_pointer(child, Some(other));
    assert!(app.current_status_line().segment(Seg::Cpu).is_none(), "released elsewhere");
    app.palette_pointer_capture = Some(capture(Kind::Segment(Seg::Cpu)));
    let same = super::PalettePointerHit::StatusRow { index, kind: Kind::Segment(Seg::Cpu) };
    app.release_command_palette_pointer(child, Some(same));
    assert!(app.current_status_line().segment(Seg::Cpu).is_some(), "the click toggled it");
    assert!(app.command_palette.is_open());
    assert!(app.palette_pointer_capture.is_none());
}

/// A checklist change from a click or a key, including turning the strip on or off, asks its
/// window for exactly one frame and no other window for any, whichever steps ask on the way.
#[test]
fn a_checklist_click_or_key_requests_one_redraw_per_terminal_window() {
    use crate::app::window_state::window_redraw_requests_for as asks;
    use sonicterm_cfg::status_line::StatusSegmentId as Seg;
    use sonicterm_ui::command_palette::StatusSegmentRowKind as Kind;
    let mut config = Config::default();
    config.status_line.enabled = false;
    let mut app = App::new(Theme::default(), config, Keymap::default());
    let child = app.__test_seed_child_window(&["work"]);
    let main = app.main_window_id.expect("the seed made a main window");
    app.open_status_segments(Some(child), Some(Seg::Cpu));
    let strip_row = app
        .command_palette
        .status_segment_rows()
        .iter()
        .position(|row| row.kind == Kind::ShowStrip)
        .expect("the checklist lists the strip row");
    let counts = |app: &App| (asks(&app.windows[&main]), asks(&app.windows[&child]));
    // One key dispatch: the palette's key handler, then the KeyboardInput arm's own request.
    let key = |app: &mut App| {
        let _dispatch = app.frame_dispatch_scope();
        app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
        app.finish_window_key(child);
    };
    // One click dispatch: the release over the row the press captured.
    let click = |app: &mut App, kind| {
        let _dispatch = app.frame_dispatch_scope();
        let index = app.command_palette.selected();
        app.palette_pointer_capture = Some(super::PalettePointerCapture {
            window_id: child,
            target: super::PalettePointerTarget::StatusRow(kind),
        });
        let hit = super::PalettePointerHit::StatusRow { index, kind };
        app.release_command_palette_pointer(child, Some(hit));
    };
    let expect_one = |app: &App, trigger: &str, (main_before, child_before): (u64, u64)| {
        let (main_after, child_after) = counts(app);
        assert_eq!(child_after - child_before, 1, "{trigger}: the checklist's window");
        assert_eq!(main_after - main_before, 0, "{trigger}: the other window");
    };
    let before = counts(&app);
    key(&mut app);
    expect_one(&app, "key toggling cpu", before);
    assert!(app.current_status_line().segment(Seg::Cpu).is_some());
    let before = counts(&app);
    click(&mut app, Kind::Segment(Seg::Cpu));
    expect_one(&app, "click toggling cpu", before);
    assert!(app.current_status_line().segment(Seg::Cpu).is_none());
    assert!(app.command_palette.select_status_row(strip_row));
    let before = counts(&app);
    key(&mut app);
    expect_one(&app, "key turning the strip on", before);
    assert!(app.current_status_line().enabled);
    let before = counts(&app);
    click(&mut app, Kind::ShowStrip);
    expect_one(&app, "click turning the strip off", before);
    assert!(!app.current_status_line().enabled);
}
