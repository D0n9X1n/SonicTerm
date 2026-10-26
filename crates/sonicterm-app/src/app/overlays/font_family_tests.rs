use super::*;
use crate::app::font_catalog::FontCatalog;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::command_palette::CommandPaletteMode;
use winit::keyboard::{Key, NamedKey};

/// A headless App on `family`, with a catalog of Menlo, Monaco and Rec Mono and one child window.
fn picker_app(family: &str) -> (App, winit::window::WindowId) {
    let mut config = Config::default();
    config.font.family = family.into();
    let mut app = App::new(Theme::default(), config, Keymap::default());
    app.font_catalog = FontCatalog::Ready(vec!["Menlo".into(), "Monaco".into(), "Rec Mono".into()]);
    let child = app.__test_seed_child_window(&["work"]);
    (app, child)
}

/// The picker's row labels, in display order.
fn labels(app: &App) -> Vec<String> {
    app.command_palette.font_family_rows().iter().map(|row| row.label.clone()).collect()
}

/// The bindable action lists the catalog with the live family marked; an arrow applies the
/// highlighted family live, and Escape restores the one the picker opened with.
#[test]
fn arrows_preview_live_and_escape_restores() {
    let (mut app, child) = picker_app("Menlo");
    assert!(app.run_action_for_window(&Action::ChooseFontFamily, child));
    assert_eq!(app.command_palette.mode(), CommandPaletteMode::FontFamily);
    assert_eq!(labels(&app), ["✓ Menlo", "  Monaco", "  Rec Mono"]);
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowDown));
    assert_eq!(app.config.font.family, "Monaco");
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Escape));
    assert_eq!(app.config.font.family, "Menlo");
    assert!(!app.command_palette.is_open());
}

/// Enter keeps the highlighted family for the session, and Save writes it to `[font] family`.
#[test]
fn enter_keeps_the_family_and_save_writes_it() {
    let (mut app, child) = picker_app("Menlo");
    let dir = std::env::temp_dir().join(format!("sonicterm-font-picker-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "[font]\nsize = 13.0\n").unwrap();
    assert!(app.run_action_for_window(&Action::ChooseFontFamily, child));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowUp));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.config.font.family, "Rec Mono");
    assert!(!app.command_palette.is_open());
    app.save_current_settings_to(&path).unwrap();
    assert_eq!(Config::load_strict(&path).unwrap().font.family, "Rec Mono");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Every label the picker reads is translated in each shipped locale.
#[test]
fn picker_labels_exist_in_every_locale() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/i18n");
    for locale in ["en", "ja", "zh-CN"] {
        let text = std::fs::read_to_string(root.join(locale).join("messages.ftl")).unwrap();
        for key in LABEL_KEYS {
            assert!(
                text.lines().any(|line| line.starts_with(&format!("{key} ="))),
                "{locale} lacks {key}"
            );
        }
    }
}
