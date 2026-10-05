//! Frame scratch release rules and its one-holder lease.

use super::*;

/// A zeroed glyph instance, the element the largest scratch vectors hold.
fn glyph() -> GlyphInstance {
    bytemuck::Zeroable::zeroed()
}

/// `count` zeroed glyph instances in a vector whose capacity is exactly `capacity`.
fn glyphs(capacity: usize, count: usize) -> Vec<GlyphInstance> {
    let mut held = Vec::with_capacity(capacity);
    held.resize(count, glyph());
    held
}

/// Bytes `held` reserves.
fn reserved_bytes<T>(held: &Vec<T>) -> usize {
    held.capacity() * std::mem::size_of::<T>()
}

const MIB: usize = 1024 * 1024;

#[test]
fn scratch_release_and_element_caps() {
    // A reused vector follows the vertex scratch's release rule (over four times the use and
    // over 1 MiB shrinks to twice the use; a pass with no use releases a large one) and then its
    // element cap: a vector above `cap_bytes` is replaced by one of exactly the cap's elements,
    // and a vector at the cap is kept as it is.
    let per_glyph = std::mem::size_of::<GlyphInstance>();
    let mut idle = glyphs(2 * MIB / per_glyph, 0);
    finish_vec(&mut idle, 0, 4 * MIB);
    assert_eq!(idle.capacity(), 0, "a pass with no use releases a vector over 1 MiB");
    let mut small = glyphs(MIB / 2 / per_glyph, 0);
    let small_capacity = small.capacity();
    finish_vec(&mut small, 0, 4 * MIB);
    assert_eq!(small.capacity(), small_capacity, "under the 1 MiB floor nothing is released");
    let mut oversized = glyphs(2 * MIB / per_glyph, 10);
    finish_vec(&mut oversized, 10, 4 * MIB);
    assert!(reserved_bytes(&oversized) < MIB, "four times over the use shrinks");
    assert!(oversized.capacity() >= 20, "to at least twice the use");
    assert!(oversized.is_empty(), "and the pass's contents are cleared");
    let mut busy = glyphs(2 * MIB / per_glyph, MIB / per_glyph);
    let busy_capacity = busy.capacity();
    finish_vec(&mut busy, MIB / per_glyph, 4 * MIB);
    assert_eq!(busy.capacity(), busy_capacity, "a vector at a quarter or more of its use stays");

    let mut wide = glyphs(6 * MIB / per_glyph, 3 * MIB / per_glyph);
    finish_vec(&mut wide, 3 * MIB / per_glyph, 4 * MIB);
    assert!(reserved_bytes(&wide) <= 4 * MIB, "glyphs at 6 MiB with 3 MiB used fall to the cap");
    let mut at_cap = glyphs(cap_elems::<GlyphInstance>(4 * MIB), cap_elems::<GlyphInstance>(4 * MIB));
    let pointer = at_cap.as_ptr();
    let at_cap_used = at_cap.len();
    finish_vec(&mut at_cap, at_cap_used, 4 * MIB);
    assert_eq!(at_cap.as_ptr(), pointer, "a vector at its cap keeps its allocation");

    let mut scratch = FrameScratch::default();
    scratch.glyphs = glyphs(8 * MIB / per_glyph, 8 * MIB / per_glyph);
    scratch.overlay_glyphs = glyphs(3 * MIB / per_glyph, 3 * MIB / per_glyph);
    scratch.quads.resize(6 * MIB / std::mem::size_of::<QuadInstance>(), bytemuck::Zeroable::zeroed());
    scratch.overlay_quads.resize(3 * MIB / std::mem::size_of::<QuadInstance>(), bytemuck::Zeroable::zeroed());
    scratch.underline_owners.resize(MIB / 8, 0);
    scratch.row_keys.resize(MIB / 8, 0);
    scratch.staged_ranges.resize(MIB / 16, (0, 0..0));
    scratch.finish();
    for (name, reserved, cap) in [
        ("glyphs", reserved_bytes(&scratch.glyphs), GLYPHS_CAP_BYTES),
        ("overlay glyphs", reserved_bytes(&scratch.overlay_glyphs), OVERLAY_CAP_BYTES),
        ("quads", reserved_bytes(&scratch.quads), QUADS_CAP_BYTES),
        ("overlay quads", reserved_bytes(&scratch.overlay_quads), OVERLAY_CAP_BYTES),
        ("underline owners", reserved_bytes(&scratch.underline_owners), INDEX_CAP_BYTES),
        ("row keys", reserved_bytes(&scratch.row_keys), INDEX_CAP_BYTES),
        ("staged ranges", reserved_bytes(&scratch.staged_ranges), SMALL_CAP_BYTES),
    ] {
        assert!(reserved <= cap, "{name}: {reserved} bytes over its {cap}-byte cap");
    }
    assert!(scratch.retained_amount().bytes <= FRAME_SCRATCH_CAP, "within the total cap");

    let mut scratch = FrameScratch::default();
    for slot in 0..6 {
        fill_snapped_slot(&mut scratch.snapped, &mut scratch.snapped_peak, slot, 0.0, 1.0, 40_000);
    }
    scratch.finish();
    scratch.snapped_peak = 0;
    fill_snapped_slot(&mut scratch.snapped, &mut scratch.snapped_peak, 0, 0.0, 1.0, 10);
    fill_snapped_slot(&mut scratch.snapped, &mut scratch.snapped_peak, 1, 0.0, 1.0, 10);
    scratch.finish();
    assert_eq!(scratch.snapped.len(), 2, "slots above the pass's peak are dropped first");
    let mut heavy = FrameScratch::default();
    // Five slots of 65,000 columns reserve about 1.3 MB, over the 1 MiB cap.
    for slot in 0..5 {
        fill_snapped_slot(&mut heavy.snapped, &mut heavy.snapped_peak, slot, 0.0, 1.0, 65_000);
    }
    assert!(snapped_bytes(&heavy.snapped) > SNAPPED_CAP_BYTES, "the pass exceeds the cap");
    heavy.finish();
    assert!(snapped_bytes(&heavy.snapped) <= SNAPPED_CAP_BYTES, "then the largest until within 1 MiB");
}

