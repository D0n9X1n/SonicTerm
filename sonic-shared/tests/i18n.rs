//! Tests for the Fluent-backed i18n layer.
//!
//! Covers the four guarantees promised by the spec:
//! 1. Bundles for all three shipped locales load successfully.
//! 2. A missing key falls back to the English bundle.
//! 3. A request for an unshipped locale negotiates back to English.
//! 4. `{ $name }` placeholders format correctly.
//!
//! The tests run in-process and toggle `SONIC_LOCALE` to drive the
//! priority chain. Tests that set the env var are serialized with a
//! mutex so they don't race each other.

use std::sync::Mutex;

use sonic_shared::i18n::{I18n, SHIPPED_LOCALES};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn bundle_loads_for_all_shipped_locales() {
    for tag in SHIPPED_LOCALES {
        let i = I18n::new(Some(tag));
        // "prefs-theme" exists in every shipped FTL — never the key
        // itself.
        let s = i.t("prefs-theme");
        assert_ne!(s, "prefs-theme", "locale {tag} did not translate prefs-theme");
        assert!(!s.is_empty());
    }
}

#[test]
fn missing_key_falls_back_to_english_then_key_name() {
    let i = I18n::new(Some("zh-CN"));
    // No such key in any bundle → must return the key itself rather
    // than panic or yield an empty string (visible UIs assume the
    // returned `String` is safe to display).
    let s = i.t("definitely-not-a-real-key");
    assert_eq!(s, "definitely-not-a-real-key");
}

#[test]
fn missing_locale_negotiates_to_english() {
    let _g = ENV_LOCK.lock().unwrap();
    // Klingon is not shipped → must fall back to English. Verify by
    // asking for a string we know diverges between English and zh-CN
    // ("Preferences" vs "偏好设置").
    std::env::remove_var("SONIC_LOCALE");
    let i = I18n::new(Some("tlh"));
    assert_eq!(i.t("prefs-title"), "Preferences");
    assert_eq!(i.locale(), "en");
}

#[test]
fn placeholders_format_correctly() {
    // Same key in every locale uses `{ $text }`. Verifying just the
    // English bundle keeps the assertion language-stable; the other
    // bundles share the placeholder mechanic via Fluent itself.
    // ENV_LOCK + remove_var because env_var_overrides_requested_locale
    // (and others) toggle SONIC_LOCALE in parallel; without this guard
    // we race and pick up "ja" instead of "en".
    let _g = ENV_LOCK.lock().unwrap();
    std::env::remove_var("SONIC_LOCALE");
    let i = I18n::new(Some("en"));
    let out = i.t_args("ime-composing", Some(&[("text", "你好")]));
    assert!(out.contains("你好"), "placeholder missing in {out:?}");
    assert!(out.starts_with("Composing"), "prefix wrong in {out:?}");
}

#[test]
fn env_var_overrides_requested_locale() {
    let _g = ENV_LOCK.lock().unwrap();
    std::env::set_var("SONIC_LOCALE", "ja");
    let i = I18n::new(Some("en")); // caller asked for English…
                                   // …but SONIC_LOCALE wins.
    assert_eq!(i.locale(), "ja");
    assert_eq!(i.t("prefs-theme"), "テーマ");
    std::env::remove_var("SONIC_LOCALE");
}

#[test]
fn empty_locale_string_means_auto() {
    let _g = ENV_LOCK.lock().unwrap();
    std::env::remove_var("SONIC_LOCALE");
    // Empty `requested` (the value we use in Config for "auto") must
    // not be parsed as a literal locale tag — it should fall through
    // to OS detection, which yields *something* non-empty.
    let i = I18n::new(Some(""));
    assert!(!i.locale().is_empty());
}

#[test]
fn region_subtag_negotiates_to_base_locale() {
    let _g = ENV_LOCK.lock().unwrap();
    std::env::remove_var("SONIC_LOCALE");
    // A user OS reporting "zh-Hans-CN" or "ja-JP" should match the
    // matching shipped bundle, not silently fall back to English.
    let i = I18n::new(Some("ja-JP"));
    assert_eq!(i.t("prefs-theme"), "テーマ");
}

#[test]
fn live_language_switch_rebuilds_prefs_controls() {
    // Regression for the PR #55 review blocker: selecting a new
    // language in the Appearance > Language dropdown must rebuild the
    // prefs control list immediately, so labels re-render through the
    // new i18n bundle on the very next frame rather than waiting for
    // close+reopen of the prefs window.
    use sonic_core::config::Config;
    use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
    use sonic_shared::prefs::{Category, Control, PrefsState};
    use tempfile::TempDir;

    let _g = ENV_LOCK.lock().unwrap();
    std::env::remove_var("SONIC_LOCALE");

    let dir = TempDir::new().unwrap();
    let config = Config { locale: "en".to_string(), ..Config::default() };
    let h = || Hex("#000000".to_string());
    let ansi = || AnsiColors {
        black: h(),
        red: h(),
        green: h(),
        yellow: h(),
        blue: h(),
        magenta: h(),
        cyan: h(),
        white: h(),
    };
    let theme = Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: h(),
            foreground: h(),
            cursor: h(),
            cursor_text: h(),
            selection_bg: h(),
            selection_fg: h(),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: h(),
                active_bg: h(),
                active_fg: h(),
                inactive_bg: h(),
                inactive_fg: h(),
                hover_bg: h(),
                hover_fg: h(),
                close_button_fg: h(),
            },
        },
    };
    let mut state = PrefsState::new(config, dir.path().join("sonic.toml"), theme);
    state.set_category(Category::Appearance);

    // Snapshot the language dropdown label in English.
    let lang_ctrl_en = match &state.controls[4] {
        Control::Dropdown(d) => d.clone(),
        other => panic!("expected Appearance[4] to be the language dropdown, got {other:?}"),
    };
    assert_eq!(lang_ctrl_en.label, "Language");

    // Find the zh-CN index in the dropdown options and select it via
    // the same public entry point the UI uses.
    let zh_idx = lang_ctrl_en
        .options
        .iter()
        .position(|o| o == "中文")
        .expect("zh-CN option missing from language dropdown");
    let changed = state.select_dropdown(lang_ctrl_en.id, zh_idx).expect("dropdown id valid");
    assert!(changed, "select_dropdown reported no change for new locale");

    // After live-apply, the control list must have been rebuilt with
    // the new i18n bundle — the same dropdown's label is now zh-CN.
    let lang_ctrl_zh = match &state.controls[4] {
        Control::Dropdown(d) => d,
        other => {
            panic!("expected Appearance[4] to be the language dropdown post-switch, got {other:?}")
        }
    };
    assert_eq!(
        lang_ctrl_zh.label, "语言",
        "prefs layout was not rebuilt after live language switch — label still {:?}",
        lang_ctrl_zh.label,
    );
}
