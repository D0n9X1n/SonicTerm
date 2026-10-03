//! One fallback apply invalidates every placeholder holder once per generation.

use super::*;
use sonicterm_text::glyph_atlas::{RasterTile, Rasterizer};
use sonicterm_types::glyph_key::GlyphKey;

/// A rasterizer that never resolves a glyph, so the atlas caches a missing sentinel.
struct Unresolved;

impl Rasterizer for Unresolved {
    fn rasterize(&mut self, _: GlyphKey) -> Option<RasterTile> {
        None
    }
}

/// The apply targets with stand-in frame key and preedit values.
struct Targets {
    applied: Option<(u64, u64)>,
    rows: RowGlyphCache,
    quads: LineQuadCache,
    style_rev: u64,
    frame_key: Option<u8>,
    atlas: GlyphAtlas,
    preedit: Option<&'static str>,
    epoch: u64,
}

impl Targets {
    fn prepare(&mut self, generation: u64) -> (FrameFonts, bool) {
        self.prepare_notice(7, generation)
    }

    fn prepare_notice(&mut self, notice_id: u64, generation: u64) -> (FrameFonts, bool) {
        prepare_frame_fonts(
            &mut self.applied,
            (notice_id, generation),
            FontApplyTargets {
                row_glyph_cache: &mut self.rows,
                line_quad_cache: &mut self.quads,
                style_rev: &mut self.style_rev,
                last_frame_key: &mut self.frame_key,
                glyph_atlas: &mut self.atlas,
                preedit_glyph_cache: &mut self.preedit,
                fallback_epoch: &mut self.epoch,
            },
        )
    }
}

#[test]
fn one_apply_invalidates_each_target_and_a_repeat_in_the_same_generation_does_nothing() {
    // A newer generation drops the missing sentinel, the frame key and the preedit, bumps the
    // style revision and the tab epoch once; preparing again in that generation changes nothing.
    let missing = GlyphKey::new('é', false, false);
    let mut targets = Targets {
        applied: Some((7, 0)),
        rows: RowGlyphCache::new(),
        quads: LineQuadCache::new(),
        style_rev: 4,
        frame_key: Some(9),
        atlas: GlyphAtlas::new(16, 16),
        preedit: Some("cached"),
        epoch: 0,
    };
    assert!(targets.atlas.get_or_insert(missing, &mut Unresolved).unwrap().missing);
    let (token, applied) = targets.prepare(1);
    assert!(applied);
    assert_eq!((token.notice_id(), token.generation()), (7, 1));
    assert_eq!(targets.atlas.get(missing), None, "the missing sentinel is forgotten");
    assert_eq!(
        (targets.style_rev, targets.frame_key, targets.preedit, targets.epoch),
        (5, None, None, 1)
    );
    targets.frame_key = Some(3);
    targets.preedit = Some("again");
    assert!(!targets.prepare(1).1, "a second preparation in the same generation does nothing");
    assert_eq!(
        (targets.style_rev, targets.frame_key, targets.preedit, targets.epoch),
        (5, Some(3), Some("again"), 1)
    );
}

#[test]
fn an_apply_counts_one_fallback_apply_and_a_repeat_counts_none() {
    // font_fallback_applies counts each preparation that applied a newer generation, once,
    // inside a counting renderer's scope; a repeat in that generation and an uncounted scope add none.
    let mut targets = Targets {
        applied: Some((7, 0)),
        rows: RowGlyphCache::new(),
        quads: LineQuadCache::new(),
        style_rev: 0,
        frame_key: None,
        atlas: GlyphAtlas::new(16, 16),
        preedit: None,
        epoch: 0,
    };
    let sink = crate::frame_stats::FrameStatsSink::default();
    {
        let _collect = crate::frame_stats::CollectGuard::enter(Some(&sink));
        assert!(targets.prepare(1).1);
        assert!(!targets.prepare(1).1);
        assert!(targets.prepare(2).1);
    }
    assert_eq!(sink.snapshot().font_fallback_applies, 2);
    assert!(targets.prepare(3).1);
    assert_eq!(sink.snapshot().font_fallback_applies, 2, "nothing counts with the gate off");
}

