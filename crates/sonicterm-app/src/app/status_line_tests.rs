use super::*;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

/// A headless App, which has no windows, with `enabled` as the configured strip state.
fn app_with_strip(enabled: bool) -> App {
    let mut config = Config::default();
    config.status_line.enabled = enabled;
    App::new(Theme::default(), config, Keymap::default())
}

#[test]
fn reload_failure_keeps_the_parse_error_line_only_when_named() {
    // `Config: ✗ line 12` needs the line from the TOML error; an error without one shows no line.
    let with_line = anyhow::anyhow!("TOML parse error at line 12, column 3");
    assert_eq!(reload_failure(&with_line), ReloadFailure { line: Some(12) });
    let nested = anyhow::anyhow!("expected a table").context("parse \"sonicterm.toml\"");
    assert_eq!(reload_failure(&nested), ReloadFailure { line: None });
    let wording = anyhow::anyhow!("pipeline failed");
    assert_eq!(
        reload_failure(&wording),
        ReloadFailure { line: None },
        "`line` inside a word is not a line"
    );
}

#[test]
fn reload_overrides_a_session_toggle_only_when_the_file_value_changed() {
    // A toggle survives unrelated reloads; changing `enabled` or `hide_sensitive` in the file wins.
    let mut app = app_with_strip(false);
    let now = Instant::now();
    let _ = app.sync_status_line(now);
    assert!(!app.status_line.visible);
    app.toggle_status_line();
    assert!(app.status_line.visible);
    let mut unrelated = app.config.clone();
    unrelated.status_line.update_interval_ms = 5000;
    app.config = unrelated;
    let _ = app.sync_status_line(now);
    assert!(app.status_line.visible, "an unrelated status line key keeps the toggle");
    app.config.status_line.enabled = true;
    let _ = app.sync_status_line(now);
    assert!(app.status_line.visible);
    app.toggle_status_line();
    assert!(!app.status_line.visible);
    app.config.status_line.update_interval_ms = 6000;
    let _ = app.sync_status_line(now);
    assert!(!app.status_line.visible, "enabled is unchanged in the file, so the toggle stands");
    app.toggle_status_line_sensitive();
    assert!(app.status_line.hide_sensitive);
    app.config.status_line.hide_sensitive = true;
    let _ = app.sync_status_line(now);
    assert!(app.status_line.hide_sensitive);
    app.config.status_line.hide_sensitive = false;
    let _ = app.sync_status_line(now);
    assert!(!app.status_line.hide_sensitive, "the file's changed value wins");
}

#[test]
fn nothing_is_sampled_or_due_without_an_eligible_window() {
    // With the strip on but no terminal window, memory is never sampled and no wake is armed.
    let mut app = app_with_strip(true);
    let now = Instant::now();
    assert_eq!(app.sync_status_line(now), None);
    assert!(app.status_line.visible);
    assert_eq!(app.status_line.mem, MemInput::default());
    assert_eq!(app.status_line.next_sample, None);
    let mut off = app_with_strip(false);
    assert_eq!(off.sync_status_line(now), None);
}

#[test]
fn reload_errors_are_kept_per_component() {
    // A theme success never clears a keymap failure; only the keymap's own success does.
    let mut app = app_with_strip(true);
    let error = anyhow::anyhow!("bad value at line 4");
    app.set_keymap_reload_error(Some(&error));
    app.set_theme_reload_error(Some(&error));
    app.set_theme_reload_error(None);
    assert_eq!(app.status_line.theme_error, None);
    assert_eq!(app.status_line.keymap_error, Some(ReloadFailure { line: Some(4) }));
    app.set_config_reload_error(Some(&error));
    app.set_keymap_reload_error(None);
    assert_eq!(app.status_line.keymap_error, None);
    assert_eq!(app.status_line.config_error, Some(ReloadFailure { line: Some(4) }));
}

#[test]
fn pane_facts_are_kept_only_while_the_strip_is_shown() {
    // A hidden strip stores no per-window facts, so it retains nothing per window.
    let mut app = app_with_strip(false);
    let _ = app.sync_status_line(Instant::now());
    let window_id = WindowId::dummy();
    app.observe_status_pane(window_id, StatusPaneFacts { mouse_captured: true, scroll_lines: 3 });
    assert!(app.status_line.pane_facts.is_empty());
    app.toggle_status_line();
    app.observe_status_pane(window_id, StatusPaneFacts { mouse_captured: true, scroll_lines: 3 });
    assert_eq!(app.status_line.pane_facts.len(), 1);
    let _ = app.sync_status_line(Instant::now());
    assert!(app.status_line.pane_facts.is_empty(), "facts of a closed window are dropped");
}

