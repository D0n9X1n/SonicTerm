use super::*;
use crate::app::{App, FrontmostKind};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

static NEXT_TEMP_PATH: AtomicU64 = AtomicU64::new(0);

fn temp_config_path(case: &str) -> PathBuf {
    let sequence = NEXT_TEMP_PATH.fetch_add(1, AtomicOrdering::Relaxed);
    std::env::temp_dir()
        .join(format!("sonicterm-app-{case}-{}-{sequence}.toml", std::process::id()))
}

fn remove_test_path(path: &std::path::Path) {
    if path.is_dir() {
        std::fs::remove_dir_all(path).expect("remove test directory");
    } else if path.exists() {
        std::fs::remove_file(path).expect("remove test file");
    }
}

/// Regression: `ResetFontSize` (Cmd+0) used to return to
/// `FontConfig::default().size` — a compile-time constant the user never
/// chose. A config asking for any other size would be snapped away from it.
#[test]
fn reset_font_size_returns_to_the_configured_size_not_the_compile_time_default() {
    let mut cfg = Config::default();
    // Deliberately unlike `FontConfig::default().size`, so a regression that
    // reintroduces the constant is caught rather than coincidentally passing.
    cfg.font.size = 20.0;
    assert_ne!(
        cfg.font.size,
        sonicterm_cfg::config::FontConfig::default().size,
        "test is only meaningful when configured size differs from the default"
    );

    let mut app = App::new(Theme::default(), cfg, Keymap::default());
    assert_eq!(app.configured_font_size, 20.0, "baseline seeds from config at construction");

    app.set_font_size(16.0);
    assert_eq!(app.config.font.size, 16.0);

    app.reset_font_size();
    assert_eq!(app.config.font.size, 20.0, "reset must return to the configured size");
}

/// A reset already at the configured size is a no-op rather than a redundant
/// font re-apply, which would reset the glyph atlas for no reason.
#[test]
fn reset_font_size_at_the_configured_size_changes_nothing() {
    let mut cfg = Config::default();
    cfg.font.size = 17.5;
    let mut app = App::new(Theme::default(), cfg, Keymap::default());

    app.reset_font_size();
    assert_eq!(app.config.font.size, 17.5);
}

/// Applying a config moves the reset target with it. There is no background
/// watcher, so the only way to reach this path is an explicit reload — which
/// means "the config the session has loaded" and "the reset target" cannot
/// drift apart. Transient Cmd+/Cmd- adjustments are what a reset discards.
#[test]
fn applying_a_config_moves_the_reset_target() {
    let mut cfg = Config::default();
    cfg.font.size = 14.0;
    let mut app = App::new(Theme::default(), cfg, Keymap::default());
    assert_eq!(app.configured_font_size, 14.0);

    let mut reloaded = Config::default();
    reloaded.font.size = 18.0;
    app.configured_font_size = reloaded.font.size;
    app.apply_new_config(reloaded);
    assert_eq!(app.config.font.size, 18.0, "the reloaded size takes effect");

    app.set_font_size(11.0);
    app.reset_font_size();
    assert_eq!(app.config.font.size, 18.0, "reset returns to the reloaded size");
}

/// `weight_scale` follows the same rule as every other font field: it is read
/// from the config being applied, with no separate mechanism and no cached
/// copy that could go stale.
#[test]
fn applying_a_config_takes_its_weight_scale() {
    let mut cfg = Config::default();
    cfg.font.weight_scale = 1.0;
    let mut app = App::new(Theme::default(), cfg, Keymap::default());

    let mut reloaded = Config::default();
    reloaded.font.weight_scale = 2.5;
    app.apply_new_config(reloaded);

    assert_eq!(app.config.font.weight_scale, 2.5);
    assert_eq!(
        app.config.font.effective_weight_scale(),
        2.5,
        "a value inside 0.5..=5.0 survives the clamp"
    );
}

