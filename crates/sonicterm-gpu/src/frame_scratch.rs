//! Renderer-owned per-frame scratch: the draw vectors one assembly pass fills.
//!
//! The renderer keeps one [`FrameScratch`] between frames in a [`ScratchHome`]. An assembly pass
//! leases it after its unchanged and no-op exits; the [`ScratchLease`] guard restores it on every
//! other exit, including `?` errors, the atlas retry, the partial fallback and unwinding. A pass
//! that produces drawable layers hands the scratch to presentation with
//! [`ScratchLease::into_scratch`], and presentation restores it on every outcome. Exactly one of
//! the home, a lease or presentation holds it at any time.
//!
//! On restore each vector records its use, is cleared, is shrunk by the vertex scratch's release
//! rule and is then held within its element cap, so one large frame cannot pin its peak.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use sonicterm_render_model::boundary::ui::pane::Rect as PaneRect;
use sonicterm_text::row_glyph_cache::UnderlineRun;
use sonicterm_text::GlyphInstance;
use sonicterm_types::ResourceAmount;

use crate::color::ChromeColor;
use crate::cursor::RowGlyphSpan;
use crate::quad::QuadInstance;
use crate::wezterm_pipeline::{release_excess, ImageInstance};

#[cfg(test)]
#[path = "frame_scratch_tests.rs"]
mod frame_scratch_tests;

/// One underline run with the origin and column count of the pane that drew it.
pub(crate) type FrameUnderline = (f32, f32, u16, u16, UnderlineRun);

/// One missing-glyph outline: `(x, y, width, height, color)` in raster px.
pub(crate) type TofuQuad = (f32, f32, f32, f32, ChromeColor);

const MIB: usize = 1024 * 1024;
const KIB: usize = 1024;

/// Byte cap of the main glyph list.
pub(crate) const GLYPHS_CAP_BYTES: usize = 4 * MIB;
/// Byte cap of the main quad list.
pub(crate) const QUADS_CAP_BYTES: usize = 4 * MIB;
/// Byte cap of each overlay list, the image list, row spans, underlines and missing tofu.
pub(crate) const OVERLAY_CAP_BYTES: usize = MIB;
/// Byte cap of the pane-rect and staged-range lists.
pub(crate) const SMALL_CAP_BYTES: usize = 64 * KIB;
/// Byte cap of the underline-owner and row-key index lists.
pub(crate) const INDEX_CAP_BYTES: usize = 256 * KIB;
/// Byte cap of all column-edge slots together, headers included.
pub(crate) const SNAPPED_CAP_BYTES: usize = MIB;

/// The most one renderer's frame scratch retains: the sum of every cap.
pub(crate) const FRAME_SCRATCH_CAP: usize = GLYPHS_CAP_BYTES
    + QUADS_CAP_BYTES
    + 6 * OVERLAY_CAP_BYTES
    + 2 * SMALL_CAP_BYTES
    + 2 * INDEX_CAP_BYTES
    + SNAPPED_CAP_BYTES;

/// Elements of `T` that fit in `cap_bytes`.
pub(crate) const fn cap_elems<T>(cap_bytes: usize) -> usize {
    cap_bytes / std::mem::size_of::<T>()
}

/// Bytes `held` reserves.
fn reserved<T>(held: &Vec<T>) -> usize {
    held.capacity().saturating_mul(std::mem::size_of::<T>())
}

/// End one pass for one reused vector that held `used` elements: clear it, release its excess
/// by the vertex scratch's rule, then replace it by an empty vector of exactly the cap's
/// elements when it still reserves more than `cap_bytes`.
pub(crate) fn finish_vec<T>(held: &mut Vec<T>, used: usize, cap_bytes: usize) {
    held.clear();
    release_excess(held, used);
    let cap = cap_elems::<T>(cap_bytes);
    if held.capacity() > cap {
        // When: the vector still reserves more than its cap, it is replaced at the cap.
        *held = Vec::with_capacity(cap);
    }
}

/// [`finish_vec`] for a vector whose current length is the pass's use.
fn finish_used<T>(held: &mut Vec<T>, cap_bytes: usize) {
    let used = held.len();
    finish_vec(held, used, cap_bytes);
}

/// Bytes the column-edge slots reserve: the slot headers and every slot's buffer.
pub(crate) fn snapped_bytes(snapped: &Vec<Vec<f32>>) -> usize {
    reserved(snapped) + snapped.iter().map(reserved).sum::<usize>()
}

