use super::*;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TMP: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(label: &str) -> PathBuf {
    let id = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("sonicterm-{label}-{}-{id}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn hex_parsing_accepts_hash_or_plain_and_rejects_malformed_values() {
    assert_eq!(Hex("#102030".into()).rgb(), Some((0x10, 0x20, 0x30)));
    assert_eq!(Hex("abcdef".into()).rgba(), Some([0xab, 0xcd, 0xef, 255]));
    for bad in ["", "#123", "#gg0000", "#12345678"] {
        assert_eq!(Hex(bad.into()).color(), None);
    }
}

#[test]
fn color_shift_clamps_amount_and_linear_conversion_covers_both_branches() {
    let from = Color::rgb(0, 10, 128);
    let to = Color::rgb(255, 110, 255);
    assert_eq!(from.shift_toward(to, -1.0), from);
    assert_eq!(from.shift_toward(to, 2.0), to);
    assert_eq!(from.shift_toward(to, 0.5), Color::rgb(128, 60, 192));

    let linear = from.to_rgba_f32_linear(0.5);
    assert_eq!(linear[0], 0.0);
    assert!((linear[1] - (10.0 / 255.0 / 12.92)).abs() < 1e-6);
    assert!((linear[2] - 0.21586).abs() < 1e-4);
    assert_eq!(linear[3], 0.5);
}

#[test]
fn path_resolution_honors_direct_user_then_bundled_precedence() {
    let assets = Path::new("/bundle/assets");
    assert_eq!(
        Theme::resolve_path_with("./theme.toml", assets, None),
        PathBuf::from("./theme.toml")
    );

    let home = temp_dir("theme-path");
    let user_theme = home.join("themes/custom.toml");
    fs::create_dir_all(user_theme.parent().unwrap()).unwrap();
    fs::write(&user_theme, "x").unwrap();
    assert_eq!(Theme::resolve_path_with("custom", assets, Some(&home)), user_theme);
    assert_eq!(
        Theme::resolve_path_with("missing", assets, Some(&home)),
        assets.join("themes/missing.toml")
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn canonical_names_collapse_non_ascii_alphanumeric_runs() {
    assert_eq!(canonical_theme_name("Modified Gruvbox Dark Hard"), "modified-gruvbox-dark-hard");
    assert_eq!(canonical_theme_name("  Théme 2!! "), "th-me-2");
    assert_eq!(canonical_theme_name("!!!"), "");
}

#[test]
fn bundled_default_and_accessibility_override_are_stable() {
    let mut theme = Theme::bundled_default();
    assert_eq!(theme.name, "Modified Gruvbox Dark Hard");
    assert_eq!(theme.appearance, Appearance::Dark);
    let before = theme.colors.background.clone();
    theme.apply_accessibility(&AccessibilityConfig::default());
    assert_eq!(theme.colors.background, before);
    theme.apply_accessibility(&AccessibilityConfig { high_contrast: true, ..Default::default() });
    assert_eq!(theme.colors.foreground.0, "#ffffff");
    assert_eq!(theme.colors.background.0, "#000000");
}

#[test]
fn strict_load_export_import_and_fallback_cover_file_branches() {
    let root = temp_dir("theme-io");
    let src = root.join("source.toml");
    let exported = root.join("exported.toml");
    let imported_dir = root.join("user-themes");
    let theme = Theme::bundled_default();
    theme.export_to_file(&src).unwrap();
    let loaded = Theme::load_strict(&src).unwrap();
    assert_eq!(loaded, theme);
    loaded.export_to_file(&exported).unwrap();
    assert_eq!(Theme::load_strict(&exported).unwrap(), theme);
    let canonical = Theme::import_from_file(&src, &imported_dir).unwrap();
    assert_eq!(canonical, "modified-gruvbox-dark-hard");
    assert!(imported_dir.join(format!("{canonical}.toml")).exists());

    let malformed = root.join("bad.toml");
    fs::write(&malformed, "not = [valid").unwrap();
    assert!(Theme::load_strict(&malformed).is_err());
    assert_eq!(Theme::load_or_default(&malformed), Theme::bundled_default());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn palette_first_eight_preserves_ansi_order() {
    let theme = Theme::bundled_default();
    let values: Vec<_> = theme.palette_first_8().iter().map(|hex| hex.0.as_str()).collect();
    assert_eq!(
        values,
        vec![
            "#1d2021", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#d5c4a1"
        ]
    );
}