/// Out-of-range `weight_scale` falls back to 1.0 rather than reaching raster
/// math, on the reload path as much as at startup.
#[test]
fn applying_a_config_clamps_an_out_of_range_weight_scale() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());

    let mut reloaded = Config::default();
    reloaded.font.weight_scale = 9.0;
    app.apply_new_config(reloaded);

    assert_eq!(app.config.font.effective_weight_scale(), 1.0, "out-of-range falls back to 1.0");
}

/// Reloading either local-target kill switch immediately revokes every window's hover state.
#[test]
fn local_target_switch_reload_revokes_all_window_authorization() {
    use crate::app::hovered_url::HoveredUrl;
    use crate::app::path_target::{
        AbsoluteCell, AbsoluteCellSpan, PathKind, PathOpenDecision, PathProbeCandidate,
        PathProbeKey, PathProbeResult, PathProbeSelection, PathRowIdentity,
    };
    use sonicterm_vt::vt::Osc7Cwd;

    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    let child_id = app.__test_seed_child_window(&["child"]);
    let window_ids = [app.main_window_id.expect("synthetic main"), child_id];

    for window_id in window_ids {
        let candidate = PathProbeCandidate {
            spans: smallvec::smallvec![AbsoluteCellSpan { row: 22, start_col: 4, end_col: 9 }],
            target: sonicterm_cfg::url_scan::DetectedTarget::BareName("entry".into()),
            resolved_path: PathBuf::from("/work/entry"),
        };
        let key = PathProbeKey {
            window_id,
            pane_id: 7,
            pointed: AbsoluteCell { row: 22, col: 4 },
            view_top: 20,
            candidates: vec![candidate.clone()],
            rows: smallvec::smallvec![PathRowIdentity { row: 22, fingerprint: 11 }],
            cwd: Some(Osc7Cwd { authority: String::new(), path: "/work".into() }),
            cwd_revision: 3,
            scrollback_evicted: 0,
            screen_epoch: 0,
            alt_screen: false,
        };
        let window = app.windows.get_mut(&window_id).expect("seeded window");
        let request = window.path_probe.request(key.clone()).expect("new target probe");
        assert!(window.path_probe.accept(
            &PathProbeResult {
                request,
                selection: Some(PathProbeSelection {
                    candidate,
                    decision: PathOpenDecision::Openable(PathKind::Directory),
                }),
            },
            Some(&key),
        ));
        window.hovered_url = Some(HoveredUrl {
            cells: sonicterm_render_model::inputs::HoveredUrlCells::single(7, 2, 4, 10, true)
                .unwrap(),
            url: "entry".into(),
        });
        window.hover_link = true;
    }

    let mut reloaded = app.config.clone();
    reloaded.terminal.clickable_bare_names = false;
    app.apply_new_config(reloaded);

    for window_id in window_ids {
        let window = app.windows.get(&window_id).expect("seeded window");
        assert!(window.hovered_url.is_none());
        assert!(!window.hover_link);
        assert!(window
            .path_probe
            .decision_for(&PathProbeKey {
                window_id,
                pane_id: 7,
                pointed: AbsoluteCell { row: 22, col: 4 },
                view_top: 20,
                candidates: vec![PathProbeCandidate {
                    spans: smallvec::smallvec![AbsoluteCellSpan {
                        row: 22,
                        start_col: 4,
                        end_col: 9,
                    }],
                    target: sonicterm_cfg::url_scan::DetectedTarget::BareName("entry".into()),
                    resolved_path: PathBuf::from("/work/entry"),
                }],
                rows: smallvec::smallvec![PathRowIdentity { row: 22, fingerprint: 11 }],
                cwd: Some(Osc7Cwd { authority: String::new(), path: "/work".into() }),
                cwd_revision: 3,
                scrollback_evicted: 0,
                screen_epoch: 0,
                alt_screen: false,
            })
            .is_none());
    }
}

