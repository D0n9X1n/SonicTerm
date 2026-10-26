use super::*;

#[test]
fn keypad_mode_defaults_auto_and_round_trips_supported_values() {
    // Existing configs retain negotiated keypad behavior; numeric input requires an explicit choice.
    assert_eq!(Config::default().terminal.keypad_mode, KeypadMode::Auto);
    assert_eq!(toml::from_str::<Config>("").unwrap().terminal.keypad_mode, KeypadMode::Auto);
    for (name, expected) in [("auto", KeypadMode::Auto), ("numeric", KeypadMode::Numeric)] {
        let config: Config =
            toml::from_str(&format!("[terminal]\nkeypad_mode = \"{name}\"\n")).unwrap();
        assert_eq!(config.terminal.keypad_mode, expected);
        assert!(config.to_toml().unwrap().contains(&format!("keypad_mode = \"{name}\"")));
    }
    assert!(toml::from_str::<Config>("[terminal]\nkeypad_mode = \"unknown\"\n").is_err());
    let template = default_config_template();
    assert!(template.contains("keypad_mode = \"auto\""));
    assert!(template.contains("numeric"));
}

#[test]
fn default_terminal_term_program_is_sonicterm() {
    let cfg = Config::default();
    assert_eq!(cfg.terminal.term_program, "SonicTerm");
}

#[test]
fn font_weight_scale_defaults_to_identity_and_is_documented() {
    let cfg = Config::default();
    assert_eq!(cfg.font.weight_scale, 1.0);
    assert!(default_config_template().contains("weight_scale = 1"));
}

#[test]
fn font_weight_scale_rejects_non_finite_and_out_of_range_values() {
    for value in [f32::NAN, f32::INFINITY, 0.0, 0.49, 5.01] {
        let font = FontConfig { weight_scale: value, ..FontConfig::default() };
        assert_eq!(font.effective_weight_scale(), 1.0);
    }
    let font = FontConfig { weight_scale: 1.1, ..FontConfig::default() };
    assert_eq!(font.effective_weight_scale(), 1.1);
    // Guards the range extension: the renderer and font-stack clamps must
    // accept the same span, otherwise a value valid here is silently reset
    // to 1.0 further down the pipeline.
    for value in [2.5, 5.0] {
        let font = FontConfig { weight_scale: value, ..FontConfig::default() };
        assert_eq!(font.effective_weight_scale(), value);
    }
}

/// LCD coverage is opt-in and all three portable configuration values round-trip.
#[test]
fn subpixel_aa_defaults_off_and_parses_lowercase_modes() {
    assert_eq!(Config::default().font.subpixel_aa, SubpixelAaMode::Off);

    for (value, expected) in
        [("off", SubpixelAaMode::Off), ("rgb", SubpixelAaMode::Rgb), ("bgr", SubpixelAaMode::Bgr)]
    {
        let config: Config = toml::from_str(&format!("[font]\nsubpixel_aa = \"{value}\"\n"))
            .expect("supported subpixel AA mode parses");
        assert_eq!(config.font.subpixel_aa, expected);
        assert!(config
            .to_toml()
            .expect("serialize config")
            .contains(&format!("subpixel_aa = \"{value}\"")));
    }
}

#[test]
fn default_warm_window_pool_keeps_one_spare() {
    let cfg = Config::default();
    assert_eq!(cfg.window.warm_window_pool, 1);
    let template = default_config_template();
    assert!(template.contains("warm_window_pool = 1"));
    assert!(template.contains("0 disables"));
}

/// Local target and contextual bare-name activation are default-on and documented kill switches.
#[test]
fn clickable_local_target_switches_default_on_and_parse_independently() {
    let defaults = Config::default();
    assert!(defaults.terminal.clickable_local_targets);
    assert!(defaults.terminal.clickable_bare_names);
    let template = default_config_template();
    assert!(template.contains("clickable_local_targets = true"));
    assert!(template.contains("clickable_bare_names = true"));

    let cfg: Config = toml::from_str(
        r#"
[terminal]
clickable_local_targets = false
clickable_bare_names = true
"#,
    )
    .unwrap();
    assert!(!cfg.terminal.clickable_local_targets);
    assert!(cfg.terminal.clickable_bare_names);
}

#[test]
fn default_cursor_does_not_blink() {
    let cfg = Config::default();
    assert!(!cfg.terminal.cursor_blink);
    assert!(default_config_template().contains("cursor_blink = false"));
}

#[test]
fn parses_terminal_term_program_override() {
    let cfg: Config = toml::from_str(
        r#"
[terminal]
term_program = "WezTerm"
"#,
    )
    .unwrap();
    assert_eq!(cfg.terminal.term_program, "WezTerm");
    assert_eq!(cfg.terminal.scrollback, TerminalConfig::default().scrollback);
}

