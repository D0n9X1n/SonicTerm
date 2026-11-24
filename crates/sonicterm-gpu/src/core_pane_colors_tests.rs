use super::*;
use sonicterm_types::{ColorOverrides, DynamicColor};

/// Program colors distinct from every default theme color: foreground, background, cursor and
/// palette entries 1 and 200.
fn program_colors() -> ColorOverrides {
    let mut overrides = ColorOverrides::default();
    overrides.set_dynamic(DynamicColor::Foreground, Some((1, 2, 3)));
    overrides.set_dynamic(DynamicColor::Background, Some((40, 50, 60)));
    overrides.set_dynamic(DynamicColor::Cursor, Some((7, 8, 9)));
    overrides.set_palette(1, Some((100, 110, 120)));
    overrides.set_palette(200, Some((130, 140, 150)));
    overrides
}

/// A pane without program colors resolves every path exactly as the theme does: the same cell
/// colors, no pane fill, and the cursor and cache revision passed through unchanged.
#[test]
fn a_pane_without_program_colors_draws_the_theme() {
    let theme = Theme::default();
    let colors = PaneColors::new(&theme, None);
    let theme_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
    let theme_bg = hex_to_chrome_color(theme.colors.background.0.as_str());
    assert_eq!(colors.foreground(), theme_fg);
    assert_eq!(colors.background(), theme_bg);
    assert_eq!(colors.default_foreground(ChromeColor::WHITE), ChromeColor::WHITE);
    for index in [0u8, 1, 15, 16, 200, 255] {
        assert_eq!(colors.indexed(index), indexed(index, &theme), "{index}");
    }
    let inverse = Cell::plain('x', Color::Default, Color::Default, CellFlags::INVERSE);
    assert_eq!(cell_fg(&inverse, colors, theme_fg), theme_bg);
    assert_eq!(cell_bg_rgba(&inverse, colors), Some(chrome_color_to_linear_rgba(theme_fg)));
    assert_eq!(colors.background_fill(1.0), None);
    assert_eq!(colors.cursor_rgba([0.5; 4]), [0.5; 4]);
    assert_eq!(colors.style_rev(42), 42);
    assert_eq!(pane_colors_identity([(7, None)]), 0);
}

/// Every cell color path reads the program colors: default foreground, inverse in both
/// directions, dim, palette foreground and background, decorations and the cursor; a palette
/// entry the program did not set keeps the theme.
#[test]
fn every_cell_color_path_follows_the_program_colors() {
    let theme = Theme::default();
    let overrides = program_colors();
    let colors = PaneColors::new(&theme, Some(&overrides));
    let program_fg = ChromeColor::rgb(1, 2, 3);
    let program_bg = ChromeColor::rgb(40, 50, 60);
    assert_eq!(colors.default_foreground(ChromeColor::WHITE), program_fg);
    let plain = Cell::plain('x', Color::Default, Color::Default, CellFlags::empty());
    assert_eq!(cell_fg(&plain, colors, program_fg), program_fg);
    assert_eq!(cell_bg_rgba(&plain, colors), None, "a default background shows the pane fill");
    let inverse = Cell::plain('x', Color::Default, Color::Default, CellFlags::INVERSE);
    assert_eq!(cell_fg(&inverse, colors, program_fg), program_bg);
    assert_eq!(cell_bg_rgba(&inverse, colors), Some(chrome_color_to_linear_rgba(program_fg)));
    let palette = Cell::plain('x', Color::Indexed(1), Color::Indexed(200), CellFlags::empty());
    assert_eq!(cell_fg(&palette, colors, program_fg), ChromeColor::rgb(100, 110, 120));
    assert_eq!(
        cell_bg_rgba(&palette, colors),
        Some(chrome_color_to_linear_rgba(ChromeColor::rgb(130, 140, 150)))
    );
    let unset = Cell::plain('x', Color::Indexed(2), Color::Default, CellFlags::empty());
    assert_eq!(cell_fg(&unset, colors, program_fg), indexed(2, &theme).unwrap());
    let dim = Cell::plain('x', Color::Default, Color::Default, CellFlags::DIM);
    assert_eq!(
        cell_fg(&dim, colors, program_fg),
        dim_toward(program_fg, program_bg, row_shaping::DIM_BLEND)
    );
    assert_eq!(
        color_to_chrome(Color::Indexed(1), colors, program_fg),
        ChromeColor::rgb(100, 110, 120)
    );
    assert_eq!(
        colors.cursor_rgba([0.5; 4]),
        chrome_color_to_linear_rgba(ChromeColor::rgb(7, 8, 9))
    );
}

