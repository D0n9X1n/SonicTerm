use super::{best_name, name_from_table, names_from_table, FontPaletteInfo, Names, ParsedFont};
use crate::ftwrap::NameRecord;
use crate::locator::{FontDataHandle, FontDataSource, FontOrigin};
use crate::rangeset::RangeSet;
use config::{
    FontAttributes, FontStretch, FontStyle, FontWeight, FreeTypeLoadFlags, FreeTypeLoadTarget,
};
use ordered_float::NotNan;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

fn name_record(platform_id: u32, language_id: u16, name: &str) -> NameRecord {
    NameRecord {
        platform_id: platform_id as u16,
        encoding_id: 0,
        language_id,
        name_id: 0,
        name: name.to_string(),
    }
}

fn font(family: &str, weight: FontWeight, stretch: FontStretch, style: FontStyle) -> ParsedFont {
    ParsedFont {
        names: Names {
            full_name: format!("{family} Face"),
            family: family.to_string(),
            sub_family: Some(style.to_string()),
            postscript_name: Some(format!("{family}-PostScript")),
            aliases: vec![format!("{family} Alias")],
        },
        weight,
        stretch,
        style,
        cap_height: None,
        handle: FontDataHandle {
            source: FontDataSource::Memory {
                name: format!("{family}.ttf"),
                data: Default::default(),
            },
            index: 0,
            variation: 0,
            origin: FontOrigin::FontDirs,
            coverage: None,
        },
        coverage: Mutex::new(RangeSet::new()),
        synthesize_italic: false,
        synthesize_bold: false,
        synthesize_dim: false,
        assume_emoji_presentation: false,
        is_math_font: false,
        pixel_sizes: vec![],
        is_built_in_fallback: false,
        palettes: vec![],
        harfbuzz_features: None,
        freetype_load_target: None,
        freetype_render_target: None,
        freetype_load_flags: None,
        scale: None,
    }
}

fn attrs(weight: FontWeight, stretch: FontStretch, style: FontStyle) -> FontAttributes {
    FontAttributes {
        family: "Fixture".to_string(),
        weight,
        stretch,
        style,
        ..FontAttributes::default()
    }
}

#[test]
fn best_name_prefers_english_microsoft_then_platform_fallbacks() {
    let records = [
        name_record(freetype::TT_PLATFORM_APPLE_UNICODE, 0, "Unicode"),
        name_record(freetype::TT_PLATFORM_MICROSOFT, 0x411, "Windows Japanese"),
        name_record(freetype::TT_PLATFORM_MACINTOSH, 0, "Macintosh"),
        name_record(freetype::TT_PLATFORM_MICROSOFT, 0x409, "Windows English"),
    ];
    assert_eq!(best_name(&records), "Windows English");

    let platform_fallbacks = [
        name_record(freetype::TT_PLATFORM_APPLE_UNICODE, 0, "Unicode"),
        name_record(freetype::TT_PLATFORM_MICROSOFT, 0x411, "Windows"),
        name_record(freetype::TT_PLATFORM_MACINTOSH, 0, "Macintosh"),
    ];
    assert_eq!(best_name(&platform_fallbacks), "Macintosh");
}

#[test]
fn name_from_table_uses_first_populated_id_and_names_are_sorted_and_deduplicated() {
    let preferred_id = freetype::TT_NAME_ID_TYPOGRAPHIC_FAMILY;
    let fallback_id = freetype::TT_NAME_ID_FONT_FAMILY;
    let mut names = HashMap::new();
    names.insert(preferred_id, Vec::new());
    names.insert(
        fallback_id,
        vec![
            name_record(freetype::TT_PLATFORM_MICROSOFT, 0x409, "Zed"),
            name_record(freetype::TT_PLATFORM_MACINTOSH, 0, "Alpha"),
            name_record(freetype::TT_PLATFORM_APPLE_UNICODE, 0, "Zed"),
        ],
    );

    assert_eq!(name_from_table(&names, &[preferred_id, fallback_id]).as_deref(), Some("Zed"));
    assert_eq!(names_from_table(&names, &[preferred_id, fallback_id]), ["Alpha", "Zed"]);
    assert_eq!(name_from_table(&names, &[999]), None);
}

