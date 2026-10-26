//! Status settings actions and multi-window renderer propagation.

use super::super::status_line_tests::*;
use super::*;

/// A strip placed at the top, under a 28 px titlebar band, takes presses inside its band only:
/// the titlebar above it and the old bottom place both stay outside.
#[test]
fn a_right_press_hits_the_strip_at_the_top() {
    use sonicterm_cfg::status_line::StatusPosition;
    use sonicterm_gpu::status_strip::strip_placement;
    let placement = strip_placement(StatusPosition::Top, 600.0, 28.0, 40.0, 23.0);
    let mut app = app_with_strip(true);
    let child = app.__test_seed_child_window(&["work"]);
    let strip = sonicterm_gpu::status_strip::StripHit { top: placement.top, ..strip_over_top() };
    app.windows.get_mut(&child).unwrap().status_strip = Some(strip);
    assert!(!app.right_press_status_line(child, 50.0, 10.0), "in the titlebar band");
    assert!(!app.right_press_status_line(child, 50.0, 580.0), "where the bottom strip was");
    assert!(app.right_press_status_line(child, 50.0, 35.0), "inside the top band");
    assert!(app.command_palette.is_open());
}

/// App tests hold no renderer, so this pins the sync call site: every window's renderer gets the
/// resolved position before its visibility, and either change refits the panes once. The
/// renderer side (insets, kept layout, the band after a present) is tested in the GPU crate.
#[test]
fn sync_moves_each_renderer_strip_before_refitting_once() {
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
    let moved = source
        .find("let moved = strip.set_position(state.resolved.position);")
        .expect("sync hands each renderer the resolved position");
    let visible = source
        .find("let refit = strip.set_visible(shown) | moved;")
        .expect("a move or a visibility change refits, with both setters always run");
    assert!(moved < visible, "the position is set before visibility decides the refit");
    let refit = source.find("            if refit {").expect("the refit");
    let block = &source[refit..refit + source[refit..].find("\n            }\n").unwrap()];
    assert_eq!(block.matches("resize_panes_to_rects(").count(), 1, "one refit");
}

/// A reload that changes `position` moves the strip: the session takes the file's new position,
/// and Save Current Settings then carries it.
#[test]
fn reload_switches_the_strip_position() {
    use sonicterm_cfg::status_line::StatusPosition;
    let mut app = app_with_strip(true);
    let now = Instant::now();
    let _ = app.sync_status_line(now);
    assert_eq!(app.status_line.resolved.position, StatusPosition::Top, "top by default");
    app.config.status_line.position = StatusPosition::Bottom;
    let _ = app.sync_status_line(now);
    assert_eq!(app.status_line.resolved.position, StatusPosition::Bottom);
    assert_eq!(app.status_line_save().position, StatusPosition::Bottom);
    app.config.status_line.position = StatusPosition::Top;
    let _ = app.sync_status_line(now);
    assert_eq!(app.status_line.resolved.position, StatusPosition::Top, "and back");
}

