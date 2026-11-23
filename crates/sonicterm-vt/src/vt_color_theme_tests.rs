//! How program color overrides meet the theme: reseeds write defaults only, an override masks a
//! later theme change, RIS clears the palette only, and no program change reports mode 2031.

use super::vt_color_tests::{dynamic_override, palette_override, reply_to, themed_parser};
use super::*;
use sonicterm_types::{ColorScheme, DynamicColor};

/// Seed a whole theme the way the host does on reload, preview, cancel and pick: defaults
/// only, then end the update.
fn reseed(parser: &mut Parser, foreground: u8, scheme: ColorScheme) -> Option<Vec<u8>> {
    parser.set_theme_fg(foreground, foreground, foreground);
    for index in 0..16u8 {
        parser.set_theme_palette_color(index, foreground, index, index);
    }
    parser.set_theme_color_scheme(scheme);
    parser.finish_theme_update()
}

/// A reseed (reload, picker preview, cancel or pick) writes defaults only: every override stays
/// and keeps masking the changed default, while an unoverridden entry follows the new theme.
#[test]
fn a_theme_reseed_keeps_every_override() {
    let mut parser = themed_parser();
    let _ = reply_to(&mut parser, b"\x1b]10;#010101\x07\x1b]4;1;#020202\x07");
    let _ = reseed(&mut parser, 99, ColorScheme::Light);
    assert_eq!(dynamic_override(&parser, DynamicColor::Foreground), Some((1, 1, 1)));
    assert_eq!(palette_override(&parser, 1), Some((2, 2, 2)));
    assert_eq!(
        reply_to(&mut parser, b"\x1b]10;?\x07\x1b]4;1;?\x07\x1b]4;2;?\x07"),
        b"\x1b]10;rgb:0101/0101/0101\x07\x1b]4;1;rgb:0202/0202/0202\x07\x1b]4;2;rgb:6363/0202/0202\x07"
    );
}

/// An override equal to the current default is still an override: a later theme change does not
/// show through it.
#[test]
fn an_override_equal_to_the_default_masks_a_later_theme_change() {
    let mut parser = themed_parser();
    let _ = reply_to(&mut parser, b"\x1b]10;#c8c8c8\x07");
    let _ = reseed(&mut parser, 50, ColorScheme::Dark);
    assert_eq!(reply_to(&mut parser, b"\x1b]10;?\x07"), b"\x1b]10;rgb:c8c8/c8c8/c8c8\x07");
}

/// A program set or reset never produces a mode 2031 report, and the next theme update compares
/// theme defaults only, so a program override is not a palette change to report.
#[test]
fn a_program_color_change_reports_nothing_under_mode_2031() {
    let mut parser = themed_parser();
    parser.advance(b"\x1b[?2031h");
    let replies =
        reply_to(&mut parser, b"\x1b]11;#ffffff\x07\x1b]4;1;#fff\x07\x1b]111\x07\x1b]104\x07");
    assert_eq!(replies, b"");
    let _ = reply_to(&mut parser, b"\x1b]11;#ffffff\x07");
    assert_eq!(parser.finish_theme_update(), None, "the theme defaults did not change");
}

/// RIS clears palette overrides only: the foreground, background and cursor overrides stay, mode
/// 2031 is reset, and nothing is reported for the reset or for a theme change after it.
#[test]
fn ris_clears_palette_overrides_only_and_reports_nothing() {
    let mut parser = themed_parser();
    parser.advance(b"\x1b[?2031h");
    let _ = reply_to(
        &mut parser,
        b"\x1b]4;1;#010101\x07\x1b]10;#020202\x07\x1b]11;#030303\x07\x1b]12;#040404\x07",
    );
    assert_eq!(reply_to(&mut parser, b"\x1bc"), b"");
    assert_eq!(palette_override(&parser, 1), None);
    assert_eq!(dynamic_override(&parser, DynamicColor::Foreground), Some((2, 2, 2)));
    assert_eq!(dynamic_override(&parser, DynamicColor::Background), Some((3, 3, 3)));
    assert_eq!(dynamic_override(&parser, DynamicColor::Cursor), Some((4, 4, 4)));
    assert_eq!(reply_to(&mut parser, b"\x1b[?2031$p"), b"\x1b[?2031;2$y");
    assert_eq!(reseed(&mut parser, 77, ColorScheme::Light), None, "mode 2031 is off after RIS");
}

/// DSR 996 keeps answering the theme's scheme, whatever background the program set.
#[test]
fn dsr_996_answers_the_theme_scheme_over_an_override() {
    let mut parser = themed_parser();
    parser.set_theme_color_scheme(ColorScheme::Dark);
    let _ = reply_to(&mut parser, b"\x1b]11;#ffffff\x07");
    assert_eq!(reply_to(&mut parser, b"\x1b[?996n"), b"\x1b[?997;1n");
}