#[test]
fn scratch_restore_rule_per_exit() {
    // Exactly one holder per exit: a pass leases the scratch, and whether it fails, retries, is
    // dropped by a panic or hands the scratch to presentation and restores it, the next pass
    // leases the same buffers with their capacity; an unchanged frame never leases, so nothing
    // is released. A scratch lost after the hand-off leaves the next pass an empty one.
    let home = ScratchHome::new();
    let warm = |home: &ScratchHome| {
        let mut lease = home.lease();
        lease.get().glyphs.resize(1000, glyph());
        lease.get().quads.resize(500, bytemuck::Zeroable::zeroed());
        assert!(home.is_lent(), "while a pass holds the scratch, the home holds none");
        lease
    };
    let glyph_capacity = |home: &ScratchHome| {
        let lease = home.lease();
        let capacity = lease.held().glyphs.capacity();
        drop(lease);
        capacity
    };

    drop(warm(&home));
    assert!(!home.is_lent(), "an `Err` or retry exit restores the scratch when its lease drops");
    let after_error = glyph_capacity(&home);
    assert!(after_error >= 1000, "with its capacity");

    let handed = warm(&home).into_scratch();
    assert!(home.is_lent(), "presentation holds the scratch; the home does not");
    assert_eq!(handed.glyphs.len(), 1000, "with the pass's contents");
    home.restore(handed);
    assert!(!home.is_lent(), "every presentation outcome restores it");
    assert_eq!(glyph_capacity(&home), after_error, "the same buffers come back");

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _lease = warm(&home);
        panic!("a pass panics while it holds the scratch");
    }));
    assert!(panicked.is_err());
    assert!(!home.is_lent(), "unwinding drops the lease, which restores the scratch");

    let before_unchanged = home.retained_amount();
    assert_eq!(home.retained_amount(), before_unchanged, "an unchanged frame leases nothing");

    drop(warm(&home).into_scratch());
    assert!(home.is_lent(), "a scratch lost after the hand-off is gone");
    let lease = home.lease();
    assert_eq!(lease.held().glyphs.capacity(), 0, "the next pass starts empty");
    drop(lease);
    assert!(!home.is_lent());

    home.set_reuse(false);
    drop(warm(&home));
    assert_eq!(home.retained_amount().bytes, 0, "with reuse off nothing is kept");
}
