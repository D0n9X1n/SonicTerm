//! Contracts responsibilities of the renderer, preserving its single owner.

use super::*;

/// The metadata receipts a presented `plan` issues: one per pane the plan acknowledges, read from the
/// grids it was assembled from. They carry no grid borrow, so they outlive the frame's source.
pub(super) fn presented_receipts(
    plan: &FramePlan,
    panes: &[sonicterm_render_model::PaneRender<'_>],
) -> Vec<sonicterm_render_model::AckReceipt> {
    panes
        .iter()
        .enumerate()
        .filter_map(|(index, pane)| {
            let rows = plan.acknowledged_rows(index, pane.id, pane.grid.revision())?;
            Some(sonicterm_render_model::AckReceipt::of(index, pane.id, pane.grid, rows))
        })
        .collect()
}

/// Apply `receipts` to the panes a caller still borrows: each clears its rows only when the pane at
/// its index has its id and every grid identity still matches. Returns how many receipts cleared.
pub fn acknowledge_receipts(
    receipts: &[sonicterm_render_model::AckReceipt],
    panes: &mut [sonicterm_render_model::PaneRender<'_>],
) -> usize {
    receipts
        .iter()
        .filter(|receipt| {
            panes
                .get_mut(receipt.index)
                .filter(|pane| pane.id == receipt.pane_id)
                .is_some_and(|pane| receipt.try_apply(pane.grid))
        })
        .count()
}

/// A test glyph attached to one terminal row: the row's pane and viewport slot, and the glyph's
/// `(x, y, w, h)` rectangle in surface pixels and color.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InjectedRowGlyph {
    /// The pane whose row draws the glyph.
    pub pane_id: u64,
    /// The viewport slot of that row.
    pub slot: u16,
    /// The glyph's rectangle in surface pixels.
    pub rect_px: (f32, f32, f32, f32),
    /// The glyph's color.
    pub color: [f32; 4],
}

/// Append the row glyph seam's instance after the row at `slot` of `pane_id`, as its own span,
/// when `seam` names that row.
pub(super) fn push_injected_row_glyph(
    atlas: &mut GlyphAtlas,
    seam: Option<InjectedRowGlyph>,
    pane_id: u64,
    slot: u16,
    glyphs: &mut Vec<GlyphInstance>,
    row_spans: &mut Vec<RowGlyphSpan>,
    surface: (f32, f32),
) {
    let Some(InjectedRowGlyph { pane_id: owner, slot: owner_slot, rect_px, color }) = seam else {
        // When: `seam` is None, as in production, nothing is appended.
        return;
    };
    if owner != pane_id || owner_slot != slot {
        // When: `owner` or `owner_slot` names another row than `pane_id` and `slot`, nothing is drawn here.
        return;
    }
    let (sw, sh) = surface;
    let base = glyphs.len();
    glyphs.extend(crate::cursor::seam_glyph(atlas, rect_px, color, sw, sh));
    row_spans.push(RowGlyphSpan::new(glyphs, base..glyphs.len(), sw, sh));
}

/// The glyph atlas facts a memory snapshot reports per renderer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlyphAtlasFacts {
    /// Current square dimension in pixels.
    pub dim: u32,
    /// Area of every resident tile at its tile size.
    pub packed_pixels: u64,
    /// Size doublings since construction.
    pub growths: u64,
    /// LRU evictions since the last reset.
    pub evictions: u64,
    /// Fit label: `256`, `512`, `1024`, `2048`, `no_headroom`, `does_not_fit` or `evicted`.
    pub fit: String,
    /// Largest resident tile width and height.
    pub max_tile: [u32; 2],
}

impl GlyphAtlasFacts {
    /// Read the facts from `atlas`.
    #[must_use]
    pub fn of(atlas: &GlyphAtlas) -> Self {
        Self {
            dim: atlas.width(),
            packed_pixels: atlas.packed_pixels(),
            growths: atlas.growths(),
            evictions: atlas.evictions(),
            fit: atlas.fit_outcome().label(),
            max_tile: atlas.max_tile_dims(),
        }
    }
}

