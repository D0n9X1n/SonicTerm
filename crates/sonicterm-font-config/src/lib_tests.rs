//! Deterministic value-semantic tests for the font configuration model.

use crate::{
    FontAttributes, FontStretch, FontStyle, FontWeight, FreeTypeLoadFlags, RgbaColor, TextStyle,
};

#[test]
fn exports_font_config_value_types() {
    assert_eq!(RgbaColor::default().alpha, 255);
    assert_eq!(FontStyle::Italic.to_string(), "Italic");
    assert_eq!(FontStretch::Normal.to_opentype_stretch(), 5);
}

#[test]
fn font_stretch_round_trips_opentype_widths_and_clamps_edges() {
    let cases = [
        (FontStretch::UltraCondensed, 1),
        (FontStretch::ExtraCondensed, 2),
        (FontStretch::Condensed, 3),
        (FontStretch::SemiCondensed, 4),
        (FontStretch::Normal, 5),
        (FontStretch::SemiExpanded, 6),
        (FontStretch::Expanded, 7),
        (FontStretch::ExtraExpanded, 8),
        (FontStretch::UltraExpanded, 9),
    ];

    for (stretch, width) in cases {
        assert_eq!(stretch.to_opentype_stretch(), width);
        assert_eq!(FontStretch::from_opentype_stretch(width), stretch);
    }

    assert_eq!(FontStretch::from_opentype_stretch(0), FontStretch::UltraCondensed);
    assert_eq!(FontStretch::from_opentype_stretch(u16::MAX), FontStretch::UltraExpanded);
}

#[test]
fn font_weight_preserves_values_adjusts_weight_and_formats_names() {
    assert_eq!(FontWeight::default(), FontWeight::REGULAR);
    assert_eq!(FontWeight::from_opentype_weight(450).to_opentype_weight(), 450);
    assert_eq!(FontWeight::REGULAR.lighter(), FontWeight::EXTRALIGHT);
    assert_eq!(FontWeight::REGULAR.bolder(), FontWeight::DEMIBOLD);
    assert_eq!(FontWeight::THIN.lighter().to_opentype_weight(), 0);
    assert_eq!(FontWeight::BOLD.bolder(), FontWeight::BLACK);
    assert_eq!(FontWeight::BOLD.to_string(), "\"Bold\"");
    assert_eq!(FontWeight::EXTRABLACK.to_string(), "\"ExtraBlack\"");
    assert_eq!(FontWeight::from_opentype_weight(450).to_string(), "450");
}

#[test]
fn style_and_stretch_defaults_and_labels_are_stable() {
    assert_eq!(FontStyle::default(), FontStyle::Normal);
    assert_eq!(FontStretch::default(), FontStretch::Normal);
    assert_eq!(FontStyle::Oblique.to_string(), "Oblique");
    assert_eq!(FontStretch::SemiCondensed.to_string(), "SemiCondensed");
    assert_eq!(FontStretch::ExtraExpanded.to_string(), "ExtraExpanded");
}

#[test]
fn font_attributes_and_text_style_transform_values_without_mutating_source() {
    let attributes = FontAttributes::new("Example Mono");
    assert_eq!(attributes.family, "Example Mono");
    assert_eq!(attributes.weight, FontWeight::REGULAR);
    assert!(!attributes.is_fallback);
    assert!(!attributes.is_synthetic);

    let fallback = FontAttributes::new_fallback("Example Fallback");
    assert!(fallback.is_fallback);

    let style = TextStyle { font: vec![attributes], foreground: None };
    let bold = style.make_bold();
    let dim = style.make_half_bright();
    let italic = style.make_italic();

    assert_eq!(style.font[0].weight, FontWeight::REGULAR);
    assert_eq!(style.font[0].style, FontStyle::Normal);
    assert_eq!(bold.font[0].weight, FontWeight::DEMIBOLD);
    assert!(bold.font[0].is_synthetic);
    assert_eq!(dim.font[0].weight, FontWeight::EXTRALIGHT);
    assert!(dim.font[0].is_synthetic);
    assert_eq!(italic.font[0].style, FontStyle::Italic);
    assert!(italic.font[0].is_synthetic);
}

#[test]
fn first_font_family_reduction_keeps_other_families_unchanged() {
    let style = TextStyle {
        font: vec![
            FontAttributes::new("Example Mono ExtraBold Italic"),
            FontAttributes::new("Fallback Bold"),
        ],
        foreground: None,
    };

    let reduced = style.reduce_first_font_to_family();
    assert_eq!(reduced.font[0].family, "Example Mono");
    assert_eq!(reduced.font[1].family, "Fallback Bold");
    assert_eq!(style.font[0].family, "Example Mono ExtraBold Italic");
}

#[test]
fn freetype_load_flag_values_format_deterministically() {
    assert_eq!(FreeTypeLoadFlags::DEFAULT.to_string(), "DEFAULT");
    assert_eq!(FreeTypeLoadFlags::default_hidpi(), FreeTypeLoadFlags::NO_HINTING);
    assert_eq!(
        (FreeTypeLoadFlags::NO_HINTING | FreeTypeLoadFlags::NO_BITMAP).to_string(),
        "NO_HINTING|NO_BITMAP"
    );
}