#[test]
fn name_matching_accepts_family_path_full_postscript_and_alias_but_is_case_sensitive() {
    let mut parsed =
        font("Fixture Family", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal);
    parsed.names.full_name = "Fixture Family Regular".to_string();
    parsed.names.postscript_name = Some("FixtureFamily-Regular".to_string());
    parsed.names.aliases = vec!["Fixture Legacy".to_string()];
    parsed.handle.source = FontDataSource::OnDisk(PathBuf::from("/fonts/fixture.ttf"));

    for matching_name in [
        "Fixture Family",
        "/fonts/fixture.ttf",
        "Fixture Family Regular",
        "FixtureFamily-Regular",
        "Fixture Legacy",
    ] {
        assert!(parsed.matches_name(&FontAttributes::new(matching_name)), "{matching_name}");
    }
    assert!(!parsed.matches_name(&FontAttributes::new("fixture family")));
    assert!(!parsed.matches_name(&FontAttributes::new("Unrelated")));
}

#[test]
fn fontconfig_match_origin_preserves_the_requested_alias() {
    let mut parsed =
        font("Resolved Family", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal);
    parsed.handle.origin = FontOrigin::FontConfigMatch("monospace".to_string());

    assert!(parsed.matches_name(&FontAttributes::new("monospace")));
    assert!(!parsed.matches_name(&FontAttributes::new("sans-serif")));
}

#[test]
fn matching_prefers_exact_stretch_then_css_directional_nearest_stretch() {
    let fonts = [
        font("Condensed", FontWeight::REGULAR, FontStretch::Condensed, FontStyle::Normal),
        font("SemiCondensed", FontWeight::REGULAR, FontStretch::SemiCondensed, FontStyle::Normal),
        font("Expanded", FontWeight::REGULAR, FontStretch::Expanded, FontStyle::Normal),
    ];
    let refs: Vec<_> = fonts.iter().collect();

    assert_eq!(
        ParsedFont::best_matching_index(
            &attrs(FontWeight::REGULAR, FontStretch::SemiCondensed, FontStyle::Normal),
            &refs,
            12,
        ),
        Some(1)
    );
    assert_eq!(
        ParsedFont::best_matching_index(
            &attrs(FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal),
            &refs,
            12,
        ),
        Some(1),
        "normal requests search narrower before wider"
    );
    assert_eq!(
        ParsedFont::best_matching_index(
            &attrs(FontWeight::REGULAR, FontStretch::SemiExpanded, FontStyle::Normal),
            &refs,
            12,
        ),
        Some(2),
        "expanded requests search wider before narrower"
    );
}

#[test]
fn matching_uses_css_style_fallback_order() {
    let fonts = [
        font("Oblique", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Oblique),
        font("Italic", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Italic),
    ];
    let refs: Vec<_> = fonts.iter().collect();

    assert_eq!(
        ParsedFont::best_matching_index(
            &attrs(FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal),
            &refs,
            12,
        ),
        Some(1),
        "normal falls back to italic before oblique"
    );
    assert_eq!(
        ParsedFont::best_matching_index(
            &attrs(FontWeight::REGULAR, FontStretch::Normal, FontStyle::Oblique),
            &refs,
            12,
        ),
        Some(0)
    );
}

#[test]
fn matching_applies_css_weight_special_cases_and_directional_search() {
    let medium_and_bold = [
        font("Medium", FontWeight::MEDIUM, FontStretch::Normal, FontStyle::Normal),
        font("Bold", FontWeight::BOLD, FontStretch::Normal, FontStyle::Normal),
    ];
    let refs: Vec<_> = medium_and_bold.iter().collect();
    assert_eq!(
        ParsedFont::best_matching_index(
            &attrs(FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal),
            &refs,
            12,
        ),
        Some(0),
        "400 prefers 500 when 400 is absent"
    );

    let regular_and_light = [
        font("Regular", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal),
        font("Light", FontWeight::LIGHT, FontStretch::Normal, FontStyle::Normal),
    ];
    let refs: Vec<_> = regular_and_light.iter().collect();
    assert_eq!(
        ParsedFont::best_matching_index(
            &attrs(FontWeight::MEDIUM, FontStretch::Normal, FontStyle::Normal),
            &refs,
            12,
        ),
        Some(0),
        "500 prefers 400 when 500 is absent"
    );

    let directional = [
        font("Thin", FontWeight::THIN, FontStretch::Normal, FontStyle::Normal),
        font("Bold", FontWeight::BOLD, FontStretch::Normal, FontStyle::Normal),
    ];
    let refs: Vec<_> = directional.iter().collect();
    assert_eq!(
        ParsedFont::best_matching_index(
            &attrs(FontWeight::LIGHT, FontStretch::Normal, FontStyle::Normal),
            &refs,
            12,
        ),
        Some(0),
        "weights through 500 search lighter first"
    );
    assert_eq!(
        ParsedFont::best_matching_index(
            &attrs(FontWeight::DEMIBOLD, FontStretch::Normal, FontStyle::Normal),
            &refs,
            12,
        ),
        Some(1),
        "weights above 500 search heavier first"
    );
}

