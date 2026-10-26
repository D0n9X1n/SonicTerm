//! The characters each fallback face cannot draw, so the shaper does not reopen an unloaded face for
//! a run made only of them.

use crate::shaper::FallbackIdx;
use crate::{ftwrap, hbwrap as harfbuzz};
use std::cell::RefCell;
use std::collections::HashSet;

/// How deep `draws` follows a canonical decomposition; Unicode's are at most four deep.
const DECOMPOSITION_DEPTH: u32 = 4;

/// Per fallback slot, the characters its face cannot draw.
pub(crate) struct FallbackMisses {
    /// One set per slot, indexed like the shaper's faces.
    slots: Vec<RefCell<HashSet<char>>>,
}

impl FallbackMisses {
    /// An empty record for `slot_count` slots.
    pub(crate) fn new(slot_count: usize) -> Self {
        Self { slots: (0..slot_count).map(|_| RefCell::default()).collect() }
    }

    /// Whether slot `font_idx`'s face is known to draw none of `run`'s characters.
    pub(crate) fn misses_all(&self, font_idx: FallbackIdx, run: &str) -> bool {
        self.slots.get(font_idx).is_some_and(|misses| {
            let misses = misses.borrow();
            !run.is_empty() && run.chars().all(|character| misses.contains(&character))
        })
    }

    /// Record the characters of `run` that `face`, slot `font_idx`'s face, cannot draw. A run the
    /// face completed no cluster of can still hold characters it draws, such as a base beside an
    /// unsupported combining mark, so each character is asked of the face itself.
    pub(crate) fn record(&self, font_idx: FallbackIdx, face: &ftwrap::Face, run: &str) {
        let Some(misses) = self.slots.get(font_idx) else {
            // When: `font_idx` names no slot, there is no record to add to.
            return;
        };
        let undrawable = run.chars().filter(|character| !draws(face, u32::from(*character), 0));
        misses.borrow_mut().extend(undrawable);
    }
}

/// Whether `face` may draw `codepoint`: its cmap maps it, or it decomposes canonically into a part
/// the face may draw, which HarfBuzz's normalizer would use instead.
fn draws(face: &ftwrap::Face, codepoint: u32, depth: u32) -> bool {
    let glyph_index =
        // SAFETY: `face.face` is a live, initialized FreeType face; the lookup only reads its charmap.
        unsafe { ftwrap::FT_Get_Char_Index(face.face, codepoint as ftwrap::FT_ULong) };
    if glyph_index != 0 || depth >= DECOMPOSITION_DEPTH {
        // When: `glyph_index` maps the character, or `depth` ends the search, no part is tried.
        return glyph_index != 0;
    }
    let (mut first, mut second) = (0, 0);
    let decomposes =
        // SAFETY: the default Unicode functions are a static HarfBuzz object, and `first` and
        // `second` are live locals the decomposition is written into.
        unsafe {
            harfbuzz::hb_unicode_decompose(
                harfbuzz::hb_unicode_funcs_get_default(),
                codepoint,
                &mut first,
                &mut second,
            )
        } != 0;
    decomposes && [first, second].into_iter().any(|part| part != 0 && draws(face, part, depth + 1))
}
