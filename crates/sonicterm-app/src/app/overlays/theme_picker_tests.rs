use super::*;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_ui::command_palette::CommandPaletteMode;
use winit::keyboard::{Key, NamedKey};

/// Write a theme file at `path` named `name` with `background` and `appearance`; every other
/// color comes from the bundled default, so the file loads strictly.
fn write_theme(path: &std::path::Path, name: &str, appearance: Appearance, background: &str) {
    let mut theme = Theme::bundled_default();
    theme.name = name.into();
    theme.appearance = appearance;
    theme.colors.background = sonicterm_cfg::theme::Hex(background.into());
    theme.export_to_file(path).expect("theme writes");
}

/// A scratch config directory with a `themes` folder holding `mine` (dark) and `paper` (light),
/// and a headless App on `mine` whose config path points there, so the picker reads the scratch
/// user themes ahead of the bundled ones and never the real home directory.
fn picker_app(label: &str) -> (App, winit::window::WindowId, PathBuf) {
    let dir =
        std::env::temp_dir().join(format!("sonicterm-theme-picker-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("themes")).unwrap();
    write_theme(&dir.join("themes/mine.toml"), "Mine", Appearance::Dark, "#202020");
    write_theme(&dir.join("themes/paper.toml"), "Paper", Appearance::Light, "#fafafa");
    std::fs::write(dir.join("themes/broken.toml"), "not = [a theme").unwrap();
    let config = Config { theme: "mine".into(), ..Config::default() };
    let mine = Theme::load_strict(&dir.join("themes/mine.toml")).unwrap();
    let mut app = App::new(mine, config, Keymap::default());
    app.configs.runtime_config_path = Some(dir.join("sonicterm.toml"));
    let child = app.__test_seed_child_window(&["work"]);
    (app, child, dir)
}

/// The listed theme names in display order.
fn values(app: &App) -> Vec<String> {
    app.interaction.command_palette.picker_rows().iter().map(|row| row.value.clone()).collect()
}

/// The action opens a theme picker listing the user's themes and the bundled ones, light first,
/// with the live theme marked and highlighted; a file that does not parse is left out.
#[test]
fn opens_with_light_themes_first_and_the_live_one_marked() {
    let (mut app, child, dir) = picker_app("open");
    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    assert_eq!(app.interaction.command_palette.mode(), CommandPaletteMode::Picker);
    assert_eq!(app.interaction.command_palette.picker_kind(), Some(PickerKind::Theme));
    assert_eq!(app.interaction.command_palette.selected_picker_value(), Some("mine"));
    let listed = values(&app);
    assert!(!listed.contains(&"broken".to_string()), "an unparsable file is left out");
    for bundled in ["wezterm", "github-light", "catppuccin-latte", "nord"] {
        assert!(listed.contains(&bundled.to_string()), "{bundled} is listed");
    }
    let first_dark = listed.iter().position(|name| name == "mine").unwrap();
    assert!(listed.iter().position(|name| name == "paper").unwrap() < first_dark, "light first");
    let rows = app.interaction.command_palette.picker_rows();
    let mine = rows.iter().find(|row| row.value == "mine").unwrap();
    assert_eq!(mine.label, "✓ mine · dark");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Arrows apply the highlighted theme live; Escape restores the theme the picker opened with from
/// its snapshot, even when that theme's file was deleted while the picker was open.
#[test]
fn arrows_preview_live_and_escape_restores_a_deleted_original() {
    let (mut app, child, dir) = picker_app("escape");
    let original = app.theme.clone();
    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    std::fs::remove_file(dir.join("themes/mine.toml")).unwrap();
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowUp));
    assert_ne!(app.config.theme, "mine", "the arrow applied another theme");
    assert_ne!(app.theme, original);
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Escape));
    assert_eq!(app.config.theme, "mine");
    assert_eq!(app.theme, original);
    assert!(!app.interaction.command_palette.is_open());
    assert!(app.configs.theme_picker.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Moving back onto the original row after a preview, then Enter, keeps the original theme from
/// the snapshot although its file is gone; Enter on another row keeps that row for the session.
#[test]
fn enter_keeps_the_highlighted_theme_and_the_original_row_uses_the_snapshot() {
    let (mut app, child, dir) = picker_app("enter");
    let original = app.theme.clone();
    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    std::fs::remove_file(dir.join("themes/mine.toml")).unwrap();
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowDown));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowUp));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.config.theme, "mine");
    assert_eq!(app.theme, original);

    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    app.interaction.command_palette.input_str("paper");
    app.preview_picker();
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.config.theme, "paper");
    assert_eq!(app.theme.appearance, Appearance::Light);
    assert!(!app.interaction.command_palette.is_open());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A config reload while the picker shows a preview ends the preview: the page closes and the
/// reloaded theme stands, so no later Escape can restore the pre-reload snapshot over it.
#[test]
fn a_config_reload_ends_the_preview_and_its_theme_stands() {
    let (mut app, child, dir) = picker_app("reload");
    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    app.interaction.command_palette.input_str("paper");
    app.preview_picker();
    assert_eq!(app.config.theme, "paper");
    let reloaded = Config { theme: "mine".into(), ..Config::default() };
    app.apply_new_config(reloaded);
    assert!(app.configs.theme_picker.is_none());
    assert!(!app.interaction.command_palette.is_open());
    assert_eq!(app.theme.colors.background.0, "#202020", "the reloaded theme stands");
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Escape));
    assert_eq!(app.theme.colors.background.0, "#202020", "Escape after reload restores nothing");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Opening the theme picker over an open font picker cancels the font picker first, so the font
