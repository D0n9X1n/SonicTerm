use super::theme_tab_color_choices;
use sonicterm_cfg::theme::Theme;

#[test]
fn tab_color_choices_include_reset_and_only_ansi_colors() {
    let theme = Theme::default();
    let bg = theme.colors.background.0.to_ascii_lowercase();
    let choices = theme_tab_color_choices(&theme);

    assert_eq!(choices.first().map(|choice| choice.name.as_str()), Some("Reset to Default"));
    assert_eq!(choices.first().and_then(|choice| choice.hex.as_deref()), None);
    assert_eq!(choices.len(), 17);
    assert!(choices
        .iter()
        .skip(1)
        .all(|choice| choice.name.starts_with("ANSI ") || choice.name.starts_with("Bright ")));
    assert!(choices
        .iter()
        .filter_map(|choice| choice.hex.as_ref())
        .all(|hex| hex.to_ascii_lowercase() != bg));
}
