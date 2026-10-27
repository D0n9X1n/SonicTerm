//! Status strip visibility, reload and source-window propagation contracts.

use super::super::status_line_tests::*;
use super::*;

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
    app.observe_status_pane(
        window_id,
        StatusPaneFacts { mouse_captured: true, scroll_lines: 3, ..Default::default() },
    );
    assert!(app.status_line.pane_facts.is_empty());
    app.toggle_status_line();
    app.observe_status_pane(
        window_id,
        StatusPaneFacts { mouse_captured: true, scroll_lines: 3, ..Default::default() },
    );
    assert_eq!(app.status_line.pane_facts.len(), 1);
    let _ = app.sync_status_line(Instant::now());
    assert!(app.status_line.pane_facts.is_empty(), "facts of a closed window are dropped");
}

#[test]
fn a_hidden_unheard_strip_skips_the_full_pass() {
    // While no window shows the strip and no reader listens, each dispatch's sync does no per-window
    // work; showing it runs the pass, and the hide that follows runs one more to take the strip down.
    let mut app = app_with_strip(false);
    app.force_frame_counters_on().expect("no window or pane exists yet");
    let now = Instant::now();
    for _ in 0..3 {
        assert_eq!(app.sync_status_line(now), None);
    }
    assert_eq!(full_passes(&app), 0, "hidden with no reader, no sync runs the full pass");
    // A toggle syncs once itself.
    app.toggle_status_line();
    let _ = app.sync_status_line(now);
    let _ = app.sync_status_line(now);
    assert_eq!(full_passes(&app), 3, "a shown strip runs the pass every sync");
    app.toggle_status_line();
    assert_eq!(full_passes(&app), 4, "the hide's own sync runs one pass to take the strip down");
    let _ = app.sync_status_line(now);
    let _ = app.sync_status_line(now);
    assert_eq!(full_passes(&app), 4, "then the hidden strip is settled");
}

#[test]
fn a_reader_keeps_the_full_pass_while_the_strip_is_hidden() {
    // A connected assistive technology hears alerts and samples with the strip hidden, so its sync
    // runs the full pass; once it disconnects, one more pass clears what it set up.
    let mut app = app_with_strip(false);
    app.force_frame_counters_on().expect("no window or pane exists yet");
    let mut reader = crate::app::accessibility::WindowAccessibility::default();
    reader.active = true;
    app.accessibility.insert(WindowId::dummy(), reader);
    let now = Instant::now();
    let _ = app.sync_status_line(now);
    assert_eq!(full_passes(&app), 1);
    app.accessibility.clear();
    let _ = app.sync_status_line(now);
    let _ = app.sync_status_line(now);
    assert_eq!(full_passes(&app), 2, "the pass after the reader leaves is the last");
}

#[test]
fn retiring_a_pane_forgets_its_status_records() {
    // A retired pane's records are dropped at the next sync even while the strip is hidden and that
    // sync skips the full pass, so closed panes leave nothing behind.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window or pane exists yet");
    let pane_id = app.__test_seed_tab("source");
    let main_id = app.main_window_id.unwrap();
    app.note_clip_write(pane_id, 5, true);
    app.status_line.history_ack.insert(pane_id, 7);
    app.status_line.size_seen.insert(pane_id, 1);
    let pane = app.windows.get_mut(&main_id).and_then(|window| window.remove_pane(pane_id));
    app.retire_pane(pane.expect("the seeded pane"));
    let _ = app.sync_status_line(Instant::now());
    assert_eq!(full_passes(&app), 0, "the strip is hidden, so the prune runs without the pass");
    assert!(!app.status_line.clip_write.contains_key(&pane_id), "clip result is forgotten");
    assert!(!app.status_line.history_ack.contains_key(&pane_id), "history ack is forgotten");
    assert!(!app.status_line.size_seen.contains_key(&pane_id), "size record is forgotten");
}

/// Contract: the link segment shows a hovered target only while the open modifier (Cmd on
/// macOS, Ctrl elsewhere) is held, and hides again when it is released, so plain hovering adds
/// no strip noise. The other platform's modifier alone never shows it.
#[test]
fn the_link_segment_shows_only_while_the_open_modifier_is_held() {
    let mut app = app_with_strip(true);
    let window_id = app.__test_seed_child_window(&["child"]);
    seed_hovered_url(&mut app, window_id);
    let hidden = strip_link(&app.windows[&window_id], None, &app.i18n);
    assert_eq!(hidden, None, "a plain hover shows no link");
    app.windows.get_mut(&window_id).unwrap().modifiers = open_modifier();
    let shown = strip_link(&app.windows[&window_id], None, &app.i18n);
    assert!(shown.is_some(), "holding the open modifier shows the hovered link");
    let other = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::CONTROL
    } else {
        winit::keyboard::ModifiersState::SUPER
    };
    app.windows.get_mut(&window_id).unwrap().modifiers = other;
    let other_shown = strip_link(&app.windows[&window_id], Some(&shown), &app.i18n);
    assert_eq!(other_shown, None, "only the platform's open modifier shows the link");
}