#[test]
fn a_hover_an_event_clears_keeps_the_link_until_a_frame_resolves_it() {
    // An IME commit clears the hover and the next frame re-detects the same URL; the strip must
    // keep the link across that gap, or each keystroke redraws it twice for an unchanged pixel.
    use sonicterm_render_model::inputs::{HoveredUrlCells, HoveredUrlSpan};
    let mut app = app_with_strip(true);
    let window_id = app.__test_seed_child_window(&["child"]);
    let span = HoveredUrlSpan { row: 0, start_col: 0, end_col: 19 };
    let cells = HoveredUrlCells::new(1, [span], false).expect("one span");
    app.windows.get_mut(&window_id).unwrap().hovered_url =
        Some(super::super::hovered_url::HoveredUrl {
            cells,
            url: "https://example.com".to_owned(),
            label: None,
        });
    let now = Instant::now();
    let mut pending = HashMap::new();
    let (shown, _) =
        strip_link(&app.windows[&window_id], window_id, None, &mut pending, now, &app.i18n);
    assert!(shown.is_some(), "a resolved hover shows its link");
    app.windows.get_mut(&window_id).unwrap().invalidate_path_hover();
    assert!(app.windows[&window_id].hover_unresolved);
    let (held, wake) =
        strip_link(&app.windows[&window_id], window_id, Some(&shown), &mut pending, now, &app.i18n);
    assert_eq!(held, shown, "a cleared hover the next frame re-detects keeps the strip unchanged");
    assert_eq!(wake, None);
    // The headless window has no grid under the pointer, so this lookup resolves to no target.
    app.refresh_target_hover(window_id);
    assert!(!app.windows[&window_id].hover_unresolved);
    let (resolved, _) =
        strip_link(&app.windows[&window_id], window_id, Some(&shown), &mut pending, now, &app.i18n);
    assert_eq!(resolved, None, "a frame that finds no target hides the link");
}

#[test]
fn home_paths_display_with_a_tilde() {
    // Paths under the home directory read `~/…`; others are shown as they are.
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        // When: the test host has no home variable, there is nothing to substitute.
        return;
    };
    let home = std::path::PathBuf::from(home);
    assert_eq!(display_path(&home), "~");
    let shown = display_path(&home.join("proj").join("src"));
    assert_eq!(shown, format!("~{0}proj{0}src", std::path::MAIN_SEPARATOR));
    let outside = std::path::Path::new("/definitely-not-home/x");
    assert_eq!(display_path(outside), "/definitely-not-home/x");
}

/// A config that failed to load at startup shows in `health` the same way a failed reload does,
/// and the check runs once per process.
#[test]
fn startup_load_errors_are_recorded_once_per_component() {
    let dir = std::env::temp_dir().join(format!("sonicterm-status-startup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "[status_line]\nupdate_interval_ms = 1\n").unwrap();
    let mut app = app_with_strip(true);
    app.runtime_config_path = Some(path.clone());
    app.record_startup_load_errors();
    assert!(app.status_line.config_error.is_some(), "an invalid table is a startup config error");
    assert_eq!(app.status_line.theme_error, None, "the bundled theme loads");
    std::fs::write(&path, "").unwrap();
    app.record_startup_load_errors();
    assert!(app.status_line.config_error.is_some(), "the check runs only once");
    let _ = std::fs::remove_dir_all(dir);
}

/// A strip band over raster rows 0..30 holding one inert segment and one that scrolls to the bottom.
fn strip_over_top() -> sonicterm_gpu::status_strip::StripHit {
    use sonicterm_gpu::status_strip::PresentedStatusSegment;
    sonicterm_gpu::status_strip::StripHit {
        top: 0.0,
        height: 30.0,
        segments: vec![
            PresentedStatusSegment { id: StatusSegmentId::Mem, x: 0.0, width: 100.0, click: None },
            PresentedStatusSegment {
                id: StatusSegmentId::Scroll,
                x: 200.0,
                width: 100.0,
                click: Some(SegmentClick::Handler(SegmentHandler::ScrollToBottom)),
            },
        ],
    }
}

/// Any-motion SGR mouse reporting, so a wheel that reached the pane would write bytes to its PTY.
fn publish_mouse_reporting(pane: &crate::app::PaneState) {
    use sonicterm_vt::vt::{MouseTracking, PointerModes};
    let modes = PointerModes::new(MouseTracking::AnyMotion, true, false, false);
    // Ordering: pointer_input is read only by this thread's handlers here.
    pane.pointer_input.store(modes.bits(), std::sync::atomic::Ordering::Relaxed);
}

/// A press and a wheel inside the main window's strip never reach the pane: no mouse report, no
/// selection or press state, no scroll; outside the strip the same wheel reports to the program.
#[test]
fn main_window_strip_consumes_press_and_wheel() {
    use winit::event::MouseScrollDelta;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    let main_id = app.main_window_id.unwrap();
    app.test_viewport_override =
        Some((sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0));
    app.__test_enable_pty_write_log();
    publish_mouse_reporting(&app.main().unwrap().panes[&pane_id]);
    let window = app.main_mut().unwrap();
    window.cursor_pos = (25.0, 15.0);
    window.status_strip = Some(strip_over_top());
    assert!(app.handle_main_left_press_chrome(main_id), "a press in the strip is consumed");
    assert!(!app.__test_mouse_down(), "no press or selection gesture starts");
    app.handle_main_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));
    assert_eq!(app.__test_pty_write_log(), Vec::new(), "nothing reaches the PTY");
    app.main_mut().unwrap().status_strip = None;
    assert!(!app.handle_main_left_press_chrome(main_id), "without a strip the press routes on");
    app.handle_main_mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0));
    assert_eq!(app.__test_pty_write_log(), vec![(pane_id, b"\x1b[<64;3;2M".repeat(3))]);
}

