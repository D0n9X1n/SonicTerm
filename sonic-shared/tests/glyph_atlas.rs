//! Tests for `sonic_shared::glyph_atlas::GlyphAtlas`.

use sonic_core::glyph_key::GlyphKey;
use sonic_shared::glyph_atlas::{
    GlyphAtlas, Rasterizer, ShelfPacker, SyntheticRasterizer, ATLAS_DIM,
};

fn k(ch: char) -> GlyphKey {
    GlyphKey::new(ch, false, false)
}

#[test]
fn fresh_atlas_is_empty() {
    let a = GlyphAtlas::default_size();
    assert!(a.is_empty());
    assert_eq!(a.len(), 0);
    assert_eq!(a.hits(), 0);
    assert_eq!(a.misses(), 0);
    assert_eq!(a.width(), ATLAS_DIM);
    assert_eq!(a.height(), ATLAS_DIM);
    assert_eq!(a.hit_rate_pct(), 0.0);
}

#[test]
fn first_lookup_is_a_miss_second_is_a_hit() {
    let mut a = GlyphAtlas::default_size();
    let mut r = SyntheticRasterizer::default();
    let i1 = a.get_or_insert(k('A'), &mut r).unwrap();
    let i2 = a.get_or_insert(k('A'), &mut r).unwrap();
    assert_eq!(i1, i2);
    assert_eq!(a.misses(), 1);
    assert_eq!(a.hits(), 1);
    assert_eq!(r.calls, 1, "rasterizer must NOT be called on a hit");
    assert_eq!(a.len(), 1);
}

#[test]
fn distinct_keys_get_distinct_tiles() {
    let mut a = GlyphAtlas::default_size();
    let mut r = SyntheticRasterizer::default();
    let ia = a.get_or_insert(k('A'), &mut r).unwrap();
    let ib = a.get_or_insert(k('B'), &mut r).unwrap();
    assert_ne!(ia.uv, ib.uv, "A and B must not share a tile");
    assert_eq!(a.len(), 2);
}

#[test]
fn bold_and_plain_of_same_char_get_distinct_tiles() {
    let mut a = GlyphAtlas::default_size();
    let mut r = SyntheticRasterizer::default();
    let plain = a.get_or_insert(GlyphKey::new('x', false, false), &mut r).unwrap();
    let bold = a.get_or_insert(GlyphKey::new('x', true, false), &mut r).unwrap();
    assert_ne!(plain.uv, bold.uv);
    assert_eq!(a.len(), 2);
}

#[test]
fn hit_rate_pct_reflects_traffic() {
    let mut a = GlyphAtlas::default_size();
    let mut r = SyntheticRasterizer::default();
    a.get_or_insert(k('A'), &mut r).unwrap(); // miss
    for _ in 0..99 {
        a.get_or_insert(k('A'), &mut r).unwrap(); // hit ×99
    }
    assert_eq!(a.hits(), 99);
    assert_eq!(a.misses(), 1);
    assert!((a.hit_rate_pct() - 99.0).abs() < 0.0001);
}

