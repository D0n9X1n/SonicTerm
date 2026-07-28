//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::glue::BgraPixel;

#[test]
fn exports_pixel_glue() {
    let px = BgraPixel::rgba(1, 2, 3, 4);
    assert_eq!(px, BgraPixel(3, 2, 1, 4));
    assert_eq!(px.a(), 4);
}

/// Block glyphs are handed to the atlas with `is_color: true`, which makes
/// the renderer skip the per-cell foreground and draw the tile's own colour.
/// A fully-opaque white texel therefore reaches the screen as pure white,
/// unlike monochrome glyph coverage which is tinted by the cell.
///
/// This scans rasterized block geometry for texels that are fully opaque
/// white while every 4-neighbour is fully transparent — an isolated dot with
/// no anti-aliasing falloff, which is the signature of the stray marks
/// reported against the terminal.
#[test]
fn block_glyph_geometry_does_not_leave_isolated_opaque_texels() {
    use crate::customglyph::{BlockKey, SizedBlockKey};
    use crate::glue::Size;

    // The reporter's cell is roughly 30x40 raster px at 2x scale.
    let size = Size::new(30, 40);

    // Braille and sextant patterns are the densest small-feature geometry
    // this crate draws, so they are where a one-texel spur is most likely.
    let mut keys: Vec<SizedBlockKey> = Vec::new();
    for bits in [0b0000_0001u8, 0b1000_0000, 0b0101_0101, 0b1111_1111] {
        keys.push(SizedBlockKey { block: BlockKey::Braille(bits), size });
        keys.push(SizedBlockKey { block: BlockKey::Sextant(bits), size });
        keys.push(SizedBlockKey { block: BlockKey::Octant(bits), size });
    }

    let mut isolated = Vec::new();
    for key in keys {
        let Ok(tile) = crate::block_sprite_with_cell_metrics(key, 2, true) else { continue };
        let (w, h) = (tile.width as usize, tile.height as usize);
        if w == 0 || h == 0 {
            continue;
        }
        let px = &tile.coverage;
        let alpha = |x: usize, y: usize| -> u8 { px[(y * w + x) * 4 + 3] };
        for y in 1..h.saturating_sub(1) {
            for x in 1..w.saturating_sub(1) {
                if alpha(x, y) != 255 {
                    continue;
                }
                let neighbours =
                    [alpha(x - 1, y), alpha(x + 1, y), alpha(x, y - 1), alpha(x, y + 1)];
                if neighbours.iter().all(|a| *a == 0) {
                    isolated.push((x, y));
                }
            }
        }
    }

    assert!(
        isolated.is_empty(),
        "block geometry produced {} fully-opaque texel(s) with no opaque neighbour, which \
         reach the screen as isolated pure-white dots because block tiles bypass the \
         per-cell foreground: {:?}",
        isolated.len(),
        &isolated[..isolated.len().min(8)]
    );
}
