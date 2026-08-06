//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use std::collections::HashSet;

use crate::{Cell, CellFlags, Color, GlyphKey, GlyphRasterVariant};

#[test]
fn exports_core_cell_contracts() {
    let cell = Cell::plain('A', Color::Rgb(1, 2, 3), Color::Default, CellFlags::BOLD);
    assert_eq!(cell.ch, 'A');
    assert_eq!(cell.fg, Color::Rgb(1, 2, 3));
    assert!(cell.flags.contains(CellFlags::BOLD));
}

#[test]
fn glyph_keys_default_to_normal_raster_identity() {
    let cell = Cell::plain('A', Color::Default, Color::Default, CellFlags::empty());

    assert_eq!(GlyphKey::from_cell(&cell).unwrap().raster_variant, GlyphRasterVariant::Normal);
    assert_eq!(GlyphKey::new('A', false, false).raster_variant, GlyphRasterVariant::Normal);
    assert_eq!(
        GlyphKey::with_slot('A', 2, false, false).raster_variant,
        GlyphRasterVariant::Normal
    );
    assert_eq!(
        GlyphKey::shaped('A', 2, 17, false, false).raster_variant,
        GlyphRasterVariant::Normal
    );
}

#[test]
fn glyph_raster_variant_separates_otherwise_identical_atlas_keys() {
    let normal = GlyphKey::shaped('A', 0, 17, false, false);
    let footer = normal.with_raster_variant(GlyphRasterVariant::PaletteFooter);
    let title = normal.with_raster_variant(GlyphRasterVariant::TabTitle);
    let keys = HashSet::from([normal, footer, title]);

    assert_eq!(keys.len(), 3);
}
