//! Frame-scoped font preparation: the one place a frame reads the fallback generation.
//!
//! When a fallback notice has published a newer generation than the renderer applied, the
//! frame's preparation invalidates everything that may hold a placeholder: shaped rows and
//! line quads, the frame key, missing atlas sentinels, the preedit cache, and stored tab widths
//! (through the tab-title fallback epoch). A second preparation in the same generation does
//! nothing.

use crate::frame_plan::FrameKey;
use sonicterm_text::glyph_atlas::GlyphAtlas;
use sonicterm_text::row_glyph_cache::RowGlyphCache;

use crate::row_quad_cache::LineQuadCache;

/// The `(notice id, generation)` a frame's preparation applied; width measurement and rendering
/// take it, so neither reads the generation itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameFonts {
    notice_id: u64,
    generation: u64,
}

impl FrameFonts {
    /// The applied notice's id.
    #[must_use]
    pub fn notice_id(&self) -> u64 {
        self.notice_id
    }

    /// The applied generation.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// What one frame preparation changed, so a fallback apply can be told from a font setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontChange {
    /// The notice and generation were already applied; nothing was invalidated.
    None,
    /// The first preparation, or a new notice from a replaced body stack.
    Initial,
    /// A newer generation of the notice already applied: a fallback apply.
    Generation,
}

impl FontChange {
    /// Whether the preparation invalidated the shaped rows and the atlas's missing entries.
    #[must_use]
    pub fn invalidated(self) -> bool {
        self != Self::None
    }
}

/// Record in `owed` whether the next render attempt carries a fallback apply after `change`. A
/// newer generation is owed until an attempt takes it, however many preparations repeat it; a
/// first or replaced stack owes none; an unchanged preparation leaves `owed` as it was.
pub(super) fn owe_apply(owed: &mut bool, change: FontChange) {
    match change {
        // A newer generation was applied: the next render attempt carries it, once.
        FontChange::Generation => *owed = true,
        // A first or replaced stack was set up: no fallback apply is owed.
        FontChange::Initial => *owed = false,
        FontChange::None => {
            // When: `change` is None, the preparation changed nothing, so an owed apply stays owed.
        }
    }
}

/// Everything a fallback apply invalidates. Generic over the frame key and preedit cache types
/// so a test can supply stand-ins.
pub(super) struct FontApplyTargets<'targets, Key = FrameKey, Preedit = super::PreeditGlyphCache> {
    pub(super) row_glyph_cache: &'targets mut RowGlyphCache,
    pub(super) line_quad_cache: &'targets mut LineQuadCache,
    pub(super) style_rev: &'targets mut u64,
    pub(super) last_frame_key: &'targets mut Option<Key>,
    pub(super) glyph_atlas: &'targets mut GlyphAtlas,
    pub(super) preedit_glyph_cache: &'targets mut Option<Preedit>,
    pub(super) fallback_epoch: &'targets mut u64,
}

/// Whether an acknowledged fallback `current` still needs a frame: true unless the last frame
/// preparation already applied exactly that notice and generation.
#[must_use]
pub(super) fn fallback_frame_due(applied: Option<(u64, u64)>, current: (u64, u64)) -> bool {
    applied != Some(current)
}

/// Attach `waker` to the fallback notice of `stack`, the body stack a renderer shapes with, so a
/// completion of that configuration wakes the renderer's window.
pub(super) fn attach_fallback_waker(
    stack: Option<&sonicterm_engine::FontStack>,
    waker: Option<&super::FontFallbackWaker>,
) {
    if let (Some(stack), Some(waker)) = (stack, waker) {
        stack.fallback_notice().attach_waker(std::sync::Arc::clone(waker));
    }
}

/// Install `stack` as the renderer's body stack in `slot` and attach `waker` to its notice, as a
/// font reload does: the new stack has a new notice, which must wake the same window.
pub(super) fn install_body_stack(
    slot: &mut Option<sonicterm_engine::FontStack>,
    stack: Option<sonicterm_engine::FontStack>,
    waker: Option<&super::FontFallbackWaker>,
) {
    *slot = stack;
    attach_fallback_waker(slot.as_ref(), waker);
}

/// Handle a delivered fallback wake for `notice_id`: an event from a notice `stack` no longer
/// carries is ignored, and leaves the current notice's claim alone; otherwise the claim is
/// acknowledged and the result says whether a frame must apply the generation it read.
pub(super) fn acknowledge_fallback_wake(
    stack: Option<&sonicterm_engine::FontStack>,
    applied: Option<(u64, u64)>,
    notice_id: u64,
) -> bool {
    let Some(stack) = stack else {
        // When: no body stack exists, so no notice of this renderer can have completed.
        return false;
    };
    let notice = stack.fallback_notice();
    if notice.id() != notice_id {
        // When: the event belongs to a notice this renderer replaced; its state is not ours.
        return false;
    }
    let generation = notice.acknowledge();
    fallback_frame_due(applied, (notice_id, generation))
}

/// Prepare one frame's fonts: apply `current` when it differs from `applied`, and return the
/// token with what changed.
pub(super) fn prepare_frame_fonts<Key, Preedit>(
    applied: &mut Option<(u64, u64)>,
    current: (u64, u64),
    targets: FontApplyTargets<'_, Key, Preedit>,
) -> (FrameFonts, FontChange) {
    let token = FrameFonts { notice_id: current.0, generation: current.1 };
    if *applied == Some(current) {
        // When: this notice and generation were already applied, no placeholder can be stale.
        return (token, FontChange::None);
    }
    let change = match *applied {
        // The same notice was applied before and only its generation moved: a fallback apply.
        Some((notice_id, _)) if notice_id == current.0 => FontChange::Generation,
        _ => FontChange::Initial,
    };
    *applied = Some(current);
    targets.row_glyph_cache.invalidate_all();
    targets.line_quad_cache.invalidate_all();
    *targets.style_rev = targets.style_rev.wrapping_add(1);
    *targets.last_frame_key = None;
    // Missing sentinels own no rectangle, so every other UV survives.
    targets.glyph_atlas.forget_missing();
    // The preedit key lacks `style_rev`, so it is dropped outright.
    *targets.preedit_glyph_cache = None;
    *targets.fallback_epoch += 1;
    crate::frame_stats::note_font_fallback_apply();
    if change == FontChange::Generation {
        // A newer generation of the same notice, counted apart from initial setups.
        crate::frame_stats::note_font_generation_apply();
    }
    (token, change)
}

#[cfg(test)]
#[path = "frame_fonts_tests.rs"]
mod frame_fonts_tests;
