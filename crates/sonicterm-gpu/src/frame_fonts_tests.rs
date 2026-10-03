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
        prepare_frame_fonts(
            &mut self.applied,
            (7, generation),
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
