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
