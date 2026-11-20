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
