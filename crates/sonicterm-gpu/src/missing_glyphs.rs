//! Missing-glyph records of the presented frame: per pane and row slot, the characters the row
//! presented there drew as tofu, and the count of those no font SonicTerm can find covers.
//!
//! A row's tofu mixes four causes (fallback still pending, unresolved, a glyph that rasterized
//! nothing, a tile too large to place). Only the unresolved ones are counted, by intersecting the
//! records with the fallback notice's unresolved set, which holds only codepoints that every
//! lookup stage ran for and none covered.

use std::collections::{BTreeSet, HashMap};

/// Distinct codepoints one row slot keeps.
pub const SLOT_CAP: usize = 64;
/// Distinct codepoints one window counts before it reports a lower bound.
pub const WINDOW_CAP: usize = 1000;
/// Codepoints a count keeps as its sample for the click's notification.
pub const SAMPLE_LEN: usize = 16;

/// What a delivered fallback wake asks of a renderer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FallbackWake {
    /// A face generation this renderer has not applied is published; one frame must apply it.
    pub faces_due: bool,
    /// The unresolved set moved since this renderer last acknowledged it; only the status line's
    /// count can change, and it needs no font apply.
    pub unresolved_changed: bool,
}

/// How many distinct unresolved codepoints a window's presented panes draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingGlyphValue {
    /// Exactly this many.
    Exact(u64),
    /// A cap was hit; at least this many, all proven unresolved.
    AtLeast(u64),
    /// The unresolved set overflowed, so membership cannot be proven for every drawn character.
    Capped,
}

impl Default for MissingGlyphValue {
    fn default() -> Self {
        Self::Exact(0)
    }
}

/// A window's missing-glyph count and up to [`SAMPLE_LEN`] of its codepoints, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MissingGlyphs {
    /// The count.
    pub value: MissingGlyphValue,
    /// The first codepoints counted, in codepoint order.
    pub sample: Vec<char>,
}

/// The characters one presented row drew as tofu, tagged with what makes it that row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotMissing {
    /// The row's content key; the record counts only while the slot's committed key matches.
    pub(crate) key: u64,
    /// The fonts the row was shaped with; a font apply drops the record.
    pub(crate) fonts: Option<(u64, u64)>,
    /// Its distinct tofu characters, at most [`SLOT_CAP`].
    pub(crate) chars: Vec<char>,
    /// The row drew more than [`SLOT_CAP`] distinct tofu characters.
    pub(crate) truncated: bool,
}

/// One emitted row's record, staged until its frame presents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedMissing {
    /// The pane the row belongs to.
    pub(crate) pane_id: u64,
    /// The row slot it was emitted at.
    pub(crate) slot: u16,
    /// What the slot shows from now on; empty `chars` clears the slot.
    pub(crate) record: SlotMissing,
}

/// The distinct characters of `drawn`, in first-seen order, capped at [`SLOT_CAP`], and whether
/// any were dropped.
pub(crate) fn distinct_capped(drawn: &[char]) -> (Vec<char>, bool) {
    let mut chars: Vec<char> = Vec::new();
    let mut truncated = false;
    for codepoint in drawn {
        if chars.contains(codepoint) {
            // When: chars already contains this codepoint, a repeat adds nothing.
            continue;
        }
        if chars.len() == SLOT_CAP {
            // When: chars reached SLOT_CAP, the rest are dropped and the row is marked truncated.
            truncated = true;
            break;
        }
        chars.push(*codepoint);
    }
    (chars, truncated)
}

/// Apply a presented frame's staged rows: each replaces its slot's record, and an empty one
/// clears it. Slots the frame did not emit keep theirs, since their pixels stay on screen.
pub(crate) fn commit_missing(
    presented: &mut HashMap<(u64, u16), SlotMissing>,
    staged: &mut Vec<StagedMissing>,
) {
    for StagedMissing { pane_id, slot, record } in staged.drain(..) {
        if record.chars.is_empty() {
            presented.remove(&(pane_id, slot));
        } else {
            // When: record.chars is not empty, the slot's row drew tofu.
            presented.insert((pane_id, slot), record);
        }
    }
}

/// Drop records whose slot no longer shows their row (`committed` gives the slot's committed
/// key, `None` once the pane is gone) or whose fonts are not `fonts`.
pub(crate) fn prune_missing(
    presented: &mut HashMap<(u64, u16), SlotMissing>,
    committed: impl Fn(u64, u16) -> Option<u64>,
    fonts: Option<(u64, u64)>,
) {
    presented.retain(|(pane_id, slot), record| {
        record.fonts == fonts && committed(*pane_id, *slot) == Some(record.key)
    });
}

/// Whether `codepoint` is in a Unicode Private Use Area, where icon fonts such as Nerd Fonts live.
pub(crate) fn is_private_use(codepoint: char) -> bool {
    matches!(u32::from(codepoint), 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

/// Count the distinct codepoints of `records` that `unresolved` holds.
pub(crate) fn count_missing<'record>(
    records: impl Iterator<Item = &'record SlotMissing>,
    unresolved: &sonicterm_engine::UnresolvedSnapshot,
    ignore_private_use: bool,
) -> MissingGlyphs {
    let mut counted = BTreeSet::new();
    let mut lower_bound = false;
    let mut drew_tofu = false;
    for record in records {
        lower_bound |= record.truncated;
        for codepoint in &record.chars {
            if ignore_private_use && is_private_use(*codepoint) {
                // When: ignore_private_use is set and is_private_use holds, the icon is not counted.
                continue;
            }
            drew_tofu = true;
            if !unresolved.contains(*codepoint) {
                // When: unresolved does not contain it, the box has another cause and is not counted.
                continue;
            }
            if counted.len() == WINDOW_CAP {
                // When: counted reached WINDOW_CAP, a new codepoint makes the count a lower bound.
                lower_bound |= !counted.contains(codepoint);
                continue;
            }
            counted.insert(*codepoint);
        }
    }
    let sample = counted.iter().copied().take(SAMPLE_LEN).collect();
    let count = counted.len() as u64;
    let value = if unresolved.overflow && drew_tofu {
        MissingGlyphValue::Capped
    } else if lower_bound && count > 0 {
        // When: lower_bound is set, a cap dropped codepoints, so the count proves only a minimum.
        MissingGlyphValue::AtLeast(count)
    } else {
        // When: neither overflow nor lower_bound applies, every drawn codepoint was judged.
        MissingGlyphValue::Exact(count)
    };
    MissingGlyphs { value, sample }
}

#[cfg(test)]
#[path = "missing_glyphs_tests.rs"]
mod missing_glyphs_tests;
