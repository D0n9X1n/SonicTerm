use super::*;

/// Each scheme names its DSR 996 report: 1 for dark, 2 for light.
#[test]
fn schemes_report_their_dsr_996_value() {
    assert_eq!(ColorScheme::Dark.report(), b"\x1b[?997;1n");
    assert_eq!(ColorScheme::Light.report(), b"\x1b[?997;2n");
}

/// The theme owns 0..=15, the cube spans 16..=231 at xterm's levels (0, then 55 + 40 per step),
/// and the gray ramp 232..=255 runs from 8 in steps of 10: the values xterm and the renderer use.
#[test]
fn indexed_palette_matches_xterm_at_every_boundary() {
    assert_eq!(indexed_rgb(0), None);
    assert_eq!(indexed_rgb(15), None);
    assert_eq!(indexed_rgb(16), Some((0, 0, 0)));
    assert_eq!(indexed_rgb(17), Some((0, 0, 95)));
    assert_eq!(indexed_rgb(21), Some((0, 0, 255)));
    assert_eq!(indexed_rgb(196), Some((255, 0, 0)));
    assert_eq!(indexed_rgb(231), Some((255, 255, 255)));
    assert_eq!(indexed_rgb(232), Some((8, 8, 8)));
    assert_eq!(indexed_rgb(255), Some((238, 238, 238)));
}

/// DECSCUSR 0 restores the configured cursor; odd values blink and even ones are steady for the
/// block, underline and bar; a value past 6 changes nothing.
#[test]
fn decscusr_values_name_their_request() {
    assert_eq!(CursorRequest::from_decscusr(0), Some(None));
    let expected = [
        (1, CursorRequestShape::Block, false),
        (2, CursorRequestShape::Block, true),
        (3, CursorRequestShape::Underline, false),
        (4, CursorRequestShape::Underline, true),
        (5, CursorRequestShape::Bar, false),
        (6, CursorRequestShape::Bar, true),
    ];
    for (value, shape, steady) in expected {
        assert_eq!(
            CursorRequest::from_decscusr(value),
            Some(Some(CursorRequest { shape, steady }))
        );
    }
    assert_eq!(CursorRequest::from_decscusr(7), None);
}

/// A theme's `#rrggbb` reads each channel pair on its own: a pair that is not hex reads 0 and the
/// others keep their values, and a value that is not six ASCII characters after its leading `#`s
/// reads black. The renderer and the parser's seeded defaults both read through it, so a
/// program's color query always answers the drawn color.
#[test]
fn theme_hex_reads_each_channel_leniently() {
    assert_eq!(theme_hex_rgb("#e04020"), (0xe0, 0x40, 0x20));
    assert_eq!(theme_hex_rgb("e04020"), (0xe0, 0x40, 0x20));
    assert_eq!(theme_hex_rgb("#zz4020"), (0, 0x40, 0x20));
    assert_eq!(theme_hex_rgb("#e0zz20"), (0xe0, 0, 0x20));
    assert_eq!(theme_hex_rgb("#abc"), (0, 0, 0));
    assert_eq!(theme_hex_rgb("#0é000"), (0, 0, 0));
    assert_eq!(theme_hex_rgb(""), (0, 0, 0));
}