#[test]
fn matching_uses_closest_bitmap_strike_only_when_every_candidate_is_bitmap() {
    let mut ten_px = font("TenPx", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal);
    ten_px.pixel_sizes = vec![10];
    let mut sixteen_px =
        font("SixteenPx", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal);
    sixteen_px.pixel_sizes = vec![16];
    let bitmaps = [ten_px.clone(), sixteen_px.clone()];
    let refs: Vec<_> = bitmaps.iter().collect();
    assert_eq!(ParsedFont::best_matching_index(&FontAttributes::default(), &refs, 15), Some(1));

    sixteen_px.pixel_sizes.clear();
    let mixed = [ten_px, sixteen_px];
    let refs: Vec<_> = mixed.iter().collect();
    assert_eq!(
        ParsedFont::best_matching_index(&FontAttributes::default(), &refs, 15),
        Some(0),
        "a scalable candidate disables bitmap-strike tie breaking"
    );
    let empty: [&ParsedFont; 0] = [];
    assert_eq!(ParsedFont::best_matching_index(&FontAttributes::default(), &empty, 15), None);
}

#[test]
fn synthesize_sets_style_weight_render_options_and_emoji_policy() {
    let mut parsed =
        font("Color Moji", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal);
    parsed.names.full_name = "Color Moji Regular".to_string();
    let requested = FontAttributes {
        family: "Color Moji".to_string(),
        weight: FontWeight::BOLD,
        style: FontStyle::Italic,
        harfbuzz_features: Some(vec!["liga=0".to_string(), "ss01=1".to_string()]),
        freetype_load_target: Some(FreeTypeLoadTarget::Light),
        freetype_render_target: Some(FreeTypeLoadTarget::HorizontalLcd),
        freetype_load_flags: Some(FreeTypeLoadFlags::NO_HINTING | FreeTypeLoadFlags::NO_BITMAP),
        scale: Some(NotNan::new(1.25).unwrap()),
        ..FontAttributes::default()
    };

    let synthesized = parsed.synthesize(&requested);
    assert!(synthesized.synthesize_italic);
    assert!(synthesized.synthesize_bold);
    assert!(!synthesized.synthesize_dim);
    assert!(synthesized.assume_emoji_presentation);
    assert_eq!(synthesized.harfbuzz_features, requested.harfbuzz_features);
    assert_eq!(synthesized.freetype_load_target, requested.freetype_load_target);
    assert_eq!(synthesized.freetype_render_target, requested.freetype_render_target);
    assert_eq!(synthesized.freetype_load_flags, requested.freetype_load_flags);
    assert_eq!(synthesized.scale, Some(1.25));
}

#[test]
fn synthesize_dims_regular_faces_and_respects_explicit_or_scoped_emoji_decisions() {
    let dim = font("Fixture", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal)
        .synthesize(&attrs(FontWeight::LIGHT, FontStretch::Normal, FontStyle::Normal));
    assert!(dim.synthesize_dim);
    assert!(!dim.synthesize_bold);

    let explicit_false = FontAttributes {
        assume_emoji_presentation: Some(false),
        ..FontAttributes::new("Color Moji")
    };
    let mut emoji = font("Color Moji", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal);
    emoji.assume_emoji_presentation = true;
    assert!(!emoji.synthesize(&explicit_false).assume_emoji_presentation);

    for (is_built_in_fallback, is_synthetic) in [(true, false), (false, true)] {
        let mut scoped =
            font("Color Moji", FontWeight::REGULAR, FontStretch::Normal, FontStyle::Normal);
        scoped.names.full_name = "Color Moji Regular".to_string();
        scoped.is_built_in_fallback = is_built_in_fallback;
        let mut requested = FontAttributes::new("Color Moji");
        requested.is_synthetic = is_synthetic;
        assert!(
            !scoped.synthesize(&requested).assume_emoji_presentation,
            "built-in and synthetic entries must not trigger the name heuristic"
        );
    }
}