/// it previewed goes back before the theme page replaces it.
#[test]
fn opening_over_the_font_picker_restores_its_font() {
    let (mut app, child, dir) = picker_app("font");
    app.config.font.family = "Menlo".into();
    app.fonts.font_catalog =
        crate::app::font_catalog::FontCatalog::Ready(vec!["Menlo".into(), "Monaco".into()]);
    assert!(app.run_action_for_window(&Action::ChooseFontFamily, child));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowDown));
    assert_eq!(app.config.font.family, "Monaco");
    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    assert_eq!(app.config.font.family, "Menlo");
    assert_eq!(app.interaction.command_palette.picker_kind(), Some(PickerKind::Theme));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Every label the picker reads is translated in each shipped locale.
#[test]
fn picker_labels_exist_in_every_locale() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/i18n");
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

/// Toggling the palette closed over a previewing theme picker skips its own close; the settle
/// after the event batch restores the original theme and drops the snapshot, so reopening the
/// picker captures the confirmed theme, not the abandoned preview.
#[test]
fn leaving_the_page_another_way_restores_the_original_theme() {
    let (mut app, child, dir) = picker_app("abandon");
    let original = app.theme.clone();
    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowUp));
    assert_ne!(app.config.theme, "mine");
    app.toggle_command_palette();
    assert_eq!(app.interaction.command_palette.picker_kind(), None);
    app.settle_abandoned_pickers();
    assert_eq!(app.config.theme, "mine");
    assert_eq!(app.theme, original);
    assert!(app.configs.theme_picker.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A font picker the palette left by another route gives back its family the same way.
#[test]
fn an_abandoned_font_picker_restores_its_family() {
    let (mut app, child, dir) = picker_app("abandon-font");
    app.config.font.family = "Menlo".into();
    app.fonts.font_catalog =
        crate::app::font_catalog::FontCatalog::Ready(vec!["Menlo".into(), "Monaco".into()]);
    assert!(app.run_action_for_window(&Action::ChooseFontFamily, child));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowDown));
    assert_eq!(app.config.font.family, "Monaco");
    app.toggle_command_palette();
    app.settle_abandoned_pickers();
    assert_eq!(app.config.font.family, "Menlo");
    assert!(app.fonts.font_picker_original.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Switching from the theme picker to the font picker cancels the theme preview first.
#[test]
fn opening_the_font_picker_over_the_theme_picker_restores_the_theme() {
    let (mut app, child, dir) = picker_app("to-font");
    let original = app.theme.clone();
    app.fonts.font_catalog = crate::app::font_catalog::FontCatalog::Ready(vec!["Menlo".into()]);
    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowUp));
    assert!(app.run_action_for_window(&Action::ChooseFontFamily, child));
    assert_eq!(app.config.theme, "mine");
    assert_eq!(app.theme, original);
    assert_eq!(app.interaction.command_palette.picker_kind(), Some(PickerKind::FontFamily));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A user theme named like a bundled one is listed once, from the user file, and previewing it
/// applies the user's colors.
#[test]
fn a_user_theme_replaces_the_bundled_theme_of_the_same_name() {
    let (mut app, child, dir) = picker_app("override");
    write_theme(&dir.join("themes/nord.toml"), "My Nord", Appearance::Dark, "#123456");
    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    let listed = values(&app);
    assert_eq!(listed.iter().filter(|name| *name == "nord").count(), 1);
    app.interaction.command_palette.input_str("nord");
    app.preview_picker();
    assert_eq!(app.config.theme, "nord");
    assert_eq!(app.theme.colors.background.0, "#123456");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Enter keeps the theme for the session only: the config file is left byte for byte.
#[test]
fn enter_does_not_write_the_config_file() {
    let (mut app, child, dir) = picker_app("no-write");
    let config_path = dir.join("sonicterm.toml");
    std::fs::write(&config_path, "theme = \"mine\"\n# keep me\n").unwrap();
    let before = std::fs::read(&config_path).unwrap();
    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    app.interaction.command_palette.input_str("paper");
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.config.theme, "paper");
    assert_eq!(std::fs::read(&config_path).unwrap(), before);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Closing the window that shows a previewing picker cancels it: the original theme comes back
/// and the snapshot is dropped, so the remaining windows never keep an unconfirmed preview.
#[test]
fn closing_the_picker_window_restores_the_original_theme() {
    let (mut app, child, dir) = picker_app("close-window");
    let original = app.theme.clone();
    assert!(app.run_action_for_window(&Action::ChooseTheme, child));
    // The headless app has no frontmost child, so the page is pinned to `child` here.
    app.interaction.palette_attached_window = Some(child);
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowUp));
    assert_ne!(app.config.theme, "mine");
    app.release_child_window_registries(child);
    assert_eq!(app.config.theme, "mine");
    assert_eq!(app.theme, original);
    assert!(app.configs.theme_picker.is_none());
    assert!(!app.interaction.command_palette.is_open());
    std::fs::remove_dir_all(&dir).unwrap();
}