/// A reload re-reads the theme and keymap files even when `[theme]` and
/// `[keymap]` still name the same ones. Those are separate files whose
/// contents change without the name changing, so comparing names would make
/// an explicit reload silently skip an edited theme or keymap.
#[test]
fn a_reload_reapplies_theme_and_keymap_even_when_their_names_are_unchanged() {
    let cfg = Config::default();
    let theme_name = cfg.theme.clone();
    let keymap_name = cfg.keymap.clone();
    let mut app = App::new(Theme::default(), cfg, Keymap::default());

    let loaded = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = loaded.clone();
    app.keymap_loader = Some(Box::new(move |_name: &str| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Keymap::default())
    }));

    // Same names as the live config — the old name-comparison guard would
    // have skipped the keymap entirely here.
    let same_names = Config { theme: theme_name, keymap: keymap_name, ..Config::default() };
    app.apply_new_config(same_names);

    assert_eq!(
        loaded.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "keymap must be re-read even when its name is unchanged"
    );
}

#[test]
fn font_weight_steps_up_and_down_from_the_configured_value() {
    let mut cfg = Config::default();
    cfg.font.weight_scale = 1.0;
    let mut app = App::new(Theme::default(), cfg, Keymap::default());

    app.change_font_weight(0.25);
    assert_eq!(app.config.font.weight_scale, 1.25);

    app.change_font_weight(0.25);
    assert_eq!(app.config.font.weight_scale, 1.5);

    app.change_font_weight(-0.5);
    assert_eq!(app.config.font.weight_scale, 1.0);
}

/// Stepping past either end of `0.5..=5.0` clamps rather than producing a
/// value the three downstream clamps would silently reset to 1.0.
#[test]
fn font_weight_clamps_at_both_ends_of_the_accepted_range() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());

    for _ in 0..40 {
        app.change_font_weight(0.25);
    }
    assert_eq!(app.config.font.weight_scale, WEIGHT_SCALE_MAX);
    // A clamped value must still survive the shared clamp, not fall back to 1.0.
    assert_eq!(app.config.font.effective_weight_scale(), WEIGHT_SCALE_MAX);

    for _ in 0..40 {
        app.change_font_weight(-0.25);
    }
    assert_eq!(app.config.font.weight_scale, WEIGHT_SCALE_MIN);
    assert_eq!(app.config.font.effective_weight_scale(), WEIGHT_SCALE_MIN);
}

/// Reset returns to the configured weight, mirroring `ResetFontSize`. A config
/// asking for a non-default weight must not be snapped to 1.0.
#[test]
fn reset_font_weight_returns_to_the_configured_weight() {
    let mut cfg = Config::default();
    cfg.font.weight_scale = 2.0;
    let mut app = App::new(Theme::default(), cfg, Keymap::default());
    assert_eq!(app.configured_weight_scale, 2.0);

    app.change_font_weight(0.5);
    assert_eq!(app.config.font.weight_scale, 2.5);

    app.reset_font_weight();
    assert_eq!(
        app.config.font.weight_scale, 2.0,
        "reset must return to the configured weight, not 1.0"
    );
}

/// Weight changes must not disturb the font size or the size reset target —
/// the two controls are independent.
#[test]
fn changing_weight_leaves_font_size_untouched() {
    let mut cfg = Config::default();
    cfg.font.size = 16.0;
    let mut app = App::new(Theme::default(), cfg, Keymap::default());

    app.change_font_weight(1.0);

    assert_eq!(app.config.font.size, 16.0, "weight must not move the size");
    assert_eq!(app.configured_font_size, 16.0, "weight must not move the size reset target");
}

/// An explicit reload moves the weight reset target, the same rule the font
/// size baseline follows.
#[test]
fn applying_a_config_moves_the_weight_reset_target() {
    let mut cfg = Config::default();
    cfg.font.weight_scale = 1.0;
    let mut app = App::new(Theme::default(), cfg, Keymap::default());

    let mut reloaded = Config::default();
    reloaded.font.weight_scale = 3.0;
    app.configured_weight_scale = reloaded.font.effective_weight_scale();
    app.apply_new_config(reloaded);

    app.change_font_weight(0.5);
    app.reset_font_weight();
    assert_eq!(app.config.font.weight_scale, 3.0);
}