#[test]
fn default_config_paths_live_under_dot_sonicterm() {
    let dir = default_config_dir().expect("home dir should exist in tests");
    assert!(dir.ends_with(".sonicterm"));
    assert_eq!(Config::default_path().unwrap(), dir.join("sonicterm.toml"));
}

/// Canonical typed serialization intentionally omits source-only TOML state.
#[test]
fn canonical_toml_is_not_a_format_preserving_persistence_path() {
    let config: Config = toml::from_str(concat!(
        "# source comment\n",
        "future_top = \"source only\"\n",
        "[font]\n",
        "size = 13\n",
        "future_font = true\n",
    ))
    .unwrap();

    let canonical = config.to_toml().unwrap();
    assert!(!canonical.contains("source comment"));
    assert!(canonical.contains("future_top"));
    assert!(!canonical.contains("future_font"));
    assert!(canonical.contains("size = 13"));
}

#[test]
fn legacy_glyph_fit_values_parse_without_remaining_active_config() {
    // Contract: old v1/v2 files keep loading after the never-wired switch leaves the schema.
    for value in ["v1", "v2"] {
        let cfg: Config = toml::from_str(&format!(
            "[render]\nglyph_fit = \"{value}\"\nalt_screen_bg_fill = \"v2\"\n"
        ))
        .unwrap();

        assert_eq!(cfg.render.alt_screen_bg_fill, RenderImpl::V2);
        assert!(!cfg.to_toml().unwrap().contains("glyph_fit"));
    }
    assert!(!default_config_template().contains("glyph_fit"));
}