/// A program background becomes one pane fill at the window's background opacity, premultiplied
/// as the theme clear is, so at full opacity it stands exactly where the theme clear would.
#[test]
fn a_program_background_fills_the_pane_at_the_theme_opacity() {
    let theme = Theme::default();
    let overrides = program_colors();
    let colors = PaneColors::new(&theme, Some(&overrides));
    assert_eq!(colors.background_fill(1.0), Some(hex_to_premultiplied_rgba("#28323c", 1.0)));
    assert_eq!(colors.background_fill(0.5), Some(hex_to_premultiplied_rgba("#28323c", 0.5)));
}

/// A pane's row-cache revision and the frame key's color identity change with every override
/// change and differ between panes, so cached rows never replay another state's colors and a
/// color change repaints the whole window.
#[test]
fn caches_and_the_frame_key_follow_the_override_generation() {
    let theme = Theme::default();
    let mut overrides = program_colors();
    let first_rev = PaneColors::new(&theme, Some(&overrides)).style_rev(42);
    let first_identity = pane_colors_identity([(7, Some(&overrides))]);
    assert_ne!(first_rev, 42);
    assert_ne!(first_identity, 0);
    assert_ne!(pane_colors_identity([(8, Some(&overrides))]), first_identity);
    overrides.set_palette(1, Some((0, 0, 0)));
    assert_ne!(PaneColors::new(&theme, Some(&overrides)).style_rev(42), first_rev);
    assert_ne!(pane_colors_identity([(7, Some(&overrides))]), first_identity);
    let changed = WindowIdentity { pane_colors: first_identity, ..Default::default() };
    assert!(changed.classify(&WindowIdentity::default()).full, "a color change repaints all");
}

/// The selection and cursor-text colors: without program colors the selection highlight and the
/// cursor text pass the theme's through and there is no selected-glyph color, so existing themes
/// look unchanged; a program's OSC 17 takes the theme highlight's opacity, and its OSC 19 and
/// OSC 21 `cursor_text` become the selected-glyph and cursor-text colors.
#[test]
fn selection_and_cursor_text_follow_the_program_colors() {
    let theme = Theme::default();
    let highlight = [0.25, 0.5, 0.125, 0.5];
    let cursor_text = [0.1, 0.2, 0.3, 1.0];
    let plain = PaneColors::new(&theme, None);
    assert_eq!(plain.selection_rgba(highlight), highlight);
    assert_eq!(plain.selection_foreground_rgba(), None);
    assert_eq!(plain.cursor_text_rgba(cursor_text), cursor_text);
    let unrelated = program_colors();
    let only_others = PaneColors::new(&theme, Some(&unrelated));
    assert_eq!(only_others.selection_rgba(highlight), highlight, "OSC 10-12 leave it alone");
    assert_eq!(only_others.selection_foreground_rgba(), None);
    let mut overrides = ColorOverrides::default();
    overrides.set_dynamic(DynamicColor::SelectionBackground, Some((255, 0, 0)));
    overrides.set_dynamic(DynamicColor::SelectionForeground, Some((0, 255, 0)));
    overrides.set_dynamic(DynamicColor::CursorText, Some((0, 0, 255)));
    let program = PaneColors::new(&theme, Some(&overrides));
    assert_eq!(program.selection_rgba(highlight), [0.5, 0.0, 0.0, 0.5]);
    assert_eq!(program.selection_foreground_rgba(), Some([0.0, 1.0, 0.0, 1.0]));
    assert_eq!(program.cursor_text_rgba(cursor_text), [0.0, 0.0, 1.0, 1.0]);
}