/// A delivered fallback wake needs a frame unless the last preparation applied exactly that
/// notice and generation; an older generation or another notice still needs one.
#[test]
fn a_fallback_frame_is_due_until_its_generation_is_applied() {
    assert!(fallback_frame_due(None, (7, 0)), "nothing applied yet");
    assert!(fallback_frame_due(Some((7, 0)), (7, 1)), "a newer generation is pending");
    assert!(fallback_frame_due(Some((6, 3)), (7, 3)), "a replaced notice is pending");
    assert!(!fallback_frame_due(Some((7, 1)), (7, 1)), "already applied");
}

/// Hold the shared font fixture even after a failed sibling test poisoned it.
fn font_fixture_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A waker that records each notice id it is called with.
fn recording_waker() -> (crate::core::FontFallbackWaker, std::sync::Arc<std::sync::Mutex<Vec<u64>>>)
{
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = std::sync::Arc::clone(&calls);
    (std::sync::Arc::new(move |notice_id| recorded.lock().unwrap().push(notice_id)), calls)
}

#[test]
fn a_stale_wake_is_a_no_op_and_leaves_the_current_claim_alone() {
    // An event from a replaced notice needs no frame and must not clear the current notice's
    // claim: while that claim stands, a further completion posts nothing; the current event's
    // acknowledgement clears it, is due until applied, and the next completion posts again.
    let _lock = font_fixture_lock();
    let old_stack = crate::lib_tests::tracked_font_stack(14.0);
    let stack = crate::lib_tests::tracked_font_stack(14.0);
    let (old_id, current_id) = (old_stack.fallback_notice().id(), stack.fallback_notice().id());
    assert_ne!(old_id, current_id, "each configuration has its own notice");
    let (waker, calls) = recording_waker();
    attach_fallback_waker(Some(&stack), Some(&waker));
    let applied = Some((current_id, 0));

    stack.fallback_notice().complete();
    assert!(!acknowledge_fallback_wake(Some(&stack), applied, old_id), "stale event");
    stack.fallback_notice().complete();
    assert_eq!(*calls.lock().unwrap(), vec![current_id], "the claim is still posted");
    assert!(acknowledge_fallback_wake(Some(&stack), applied, current_id));
    assert!(!acknowledge_fallback_wake(Some(&stack), Some((current_id, 2)), current_id));
    stack.fallback_notice().complete();
    assert_eq!(*calls.lock().unwrap(), vec![current_id, current_id]);
    assert!(!acknowledge_fallback_wake(None, applied, current_id), "no stack, no frame");
}

#[test]
fn a_reload_attaches_the_waker_to_the_new_notice_and_ignores_the_old_one() {
    // A font reload replaces the body stack and attaches the same waker to its new notice. The
    // old notice may still complete and post; queued alone or with a current event it is a
    // no-op, and the current event needs exactly the frame that applies its generation.
    let _lock = font_fixture_lock();
    let (waker, calls) = recording_waker();
    let old_stack = crate::lib_tests::tracked_font_stack(14.0);
    attach_fallback_waker(Some(&old_stack), Some(&waker));
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    attach_fallback_waker(Some(&stack), Some(&waker));
    let (old_id, current_id) = (old_stack.fallback_notice().id(), stack.fallback_notice().id());
    let applied = Some((current_id, 0));

    old_stack.fallback_notice().complete();
    assert!(!acknowledge_fallback_wake(Some(&stack), applied, old_id), "old event alone");
    old_stack.fallback_notice().complete();
    stack.fallback_notice().complete();
    assert_eq!(*calls.lock().unwrap(), vec![old_id, current_id], "one event per notice");
    assert!(!acknowledge_fallback_wake(Some(&stack), applied, old_id), "old event queued first");
    assert!(acknowledge_fallback_wake(Some(&stack), applied, current_id));
}

#[test]
fn a_scale_rebuild_costs_at_most_one_extra_apply() {
    // Rebuilding faces for a new scale gives a new notice at generation 0. The next preparation
    // applies it once, bumping the epoch once; later preparations in it apply nothing.
    let mut targets = Targets {
        applied: Some((7, 3)),
        rows: RowGlyphCache::new(),
        quads: LineQuadCache::new(),
        style_rev: 0,
        frame_key: None,
        atlas: GlyphAtlas::new(16, 16),
        preedit: None,
        epoch: 0,
    };
    assert!(targets.prepare_notice(8, 0).1);
    assert!(!targets.prepare_notice(8, 0).1);
    assert!(!targets.prepare_notice(8, 0).1);
    assert_eq!(targets.epoch, 1);
}