#[test]
fn space_yields_empty_uv_and_no_dirty_rect() {
    let mut a = GlyphAtlas::default_size();
    let mut r = SyntheticRasterizer::default();
    // Pre-flush dirty list.
    let _ = a.take_dirty_rects();
    let info = a.get_or_insert(k(' '), &mut r).unwrap();
    assert_eq!(info.uv, [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(info.px_size, [0, 0]);
    assert!(a.take_dirty_rects().is_empty(), "empty tile must not produce a dirty rect");
}

#[test]
fn dirty_rects_record_uploaded_tiles_and_drain_to_empty() {
    let mut a = GlyphAtlas::default_size();
    let mut r = SyntheticRasterizer::default();
    a.get_or_insert(k('A'), &mut r).unwrap();
    a.get_or_insert(k('B'), &mut r).unwrap();
    let rects = a.take_dirty_rects();
    assert_eq!(rects.len(), 2);
    assert!(a.take_dirty_rects().is_empty(), "second take must be empty");
}

#[test]
fn rasterized_pixels_actually_land_in_the_atlas_buffer() {
    let mut a = GlyphAtlas::default_size();
    let mut r = SyntheticRasterizer::default();
    let info = a.get_or_insert(k('M'), &mut r).unwrap();
    // Sample the top-left pixel of the tile (UV → x/y).
    let x = (info.uv[0] * a.width() as f32).round() as u32;
    let y = (info.uv[1] * a.height() as f32).round() as u32;
    // Synthetic rasterizer fills with `(x+y)*11`; (0,0) -> 0. Check a
    // non-zero offset instead so we're sure we hit a written pixel.
    assert_ne!(a.sample(x + 1, y + 1), 0, "rasterized pixels must land in the atlas");
}

#[test]
fn shelf_packer_handles_many_glyphs_without_collision() {
    let mut a = GlyphAtlas::default_size();
    let mut r = SyntheticRasterizer::default();
    // ASCII printables: 95 unique glyphs. All must fit, none alias.
    let mut uvs = Vec::with_capacity(95);
    for c in '!'..='~' {
        let info = a.get_or_insert(k(c), &mut r).unwrap();
        uvs.push(info.uv);
    }
    let unique: std::collections::HashSet<[u32; 4]> = uvs
        .iter()
        .map(|u| [u[0].to_bits(), u[1].to_bits(), u[2].to_bits(), u[3].to_bits()])
        .collect();
    assert_eq!(unique.len(), uvs.len(), "no two glyphs may share a UV rect");
}

#[test]
fn get_without_insert_returns_none_for_missing_key() {
    let a = GlyphAtlas::default_size();
    assert!(a.get(k('Z')).is_none());
}

#[test]
fn second_get_after_insert_does_not_call_rasterizer() {
    let mut a = GlyphAtlas::default_size();
    let mut r = SyntheticRasterizer::default();
    a.get_or_insert(k('y'), &mut r).unwrap();
    assert_eq!(r.calls, 1);
    let info = a.get(k('y')).unwrap();
    assert_eq!(info.advance, 8.0 + (('y' as u32 % 8) as f32));
    assert_eq!(r.calls, 1, "non-mutating get must not rasterize");
}

#[test]
fn atlas_full_returns_none_gracefully() {
    // Tiny atlas: 16x16. Synthetic tiles are >= 8x8. ~4 tiles max.
    let mut a = GlyphAtlas::new(16, 16);
    struct Fixed;
    impl Rasterizer for Fixed {
        fn rasterize(&mut self, _: GlyphKey) -> Option<sonic_shared::glyph_atlas::RasterTile> {
            Some(sonic_shared::glyph_atlas::RasterTile {
                width: 12,
                height: 12,
                offset_x: 0,
                offset_y: 0,
                advance: 12.0,
                coverage: vec![255; 144],
            })
        }
    }
    let mut r = Fixed;
    let mut filled = 0;
    for c in 'a'..='z' {
        if a.get_or_insert(k(c), &mut r).is_some() {
            filled += 1;
        } else {
            break;
        }
    }
    // At least 1 should fit, but not all 26 in 16×16.
    assert!((1..26).contains(&filled));
}

#[test]
fn uv_rect_is_normalized_within_atlas() {
    let mut a = GlyphAtlas::default_size();
    let mut r = SyntheticRasterizer::default();
    let info = a.get_or_insert(k('Q'), &mut r).unwrap();
    for c in info.uv {
        assert!((0.0..=1.0).contains(&c), "UV {c} out of [0,1]");
    }
    assert!(info.uv[0] < info.uv[2], "u0 < u1");
    assert!(info.uv[1] < info.uv[3], "v0 < v1");
}

#[test]
fn packer_no_overlap_at_scale() {
    // Stress test the shelf packer with >1000 tiles and verify every
    // returned rect is disjoint from every previous one.
    let mut p = ShelfPacker::new(2048, 2048);
    let mut placed: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut rng = 17u32;
    let mut placed_count = 0;
    for _ in 0..1500 {
        // simple LCG for reproducible "varied" sizes 8..32
        rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
        let w = 8 + (rng / 65536) % 24;
        rng = rng.wrapping_mul(1103515245).wrapping_add(12345);
        let h = 12 + (rng / 65536) % 20;
        if let Some((x, y)) = p.alloc(w, h) {
            // every previously placed rect must be disjoint
            for (px, py, pw, ph) in &placed {
                let disjoint = x + w <= *px || *px + pw <= x || y + h <= *py || *py + ph <= y;
                assert!(disjoint, "overlap: new ({x},{y},{w},{h}) vs ({px},{py},{pw},{ph})");
            }
            placed.push((x, y, w, h));
            placed_count += 1;
        }
    }
    assert!(placed_count >= 1000, "expected to fit ≥1000 tiles in 2048², got {placed_count}");
}

#[test]
fn packer_failure_does_not_corrupt_state() {
    // Regression for Haiku PR #35 review: alloc() used to mutate shelf_y
    // before the vertical-bounds check, so a too-tall tile would leave
    // the packer in an overfull-shelf state and subsequent smaller tiles
    // that DID fit the original shelf would also fail.
    let mut p = ShelfPacker::new(128, 64);
    // First, place a small tile to seed shelf_h.
    let (x0, y0) = p.alloc(10, 20).expect("first alloc");
    assert_eq!((x0, y0), (0, 0));
    // Now try to place a 20×80 — taller than the entire atlas height.
    // Must return None.
    assert_eq!(p.alloc(20, 80), None);
    // After the failure, a tile that fits the current shelf must still
    // succeed (and not be displaced by a corrupted shelf_y/shelf_h).
    let next = p.alloc(10, 20).expect("alloc after failure must succeed");
    assert!(next.0 > 0 || next.1 == 0, "next tile should land on the current shelf");
}
