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
    let mut at_cap =
        glyphs(cap_elems::<GlyphInstance>(4 * MIB), cap_elems::<GlyphInstance>(4 * MIB));
    let pointer = at_cap.as_ptr();
    let at_cap_used = at_cap.len();
    finish_vec(&mut at_cap, at_cap_used, 4 * MIB);
    assert_eq!(at_cap.as_ptr(), pointer, "a vector at its cap keeps its allocation");

    let mut scratch = FrameScratch {
        glyphs: glyphs(8 * MIB / per_glyph, 8 * MIB / per_glyph),
        overlay_glyphs: glyphs(3 * MIB / per_glyph, 3 * MIB / per_glyph),
        ..FrameScratch::default()
    };
    scratch
        .quads
        .resize(6 * MIB / std::mem::size_of::<QuadInstance>(), bytemuck::Zeroable::zeroed());
    scratch
        .overlay_quads
        .resize(3 * MIB / std::mem::size_of::<QuadInstance>(), bytemuck::Zeroable::zeroed());
    scratch.underline_owners.resize(MIB / 8, 0);
    scratch.row_keys.resize(MIB / 8, 0);
    scratch.staged_ranges.resize(MIB / 16, (0, 0..0));
    scratch.finish(PassEnd::Completed);
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
    scratch.finish(PassEnd::Completed);
    scratch.snapped_peak = 0;
    fill_snapped_slot(&mut scratch.snapped, &mut scratch.snapped_peak, 0, 0.0, 1.0, 10);
    fill_snapped_slot(&mut scratch.snapped, &mut scratch.snapped_peak, 1, 0.0, 1.0, 10);
    scratch.finish(PassEnd::Completed);
    assert_eq!(scratch.snapped.len(), 2, "slots above the pass's peak are dropped first");
    let mut heavy = FrameScratch::default();
    // Five slots of 65,000 columns reserve about 1.3 MB, over the 1 MiB cap.
    for slot in 0..5 {
        fill_snapped_slot(&mut heavy.snapped, &mut heavy.snapped_peak, slot, 0.0, 1.0, 65_000);
    }
    assert!(snapped_bytes(&heavy.snapped) > SNAPPED_CAP_BYTES, "the pass exceeds the cap");
    heavy.finish(PassEnd::Completed);
    assert!(
        snapped_bytes(&heavy.snapped) <= SNAPPED_CAP_BYTES,
        "then the largest until within 1 MiB"
    );
}

#[test]
fn scratch_restore_rule_per_exit() {
    // Exactly one holder at a time, and no exit loses the scratch: a lease dropped by an `Err`, a
    // retry or a fallback, a completed lease carried to presentation and dropped there, and a
    // lease dropped by unwinding all return the same warm buffers to the home; an unchanged frame
    // never leases, so nothing is released; with reuse off nothing is kept.
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
    assert!(!home.is_lent(), "an `Err`, retry or fallback exit restores the scratch");
    let after_error = glyph_capacity(&home);
    assert!(after_error >= 1000, "with its capacity");

    let mut carried = warm(&home);
    carried.complete();
    assert!(home.is_lent(), "the lease carried to presentation still holds the scratch");
    assert_eq!(carried.held().glyphs.len(), 1000, "with the pass's contents");
    drop(carried);
    assert!(!home.is_lent(), "presentation's end restores it");
    assert_eq!(glyph_capacity(&home), after_error, "the same buffers come back");

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _lease = warm(&home);
        panic!("a pass panics while it holds the scratch");
    }));
    assert!(panicked.is_err());
    assert!(!home.is_lent(), "unwinding drops the lease, which restores the scratch");
    assert_eq!(glyph_capacity(&home), after_error, "with its buffers");

    let before_unchanged = home.retained_amount();
    assert_eq!(home.retained_amount(), before_unchanged, "an unchanged frame leases nothing");

    home.set_reuse(false);
    drop(warm(&home));
    assert_eq!(home.retained_amount().bytes, 0, "with reuse off nothing is kept");
}

/// A drawable frame on its way to presentation, carrying its pass's lease as the renderer's
/// assembled layers do.
struct CarriedFrame {
    scratch: ScratchLease,
}

#[test]
fn unwinding_after_assembly_or_during_presentation_restores_the_scratch() {
    // Once a pass completes, its lease travels with the frame: an unwind between assembly's return
    // and presentation (the source release and upload rebuild), or one while presentation reads
    // the batches, drops the frame and so the lease, which returns the warm scratch to its home.
    for (name, reading) in [("after assembly", false), ("during presentation", true)] {
        let home = ScratchHome::new();
        let mut lease = home.lease();
        lease.get().glyphs.resize(1000, glyph());
        drop(lease);
        let warm_bytes = home.retained_amount().bytes;
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut lease = home.lease();
            lease.get().glyphs.resize(1000, glyph());
            lease.complete();
            let frame = CarriedFrame { scratch: lease };
            if reading {
                let batches = frame.scratch.held();
                assert_eq!(batches.glyphs.len(), 1000, "presentation reads the batches");
                panic!("presentation unwinds while it reads the batches");
            }
            panic!("the frame unwinds before presentation");
        }));
        assert!(unwound.is_err(), "{name}: the closure unwound");
        assert!(!home.is_lent(), "{name}: the scratch is home again");
        assert_eq!(home.retained_amount().bytes, warm_bytes, "{name}: with its buffers");
    }
}