/// Contract: for a local path the segment reports only a failed detection (not found or
/// blocked). A path that resolved is already underlined and one still being checked has
/// nothing to say, so neither adds strip text.
#[test]
fn a_path_shows_only_when_detection_fails() {
    let app = app_with_strip(true);
    let path = std::path::Path::new("/tmp/project/src");
    assert_eq!(path_failure(ProbeStatus::Pending(path), &app.i18n), None, "checking is silent");
    for kind in [
        super::super::super::path_target::PathKind::File,
        super::super::super::path_target::PathKind::Directory,
    ] {
        let resolved = ProbeStatus::Selected(path, PathOpenDecision::Openable(kind));
        assert_eq!(path_failure(resolved, &app.i18n), None, "a resolved {kind:?} is silent");
    }
    let source = ProbeStatus::Selected(path, PathOpenDecision::SourceReveal);
    assert_eq!(path_failure(source, &app.i18n), None, "a revealable source is silent");
    let missing = ProbeStatus::Selected(path, PathOpenDecision::Missing);
    assert!(matches!(path_failure(missing, &app.i18n), Some(LinkInput::NotFound { .. })));
    let failed_missing = ProbeStatus::Failed(path, "path-error-missing");
    assert!(matches!(path_failure(failed_missing, &app.i18n), Some(LinkInput::NotFound { .. })));
    let blocked = ProbeStatus::Selected(path, PathOpenDecision::Blocked);
    assert!(matches!(path_failure(blocked, &app.i18n), Some(LinkInput::Blocked { .. })));
    for status in [
        ProbeStatus::Failed(path, "path-error-blocked"),
        ProbeStatus::NoDirectory(path),
        ProbeStatus::AltScreen(path),
    ] {
        assert!(
            matches!(path_failure(status, &app.i18n), Some(LinkInput::Blocked { .. })),
            "{status:?}"
        );
    }
}

#[test]
fn a_hover_an_event_clears_keeps_the_link_until_a_frame_resolves_it() {
    // An IME commit clears the hover and the next frame re-detects the same URL; the strip must
    // keep the link across that gap, or each keystroke redraws it twice for an unchanged pixel.
    // The open modifier is held, since the segment shows nothing without it.
    let mut app = app_with_strip(true);
    let window_id = app.__test_seed_child_window(&["child"]);
    seed_hovered_url(&mut app, window_id);
    app.windows.get_mut(&window_id).unwrap().modifiers = open_modifier();
    let shown = strip_link(&app.windows[&window_id], None, &app.i18n);
    assert!(shown.is_some(), "a resolved hover shows its link");
    app.windows.get_mut(&window_id).unwrap().invalidate_path_hover();
    assert!(app.windows[&window_id].hover_unresolved);
    let held = strip_link(&app.windows[&window_id], Some(&shown), &app.i18n);
    assert_eq!(held, shown, "a cleared hover the next frame re-detects keeps the strip unchanged");
    // The headless window has no grid under the pointer, so this lookup resolves to no target.
    app.refresh_target_hover(window_id);
    assert!(!app.windows[&window_id].hover_unresolved);
    let resolved = strip_link(&app.windows[&window_id], Some(&shown), &app.i18n);
    assert_eq!(resolved, None, "a frame that finds no target hides the link");
}

#[test]
fn a_strip_no_window_can_show_arms_no_wake() {
    // A focus-out report while covered records input whose hold ends later; nothing can show
    // that end, so the strip arms no wake until a window is eligible again.
    let mut app = app_with_strip(true);
    let window_id = app.__test_seed_child_window(&["child"]);
    let now = Instant::now();
    let _ = app.sync_status_line(now);
    assert!(app.status_line.visible);
    app.note_input(window_id, InputState::Blocked(1), now);
    assert_eq!(app.sync_status_line(now), None, "no window has a renderer, so none is eligible");
    assert!(
        app.status_line.input.contains_key(&window_id),
        "the record waits for an eligible sync"
    );
}