/// Fill column-edge slot `slot` of `snapped` for a pane at `origin_x` with `cols` cells of
/// `cell_w`, growing the slot list as needed and recording the pass's peak slot count.
pub(crate) fn fill_snapped_slot(
    snapped: &mut Vec<Vec<f32>>,
    peak: &mut usize,
    slot: usize,
    origin_x: f32,
    cell_w: f32,
    cols: u16,
) {
    if snapped.len() <= slot {
        // When: this pass reaches a slot the list has never held, empty slots are added.
        snapped.resize_with(slot + 1, Vec::new);
    }
    *peak = (*peak).max(slot + 1);
    super::fill_snapped_cell_x(&mut snapped[slot], origin_x, cell_w, cols);
}

/// The draw vectors one assembly pass fills and presentation reads.
#[derive(Debug, Default)]
pub(crate) struct FrameScratch {
    /// Terminal and tab-title glyphs.
    pub(crate) glyphs: Vec<GlyphInstance>,
    /// Modal and overlay glyphs, drawn after the overlay quads.
    pub(crate) overlay_glyphs: Vec<GlyphInstance>,
    /// Backgrounds, cursors, decorations and chrome quads.
    pub(crate) quads: Vec<QuadInstance>,
    /// Overlay quads, drawn after the terminal text.
    pub(crate) overlay_quads: Vec<QuadInstance>,
    /// Inline image draws.
    pub(crate) images: Vec<ImageInstance>,
    /// Each emitted row's glyph range and ink.
    pub(crate) row_spans: Vec<RowGlyphSpan>,
    /// Underline runs with their pane's origin and columns.
    pub(crate) underlines: Vec<FrameUnderline>,
    /// Per underline, the staging index of the row that pushed it.
    pub(crate) underline_owners: Vec<usize>,
    /// Per drawn pane, the staging indices its rows took.
    pub(crate) staged_ranges: Vec<(u64, Range<usize>)>,
    /// Missing-glyph outlines collected during the cell walk.
    pub(crate) missing_tofu: Vec<TofuQuad>,
    /// Every planned pane's rect.
    pub(crate) pane_rects: Vec<(u64, PaneRect)>,
    /// Column-edge buffers, one slot per (pane, site).
    pub(crate) snapped: Vec<Vec<f32>>,
    /// Slots this pass filled; slots above it are dropped first on release.
    pub(crate) snapped_peak: usize,
    /// One pane's row keys while its rows are pinned.
    pub(crate) row_keys: Vec<u64>,
}

impl FrameScratch {
    /// End a pass: record each vector's use, clear it and hold it within its release rule and
    /// cap; then keep only the column-edge slots the pass used and drop the largest until all
    /// of them fit their cap.
    pub(crate) fn finish(&mut self) {
        finish_used(&mut self.glyphs, GLYPHS_CAP_BYTES);
        finish_used(&mut self.overlay_glyphs, OVERLAY_CAP_BYTES);
        finish_used(&mut self.quads, QUADS_CAP_BYTES);
        finish_used(&mut self.overlay_quads, OVERLAY_CAP_BYTES);
        finish_used(&mut self.images, OVERLAY_CAP_BYTES);
        finish_used(&mut self.row_spans, OVERLAY_CAP_BYTES);
        finish_used(&mut self.underlines, OVERLAY_CAP_BYTES);
        finish_used(&mut self.underline_owners, INDEX_CAP_BYTES);
        finish_used(&mut self.staged_ranges, SMALL_CAP_BYTES);
        finish_used(&mut self.missing_tofu, OVERLAY_CAP_BYTES);
        finish_used(&mut self.pane_rects, SMALL_CAP_BYTES);
        finish_used(&mut self.row_keys, INDEX_CAP_BYTES);
        self.snapped.truncate(self.snapped_peak);
        for slot in &mut self.snapped {
            finish_used(slot, SNAPPED_CAP_BYTES);
        }
        self.snapped_peak = 0;
        while snapped_bytes(&self.snapped) > SNAPPED_CAP_BYTES {
            let Some(largest) = (0..self.snapped.len()).max_by_key(|index| self.snapped[*index].capacity())
            else {
                // When: no slot is left yet the headers alone pass the cap, they are released.
                self.snapped = Vec::new();
                break;
            };
            if self.snapped[largest].capacity() == 0 {
                // When: every slot is already empty, only the header list is left to release.
                self.snapped = Vec::new();
                break;
            }
            self.snapped[largest] = Vec::new();
        }
    }

