//! Renderer preedit cache contracts, sharing one root fixture owner.

use super::*;

#[test]
fn preedit_cache_matches_only_on_identical_inputs_and_atlas_stamp() {
    // Preedit reuse requires identical input and qualified atlas identity to reject recycled UVs.
    let c = PreeditGlyphCache {
        text: "ni'hao".to_string(),
        font_size: 14.0,
        start_x: 100.0,
        top_y: 50.0,
        color_bits: 0xAABBCCFF,
        atlas_stamp: GlyphContentStamp {
            device_generation: 7,
            allocation_generation: 1,
            content_identity: 7,
            growths: 0,
        },
        glyphs: Vec::new(),
        missing_boxes: Vec::new(),
        missing_chrome_chars: Vec::new(),
    };
    // Exact match.
    let epoch = GlyphContentStamp {
        device_generation: 7,
        allocation_generation: 1,
        content_identity: 7,
        growths: 0,
    };
    assert!(c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, epoch));
    // Any single field differing must miss.
    assert!(!c.matches("ni'ha", 14.0, 100.0, 50.0, 0xAABBCCFF, epoch)); // text grew
    assert!(!c.matches("ni'hao", 15.0, 100.0, 50.0, 0xAABBCCFF, epoch)); // font size
    assert!(!c.matches("ni'hao", 14.0, 101.0, 50.0, 0xAABBCCFF, epoch)); // x (scroll)
    assert!(!c.matches("ni'hao", 14.0, 100.0, 51.0, 0xAABBCCFF, epoch)); // y
    assert!(!c.matches("ni'hao", 14.0, 100.0, 50.0, 0x11223344, epoch)); // color
    let evicted_epoch = GlyphContentStamp {
        device_generation: 7,
        allocation_generation: 1,
        content_identity: 8,
        growths: 0,
    };
    assert!(!c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, evicted_epoch));
}

#[test]
fn preedit_cache_rejects_same_content_identity_after_atlas_replacement() {
    // Equal local content identities cannot validate UVs from another allocation.
    let old_epoch = GlyphContentStamp {
        device_generation: 7,
        allocation_generation: 3,
        content_identity: 0,
        growths: 0,
    };
    let c = PreeditGlyphCache {
        text: "ni'hao".to_string(),
        font_size: 14.0,
        start_x: 100.0,
        top_y: 50.0,
        color_bits: 0xAABBCCFF,
        atlas_stamp: old_epoch,
        glyphs: Vec::new(),
        missing_boxes: Vec::new(),
        missing_chrome_chars: Vec::new(),
    };
    let replacement_epoch = GlyphContentStamp {
        device_generation: 7,
        allocation_generation: 4,
        content_identity: 0,
        growths: 0,
    };

    assert!(
        !c.matches("ni'hao", 14.0, 100.0, 50.0, 0xAABBCCFF, replacement_epoch),
        "equal content identities from different atlas allocations must not reuse cached UVs"
    );
}

#[test]
fn preedit_cache_rejects_reset_with_unchanged_evictions() {
    // The cache stamp independently rejects reset content even with fixed device/allocation qualifiers.
    let mut atlas = GlyphAtlas::new(2, 1);
    let capture = |atlas: &GlyphAtlas| GlyphContentStamp::capture(7, 3, atlas);
    let cache = PreeditGlyphCache {
        text: "preedit".to_string(),
        font_size: 14.0,
        start_x: 100.0,
        top_y: 50.0,
        color_bits: 0xAABBCCFF,
        atlas_stamp: capture(&atlas),
        glyphs: Vec::new(),
        missing_boxes: Vec::new(),
        missing_chrome_chars: Vec::new(),
    };
    assert!(cache.matches("preedit", 14.0, 100.0, 50.0, 0xAABBCCFF, capture(&atlas)));
    let evictions = atlas.evictions();
    let identity = atlas.identity();
    atlas.reset_in_place();
    assert_eq!(atlas.evictions(), evictions);
    assert_ne!(atlas.identity(), identity);
    assert!(!cache.matches("preedit", 14.0, 100.0, 50.0, 0xAABBCCFF, capture(&atlas)));
}
