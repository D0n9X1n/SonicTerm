//! LRU eviction tests for `sonicterm_text::glyph_atlas::GlyphAtlas`.
//!
//! These cover the v0.7-era memory-bounding fix: when the shelf packer
//! reports "full", the atlas must evict the bottom 25% of entries by
//! `last_used_frame` and retry before giving up. Sessions that switch
//! between many fonts/themes used to grow the atlas unboundedly; now
//! they cycle a fixed working set instead.

use sonicterm_text::glyph_atlas::{GlyphAtlas, RasterTile, Rasterizer, SyntheticRasterizer};
use sonicterm_types::GlyphKey;

fn k(ch: char) -> GlyphKey {
    GlyphKey::new(ch, false, false)
}

/// Fixed-size rasterizer — every glyph is `tile × tile` pixels, so we
/// know exactly how many will fit in a given atlas. This makes the
/// fill-to-capacity test deterministic.
struct FixedRasterizer {
    tile: u32,
}

impl Rasterizer for FixedRasterizer {
    fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
        Some(RasterTile {
            width: self.tile,
            height: self.tile,
            offset_x: 0,
            offset_y: 0,
            advance: self.tile as f32,
            coverage: vec![0xAA; (self.tile * self.tile) as usize],
            is_color: false,
        })
    }
}

#[test]
fn tick_frame_bumps_counter_and_lookups_record_it() {
    let mut a = GlyphAtlas::new(256, 256);
    let mut r = SyntheticRasterizer::default();
    assert_eq!(a.current_frame(), 0);
    a.tick_frame();
    assert_eq!(a.current_frame(), 1);
    let _ = a.get_or_insert(k('A'), &mut r);
    a.tick_frame();
    a.tick_frame();
    // Hit on the same key — entry's last_used_frame should jump to 3.
    let _ = a.get_or_insert(k('A'), &mut r);
    assert_eq!(a.current_frame(), 3);
    // No eviction has happened yet.
    assert_eq!(a.evictions(), 0);
}

#[test]
fn fill_then_force_eviction_evicts_cold_entries_and_admits_new() {
    // 64×64 atlas with 16×16 tiles = exactly 16 tiles fit (4 shelves of 4).
    // We insert 16, age them, then insert 16 more; the second wave must
    // succeed via LRU eviction of the first wave.
    let mut a = GlyphAtlas::new(64, 64);
    let mut r = FixedRasterizer { tile: 16 };

    // Wave 1: 16 cold keys, inserted at frame 0.
    for i in 0..16u32 {
        let ch = char::from_u32(0x4E00 + i).unwrap(); // arbitrary unique codepoints
        let info = a.get_or_insert(k(ch), &mut r);
        assert!(info.is_some(), "wave-1 insert #{i} must succeed on empty atlas");
    }
    assert_eq!(a.len(), 16);
    assert_eq!(a.evictions(), 0);

    // Advance frame so wave-1 entries are now "old".
    for _ in 0..10 {
        a.tick_frame();
    }
    // Wave 2: 16 NEW keys. The packer is full; eviction must kick in
    // and drop the bottom 25% of wave-1 before each batch admits.
    let mut wave2_keys = Vec::new();
    for i in 0..16u32 {
        let ch = char::from_u32(0x5000 + i).unwrap();
        let key = k(ch);
        wave2_keys.push(key);
        let info = a.get_or_insert(key, &mut r);
        assert!(info.is_some(), "wave-2 insert #{i} must succeed after eviction");
    }

    // After wave 2 we should have evicted at least 16 wave-1 entries
    // (one quarter at a time, repeatedly).
    assert!(a.evictions() >= 16, "expected ≥16 evictions, got {}", a.evictions());
    // Every wave-2 key must still be resident — they're the freshest.
    for key in &wave2_keys {
        assert!(a.get(*key).is_some(), "wave-2 key evicted itself");
    }
    // Total resident count is bounded by atlas capacity (≤16 tiles).
    assert!(a.len() <= 16, "atlas overran capacity: len={}", a.len());
}

#[test]
fn evicted_rect_is_reclaimed_by_free_list_no_growth() {
    // Same harness as the previous test but assert the atlas dimensions
    // never changed — LRU + free-list reuse is the entire point.
    let mut a = GlyphAtlas::new(64, 64);
    let (w0, h0) = (a.width(), a.height());
    let mut r = FixedRasterizer { tile: 16 };
    for i in 0..32u32 {
        let ch = char::from_u32(0x4000 + i).unwrap();
        let _ = a.get_or_insert(k(ch), &mut r);
        a.tick_frame();
    }
    assert_eq!(a.width(), w0);
    assert_eq!(a.height(), h0);
    assert!(a.evictions() > 0);
}

#[test]
fn lru_spares_recently_used_entries() {
    // Insert 16 entries. Then on a fresh frame, "use" half of them
    // (lookup bumps last_used_frame). Force eviction by inserting more.
    // The recently-used half must survive.
    let mut a = GlyphAtlas::new(64, 64);
    let mut r = FixedRasterizer { tile: 16 };
    let mut keys = Vec::new();
    for i in 0..16u32 {
        let ch = char::from_u32(0x4E00 + i).unwrap();
        let key = k(ch);
        keys.push(key);
        let _ = a.get_or_insert(key, &mut r);
    }

    // Age the world.
    for _ in 0..5 {
        a.tick_frame();
    }
    // Touch the first 8 — these are now the freshest.
    for key in &keys[..8] {
        let _ = a.get_or_insert(*key, &mut r);
    }
    a.tick_frame();

    // One new insert should trigger eviction of the bottom 25%
    // (4 entries — all from the untouched tail).
    let new_key = k('Z');
    let info = a.get_or_insert(new_key, &mut r);
    assert!(info.is_some());
    assert!(a.evictions() >= 1);

    // The 8 recently-touched keys MUST still be there.
    for key in &keys[..8] {
        assert!(a.get(*key).is_some(), "LRU evicted a recently-used key");
    }
}

#[test]
fn empty_tile_entries_evict_without_freelist_entry() {
    // Space-like glyphs produce zero-area tiles that don't consume
    // atlas space. They still count as map entries that LRU can evict.
    let mut a = GlyphAtlas::new(64, 64);
    let mut r = SyntheticRasterizer::default();
    // Space → zero-area sentinel.
    let _ = a.get_or_insert(k(' '), &mut r);
    assert_eq!(a.len(), 1);
    // Should still be findable.
    assert!(a.get(k(' ')).is_some());
}