#[test]
fn save_current_settings_persists_transient_font_values_without_reapplying_them() {
    // Contract: saving writes live font values without reloading or mutating the running config.
    let path = temp_config_path("save-font-values");
    remove_test_path(&path);
    let mut stored = Config::default();
    stored.font.size = 13.0;
    stored.font.weight_scale = 1.25;
    std::fs::write(&path, stored.to_toml().expect("serialize starting config"))
        .expect("write starting config");

    let mut app = App::new(Theme::default(), stored, Keymap::default());
    app.change_font_size(2.0);
    app.change_font_weight(0.5);
    let live_size = app.config.font.size;
    let live_weight = app.config.font.effective_weight_scale();

    app.save_current_settings_to(&path).expect("save transient values");

    let persisted = Config::load_strict(&path).expect("strictly read saved config");
    assert_eq!(persisted.font.size, live_size);
    assert_eq!(persisted.font.effective_weight_scale(), live_weight);
    assert_eq!(app.config.font.size, live_size, "save must not reload the live config");
    assert_eq!(
        app.config.font.effective_weight_scale(),
        live_weight,
        "save must not reapply the renderer or live config",
    );
    remove_test_path(&path);
}

#[test]
fn successful_save_advances_both_reset_baselines() {
    // Contract: a successful save makes the persisted font values the new reset baselines.
    let path = temp_config_path("save-reset-baselines");
    remove_test_path(&path);
    let mut cfg = Config::default();
    cfg.font.size = 14.0;
    cfg.font.weight_scale = 1.0;
    std::fs::write(&path, cfg.to_toml().expect("serialize starting config"))
        .expect("write starting config");
    let mut app = App::new(Theme::default(), cfg, Keymap::default());

    app.change_font_size(3.0);
    app.change_font_weight(0.75);
    let saved_size = app.config.font.size;
    let saved_weight = app.config.font.effective_weight_scale();
    app.save_current_settings_to(&path).expect("save current values");

    assert_eq!(app.configured_font_size, saved_size);
    assert_eq!(app.configured_weight_scale, saved_weight);
    app.change_font_size(1.0);
    app.change_font_weight(0.25);
    app.reset_font_size();
    app.reset_font_weight();
    assert_eq!(app.config.font.size, saved_size);
    assert_eq!(app.config.font.effective_weight_scale(), saved_weight);
    remove_test_path(&path);
}

#[test]
fn failed_save_preserves_disk_live_values_and_reset_baselines() {
    // Contract: a failed save changes neither disk bytes, live values, nor reset baselines.
    let path = temp_config_path("save-failure");
    remove_test_path(&path);
    std::fs::write(&path, b"[font]\nsize = 'wrong shape'\nweight_scale = 1.0\n")
        .expect("write invalid original config");
    let before = std::fs::read(&path).expect("read original config bytes");

    let mut cfg = Config::default();
    cfg.font.size = 12.0;
    cfg.font.weight_scale = 1.0;
    let mut app = App::new(Theme::default(), cfg, Keymap::default());
    app.change_font_size(4.0);
    app.change_font_weight(0.5);
    let live_size = app.config.font.size;
    let live_weight = app.config.font.effective_weight_scale();
    let baseline_size = app.configured_font_size;
    let baseline_weight = app.configured_weight_scale;

    assert!(app.save_current_settings_to(&path).is_err());

    assert_eq!(std::fs::read(&path).expect("read unchanged config"), before);
    assert_eq!(app.config.font.size, live_size);
    assert_eq!(app.config.font.effective_weight_scale(), live_weight);
    assert_eq!(app.configured_font_size, baseline_size);
    assert_eq!(app.configured_weight_scale, baseline_weight);
    remove_test_path(&path);
}