#[test]
fn assembled_layers_carry_the_lease_itself() {
    // The renderer's assembled layers hold the pass's lease, not raw scratch taken out of it, so
    // nothing between assembly and the end of presentation can drop the scratch unguarded.
    let core = include_str!("core.rs").replace("\r\n", "\n");
    assert!(
        core.contains("    scratch: frame_scratch::ScratchLease,"),
        "the layers hold the lease"
    );
    assert!(!core.contains("into_scratch("), "no raw scratch leaves the lease");
    assert!(!core.contains("std::mem::take(sinks.row_keys)"), "row keys are borrowed in place");
}

#[test]
fn an_incomplete_pass_keeps_every_warm_edge_slot() {
    // A frame uses column-edge slots 0 (glyphs) and 1 (background). A pass that stops after
    // filling slot 0 only (an `Err` or a retry before the background loop) must not drop slot 1:
    // that pass's peak understates the frame's working set, so its warm buffer is kept.
    let home = ScratchHome::new();
    let mut lease = home.lease();
    for slot in 0..2 {
        let scratch = lease.get();
        fill_snapped_slot(&mut scratch.snapped, &mut scratch.snapped_peak, slot, 0.0, 10.0, 80);
    }
    drop(lease);
    let before = home.retained_amount().bytes;
    let mut lease = home.lease();
    let scratch = lease.get();
    fill_snapped_slot(&mut scratch.snapped, &mut scratch.snapped_peak, 0, 0.0, 10.0, 80);
    drop(lease);
    let after = home.retained_amount().bytes;
    assert_eq!(after, before, "an incomplete pass kept {after} of {before} edge bytes");
}

#[test]
fn every_scratch_vector_is_held_within_its_own_cap() {
    // Each draw vector has its own byte cap, listed here independently of the module: a pass that
    // leaves any one of them far above its cap returns it within that cap, kept warm rather than
    // dropped, and the column-edge slots together stay within theirs. An incomplete pass applies
    // only the caps, so every category is checked on its cap alone.
    fn oversized<T>(cap_bytes: usize) -> Vec<T> {
        Vec::with_capacity(4 * cap_bytes / std::mem::size_of::<T>() + 1)
    }
    const KIB: usize = 1024;
    let mut scratch = FrameScratch {
        glyphs: oversized(4 * MIB),
        overlay_glyphs: oversized(MIB),
        quads: oversized(4 * MIB),
        overlay_quads: oversized(MIB),
        images: oversized(MIB),
        row_spans: oversized(MIB),
        underlines: oversized(MIB),
        underline_owners: oversized(256 * KIB),
        staged_ranges: oversized(64 * KIB),
        missing_tofu: oversized(MIB),
        pane_rects: oversized(64 * KIB),
        snapped: vec![oversized(MIB), oversized(MIB)],
        snapped_peak: 2,
        row_keys: oversized(256 * KIB),
    };
    scratch.finish(PassEnd::Incomplete);
    for (name, reserved, cap) in [
        ("glyphs", reserved_bytes(&scratch.glyphs), 4 * MIB),
        ("overlay glyphs", reserved_bytes(&scratch.overlay_glyphs), MIB),
        ("quads", reserved_bytes(&scratch.quads), 4 * MIB),
        ("overlay quads", reserved_bytes(&scratch.overlay_quads), MIB),
        ("images", reserved_bytes(&scratch.images), MIB),
        ("row spans", reserved_bytes(&scratch.row_spans), MIB),
        ("underlines", reserved_bytes(&scratch.underlines), MIB),
        ("underline owners", reserved_bytes(&scratch.underline_owners), 256 * KIB),
        ("staged ranges", reserved_bytes(&scratch.staged_ranges), 64 * KIB),
        ("missing tofu", reserved_bytes(&scratch.missing_tofu), MIB),
        ("pane rects", reserved_bytes(&scratch.pane_rects), 64 * KIB),
        ("row keys", reserved_bytes(&scratch.row_keys), 256 * KIB),
        ("column edges", snapped_bytes(&scratch.snapped), MIB),
    ] {
        assert!(reserved <= cap, "{name}: {reserved} bytes over its {cap}-byte cap");
    }
    assert!(reserved_bytes(&scratch.pane_rects) > 0, "a capped vector is kept warm, not dropped");
    assert!(reserved_bytes(&scratch.glyphs) > 0, "a capped vector is kept warm, not dropped");
}