#[test]
fn lua_fallback_reports_bitmap_synthesis_palette_alias_and_non_default_options() {
    let mut parsed = font("Fixture", FontWeight::BOLD, FontStretch::Condensed, FontStyle::Italic);
    parsed.synthesize_italic = true;
    parsed.synthesize_bold = true;
    parsed.assume_emoji_presentation = true;
    parsed.pixel_sizes = vec![12, 16];
    parsed.palettes = vec![FontPaletteInfo {
        name: "Dark".to_string(),
        palette_index: 2,
        usable_with_light_bg: true,
        usable_with_dark_bg: true,
    }];
    parsed.scale = Some(1.5);
    parsed.freetype_load_flags = Some(FreeTypeLoadFlags::NO_HINTING);
    parsed.freetype_load_target = Some(FreeTypeLoadTarget::Light);
    parsed.freetype_render_target = Some(FreeTypeLoadTarget::HorizontalLcd);
    parsed.harfbuzz_features = Some(vec!["liga=0".to_string()]);

    let output = ParsedFont::lua_fallback(&[parsed]);
    for expected in [
        "-- Will synthesize italics",
        "-- Will synthesize bold",
        "-- Assumed to have Emoji Presentation",
        "-- Pixel sizes: [12, 16]",
        "-- Palette: 2 Dark (with light bg) (with dark bg)",
        "-- AKA: \"Fixture Alias\"",
        "family=\"Fixture\"",
        "weight=\"Bold\"",
        "stretch=\"Condensed\"",
        "style=\"Italic\"",
        "scale=1.5",
        "freetype_load_flags=\"NO_HINTING\"",
        "freetype_load_target=\"Light\"",
        "freetype_render_target=\"HorizontalLcd\"",
        "harfbuzz_features={\"liga=0\"}",
    ] {
        assert!(output.contains(expected), "missing {expected:?} in {output}");
    }
}

fn tracked_font_path(file: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts").join(file)
}

fn parse_from_disk(path: &std::path::Path) -> ParsedFont {
    let handle = FontDataHandle {
        source: FontDataSource::OnDisk(path.to_path_buf()),
        index: 0,
        variation: 0,
        origin: FontOrigin::FontDirs,
        coverage: None,
    };
    ParsedFont::from_locator(&handle)
        .unwrap_or_else(|err| panic!("fixture {path:?} must parse: {err:#}"))
}

/// The configured family must not be mistaken for a math font.
///
/// This is the direction that would break the terminal outright: automatic
/// fallback resolution drops math fonts, so a predicate that caught an
/// ordinary monospace face would strip legitimate fonts out of the chain.
/// Asserting it separately is what stops a predicate that answers `true` for
/// everything from "fixing" the defect by emptying the chain.
#[test]
fn tracked_text_fixtures_are_not_math_fonts() {
    for file in [
        "RecMonoSt.Helens-Regular.ttf",
        "RecMonoSt.Helens-Bold.ttf",
        "RecMonoSt.Helens-Italic.ttf",
        "RecMonoSt.Helens-BoldItalic.ttf",
    ] {
        let font = parse_from_disk(&tracked_font_path(file));
        assert!(
            !font.is_math_font,
            "{file} carries no MATH table and must not be treated as a math font"
        );
    }
}

/// A face carrying `MATH` is detected, and the text faces it competes with
/// are not.
///
/// STIX Two Math and STIX Two Text share a design and a name prefix, so a
/// predicate keyed on the family name passes one and fails the other; they are
/// asserted together for that reason. Menlo is included because it is the font
/// that *should* win these codepoints — it served every correctly sized
/// fallback tile measured while diagnosing the defect, against STIX Two Math's
/// oversized ones.
///
/// No font with a `MATH` table is tracked in this repository, so the positive
/// case reads macOS's bundled STIX Two Math and is gated to that platform.
#[cfg(target_os = "macos")]
#[test]
fn math_table_detection_separates_math_from_text_faces() {
    // The positive case is the point of the test, so its absence is a failure
    // rather than a skip: a loop over missing files reports a pass while
    // asserting nothing.
    let math = std::path::Path::new("/System/Library/Fonts/Supplemental/STIXTwoMath.otf");
    assert!(math.exists(), "{math:?} is expected on macOS but is missing");
    assert!(
        parse_from_disk(math).is_math_font,
        "STIXTwoMath.otf carries a MATH table and must be detected as a math font"
    );

    // Negative cases. These paths move between macOS releases, and the
    // convention against silent skips is about a test quietly asserting
    // nothing — so the counter is explicit: whichever of these exist must all
    // report false, and at least one must have existed.
    let text_faces = [
        "/System/Library/Fonts/Supplemental/STIXTwoText.ttf",
        "/System/Library/Fonts/Apple Symbols.ttf",
        "/System/Library/Fonts/Menlo.ttc",
    ];
    let mut checked = 0;
    for path in text_faces {
        let path = std::path::Path::new(path);
        if !path.exists() {
            continue;
        }
        checked += 1;
        assert!(
            !parse_from_disk(path).is_math_font,
            "{path:?} carries no MATH table and must not be detected as a math font"
        );
    }
    assert!(checked > 0, "no macOS text face was found, so the negative direction went untested");
}