#[test]
fn read_only_allows_explicit_save_current_settings_action() {
    // Contract: READONLY blocks terminal input but permits this explicit configuration action.
    assert!(super::super::keymap_dispatch::read_only_allows_action(&Action::SaveCurrentSettings));
}

#[test]
fn save_action_dispatch_persists_real_file_and_shows_confirmation() {
    // Contract: dispatch persists only live font scalars and confirms success to the user.
    let path = temp_config_path("save-action-dispatch");
    remove_test_path(&path);
    std::fs::write(
        &path,
        b"# preserve this comment\ntheme = 'wezterm'\n[font]\nsize = 13 # size note\nweight_scale = 1 # weight note\nunknown_font_key = 'keep'\n",
    )
    .expect("write action-dispatch config");

    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    let _path_guard = App::set_test_current_settings_path(path.clone());
    assert!(app.run_action(&Action::IncreaseFontSize));
    assert!(app.run_action(&Action::IncreaseFontWeight));
    assert!(app.run_action(&Action::SaveCurrentSettings));

    let saved = std::fs::read_to_string(&path).expect("read action-dispatch config");
    assert!(saved.contains("# preserve this comment"));
    assert!(saved.contains("unknown_font_key = 'keep'"));
    assert!(saved.contains("size = 14"));
    assert!(saved.contains("weight_scale = 1.25"));
    assert_eq!(app.__test_main_notification_message(), Some("Current font settings saved"));
    remove_test_path(&path);
}

#[test]
fn palette_enter_saves_once_and_targets_the_attached_child() {
    // Contract: palette submission saves once and routes confirmation to its attached child.
    let path = temp_config_path("save-palette-enter");
    remove_test_path(&path);
    std::fs::write(&path, b"[font]\nsize = 13\nweight_scale = 1\n")
        .expect("write palette-enter config");

    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_set_frontmost_window(Some(child));
    let _path_guard = App::set_test_current_settings_path(path.clone());
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_palette_query("current settings");

    assert!(app.__test_command_palette_handle_key(&winit::keyboard::Key::Named(
        winit::keyboard::NamedKey::Enter,
    )));

    assert!(!app.__test_palette_open());
    assert_eq!(app.__test_child_notification_message(child), Some("Current font settings saved"));
    assert_eq!(app.__test_main_notification_message(), None);
    let saved = std::fs::read_to_string(&path).expect("read palette-enter config");
    assert_eq!(saved.matches("size = 13").count(), 1);
    assert_eq!(saved.matches("weight_scale = 1").count(), 1);
    remove_test_path(&path);
}

#[test]
fn source_window_save_action_writes_once_and_targets_the_child_notification() {
    // Contract: source-window dispatch saves once and reports to that child, not the main window.
    let path = temp_config_path("save-source-window-dispatch");
    remove_test_path(&path);
    std::fs::write(&path, b"[font]\nsize = 13\nweight_scale = 1\n")
        .expect("write source-window config");

    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_set_frontmost_window(None);
    let _path_guard = App::set_test_current_settings_path(path.clone());

    assert!(app.run_action_for_window(&Action::SaveCurrentSettings, child));

    assert_eq!(app.__test_child_notification_message(child), Some("Current font settings saved"));
    assert_eq!(app.__test_main_notification_message(), None);
    let saved = std::fs::read_to_string(&path).expect("read source-window config");
    assert_eq!(saved.matches("size = 13").count(), 1);
    assert_eq!(saved.matches("weight_scale = 1").count(), 1);
    remove_test_path(&path);
}

