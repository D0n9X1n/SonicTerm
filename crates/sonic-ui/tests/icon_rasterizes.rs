use sonic_ui::{icon, prefs::Category};

#[test]
fn category_icon_rasterizes_non_blank_at_24_px() {
    let mask = icon::rasterize_alpha(Category::Font.icon(), 24);
    assert_eq!(mask.len(), 24 * 24);
    assert!(mask.iter().any(|alpha| *alpha > 0), "font icon rasterized blank");
}
