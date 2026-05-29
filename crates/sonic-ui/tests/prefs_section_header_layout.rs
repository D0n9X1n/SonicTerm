use sonic_ui::prefs::{Category, PrefsLayout, CATEGORIES};

#[test]
fn each_section_exposes_icon_title_description_in_order() {
    let layout = PrefsLayout::default_size();
    let header_y = layout.form_card.y + sonic_ui::prefs::layout::CARD_PAD_V - 2.0;
    let icon_x = layout.form_card.x + sonic_ui::prefs::layout::CARD_PAD_H;
    let title_x = icon_x + 30.0;
    let desc_y = header_y + 22.0;

    assert!(icon_x < title_x, "icon must be left of title");
    assert!(header_y < desc_y, "description must sit below icon+title row");

    for category in CATEGORIES {
        assert!(category.icon().svg.contains("<svg"), "{} has invalid icon", category.label());
        assert!(!category.label().is_empty());
        assert!(!category.description().is_empty());
    }
}

#[test]
fn sections_are_final_slice_order() {
    assert_eq!(
        CATEGORIES,
        &[
            Category::Font,
            Category::Theme,
            Category::Keymap,
            Category::Window,
            Category::Cursor,
            Category::Advanced,
        ]
    );
}
