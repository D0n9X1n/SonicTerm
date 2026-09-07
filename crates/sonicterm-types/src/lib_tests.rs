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
    // Contract: every legacy glyph-key constructor preserves normal raster identity by default.
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
    // Contract: raster roles participate in glyph identity and cannot alias one atlas entry.
    let normal = GlyphKey::shaped('A', 0, 17, false, false);
    let footer = normal.with_raster_variant(GlyphRasterVariant::PaletteFooter);
    let title = normal.with_raster_variant(GlyphRasterVariant::TabTitle);
    let keys = HashSet::from([normal, footer, title]);

    assert_eq!(keys.len(), 3);
}

/// The legacy type-erased painter seam remains implementable by compatibility callers.
#[test]
fn legacy_painter_contract_remains_implementable() {
    struct Frame;

    impl crate::traits::painter::FrameLike for Frame {
        fn cols(&self) -> u32 {
            80
        }

        fn rows(&self) -> u32 {
            24
        }
    }

    struct Recorder {
        size: (u32, u32),
    }

    impl crate::traits::painter::Painter for Recorder {
        fn paint_frame(
            &mut self,
            frame: &dyn crate::traits::painter::FrameLike,
        ) -> Result<(), crate::traits::painter::PaintError> {
            self.size = (frame.cols(), frame.rows());
            Ok(())
        }

        fn resize_surface(&mut self, width_px: u32, height_px: u32) {
            self.size = (width_px, height_px);
        }
    }

    let mut painter = Recorder { size: (0, 0) };
    crate::traits::painter::Painter::paint_frame(&mut painter, &Frame).unwrap();
    assert_eq!(painter.size, (80, 24));
    crate::traits::painter::Painter::resize_surface(&mut painter, 1280, 720);
    assert_eq!(painter.size, (1280, 720));
}