#[test]
fn session_segment_choices_survive_an_unrelated_or_failed_reload() {
    // A checklist choice applies at once; a reload that changes another key keeps it, a table that
    // fails to resolve keeps the last good file and every choice, and Reset returns to the file.
    use sonicterm_cfg::status_line::StatusSegmentId;
    use sonicterm_cfg::status_line::StatusSide;
    use sonicterm_cfg::status_overrides::MoveDirection;
    let mut app = app_with_strip(true);
    let now = Instant::now();
    let _ = app.sync_status_line(now);
    app.toggle_status_segment(StatusSegmentId::Cpu);
    app.move_status_segment(StatusSegmentId::Mem, MoveDirection::Left);
    let shown = |app: &App, id| app.status_line.resolved.segment(id).map(|segment| segment.side);
    assert_eq!(shown(&app, StatusSegmentId::Cpu), Some(StatusSide::Right));
    assert_eq!(shown(&app, StatusSegmentId::Mem), Some(StatusSide::Center));
    app.config.status_line.update_interval_ms = 5000;
    let _ = app.sync_status_line(now);
    assert_eq!(
        shown(&app, StatusSegmentId::Cpu),
        Some(StatusSide::Right),
        "an unrelated reload keeps it"
    );
    app.config.status_line.update_interval_ms = 1;
    let _ = app.sync_status_line(now);
    assert_eq!(
        shown(&app, StatusSegmentId::Mem),
        Some(StatusSide::Center),
        "a failed resolve keeps it"
    );
    assert_eq!(app.status_line.resolved.update_interval_ms, 5000, "the last good file stands");
    app.reset_status_line_choices();
    assert_eq!(shown(&app, StatusSegmentId::Cpu), None);
    assert_eq!(shown(&app, StatusSegmentId::Mem), Some(StatusSide::Right));
}

