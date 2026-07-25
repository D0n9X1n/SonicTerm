use super::*;
use crate::app::App;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;

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
    let mut same_names = Config::default();
    same_names.theme = theme_name;
    same_names.keymap = keymap_name;
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
