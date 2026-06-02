use sonicterm_cfg::theme::Theme;

#[test]
fn new_bundled_themes_parse_and_have_non_empty_colors() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");

    for theme_name in ["solarized-dark", "monokai-pro", "one-dark"] {
        let theme = Theme::load_strict(&root.join(format!("assets/themes/{theme_name}.toml")))
            .unwrap_or_else(|err| panic!("load {theme_name}: {err:#}"));

        assert!(!theme.name.is_empty(), "{theme_name} name should not be empty");
        assert!(theme.colors.background.rgb().is_some(), "{theme_name} background should be valid");
        assert!(theme.colors.foreground.rgb().is_some(), "{theme_name} foreground should be valid");
        assert!(
            theme.colors.ansi.black.rgb().is_some(),
            "{theme_name} ANSI palette should be valid"
        );
        assert!(
            theme.colors.bright.white.rgb().is_some(),
            "{theme_name} bright palette should be valid"
        );
    }
}