#[test]
fn a_saved_choice_survives_a_reload_with_no_choice_left() {
    // Save writes the shown lists; the written file becomes the baseline, so no choice is left,
    // and a following real reload shows exactly the same strip.
    use sonicterm_cfg::status_overrides::MoveDirection;
    let path = scratch_config("reload", "[status_line]\nenabled = true\n");
    let mut app =
        App::new(Theme::default(), Config::load_strict(&path).unwrap(), Keymap::default());
    let _ = app.sync_status_line(Instant::now());
    app.toggle_status_segment(StatusSegmentId::Cpu);
    app.move_status_segment(StatusSegmentId::Mem, MoveDirection::Left);
    let shown = app.status_line.resolved.clone();
    app.save_current_settings_to(&path).expect("save");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("right = [") && text.contains("center = ["), "{text}");
    assert!(!text.contains("preset"), "preset is never written: {text}");
    assert!(app.status_line.overrides.is_empty(), "the save satisfied every choice");
    assert_eq!(app.status_line.resolved, shown, "the strip did not change");
    app.runtime_config_path = Some(path.clone());
    app.force_reload_config();
    let _ = app.sync_status_line(Instant::now());
    assert_eq!(app.status_line.resolved, shown, "a reload after the save changes nothing");
    assert!(app.status_line.overrides.is_empty());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_save_drops_a_choice_an_outside_edit_already_satisfies() {
    // The file gained the chosen `cpu` on its own; the save leaves that line as edited, writes
    // only `hide_sensitive`, and drops both choices because the written file satisfies them.
    use sonicterm_cfg::status_line::StatusSide;
    let path = scratch_config("outside", "[status_line]\nenabled = true\n");
    let mut app =
        App::new(Theme::default(), Config::load_strict(&path).unwrap(), Keymap::default());
    let _ = app.sync_status_line(Instant::now());
    app.toggle_status_segment(StatusSegmentId::Cpu);
    app.toggle_status_line_sensitive();
    let right: Vec<String> = side_ids(&app.status_line.resolved, StatusSide::Right)
        .iter()
        .map(|id| format!("\"{}\"", id.as_str()))
        .collect();
    let edited = format!("[status_line]\nenabled = true\nright = [{}] # mine\n", right.join(", "));
    std::fs::write(&path, &edited).unwrap();
    app.save_current_settings_to(&path).expect("save");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(&format!("right = [{}] # mine", right.join(", "))), "{text}");
    assert!(text.contains("hide_sensitive = true"), "{text}");
    assert_eq!(text.matches("right =").count(), 1, "{text}");
    assert!(app.status_line.overrides.is_empty(), "{:?}", app.status_line.overrides);
    assert!(app.status_line.resolved.segment(StatusSegmentId::Cpu).is_some());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn restoring_the_file_after_a_save_takes_effect_on_reload() {
    // Save records the written table as applied, so a reload of the original file afterwards is a
    // real change: `cpu`, enabled and saved, goes away when the file no longer lists it.
    let original = "[status_line]\nenabled = true\n";
    let path = scratch_config("restore", original);
    let mut app =
        App::new(Theme::default(), Config::load_strict(&path).unwrap(), Keymap::default());
    let _ = app.sync_status_line(Instant::now());
    app.toggle_status_segment(StatusSegmentId::Cpu);
    app.save_current_settings_to(&path).expect("save");
    assert!(std::fs::read_to_string(&path).unwrap().contains("cpu"));
    std::fs::write(&path, original).unwrap();
    app.runtime_config_path = Some(path.clone());
    app.force_reload_config();
    let _ = app.sync_status_line(Instant::now());
    assert!(app.status_line.resolved.segment(StatusSegmentId::Cpu).is_none(), "the reload applied");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn every_ask_in_one_dispatch_reaches_a_window_once() {
    // The strip pass's layout and content asks, the checklist's ask and the input path's ask share
    // one record: within a dispatch a window is asked once, and the next dispatch may ask again.
    use crate::app::window_state::window_redraw_requests_for as asks;
    let mut app = app_with_strip(true);
    let child = app.__test_seed_child_window(&["work"]);
    let main = app.main_window_id.expect("the seed made a main window");
    app.open_status_segments(Some(child), Some(StatusSegmentId::Cpu));
    let before = (asks(&app.windows[&main]), asks(&app.windows[&child]));
    {
        let _dispatch = app.frame_dispatch_scope();
        let App { windows, dispatch_redraws, .. } = &mut app;
        // The strip pass asks twice when one pass both shows the strip and changes its pixels.
        dispatch_redraws.request(child, &windows[&child]);
        dispatch_redraws.request(child, &windows[&child]);
        app.request_checklist_redraw();
        app.finish_window_key(child);
    }
    assert_eq!(asks(&app.windows[&child]) - before.1, 1, "one ask in one dispatch");
    assert_eq!(asks(&app.windows[&main]), before.0, "the other window is not asked");
    {
        let _dispatch = app.frame_dispatch_scope();
        app.request_checklist_redraw();
    }
    assert_eq!(asks(&app.windows[&child]) - before.1, 2, "a new dispatch asks again");
}

#[test]
fn the_strip_pass_asks_for_frames_only_through_the_dispatch_record() {
    // Headless windows have no renderer, so the strip pass's own asks are pinned by source: the
    // layout, text-size and content asks all go through the record, never straight to the window.
    let source = concat!(
        include_str!("status_line.rs"),
        "\n",
        include_str!("status_line_actions.rs"),
        "\n",
        include_str!("status_line_inputs.rs"),
        "\n",
        include_str!("status_line_sync.rs")
    )
    .replace("\r\n", "\n");
    let start = source.find("fn sync_status_line(").expect("the strip pass");
    let body = &source[start..];
    let body = &body[..body.find("\n    }\n").expect("its end")];
    assert_eq!(body.matches("dispatch_redraws.request(*window_id, window)").count(), 3);
    assert!(!body.contains("request_window_redraw()"), "{body}");
}

#[test]
fn an_existing_and_a_later_window_build_their_strip_from_the_effective_lists() {
    // `rebuild_window_strip` is the per-window step of the strip pass: after a checklist change an
    // existing window rebuilds instead of keeping its cached strip, and a window opened later
    // builds the same segments from the same inputs.
    let mut app = app_with_strip(true);
    let existing = app.__test_seed_child_window(&["existing"]);
    let inputs = StatusLineInputs { cpu_percent: Some(12), ..StatusLineInputs::default() };
    let rebuild = |app: &mut App, window_id| {
        let App { status_line, i18n, .. } = app;
        let numbers = NumberStyle::for_locale("en");
        let mut shows = StripShows::default();
        status_line.rebuild_window_strip(window_id, inputs.clone(), i18n, numbers, &mut shows)
    };
    let has_cpu = |view: &StatusLineView| {
        view.segments.iter().any(|segment| segment.id == StatusSegmentId::Cpu)
    };
    let cpu_listed = app.current_status_line().segment(StatusSegmentId::Cpu).is_some();
    let first = rebuild(&mut app, existing).expect("a first build");
    assert_eq!(has_cpu(&first), cpu_listed);
    assert!(rebuild(&mut app, existing).is_none(), "unchanged lists and inputs build nothing");
    app.open_status_segments(Some(existing), Some(StatusSegmentId::Cpu));
    app.activate_status_segment_row();
    let after = rebuild(&mut app, existing).expect("the change rebuilds the existing window");
    assert_eq!(has_cpu(&after), !cpu_listed, "the existing window follows the checklist");
    let later = app.__test_seed_child_window(&["later"]);
    let fresh = rebuild(&mut app, later).expect("a new window builds its strip");
    assert_eq!(fresh, after, "a later window builds the same segments from the same inputs");
}

#[test]
fn a_right_press_routes_through_the_shared_dispatcher_in_main_and_torn_out_windows() {
    // Both window kinds route a right press through the same pointer owners: on the strip it
    // opens the checklist on the pressed row, the press and its release are consumed, nothing
    // reaches a PTY, and an already-open palette takes the click instead.
    use sonicterm_ui::command_palette::{CommandPaletteMode, StatusSegmentRowKind as Kind};
    use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
    let right = |state| WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: MouseButton::Right,
    };
    let mut app = app_with_strip(true);
    let child = app.__test_seed_child_window(&["torn"]);
    let main = app.main_window_id.expect("the seed made a main window");
    app.__test_enable_pty_write_log();
    for window_id in [main, child] {
        {
            let window = app.windows.get_mut(&window_id).unwrap();
            window.status_strip = Some(strip_over_top());
            window.cursor_pos = (50.0, 10.0);
        }
        assert_eq!(app.route_shared_pointer(window_id, &right(ElementState::Pressed)), Some(false));
        assert_eq!(
            app.command_palette.selected_status_row(),
            Some(Kind::Segment(StatusSegmentId::Mem)),
            "the pressed row is highlighted"
        );
        assert_eq!(app.palette_attached_window.or(app.main_window_id), Some(window_id));
        let release = app.route_shared_pointer(window_id, &right(ElementState::Released));
        assert_eq!(release, Some(false), "the release is consumed too");
        app.command_palette.close();
        app.palette_attached_window = None;
    }
    app.command_palette.open();
    assert!(app.route_shared_pointer(main, &right(ElementState::Pressed)).is_some());
    assert_eq!(
        app.command_palette.mode(),
        CommandPaletteMode::Commands,
        "the open palette took it"
    );
    assert!(app.__test_drain_pty_writes().is_empty(), "nothing reached a PTY");
}