/// Settle one releasing frame for a caller that still borrows its grids: apply the receipts only
/// when the frame was presented, and return its outcome. Every other outcome keeps the dirty rows.
pub fn settle_borrowed_frame(
    frame: FrameOutcome,
    panes: &mut [sonicterm_render_model::PaneRender<'_>],
) -> PresentOutcome {
    let FrameOutcome { outcome, receipts } = frame;
    if matches!(outcome, PresentOutcome::Presented) {
        // Presented: nothing could change the borrowed grids since assembly, so apply the receipts.
        acknowledge_receipts(&receipts, panes);
    }
    outcome
}

/// What `render_releasing` reports: how the frame ended, and when it presented, one metadata receipt
/// per pane its plan acknowledges. Receipts are non-empty only for `Presented`.
#[derive(Debug)]
#[must_use = "a presented frame's receipts must be applied, or its dirt is drawn again"]
pub struct FrameOutcome {
    /// How the frame ended.
    pub outcome: PresentOutcome,
    /// What a presented frame drew of each acknowledged pane.
    pub receipts: Vec<sonicterm_render_model::AckReceipt>,
}

impl FrameOutcome {
    pub(super) fn without_receipts(outcome: PresentOutcome) -> Self {
        FrameOutcome { outcome, receipts: Vec::new() }
    }
}

/// The renderer's `UploadStaging` part from its `vertex` scratch and its two atlas uploads; a
/// free function so a headless test sums exactly what the renderer reports.
pub(crate) fn upload_staging_amount(
    vertex: ResourceAmount,
    glyph_upload: &AtlasUpload,
    image_upload: &AtlasUpload,
) -> ResourceAmount {
    let uploads = glyph_upload.retained_bytes() + image_upload.retained_bytes();
    ResourceAmount { bytes: vertex.bytes + uploads, items: vertex.items }
}

/// Settle what a frame's outcome leaves retained: only `Presented` with its plan commits the
/// staged ink records and glyph slot keys, counts a partial frame, keeps the plan's key and
/// returns its receipts. Every other outcome discards the staged records and slot keys, keeps
/// the committed ones (their pixels are still on screen), returns no receipt and clears the key,
/// so the retry plans a Full first frame.
pub(super) fn settle_retained_frame(
    last_frame_key: &mut Option<FrameKey>,
    row_ink: &mut crate::row_ink::RowInkTable,
    row_glyphs: &mut sonicterm_text::row_glyph_cache::RowGlyphCache,
    outcome: &PresentOutcome,
    presented_plan: Option<FramePlan>,
    receipts: Vec<sonicterm_render_model::AckReceipt>,
) -> Vec<sonicterm_render_model::AckReceipt> {
    match (outcome, presented_plan) {
        (PresentOutcome::Presented, Some(plan)) => {
            row_ink.commit(&plan.drawn_row_counts());
            row_glyphs.commit_slots();
            crate::frame_stats::note_partial_frame(plan.mode == RenderMode::Partial);
            *last_frame_key = Some(plan.key);
            receipts
        }
        _ => {
            // Nothing this frame assembled reached the screen, so nothing of it is kept.
            row_ink.begin_frame();
            row_glyphs.discard_staged();
            *last_frame_key = None;
            Vec::new()
        }
    }
}

/// Assemble one frame through `assemble`, called with whether the pass is forced Full. A first
/// pass whose partial plan's final damage reached a row it did not emit is assembled again forced
/// Full under the same guards, so the scissor never erases unemitted ink; that fallback is counted
/// here, once, whatever mode the second pass plans. The passes' time is one assembly sample.
pub(super) fn assemble_with_fallback(
    mut assemble: impl FnMut(bool) -> Result<Assembled>,
) -> Result<Assembled> {
    let assembled = match assemble(false) {
        Ok(Assembled::PartialFallback) => {
            crate::frame_stats::note_partial_fallback();
            assemble(true)
        }
        other => other,
    };
    crate::frame_stats::finish_assembly();
    assembled
}

/// Lend `source` once and decide the exits that need no renderer, in their existing order: an empty
/// source is `NoPanes`, then a device that no longer accepts work is `Unavailable`; only otherwise does
/// `assemble` run. The source is dropped before this returns.
pub(super) fn lend_and_assemble(
    source: impl sonicterm_render_model::FrameSource,
    accepts_gpu_work: bool,
    assemble: impl for<'slice, 'grid> FnOnce(
        &'slice mut [sonicterm_render_model::PaneRender<'grid>],
    ) -> Result<Assembled>,
) -> Result<Assembled> {
    source.lend(|panes| {
        if panes.is_empty() {
            // When: `panes` is empty, no grid is available to draw, so skip the frame.
            return Ok(Assembled::NoPanes);
        }
        if !accepts_gpu_work {
            // When: `accepts_gpu_work` is false, frames are skipped and their dirty state kept.
            return Ok(Assembled::Unavailable);
        }
        assemble(panes)
    })
}