/// The same holds in a torn-out window: its strip consumes the press, and a wheel over it
/// scrolls nothing, where the same wheel outside the strip scrolls the pane back into history.
/// (A torn-out window with no event proxy never writes wheel reports, so local scroll is the probe.)
#[test]
fn torn_out_window_strip_consumes_press_and_wheel() {
    use winit::event::MouseScrollDelta;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.__test_set_child_pane_viewport(
        child,
        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0,
    ));
    let pane_id = app.__test_child_active_pane(child).unwrap();
    let history = "line\r\n".repeat(300);
    assert!(app.__test_advance_child_pane_parser(child, pane_id, history.as_bytes()));
    app.__test_enable_pty_write_log();
    let window = app.windows.get_mut(&child).unwrap();
    window.cursor_pos = (25.0, 15.0);
    window.status_strip = Some(strip_over_top());
    assert!(app.handle_child_left_press_chrome(child), "a press in the strip is consumed");
    assert_eq!(app.__test_child_mouse_down(child), Some(false), "no press or selection starts");
    let scrollbar = app.config.appearance.scrollbar;
    let wheel = |app: &mut App| {
        App::handle_child_mouse_wheel(
            app.windows.get_mut(&child).unwrap(),
            MouseScrollDelta::LineDelta(0.0, 1.0),
            &None,
            scrollbar,
        )
    };
    wheel(&mut app);
    assert_eq!(
        app.windows[&child].panes[&pane_id].viewport_top_abs, None,
        "no scroll over the strip"
    );
    assert_eq!(app.__test_pty_write_log(), Vec::new(), "nothing reaches the PTY");
    app.windows.get_mut(&child).unwrap().status_strip = None;
    assert!(!app.handle_child_left_press_chrome(child), "without a strip the press routes on");
    wheel(&mut app);
    assert!(
        app.windows[&child].panes[&pane_id].viewport_top_abs.is_some(),
        "the same wheel scrolls back"
    );
}

/// A press on the scroll segment runs its default handler and returns the pane to the bottom.
#[test]
fn strip_scroll_segment_returns_the_pane_to_the_bottom() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    let main_id = app.main_window_id.unwrap();
    app.main_mut().unwrap().panes.get_mut(&pane_id).unwrap().pin_viewport_top(Some(0));
    assert_eq!(app.main().unwrap().panes[&pane_id].viewport_top_abs, Some(0));
    let window = app.main_mut().unwrap();
    window.cursor_pos = (250.0, 15.0);
    window.status_strip = Some(strip_over_top());
    assert!(app.handle_main_left_press_chrome(main_id));
    assert_eq!(
        app.main().unwrap().panes[&pane_id].viewport_top_abs,
        None,
        "following the live tail"
    );
}

/// Each presenter and adapter has its own health label: GDI only where the degrade policy runs on
/// Windows, a CPU adapter also when `software_render_mode = "off"` declines to degrade, forced
/// software on real hardware; and GPU recovery shows only for a window waiting on it.
#[test]
fn health_classifies_each_presenter_and_recovery_per_window() {
    use RenderHealth::{Hardware, SoftwareAdapter, SoftwareForced, SoftwareGdi};
    assert_eq!(classify_render(true, true, true), SoftwareGdi, "Windows CPU adapter, auto");
    assert_eq!(classify_render(true, false, true), SoftwareGdi, "Windows, forced");
    assert_eq!(
        classify_render(true, true, false),
        SoftwareAdapter,
        "Windows CPU adapter, mode off"
    );
    assert_eq!(classify_render(false, true, false), SoftwareAdapter, "CPU adapter, mode off");
    assert_eq!(
        classify_render(false, true, true),
        SoftwareAdapter,
        "CPU adapter, auto, off Windows"
    );
    assert_eq!(classify_render(false, false, true), SoftwareForced, "hardware, forced");
    assert_eq!(classify_render(false, false, false), Hardware);
    assert!(waits_on_gpu_recovery(true, true, false), "stopped window during recovery");
    assert!(waits_on_gpu_recovery(true, false, false), "device refuses work during recovery");
    assert!(!waits_on_gpu_recovery(true, false, true), "a healthy window is not waiting");
    assert!(!waits_on_gpu_recovery(false, true, false), "no recovery in progress");
}
