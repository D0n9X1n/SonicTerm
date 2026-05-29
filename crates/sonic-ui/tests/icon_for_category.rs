use sonic_ui::prefs::CATEGORIES;

#[test]
fn each_category_returns_non_empty_icon_svg() {
    for category in CATEGORIES {
        let icon = category.icon();
        assert!(!icon.key.is_empty(), "{} has empty icon key", category.label());
        assert!(!icon.svg.trim().is_empty(), "{} has empty icon SVG", category.label());
        assert!(icon.svg.contains("<svg"), "{} icon is not SVG", category.label());
    }
}