/// A reload that changes `font_scale` reaches every window's renderer: each window's strip is
/// handed the file's new scale and asked for one frame, with nothing else changed.
#[test]
fn reload_changes_the_strip_font_scale_in_every_window() {
    use crate::app::window_state::window_redraw_requests_for as asks;
    use sonicterm_cfg::status_line::StatusFontScale;
    let path = scratch_config("reload-font-scale", "[status_line]\nenabled = true\n");
    let mut app =
        App::new(Theme::default(), Config::load_strict(&path).unwrap(), Keymap::default());
    app.runtime_config_path = Some(path.clone());
    let windows = seed_recorded_windows(&mut app);
    {
        let _dispatch = app.frame_dispatch_scope();
        let _ = app.sync_status_line(Instant::now());
    }
    for id in windows {
        let strip = recorded_strip(&app, id);
        assert!(strip.visible, "the strip is shown in every window");
        assert!((strip.font_scale - 0.8).abs() < 1e-6, "{strip:?}");
    }
    let before = windows.map(|id| asks(&app.windows[&id]));
    std::fs::write(&path, "[status_line]\nenabled = true\nfont_scale = 0.6\n").unwrap();
    app.force_reload_config();
    {
        let _dispatch = app.frame_dispatch_scope();
        let _ = app.sync_status_line(Instant::now());
    }
    assert_eq!(app.status_line.resolved.font_scale, StatusFontScale::from_f64(0.6).unwrap());
    for (index, id) in windows.into_iter().enumerate() {
        let strip = recorded_strip(&app, id);
        assert!((strip.font_scale - 0.6).abs() < 1e-6, "window {index}: {strip:?}");
        assert_eq!(asks(&app.windows[&id]) - before[index], 1, "window {index} redraws once");
    }
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Each palette action changes the live value and the strip geometry that follows from it, and
/// hands it to every window's renderer strip: Move to Top/Bottom moves the band, Larger/Smaller
/// step the text by 0.05 within 0.3..=1.0, and Reset returns it to 0.8.
#[test]
fn status_line_palette_actions_change_the_live_strip() {
    use sonicterm_cfg::keymap::Action;
    use sonicterm_cfg::status_line::{StatusFontScale, StatusPosition};
    use sonicterm_gpu::status_strip::{status_line_font_size, strip_placement};
    let mut app = app_with_strip(true);
    let windows = seed_recorded_windows(&mut app);
    let _ = app.sync_status_line(Instant::now());
    // Every window's renderer strip holds the session's position, text size and visibility.
    let every_window_follows = |app: &App, action: &str| {
        for id in windows {
            let strip = recorded_strip(app, id);
            let resolved = &app.status_line.resolved;
            assert_eq!(strip.position, resolved.position, "{action}: {strip:?}");
            assert!(
                (strip.font_scale - resolved.font_scale.as_f32()).abs() < 1e-6,
                "{action}: {strip:?}"
            );
            assert!(strip.visible, "{action}: {strip:?}");
        }
    };
    let band_top =
        |app: &App| strip_placement(app.status_line.resolved.position, 600.0, 28.0, 40.0, 23.0).top;
    let text_pt =
        |app: &App| status_line_font_size(13.0, app.status_line.resolved.font_scale.as_f32());
    assert_eq!(band_top(&app), 28.0);
    assert!(app.run_action(&Action::StatusLinePositionBottom));
    every_window_follows(&app, "Move to Bottom");
    assert_eq!(app.status_line.resolved.position, StatusPosition::Bottom);
    assert_eq!(band_top(&app), 537.0, "the band moves to the bottom edge");
    assert!(app.run_action(&Action::StatusLinePositionTop));
    every_window_follows(&app, "Move to Top");
    assert_eq!(band_top(&app), 28.0);
    assert!(app.run_action(&Action::IncreaseStatusLineFontScale));
    every_window_follows(&app, "Larger Text");
    assert!((app.status_line.resolved.font_scale.as_f32() - 0.85).abs() < 1e-6);
    assert!((text_pt(&app) - 11.05).abs() < 1e-4, "the strip text grows");
    for _ in 0..20 {
        app.run_action(&Action::IncreaseStatusLineFontScale);
    }
    assert_eq!(app.status_line.resolved.font_scale, StatusFontScale::MAX, "clamped at 1.0");
    for _ in 0..30 {
        app.run_action(&Action::DecreaseStatusLineFontScale);
    }
    assert_eq!(app.status_line.resolved.font_scale, StatusFontScale::MIN, "clamped at 0.3");
    every_window_follows(&app, "Smaller Text");
    assert!(app.run_action(&Action::ResetStatusLineFontScale));
    every_window_follows(&app, "Reset Text Size");
    assert_eq!(app.status_line.resolved.font_scale, StatusFontScale::DEFAULT);
}

/// Save Current Settings writes what the session shows, each key only where it differs from the
/// file: `enabled` after the toggle, `position` and `font_scale` after their actions; a reload of
/// the written file shows the same strip with no choice left.
#[test]
fn a_save_round_trips_the_toggle_position_and_text_size() {
    use sonicterm_cfg::keymap::Action;
    use sonicterm_cfg::status_line::{StatusFontScale, StatusPosition};
    let path = scratch_config("palette-save", "# mine\n");
    let mut app =
        App::new(Theme::default(), Config::load_strict(&path).unwrap(), Keymap::default());
    let _ = app.sync_status_line(Instant::now());
    app.save_current_settings_to(&path).expect("save");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("status_line"), "nothing differs, nothing is written: {text}");
    assert!(app.run_action(&Action::ToggleStatusLine));
    assert!(app.run_action(&Action::StatusLinePositionBottom));
    assert!(app.run_action(&Action::DecreaseStatusLineFontScale));
    let shown = app.status_line.resolved.clone();
    app.save_current_settings_to(&path).expect("save");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("enabled = true"), "{text}");
    assert!(text.contains("position = \"bottom\""), "{text}");
    assert!(text.contains("font_scale = 0.75"), "{text}");
    assert!(app.status_line.overrides.is_empty(), "the save satisfied every choice");
    app.runtime_config_path = Some(path.clone());
    app.force_reload_config();
    let _ = app.sync_status_line(Instant::now());
    assert_eq!(app.status_line.resolved, shown, "the reload shows what was saved");
    assert_eq!(app.status_line.resolved.position, StatusPosition::Bottom);
    assert_eq!(app.status_line.resolved.font_scale, StatusFontScale::DEFAULT.stepped(-1));
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