/// The outcome of an exit that needs no renderer: an empty source is skipped with no receipts. Any
/// other assembly is handed back for presentation.
pub(super) fn settle_without_renderer(
    assembled: Assembled,
) -> std::result::Result<FrameOutcome, Assembled> {
    match assembled {
        Assembled::NoPanes => {
            Ok(FrameOutcome::without_receipts(PresentOutcome::Skipped(SkipReason::NoPanes)))
        }
        other => Err(other),
    }
}

/// What assembling a frame decided, owning everything presentation needs and borrowing no grid, so
/// the frame's source is released before any of it is presented. The typed exits come first, in the
/// order assembly checks them.
pub(super) enum Assembled {
    /// The source lent no pane.
    NoPanes,
    /// The device stopped accepting work.
    Unavailable,
    /// The frame key is unchanged; `focus_flash` asks for the next flash frame.
    Unchanged { focus_flash: bool },
    /// Nothing drawable changed; remember the key.
    Noop(Box<FrameKey>),
    /// The glyph atlas changed during assembly, so its UVs are stale.
    AtlasRetry { stamp: GlyphContentStamp, evictions: u64 },
    /// A partial plan's final damage reached a row it did not emit; assemble it again as Full.
    PartialFallback,
    /// Drawable batches and their plan.
    Layers(Box<AssembledLayers>),
}

/// What ending one assembly pass reads besides its plan and panes: the atlas content stamp when
/// the pass started and now, the evictions when it started, and the recolors and tab-title ink of
/// the last presented frame and of this pass.
pub(super) struct PassEnd {
    pub(super) atlas_stamp_at_start: GlyphContentStamp,
    pub(super) atlas_stamp_now: GlyphContentStamp,
    pub(super) atlas_evictions_at_start: u64,
    pub(super) previous_recolor: crate::cursor::RecolorRecord,
    pub(super) current_recolor: crate::cursor::RecolorRecord,
    pub(super) previous_tab_ink: crate::cursor::RecolorBounds,
    pub(super) current_tab_ink: crate::cursor::RecolorBounds,
}

/// An assembled frame's owned batches, geometry, plan and receipts.
pub(super) struct AssembledLayers {
    pub(super) surface_width: f32,
    pub(super) surface_height: f32,
    pub(super) subpixel_aa: SubpixelAaMode,
    /// The pass's lease on the renderer's frame scratch, holding this frame's quads, images and
    /// glyphs. It travels with the frame and restores the scratch when it drops, on every
    /// presentation outcome and on unwinding.
    pub(super) scratch: frame_scratch::ScratchLease,
    pub(super) field_candidates: PresentedFields,
    pub(super) missing_chars: Vec<char>,
    /// Chrome characters this frame drew as tofu or dropped; published only if it presents.
    pub(super) missing_chrome_chars: Vec<char>,
    pub(super) gpu_timing: present::FrameTiming,
    pub(super) plan: FramePlan,
    pub(super) receipts: Vec<sonicterm_render_model::AckReceipt>,
    /// This frame's cursor recolors; kept as `last_recolor` only if the frame presents.
    pub(super) recolor: crate::cursor::RecolorRecord,
    /// Where this frame's tab-title glyphs draw; kept as `last_tab_ink` only if it presents.
    pub(super) tab_ink: crate::cursor::RecolorBounds,
}

#[cfg(test)]
#[path = "core_contracts_tests.rs"]
mod core_contracts_tests;