    /// Bytes and allocated vectors the scratch retains: every vector's capacity times its
    /// element size, plus the column-edge slot headers.
    pub(crate) fn retained_amount(&self) -> ResourceAmount {
        let parts = [
            reserved(&self.glyphs),
            reserved(&self.overlay_glyphs),
            reserved(&self.quads),
            reserved(&self.overlay_quads),
            reserved(&self.images),
            reserved(&self.row_spans),
            reserved(&self.underlines),
            reserved(&self.underline_owners),
            reserved(&self.staged_ranges),
            reserved(&self.missing_tofu),
            reserved(&self.pane_rects),
            reserved(&self.row_keys),
            snapped_bytes(&self.snapped),
        ];
        ResourceAmount {
            bytes: parts.iter().sum::<usize>(),
            items: parts.iter().filter(|bytes| **bytes > 0).count(),
        }
    }
}

/// Where the scratch lives between passes.
#[derive(Debug, Default)]
struct HomeState {
    /// The scratch while no pass or presentation holds it.
    held: Option<FrameScratch>,
    /// Whether a lease or presentation holds the scratch.
    lent: bool,
    /// Whether a restored scratch is kept for the next pass; off drops it, so every pass
    /// allocates afresh (a test compares the two).
    no_reuse: bool,
}

/// The renderer's frame scratch between passes, shared with the lease a pass holds.
#[derive(Debug, Clone, Default)]
pub(crate) struct ScratchHome {
    state: Rc<RefCell<HomeState>>,
}

impl ScratchHome {
    /// A home holding no scratch yet.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Lend the scratch to one assembly pass; an empty one when none is held.
    pub(crate) fn lease(&self) -> ScratchLease {
        let mut state = self.state.borrow_mut();
        state.lent = true;
        let scratch = state.held.take().unwrap_or_default();
        ScratchLease { home: self.clone(), scratch: Some(scratch) }
    }

    /// Guard a scratch handed out by [`ScratchLease::into_scratch`] while presentation reads it;
    /// dropping the guard restores the scratch, whatever the outcome.
    pub(crate) fn hold(&self, scratch: FrameScratch) -> ScratchLease {
        debug_assert!(self.is_lent(), "only a scratch the home lent can be held");
        ScratchLease { home: self.clone(), scratch: Some(scratch) }
    }

    /// Take the scratch back after a pass or presentation: finish it and keep it for the next
    /// pass, or drop it when reuse is off.
    pub(crate) fn restore(&self, mut scratch: FrameScratch) {
        scratch.finish();
        let mut state = self.state.borrow_mut();
        debug_assert!(state.lent, "only the one holder restores the frame scratch");
        debug_assert!(state.held.is_none(), "the home never holds a second scratch");
        state.lent = false;
        if !state.no_reuse {
            // When: reuse is on, the finished buffers wait for the next pass.
            state.held = Some(scratch);
        }
    }

    /// Whether a pass or presentation holds the scratch.
    pub(crate) fn is_lent(&self) -> bool {
        self.state.borrow().lent
    }

    /// Keep restored scratch for the next pass when `reuse`; otherwise drop it now and on every
    /// later restore.
    pub(crate) fn set_reuse(&self, reuse: bool) {
        let mut state = self.state.borrow_mut();
        state.no_reuse = !reuse;
        if !reuse {
            // When: reuse is turned off, the held buffers are released at once.
            state.held = None;
        }
    }

    /// What the home retains between passes; zero while the scratch is lent.
    pub(crate) fn retained_amount(&self) -> ResourceAmount {
        self.state.borrow().held.as_ref().map(FrameScratch::retained_amount).unwrap_or_default()
    }
}

/// One assembly pass's hold on the frame scratch; dropping it restores the scratch.
#[derive(Debug)]
pub(crate) struct ScratchLease {
    home: ScratchHome,
    scratch: Option<FrameScratch>,
}

impl ScratchLease {
    /// The leased scratch.
    pub(crate) fn get(&mut self) -> &mut FrameScratch {
        self.scratch.as_mut().expect("a live lease holds the scratch")
    }

    /// The leased scratch, read-only.
    pub(crate) fn held(&self) -> &FrameScratch {
        self.scratch.as_ref().expect("a live lease holds the scratch")
    }

    /// Hand the scratch to presentation, which must restore it on every outcome.
    pub(crate) fn into_scratch(mut self) -> FrameScratch {
        self.scratch.take().expect("a live lease holds the scratch")
    }
}

// Lifecycle: a ScratchLease still holding its scratch when dropped (an `Err`, a retry, a
// fallback or unwinding) restores it to its home, so no exit loses the buffers.
impl Drop for ScratchLease {
    fn drop(&mut self) {
        if let Some(scratch) = self.scratch.take() {
            // When: the scratch was not handed to presentation, the lease returns it.
            self.home.restore(scratch);
        }
    }
}