#[test]
fn seeding_user_examples_writes_theme_and_platform_keymaps() {
    let nonce =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "sonicterm-config-seed-{}-{}",
        std::process::id(),
        nonce
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    seed_user_examples(&dir).unwrap();
    assert!(dir.join("themes/wezterm.toml").exists());
    assert!(dir.join("keymaps/sonicterm-macos.toml").exists());
    assert!(dir.join("keymaps/sonicterm-windows.toml").exists());
    assert!(dir.join("keymaps/sonicterm-linux.toml").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

pub(super) fn font_persist_test_dir(label: &str) -> std::path::PathBuf {
    let nonce =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir()
        .join(format!("sonicterm-config-font-persist-{label}-{}-{nonce}", std::process::id(),));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `quit_on_last_window_close` is accepted and ignored: a config that sets it
/// to `false` still loads, and the parsed value survives. Removing the field
/// would break this test, so that removal needs an explicit decision.
#[test]
fn quit_on_last_window_close_false_still_loads() {
    let cfg: Config = toml::from_str("quit_on_last_window_close = false\n").unwrap();
    assert!(!cfg.quit_on_last_window_close);
    assert!(Config::default().quit_on_last_window_close);
}

/// A one-letter title gets a comfortable tab and a long title a roomier cap: the tab width
/// limits default to 240 and 320 logical pixels, with or without a config file.
#[test]
fn tab_width_limits_default_to_240_and_320() {
    let defaults = Config::default();
    assert_eq!((defaults.tab_min_width, defaults.tab_max_width), (240.0, 320.0));
    let parsed: Config = toml::from_str("").unwrap();
    assert_eq!((parsed.tab_min_width, parsed.tab_max_width), (240.0, 320.0));
}

/// Each width key parses on its own, as a float or an integer, and a key the config omits
/// keeps its default.
#[test]
fn tab_width_keys_parse_whether_present_or_missing() {
    let min_only: Config = toml::from_str("tab_min_width = 180.0\n").unwrap();
    assert_eq!((min_only.tab_min_width, min_only.tab_max_width), (180.0, 320.0));
    let max_only: Config = toml::from_str("tab_max_width = 400\n").unwrap();
    assert_eq!((max_only.tab_min_width, max_only.tab_max_width), (240.0, 400.0));
    let both: Config = toml::from_str("tab_min_width = 200\ntab_max_width = 260\n").unwrap();
    assert_eq!((both.tab_min_width, both.tab_max_width), (200.0, 260.0));
}

/// An unset `clickable_path_max_chars` defaults to 1024, the template documents it, and values clamp to 1..=1024.
#[test]
fn clickable_path_max_chars_defaults_and_clamps() {
    let defaults = Config::default();
    assert_eq!(defaults.terminal.clickable_path_max_chars, DEFAULT_CLICKABLE_PATH_CHARS);
    assert_eq!(DEFAULT_CLICKABLE_PATH_CHARS, 1024);
    assert!(default_config_template().contains("clickable_path_max_chars = 1024"));
    // A config without the key keeps the default.
    let unset: Config = toml::from_str("[terminal]\nclickable_bare_names = true\n").unwrap();
    assert_eq!(unset.terminal.effective_clickable_path_max_chars(), 1024);
    for (configured, effective) in [(0, 1), (1, 1), (200, 200), (1024, 1024), (5000, 1024)] {
        let cfg: Config =
            toml::from_str(&format!("[terminal]\nclickable_path_max_chars = {configured}\n"))
                .unwrap();
        assert_eq!(cfg.terminal.effective_clickable_path_max_chars(), effective, "{configured}");
    }
    assert_eq!(MAX_CLICKABLE_PATH_CHARS, 1024);
}

/// A non-empty `SONICTERM_CONFIG_DIR` replaces `~/.sonicterm`, so a diagnostic
/// run reads and writes only its scratch copy of the config.
#[test]
fn config_dir_override_replaces_the_default() {
    let dir = config_dir_with(|name| match name {
        CONFIG_DIR_ENV => Some("/scratch/config".into()),
        "HOME" => Some("/home/user".into()),
        _ => None,
    });
    assert_eq!(dir, Some(PathBuf::from("/scratch/config")));
}

/// An empty override is ignored and the home directory decides, as before.
#[test]
fn empty_config_dir_override_falls_back_to_home() {
    let dir = config_dir_with(|name| match name {
        CONFIG_DIR_ENV => Some("".into()),
        "HOME" => Some("/home/user".into()),
        _ => None,
    });
    assert_eq!(dir, Some(PathBuf::from("/home/user/.sonicterm")));
}

/// With no override and no home directory there is no config directory, which
/// callers already handle by running on defaults.
#[test]
fn config_dir_is_none_without_override_or_home() {
    assert_eq!(config_dir_with(|_| None), None);
}

#[test]
fn load_strict_rejects_an_invalid_status_line_and_template_resolves() {
    // An invalid `[status_line]` fails the whole load; the starter template's table is the default.
    let dir = font_persist_test_dir("status-line-invalid");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "[status_line]\nupdate_interval_ms = 1\n").unwrap();
    assert!(Config::load_strict(&path).is_err());
    let mut warnings = Vec::new();
    let fallback = Config::load_or_default_collecting(&path, &mut warnings);
    assert_eq!(fallback.status_line, crate::status_line::StatusLineConfig::default());
    assert_eq!(warnings.len(), 1);
    std::fs::write(&path, "[status_line.segment.mem]\ncolour = \"red\"\n").unwrap();
    let mut warnings = Vec::new();
    let _ = Config::load_or_default_collecting(&path, &mut warnings);
    assert_eq!(warnings.len(), 1, "unknown keys are reported once per load");
    assert!(warnings[0].contains("status_line.segment.mem.colour"));
    let template: Config = toml::from_str(&default_config_template()).unwrap();
    assert_eq!(template.status_line, crate::status_line::StatusLineConfig::default());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn program_notification_keys_default_on_and_reject_unknown_values() {
    // OSC 9 goes to the app and progress mirrors to the taskbar by default; a misspelled
    // delivery fails the load rather than silently turning notifications off.
    let defaults = NotificationsConfig::default();
    assert_eq!(defaults.osc9, Osc9Delivery::App);
    assert!(defaults.taskbar_progress);
    let parsed: Config =
        toml::from_str("[notifications]\nosc9 = \"off\"\ntaskbar_progress = false\n").unwrap();
    assert_eq!(parsed.notifications.osc9, Osc9Delivery::Off);
    assert!(!parsed.notifications.taskbar_progress);
    let desktop: Config = toml::from_str("[notifications]\nosc9 = \"desktop\"\n").unwrap();
    assert_eq!(desktop.notifications.osc9, Osc9Delivery::Desktop);
    assert!(toml::from_str::<Config>("[notifications]\nosc9 = \"toast\"\n").is_err());
}

/// A fresh config names the host's own monospace family, never a bundled font, so an
/// unconfigured install renders with a face the platform already provides.
#[test]
fn default_font_family_is_the_platform_monospace() {
    let expected = if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(windows) {
        "Cascadia Mono"
    } else {
        "monospace"
    };
    assert_eq!(DEFAULT_FONT_FAMILY, expected);
    assert_eq!(FontConfig::default().family, expected);
    assert_eq!(Config::default().font.family, expected);
}

/// Only text that names `[font] family` sets it: the starter template leaves the family to the
/// platform default, and text that does not parse counts as setting one.
#[test]
fn only_a_named_family_sets_the_font_family() {
    assert!(!Config::sets_font_family(&default_config_template()));
    assert!(!Config::sets_font_family(""));
    assert!(!Config::sets_font_family("[font]\nsize = 14.0\n"));
    assert!(Config::sets_font_family("[font]\nfamily = \"Monaco\"\n"));
    assert!(Config::sets_font_family("[font\nfamily = "));
    let template: Config = toml::from_str(&default_config_template()).unwrap();
    assert_eq!(template.font.family, DEFAULT_FONT_FAMILY);
}