#[test]
fn save_notification_helper_routes_success_and_failure_to_the_requested_kind() {
    // Contract: success and failure notifications target only the requested window kind.
    let success_path = temp_config_path("save-notification-success");
    remove_test_path(&success_path);
    let failure_path = success_path.with_extension("directory");
    remove_test_path(&failure_path);
    std::fs::create_dir(&failure_path).expect("create invalid directory target");
    std::fs::write(failure_path.join("sentinel"), b"keep directory nonempty")
        .expect("write failure sentinel");

    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_synthetic_main();
    let child = app.__test_seed_child_window(&["child"]);
    app.save_current_settings_to_for_kind(&success_path, FrontmostKind::Child(child));
    assert_eq!(app.__test_child_notification_message(child), Some("Current font settings saved"));
    assert_eq!(app.__test_main_notification_message(), None);

    app.save_current_settings_to_for_kind(&failure_path, FrontmostKind::Main);
    assert_eq!(
        app.__test_main_notification_message(),
        Some("Unable to save current font settings; existing config unchanged")
    );
    assert_eq!(
        app.__test_child_notification_message(child),
        Some("Current font settings saved"),
        "failure routing must not overwrite the child's success bubble",
    );

    remove_test_path(&failure_path);
    remove_test_path(&success_path);
}

/// Clearing the software-render mode restores the monitor's frame period.
///
/// The defect this pins was not in `software_render_frame_period` — that
/// function is a correct pure map. It was in the call sites, which resolved
/// the new period from `frame_period`, the field the degrade write had already
/// replaced with the cap. `Force` → `Off` therefore left a 144 Hz window paced
/// at 40 fps until restart.
///
/// Driven through `apply_new_config` rather than the resolver, because a test
/// of the resolver alone passes whether or not the fields are wired correctly.
#[test]
fn clearing_software_render_mode_restores_the_monitor_frame_period() {
    use sonicterm_cfg::config::SoftwareRenderMode;
    use std::time::Duration;

    let monitor = Duration::from_micros(6_944); // 144 Hz

    let mut cfg = Config::default();
    cfg.appearance.software_render_mode = SoftwareRenderMode::Force;
    let mut app = App::new(Theme::default(), cfg, Keymap::default());

    // The state the window-ready path leaves behind once degrade engages:
    // the monitor's period recorded, the resolved period capped.
    app.monitor_frame_period = monitor;
    app.frame_period = crate::app::SOFTWARE_RENDER_FRAME_PERIOD;
    app.software_render_degrade = true;

    let mut reloaded = Config::default();
    reloaded.appearance.software_render_mode = SoftwareRenderMode::Off;
    app.apply_new_config(reloaded);

    assert!(!app.software_render_degrade, "Off must clear the degrade decision");
    assert_eq!(
        app.frame_period, monitor,
        "clearing degrade must restore the monitor's period, not leave the software cap",
    );
    assert_eq!(
        app.monitor_frame_period, monitor,
        "the monitor's own period must survive the transition untouched",
    );
}

/// Re-engaging degrade caps again, and the monitor period still survives.
///
/// A fix that restored the monitor period by overwriting `monitor_frame_period`
/// with the resolved value would pass the test above once and fail on the
/// second cycle. This runs the transition twice.
#[test]
fn software_render_mode_can_be_toggled_repeatedly() {
    use sonicterm_cfg::config::SoftwareRenderMode;
    use std::time::Duration;

    let monitor = Duration::from_micros(8_333); // 120 Hz

    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.monitor_frame_period = monitor;
    app.frame_period = monitor;

    for cycle in 0..3 {
        let mut on = Config::default();
        on.appearance.software_render_mode = SoftwareRenderMode::Force;
        app.apply_new_config(on);
        assert_eq!(
            app.monitor_frame_period, monitor,
            "cycle {cycle}: the monitor period must survive engaging degrade",
        );

        let mut off = Config::default();
        off.appearance.software_render_mode = SoftwareRenderMode::Off;
        app.apply_new_config(off);
        assert_eq!(
            app.frame_period, monitor,
            "cycle {cycle}: clearing degrade must restore the monitor's period",
        );
    }
}
