//! Renderer theme contracts, sharing one root fixture owner.

use super::*;

// --- Dim / faint text (SGR 2), -------------------------------------

#[test]
fn dim_toward_endpoints_and_midpoint() {
    let fg = ChromeColor::rgba(200, 100, 40, 255);
    let bg = ChromeColor::rgb(0, 0, 0);
    // t = 0 → unchanged fg.
    assert_eq!(dim_toward(fg, bg, 0.0), fg);
    // t = 1 → exactly bg (but fg's alpha preserved).
    let full = dim_toward(fg, bg, 1.0);
    assert_eq!((full.r(), full.g(), full.b()), (0, 0, 0));
    assert_eq!(full.a(), 255, "alpha is preserved, not blended");
    // t = 0.5 → halfway each channel.
    let mid = dim_toward(fg, bg, 0.5);
    assert_eq!((mid.r(), mid.g(), mid.b()), (100, 50, 20));
}

#[test]
fn dim_toward_clamps_factor_and_preserves_alpha() {
    let fg = ChromeColor::rgba(255, 255, 255, 128);
    let bg = ChromeColor::rgb(0, 0, 0);
    // Out-of-range t is clamped (no panic, no overshoot).
    assert_eq!(dim_toward(fg, bg, -1.0), fg, "t<0 clamps to 0 → unchanged");
    let over = dim_toward(fg, bg, 2.0);
    assert_eq!((over.r(), over.g(), over.b()), (0, 0, 0), "t>1 clamps to 1 → bg");
    assert_eq!(over.a(), 128, "alpha untouched");
}

#[test]
fn cell_fg_dims_faint_text_toward_background() {
    let theme = Theme::default();
    let default = ChromeColor::rgb(255, 255, 255);
    let fg = Color::Rgb(200, 200, 200);
    let bg = Color::Rgb(0, 0, 0);

    let normal = Cell::plain('x', fg, bg, CellFlags::empty());
    let faint = Cell::plain('x', fg, bg, CellFlags::DIM);

    let normal_c = cell_fg(&normal, &theme, default);
    let faint_c = cell_fg(&faint, &theme, default);

    // Regression: the faint cell must NOT equal the normal cell,
    // and must be strictly dimmer on every channel (closer to the black bg).
    assert_ne!(faint_c, normal_c, "dim text must differ from normal text");
    assert!(faint_c.r() < normal_c.r(), "dim R should be lower");
    assert!(faint_c.g() < normal_c.g(), "dim G should be lower");
    assert!(faint_c.b() < normal_c.b(), "dim B should be lower");
}

#[test]
fn cell_fg_leaves_normal_text_unchanged() {
    let theme = Theme::default();
    let default = ChromeColor::rgb(255, 255, 255);
    let fg = Color::Rgb(123, 200, 50);
    let cell = Cell::plain('x', fg, Color::Default, CellFlags::empty());
    // No DIM → exactly the resolved fg, no blending.
    assert_eq!(cell_fg(&cell, &theme, default), ChromeColor::rgb(123, 200, 50));
}

#[test]
fn cell_fg_dim_with_inverse_dims_swapped_foreground() {
    let theme = Theme::default();
    let default = ChromeColor::rgb(255, 255, 255);
    // INVERSE: the glyph is painted in the cell's bg color over the fg.
    let fg = Color::Rgb(0, 0, 0);
    let bg = Color::Rgb(200, 200, 200);

    let inverse = Cell::plain('x', fg, bg, CellFlags::INVERSE);
    let inverse_dim = Cell::plain('x', fg, bg, CellFlags::INVERSE | CellFlags::DIM);

    let inv_c = cell_fg(&inverse, &theme, default);
    let inv_dim_c = cell_fg(&inverse_dim, &theme, default);

    // Inverse foreground resolves to the cell bg (200,200,200); DIM then
    // pulls it toward the swapped background (the cell fg = black), so each
    // channel must drop.
    assert_eq!(inv_c, ChromeColor::rgb(200, 200, 200));
    assert_ne!(inv_dim_c, inv_c, "inverse+dim must still dim");
    assert!(inv_dim_c.r() < inv_c.r() && inv_dim_c.g() < inv_c.g() && inv_dim_c.b() < inv_c.b());
}

#[test]
fn cursor_color_uses_theme_cursor_accent() {
    let theme = Theme::default();
    assert_eq!(
        cursor_color_from_theme(&theme),
        hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 1.0)
    );
    assert_eq!(theme.colors.cursor, theme.colors.tab.active_fg);
}

#[test]
fn cursor_text_color_uses_theme_cursor_text() {
    let theme = Theme::default();
    assert_eq!(
        cursor_text_color_from_theme(&theme),
        hex_to_premultiplied_rgba(theme.colors.cursor_text.0.as_str(), 1.0)
    );
    assert_ne!(
        cursor_text_color_from_theme(&theme),
        cursor_color_from_theme(&theme),
        "cursor text must not reuse the colored cursor accent"
    );
}

#[test]
fn cursor_color_preserves_theme_channels() {
    // Cursor shape and blink policy do not modify the configured theme channels.
    for color in [[1.0, 0.5, 0.0, 1.0], [0.0, 0.25, 0.5, 0.5]] {
        assert_eq!(active_cursor_color(color), color);
    }
}

#[test]
fn indexed_color_supports_full_xterm_256_palette() {
    let theme = Theme::default();
    assert_eq!(indexed(16, &theme), Some(ChromeColor::rgb(0, 0, 0)));
    assert_eq!(indexed(231, &theme), Some(ChromeColor::rgb(255, 255, 255)));
    assert_eq!(indexed(232, &theme), Some(ChromeColor::rgb(8, 8, 8)));
    assert_eq!(indexed(255, &theme), Some(ChromeColor::rgb(238, 238, 238)));
}