#[test]
fn a_right_press_in_the_strip_opens_the_checklist_on_that_segment() {
    // Inside the band the press opens the checklist for that window, highlighting the pressed
    // segment, clickable or not, or the first row between segments; outside it nothing happens,
    // and the press never reaches the pane or its PTY.
    use sonicterm_ui::command_palette::StatusSegmentRowKind as Kind;
    let mut app = app_with_strip(true);
    let child = app.__test_seed_child_window(&["work"]);
    app.windows.get_mut(&child).unwrap().status_strip = Some(strip_over_top());
    app.__test_enable_pty_write_log();
    assert!(!app.right_press_status_line(child, 50.0, 200.0), "below the band");
    assert!(!app.command_palette.is_open());
    assert!(app.right_press_status_line(child, 50.0, 10.0), "on mem, which is inert");
    assert_eq!(
        app.command_palette.selected_status_row(),
        Some(Kind::Segment(StatusSegmentId::Mem))
    );
    assert_eq!(app.palette_attached_window, Some(child));
    app.command_palette.close();
    assert!(app.right_press_status_line(child, 150.0, 10.0), "between segments");
    assert_eq!(app.command_palette.selected_status_row(), Some(Kind::ShowStrip));
    assert!(app.__test_drain_pty_writes().is_empty(), "nothing reached a PTY");
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
fn startup_load_errors_come_from_the_startup_loads() {
    // The strip shows what startup loaded, read once then: a config fixed on disk before the check
    // still shows its startup failure until it reloads, and the check runs only once.
    let dir = std::env::temp_dir().join(format!("sonicterm-status-startup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "[status_line]\nupdate_interval_ms = 1\n").unwrap();
    let mut warnings = Vec::new();
    let (_, config_error) = Config::load_or_default_reporting(&path, &mut warnings);
    assert!(config_error.is_some(), "an invalid table fails the startup load");
    std::fs::write(&path, "").unwrap();
    let mut app = app_with_strip(true);
    app.runtime_config_path = Some(path.clone());
    app.set_startup_loads(crate::app::StartupLoads { config: config_error, ..Default::default() });
    app.record_startup_load_errors();
    assert!(app.status_line.config_error.is_some(), "the startup failure shows, not a reread");
    assert_eq!(app.status_line.theme_error, None, "the theme loaded at startup");
    app.set_startup_loads(crate::app::StartupLoads::default());
    app.record_startup_load_errors();
    assert!(app.status_line.config_error.is_some(), "the check runs only once");
    let _ = std::fs::remove_dir_all(dir);
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
