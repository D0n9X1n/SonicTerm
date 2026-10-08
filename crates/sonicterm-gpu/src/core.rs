//! GPU renderer for the terminal grid using wgpu 29.
//!
//! The legacy `glyphon` chrome path is
//! gone. Every chrome string (tab titles, palette, search, IME,
//! broadcast, drag chip, quick-select hints) flows through
//! [`crate::chrome_text::layout`] → the shared `GlyphAtlas` →
//! [`crate::wezterm_pipeline::WeztermPipeline`]. No second font system,
//! no second atlas, no second render pass.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
#[cfg(test)]
use sonicterm_render_model::boundary::cfg::config::ScrollbarMode;
use sonicterm_render_model::boundary::cfg::config::{
    BackdropKind, SoftwareRenderMode, SubpixelAaMode,
};
use sonicterm_render_model::boundary::cfg::theme::{Color as ThemeColor, Theme};
use sonicterm_render_model::boundary::grid::grid::{
    bounded_grid_size, Cell, CellFlags, Color, Grid, UnderlineStyle,
};
use sonicterm_types::{GlyphRasterVariant, ResourceAmount, ResourceClass};

use crate::field_geometry::{
    clip_glyphs_to_rect, field_caret_rect, field_hit, plan_field, FieldBoundaries, FieldHit,
    FieldHitMode, FieldKind, FieldPlacement, FieldRect, FieldText, PresentedFields,
};
use wgpu::{
    CompositeAlphaMode, DeviceDescriptor, Instance, InstanceDescriptor, LoadOp, Operations,
    PresentMode, RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions,
    SurfaceConfiguration, Texture, TextureDescriptor, TextureDimension, TextureFormat,
    TextureUsages, TextureView, TextureViewDescriptor,
};
use winit::{event_loop::ActiveEventLoop, window::Window};

use crate::chrome_text::{self, ChromeAttrs, ChromeClip};
use crate::color::{
    chrome_color_to_linear_rgba, dim_toward, hex_to_chrome_color, hex_to_premultiplied_rgba,
    hex_to_wgpu_with_alpha, ChromeColor,
};
use crate::cursor::{recolor_cursor_glyphs_in, InactivePaneCursor, RecolorOutcome, RowGlyphSpan};
use crate::device_errors::{
    create_frame_fault_probe, destroy_and_await_loss, install_device_error_handlers,
    run_isolated_validation, DeviceErrorSnapshot, DeviceErrorState, DeviceStateWaker, GpuFaultKind,
};
use sonicterm_render_model::boundary::ui::drag_chip::{DragChipOverlay, DragChipVisual};
use sonicterm_render_model::boundary::ui::tab_spans::tab_title_font_size;

const PANE_FOCUS_FLASH_DURATION: Duration = Duration::from_millis(360);
const PANE_FOCUS_FLASH_BUCKET: Duration = Duration::from_millis(16);

#[cfg(test)]
use crate::frame_plan::{
    decide_render_mode, dirty_rows_damage_rect_with_ink_pad, effective_scrollbar_bucket,
    pane_damage_rect_with_ink_pad, pane_scrollbar_identity, RenderSignals,
};
use crate::frame_plan::{
    CopyModeIdentity, CursorCell, FrameFacts, FrameKey, FramePlan, PaneMetadata, PlannedPane,
    RenderMode, WindowIdentity,
};

#[path = "atlas_lifecycle.rs"]
mod atlas_lifecycle;

#[path = "frame_fonts.rs"]
mod frame_fonts;
use crate::frame_stats::CountingRasterizer;
pub use frame_fonts::{FontChange, FrameFonts};
/// The App's wake for a fallback completion, called with the completed notice's id from the
/// fallback worker thread. It must only post an event and never touch renderer state.
pub type FontFallbackWaker = std::sync::Arc<dyn Fn(u64) + Send + Sync>;
#[path = "init_timing.rs"]
mod init_timing;
use init_timing::{InitOutcome, InitTiming};

// The presenters stay a private child module so they keep direct access to renderer fields.
#[path = "present.rs"]
mod present;
#[path = "rebind.rs"]
mod rebind;
#[path = "recovery_context.rs"]
mod recovery_context;
#[cfg(any(target_os = "macos", test))]
pub use present::SurfaceAvailability;
use present::{lap, FrameBatches, FrameLayers};
pub use present::{PresentOutcome, SkipReason, SurfaceRetryReason, SuspendedContext};
pub use rebind::PreparedRebind;
pub use recovery_context::{CandidateSurface, ContextRequest, RecoveredContext, RequestFailure};

/// The metadata receipts a presented `plan` issues: one per pane the plan acknowledges, read from the
/// grids it was assembled from. They carry no grid borrow, so they outlive the frame's source.
fn presented_receipts(
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
fn push_injected_row_glyph(
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
    fn without_receipts(outcome: PresentOutcome) -> Self {
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
fn settle_retained_frame(
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
fn assemble_with_fallback(
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
fn lend_and_assemble(
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
fn settle_without_renderer(assembled: Assembled) -> std::result::Result<FrameOutcome, Assembled> {
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
enum Assembled {
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
struct PassEnd {
    atlas_stamp_at_start: GlyphContentStamp,
    atlas_stamp_now: GlyphContentStamp,
    atlas_evictions_at_start: u64,
    previous_recolor: crate::cursor::RecolorRecord,
    current_recolor: crate::cursor::RecolorRecord,
    previous_tab_ink: crate::cursor::RecolorBounds,
    current_tab_ink: crate::cursor::RecolorBounds,
}

/// An assembled frame's owned batches, geometry, plan and receipts.
struct AssembledLayers {
    surface_width: f32,
    surface_height: f32,
    subpixel_aa: SubpixelAaMode,
    /// The pass's lease on the renderer's frame scratch, holding this frame's quads, images and
    /// glyphs. It travels with the frame and restores the scratch when it drops, on every
    /// presentation outcome and on unwinding.
    scratch: frame_scratch::ScratchLease,
    field_candidates: PresentedFields,
    missing_chars: Vec<char>,
    /// Chrome characters this frame drew as tofu or dropped; published only if it presents.
    missing_chrome_chars: Vec<char>,
    gpu_timing: present::FrameTiming,
    plan: FramePlan,
    receipts: Vec<sonicterm_render_model::AckReceipt>,
    /// This frame's cursor recolors; kept as `last_recolor` only if the frame presents.
    recolor: crate::cursor::RecolorRecord,
    /// Where this frame's tab-title glyphs draw; kept as `last_tab_ink` only if it presents.
    tab_ink: crate::cursor::RecolorBounds,
}

fn pane_focus_flash_sample(elapsed: Duration) -> Option<(u8, f32)> {
    if elapsed >= PANE_FOCUS_FLASH_DURATION {
        // When: `elapsed` reaches the bounded lifetime, no flash frame remains.
        return None;
    }
    let bucket = ((elapsed.as_millis() / PANE_FOCUS_FLASH_BUCKET.as_millis()) + 1)
        .min(u128::from(u8::MAX)) as u8;
    let t = elapsed.as_secs_f32() / PANE_FOCUS_FLASH_DURATION.as_secs_f32();
    Some((bucket, (1.0 - t).powi(2) * 0.12))
}

fn hovered_url_needs_accent(
    hovered: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
) -> bool {
    hovered.is_some_and(|h| h.active)
}

fn hovered_url_for_pane_row(
    hovered: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    pane_id: u64,
    row: u16,
) -> Option<sonicterm_render_model::inputs::HoveredUrlCells> {
    let hovered = hovered.filter(|hovered| hovered.pane_id == pane_id)?;
    let span = hovered.span_for_row(row)?;
    sonicterm_render_model::inputs::HoveredUrlCells::new(pane_id, [span], hovered.active)
}

/// The inclusive columns of `row`'s active (recoloring) hover fragment, folded into that row's
/// content key; `None` for an absent or hint-only hover, which leaves glyph colours unchanged.
fn hovered_url_row_key_span(
    hovered: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    row: u16,
) -> Option<(u16, u16)> {
    let hovered = hovered.filter(|hovered| hovered.active)?;
    let span = hovered.span_for_row(row)?;
    Some((span.start_col, span.end_col))
}

#[allow(clippy::too_many_arguments)]
fn hovered_url_span_rect(
    span: sonicterm_render_model::inputs::HoveredUrlSpan,
    cols: u16,
    rows: u16,
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    snapped_cell_x: &[f32],
) -> Option<(f32, f32, f32, f32)> {
    if cols == 0 || span.row >= rows || span.start_col >= cols || span.end_col <= span.start_col {
        // When: `span` has no visible row or column coverage, emit no underline rectangle.
        return None;
    }
    let start_col = span.start_col as usize;
    let end_col = span.end_col.min(cols) as usize;
    let x = snapped_cell_x
        .get(start_col)
        .copied()
        .unwrap_or(origin_x + f32::from(span.start_col) * cell_w);
    let width = snapped_cell_x
        .get(end_col)
        .map(|right| right - x)
        .unwrap_or_else(|| f32::from(span.end_col.min(cols) - span.start_col) * cell_w);
    (width > 0.0).then_some((x, origin_y + f32::from(span.row) * cell_h, width, cell_h))
}

pub(crate) fn palette_footer_font_size(body_font_size: f32) -> f32 {
    (body_font_size - 1.0).max(1.0)
}

// Returns label baseline, subtitle top, and subtitle baseline in row-relative raster pixels.
fn palette_detail_positions(
    row_height: f32,
    label_size: f32,
    detail_size: f32,
    gap: f32,
) -> (f32, f32, f32) {
    let top = ((row_height - label_size - gap - detail_size) * 0.5).max(0.0);
    let detail_top = (top + label_size + gap).min((row_height - detail_size).max(0.0));
    (top + label_size * 0.8, detail_top, detail_top + detail_size * 0.8)
}

const PALETTE_FOOTER_INSET_X: f32 = 18.0;
const READ_ONLY_BADGE_ICON: &str = "";
const READ_ONLY_BADGE_LABEL: &str = "READONLY";
const SEARCH_BADGE_ICON: &str = "";
const NOTIFICATION_CLOSE_ICON: &str = "";
const READ_ONLY_BADGE_W: f32 = 250.0;
const READ_ONLY_BADGE_H: f32 = SEARCH_BAR_HEIGHT;
const READ_ONLY_BADGE_MARGIN: f32 = 12.0;
const READ_ONLY_BADGE_PAD_RIGHT: f32 = 15.0;
const READ_ONLY_BADGE_BASELINE_NUDGE_Y: f32 = -2.0;
const READ_ONLY_BADGE_RADIUS: f32 = 7.0;

/// Renderer compositor settings that affect surface configuration.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceAppearance {
    /// System backdrop material requested by config.
    pub backdrop: BackdropKind,
    /// Theme background opacity.
    pub opacity: f32,
    /// Scrollbar visibility policy. `Auto` consumes per-pane opacity from the
    /// app, `Always` draws whenever scrollback exists, and `Never` suppresses it.
    pub scrollbar: sonicterm_render_model::boundary::cfg::config::ScrollbarMode,
    /// Padding between overlay panel chrome and inner content.
    pub panel_padding: f32,
    /// User override for the software-render degrade path.
    pub software_render_mode: SoftwareRenderMode,
}

fn estimate_badge_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().map(|ch| if ch.is_ascii() { 0.58 } else { 1.0 }).sum::<f32>() * font_size
}

fn conservative_badge_text_width(fallback: f32, shaped: Option<f32>) -> f32 {
    shaped.filter(|width| width.is_finite() && *width >= 0.0).unwrap_or(fallback).max(fallback)
}

/// Content width of the search badge: icon, gap and label. `measure` returns a text's shaped
/// width, or `None` when it cannot be shaped; with no measure, or when either text fails, the
/// conservative estimate stands, and a shaped width below the estimate never shrinks the badge.
fn search_badge_content_width(
    icon: &str,
    label: &str,
    font_size: f32,
    gap: f32,
    measure: Option<&mut dyn FnMut(&str) -> Option<f32>>,
) -> f32 {
    let fallback = estimate_badge_text_width(icon, font_size)
        + gap
        + estimate_badge_text_width(label, font_size);
    let shaped = measure.and_then(|measure| {
        let icon_w = measure(icon)?;
        let label_w = measure(label)?;
        Some(icon_w + gap + label_w)
    });
    conservative_badge_text_width(fallback, shaped)
}

/// True when an IME preedit string carries visible ink worth drawing an
/// inline composition overlay for.
///
/// The inline preedit overlay (composing glyphs + a one-cell-min underline
/// at the terminal cursor) must only paint when there is real composition
/// text. A preedit that is empty **or whitespace-only** has no glyph ink,
/// yet the underline quad is clamped to `max(self.cell_w)` — so drawing it
/// leaves a stray ~1-cell underscore mark at the cursor that lingers until
/// the next repaint. macOS can momentarily deliver a single-space marked
/// string during ordinary typing, which is exactly that case.
///
/// Real CJK / multi-key composition always carries non-whitespace ink, so
/// gating on this never suppresses a genuine composition overlay.
fn preedit_has_visible_ink(preedit: &str) -> bool {
    preedit.chars().any(|c| !c.is_whitespace())
}

/// Horizontal advance (px) for the terminal cursor caret while an inline IME
/// composition is active, so the cursor block sits at the composition
/// insertion point WezTerm-style.
///
/// CRITICAL: this MUST be gated on the SAME predicate as the inline
/// preedit glyph overlay — [`preedit_has_visible_ink`]. macOS delivers a
/// whitespace-only marked string during ordinary typing (and on bare Enter)
/// whenever a CJK/Pinyin input source is active, even for plain Latin. The
/// glyph overlay correctly suppresses that case, but if the caret advance
/// does NOT, the cursor-colored block gets shoved right by the width of the
/// (invisible) whitespace and floats in empty prompt space with no glyph
/// under it — the stray "yellow line/block" users reported. Returning 0 for
/// no-visible-ink keeps the cursor at the grid's real column. Real CJK
/// composition always carries non-whitespace ink, so genuine compositions
/// still advance the caret. Pure so the gate is unit-testable without a GPU.
fn preedit_caret_advance(preedit: &str, caret_byte: usize, font_size: f32) -> f32 {
    if !preedit_has_visible_ink(preedit) {
        // When: `preedit_has_visible_ink` is false — the whitespace-only marked
        // string macOS sends for Latin typing. Advancing would strand the block.
        return 0.0;
    }
    let mut cb = caret_byte.min(preedit.len());
    if !preedit.is_char_boundary(cb) {
        cb = preedit.len();
    }
    estimate_badge_text_width(&preedit[..cb], font_size)
}

/// Opaque-background rect for the inline IME preedit.
///
/// Returns `(x, y, w, h)` in renderer pixels for the mask that is laid
/// down behind the composing run so it stays legible over whatever the
/// app already painted in those cells (placeholder/hint text). It must:
/// * start at `start_x` — the cursor cell's left edge — so the mask's left
///   edge aligns with where the glyphs begin (they are nudged right by
///   `pad`, so the mask starting at `start_x` fully contains them);
/// * span `pre_w + pad` — the same width used to emit the glyphs plus the
///   right-nudge — so the mask covers the whole run and no wider, never
///   bleeding onto adjacent cells;
/// * be exactly one line tall.
///
/// Pure so the geometry is unit-testable without a GPU context.
fn preedit_bg_rect(
    start_x: f32,
    top_y: f32,
    pre_w: f32,
    pad: f32,
    line_h: f32,
) -> (f32, f32, f32, f32) {
    (start_x, top_y, pre_w + pad, line_h)
}

/// Renderer initialization settings derived from config.
#[derive(Debug, Clone, Copy)]
pub struct RendererSettings<'a> {
    /// Font family to use for terminal text.
    pub font_family: &'a str,
    /// Packaged directories searched before platform-native font discovery.
    pub font_dirs: &'a [PathBuf],
    /// Font size in points.
    pub font_size: f32,
    /// Line-height multiplier.
    pub line_height_mult: f32,
    /// Regular-text coverage scale.
    pub font_weight_scale: f32,
    /// Requested LCD subpixel coverage order.
    pub subpixel_aa: SubpixelAaMode,
    /// Window padding in logical pixels: left, right, top, bottom.
    pub padding: [f32; 4],
    /// Surface/backdrop settings.
    pub appearance: SurfaceAppearance,
    /// Stable renderer role used by memory and timing diagnostics.
    pub role: &'static str,
    /// Size the renderer's glyph atlas starts at before it grows on demand.
    pub glyph_atlas_start: GlyphAtlasStart,
}

/// Which start size a renderer's glyph atlas takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlyphAtlasStart {
    /// The measured start size for the window's scale factor.
    #[default]
    Normal,
    /// The 256-pixel floor, for warm-pool renderers that may never draw.
    Minimum,
}

/// Square start dimension of a glyph atlas for `scale_factor` and `start`.
///
/// `Normal` takes the scale-1 start up to 1.5 and the scale-2 start above it; `Minimum` is the
/// floor. A later scale change resets the atlas in place at its current size and never shrinks it.
#[must_use]
pub fn start_dim(scale_factor: f32, start: GlyphAtlasStart) -> u32 {
    let GlyphAtlasStart::Normal = start else {
        // When: start is Minimum the renderer may never draw, so its atlas takes the 256 floor.
        return sonicterm_text::glyph_atlas::MIN_ATLAS_DIM;
    };
    if scale_factor <= 1.5 {
        sonicterm_text::glyph_atlas::START_ATLAS_DIM_1X
    } else {
        // When: scale_factor is above 1.5 the window rasterizes at scale 2, so it takes that start.
        sonicterm_text::glyph_atlas::START_ATLAS_DIM_2X
    }
}

fn cursor_color_from_theme(theme: &Theme) -> [f32; 4] {
    hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 1.0)
}

fn cursor_text_color_from_theme(theme: &Theme) -> [f32; 4] {
    hex_to_premultiplied_rgba(theme.colors.cursor_text.0.as_str(), 1.0)
}

fn active_cursor_color(base: [f32; 4]) -> [f32; 4] {
    base
}

pub(crate) fn glyph_flags(is_color: bool, is_subpixel: bool) -> [f32; 4] {
    [if is_color { 1.0 } else { 0.0 }, if is_subpixel { 1.0 } else { 0.0 }, 0.0, 0.0]
}

fn effective_font_weight_scale(scale: f32) -> f32 {
    if scale.is_finite() && (0.5..=5.0).contains(&scale) {
        scale
    } else {
        // When: `scale` is NaN, infinite, or outside 0.5..=5.0. 1.0 is the
        // weight the font was drawn at, so a bad config still renders.
        1.0
    }
}

#[path = "frame_scratch.rs"]
mod frame_scratch;

#[path = "tab_title_font.rs"]
mod tab_title_font;
use tab_title_font::TabTitleFont;

pub(crate) struct RendererFontStacks {
    pub(crate) body: Option<sonicterm_engine::FontStack>,
    pub(crate) tab_title: Option<sonicterm_engine::FontStack>,
    pub(crate) palette_footer: Option<sonicterm_engine::FontStack>,
}

pub(crate) fn renderer_font_stacks(
    family: &str,
    body_size: f32,
    dpi: usize,
    weight_scale: f32,
    font_dirs: &[PathBuf],
) -> RendererFontStacks {
    let body = sonicterm_engine::FontStack::try_new_full_with_weight_and_font_dirs(
        family,
        f64::from(body_size),
        dpi,
        weight_scale,
        font_dirs,
    )
    .ok();
    renderer_font_views(body, body_size)
}

/// The renderer's three stacks from `body`: the tab-title and palette-footer views share its
/// configuration at their own sizes for a `body_size` grid font.
pub(crate) fn renderer_font_views(
    body: Option<sonicterm_engine::FontStack>,
    body_size: f32,
) -> RendererFontStacks {
    let tab_title =
        body.as_ref().map(|stack| stack.with_font_size(f64::from(tab_title_font_size(body_size))));
    let palette_footer = body
        .as_ref()
        .map(|stack| stack.with_font_size(f64::from(palette_footer_font_size(body_size))));
    RendererFontStacks { body, tab_title, palette_footer }
}

fn software_block_glyph_target_rect(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) -> (f32, f32, f32, f32) {
    let left = (left - 0.5).ceil();
    let top = (top - 0.5).ceil();
    let right = (right - 0.5).ceil().max(left + 1.0);
    let bottom = (bottom - 0.5).ceil().max(top + 1.0);
    (left, top, right - left, bottom - top)
}

/// Apply HarfBuzz positioning while preserving the raster tile's size.
fn positioned_shaped_glyph_rect(
    natural: (f32, f32, f32, f32),
    x_offset: f32,
    y_offset: f32,
) -> (f32, f32, f32, f32) {
    (natural.0 + x_offset, natural.1 + y_offset, natural.2, natural.3)
}

/// Resolve a shaped glyph's horizontal offset from its cluster-local running pen.
fn shaped_cluster_x_offset(
    prior_col: &mut Option<u16>,
    pen_x: &mut f32,
    glyph: &sonicterm_text::shape::ShapedGlyph,
) -> f32 {
    if *prior_col != Some(glyph.lead_col) {
        // A new terminal cluster anchors its pen to the lead cell instead of carrying the
        // preceding cluster's accumulated advance.
        *prior_col = Some(glyph.lead_col);
        *pen_x = 0.0;
    }
    let offset = *pen_x + glyph.x_offset;
    *pen_x += glyph.x_advance;
    offset
}

/// Reserve vertical retained-damage margin for native glyph bearings and GPOS offsets.
fn terminal_vertical_ink_pad(cell_h: f32, metrics: Option<sonicterm_engine::CellMetricsPx>) -> f32 {
    metrics.map(|metrics| metrics.cell_h as f32).unwrap_or(cell_h).max(0.0).ceil()
}

/// Whether a glyph is a standalone Claude Code circle marker that projection fits inside its
/// cell: an approved codepoint in a one-cell, narrow cluster with no combining extras.
pub(crate) fn status_marker_fit_eligible(
    ch: char,
    cluster_cells: usize,
    is_wide: bool,
    has_extras: bool,
) -> bool {
    matches!(ch, '\u{23fa}' | '\u{25ef}' | '\u{25cf}')
        && cluster_cells == 1
        && !is_wide
        && !has_extras
}

/// Scale `natural` uniformly to fit `cell` and centre it; a non-positive size on either side has
/// no meaningful ratio, so the natural rectangle is kept.
pub(crate) fn fit_status_marker_rect(
    natural: (f32, f32, f32, f32),
    cell: (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    let (_, _, glyph_w, glyph_h) = natural;
    let (cell_x, cell_y, cell_w, cell_h) = cell;
    // When: glyph_w, glyph_h, cell_w, or cell_h is non-positive, no meaningful
    // fit ratio exists, so preserve the natural rectangle.
    if glyph_w <= 0.0 || glyph_h <= 0.0 || cell_w <= 0.0 || cell_h <= 0.0 {
        return natural;
    }
    let scale = (cell_w / glyph_w).min(cell_h / glyph_h);
    let fitted_w = glyph_w * scale;
    let fitted_h = glyph_h * scale;
    (cell_x + (cell_w - fitted_w) * 0.5, cell_y + (cell_h - fitted_h) * 0.5, fitted_w, fitted_h)
}

/// Normalizes standalone Claude Code circle markers inside one cell without distortion: the
/// eligibility predicate and the fit composed, as shaping decides and projection applies them.
#[cfg(test)]
pub(crate) fn fit_single_cell_status_marker(
    ch: char,
    cluster_cells: usize,
    is_wide: bool,
    has_extras: bool,
    natural: (f32, f32, f32, f32),
    cell: (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    if status_marker_fit_eligible(ch, cluster_cells, is_wide, has_extras) {
        fit_status_marker_rect(natural, cell)
    } else {
        // When: `status_marker_fit_eligible` is false, the rasterizer's geometry is kept exactly.
        natural
    }
}

fn tab_bar_hash(tabs: &TabBar, now: Instant) -> u64 {
    // The limits the strip is laid out with, so a reload repaints it in the frame that draws it.
    let (min_px, max_px) =
        sonicterm_render_model::boundary::ui::tabbar_view::tab_width_limits_of(tabs);
    tab_bar_hash_with_limits(tabs, now, min_px, max_px)
}

/// [`tab_bar_hash`] with explicit tab width limits, in logical pixels.
fn tab_bar_hash_with_limits(
    tabs: &TabBar,
    now: Instant,
    min_tab_width_px: f32,
    max_tab_width_px: f32,
) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hash = DefaultHasher::new();
    // Tab geometry also depends on both width limits, even when every title is unchanged.
    min_tab_width_px.to_bits().hash(&mut hash);
    max_tab_width_px.to_bits().hash(&mut hash);
    tabs.active_index().hash(&mut hash);
    let active_index = tabs.active_index();
    for (index, tab) in tabs.tabs().iter().enumerate() {
        tab.id.0.hash(&mut hash);
        tab.title.hash(&mut hash);
        // A stored width moves the bar with an unchanged title, as when a held width applies.
        tab.content_width_px().map(f32::to_bits).hash(&mut hash);
        tab.custom_color.hash(&mut hash);
        tab.foreground_privileged.hash(&mut hash);
        command_status_hash(&tab.command, now, index == active_index).hash(&mut hash);
    }
    hash.finish()
}

/// Resolve the title text colour for a single tab, honouring hover.
///
/// Default tabs use the active foreground when active or hovered. Custom
/// colours stay full-strength while active, hovered, or panel-focused and
/// otherwise recede to the standard unfocused alpha.
fn tab_title_color(
    custom_color: Option<&str>,
    active: bool,
    hovered: bool,
    active_panel_focused: bool,
    active_fg: ChromeColor,
    inactive_fg: ChromeColor,
) -> ChromeColor {
    match custom_color {
        None => {
            if active || hovered {
                active_fg
            } else {
                // When: neither `active` nor `hovered`, so nothing highlights
                // the tab and it recedes to the dimmer foreground.
                inactive_fg
            }
        }
        Some(hex) => {
            let color = hex_to_chrome_color(hex);
            // Full strength when the tab is highlighted (active/hovered) or
            // lives in the focused panel; otherwise recede with the rest of
            // the unfocused chrome.
            if active || hovered || active_panel_focused {
                color
            } else {
                // When: not `active`, `hovered`, or `active_panel_focused` — the
                // user's colour recedes with the rest of the unfocused chrome.
                scale_chrome_text_alpha(color, 0.55)
            }
        }
    }
}

const PRIVILEGE_BADGE_SIZE_PX: f32 = 18.0;
const PRIVILEGE_BADGE_GAP_PX: f32 = 6.0;
#[cfg(test)]
const PRIVILEGE_BADGE_QUAD_COUNT: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq)]
struct TabTitleBlockPlacement {
    badge_rect: Option<TabTitleRect>,
    text_x: f32,
    text_clip: TabTitleRect,
}

fn scaled_privilege_badge_metrics(scale: f32) -> (f32, f32) {
    let scale = scale.max(0.1);
    (PRIVILEGE_BADGE_SIZE_PX * scale, PRIVILEGE_BADGE_GAP_PX * scale)
}

/// Raster-pixel width reserved for the privilege marker and its gap before the
/// title text, or zero when no marker is drawn.
fn privilege_marker_reserve_px(privileged: bool, scale: f32) -> f32 {
    if privileged {
        let (badge, gap) = scaled_privilege_badge_metrics(scale);
        badge + gap
    } else {
        // When: `privileged` is false, no marker is drawn and the title keeps the whole rect.
        0.0
    }
}

/// Drop the background-quad cache entries of every live row `planned` carries as dirty, stored at
/// absolute row `scrollback_len + row`; this cache is uncounted. The glyph cache drops nothing for
/// dirt: its rows are keyed by content, so changed content misses by itself.
fn invalidate_planned_quad_rows(
    cache: &mut crate::row_quad_cache::LineQuadCache,
    planned: &PlannedPane,
) {
    for &row in &planned.dirty_live_rows {
        cache.invalidate_row_abs(planned.id, planned.scrollback_len + row as u64);
    }
}

/// Whether the terminal cursor is drawn for a view whose top is absolute row `view_top_abs`.
/// It is drawn only when the view sits at `live_top_abs`, the live top; any scrolled-back view
/// hides it, even one whose rows still include the cursor's live row.
fn terminal_cursor_drawn_at_view(view_top_abs: u64, live_top_abs: u64) -> bool {
    view_top_abs == live_top_abs
}

/// The first column and width in columns of the terminal cursor's block on `grid`: a cursor on
/// either half of a wide character covers both halves.
fn terminal_cursor_columns(grid: &Grid) -> (usize, usize) {
    let row = grid.row(grid.cursor.row);
    let mut first_col = grid.cursor.col as usize;
    let mut span = 1usize;
    if let Some(cell) = row.get(first_col) {
        if cell.flags.contains(CellFlags::WIDE_CONT) && first_col > 0 {
            first_col -= 1;
            span = 2;
        } else if cell.flags.contains(CellFlags::WIDE) {
            // When: `WIDE` — the cursor is on the lead half, so the
            // block spans two columns from where it already is.
            span = 2;
        }
    }
    (first_col, span)
}

/// The cell the terminal cursor is drawn in on `grid` (pane `pane_id`, view top `view_top_abs`),
/// by the draw's own rule: only when `cursor_visible`, `window_focused` and not `read_only`, and
/// only while the view is at the live top. The frame identity records it so the planner can
/// damage the old and new cursor rows.
fn drawn_cursor_cell(
    grid: &Grid,
    pane_id: u64,
    view_top_abs: u64,
    cursor_visible: bool,
    window_focused: bool,
    read_only: bool,
) -> Option<CursorCell> {
    let live_top = grid.scrollback_len() as u64;
    let drawn = cursor_visible
        && window_focused
        && !read_only
        && terminal_cursor_drawn_at_view(view_top_abs, live_top);
    if !drawn {
        // When: `drawn` is false (hidden, unfocused, read-only or scrolled back), no cursor pixels exist.
        return None;
    }
    let (first_col, span) = terminal_cursor_columns(grid);
    Some(CursorCell {
        pane_id,
        slot: grid.cursor.row,
        col: u16::try_from(first_col).unwrap_or(u16::MAX),
        span: u16::try_from(span).unwrap_or(u16::MAX),
    })
}

/// Drawn width of one tab's content in raster pixels: the privilege-marker
/// reserve plus the shaped advance of the badge and title in the tab font.
/// Returns `None` when the text cannot be shaped; with no tab font only the
/// reserve counts, because no text is drawn.
fn tab_content_width_px(
    stack: Option<&sonicterm_engine::FontStack>,
    content: &TabContent<'_>,
    font_size_px: f32,
    scale: f32,
) -> Option<f32> {
    let reserve_px = privilege_marker_reserve_px(content.privileged, scale);
    let Some(stack) = stack else {
        // When: `stack` is absent, no title text is drawn, so only the marker reserve counts.
        return Some(reserve_px);
    };
    let advances = chrome_text::shaped_advances(
        stack,
        &content.display_text(),
        ChromeAttrs::default(),
        font_size_px,
        font_size_px,
    )?;
    Some(reserve_px + advances.iter().map(|(_, advance)| advance).sum::<f32>())
}

/// Identity of the font and scale that tab titles are measured with. A change
/// measures and lays out every tab again, even while the bar is held.
fn tab_font_key(
    family: &str,
    size: f32,
    weight_scale: f32,
    scale_factor: f32,
    has_tab_font: bool,
) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hash = DefaultHasher::new();
    family.hash(&mut hash);
    size.to_bits().hash(&mut hash);
    weight_scale.to_bits().hash(&mut hash);
    scale_factor.to_bits().hash(&mut hash);
    has_tab_font.hash(&mut hash);
    hash.finish()
}

fn tab_title_block_placement(
    rect: TabTitleRect,
    measured_text_width: f32,
    privileged: bool,
    scale: f32,
) -> TabTitleBlockPlacement {
    if !privileged {
        // When: `!privileged`, center text in the unchanged historical title rectangle.
        let text_x = rect.x + ((rect.w - measured_text_width) * 0.5).max(0.0);
        return TabTitleBlockPlacement { badge_rect: None, text_x, text_clip: rect };
    }
    let (badge_size, gap) = scaled_privilege_badge_metrics(scale);
    let badge_size = badge_size.min(rect.h).min(rect.w.max(0.0));
    let total_width = (badge_size + gap + measured_text_width).min(rect.w.max(0.0));
    let block_x = rect.x + ((rect.w - total_width) * 0.5).max(0.0);
    let badge_y = rect.y + ((rect.h - badge_size) * 0.5).max(0.0);
    let badge_rect = TabTitleRect { x: block_x, y: badge_y, w: badge_size, h: badge_size };
    let text_x = (badge_rect.x + badge_rect.w + gap).min(rect.x + rect.w);
    let text_clip =
        TabTitleRect { x: text_x, y: rect.y, w: (rect.x + rect.w - text_x).max(0.0), h: rect.h };
    TabTitleBlockPlacement { badge_rect: Some(badge_rect), text_x, text_clip }
}

fn privilege_lock_color(danger: [f32; 4]) -> [f32; 4] {
    if danger[0] * 0.2126 + danger[1] * 0.7152 + danger[2] * 0.0722 > 0.179 {
        [0.0, 0.0, 0.0, danger[3]]
    } else {
        // When: danger luminance is at most 0.179, white lock geometry has stronger contrast.
        [danger[3], danger[3], danger[3], danger[3]]
    }
}

#[cfg(test)]
fn linear_contrast_ratio(a: [f32; 4], b: [f32; 4]) -> f32 {
    let luminance = |color: [f32; 4]| color[0] * 0.2126 + color[1] * 0.7152 + color[2] * 0.0722;
    let (brighter, darker) = {
        let a = luminance(a);
        let b = luminance(b);
        if a >= b {
            (a, b)
        } else {
            // When: `a < b`, use `b` as the brighter luminance in the contrast ratio.
            (b, a)
        }
    };
    (brighter + 0.05) / (darker + 0.05)
}

fn privilege_lock_rects(badge: TabTitleRect) -> [TabTitleRect; 4] {
    let unit = badge.w.min(badge.h) / 18.0;
    let x = badge.x;
    let y = badge.y;
    [
        TabTitleRect { x: x + 5.0 * unit, y: y + 3.0 * unit, w: 2.0 * unit, h: 6.0 * unit },
        TabTitleRect { x: x + 11.0 * unit, y: y + 3.0 * unit, w: 2.0 * unit, h: 6.0 * unit },
        TabTitleRect { x: x + 7.0 * unit, y: y + 3.0 * unit, w: 4.0 * unit, h: 2.0 * unit },
        TabTitleRect { x: x + 4.0 * unit, y: y + 8.0 * unit, w: 10.0 * unit, h: 7.0 * unit },
    ]
}

fn emit_privilege_badge_quads(
    quads: &mut Vec<QuadInstance>,
    badge: TabTitleRect,
    danger: [f32; 4],
    alpha: f32,
    surface: (f32, f32),
) {
    let alpha = alpha.clamp(0.0, 1.0);
    let scale = |mut color: [f32; 4]| {
        for channel in &mut color {
            *channel *= alpha;
        }
        color
    };
    let (sw, sh) = surface;
    let lock = scale(privilege_lock_color(danger));
    let danger = scale(danger);
    quads.push(QuadInstance::rounded(
        px_to_ndc(badge.x, badge.y, badge.w, badge.h, sw, sh),
        danger,
        [badge.w, badge.h],
        badge.w.min(badge.h) * 0.25,
    ));
    for part in privilege_lock_rects(badge) {
        quads.push(QuadInstance::sharp(px_to_ndc(part.x, part.y, part.w, part.h, sw, sh), lock));
    }
}

fn splitter_color_from_theme(theme: &Theme) -> [f32; 4] {
    let bg = theme.colors.background.color().unwrap_or_else(|| ThemeColor::rgb(0, 0, 0));
    let fg = theme.colors.foreground.color().unwrap_or_else(|| ThemeColor::rgb(255, 255, 255));
    bg.shift_toward(fg, 0.18).to_rgba_f32_linear(1.0)
}

/// Resolve a scrollbar tint from the theme foreground at `derived_alpha`.
/// Theme-customizable explicit scrollbar colors are intentionally not
/// supported: they would require updating ~50 `Palette { .. }` literals in
/// tests for no shipped benefit. Returns premultiplied linear RGBA.
fn scrollbar_tint(fg: &str, derived_alpha: f32) -> [f32; 4] {
    hex_to_premultiplied_rgba(fg, derived_alpha)
}

fn read_only_badge_rect(sw: f32, sh: f32, scale: f32, content_w: f32) -> (f32, f32, f32, f32) {
    // Badge width hugs its content (icon + "READONLY") instead of a fixed
    // constant, so it never looks over-long. `content_w` is already in raster
    // px (estimated from the DPI-scaled badge font); add scaled paddings. The
    // edge MARGIN is a window-anchored position and stays in window space.
    let s = scale.max(0.01);
    let pad = (SEARCH_BAR_PAD_LEFT + SEARCH_BAR_PAD_RIGHT) * s;
    let w = (content_w + pad)
        .max(READ_ONLY_BADGE_W * 0.4 * s) // small floor so it never collapses
        .min((sw - READ_ONLY_BADGE_MARGIN * 2.0).max(40.0));
    let h = (READ_ONLY_BADGE_H * s).min((sh - READ_ONLY_BADGE_MARGIN * 2.0).max(20.0));
    let x = (sw - w - READ_ONLY_BADGE_MARGIN).max(0.0);
    let y = READ_ONLY_BADGE_MARGIN.min((sh - h).max(0.0));
    (x, y, w, h)
}

/// Classify a wgpu adapter as a software (CPU) rasterizer.
///
/// True when the adapter is a CPU device, or its name matches a known
/// software rasterizer (Microsoft WARP, Mesa llvmpipe, Google SwiftShader).
/// Used to drive the no-GPU degrade path. Pure fn over the
/// adapter info so it is unit-testable without a live GPU.
#[must_use]
pub fn detect_software_rendering(info: &wgpu::AdapterInfo) -> bool {
    software_rendering_from(&info.name, info.device_type)
}

/// Inner predicate over just the adapter name + device type, so it can be
/// unit-tested without building a full `wgpu::AdapterInfo` (which has no
/// `Default`).
#[must_use]
fn software_rendering_from(name: &str, device_type: wgpu::DeviceType) -> bool {
    if device_type == wgpu::DeviceType::Cpu {
        // When: `device_type` is `Cpu` — authoritative, so no name match is
        // needed. The string tests below cover rasterizers reporting otherwise.
        return true;
    }
    let name = name.to_ascii_lowercase();
    name.contains("microsoft basic render driver")
        || name.contains("llvmpipe")
        || name.contains("swiftshader")
        || name.contains("software adapter")
}

fn software_render_degrade_from(mode: SoftwareRenderMode, detected: bool) -> bool {
    match mode {
        SoftwareRenderMode::Auto => detected,
        SoftwareRenderMode::Force => true,
        SoftwareRenderMode::Off => false,
    }
}

/// Memory-allocation strategy selected for a wgpu device.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMemoryPolicy {
    /// Favor lower allocator reserve on software adapters.
    MemoryUsage,
    /// Favor rendering performance on hardware adapters.
    Performance,
}

/// Select the device memory policy for an adapter classification.
///
/// Software adapters minimize allocator reserve; hardware adapters retain
/// wgpu's performance-oriented policy.
#[doc(hidden)]
#[must_use]
pub fn device_memory_policy_from(software_rendering: bool) -> DeviceMemoryPolicy {
    match software_rendering {
        true => DeviceMemoryPolicy::MemoryUsage,
        false => DeviceMemoryPolicy::Performance,
    }
}

/// Select optional device features that SonicTerm can use without losing fallback support.
#[doc(hidden)]
#[must_use]
pub fn selected_optional_device_features(
    adapter_features: wgpu::Features,
    windows_host: bool,
) -> wgpu::Features {
    if windows_host {
        adapter_features & wgpu::Features::DUAL_SOURCE_BLENDING
    } else {
        // When: `windows_host` is false, LCD presentation stays disabled and requests no feature.
        wgpu::Features::empty()
    }
}

/// Resolve the requested LCD mode against platform, target opacity, and presenter capability.
#[doc(hidden)]
#[must_use]
pub const fn effective_subpixel_aa_mode(
    requested: SubpixelAaMode,
    windows_host: bool,
    opaque_target: bool,
    software_presenter: bool,
    dual_source_supported: bool,
) -> SubpixelAaMode {
    if !windows_host || !opaque_target {
        // When: `windows_host` or `opaque_target` is false, LCD coverage cannot reach an opaque Windows target.
        return SubpixelAaMode::Off;
    }
    if software_presenter || dual_source_supported {
        requested
    } else {
        // When: both `software_presenter` and `dual_source_supported` are false, use grayscale destination blending.
        SubpixelAaMode::Off
    }
}

/// Build the sole wgpu device descriptor used by the renderer.
///
/// The caller selects only features advertised by the adapter, while memory
/// policy remains independently derived from software-render classification.
#[doc(hidden)]
#[must_use]
pub fn device_descriptor_for(
    software_rendering: bool,
    required_features: wgpu::Features,
) -> DeviceDescriptor<'static> {
    let memory_hints = match device_memory_policy_from(software_rendering) {
        DeviceMemoryPolicy::MemoryUsage => wgpu::MemoryHints::MemoryUsage,
        DeviceMemoryPolicy::Performance => wgpu::MemoryHints::Performance,
    };
    DeviceDescriptor { memory_hints, required_features, ..DeviceDescriptor::default() }
}

/// Aggregate allocator usage without retaining allocation labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocatorSnapshot {
    /// Bytes occupied by live GPU allocations.
    pub allocated_bytes: u64,
    /// Bytes reserved by GPU memory blocks.
    pub reserved_bytes: u64,
    /// Number of live GPU allocations.
    pub allocations: u32,
    /// Number of reserved GPU memory blocks.
    pub blocks: u32,
    /// Size of the largest reserved GPU memory block.
    pub largest_block_bytes: u64,
}

/// Summarize a wgpu allocator report without reading allocation names.
#[doc(hidden)]
#[must_use]
pub fn allocator_snapshot_from(report: &wgpu::AllocatorReport) -> AllocatorSnapshot {
    AllocatorSnapshot {
        allocated_bytes: report.total_allocated_bytes,
        reserved_bytes: report.total_reserved_bytes,
        allocations: u32::try_from(report.allocations.len()).unwrap_or(u32::MAX),
        blocks: u32::try_from(report.blocks.len()).unwrap_or(u32::MAX),
        largest_block_bytes: report.blocks.iter().map(|block| block.size).max().unwrap_or(0),
    }
}

/// Preserve report unavailability while projecting an available report to scalar counters.
fn allocator_snapshot_from_report(
    report: Option<wgpu::AllocatorReport>,
) -> Option<AllocatorSnapshot> {
    report.as_ref().map(allocator_snapshot_from)
}

/// Emit a pane's scrollbar (track + thumb) into `quads_overlay` using the
/// shared geometry model. No-op when the pane has nothing to scroll, the
/// mode is `Never`, or `alpha` is at or below the emit floor.
/// Returns the number of quads emitted (for tests).
///
/// `alpha` in `[0.0, 1.0]` scales both track + thumb tint alphas; the
/// caller (app loop) feeds the lerped per-pane fade value from
/// `sonicterm_app::app::scrollbar_visibility::tick`.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_pane_scrollbar(
    quads_overlay: &mut Vec<QuadInstance>,
    pane_rect: PaneRect,
    viewport_rows: u16,
    total_rows: u64,
    view_top: u64,
    mode: sonicterm_render_model::boundary::cfg::config::ScrollbarMode,
    theme: &Theme,
    sw: f32,
    sh: f32,
    alpha: f32,
    scale: f32,
) -> usize {
    if alpha <= sonicterm_render_model::boundary::ui::scrollbar::ALPHA_EMIT_FLOOR {
        // When: `alpha` is at the fade floor where the bar is invisible —
        // emitting costs two quads per pane per frame for unseeable pixels.
        return 0;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    // The planner damages the same track, so drawing and damage cannot diverge.
    let Some(geom) = crate::frame_plan::pane_scrollbar_geometry(
        pane_rect,
        viewport_rows,
        total_rows,
        view_top,
        mode,
        scale,
    ) else {
        // When: `scrollbar::compute` yields None — nothing beyond the viewport
        // to scroll, or mode is `Never`. No track or thumb to place.
        return 0;
    };
    let fg_hex = theme.colors.foreground.0.as_str();
    let track_color = scrollbar_tint(fg_hex, 0.10 * alpha);
    let thumb_color = scrollbar_tint(fg_hex, 0.30 * alpha);
    quads_overlay.push(QuadInstance::sharp(
        px_to_ndc(
            geom.track_rect.x,
            geom.track_rect.y,
            geom.track_rect.w,
            geom.track_rect.h,
            sw,
            sh,
        ),
        track_color,
    ));
    quads_overlay.push(QuadInstance::sharp(
        px_to_ndc(
            geom.thumb_rect.x,
            geom.thumb_rect.y,
            geom.thumb_rect.w,
            geom.thumb_rect.h,
            sw,
            sh,
        ),
        thumb_color,
    ));
    2
}

fn splitter_rects_from_panes(pane_rects: &[(u64, PaneRect)], thickness: f32) -> Vec<SplitterRect> {
    let mut out = Vec::new();
    let thickness = thickness.max(0.0);
    let eps = 0.5_f32;

    for (i, (_, a)) in pane_rects.iter().enumerate() {
        for (_, b) in pane_rects.iter().skip(i + 1) {
            let vertical_overlap = a.y.max(b.y) < (a.y + a.h).min(b.y + b.h) - eps;
            if vertical_overlap && ((a.x + a.w) - b.x).abs() <= eps {
                let y = a.y.max(b.y);
                let h = (a.y + a.h).min(b.y + b.h) - y;
                out.push(SplitterRect {
                    axis: SplitAxis::Vertical,
                    rect: PaneRect::new(b.x - thickness * 0.5, y, thickness, h),
                });
            } else if vertical_overlap && ((b.x + b.w) - a.x).abs() <= eps {
                // When: `(b.x + b.w) - a.x` is the contact instead — the pair
                // walk is unordered, so the splitter straddles a's left edge.
                let y = a.y.max(b.y);
                let h = (a.y + a.h).min(b.y + b.h) - y;
                out.push(SplitterRect {
                    axis: SplitAxis::Vertical,
                    rect: PaneRect::new(a.x - thickness * 0.5, y, thickness, h),
                });
            }

            let horizontal_overlap = a.x.max(b.x) < (a.x + a.w).min(b.x + b.w) - eps;
            if horizontal_overlap && ((a.y + a.h) - b.y).abs() <= eps {
                let x = a.x.max(b.x);
                let w = (a.x + a.w).min(b.x + b.w) - x;
                out.push(SplitterRect {
                    axis: SplitAxis::Horizontal,
                    rect: PaneRect::new(x, b.y - thickness * 0.5, w, thickness),
                });
            } else if horizontal_overlap && ((b.y + b.h) - a.y).abs() <= eps {
                // When: `(b.y + b.h) - a.y` is the contact — mirrored pair
                // order, so the splitter straddles a's top edge.
                let x = a.x.max(b.x);
                let w = (a.x + a.w).min(b.x + b.w) - x;
                out.push(SplitterRect {
                    axis: SplitAxis::Horizontal,
                    rect: PaneRect::new(x, a.y - thickness * 0.5, w, thickness),
                });
            }
        }
    }

    out
}

use crate::{
    atlas_upload::{AtlasBindingKind, AtlasUpload, AtlasUploadStats},
    quad::{
        premultiply, px_to_ndc, scale_premultiplied_alpha, with_premultiplied_alpha, QuadInstance,
    },
    wezterm_pipeline::{ImageInstance, WeztermPipeline},
};
use sonicterm_render_model::boundary::cfg::config::CursorShape;
use sonicterm_render_model::boundary::ui::{
    command_palette::CommandPalette,
    copy_mode::{CopyModeState, QuickSelectState},
    cursor as ui_cursor,
    ime::ImeState,
    overlays::{
        search_bar_label, NotificationBubble, NotificationBubbleLayout, NotificationLevel,
        PaletteLayout, SearchBarLayout, PALETTE_BORDER, PALETTE_PANEL_RADIUS, PALETTE_QUERY_RADIUS,
        PALETTE_ROW_RADIUS, SEARCH_BAR_HEIGHT, SEARCH_BAR_ICON_GAP, SEARCH_BAR_PAD_LEFT,
        SEARCH_BAR_PAD_RIGHT,
    },
    pane::{Rect as PaneRect, SplitAxis, SplitterRect},
    search::SearchState,
    selection::Selection,
    tabbar_view::{
        tab_bar_height, Rect as TabTitleRect, TabBarLayout, ACTIVE_TOP_ACCENT_H,
        ACTIVE_TOP_ACCENT_INSET, TAB_BAR_HEIGHT, TAB_GAP, TAB_VERT_INSET,
    },
    tabs::{ContentWidthRefresh, TabBar, TabContent},
};
use sonicterm_render_model::geometry::PixelRect;
use sonicterm_text::GlyphInstance;
use sonicterm_text::{
    glyph_atlas::GlyphAtlas,
    // The ASCII fast-path predicate is cell-shape based, independent of the FontStack shaper.
    shape::{run_is_ascii_fast, RunStyle},
};

#[cfg(test)]
#[must_use]
#[allow(clippy::too_many_arguments)]
fn dirty_rows_damage_rect<I>(
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    dirty_rows_damage_rect_with_ink_pad(
        dirty_rows, pane_rect, origin_x, origin_y, cols, cell_w, cell_h, 0.0, surface_w, surface_h,
    )
}

/// Decide a pane's per-frame damage rectangle, given whether the pane is
/// showing the alternate screen.
///
/// A hardware surface keeps the previous frame's pixels, so damage-limited
/// repainting only redraws the rows the grid marked dirty. That is correct
/// for a normal shell pane: a changed prompt line is a narrow edit and
/// leaving the surrounding rows untouched is exactly what we want. It is
/// WRONG for an alternate-screen app (vim/nvim/less/tmux). Those apps
/// scroll, split, and repaint regions such that a row which was NOT
/// re-emitted this frame can still be visually stale — the app moved
/// content out from under it. For an alt-screen pane we therefore repaint
/// the pane's whole clipped rectangle whenever ANY row is dirty, and
/// nothing when the pane is clean.
///
/// Returns:
/// * `None` for an alt-screen pane with no dirty rows (clean -> no repaint).
/// * the full pane rectangle clipped to the surface for a dirty alt-screen
///   pane — a complete pane repaint, never an unconditional full-window one.
/// * the narrow, glyph-padded dirty-row union ([`dirty_rows_damage_rect_with_ink_pad`])
///   for a normal-screen pane.
///
/// The alt-screen decision is independent of cell metrics, so sparse /
/// scattered dirty rows and fractional cell heights all resolve to the same
/// complete-pane repaint; the surface clip both bounds the rect to on-screen
/// pixels and rejects a fully off-surface pane.
#[cfg(test)]
#[must_use]
#[allow(clippy::too_many_arguments)]
fn pane_damage_rect<I>(
    is_alt: bool,
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    pane_damage_rect_with_ink_pad(
        is_alt, dirty_rows, pane_rect, origin_x, origin_y, cols, cell_w, cell_h, 0.0, surface_w,
        surface_h,
    )
}

#[must_use]
fn atlas_changed_during_frame(before: GlyphContentStamp, after: GlyphContentStamp) -> bool {
    before != after
}

#[must_use]
fn row_cache_atlas_identity(atlas: &GlyphAtlas) -> u64 {
    atlas.identity()
}

fn atlas_texture_rebuild_required(current: (u32, u32), next: (u32, u32)) -> bool {
    current != next
}

fn scale_factor_rebuild_required(current: f32, next: f32) -> bool {
    (current - next.max(0.1)).abs() >= f32::EPSILON
}

const PLACEHOLDER_ATLAS_DIM: u32 = 1;

#[must_use]
fn atlas_payload_bytes(width: u32, height: u32) -> u64 {
    u64::from(width).saturating_mul(u64::from(height)).saturating_mul(4)
}

#[must_use]
fn desired_gpu_atlas_dimensions(software_presenter: bool, atlas: &GlyphAtlas) -> (u32, u32) {
    if software_presenter {
        (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
    } else {
        // When: `!software_presenter` — the GPU samples the atlas texture, so
        // it must match the CPU atlas or the UVs address the wrong tiles.
        (atlas.width(), atlas.height())
    }
}

#[must_use]
fn image_atlas_promotion_required(atlas: &GlyphAtlas, has_inline_media: bool) -> bool {
    has_inline_media
        && (atlas.width(), atlas.height()) == (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
}

/// Frames a window must draw with no renderable inline media before its image
/// atlas is released.
///
/// At 60fps this is about four seconds. Long enough that scrolling an image
/// out of view and back does not free and reallocate 16 MiB — which would also
/// force every visible image to re-decode — and short enough that a window
/// that has genuinely finished with images does not hold the allocation for
/// the rest of its life.
const IMAGE_ATLAS_IDLE_FRAMES: u32 = 240;

/// How long a window may go without renderable inline media before its promoted image atlas is
/// released, whether or not it assembles frames meanwhile. A window that stops drawing after an image
/// scrolls away never reaches [`IMAGE_ATLAS_IDLE_FRAMES`], so this interval releases it without a frame.
const IMAGE_ATLAS_IDLE_INTERVAL: Duration = Duration::from_secs(30);

/// Whether the image atlas holds a full allocation rather than the 1x1 placeholder.
#[must_use]
fn image_atlas_promoted(atlas: &GlyphAtlas) -> bool {
    (atlas.width(), atlas.height()) != (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
}

/// Whether the interval trigger releases the image atlas at `now`: it is promoted and no renderable
/// media has been visible since `absent_since`, at least [`IMAGE_ATLAS_IDLE_INTERVAL`] ago.
#[must_use]
fn image_atlas_release_due(promoted: bool, absent_since: Option<Instant>, now: Instant) -> bool {
    promoted
        && absent_since
            .is_some_and(|since| now.saturating_duration_since(since) >= IMAGE_ATLAS_IDLE_INTERVAL)
}

/// When the interval trigger is due; `None` for a placeholder atlas or while media is visible.
#[must_use]
fn image_atlas_release_deadline_for(
    promoted: bool,
    absent_since: Option<Instant>,
) -> Option<Instant> {
    absent_since.filter(|_| promoted).map(|since| since + IMAGE_ATLAS_IDLE_INTERVAL)
}

/// The absence instant after one assembly: set by the first media-free frame and kept by later ones,
/// so repeated idle frames never move the deadline; cleared by a frame with renderable media.
#[must_use]
fn next_inline_media_absent_since(
    current: Option<Instant>,
    has_renderable_inline_media: bool,
    now: Instant,
) -> Option<Instant> {
    if has_renderable_inline_media {
        None
    } else {
        // When: !has_renderable_inline_media, keep the current absence instant or start one at now.
        current.or(Some(now))
    }
}

/// Whether an idle window should release its full-size image atlas.
///
/// Promotion is otherwise one-way: `reset_in_place` clears the map and
/// repacker but never touches the pixel buffer or the dimensions, so a window
/// that displays one inline image keeps the allocation until it closes.
#[must_use]
fn image_atlas_demotion_ready(
    atlas: &GlyphAtlas,
    has_inline_media: bool,
    frames_without_inline_media: u32,
) -> bool {
    !has_inline_media
        && (atlas.width(), atlas.height()) != (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
        && frames_without_inline_media >= IMAGE_ATLAS_IDLE_FRAMES
}

/// Whether clearing the image atlas would actually do anything.
///
/// The frame-assembly caller resets whenever inline media "changed", but that
/// signal is `true` on any frame whose predecessor's key was absent — which is
/// every frame following the many state changes that clear it. On a window
/// that has never shown an image the media hash cannot change, so the absent
/// key accounts for every reset, once per rendered frame.
///
/// An untouched placeholder atlas holds no entries and no packing state, so
/// resetting it changes nothing while still rebuilding the packer and bumping
/// the atlas identity — which invalidates every cache keyed to it. A promoted
/// atlas carries that state even while its entry map is momentarily empty and
/// must still be reset, or the packer would keep handing out coordinates from
/// a layout the caller believes it discarded.
#[must_use]
fn image_atlas_reset_warranted(atlas: &GlyphAtlas) -> bool {
    !atlas.is_empty()
        || (atlas.width(), atlas.height()) != (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
}

fn full_surface_rect(width: u32, height: u32) -> PixelRect {
    PixelRect { x: 0, y: 0, w: width.max(1), h: height.max(1) }
}

pub(crate) const MAX_SURFACE_DIMENSION: u32 = 16_384;
const MAX_SURFACE_BYTES: u64 = 160 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedSurfaceSize {
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
}

/// What a requested surface resize did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeOutcome {
    /// The surface took a new size.
    Changed,
    /// The validated size equals the configured one; nothing was reconfigured.
    Unchanged,
    /// The size is unrepresentable; the previous surface stays configured.
    Rejected,
}

/// Classify a resize from the configured `(width, height)` to a validated candidate.
#[must_use]
pub(crate) fn classify_resize(
    configured: (u32, u32),
    candidate: Option<&ValidatedSurfaceSize>,
) -> ResizeOutcome {
    match candidate {
        None => ResizeOutcome::Rejected,
        Some(size) if (size.width, size.height) == configured => ResizeOutcome::Unchanged,
        Some(_) => ResizeOutcome::Changed,
    }
}

#[must_use]
pub(crate) fn validated_surface_size(
    width: u32,
    height: u32,
    device_max_dimension: u32,
) -> Option<ValidatedSurfaceSize> {
    let width = width.max(1);
    let height = height.max(1);
    let max_dimension = device_max_dimension.clamp(1, MAX_SURFACE_DIMENSION);
    if width > max_dimension || height > max_dimension {
        // When: an axis exceeds `max_dimension` — configuring the surface
        // anyway is a driver-level failure, so the caller rejects the size.
        return None;
    }
    let bytes = u64::from(width).checked_mul(u64::from(height))?.checked_mul(4)?;
    if bytes > MAX_SURFACE_BYTES {
        // When: `bytes > MAX_SURFACE_BYTES` — both axes legal, their product
        // not. The ceiling bounds one surface against the process budget.
        return None;
    }
    Some(ValidatedSurfaceSize { width, height, bytes: usize::try_from(bytes).ok()? })
}

#[allow(clippy::too_many_arguments)]
fn draw_retained_frame(
    pipeline: &mut WeztermPipeline,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    view: &TextureView,
    image_atlas: &wgpu::BindGroup,
    glyph_atlas: &wgpu::BindGroup,
    sw: f32,
    sh: f32,
    first: bool,
    damage: PixelRect,
    background: wgpu::Color,
    subpixel_aa: SubpixelAaMode,
    quads: &[QuadInstance],
    images: &[ImageInstance],
    glyphs: &[GlyphInstance],
    overlay_quads: &[QuadInstance],
    overlay_glyphs: &[GlyphInstance],
) {
    let full = first || damage == full_surface_rect(sw as u32, sh as u32);
    let reset = (!full).then(|| {
        QuadInstance::sharp(
            px_to_ndc(damage.x as f32, damage.y as f32, damage.w as f32, damage.h as f32, sw, sh),
            [background.r as f32, background.g as f32, background.b as f32, background.a as f32],
        )
    });
    let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some("sonic-retained-pass"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: Operations {
                load: if full {
                    LoadOp::Clear(background)
                } else {
                    // When: `full` is false, replace only the damaged pixels and retain the rest of the attachment.
                    LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_scissor_rect(
        damage.x.max(0) as u32,
        damage.y.max(0) as u32,
        damage.w.max(1),
        damage.h.max(1),
    );
    pipeline.draw_frame(
        device,
        queue,
        &mut pass,
        image_atlas,
        glyph_atlas,
        sw,
        sh,
        subpixel_aa,
        reset,
        quads,
        images,
        glyphs,
        overlay_quads,
        overlay_glyphs,
    );
}

fn create_frame_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: TextureFormat,
    copy_source: bool,
) -> (Texture, TextureView) {
    let mut usage = TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    if copy_source {
        // Only a test that enabled retained-frame readback copies the frame out.
        usage |= TextureUsages::COPY_SRC;
    }
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("sonic-retained-frame"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    });
    let view = texture.create_view(&TextureViewDescriptor::default());
    (texture, view)
}

/// The retained frame texture's extent: 1x1 under the Windows software presenter, which presents the
/// CPU frame and never samples it, else the surface size, never zero.
#[must_use]
fn frame_texture_extent(software_presenter: bool, width: u32, height: u32) -> (u32, u32) {
    if software_presenter {
        (1, 1)
    } else {
        // When: !software_presenter, the GPU presenter draws into the texture at width x height.
        (width.max(1), height.max(1))
    }
}

/// GPU bytes a frame texture of `extent` holds, at 4 bytes per texel.
#[must_use]
fn frame_texture_payload_bytes(extent: (u32, u32)) -> u64 {
    atlas_payload_bytes(extent.0, extent.1)
}

/// Build the retained frame texture sized for the presenter; the only caller of `create_frame_texture`.
/// `copy_source` is the test-only readback flag; production passes false.
fn build_frame_texture(
    device: &wgpu::Device,
    software_presenter: bool,
    width: u32,
    height: u32,
    format: TextureFormat,
    copy_source: bool,
) -> (Texture, TextureView) {
    let (texture_width, texture_height) = frame_texture_extent(software_presenter, width, height);
    create_frame_texture(device, texture_width, texture_height, format, copy_source)
}

/// Bytes per row of a readback buffer for a `width_px`-wide 4-byte frame, padded to wgpu's copy
/// alignment.
#[doc(hidden)]
#[must_use]
pub fn padded_readback_row_bytes(width_px: u32) -> u32 {
    let tight_bytes = width_px.max(1) * 4;
    tight_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

/// The tightly packed 4-byte pixels of a mapped readback whose `height_px` rows are each
/// `padded_row_bytes` long, the padding dropped.
#[doc(hidden)]
#[must_use]
pub fn unpad_readback_rows(
    mapped: &[u8],
    width_px: u32,
    height_px: u32,
    padded_row_bytes: u32,
) -> Vec<u8> {
    let tight_bytes = width_px as usize * 4;
    mapped
        .chunks(padded_row_bytes as usize)
        .take(height_px as usize)
        .flat_map(|row| &row[..tight_bytes])
        .copied()
        .collect()
}

/// The damage a presented frame carried to the presenter, read back by real-renderer tests.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentedDamage {
    /// Whether the frame was a first frame, which repaints the whole surface.
    pub first_frame: bool,
    /// The damage rectangle the presenter drew under its scissor.
    pub damage: PixelRect,
    /// The whole surface.
    pub surface: PixelRect,
}

impl PresentedDamage {
    /// Whether the frame redrew less than the whole surface: not a first frame, and its damage
    /// covers a smaller area than the surface, so a full repaint can never count as narrow.
    #[must_use]
    pub fn is_narrow(&self) -> bool {
        let area = |rect: PixelRect| u64::from(rect.w) * u64::from(rect.h);
        !self.first_frame && area(self.damage) < area(self.surface)
    }
}

/// Keeps the last presented frame's damage for a real-renderer test, only once enabled.
///
/// Disabled by default, as in production: `record` then tests one bool and builds nothing.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct PresentedDamageRecorder {
    enabled: bool,
    last: Option<PresentedDamage>,
}

impl PresentedDamageRecorder {
    /// Start keeping each presented frame's damage.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Keep the snapshot `build` makes, only when enabled; a disabled recorder never calls it.
    #[inline]
    pub fn record(&mut self, build: impl FnOnce() -> PresentedDamage) {
        if self.enabled {
            // Only a test hook sets `enabled`; production skips the snapshot entirely.
            self.last = Some(build());
        }
    }

    /// The last kept snapshot, cleared by this read.
    pub fn take(&mut self) -> Option<PresentedDamage> {
        self.last.take()
    }
}

/// A copy of the retained frame in a buffer the test maps itself; production code under `src/`
/// never maps or polls.
#[doc(hidden)]
pub struct RetainedFrameReadback {
    /// The device that owns `buffer`, which the test polls until the map completes.
    pub device: wgpu::Device,
    /// The `MAP_READ` buffer the frame was copied into.
    pub buffer: wgpu::Buffer,
    /// Frame width in pixels.
    pub width_px: u32,
    /// Frame height in pixels.
    pub height_px: u32,
    /// Bytes per buffer row, padded to wgpu's copy alignment.
    pub padded_row_bytes: u32,
}

/// Create a wgpu instance for `event_loop`'s display, honoring `WGPU_BACKEND`.
///
/// Startup and GPU recovery both call this, so a rebuilt device is requested from
/// an instance configured exactly like the first one.
fn new_instance(event_loop: &ActiveEventLoop) -> Instance {
    Instance::new(InstanceDescriptor::new_with_display_handle_from_env(Box::new(
        event_loop.owned_display_handle(),
    )))
}

/// Opacity of the active tab's accent bar while the window holds keyboard
/// focus.
pub const ACTIVE_PANEL_MARKER_ALPHA_FOCUSED: f32 = 1.0;

/// Opacity of the active tab's accent bar while the window is unfocused.
///
/// Low enough to read as "not the focused window" at a glance, high enough
/// that the active tab is still identifiable without focusing the window to
/// ask.
pub const ACTIVE_PANEL_MARKER_ALPHA_UNFOCUSED: f32 = 0.4;

/// Where a window's tab bar sits, in the pixel space pointer positions use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TabBarHoverGeometry {
    /// Window width the bar spans.
    pub(crate) width_px: f32,
    /// Bar height.
    pub(crate) bar_height_px: f32,
    /// Distance from the window top to the bar top.
    pub(crate) top_offset_px: f32,
    /// Whether the bar is shown; a hidden bar has no hovered tab.
    pub(crate) visible: bool,
}

/// The index of the tab under `cursor`, or `u32::MAX` when the bar is hidden,
/// the pointer is outside the window, or it rests on no tab.
///
/// This is the one hit test for tab hover: the frame draws the tab it returns,
/// and a pointer move asks for a frame only when its result changes.
pub(crate) fn hovered_tab_at(
    tabs: &TabBar,
    geometry: TabBarHoverGeometry,
    cursor: Option<(f32, f32)>,
) -> u32 {
    if !geometry.visible {
        // When: the bar is not `visible`, it has no widgets to hit-test.
        return u32::MAX;
    }
    let Some((cursor_x, cursor_y)) = cursor else {
        // When: `cursor` is None, the pointer left the window and hovers nothing.
        return u32::MAX;
    };
    let layout = TabBarLayout::compute_with_height(tabs, geometry.width_px, geometry.bar_height_px)
        .with_top_offset(geometry.top_offset_px);
    let point =
        sonicterm_render_model::boundary::ui::tabbar_view::Point { x: cursor_x, y: cursor_y };
    layout
        .tabwidgets()
        .iter()
        // Close buttons are no longer drawn, so only a body hover names a tab.
        .find(|widget| {
            matches!(
                widget.hover_at(Some(point)),
                sonicterm_render_model::boundary::ui::tabbar_view::TabHover::Body
            )
        })
        .map_or(u32::MAX, |widget| widget.idx as u32)
}

/// Style and sizing inputs for tab-bar quad emission.
pub struct TabBarQuadParams {
    /// Active tab accent color.
    pub accent: [f32; 4],
    /// Inactive tab separator color.
    pub separator: [f32; 4],
    /// Bar background and bottom border color.
    pub border: [f32; 4],
    /// Hovered tab index, or `u32::MAX` when no tab is hovered.
    pub hover_tab_idx: u32,
    /// Surface dimensions in the same units as the layout rects.
    pub surface: (f32, f32),
    /// Opacity of the active tab's accent bar, `0.0`–`1.0`.
    ///
    /// Not a visibility flag. The accent answers *which tab is active*, which
    /// is true of the window whether or not it holds keyboard focus, so an
    /// unfocused window dims it rather than dropping it — otherwise the
    /// window stops saying anything about its own state and has to be
    /// focused to find out.
    pub active_panel_marker_alpha: f32,
}

/// Paint the tab-bar background and tab chrome quads into `quads`.
pub fn emit_tab_bar_quads(
    quads: &mut Vec<QuadInstance>,
    layout: &TabBarLayout,
    params: &TabBarQuadParams,
) {
    let (sw, sh) = params.surface;
    quads.push(QuadInstance {
        rect: px_to_ndc(layout.bar.x, layout.bar.y, layout.bar.w, layout.bar.h, sw, sh),
        color: params.border,
        ..Default::default()
    });
    quads.push(QuadInstance {
        rect: px_to_ndc(layout.bar.x, layout.bar.y + layout.bar.h - 1.0, layout.bar.w, 1.0, sw, sh),
        color: params.border,
        ..Default::default()
    });
    for (position, t) in layout.tabs.iter().enumerate() {
        let is_active = layout.active == Some(t.idx);
        let marker_alpha = params.active_panel_marker_alpha.clamp(0.0, 1.0);
        if is_active && marker_alpha > 0.0 {
            let scale = (t.bg_rect.h / (TAB_BAR_HEIGHT - 2.0 * TAB_VERT_INSET)).max(0.1);
            let inset = ACTIVE_TOP_ACCENT_INSET * scale;
            let acc = sonicterm_render_model::boundary::ui::tabbar_view::Rect {
                x: t.bg_rect.x + inset,
                y: t.bg_rect.y + 1.0 * scale,
                w: (t.bg_rect.w - inset * 2.0).max(0.0),
                h: ACTIVE_TOP_ACCENT_H * scale,
            };
            let base = t
                .custom_color
                .as_deref()
                .map(|hex| hex_to_premultiplied_rgba(hex, 1.0))
                .unwrap_or(params.accent);
            let color = scale_premultiplied_alpha(base, marker_alpha);
            quads.push(QuadInstance {
                rect: px_to_ndc(acc.x, acc.y, acc.w, acc.h, sw, sh),
                color,
                ..Default::default()
            });
        }
        if position + 1 < layout.tabs.len() {
            // Geometric scale = bar.h / default-logical-bar-h. Mirrors
            // the per-bar-height scale `TabBarLayout::compute_at_y`
            // uses to grow TAB_GAP / padding with bar height — keeps
            // separators centered in each adjacent-tab gap.
            let scale = (layout.bar.h / 40.0).max(0.1);
            let sep_w = 1.0_f32 * scale;
            let sep_h = (layout.bar.h - 16.0 * scale).max(1.0);
            let sep_y = layout.bar.y + (layout.bar.h - sep_h) * 0.5;
            let gap_mid = t.bg_rect.x + t.bg_rect.w + (TAB_GAP * scale - sep_w) * 0.5;
            quads.push(QuadInstance {
                rect: px_to_ndc(gap_mid, sep_y, sep_w, sep_h, sw, sh),
                color: params.separator,
                ..Default::default()
            });
        }
    }
    if let Some(control) = layout.overflow.filter(|control| control.w > 0.0) {
        let size = (control.w.min(control.h) * 0.35).max(0.0);
        let x = control.x + (control.w - size) * 0.5;
        let y = control.y + (control.h - size) * 0.5;
        let rect = px_to_ndc(x, y, size, size, sw, sh);
        let stroke = (size * 0.12).max(1.0).min(size * 0.5);
        quads.push(QuadInstance::line(
            rect,
            params.accent,
            [size, size],
            [-size * 0.3, -size * 0.15],
            [0.0, size * 0.15],
            stroke,
        ));
        quads.push(QuadInstance::line(
            rect,
            params.accent,
            [size, size],
            [0.0, size * 0.15],
            [size * 0.3, -size * 0.15],
            stroke,
        ));
    }
}

/// Shared ownership of the GPU handles and device-containment state used by sibling renderers.
#[allow(dead_code)]
#[derive(Clone)]
pub struct GpuSharedContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Containment state of `device`, shared by every renderer built from
    /// this context.
    device_errors: Arc<DeviceErrorState>,
}

/// Top-level GPU-backed terminal renderer. Owns the wgpu surface, the
/// text + quad pipelines, the glyph atlas, font/shape caches, and all
/// per-frame layout / cursor / overlay state. One per OS window.
pub struct GpuRenderer {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    /// True when wgpu selected a CPU/software rasterizer (WARP, llvmpipe,
    /// SwiftShader) — see [`detect_software_rendering`]. The app reads this
    /// via [`GpuRenderer::is_software_rendering`] to degrade the frame cap and
    /// per-frame animation.
    software_rendering: bool,
    /// Resolved no-GPU degrade state: adapter detection combined with
    /// `[appearance].software_render_mode`.
    software_render_degrade: bool,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Containment state of `device`, shared with every renderer on it.
    device_errors: Arc<DeviceErrorState>,
    /// Whether a frame outcome already carried this renderer's one-time stop report.
    device_stop_reported: bool,
    /// Test fault: the next glyph-upload rebuild creates an invalid texture.
    fault_invalid_glyph_upload: bool,
    /// Test fault: every later frame records an invalid clear of this buffer.
    fault_frame_probe: Option<wgpu::Buffer>,
    /// Test seam: the next assembly returns `Err` after its glyph rows were staged.
    fault_assembly_error: bool,
    /// Test inspector: when `Some`, each assembly pass clears it at its start and records, per
    /// drawn pane, every slot at the moment its row is emitted; production leaves it `None`.
    emitted_rows_probe: Option<Vec<(u64, Vec<u16>)>>,
    /// Test seam: the next presentation returns `Err` from the presenter call.
    fault_present_error: bool,
    /// Test seam: return one backend occlusion after the normal frame device gate.
    fault_surface_occluded: bool,
    /// Test seam: replace the glyph atlas identity during the next assembly, as an atlas reset
    /// mid-frame would, so that frame takes the atlas retry.
    fault_atlas_change_during_assembly: bool,
    /// Test seam: stop the device just before the next cached Windows CPU reblit.
    #[cfg(target_os = "windows")]
    fault_stop_before_cached_present: bool,
    /// Frames handed to a native presenter, counted at the present call.
    present_calls: u64,
    surface: wgpu::Surface<'static>,
    config: SurfaceConfiguration,
    hardware_present_mode: PresentMode,
    hardware_alpha_mode: CompositeAlphaMode,
    window: Arc<Window>,

    /// WezTerm-style final presentation pipeline. It consumes every glyph and
    /// geometry primitive for a frame and emits one indexed draw stream.
    present_pipeline: WeztermPipeline,
    frame_texture: Texture,
    frame_view: TextureView,
    frame_blitter: wgpu::util::TextureBlitter,

    // B3 GPU text path for terminal and chrome glyphs.
    glyph_atlas: GlyphAtlas,
    glyph_upload: AtlasUpload,
    /// Monotonic identity of the current glyph-atlas allocation. Unlike the
    /// atlas-local eviction count, this survives replacement and prevents an
    /// old epoch-0 UV cache from matching a new epoch-0 atlas.
    glyph_atlas_generation: u64,
    // Inline media is isolated so large images cannot evict glyph tiles
    // referenced by the row cache.
    image_atlas: GlyphAtlas,
    image_upload: AtlasUpload,
    retained_inline_media_bytes: usize,
    /// Consecutive frames this window has drawn with no renderable inline
    /// media. Promotion of the image atlas is one-way without this: a window
    /// that shows a single image holds the full-size allocation for its whole
    /// life, even after the image scrolls into history. Demoting on the first
    /// idle frame would instead free and reallocate 16 MiB every time an image
    /// scrolled off and back, and re-decode it each time, so the count gates
    /// demotion behind a sustained absence.
    frames_without_inline_media: u32,
    /// Set when assembly resized the CPU image atlas: the GPU mirror is rebuilt after the frame's
    /// source is released, so no device resource is created while parser guards are held.
    image_upload_rebuild_pending: bool,
    /// When the window last assembled a frame without renderable inline media after one with it, or
    /// `None` while media is visible. The interval release counts [`IMAGE_ATLAS_IDLE_INTERVAL`] from it.
    inline_media_absent_since: Option<Instant>,
    /// True after an eviction-triggered compaction. The rebuilt atlas has
    /// eviction disabled until one frame presents successfully, bounding
    /// retries when the visible glyph working set exceeds atlas capacity.
    glyph_atlas_retry_without_eviction: bool,
    /// Growths already counted and the growth episode awaiting its present.
    growth_episodes: crate::frame_stats::GrowthEpisodes,
    /// Test-only: the retained frame texture is built with `COPY_SRC` so a test can read it back.
    /// Only `__enable_retained_frame_readback` sets it; production keeps it false.
    retained_frame_readback: bool,
    /// In-place glyph atlas resets since construction, read only by tests through
    /// [`Self::__test_glyph_atlas_resets`] to prove a growth retry never resets.
    glyph_atlas_resets: u64,
    /// True while a covered-window trim has replaced the frame texture with a 1x1 one; the next
    /// GPU present, a resize, a degrade switch, a readback enablement or a recovery commit clears it.
    frame_texture_trimmed: bool,
    /// Frame textures installed after construction, by a rebuild or a recovery commit; read only
    /// through [`Self::__frame_texture_rebuilds`].
    frame_texture_installs: u64,

    font_family: String,
    font_dirs: Vec<PathBuf>,
    font_size: f32,
    line_height: f32,
    font_weight_scale: f32,
    /// Requested LCD coverage order; effective-mode resolution stays separate.
    subpixel_aa: SubpixelAaMode,
    /// Multiplier applied to the font's natural cell height to derive the
    /// rendered line height (`cell_h = natural_cell_h * line_height_mult`).
    /// Stored so a DPI/scale-factor change can recompute `cell_h` from the
    /// freshly-rasterized natural height — see `rebuild_for_sf`. Without it,
    /// the rebuild had to back this factor out of the stale `line_height`,
    /// which algebraically cancelled and pinned `cell_h` to the old DPI.
    line_height_mult: f32,
    /// DPI multiplier (e.g. 2.0 on Retina). The renderer is raster-px
    /// end-to-end, so draw and hit-test sites no longer multiply or
    /// divide by it; it converts logical sizes into raster pixels, for
    /// example the font size in `raster_px` and the configured padding.
    scale_factor: f32,
    /// Cell width in raster pixels (one terminal column). Sourced from
    /// `FontStack::cell_metrics_raster_px()` so sonicterm-font metrics
    /// drop in without a unit conversion.
    pub cell_w: f32,
    /// Cell height in raster pixels (one terminal row).
    pub cell_h: f32,
    padding_left: f32,
    padding_right: f32,
    padding_top: f32,
    padding_bottom: f32,
    bg: wgpu::Color,
    bg_opacity: f32,
    /// Scrollbar visibility policy from config. Read on
    /// every frame in the per-pane scrollbar emit loop.
    scrollbar_mode: sonicterm_render_model::boundary::cfg::config::ScrollbarMode,
    /// Padding between overlay panel chrome and inner content.
    panel_padding: f32,
    fg_default: ChromeColor,
    cursor_color: [f32; 4],
    /// Theme cursor-text color as straight RGBA. Used to recolor glyphs
    /// under block cursors and cursor-like overlay carets without deriving
    /// text color from the background.
    cursor_text_color: [f32; 4],
    /// Theme background as straight RGBA. Used by overlays that intentionally
    /// mask text with the terminal background.
    bg_rgba: [f32; 4],
    /// Visual style of the text cursor (block / bar / underline).
    /// Live-updated from config; see [`Self::set_cursor_shape`].
    cursor_shape: CursorShape,
    /// Whether the text cursor blinks. When `false` the cursor renders
    /// at solid alpha and the FrameKey ignores the phase bucket.
    cursor_blink: bool,
    /// Anchor for the blink phase. Reset on every config change so the
    /// user sees the cursor at full brightness immediately after they
    /// toggle the setting (rather than wherever the cycle happened to
    /// be at the time).
    blink_epoch: Instant,
    /// Whether the OS window currently holds keyboard focus. The text
    /// cursor is hidden while the window is inactive. Defaults to
    /// `true` so a freshly created renderer draws the cursor on the
    /// very first frame, before winit has a chance to deliver
    /// `Focused(true)`.
    window_focused: bool,
    /// Cursor positions inside inactive panes (panes that share the
    /// window with the active pane but don't currently own keyboard
    /// focus). Kept as a compatibility sink for the app-side plumbing;
    /// inactive pane cursors are no longer drawn.
    inactive_pane_cursors: Vec<InactivePaneCursor>,
    /// Short-lived focus confirmation animation for the pane that just
    /// became active. Cleared automatically after
    /// [`PANE_FOCUS_FLASH_DURATION`].
    pane_focus_flash: Option<(u64, Instant)>,
    selection_color: [f32; 4],
    tab_bar_bg: [f32; 4],
    tab_active_bg: [f32; 4],
    tab_inactive_bg: [f32; 4],
    tab_active_fg: ChromeColor,
    tab_inactive_fg: ChromeColor,
    /// Deprecated user override for the removed tab close button. Kept
    /// only so older configs round-trip without changing the renderer
    /// settings surface.
    tab_close_override: Option<[f32; 4]>,
    /// Last reported cursor position in LOGICAL pixels, or `None` when
    /// the cursor is outside the window. Drives tab hover state.
    hover_cursor: Option<(f32, f32)>,
    /// Color for the wezterm-style vertical bar drawn between adjacent
    /// inactive tabs. A dim variant of the inactive-fg works in every
    /// theme; we precompute it here so the per-frame render path stays
    /// allocation-free.
    tab_separator: [f32; 4],
    hyperlink_underline: [f32; 4],
    splitter_color: [f32; 4],
    hyperlink_tint: [f32; 4],
    search_highlight: [f32; 4],
    search_fg: ChromeColor,
    search_bg: [f32; 4],
    // The 11 `*_buffer: legacy chrome buffer`
    // fields that lived here (search, quick_select, palette_{query,rows,
    // footer}, ime, broadcast,
    // drag_chip) are gone. Every chrome string is now shaped on demand
    // inside `render()` via `chrome_text::layout(...)`; the resulting
    // glyph instances feed either `glyph_instances` (pre-overlay
    // chrome — tab titles, search status bar) or
    // `overlay_glyph_instances` (modal chrome — palette,
    // IME preedit, drag-chip title). No per-renderer glyphon buffer
    // state survives.
    /// Last rendered drag-chip rect in raster pixels; absent when no chip was drawn.
    drag_chip_visual: Option<DragChipVisual>,
    /// Last rendered frame key — when the next frame would produce an
    /// identical key, render() short-circuits before any GPU work.
    last_frame_key: Option<FrameKey>,
    /// What the last presented frame's cursor recolors rewrote; written only beside
    /// `last_frame_key` on a presented frame, so it always describes the pixels on screen.
    last_recolor: crate::cursor::RecolorRecord,
    /// Where the last presented frame's tab-title glyphs draw; written only beside
    /// `last_recolor` on a presented frame, so it always describes the title pixels on screen.
    last_tab_ink: crate::cursor::RecolorBounds,
    /// Test seam: a glyph `(x, y, w, h)` surface-pixel rectangle and color appended before the
    /// cursor recolors; only `__inject_test_glyph` sets it, so production keeps `None`.
    injected_test_glyph: Option<((f32, f32, f32, f32), [f32; 4])>,
    /// Test seam: a glyph `(x, y, w, h)` and color attached to the row at `(pane, slot)`, drawn
    /// only when that row is emitted; only `__inject_row_glyph` sets it, so production keeps `None`.
    injected_row_glyph: Option<InjectedRowGlyph>,
    /// Test seam: the reason the next wgpu acquisition reports instead of asking the surface; only
    /// `__fail_next_surface_acquire` sets it, so production keeps `None`.
    fault_surface_acquire: Option<SurfaceRetryReason>,
    /// Where each presented row drew, per `(pane, slot)`; staged during assembly and committed
    /// beside `last_frame_key` only when a frame presents. A partial plan emits by these records.
    row_ink: crate::row_ink::RowInkTable,
    /// The last presented frame's damage, kept beside `last_frame_key` only after
    /// `__enable_presented_damage`; production never enables it.
    presented_damage: PresentedDamageRecorder,
    /// Constant-size geometry of the palette and search query fields as last presented.
    presented_fields: PresentedFields,
    /// Preedit glyphs keyed by text, placement, color, and qualified atlas identity to reject stale UVs.
    preedit_glyph_cache: Option<PreeditGlyphCache>,
    /// Cumulative count of frames skipped via the FrameKey fast-path.
    /// Exposed via tracing::trace for `RUST_LOG=trace` hit-rate dashboards.
    skipped_frames: u64,
    /// Frames that reached a native presentation boundary successfully.
    successful_frame_count: u64,
    /// This renderer's statistics when it counts; set once by its App, before any frame.
    frame_sink: Option<crate::frame_stats::FrameStatsSink>,
    /// Test hook run at the start of each presentation; returning true stops the device there.
    present_hook: Option<Box<dyn FnMut() -> bool + Send>>,
    #[cfg(target_os = "windows")]
    software_frame: Option<crate::software_frame::SoftwareFrame>,
    /// Window label used in renderer-internal timing logs.
    render_timing_label: &'static str,
    /// Whether the tab bar is currently shown. Toggled at runtime by the
    /// View → Toggle Tab Bar menu action; when `false`, [`Self::top_inset`]
    /// returns 0 and the tab bar draw block in [`Self::render`] is skipped.
    tab_bar_visible: bool,
    /// Reserved height (logical px) above the tab bar for the OS native
    /// titlebar. Kept at zero while SonicTerm uses the normal OS titlebar with a
    /// bottom-pinned tab bar.
    titlebar_inset: f32,
    /// Characters from the most recent `render()` call that the
    /// rasterizer could not produce a tile for (i.e. would draw as a
    /// tofu outline). Whitespace is excluded. Test-only diagnostic
    /// surfaced through [`Self::last_missing_tofu`]; production code
    /// must not depend on it.
    last_missing_chars: Vec<char>,
    /// Chrome characters (tab titles, palette, search, preedit, footer) the most recent presented
    /// frame drew as tofu or dropped, whitespace excluded. Test-only diagnostic surfaced through
    /// [`Self::last_missing_chrome`]; production code must not depend on it.
    last_missing_chrome_chars: Vec<char>,
    /// What the last presented `Full` frame certified about missing characters, read by
    /// [`Self::completeness_checkpoint`]; only presented frames change it.
    completeness: Option<crate::completeness::Certificate<FrameKey, GlyphContentStamp>>,
    // Row-cache hits skip style-run shaping; misses shape through the font stack before atlas insertion.
    /// Sonicterm-font driven shaper. Owns
    /// the cell metrics (`cell_metrics_raster_px()`), the resolved
    /// font fallback chain, and the `blocking_shape` entry point that
    /// `flush_shape_run` calls through `shape_run_with_wezterm`. The
    /// renderer keeps the `Option<...>` shape so test fixtures (no
    /// bundled fonts on disk) can still construct a `GpuRenderer`
    /// even though the grid path is degraded.
    pub(crate) font_stack: Option<sonicterm_engine::FontStack>,
    /// Tab-title font: its native-size stack (`body + 1`), raster size and width key.
    tab_title_font: TabTitleFont,
    /// Native-size stack for the command-palette footer (`body - 1`).
    palette_footer_font_stack: Option<sonicterm_engine::FontStack>,
    /// Kept tab titles, search-overlay runs and the UI palette; reported as `chrome_cache`.
    chrome_caches: crate::chrome_cache::ChromeCaches,
    /// Whether the title and chrome-run caches keep what they shape; a test turns it off to
    /// compare against cold drawing.
    chrome_reuse: bool,
    /// The per-frame draw vectors, kept between assembled frames; reported as `frame_scratch`.
    frame_scratch: frame_scratch::ScratchHome,
    /// Per-row glyph cache. Stores the shaped
    /// `GlyphInstance`s, underline coalescing, and missing-tofu list
    /// for each visible row, keyed by absolute row index + a content
    /// hash. A row whose contents / style / selection-overlap haven't
    /// changed splices its cached output straight into the frame and
    /// skips the entire `flush_shape_run` walk.
    row_glyph_cache: sonicterm_text::row_glyph_cache::RowGlyphCache,
    /// Per-row cache for background/underline/hyperlink-tint quads
    /// Mirrors `row_glyph_cache` but for the
    /// `QuadInstance`s emitted by `emit_cell_bg_quads_clipped` — on a
    /// hit we splice the cached `Vec<QuadInstance>` straight into the
    /// frame's quad vector and skip the per-cell run-length-encode.
    line_quad_cache: crate::row_quad_cache::LineQuadCache,
    /// Test-only `(pane_id, [origin_x_px, origin_y_px])` records for every rendered pane; not a production contract.
    last_emit_origins: Vec<(u64, [f32; 2])>,
    /// Raster-pixel pane layouts for hit-testing with the same snapped column edges; empty before the first layout.
    last_pane_layout: Vec<PaneLayoutSnapshot>,
    /// Monotonic counter bumped on theme / default-fg / default-bg
    /// changes. Folded into every `row_hash` so palette swaps
    /// invalidate cached colours without iterating the cache.
    style_rev: u64,
    /// The `(notice id, generation)` the last frame preparation applied.
    applied_fonts: Option<(u64, u64)>,
    /// A fallback generation apply no render attempt has carried yet; the next attempt takes it.
    unattributed_apply: bool,
    /// The App's wake for fallback completions; attached to every body stack this renderer installs.
    fallback_waker: Option<FontFallbackWaker>,
    /// Active drag-chip overlay: translucent rect drawn at the cursor
    /// while a tab is held. Cleared on release.
    drag_chip: Option<DragChipOverlay>,
    /// Compatibility attachment state, independent of FontStack fallback discovery.
    async_loader: Option<()>,
}

/// Atlas-local UV identity qualified by its device and renderer-owned allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GlyphContentStamp {
    device_generation: u64,
    allocation_generation: u64,
    content_identity: u64,
    /// The atlas's monotonic growth count, so a growth is told apart from a reset or eviction.
    growths: u64,
}

impl GlyphContentStamp {
    fn capture(device_generation: u64, allocation_generation: u64, atlas: &GlyphAtlas) -> Self {
        Self {
            device_generation,
            allocation_generation,
            content_identity: atlas.identity(),
            growths: atlas.growths(),
        }
    }
}

/// Whether the atlas changed during a frame only by growing: same device, same allocation, no
/// eviction since `frame_evictions`, and a higher growth count. Growth moves no tile, so the
/// frame retries without resetting the atlas or disabling eviction.
fn growth_only_change(
    before: GlyphContentStamp,
    after: GlyphContentStamp,
    frame_evictions: u64,
    current_evictions: u64,
) -> bool {
    before.device_generation == after.device_generation
        && before.allocation_generation == after.allocation_generation
        && frame_evictions == current_evictions
        && after.growths > before.growths
}

struct PreeditGlyphCache {
    text: String,
    font_size: f32,
    start_x: f32,
    top_y: f32,
    color_bits: u32,
    atlas_stamp: GlyphContentStamp,
    glyphs: Vec<GlyphInstance>,
    /// Outline quads of unresolved characters, replayed with the glyphs.
    missing_boxes: Vec<QuadInstance>,
    /// The characters those outlines stand for, replayed into the frame's chrome readout.
    missing_chrome_chars: Vec<char>,
}

impl PreeditGlyphCache {
    /// True when this cache entry exactly matches the requested preedit emit
    /// (text + placement + color + atlas generation).
    fn matches(
        &self,
        text: &str,
        font_size: f32,
        start_x: f32,
        top_y: f32,
        color_bits: u32,
        atlas_stamp: GlyphContentStamp,
    ) -> bool {
        self.atlas_stamp == atlas_stamp
            && self.color_bits == color_bits
            && self.font_size.to_bits() == font_size.to_bits()
            && self.start_x.to_bits() == start_x.to_bits()
            && self.top_y.to_bits() == top_y.to_bits()
            && self.text == text
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabTitleGlyphDebug {
    pub raster_px: f32,
    pub rect: [f32; 4],
    pub px_size: [u32; 2],
}

/// Snapshot of one pane's layout in raster pixels, captured at the
/// end of each `render()` call. Used by [`GpuRenderer::pixel_to_cell`]
/// to (a) figure out which pane was clicked and (b) reconstruct
/// that pane's `snapped_cell_x` edge cache on-demand so the column
/// search uses the same device-pixel-snapped edges the renderer drew.
///
/// Raster coordinates match winit cursor input without a scale conversion, despite the `_logical` field names.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct PaneLayoutSnapshot {
    /// Stable id of the pane this snapshot describes.
    pub id: u64,
    /// Raster-px left edge of the pane (== that pane's `padding_left`
    /// equivalent — the origin `build_snapped_cell_x` was passed).
    pub origin_x_logical: f32,
    /// Raster-px top edge of the pane (already adjusted for tab-bar /
    /// top inset).
    pub origin_y_logical: f32,
    /// Raster-px width of the pane's content rect.
    pub w_logical: f32,
    /// Raster-px height of the pane's content rect.
    pub h_logical: f32,
    /// Cell width in raster pixels for the pane.
    pub cell_w_logical: f32,
    /// Cell height in raster pixels for the pane.
    pub cell_h_logical: f32,
    /// Number of columns in the pane's grid at snapshot time.
    pub cols: u16,
    /// Number of rows in the pane's grid at snapshot time.
    pub rows: u16,
}

/// Shape and emit one tab's title spans as glyph instances.
///
/// Each `(text, colour, attrs)` span is laid out through
/// [`chrome_text::layout`] into the shared glyph atlas. Title and terminal
/// tiles remain distinct cache entries but draw through the same pass. The pen advances by
/// `avg_glyph_w` per character rather than by the shaper's advances, matching
/// the column arithmetic the caller already used to truncate and centre the
/// title.
///
/// Returns the outline quads of characters no face resolved; the caller draws them
/// with its chrome quads.
///
/// `debug`, when supplied, receives one record per emitted glyph for tests
/// asserting the atlas path was taken.
#[doc(hidden)]
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn emit_tab_title_glyphs(
    glyph_atlas: &mut GlyphAtlas,
    font_stack: &sonicterm_engine::FontStack,
    raster_px: f32,
    native_em_px: f32,
    wt_raster: &mut impl sonicterm_text::glyph_atlas::Rasterizer,
    spans: &[(&str, ChromeColor, ChromeAttrs)],
    baseline_y: f32,
    avg_glyph_w: f32,
    sw: f32,
    sh: f32,
    glyph_instances: &mut Vec<GlyphInstance>,
    mut debug: Option<&mut Vec<TabTitleGlyphDebug>>,
) -> Vec<QuadInstance> {
    // Each title span uses the native-size FontStack and shared atlas through chrome_text.
    let mut pen_x: f32 = 0.0;
    let mut missing_boxes = Vec::new();
    for (text, color, attrs) in spans {
        if text.is_empty() {
            // When: `text.is_empty()` — the title builder emits empty spans for
            // absent segments. Layout would yield no glyphs and no advance.
            continue;
        }
        let layout = chrome_text::layout_with_raster_variant(
            font_stack,
            wt_raster,
            glyph_atlas,
            text,
            *color,
            *attrs,
            raster_px,
            native_em_px,
            (pen_x, baseline_y),
            (sw, sh),
            None,
            GlyphRasterVariant::TabTitle,
        );
        let count_pre = glyph_instances.len();
        glyph_instances.extend(layout.glyphs.iter().copied());
        missing_boxes.extend(layout.missing_boxes);
        // Tab titles use `avg_glyph_w` columns × char count as the
        // logical layout stride (column-snapped), regardless of the
        // shaper's per-glyph advances. Preserves the existing
        // build_tab_title_spans column arithmetic that drives the
        // truncation / centering math upstream.
        let cols = text.chars().count() as f32;
        pen_x += cols * avg_glyph_w;
        if let Some(out) = debug.as_deref_mut() {
            for g in &glyph_instances[count_pre..] {
                // Tab-title debug records track only `raster_px` +
                // a rough px_size derived from the NDC quad height.
                let h = (-g.rect[3] * 0.5 * sh).abs();
                let w = (g.rect[2] * 0.5 * sw).abs();
                out.push(TabTitleGlyphDebug {
                    raster_px,
                    rect: [(g.rect[0] + 1.0) * 0.5 * sw, (1.0 - g.rect[1]) * 0.5 * sh, w, h],
                    px_size: [w as u32, h as u32],
                });
            }
        }
    }
    missing_boxes
}

/// Debug record emitted by [`emit_overlay_text_glyphs`] so tests can
/// assert the device-scaled atlas path was taken.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayTextGlyphDebug {
    pub raster_px: f32,
    pub font_size: f32,
    pub rect: [f32; 4],
    pub px_size: [u32; 2],
}

/// Emit one line of overlay text (palette query, rows, footer, search field)
/// as clipped glyph instances.
///
/// Positions glyphs from an explicit pixel `origin_x`/`baseline_y` so a caller
/// draws a multi-line overlay by calling once per line and advancing the
/// baseline itself. Glyphs falling outside `bounds` are dropped by the layout
/// clip, which is what keeps text inside a modal panel instead of painting
/// across the terminal behind it. Returns the outline quads of characters no face
/// resolved; the caller draws them with the overlay quads.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn emit_overlay_text_glyphs(
    glyph_atlas: &mut GlyphAtlas,
    font_stack: &sonicterm_engine::FontStack,
    font_size_px: f32,
    native_em_px: f32,
    wt_raster: &mut impl sonicterm_text::glyph_atlas::Rasterizer,
    text: &str,
    color: ChromeColor,
    attrs: ChromeAttrs,
    origin_x: f32,
    baseline_y: f32,
    bounds: [f32; 4], // [x, y, w, h] in raster px; glyphs outside are clipped
    sw: f32,
    sh: f32,
    glyph_instances: &mut Vec<GlyphInstance>,
    debug: Option<&mut Vec<OverlayTextGlyphDebug>>,
) -> Vec<QuadInstance> {
    if text.is_empty() {
        // When: `text.is_empty()` — an unset footer or empty query. Returning
        // leaves `glyph_instances` untouched, so line advance is unaffected.
        return Vec::new();
    }
    let [bx, by, bw, bh] = bounds;
    let layout = chrome_text::layout(
        font_stack,
        wt_raster,
        glyph_atlas,
        text,
        color,
        attrs,
        font_size_px,
        native_em_px,
        (origin_x, baseline_y),
        (sw, sh),
        Some(ChromeClip { x: bx, y: by, w: bw, h: bh }),
    );
    let count_pre = glyph_instances.len();
    glyph_instances.extend(layout.glyphs.iter().copied());
    if let Some(out) = debug {
        for g in &glyph_instances[count_pre..] {
            let h = (-g.rect[3] * 0.5 * sh).abs();
            let w = (g.rect[2] * 0.5 * sw).abs();
            out.push(OverlayTextGlyphDebug {
                raster_px: font_size_px,
                font_size: font_size_px,
                rect: [(g.rect[0] + 1.0) * 0.5 * sw, (1.0 - g.rect[1]) * 0.5 * sh, w, h],
                px_size: [w as u32, h as u32],
            });
        }
    }
    layout.missing_boxes
}

/// Renderers constructed but not yet dropped, across the whole process.
///
/// A renderer's own `retained_amounts()` cannot answer "did the last one go
/// away": it reports the instance being asked, and every instance reports the
/// same atlas capacity. Reading it in a loop compares a constant to itself and
/// holds whether or not anything leaked. This counter is the quantity a leak
/// actually moves — it rises on construction, falls in `Drop`, and returns to
/// its starting value only if every renderer built was also released.
static LIVE_RENDERERS: AtomicUsize = AtomicUsize::new(0);

/// How many `GpuRenderer`s are alive right now.
///
/// Intended for churn and lifecycle checks: take a reading, create and drop
/// renderers, and compare. A surviving renderer leaves this above where it
/// started.
// Ordering: `LIVE_RENDERERS.load(Ordering::Acquire)`, pairing with the
// `Ordering::AcqRel` RMWs in `new_async` and `Drop`. No payload is published.
pub fn live_renderer_count() -> usize {
    LIVE_RENDERERS.load(Ordering::Acquire)
}

/// What one covered-window trim released, as request sizes rather than residency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrimReport {
    /// The device refused GPU work, so nothing was released.
    pub refused: bool,
    /// The frame texture's extent before the trim; unchanged under the software presenter.
    pub frame_texture_before: (u32, u32),
    /// Bytes both present buffers requested before the trim.
    pub present_buffer_bytes_before: u64,
    /// Requested device bytes the trim gave back: the frame texture minus its 1x1 replacement,
    /// plus each present buffer minus its initial-size replacement.
    pub gpu_released_requested_bytes: u64,
}

impl TrimReport {
    /// The report for a trim the device refused: nothing released.
    #[must_use]
    pub fn refused() -> Self {
        Self { refused: true, ..Self::default() }
    }
}

/// CPU-side storage a renderer holds, split by owning class.
///
/// Deliberately not a single total. The parts have different lifetimes
/// and remedies: atlases grow with content, row caches follow viewport churn,
/// a software frame is sized by the window, and the vertex scratch follows the
/// largest recent frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RendererRetention {
    /// Rasterized glyph pixels mirrored on the CPU, and resident entries.
    pub glyph_atlas: ResourceAmount,
    /// Decoded inline-image pixels mirrored on the CPU, and resident entries.
    pub image_atlas: ResourceAmount,
    /// Cached per-row glyph instances and decoration metadata.
    pub row_glyph_cache: ResourceAmount,
    /// Cached per-row background and decoration quads.
    pub row_quad_cache: ResourceAmount,
    /// Windows software presentation buffer. Zero elsewhere.
    pub software_frame: ResourceAmount,
    /// Reused per-frame vertex assembly storage of the presentation pipeline, plus both atlas
    /// uploads' dirty and coalesced rect lists. CPU memory, so the GPU-buffer exclusion does not
    /// cover it; one item while the vertex scratch holds an allocation.
    pub vertex_scratch: ResourceAmount,
    /// Per-row ink records of the presented frame and one frame's staging; items are records.
    pub row_ink: ResourceAmount,
    /// The reused per-frame draw vectors held between frames; items are allocated vectors.
    pub frame_scratch: ResourceAmount,
    /// Kept tab titles and chrome runs plus the UI palette's colors; items are kept runs.
    pub chrome_cache: ResourceAmount,
}

impl RendererRetention {
    /// Class-tagged parts, for checking that every part is accounted for.
    ///
    /// **Nothing charges these.** `sonicterm-gpu` declares no dependency on
    /// `sonicterm-resource`, so this crate cannot reserve against a governor
    /// at all, and the renderer's memory reaches the app as a report rather
    /// than a ledger entry. The `ResourceClass` tags exist so a part cannot be
    /// added to this struct without deciding what it is.
    ///
    /// Wiring this to charging is not a small change, and the reason is here
    /// rather than in the caller that would attempt it: `image_atlas` maps to
    /// `InlineMediaRetained`, which a pane's decoded media already uses. They
    /// are different resident allocations — `capacity()` of one contiguous
    /// atlas buffer versus summed `len()` across separately-owned per-image
    /// `Vec`s — so charging both under one class would make the class mean two
    /// things and leave a reader unable to tell which allocation to act on.
    #[must_use]
    pub fn seam_classes(&self) -> [(ResourceClass, ResourceAmount); 9] {
        [
            (ResourceClass::GlyphAtlas, self.glyph_atlas),
            (ResourceClass::InlineMediaRetained, self.image_atlas),
            (ResourceClass::RowGlyphCache, self.row_glyph_cache),
            (ResourceClass::RowQuadCache, self.row_quad_cache),
            (ResourceClass::SoftwareFrame, self.software_frame),
            // The vertex scratch is CPU storage staged for the vertex-buffer upload.
            (ResourceClass::UploadStaging, self.vertex_scratch),
            (ResourceClass::RowInk, self.row_ink),
            (ResourceClass::FrameScratch, self.frame_scratch),
            (ResourceClass::ChromeCache, self.chrome_cache),
        ]
    }

    /// Sum of every part.
    #[must_use]
    pub fn total(&self) -> ResourceAmount {
        [
            self.glyph_atlas,
            self.image_atlas,
            self.row_glyph_cache,
            self.row_quad_cache,
            self.software_frame,
            self.vertex_scratch,
            self.row_ink,
            self.frame_scratch,
            self.chrome_cache,
        ]
        .into_iter()
        .fold(ResourceAmount::default(), |acc, part| ResourceAmount {
            bytes: acc.bytes.saturating_add(part.bytes),
            items: acc.items.saturating_add(part.items),
        })
    }
}

enum RendererBootstrap<'a> {
    Fresh(&'a ActiveEventLoop),
    Shared(GpuSharedContext),
    Recovered(RecoveredContext),
}

impl GpuRenderer {
    /// Build a renderer bound to `window`. Creates the wgpu surface +
    /// device + pipelines, the FontStack shaping and rasterization stacks, the glyph atlas,
    /// and seeds the initial cell metrics from `theme`'s configured
    /// font family / size / line height.
    pub fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        settings: RendererSettings<'_>,
    ) -> Result<Self> {
        let span = tracing::debug_span!(target: "render_timing", "renderer_init", window_id = ?window.id(), role = settings.role, shared = false);
        let _entered = span.enter();
        let timing = InitTiming::begin("renderer_new");
        let result = pollster::block_on(Self::new_async(
            window,
            theme,
            settings,
            RendererBootstrap::Fresh(event_loop),
        ));
        InitTiming::finish(
            timing,
            if result.is_ok() { InitOutcome::Ok } else { InitOutcome::Error },
        );
        result
    }

    /// Build a renderer that shares an existing wgpu instance, adapter, device,
    /// and queue with another window.
    ///
    /// Every window after the first takes this path, including New Window,
    /// warm-pool, and tear-out windows: one device serves all of them, so opening
    /// a window neither re-enumerates adapters nor allocates a second device. The
    /// surface, pipelines, and atlases are still per-window.
    pub fn new_with_shared_context(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        settings: RendererSettings<'_>,
        shared: GpuSharedContext,
    ) -> Result<Self> {
        let span = tracing::debug_span!(target: "render_timing", "renderer_init", window_id = ?window.id(), role = settings.role, shared = true);
        let _entered = span.enter();
        let timing = InitTiming::begin("renderer_new");
        let _ = event_loop;
        let result = pollster::block_on(Self::new_async(
            window,
            theme,
            settings,
            RendererBootstrap::Shared(shared),
        ));
        InitTiming::finish(
            timing,
            if result.is_ok() { InitOutcome::Ok } else { InitOutcome::Error },
        );
        result
    }

    /// Finish a negotiated startup on its window's event-loop thread without requesting another device or surface.
    pub fn finish_startup(
        prepared: RecoveredContext,
        theme: &Theme,
        settings: RendererSettings<'_>,
    ) -> Result<Self> {
        let window = Arc::clone(prepared.window());
        let span = tracing::debug_span!(target: "render_timing", "renderer_init", window_id = ?window.id(), role = settings.role, prepared = true);
        let _entered = span.enter();
        let timing = InitTiming::begin("renderer_finish");
        let result = pollster::block_on(Self::new_async(
            window,
            theme,
            settings,
            RendererBootstrap::Recovered(prepared),
        ));
        InitTiming::finish(
            timing,
            if result.is_ok() { InitOutcome::Ok } else { InitOutcome::Error },
        );
        result
    }

    /// Clone the handles a sibling window needs to share this renderer's GPU
    /// context, for passing to [`Self::new_with_shared_context`].
    ///
    /// The clones are wgpu reference-counted handles to one underlying device,
    /// not copies of it. They also share that device's containment state: once
    /// the device stops accepting work, a renderer built from them fails to
    /// construct.
    pub fn shared_context(&self) -> GpuSharedContext {
        GpuSharedContext {
            instance: self.instance.clone(),
            adapter: self.adapter.clone(),
            device: self.device.clone(),
            queue: self.queue.clone(),
            device_errors: Arc::clone(&self.device_errors),
        }
    }

    /// Whether `self` and `other` render through the same wgpu device.
    ///
    /// Test seam for the one-device-per-process contract. It compares the
    /// instance as well as the device: wgpu compares a device only by its id,
    /// and each instance allocates ids from the same start, so two unshared
    /// renderers can hold equal device ids. An instance compares by the address
    /// of its shared context, which [`GpuSharedContext`] clones rather than copies.
    #[doc(hidden)]
    #[must_use]
    pub fn shares_device_with(&self, other: &GpuRenderer) -> bool {
        self.instance == other.instance && self.device == other.device
    }

    // Ordering: `LIVE_RENDERERS.fetch_add(1, Ordering::AcqRel)`, pairing with
    // the `Ordering::AcqRel` decrement in `Drop`. Publishes no payload.
    async fn new_async(
        window: Arc<Window>,
        theme: &Theme,
        settings: RendererSettings<'_>,
        bootstrap: RendererBootstrap<'_>,
    ) -> Result<Self> {
        let RendererSettings {
            font_family,
            font_dirs,
            font_size,
            line_height_mult,
            font_weight_scale,
            subpixel_aa,
            padding,
            appearance,
            role,
            glyph_atlas_start,
        } = settings;
        let font_weight_scale = effective_font_weight_scale(font_weight_scale);
        let [padding_left, padding_right, padding_top, padding_bottom] = padding;
        let size = window.inner_size();
        // The OS scale converts logical font and chrome sizes into raster pixels.
        let sf = window.scale_factor() as f32;
        let (instance, surface, adapter, device, queue, errors, software_rendering) =
            match bootstrap {
                RendererBootstrap::Fresh(event_loop) => {
                    // Fresh has no device, so negotiation precedes the common renderer gate.
                    let timing = InitTiming::begin("instance_new");
                    let instance = new_instance(event_loop);
                    InitTiming::finish(timing, InitOutcome::Returned);
                    let timing = InitTiming::begin("create_surface");
                    let surface_result = instance.create_surface(window.clone());
                    InitTiming::finish(
                        timing,
                        if surface_result.is_ok() { InitOutcome::Ok } else { InitOutcome::Error },
                    );
                    let surface = surface_result.context("create surface")?;
                    let negotiated =
                        recovery_context::negotiate_device(&instance, &surface).await?;
                    (
                        instance,
                        surface,
                        negotiated.adapter,
                        negotiated.device,
                        negotiated.queue,
                        negotiated.device_errors,
                        negotiated.software_rendering,
                    )
                }
                RendererBootstrap::Shared(shared) => {
                    // When: Shared already owns a device, create only this window's surface before assembly.
                    if !shared.device_errors.accepts_gpu_work() {
                        // When: accepts_gpu_work rejects the shared device, no new surface can become drawable.
                        return Err(anyhow!("shared GPU device stopped accepting work"));
                    }
                    let timing = InitTiming::begin("instance_reuse");
                    let instance = shared.instance;
                    InitTiming::finish(timing, InitOutcome::Returned);
                    let timing = InitTiming::begin("create_surface");
                    let surface_result = instance.create_surface(window.clone());
                    InitTiming::finish(
                        timing,
                        if surface_result.is_ok() { InitOutcome::Ok } else { InitOutcome::Error },
                    );
                    let surface = surface_result.context("create surface")?;
                    let info = shared.adapter.get_info();
                    let software_rendering = detect_software_rendering(&info);
                    let device_memory_policy = device_memory_policy_from(software_rendering);
                    tracing::info!(backend = ?info.backend, name = %info.name, driver = %info.driver,
                    device_type = ?info.device_type, software_rendering,
                    device_memory_policy = ?device_memory_policy, "wgpu adapter reused");
                    (
                        instance,
                        surface,
                        shared.adapter,
                        shared.device,
                        shared.queue,
                        shared.device_errors,
                        software_rendering,
                    )
                }
                RendererBootstrap::Recovered(prepared) => {
                    // When: Recovered contains a negotiated device and surface, reuse both without native negotiation.
                    let (shared, candidate) = prepared.into_parts();
                    if !shared.device_errors.accepts_gpu_work() {
                        // When: accepts_gpu_work rejects the negotiated device, do not configure its retained surface.
                        return Err(anyhow!("prepared GPU device stopped accepting work"));
                    }
                    let software_rendering = detect_software_rendering(&shared.adapter.get_info());
                    (
                        shared.instance,
                        candidate.surface,
                        shared.adapter,
                        shared.device,
                        shared.queue,
                        shared.device_errors,
                        software_rendering,
                    )
                }
            };

        let format = TextureFormat::Bgra8UnormSrgb;
        let max_surface_dimension =
            device.limits().max_texture_dimension_2d.min(MAX_SURFACE_DIMENSION);
        let validated_size =
            validated_surface_size(size.width, size.height, max_surface_dimension).ok_or_else(
                || {
                    anyhow!(
                        "window surface {}x{} exceeds renderer limits (max dimension {}, max BGRA bytes {})",
                        size.width,
                        size.height,
                        max_surface_dimension,
                        MAX_SURFACE_BYTES
                    )
                },
            )?;
        tracing::debug!(
            target: "memory",
            requested_width = size.width,
            requested_height = size.height,
            width = validated_size.width,
            height = validated_size.height,
            bgra_bytes = validated_size.bytes,
            software_rendering,
            "renderer initial surface allocation accepted"
        );
        // Prefer Mailbox when the backend exposes it: Mailbox drops in-flight
        // superseded frames so a fast-typing user always sees the newest
        // keystroke without waiting a full vblank. Fall back to Fifo on
        // backends that don't advertise Mailbox (Fifo is universally supported
        // and remains the spec-mandated default).
        let timing = InitTiming::begin("surface_capabilities");
        let surface_caps = surface.get_capabilities(&adapter);
        InitTiming::finish(timing, InitOutcome::Returned);
        let hardware_present_mode = if surface_caps.present_modes.contains(&PresentMode::Mailbox) {
            PresentMode::Mailbox
        } else {
            PresentMode::Fifo
        };
        let hardware_alpha_mode = if appearance.backdrop == BackdropKind::Opaque {
            CompositeAlphaMode::Opaque
        } else {
            CompositeAlphaMode::PreMultiplied
        };
        let software_render_degrade =
            software_render_degrade_from(appearance.software_render_mode, software_rendering);
        let present_mode =
            if software_render_degrade { PresentMode::Fifo } else { hardware_present_mode };
        let alpha_mode =
            if software_render_degrade { CompositeAlphaMode::Opaque } else { hardware_alpha_mode };
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: validated_size.width,
            height: validated_size.height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: if software_render_degrade { 1 } else { 2 },
        };
        let init_scope = errors
            .enter_gpu_work("renderer.configure")
            .ok_or_else(|| anyhow!("GPU device stopped accepting work"))?;
        let timing = InitTiming::begin("surface_configure");
        surface.configure(&device, &config);
        InitTiming::finish(timing, InitOutcome::Returned);
        if !errors.accepts_gpu_work() {
            // When: `accepts_gpu_work` fails after `configure`, the surface cannot be acquired.
            tracing::debug!(target: "render_timing", operation = "surface_configure", phase = "gate", accepted = false, "renderer initialization");
            return Err(anyhow!("initial surface configure raised a contained GPU error"));
        }
        tracing::debug!(target: "render_timing", operation = "surface_configure", phase = "gate", accepted = true, "renderer initialization");
        init_scope.set_operation("renderer.init");

        // B3 GPU text path. Allocate independent glyph and inline-image
        // atlases up front so media pressure cannot recycle text UVs.
        // No more SwashRasterizer
        // prebake — chrome and grid share the glyph atlas, populated
        // on demand by the wezterm rasterizer on every miss.
        let timing = InitTiming::begin("present_pipeline");
        let present_pipeline = WeztermPipeline::new(&device, format, 4096);
        InitTiming::finish(timing, InitOutcome::Returned);
        let software_presenter = cfg!(target_os = "windows") && software_render_degrade;
        let timing = InitTiming::begin("frame_texture");
        let (frame_texture, frame_view) = build_frame_texture(
            &device,
            software_presenter,
            config.width,
            config.height,
            format,
            false,
        );
        InitTiming::finish(timing, InitOutcome::Returned);
        let timing = InitTiming::begin("frame_blitter");
        let frame_blitter = wgpu::util::TextureBlitter::new(&device, format);
        InitTiming::finish(timing, InitOutcome::Returned);
        let timing = InitTiming::begin("glyph_atlas");
        // The glyph atlas is the only growable atlas: it starts at its measured size and doubles
        // up to ATLAS_DIM before it evicts.
        let glyph_atlas = GlyphAtlas::growable(
            start_dim(sf, glyph_atlas_start),
            sonicterm_text::glyph_atlas::ATLAS_DIM,
        );
        InitTiming::finish(timing, InitOutcome::Returned);
        let timing = InitTiming::begin("image_atlas");
        let image_atlas = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);
        InitTiming::finish(timing, InitOutcome::Returned);
        let glyph_gpu_dimensions = desired_gpu_atlas_dimensions(software_presenter, &glyph_atlas);
        let image_gpu_dimensions = desired_gpu_atlas_dimensions(software_presenter, &image_atlas);
        let timing = InitTiming::begin("glyph_upload");
        let glyph_upload = AtlasUpload::new_sized(
            &device,
            glyph_gpu_dimensions.0,
            glyph_gpu_dimensions.1,
            present_pipeline.glyph_bind_group_layout(),
            AtlasBindingKind::Glyph,
        );
        InitTiming::finish(timing, InitOutcome::Returned);
        let timing = InitTiming::begin("image_upload");
        let image_upload = AtlasUpload::new_sized(
            &device,
            image_gpu_dimensions.0,
            image_gpu_dimensions.1,
            present_pipeline.image_bind_group_layout(),
            AtlasBindingKind::Image,
        );
        InitTiming::finish(timing, InitOutcome::Returned);
        tracing::debug!(
            target: "memory",
            renderer_role = role,
            window_id = ?window.id(),
            software_presenter,
            glyph_cpu_width = glyph_atlas.width(),
            glyph_cpu_height = glyph_atlas.height(),
            glyph_cpu_payload_bytes = atlas_payload_bytes(glyph_atlas.width(), glyph_atlas.height()),
            glyph_gpu_width = glyph_gpu_dimensions.0,
            glyph_gpu_height = glyph_gpu_dimensions.1,
            glyph_gpu_payload_bytes = glyph_upload.payload_bytes(),
            image_cpu_width = image_atlas.width(),
            image_cpu_height = image_atlas.height(),
            image_cpu_payload_bytes = atlas_payload_bytes(image_atlas.width(), image_atlas.height()),
            image_gpu_width = image_gpu_dimensions.0,
            image_gpu_height = image_gpu_dimensions.1,
            image_gpu_payload_bytes = image_upload.payload_bytes(),
            glyph_resident = glyph_atlas.len(),
            image_resident = image_atlas.len(),
            retained_inline_media_bytes = 0,
            payload_estimate = true,
            "renderer atlas payload initialized"
        );

        // FontStack derives raster metrics from point size at 72 * sf DPI; missing fonts use a scaled size estimate.
        let fs_dpi = (72.0 * sf).round() as usize;
        let timing = InitTiming::begin("font_stacks");
        let font_stacks =
            renderer_font_stacks(font_family, font_size, fs_dpi, font_weight_scale, font_dirs);
        InitTiming::finish(timing, InitOutcome::Returned);
        let timing = InitTiming::begin("cell_metrics");
        let (cell_w, natural_cell_h) =
            match font_stacks.body.as_ref().and_then(|s| s.cell_metrics_raster_px().ok()) {
                Some(m) => (m.cell_w as f32, m.cell_h as f32),
                None => (font_size * 0.6 * sf, font_size * 1.2 * sf),
            };
        InitTiming::finish(timing, InitOutcome::Returned);
        let line_height = natural_cell_h * line_height_mult.max(0.0).max(0.01);
        let cell_h = line_height;

        let bg = hex_to_wgpu_with_alpha(theme.colors.background.0.as_str(), appearance.opacity);
        let bg_rgba = hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
        let fg_default = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        let cursor_color = cursor_color_from_theme(theme);
        let cursor_text_color = cursor_text_color_from_theme(theme);
        let selection_color = hex_to_premultiplied_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        let tab_bar_bg = hex_to_premultiplied_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        let tab_active_bg = hex_to_premultiplied_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        let tab_inactive_bg =
            hex_to_premultiplied_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        let tab_active_fg = hex_to_chrome_color(theme.colors.tab.active_fg.0.as_str());
        let tab_inactive_fg = hex_to_chrome_color(theme.colors.tab.inactive_fg.0.as_str());
        let tab_separator =
            hex_to_premultiplied_rgba(theme.colors.tab.inactive_fg.0.as_str(), 0.45);
        // Hyperlink visuals: theme-aware. Use the theme's cursor color as the
        // accent (every bundled theme designates it). Underline reads as
        // deliberate at high opacity; the tint behind the run is subtle.
        let hyperlink_underline = hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 0.9);
        let splitter_color = splitter_color_from_theme(theme);
        let tint_alpha = match theme.appearance {
            sonicterm_render_model::boundary::cfg::theme::Appearance::Dark => {
                // When: `Appearance::Dark` — dark needs more accent before
                // the hyperlink tint reads as tinted at all.
                0.14
            }
            sonicterm_render_model::boundary::cfg::theme::Appearance::Light => {
                // When: `Appearance::Light` — 0.14 reads as a highlighter
                // stripe over the text rather than a hint beneath it.
                0.10
            }
        };
        let hyperlink_tint = hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), tint_alpha);
        let search_highlight =
            hex_to_premultiplied_rgba(theme.colors.bright.yellow.0.as_str(), 0.35);
        let search_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        let search_bg = hex_to_premultiplied_rgba(theme.colors.tab.bar_bg.0.as_str(), 0.95);
        // Cosmic-text Buffer / Metrics allocations deleted.
        // Chrome strings are shape+raster'd on demand inside `render()`
        // through `chrome_text::layout(...)`; there is no persistent
        // per-overlay text buffer to size at construction.

        if !errors.accepts_gpu_work() {
            // When: `accepts_gpu_work` fails after creation, the pipelines may be invalid.
            return Err(anyhow!("renderer creation raised a contained GPU error"));
        }
        drop(init_scope);
        // Counted here rather than earlier in `new`: every `?` above this
        // point returns without producing a renderer, so incrementing sooner
        // would charge for instances that never existed and never drop.
        LIVE_RENDERERS.fetch_add(1, Ordering::AcqRel);

        let renderer = Self {
            instance,
            adapter,
            software_rendering,
            software_render_degrade,
            device,
            queue,
            device_errors: errors,
            device_stop_reported: false,
            fault_invalid_glyph_upload: false,
            fault_frame_probe: None,
            fault_assembly_error: false,
            emitted_rows_probe: None,
            fault_present_error: false,
            fault_surface_occluded: false,
            #[cfg(target_os = "windows")]
            fault_stop_before_cached_present: false,
            present_calls: 0,
            surface,
            config,
            hardware_present_mode,
            hardware_alpha_mode,
            window,
            present_pipeline,
            frame_texture,
            frame_view,
            frame_blitter,
            glyph_atlas,
            glyph_upload,
            glyph_atlas_generation: 0,
            fault_atlas_change_during_assembly: false,
            image_atlas,
            image_upload,
            retained_inline_media_bytes: 0,
            frames_without_inline_media: 0,
            image_upload_rebuild_pending: false,
            inline_media_absent_since: None,
            glyph_atlas_retry_without_eviction: false,
            growth_episodes: crate::frame_stats::GrowthEpisodes::default(),
            retained_frame_readback: false,
            glyph_atlas_resets: 0,
            frame_texture_trimmed: false,
            frame_texture_installs: 0,
            font_family: font_family.to_string(),
            font_dirs: font_dirs.to_vec(),
            font_size,
            line_height,
            font_weight_scale,
            subpixel_aa,
            line_height_mult: line_height_mult.max(0.0).max(0.01),
            scale_factor: sf,
            cell_w,
            cell_h,
            padding_left,
            padding_right,
            padding_top,
            padding_bottom,
            bg,
            bg_opacity: appearance.opacity.clamp(0.0, 1.0),
            scrollbar_mode: appearance.scrollbar,
            panel_padding: appearance.panel_padding.max(0.0),
            fg_default,
            cursor_color,
            cursor_text_color,
            bg_rgba,
            cursor_shape: CursorShape::default(),
            cursor_blink: true,
            blink_epoch: Instant::now(),
            window_focused: true,
            inactive_pane_cursors: Vec::new(),
            pane_focus_flash: None,
            selection_color,
            tab_bar_bg,
            tab_active_bg,
            tab_inactive_bg,
            tab_active_fg,
            tab_inactive_fg,
            tab_close_override: None,
            hover_cursor: None,
            tab_separator,
            hyperlink_underline,
            splitter_color,
            hyperlink_tint,
            search_highlight,
            search_fg,
            search_bg,
            drag_chip_visual: None,
            last_frame_key: None,
            last_recolor: crate::cursor::RecolorRecord::default(),
            last_tab_ink: crate::cursor::RecolorBounds::Empty,
            injected_test_glyph: None,
            injected_row_glyph: None,
            fault_surface_acquire: None,
            row_ink: crate::row_ink::RowInkTable::default(),
            presented_damage: PresentedDamageRecorder::default(),
            presented_fields: PresentedFields::default(),
            preedit_glyph_cache: None,
            skipped_frames: 0,
            successful_frame_count: 0,
            frame_sink: None,
            present_hook: None,
            #[cfg(target_os = "windows")]
            software_frame: None,
            render_timing_label: role,
            tab_bar_visible: true,
            titlebar_inset: 0.0,
            last_missing_chars: Vec::new(),
            last_missing_chrome_chars: Vec::new(),
            completeness: None,
            // `shape_cache` field deleted with the cosmic-text path.
            font_stack: font_stacks.body,
            tab_title_font: TabTitleFont::new(
                font_family,
                font_size,
                font_weight_scale,
                sf,
                font_stacks.tab_title,
            ),
            palette_footer_font_stack: font_stacks.palette_footer,
            // Seeded from the constructor theme, so the first frame derives no palette.
            chrome_caches: crate::chrome_cache::ChromeCaches::new(theme),
            // Ablation: tab titles and chrome runs are prepared cold on every frame.
            chrome_reuse: false,
            // Ablation: the frame scratch is dropped on every restore, so each pass allocates afresh.
            frame_scratch: {
                let home = frame_scratch::ScratchHome::new();
                home.set_reuse(false);
                home
            },
            row_glyph_cache: sonicterm_text::row_glyph_cache::RowGlyphCache::new(),
            line_quad_cache: crate::row_quad_cache::LineQuadCache::new(),
            last_emit_origins: Vec::new(),
            last_pane_layout: Vec::new(),
            style_rev: 0,
            applied_fonts: None,
            unattributed_apply: false,
            fallback_waker: None,
            drag_chip: None,
            async_loader: None,
        };
        renderer.log_subpixel_aa_policy();
        Ok(renderer)
    }

    /// Checked resize used by window-event paths that must react to rejection.
    ///
    /// While the device is stopped, a valid size is recorded and `true` is
    /// returned without reconfiguring the surface.
    #[must_use]
    pub fn try_resize(&mut self, width: u32, height: u32) -> bool {
        self.try_resize_outcome(width, height) != ResizeOutcome::Rejected
    }

    /// Checked resize that reports whether the surface actually changed size.
    ///
    /// `Unchanged` reconfigures nothing and keeps the retained frame; only `Changed` resizes.
    #[must_use]
    pub fn try_resize_outcome(&mut self, width: u32, height: u32) -> ResizeOutcome {
        let max_dimension =
            self.device.limits().max_texture_dimension_2d.min(MAX_SURFACE_DIMENSION);
        let Some(size) = validated_surface_size(width, height, max_dimension) else {
            // When: `validated_surface_size` returns None. The old surface
            // stays configured — refusing is recoverable, resizing is not.
            tracing::error!(
                target: "memory",
                requested_width = width,
                requested_height = height,
                current_width = self.config.width,
                current_height = self.config.height,
                max_dimension,
                max_bgra_bytes = MAX_SURFACE_BYTES,
                "renderer rejected unsafe surface resize"
            );
            return ResizeOutcome::Rejected;
        };
        if classify_resize((self.config.width, self.config.height), Some(&size))
            == ResizeOutcome::Unchanged
        {
            // When: the validated size equals the configured one — common on
            // scale events. Reconfiguring would drop both caches for nothing.
            return ResizeOutcome::Unchanged;
        }
        let planned_frame_texture =
            frame_texture_extent(self.uses_windows_software_presenter(), size.width, size.height);
        tracing::debug!(
            target: "memory",
            window = self.render_timing_label,
            old_width = self.config.width,
            old_height = self.config.height,
            requested_width = width,
            requested_height = height,
            width = size.width,
            height = size.height,
            bgra_bytes = size.bytes,
            software_rendering = self.software_rendering,
            frame_texture_width = planned_frame_texture.0,
            frame_texture_height = planned_frame_texture.1,
            frame_texture_payload_bytes = frame_texture_payload_bytes(planned_frame_texture),
            payload_estimate = true,
            "renderer surface resize accepted"
        );
        self.config.width = size.width;
        self.config.height = size.height;
        if let Some(_scope) = self.device_errors.enter_gpu_work("try_resize") {
            // A stopped device records the size only; the surface is not reconfigured.
            self.surface.configure(&self.device, &self.config);
            self.rebuild_frame_texture();
        }
        // Geometry change → force the next frame to actually render.
        self.last_frame_key = None;
        self.last_pane_layout.clear();
        // Field clips and caret rects are in the old surface's pixels.
        self.presented_fields.clear();
        // Cell layout and absolute-row positioning both change with
        // the surface size; cached glyph instances would land at the
        // wrong NDC coordinates.
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        // Post-glyphon there is no persistent text buffer to
        // resize — chrome strings are re-shaped through
        // `chrome_text::layout` on every frame, picking up the new
        // surface dims via the per-call `(sw, sh)` parameter. The
        // legacy `*_buffer.set_size(...)` block that lived here is
        // gone with the glyphon plumbing.
        ResizeOutcome::Changed
    }

    /// Top inset reserved above the grid: OS titlebar band (when active)
    /// plus top window padding, returned in **raster px** so it lives in
    /// the same coordinate system as `config.width`/`config.height` and the
    /// rest of the renderer. The tab bar is always bottom-pinned,
    /// so its height is reserved via [`Self::bottom_inset`] instead of here.
    ///
    /// `titlebar_inset` and `padding_top` are stored in logical px (matching
    /// the config schema); both are scaled by [`Self::scale_factor`] before
    /// being summed so a 2x Retina display gets the right number of raster
    /// rows reserved for the OS titlebar band + user padding. Without the
    /// scale the grid was reporting one fewer row than the window could fit,
    /// leaving a dead strip below the last painted row that showed the
    /// surface clear color instead of vim's bg.
    pub fn top_inset(&self) -> f32 {
        (self.titlebar_inset + self.padding_top) * self.scale_factor
    }

    /// Bottom inset reserved below the grid for the bottom-pinned tab bar,
    /// in **raster px** (same units as `config.height`). Returns 0 when
    /// the bar is hidden; the consumer still subtracts `padding_bottom *
    /// scale_factor` separately so window padding still applies when the
    /// bar is off.
    pub fn bottom_inset(&self) -> f32 {
        if self.tab_bar_visible {
            self.tab_bar_logical_height()
        } else {
            // When: `!tab_bar_visible` — the bar reserves nothing. Window
            // padding is applied separately by the caller.
            0.0
        }
    }

    /// Y offset (in raster px) at which the tab bar layout should be
    /// anchored. The tab bar is always pinned to the bottom of the window.
    /// Callers pass this into [`TabBarLayout::with_top_offset`].
    pub fn tab_bar_y_offset(&self) -> f32 {
        let surf_h = self.config.height as f32;
        (surf_h - self.tab_bar_logical_height()).max(0.0)
    }

    /// Raster-pixel height of the tab bar for the renderer's current font
    /// size. Derived from [`tab_bar_height`] (logical formula) and scaled
    /// to raster px to live in the same coordinate system as
    /// `config.width`/`config.height`. WezTerm fancy-mode parity: `font_size × 2 + 12` clamped.
    pub fn tab_bar_logical_height(&self) -> f32 {
        tab_bar_height(self.font_size) * self.scale_factor
    }

    /// The titlebar inset alone (logical px) — the y-offset at which the
    /// tab bar strip itself begins, regardless of whether the bar is
    /// visible. Used by hit-testing / tab-bar layout to shift their
    /// rectangles down so clicks under the OS titlebar are not consumed
    /// as tab activations.
    pub fn titlebar_inset(&self) -> f32 {
        self.titlebar_inset
    }

    /// Set the reserved OS titlebar band height (logical px). Called once
    /// from `app.rs` after creating each window so the renderer knows
    /// whether the macOS integrated-titlebar style is in effect.
    /// Invalidates the cached frame key so the next render() relays out.
    pub fn set_titlebar_inset(&mut self, inset: f32) {
        let clamped = inset.max(0.0);
        if (self.titlebar_inset - clamped).abs() < f32::EPSILON {
            // When: `titlebar_inset` is unchanged. This runs on every
            // window-state event, so clearing the key would relayout each time.
            return;
        }
        self.titlebar_inset = clamped;
        self.last_frame_key = None;
        self.last_pane_layout.clear();
    }

    /// Show or hide the tab bar. Returns `true` if the visibility actually
    /// changed (so callers can decide whether to recompute grid dims).
    /// Invalidates the cached frame key so the next `render()` call rebuilds.
    pub fn set_tab_bar_visible(&mut self, visible: bool) -> bool {
        if self.tab_bar_visible == visible {
            // When: `tab_bar_visible == visible`. `false` tells the caller no
            // grid resize is needed for identical geometry.
            return false;
        }
        self.tab_bar_visible = visible;
        self.last_frame_key = None;
        self.last_pane_layout.clear();
        true
    }

    /// Whether the tab bar is currently shown.
    pub fn tab_bar_visible(&self) -> bool {
        self.tab_bar_visible
    }

    /// Update the requested LCD subpixel coverage order without rebuilding fonts or atlases.
    pub fn set_subpixel_aa_mode(&mut self, mode: SubpixelAaMode) -> bool {
        if self.subpixel_aa == mode {
            // When: `subpixel_aa == mode`, the retained frame already reflects this request.
            return false;
        }
        self.subpixel_aa = mode;
        self.last_frame_key = None;
        self.log_subpixel_aa_policy();
        self.request_window_redraw();
        true
    }

    /// Requested LCD subpixel coverage order before platform/capability fallback.
    #[doc(hidden)]
    #[must_use]
    pub const fn subpixel_aa_mode(&self) -> SubpixelAaMode {
        self.subpixel_aa
    }

    /// LCD mode after platform, target-opacity, and presenter capability fallback.
    #[doc(hidden)]
    #[must_use]
    pub fn effective_subpixel_aa_mode(&self) -> SubpixelAaMode {
        effective_subpixel_aa_mode(
            self.subpixel_aa,
            cfg!(windows),
            self.hardware_alpha_mode == CompositeAlphaMode::Opaque
                && self.bg_opacity >= 1.0 - f32::EPSILON,
            self.uses_windows_software_presenter(),
            self.device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING),
        )
    }

    fn log_subpixel_aa_policy(&self) {
        let opaque_target = self.hardware_alpha_mode == CompositeAlphaMode::Opaque
            && self.bg_opacity >= 1.0 - f32::EPSILON;
        let software_presenter = self.uses_windows_software_presenter();
        let dual_source_supported =
            self.device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING);
        let effective = effective_subpixel_aa_mode(
            self.subpixel_aa,
            cfg!(windows),
            opaque_target,
            software_presenter,
            dual_source_supported,
        );
        tracing::info!(
            target: "render_policy",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            requested = ?self.subpixel_aa,
            ?effective,
            windows_host = cfg!(windows),
            opaque_target,
            software_presenter,
            dual_source_supported,
            "renderer LCD subpixel policy"
        );
    }

    /// Update scrollbar visibility policy from live config reload.
    ///
    /// `render()` folds this cached mode into the per-pane scrollbar emit
    /// path, so config changes must invalidate the frame key explicitly;
    /// otherwise an idle window could keep the previous scrollbar quads
    /// until some unrelated grid/theme/input change forced a redraw.
    pub fn set_scrollbar_mode(
        &mut self,
        mode: sonicterm_render_model::boundary::cfg::config::ScrollbarMode,
    ) -> bool {
        if self.scrollbar_mode == mode {
            // When: `scrollbar_mode == mode`. Every reload calls this, so the
            // early return keeps an unrelated edit from busting an idle frame.
            return false;
        }
        self.scrollbar_mode = mode;
        self.last_frame_key = None;
        true
    }

    /// Current scrollbar visibility policy. Test-only inspector for the
    /// live-reload path; production code pushes updates via
    /// [`Self::set_scrollbar_mode`].
    #[doc(hidden)]
    pub fn scrollbar_mode(&self) -> sonicterm_render_model::boundary::cfg::config::ScrollbarMode {
        self.scrollbar_mode
    }

    /// Update overlay panel padding from live config reload.
    pub fn set_panel_padding(&mut self, padding: f32) -> bool {
        let padding = padding.max(0.0);
        if (self.panel_padding - padding).abs() < f32::EPSILON {
            // When: `(self.panel_padding - padding).abs() < f32::EPSILON` — a
            // reload carried the same padding; `false` skips the relayout.
            return false;
        }
        self.panel_padding = padding;
        self.last_frame_key = None;
        true
    }

    /// Update the cursor shape. The frame key carries the shape, so the next
    /// render damages only the cursor row rather than the whole surface.
    pub fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.cursor_shape = shape;
    }

    /// Current cursor shape.
    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

    /// Enable or disable the cursor blink. Resets the blink phase so
    /// the user always sees a full-brightness cursor immediately after
    /// flipping the setting (no random mid-cycle pop).
    pub fn set_cursor_blink(&mut self, blink: bool) {
        if self.cursor_blink == blink {
            // When: `self.cursor_blink == blink` — returning also preserves
            // `blink_epoch`, so a reload cannot restart the blink phase.
            return;
        }
        self.cursor_blink = blink;
        self.blink_epoch = Instant::now();
    }

    /// Whether the cursor is currently configured to blink.
    pub fn cursor_blink(&self) -> bool {
        self.cursor_blink
    }

    /// Suggested wall-clock interval between blink-only redraws. The
    /// app loop schedules a redraw at this cadence whenever the cursor
    /// is visible AND [`Self::cursor_blink`] is true; otherwise nothing
    /// new would render and the request would be wasted.
    pub fn blink_redraw_interval(&self) -> std::time::Duration {
        ui_cursor::redraw_interval()
    }

    /// Wall-clock instant at which the next blink phase bucket begins,
    /// or `None` when blinking is disabled. The app loop should set
    /// `ControlFlow::WaitUntil(this)` so the renderer wakes up exactly
    /// at bucket boundaries instead of busy-looping `request_redraw()`
    /// after every frame (the project landmine flagged).
    pub fn next_blink_redraw_at(&self) -> Option<Instant> {
        // Blink-driven redraws are intentionally disabled in the idle
        // path. Re-shaping the grid 26×/sec just to fade the cursor
        // alpha melted the headless CPU bench at 17% — see the
        // `cursor_phase: 0` comment where `FrameKey` is built. The
        // cursor still re-evaluates its alpha on every real redraw
        // (PTY bytes, keys, mouse, resize, focus), which keeps it
        // visibly pulsing whenever the user is doing anything. Pure
        // idle leaves the cursor frozen at a fixed (always-visible)
        // alpha — strictly better than burning CPU on a backgrounded
        // window. The remaining fields (`cursor_blink`,
        // `window_focused`, `blink_epoch`) are kept so a future
        // event-driven re-enable (e.g. only blink for the first 5s
        // after a keypress) can pick the right starting bucket.
        let _ = (&self.cursor_blink, &self.window_focused, &self.blink_epoch);
        None
    }

    /// Update the cached "is the OS window focused" flag. Hides the
    /// text cursor when `false`. Bumps the FrameKey via
    /// `Self::last_frame_key` so the next render is not skipped by
    /// the cache.
    /// Host-side storage this renderer holds, split by the class that owns it.
    ///
    /// Every figure here already existed and was unreachable from outside the
    /// crate: `GlyphAtlas::retained_amount` and
    /// `SoftwareFrame::retained_bytes` were both written, tested, and
    /// called by nothing. What was missing was a way for the owner of the
    /// governor to read them, which is what this provides.
    ///
    /// **CPU-side only.** GPU textures and buffers are not included: their
    /// memory belongs to the driver, `wgpu` exposes no size accounting for
    /// them, and a figure invented here would be a guess presented as a
    /// measurement. The atlases are the CPU mirrors that back those textures,
    /// so they track the same content without claiming to measure VRAM.
    #[must_use]
    pub fn retained_amounts(&self) -> RendererRetention {
        RendererRetention {
            glyph_atlas: self.glyph_atlas.retained_amount(),
            image_atlas: self.image_atlas.retained_amount(),
            row_glyph_cache: self.row_glyph_cache.retained_amount(),
            row_quad_cache: self.line_quad_cache.retained_amount(),
            software_frame: self.software_frame_retained_amount(),
            vertex_scratch: self.upload_staging_retained(),
            row_ink: self.row_ink.retained_amount(),
            frame_scratch: self.frame_scratch.retained_amount(),
            chrome_cache: ResourceAmount {
                bytes: self.chrome_caches.retained_bytes(),
                items: self.chrome_caches.items(),
            },
        }
    }

    /// The glyph atlas's size, packing, growth, eviction and fit facts.
    #[must_use]
    pub fn glyph_atlas_facts(&self) -> GlyphAtlasFacts {
        GlyphAtlasFacts::of(&self.glyph_atlas)
    }

    /// The `UploadStaging` part: the vertex scratch plus both atlas uploads' CPU storage.
    fn upload_staging_retained(&self) -> ResourceAmount {
        upload_staging_amount(
            self.present_pipeline.vertex_scratch_retained(),
            &self.glyph_upload,
            &self.image_upload,
        )
    }

    /// Release one permanently removed pane's cached rows without evicting peers.
    ///
    /// Both caches and retention snapshots are event-loop-owned through `&mut
    /// GpuRenderer`, so the fixed glyph-then-quad order is observed atomically by
    /// every app caller. Cross-window detach calls this for the source renderer;
    /// same-renderer reorder keeps its entries because no pane leaves that owner.
    pub fn invalidate_pane_caches(&mut self, pane_id: u64) {
        self.row_glyph_cache.invalidate_pane(pane_id);
        self.line_quad_cache.invalidate_pane(pane_id);
        self.row_ink.drop_pane(pane_id);
    }

    #[cfg(windows)]
    fn software_frame_retained_amount(&self) -> ResourceAmount {
        self.software_frame.as_ref().map_or_else(ResourceAmount::default, |frame| ResourceAmount {
            bytes: frame.retained_bytes(),
            items: usize::from(frame.retained_bytes() > 0),
        })
    }

    /// Non-Windows builds have no software presentation path, so this is
    /// always zero rather than absent — a caller charging it should not need
    /// a platform branch to do so.
    #[cfg(not(windows))]
    fn software_frame_retained_amount(&self) -> ResourceAmount {
        ResourceAmount::default()
    }

    /// Update the cached keyboard-focus flag for the OS window.
    ///
    /// The text cursor is hidden while the window is unfocused. The frame key
    /// carries focus, so the next render damages only the cursor rows and the
    /// tab bar rather than the whole surface.
    pub fn set_window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
    }

    /// Whether the OS window currently has keyboard focus.
    pub fn window_focused(&self) -> bool {
        self.window_focused
    }

    /// Set the window label that renderer-internal timing logs are tagged with.
    ///
    /// Affects diagnostics only; no frame state changes, so the frame key is
    /// deliberately left intact.
    pub fn set_render_timing_label(&mut self, label: &'static str) {
        self.render_timing_label = label;
    }

    /// Start the short focus-confirmation flash on `pane_id`.
    ///
    /// The flash is bounded by `PANE_FOCUS_FLASH_DURATION` and animates
    /// through the frame key's quantised bucket, so this requests one redraw
    /// rather than starting a repeating timer.
    pub fn flash_pane_focus(&mut self, pane_id: u64) {
        self.pane_focus_flash = Some((pane_id, Instant::now()));
        self.last_frame_key = None;
        self.request_window_redraw();
    }

    /// Accept the historical per-frame inactive-pane cursor list.
    /// Inactive panes no longer draw cursors, so any previously cached
    /// cursor records are cleared and new records are ignored.
    pub fn set_inactive_pane_cursors(&mut self, _cursors: Vec<InactivePaneCursor>) {
        if !self.inactive_pane_cursors.is_empty() {
            self.inactive_pane_cursors.clear();
            self.last_frame_key = None;
        }
    }

    fn pane_focus_flash_bucket(&mut self, now: Instant) -> u8 {
        let Some((_, started_at)) = self.pane_focus_flash else {
            // When: `self.pane_focus_flash` is None — no flash is running, and
            // 0 keeps the bucket out of the frame key so nothing animates.
            return 0;
        };
        let elapsed = now.saturating_duration_since(started_at);
        let Some((bucket, _)) = pane_focus_flash_sample(elapsed) else {
            // When: `pane_focus_flash_sample(elapsed)` returns `None`, the bounded
            // flash expired; clearing its state ends the redraw chain.
            self.pane_focus_flash = None;
            return 0;
        };
        bucket
    }

    fn pane_focus_flash_alpha(&self, now: Instant) -> Option<(u64, f32)> {
        let (pane_id, started_at) = self.pane_focus_flash?;
        let elapsed = now.saturating_duration_since(started_at);
        pane_focus_flash_sample(elapsed).map(|(_, alpha)| (pane_id, alpha))
    }

    /// Return the pane targeted by the live focus flash, for integration diagnostics.
    #[doc(hidden)]
    pub fn __test_pane_focus_flash_target(&self) -> Option<u64> {
        self.pane_focus_flash.map(|(pane_id, _)| pane_id)
    }

    /// Current physical surface width in pixels.
    pub fn width(&self) -> u32 {
        self.config.width
    }

    /// Current physical surface height in pixels.
    pub fn height(&self) -> u32 {
        self.config.height
    }

    /// Left padding (logical px). Kept for backward compatibility with
    /// callers that pre-date per-side padding; new code should prefer
    /// the per-side accessors below.
    pub fn padding(&self) -> f32 {
        self.padding_left
    }

    /// Left padding in logical pixels.
    pub fn padding_left(&self) -> f32 {
        self.padding_left
    }
    /// Right padding in logical pixels.
    pub fn padding_right(&self) -> f32 {
        self.padding_right
    }
    /// Top padding in logical pixels (above any tab bar / titlebar inset).
    pub fn padding_top(&self) -> f32 {
        self.padding_top
    }
    /// Bottom padding in logical pixels.
    pub fn padding_bottom(&self) -> f32 {
        self.padding_bottom
    }

    /// Left padding scaled to **raster px**, i.e. the same coordinate
    /// system as `config.width`/`config.height` and the rest of the
    /// renderer. Prefer this over [`Self::padding_left`] when
    /// building geometry that will be handed back to the renderer (e.g.
    /// the per-pane rect in `compute_pane_rects_for`). Mixing the
    /// logical-px accessor with raster surface dims off-by-ones the row
    /// count and leaves a dead strip below the last painted row.
    pub fn padding_left_px(&self) -> f32 {
        self.padding_left * self.scale_factor
    }
    /// Right padding scaled to raster px. See [`Self::padding_left_px`].
    pub fn padding_right_px(&self) -> f32 {
        self.padding_right * self.scale_factor
    }
    /// Top padding scaled to raster px. See [`Self::padding_left_px`].
    /// `top_inset()` includes titlebar padding; `pane_grid_origin()` also
    /// includes pane position and bottom-alignment slack after layout.
    pub fn padding_top_px(&self) -> f32 {
        self.padding_top * self.scale_factor
    }
    /// Bottom padding scaled to raster px. See [`Self::padding_left_px`].
    pub fn padding_bottom_px(&self) -> f32 {
        self.padding_bottom * self.scale_factor
    }
    /// Current planned text origin for a pane, absent before layout or after a geometry change.
    pub fn pane_grid_origin(&self, pane_id: u64) -> Option<[f32; 2]> {
        self.last_pane_layout
            .iter()
            .find(|pane| pane.id == pane_id)
            .map(|pane| [pane.origin_x_logical, pane.origin_y_logical])
    }

    /// Test-only: the configured surface size `(width, height)` in physical pixels, which a
    /// `Resized` at the same size leaves `Unchanged`.
    #[doc(hidden)]
    #[must_use]
    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Rendered layout of a pane from the most recent frame, absent before layout.
    #[doc(hidden)]
    pub fn pane_layout(&self, pane_id: u64) -> Option<PaneLayoutSnapshot> {
        self.last_pane_layout.iter().find(|pane| pane.id == pane_id).copied()
    }

    /// Test-only raster-pixel pane origins from the last render; production code must not depend on this diagnostic.
    #[doc(hidden)]
    pub fn last_emitted_origins(&self) -> Vec<(u64, [f32; 2])> {
        self.last_emit_origins.clone()
    }

    /// Translate a scrollback-absolute row into the row index visible in the
    /// current viewport. Returns `None` when the row lies above or below the
    /// rendered viewport.
    #[doc(hidden)]
    pub fn viewport_relative_row(
        absolute_row: usize,
        view_top_abs: u64,
        visible_rows: u16,
    ) -> Option<u16> {
        let visible_row = absolute_row as i128 - i128::from(view_top_abs);
        (0..i128::from(visible_rows)).contains(&visible_row).then_some(visible_row as u16)
    }

    /// Resolve the viewport top used by the renderer after clamping explicit
    /// scrollback requests to the live bottom.
    ///
    /// The clamp only bounds the index; it cannot tell which row an index
    /// named before history evicted rows. Callers pass a projection the app
    /// has already rebased for eviction.
    #[doc(hidden)]
    pub fn resolved_view_top_abs(grid: &Grid, viewport_top_abs: Option<u64>) -> u64 {
        let live_top_abs = grid.scrollback_len() as u64;
        viewport_top_abs.map(|v| v.min(live_top_abs)).unwrap_or(live_top_abs)
    }

    /// Compatibility name for the same Grid clamp; callers must already have rebased history eviction.
    #[doc(hidden)]
    pub fn resolved_view_top_abs_legacy(
        grid: &sonicterm_render_model::boundary::grid::grid::Grid,
        viewport_top_abs: Option<u64>,
    ) -> u64 {
        Self::resolved_view_top_abs(grid, viewport_top_abs)
    }

    /// Adjust a viewport after copy-mode movement so the scrollback-absolute
    /// copy-mode cursor remains visible.
    #[doc(hidden)]
    pub fn copy_mode_view_top_after_move(
        copy_mode: &CopyModeState,
        grid: &Grid,
        viewport_top_abs: Option<u64>,
    ) -> Option<u64> {
        let view_top_abs = Self::resolved_view_top_abs(grid, viewport_top_abs);
        let cursor_row = copy_mode.cursor.1 as u64;
        let viewport_height = u64::from(grid.rows);
        if cursor_row < view_top_abs {
            Some(cursor_row)
        } else if cursor_row >= view_top_abs.saturating_add(viewport_height) {
            // When: `cursor_row >= view_top_abs + viewport_height` — cursor
            // below the viewport; scroll so it lands on the last row.
            Some(cursor_row.saturating_add(1).saturating_sub(viewport_height))
        } else {
            // When: `cursor_row` is already inside the viewport — return the
            // caller's `viewport_top_abs` so an explicit scroll survives.
            viewport_top_abs
        }
    }

    /// Legacy-Grid variant. See `resolved_view_top_abs_legacy`.
    #[doc(hidden)]
    pub fn copy_mode_view_top_after_move_legacy(
        copy_mode: &CopyModeState,
        grid: &sonicterm_render_model::boundary::grid::grid::Grid,
        viewport_top_abs: Option<u64>,
    ) -> Option<u64> {
        let view_top_abs = Self::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        let cursor_row = copy_mode.cursor.1 as u64;
        let viewport_height = u64::from(grid.rows);
        if cursor_row < view_top_abs {
            Some(cursor_row)
        } else if cursor_row >= view_top_abs.saturating_add(viewport_height) {
            // When: `cursor_row >= view_top_abs + viewport_height` — cursor
            // below the viewport; scroll so it lands on the last row.
            Some(cursor_row.saturating_add(1).saturating_sub(viewport_height))
        } else {
            // When: `cursor_row` is already visible — return the caller's
            // `viewport_top_abs` so an explicit scroll position survives.
            viewport_top_abs
        }
    }

    /// Emit copy-mode selection and cursor quads using scrollback-absolute
    /// copy-mode coordinates translated into viewport-relative rows.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn emit_copy_mode_quads(
        copy_mode: &CopyModeState,
        grid: &Grid,
        view_top_abs: u64,
        origin_x: f32,
        origin_y: f32,
        cell_w: f32,
        cell_h: f32,
        sw: f32,
        sh: f32,
        selection_color: [f32; 4],
        cursor_color: [f32; 4],
        quads: &mut Vec<QuadInstance>,
        snapped_cell_x: &[f32],
    ) -> Option<(f32, f32)> {
        // derive selection-row x/w and copy-cursor cx from the
        // shared snapped-edge cache so copy-mode overlays share
        // device-pixel edges with adjacent glyph cells at fractional
        // DPI. Empty-cache fallback preserves the raw arithmetic for
        // callers (debug/test helpers) that don't carry a real cache;
        // integer scales make the two identical via the identity fast
        // path in `snap_to_device_pixels`.
        let raw_fallback = snapped_cell_x.is_empty();
        if let Some((start, end)) = copy_mode.selected_range() {
            // When: `copy_mode.selected_range()` is Some — copy mode has an
            // anchored selection. The cursor quad below is emitted regardless.
            for row_abs in start.1..=end.1 {
                let Some(visible_row) =
                    Self::viewport_relative_row(row_abs, view_top_abs, grid.rows)
                else {
                    // When: `viewport_relative_row` is None — a selection may
                    // span scrollback, so off-screen rows are skipped.
                    continue;
                };
                let col_a = if row_abs == start.1 {
                    start.0
                } else {
                    // When: `row_abs != start.1` — an interior or final row,
                    // which starts at column 0, not the selection anchor.
                    0
                }
                .min(grid.cols as usize);
                let col_b = if row_abs == end.1 {
                    end.0.min(grid.cols.saturating_sub(1) as usize)
                } else {
                    // When: `row_abs != end.1` — an interior row of a
                    // multi-row selection, which runs to the right edge.
                    grid.cols.saturating_sub(1) as usize
                };
                if col_b < col_a {
                    // When: `col_b < col_a` — an empty span on this row, which
                    // would still push a zero-width quad into the buffer.
                    continue;
                }
                let end_exclusive = col_b + 1;
                let (x, w) = if raw_fallback {
                    (origin_x + col_a as f32 * cell_w, (end_exclusive - col_a) as f32 * cell_w)
                } else {
                    // When: `!raw_fallback` — a real snapped-edge cache, so the
                    // quad takes device-pixel edges shared with glyph cells.
                    let cache_end = end_exclusive.min(snapped_cell_x.len() - 1);
                    let lo = snapped_cell_x[col_a];
                    let hi = snapped_cell_x[cache_end];
                    (lo, hi - lo)
                };
                let y = origin_y + f32::from(visible_row) * cell_h;
                quads.push(QuadInstance {
                    rect: px_to_ndc(x, y, w, cell_h, sw, sh),
                    color: selection_color,
                    ..Default::default()
                });
            }
        }

        if copy_mode.is_read_only() {
            // When: `copy_mode.is_read_only()` — a read-only view has no
            // editable cursor to draw, so only the selection quads above stand.
            return None;
        }

        let visible_row = Self::viewport_relative_row(copy_mode.cursor.1, view_top_abs, grid.rows)?;
        let copy_col = copy_mode.cursor.0.min(grid.cols.saturating_sub(1) as usize);
        let (cx, cw) = if raw_fallback {
            (origin_x + copy_col as f32 * cell_w, cell_w)
        } else {
            // When: `!raw_fallback` — a real snapped-edge cache, so the cursor
            // takes the same edges as the glyph cell beneath it.
            let lo = snapped_cell_x[copy_col];
            let hi = snapped_cell_x[(copy_col + 1).min(snapped_cell_x.len() - 1)];
            (lo, hi - lo)
        };
        let cy = origin_y + f32::from(visible_row) * cell_h;
        quads.push(QuadInstance {
            rect: px_to_ndc(cx, cy, cw, cell_h, sw, sh),
            color: cursor_color,
            ..Default::default()
        });
        Some((cx, cy))
    }

    /// Fix 1 test hook: number of panes the most recent
    /// `render()` call received in its slice. The integration test
    /// asserts this equals the active tab's pane count so a regression
    /// to a single-element slice (the original bug) is caught
    /// mechanically. Production code must not depend on this.
    #[doc(hidden)]
    pub fn last_panes_received(&self) -> usize {
        self.last_emit_origins.len()
    }

    /// Update all four padding values at once (used by the live config
    /// reload path so editing `sonicterm.toml` takes effect without restart).
    /// Invalidates the cached frame so the next render relays out.
    pub fn set_padding(&mut self, padding: [f32; 4]) {
        let [l, r, t, b] = padding;
        if (self.padding_left - l).abs() < f32::EPSILON
            && (self.padding_right - r).abs() < f32::EPSILON
            && (self.padding_top - t).abs() < f32::EPSILON
            && (self.padding_bottom - b).abs() < f32::EPSILON
        {
            // When: all four `.abs() < f32::EPSILON` — a reload carried the
            // same padding, so relayout and a frame-key clear are wasted.
            return;
        }
        self.padding_left = l;
        self.padding_right = r;
        self.padding_top = t;
        self.padding_bottom = b;
        self.last_frame_key = None;
        self.last_pane_layout.clear();
    }

    /// Render surface width and height in raster pixels, despite the compatibility method name.
    pub fn logical_size(&self) -> (f32, f32) {
        (self.config.width as f32, self.config.height as f32)
    }

    /// Snapshot of every codepoint the previous `render()` call could
    /// not produce a glyph tile for (i.e. that drew a tofu outline).
    /// Whitespace is filtered out — those are intentionally blank.
    ///
    /// Test-only diagnostic. Production code MUST NOT depend on this
    /// surface — it exists so the renderer-capability matrix can
    /// assert "no character class regressed" without sniffing pixels
    /// off the swapchain. Doc-hidden to keep it out of the public
    /// rustdoc; still `pub` so integration tests under `tests/` can
    /// reach it.
    #[doc(hidden)]
    pub fn last_missing_tofu(&self) -> &[char] {
        &self.last_missing_chars
    }

    /// Every chrome character (tab titles, palette rows, query, footer, search, preedit) the
    /// previous presented frame drew as tofu or dropped because its run could not be shaped or
    /// its tile was not placed; whitespace is excluded. The chrome counterpart of
    /// [`Self::last_missing_tofu`], so fallback is complete only when both are empty.
    ///
    /// Test-only diagnostic, doc-hidden like `last_missing_tofu`.
    #[doc(hidden)]
    pub fn last_missing_chrome(&self) -> &[char] {
        &self.last_missing_chrome_chars
    }

    /// The perf-end completeness checkpoint: the distinct missing terminal and chrome characters the
    /// last presented `Full` frame certified, only while the retained scene and the current glyph
    /// atlas still match it; otherwise the reason it is unavailable.
    ///
    /// Test-only diagnostic, doc-hidden like `last_missing_tofu`.
    #[doc(hidden)]
    pub fn completeness_checkpoint(&self) -> crate::completeness::CompletenessCheckpoint {
        let retained_scene = self.last_frame_key.as_ref().map(FrameKey::scene);
        crate::completeness::read_checkpoint(
            self.completeness.as_ref(),
            retained_scene.as_ref(),
            &self.glyph_atlas_stamp(),
            (self.glyph_atlas.width(), self.glyph_atlas.height()),
        )
    }

    /// Grid `(cols, rows)` from raster surface and cell dimensions; logical padding is scaled before subtraction.
    pub fn cells(&self) -> (u16, u16) {
        let surf_w = self.config.width as f32;
        let surf_h = self.config.height as f32;
        let sf = self.scale_factor;
        let inner_w = (surf_w - self.padding_left * sf - self.padding_right * sf).max(self.cell_w);
        let inner_h = (surf_h - self.top_inset() - self.bottom_inset() - self.padding_bottom * sf)
            .max(self.cell_h);
        let cols = (inner_w / self.cell_w).floor() as u64;
        let rows = (inner_h / self.cell_h).floor() as u64;
        bounded_grid_size(cols, rows)
    }

    /// Cell width and height in raster pixels, matching rendered pane content rectangles.
    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_w, self.cell_h)
    }

    /// Test hook: evict the glyph atlas's coldest quarter at `cap` entries instead of the
    /// production maximum, so a native test can drive a real eviction in the same assembly as a
    /// growth. Passing `sonicterm_text::glyph_atlas::MAX_ATLAS_ENTRIES` restores production.
    #[doc(hidden)]
    pub fn __set_glyph_atlas_entry_cap(&mut self, cap: usize) {
        self.glyph_atlas.__set_entry_cap_for_test(cap);
    }

    /// Test hook: change the glyph atlas identity during the next assembly, so that frame returns
    /// `AtlasRetry` and presents nothing.
    #[doc(hidden)]
    pub fn __change_glyph_atlas_during_next_assembly(&mut self) {
        self.fault_atlas_change_during_assembly = true;
    }

    /// Test hook: run `hook` at the start of every presentation, after the frame's source was
    /// released; when it returns true the device is stopped there, as a loss during present would.
    #[doc(hidden)]
    pub fn __set_present_hook(&mut self, hook: Option<Box<dyn FnMut() -> bool + Send>>) {
        self.present_hook = hook;
    }

    /// Collect frame statistics from now on. The App calls this once, before the renderer draws.
    pub fn set_frame_counting(&mut self, counting: bool) {
        self.frame_sink = counting.then(crate::frame_stats::FrameStatsSink::default);
    }

    /// Settle this renderer's statistics for a final read: count glyph atlas growths not yet
    /// counted and abandon a pending growth episode. The App calls it before it copies a retiring
    /// or exiting window's statistics; it is idempotent, and `Drop` repeats it as a fallback that
    /// then adds nothing.
    pub fn finalize_frame_stats(&mut self) {
        self.finalize_growth_episodes();
    }

    /// The cumulative frame statistics; zero while the renderer does not count.
    #[must_use]
    pub fn frame_stats(&self) -> crate::frame_stats::FrameStats {
        self.frame_sink.as_ref().map_or(
            crate::frame_stats::FrameStats::ZERO,
            crate::frame_stats::FrameStatsSink::snapshot,
        )
    }

    /// Ask the native window to redraw, counting the request when this renderer counts. Every
    /// renderer-owned request goes through here; scheduling is exactly `request_redraw`.
    pub(crate) fn request_window_redraw(&self) {
        if let Some(sink) = &self.frame_sink {
            // this renderer counts, its own native request is recorded.
            sink.note_native_request();
        }
        self.window.request_redraw();
    }

    /// Number of frames that completed a native presentation successfully.
    ///
    /// Skipped, occluded, outdated, lost, and failed frames do not advance it.
    #[must_use]
    pub fn successful_frame_count(&self) -> u64 {
        self.successful_frame_count
    }

    /// Frames handed to a native presenter, counted at the present call.
    ///
    /// Unlike [`Self::successful_frame_count`], it also counts a presentation
    /// that stopped the device, so tests can prove a failed frame never reached
    /// the presenter.
    #[doc(hidden)]
    #[must_use]
    pub fn present_call_count(&self) -> u64 {
        self.present_calls
    }

    /// Stop this renderer's device just before its next cached Windows CPU
    /// reblit, after `render`'s entry check has passed.
    ///
    /// Test seam for the one device checkpoint no production GPU call reaches:
    /// an unchanged frame issues no GPU work between the entry check and the
    /// reblit, so only another thread could stop the device there.
    #[cfg(target_os = "windows")]
    #[doc(hidden)]
    pub fn __stop_device_before_cached_present(&mut self) {
        self.fault_stop_before_cached_present = true;
    }

    /// Clear retained frame identity in memory, forcing the next real frame to assemble and draw in full.
    pub fn invalidate_retained_frame(&mut self) {
        self.last_frame_key = None;
    }

    /// Force one typed backend-occlusion result on the next real wgpu frame, at the acquire step,
    /// without switching macOS Spaces or covering a window. The Windows software presenter has no
    /// surface acquire, so on that presenter the armed fault waits for a wgpu frame.
    #[doc(hidden)]
    pub fn __occlude_next_surface_acquire(&mut self) {
        self.fault_surface_occluded = true;
        self.invalidate_retained_frame();
    }

    /// The containment state of this renderer's wgpu device, shared by every
    /// renderer built from the same shared context.
    #[must_use]
    pub fn device_error_state(&self) -> &Arc<DeviceErrorState> {
        &self.device_errors
    }

    /// A point-in-time copy of this renderer's device containment state.
    #[must_use]
    pub fn device_error_snapshot(&self) -> DeviceErrorSnapshot {
        self.device_errors.snapshot()
    }

    /// Whether this renderer's device still accepts GPU work.
    #[must_use]
    pub fn device_accepts_gpu_work(&self) -> bool {
        self.device_errors.accepts_gpu_work()
    }

    /// Take the device's one-time stopped-frame report without assembling or presenting a frame.
    pub fn take_stopped_render_outcome(&mut self) -> Option<PresentOutcome> {
        // The App's stopped path refuses rendering before any assembly, so a pending growth
        // episode ends here, even when the stop was already reported.
        self.finalize_growth_episodes_if_device_stopped();
        if self.device_errors.accepts_gpu_work() || self.device_stop_reported {
            // When: `device_errors` accepts work or `device_stop_reported` is set, no stop report remains.
            return None;
        }
        Some(self.rendering_unavailable())
    }

    /// Process-unique identity of this renderer's device. Renderers that share
    /// a device report the same generation.
    #[must_use]
    pub fn device_generation(&self) -> u64 {
        self.device_errors.generation()
    }

    /// Stamp of everything outside the field text that moves field pixels:
    /// surface size, scale, font, cell metrics, panel padding, device, and device state.
    fn field_environment(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&self.config.width, &mut hasher);
        std::hash::Hash::hash(&self.config.height, &mut hasher);
        std::hash::Hash::hash(&self.scale_factor.to_bits(), &mut hasher);
        std::hash::Hash::hash(&self.font_size.to_bits(), &mut hasher);
        std::hash::Hash::hash(&self.font_family, &mut hasher);
        std::hash::Hash::hash(&self.font_weight_scale.to_bits(), &mut hasher);
        std::hash::Hash::hash(&self.cell_w.to_bits(), &mut hasher);
        std::hash::Hash::hash(&self.panel_padding.to_bits(), &mut hasher);
        std::hash::Hash::hash(&self.device_generation(), &mut hasher);
        // A stopped device keeps its generation until recovery, but nothing it presented is current.
        std::hash::Hash::hash(&self.device_errors.state(), &mut hasher);
        std::hash::Hasher::finish(&hasher)
    }

    /// Map a pointer (physical px) onto the presented command-palette query.
    ///
    /// `preedit` must be the IME preedit the palette was drawn with. Returns
    /// [`FieldHit::Stale`] when the presented frame shows other text, font,
    /// scale, or surface — request a redraw and retry — and never estimates.
    #[must_use]
    pub fn palette_field_hit(
        &self,
        palette: &CommandPalette,
        preedit: &str,
        point: (f32, f32),
        mode: FieldHitMode,
    ) -> FieldHit {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        let Some(text) = FieldText::palette(palette, preedit) else {
            // When: the palette is closed or shows the colour picker, there is no query to hit.
            return FieldHit::Outside;
        };
        field_hit(
            self.presented_fields.palette.as_ref(),
            &text,
            self.field_environment(),
            self.font_stack.as_ref(),
            point,
            mode,
        )
    }

    /// Map a pointer (physical px) onto the presented search query.
    ///
    /// Offsets are relative to `search.query`; the prompt and counter clamp to its edges.
    #[must_use]
    pub fn search_field_hit(
        &self,
        search: &SearchState,
        preedit: &str,
        point: (f32, f32),
        mode: FieldHitMode,
    ) -> FieldHit {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        field_hit(
            self.presented_fields.search.as_ref(),
            &FieldText::search(search, preedit),
            self.field_environment(),
            self.font_stack.as_ref(),
            point,
            mode,
        )
    }

    /// Presented caret of the palette query (physical px) when the screen shows
    /// exactly this query, caret, selection, and preedit; `None` means redraw first.
    #[must_use]
    pub fn palette_field_caret_rect(
        &self,
        palette: &CommandPalette,
        preedit: &str,
    ) -> Option<FieldRect> {
        let text = FieldText::palette(palette, preedit)?;
        field_caret_rect(self.presented_fields.palette.as_ref(), &text, self.field_environment())
    }

    /// Presented caret of the search query (physical px), or `None` until the
    /// screen shows exactly this query, caret, selection, and preedit.
    #[must_use]
    pub fn search_field_caret_rect(
        &self,
        search: &SearchState,
        preedit: &str,
    ) -> Option<FieldRect> {
        field_caret_rect(
            self.presented_fields.search.as_ref(),
            &FieldText::search(search, preedit),
            self.field_environment(),
        )
    }

    /// Install the callback that wakes the app after this renderer's device
    /// changes state. Renderers that share a device keep the first waker.
    pub fn set_device_state_waker(&self, waker: DeviceStateWaker) -> bool {
        self.device_errors.set_waker(waker)
    }

    /// Current font family in effect. Test-only inspector for the
    /// live-reload path; production code reads font fields directly.
    #[doc(hidden)]
    pub fn font_family(&self) -> &str {
        &self.font_family
    }

    /// Current font size in px.
    #[doc(hidden)]
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Measure overlay text in raster pixels with the renderer's active font
    /// stack, falling back conservatively when shaping is unavailable.
    pub fn measure_overlay_text_width(&self, text: &str, font_size: f32) -> f32 {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        let estimate = estimate_badge_text_width(text, font_size);
        conservative_badge_text_width(
            estimate,
            self.font_stack.as_ref().and_then(|stack| {
                crate::frame_stats::shape_request(|| stack.measure_text_width_for_frame(text)).ok()
            }),
        )
    }

    /// Lay out notification text with the same native shaping used to paint its glyphs.
    pub fn notification_layout(
        &self,
        message: &str,
        window: (f32, f32),
        row: u8,
    ) -> sonicterm_render_model::boundary::ui::overlays::NotificationTextLayout {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        let font_size = self.raster_px(self.font_size.max(1.0));
        NotificationBubbleLayout::compute_text(
            window.0,
            window.1,
            message,
            font_size,
            row,
            self.scale_factor,
            |text| {
                self.font_stack
                    .as_ref()
                    .and_then(|stack| {
                        crate::frame_stats::shape_request(|| {
                            stack.measure_text_width_for_frame(text)
                        })
                        .ok()
                    })
                    .unwrap_or_else(|| estimate_badge_text_width(text, font_size))
            },
        )
    }

    /// Return current allocator totals when the selected backend exposes them.
    ///
    /// `None` means allocator reporting is unavailable or the device has
    /// stopped accepting work; it does not represent an allocator with zero
    /// usage.
    #[must_use]
    pub fn allocator_snapshot(&self) -> Option<AllocatorSnapshot> {
        self.device_errors
            .gpu_work("allocator_snapshot", || {
                allocator_snapshot_from_report(self.device.generate_allocator_report())
            })
            .flatten()
    }

    /// True when wgpu fell back to a CPU/software rasterizer for this window.
    /// The app uses this to degrade frame pacing and per-frame animation in
    /// the no-GPU case.
    pub fn is_software_rendering(&self) -> bool {
        self.software_rendering
    }

    /// Whether the no-GPU degrade path is active for this window.
    ///
    /// Distinct from [`Self::is_software_rendering`]: that reports what the
    /// adapter is, this reports the resolved policy after
    /// `[appearance].software_render_mode` is applied, so `Force` degrades on
    /// real hardware and `Off` declines to degrade on a CPU rasterizer.
    pub fn is_software_render_degraded(&self) -> bool {
        self.software_render_degrade
    }

    /// Read one BGRA pixel out of the Windows software presentation buffer.
    ///
    /// Test-only inspector for the software path: it is the only way to assert
    /// what that path actually wrote without a GPU readback. Returns `None`
    /// when no software frame is allocated or the coordinates fall outside it.
    #[cfg(target_os = "windows")]
    #[doc(hidden)]
    pub fn __test_software_frame_pixel_bgra(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        self.software_frame.as_ref()?.pixel_bgra_at(x, y)
    }

    /// Update the resolved no-GPU degrade state after a config reload.
    /// A transition invalidates the retained frame and reconfigures the
    /// surface with the software-render present tweaks. While the device is
    /// stopped, only the flag and present settings are recorded; the surface
    /// and GPU atlas uploads are left alone.
    pub fn set_software_render_degrade(&mut self, degrade: bool) {
        if self.software_render_degrade == degrade {
            // When: `software_render_degrade == degrade`. The body below
            // reconfigures the surface and may drop both atlases.
            return;
        }
        let used_software_presenter = self.uses_windows_software_presenter();
        self.software_render_degrade = degrade;
        if degrade {
            // Fifo, opaque compositing, and one frame of latency trade
            // smoothness for the lowest CPU cost per presented frame.
            self.config.present_mode = PresentMode::Fifo;
            self.config.alpha_mode = CompositeAlphaMode::Opaque;
            self.config.desired_maximum_frame_latency = 1;
        } else {
            // When: leaving degrade. The modes captured at construction are
            // restored, so a backend without Mailbox does not acquire it here.
            self.config.present_mode = self.hardware_present_mode;
            self.config.alpha_mode = self.hardware_alpha_mode;
            self.config.desired_maximum_frame_latency = 2;
            #[cfg(target_os = "windows")]
            {
                self.software_frame = None;
            }
        }
        if let Some(_scope) = self.device_errors.enter_gpu_work("set_software_render_degrade") {
            // A stopped device records the flag only; the surface is not reconfigured.
            self.surface.configure(&self.device, &self.config);
        }
        let uses_software_presenter = self.uses_windows_software_presenter();
        if used_software_presenter != uses_software_presenter {
            // The software and GPU presenters size their atlas textures
            // differently, so cached UVs do not survive the transition.
            self.row_glyph_cache.invalidate_all();
            self.line_quad_cache.invalidate_all();
            if !uses_software_presenter {
                // GPU textures must grow from the placeholder dimensions the
                // software path left behind.
                self.reset_glyph_atlas_in_place("software_to_gpu");
                self.reset_image_atlas();
                self.glyph_atlas_retry_without_eviction = false;
            }
            self.rebuild_glyph_upload_if_needed();
            self.rebuild_image_upload_if_needed();
            // The GDI presenter never samples the frame texture, so it shrinks to 1x1; leaving GDI
            // grows it once. A stopped device keeps it, and recovery builds it from this flag.
            self.rebuild_frame_texture();
            let frame_texture = self.frame_texture_extent();
            tracing::debug!(
                target: "memory",
                renderer_role = self.render_timing_label,
                window_id = ?self.window.id(),
                software_presenter = uses_software_presenter,
                frame_texture_width = frame_texture.0,
                frame_texture_height = frame_texture.1,
                frame_texture_payload_bytes = frame_texture_payload_bytes(frame_texture),
                glyph_gpu_width = self.glyph_upload.width(),
                glyph_gpu_height = self.glyph_upload.height(),
                glyph_gpu_payload_bytes = self.glyph_upload.payload_bytes(),
                image_gpu_width = self.image_upload.width(),
                image_gpu_height = self.image_upload.height(),
                image_gpu_payload_bytes = self.image_upload.payload_bytes(),
                glyph_resident = self.glyph_atlas.len(),
                image_resident = self.image_atlas.len(),
                payload_estimate = true,
                "renderer software/GPU atlas transition"
            );
        }
        self.last_frame_key = None;
        self.log_subpixel_aa_policy();
        self.request_window_redraw();
    }

    fn uses_windows_software_presenter(&self) -> bool {
        cfg!(target_os = "windows") && self.software_render_degrade
    }

    /// Size the frame texture for the current presenter and surface, inside the device gate.
    ///
    /// A stopped device refuses; `commit_rebind` builds the texture later from the current flag.
    fn rebuild_frame_texture(&mut self) {
        let Some(_scope) = self.device_errors.enter_gpu_work("frame_texture.rebuild") else {
            // When: enter_gpu_work refuses, the stopped device keeps its texture until recovery.
            return;
        };
        let (frame_texture, frame_view) = build_frame_texture(
            &self.device,
            self.uses_windows_software_presenter(),
            self.config.width,
            self.config.height,
            self.config.format,
            self.retained_frame_readback,
        );
        self.frame_texture = frame_texture;
        self.frame_view = frame_view;
        self.frame_texture_trimmed = false;
        self.frame_texture_installs += 1;
    }

    /// Restore a trimmed frame texture to the surface size before a GPU present; returns false
    /// when the device refused the rebuild, so the caller presents nothing and retries later.
    pub(crate) fn ensure_frame_texture(&mut self) -> bool {
        if !self.frame_texture_trimmed || self.uses_windows_software_presenter() {
            // When: frame_texture_trimmed is clear or uses_windows_software_presenter, the texture already fits.
            return true;
        }
        self.rebuild_frame_texture();
        !self.frame_texture_trimmed
    }

    /// Release what a covered window does not need until it is shown again, inside the device
    /// gate. A refused admission changes nothing; once admitted, the releases are not rolled back.
    ///
    /// The glyph atlas, its GPU texture, the retry and eviction state, the software frame and the
    /// preedit cache are kept. The next frame is a full first frame.
    pub fn trim_for_occlusion(&mut self) -> TrimReport {
        let Some(_scope) = self.device_errors.enter_gpu_work("trim") else {
            // When: enter_gpu_work refuses, the stopped device keeps every resource untouched.
            return TrimReport::refused();
        };
        let frame_texture_before = self.frame_texture_extent();
        let present_buffer_bytes_before = self.present_pipeline.present_buffer_bytes();
        let mut released_bytes = 0_u64;
        if !self.uses_windows_software_presenter() {
            // The GPU presenter's texture is surface-sized; the software presenter's is already 1x1.
            let (frame_texture, frame_view) = build_frame_texture(
                &self.device,
                false,
                1,
                1,
                self.config.format,
                self.retained_frame_readback,
            );
            self.frame_texture = frame_texture;
            self.frame_view = frame_view;
            self.frame_texture_trimmed = true;
            released_bytes += frame_texture_payload_bytes(frame_texture_before)
                .saturating_sub(frame_texture_payload_bytes((1, 1)));
        }
        released_bytes += self.present_pipeline.reset_to_initial(&self.device);
        self.row_glyph_cache.release_all();
        self.line_quad_cache.release_all();
        self.row_ink.release_all();
        self.glyph_upload.release_scratch();
        self.image_upload.release_scratch();
        self.frame_scratch.release_held();
        self.chrome_caches.release_runs();
        self.release_image_atlas_for_trim();
        self.last_frame_key = None;
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            frame_texture_width = frame_texture_before.0,
            frame_texture_height = frame_texture_before.1,
            present_buffer_bytes_before,
            gpu_released_requested_bytes = released_bytes,
            reason = "occlusion_trim",
            "covered window trimmed"
        );
        TrimReport {
            refused: false,
            frame_texture_before,
            present_buffer_bytes_before,
            gpu_released_requested_bytes: released_bytes,
        }
    }

    /// Frame textures installed since construction by a rebuild or a recovery commit, so a native
    /// test can tell whether a present rebuilt the texture.
    #[doc(hidden)]
    #[must_use]
    pub fn __frame_texture_rebuilds(&self) -> u64 {
        self.frame_texture_installs
    }

    /// Whether a covered-window trim left the frame texture at 1x1 awaiting the next present.
    #[doc(hidden)]
    #[must_use]
    pub fn __frame_texture_trimmed(&self) -> bool {
        self.frame_texture_trimmed
    }

    /// The glyph atlas's pending compaction retry and its configured eviction permission, so a
    /// native test can check that a trim leaves both alone.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_glyph_atlas_retry_state(&self) -> (bool, bool) {
        (self.glyph_atlas_retry_without_eviction, self.glyph_atlas.eviction_enabled())
    }

    /// Test hook: append one glyph drawing `rect_px` (`x, y, w, h` in surface pixels) in `color`
    /// to every later frame's terminal glyphs, before the cursor recolors, with the atlas
    /// coordinates of the frame's first terminal glyph; `None` removes it. It is not part of the
    /// frame key: a test changes it together with grid dirt, as a real glyph change would.
    #[doc(hidden)]
    pub fn __inject_test_glyph(&mut self, glyph: Option<((f32, f32, f32, f32), [f32; 4])>) {
        self.injected_test_glyph = glyph;
    }

    /// Test hook: attach a glyph `(x, y, w, h)` in surface pixels and its color to the row at viewport
    /// slot `slot` of pane `pane_id`, drawn after that row's own glyphs whenever the row is emitted,
    /// so its ink joins the row's record; `None` removes it. It is not part of the frame key: a test
    /// changes it together with that row's dirt, as a real glyph change would.
    #[doc(hidden)]
    pub fn __inject_row_glyph(&mut self, glyph: Option<InjectedRowGlyph>) {
        self.injected_row_glyph = glyph;
    }

    /// Test hook: the committed and staged glyph-row keys of `slot` of `pane_id` (0 for none),
    /// or `None` while the pane is untracked by the row glyph cache.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_glyph_slot_keys(&self, pane_id: u64, slot: u16) -> Option<(u64, u64)> {
        let committed = self.row_glyph_cache.committed_slot(pane_id, slot)?;
        Some((committed, self.row_glyph_cache.staged_slot(pane_id, slot)?))
    }

    /// Test hook: the committed ink record of viewport slot `slot` of pane `pane_id`, as the last
    /// presented frame left it; `None` when no record is kept.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_row_ink(&self, pane_id: u64, slot: u16) -> Option<PixelRect> {
        self.row_ink.committed_rect(pane_id, slot)
    }

    /// Test hook: the next wgpu presentation reports `reason` at the acquire point instead of
    /// asking the surface, after its plan was assembled, and takes the production retry branch.
    /// Arming it keeps the frame key, so the failing frame plans against the last presented one.
    #[doc(hidden)]
    pub fn __fail_next_surface_acquire(&mut self, reason: SurfaceRetryReason) {
        self.fault_surface_acquire = Some(reason);
    }

    /// Test hook: the next presented frame's submission fails validation after its retained draw,
    /// as `GpuFaultKind::FrameValidation` makes it, but arming it keeps the frame key, so the failing
    /// frame plans against the last presented one; the failure path clears the key itself.
    #[doc(hidden)]
    pub fn __fail_next_frame_submission(&mut self) {
        self.fault_frame_probe = self
            .device_errors
            .gpu_work("fault.frame_probe", || create_frame_fault_probe(&self.device));
    }

    /// Test hook: the next assembly fails with `Err` after its glyph rows are staged, reaching
    /// the assembly error arm of `render_releasing` rather than an ordinary failed outcome.
    #[doc(hidden)]
    pub fn __fail_next_assembly(&mut self) {
        self.fault_assembly_error = true;
    }

    /// Test hook: the next presentation fails with `Err` from the presenter call itself, rather
    /// than the ordinary failed outcome `__fail_next_frame_submission` produces.
    #[doc(hidden)]
    pub fn __fail_next_present(&mut self) {
        self.fault_present_error = true;
    }

    /// Test hook: record the slots each later assembly pass emits, per drawn pane, for
    /// `__take_emitted_rows`. Production never calls it, so no frame records anything.
    #[doc(hidden)]
    pub fn __enable_emitted_rows(&mut self) {
        self.emitted_rows_probe = Some(Vec::new());
    }

    /// Test hook: the emitted slots of every drawn pane in the last assembly pass, cleared by
    /// this read; empty when the inspector is off or no pass ran since the last read.
    #[doc(hidden)]
    #[must_use]
    pub fn __take_emitted_rows(&mut self) -> Vec<(u64, Vec<u16>)> {
        self.emitted_rows_probe.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Test hook: keep each later presented frame's damage for `__take_presented_damage`.
    /// Production never calls it, so a presented frame builds and keeps no snapshot.
    #[doc(hidden)]
    pub fn __enable_presented_damage(&mut self) {
        self.presented_damage.enable();
    }

    /// Test hook: the damage of the last presented frame, cleared by this read, so a frame that
    /// presents nothing reads `None` instead of an older frame's damage. `None` until enabled.
    #[doc(hidden)]
    pub fn __take_presented_damage(&mut self) -> Option<PresentedDamage> {
        self.presented_damage.take()
    }

    /// Test hook: whether a retained frame key is kept, so the next changed frame is not a
    /// first frame.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_has_frame_key(&self) -> bool {
        self.last_frame_key.is_some()
    }

    /// Append the test glyph seam's instance, if one is set, after the terminal rows.
    fn push_injected_test_glyph(&mut self, glyphs: &mut Vec<GlyphInstance>, sw: f32, sh: f32) {
        let Some((rect_px, color)) = self.injected_test_glyph else {
            // When: injected_test_glyph is None, as in production, nothing is appended.
            return;
        };
        glyphs.extend(crate::cursor::seam_glyph(&mut self.glyph_atlas, rect_px, color, sw, sh));
    }

    /// Test hook: recreate the retained frame texture copyable (`COPY_SRC`), now and on every later
    /// recreation, so [`Self::__copy_retained_frame`] can read it back. Production never calls it,
    /// so production frame textures keep their usage. The next frame draws in full.
    #[doc(hidden)]
    pub fn __enable_retained_frame_readback(&mut self) {
        self.retained_frame_readback = true;
        self.rebuild_frame_texture();
        self.last_frame_key = None;
    }

    /// Test hook: copy the retained frame into a new readback buffer, through the device gate, and
    /// return what the test needs to map it. `None` until readback is enabled, or once the device
    /// stopped. The test maps and polls; this never does.
    #[doc(hidden)]
    pub fn __copy_retained_frame(&mut self) -> Option<RetainedFrameReadback> {
        if !self.retained_frame_readback {
            // When: retained_frame_readback is false the texture has no COPY_SRC to copy from.
            return None;
        }
        let Some(_scope) = self.device_errors.enter_gpu_work("frame_texture.readback") else {
            // When: enter_gpu_work refuses, a stopped device copies nothing.
            return None;
        };
        let (width_px, height_px) = (self.frame_texture.width(), self.frame_texture.height());
        let padded_row_bytes = padded_readback_row_bytes(width_px);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sonic-retained-frame-readback"),
            size: u64::from(padded_row_bytes) * u64::from(height_px),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sonic-retained-frame-readback"),
        });
        encoder.copy_texture_to_buffer(
            self.frame_texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(height_px),
                },
            },
            wgpu::Extent3d { width: width_px, height: height_px, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        Some(RetainedFrameReadback {
            device: self.device.clone(),
            buffer,
            width_px,
            height_px,
            padded_row_bytes,
        })
    }

    /// Test hook: resident glyph tiles that hold colour pixels, so a native test can tell a colour
    /// face resolved before it compares colour tiles.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_resident_color_tiles(&self) -> usize {
        self.glyph_atlas
            .resident_tile_keys()
            .into_iter()
            .filter(|key| self.glyph_atlas.get(*key).is_some_and(|info| info.is_color))
            .count()
    }

    /// Test hook: the alpha census of `character`'s resident colour tile, read from the CPU atlas,
    /// so a native test can assert the colour glyph it selected holds translucent edge pixels.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_colour_tile_alpha(
        &self,
        character: char,
    ) -> Option<crate::glyph_working_set::ColourTileAlpha> {
        crate::glyph_working_set::colour_tile_alpha(&self.glyph_atlas, character)
    }

    /// The retained frame texture's actual extent: 1x1 under the Windows software presenter, else
    /// the configured surface size. GPU memory, so it is not part of [`Self::retained_amounts`].
    #[must_use]
    pub fn frame_texture_extent(&self) -> (u32, u32) {
        (self.frame_texture.width(), self.frame_texture.height())
    }

    /// Current OS display scale factor (physical px per logical px). Exposed so
    /// the app layer can scale window-event geometry (e.g. the search-bar IME
    /// caret rect) to match the renderer's physical-px coordinate space.
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Number of glyph tiles currently resident in the rasterizer atlas.
    /// Test-only; the atlas is cleared and rebuilt by [`Self::set_font`].
    #[doc(hidden)]
    pub fn glyph_atlas_len(&self) -> usize {
        self.glyph_atlas.len()
    }

    /// Test hook: in-place glyph atlas resets since construction.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_glyph_atlas_resets(&self) -> u64 {
        self.glyph_atlas_resets
    }

    /// Test hook: glyph atlas lookups that missed, each one a rasterization; a reset zeroes it.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_glyph_atlas_misses(&self) -> u64 {
        self.glyph_atlas.misses()
    }

    /// Test hook: the glyph atlas's CPU size and its GPU upload's size, so a native test can check
    /// that the upload follows a growth (or stays the 1x1 placeholder under the software presenter).
    #[doc(hidden)]
    #[must_use]
    pub fn __test_glyph_atlas_dimensions(&self) -> ((u32, u32), (u32, u32)) {
        (
            (self.glyph_atlas.width(), self.glyph_atlas.height()),
            (self.glyph_upload.width(), self.glyph_upload.height()),
        )
    }

    /// Test hook: the keys of every glyph tile resident in the atlas, sentinels excluded, so a
    /// native test can compare them with the working-set helper's key set.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_resident_tile_keys(
        &self,
    ) -> std::collections::HashSet<sonicterm_types::GlyphKey> {
        self.glyph_atlas.resident_tile_keys()
    }

    /// Test seam: every resident glyph tile by identity across font configurations, with its
    /// raster size, resolved through the stack that drew its raster variant; the second value
    /// lists the resident keys that resolved to no identity.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_resident_tile_identities(
        &self,
    ) -> (
        std::collections::HashMap<crate::glyph_working_set::TileIdentity, [u32; 2]>,
        Vec<sonicterm_types::GlyphKey>,
    ) {
        crate::glyph_working_set::resident_tile_identities(&self.glyph_atlas, |variant| {
            match variant {
                GlyphRasterVariant::Normal => self.font_stack.as_ref(),
                GlyphRasterVariant::TabTitle => self.tab_title_font.stack(),
                GlyphRasterVariant::PaletteFooter => self.palette_footer_font_stack.as_ref(),
            }
        })
    }

    /// Apply a new font family / size / line-height multiplier without
    /// reconstructing the renderer.
    ///
    /// The shelf-packed glyph atlas is cleared because existing tiles
    /// are sized for the old metrics — reusing them would render at the
    /// wrong pixel scale. The frame-key cache is also invalidated so
    /// Set (or clear) the translucent drag-chip overlay drawn on top
    /// of the frame. Called by the app on every CursorMoved during a
    /// held-tab drag, and with `None` on release.
    pub fn set_drag_chip(&mut self, chip: Option<DragChipOverlay>) {
        self.drag_chip = chip;
        // Bust the frame-key cache so a new chip position is actually
        // drawn — otherwise the no-change fast path would short-circuit.
        self.last_frame_key = None;
    }

    /// Active drag chip overlay (if any). Read-only accessor used by
    /// tests and the app event loop to inspect the live chip state.
    pub fn drag_chip(&self) -> Option<&DragChipOverlay> {
        self.drag_chip.as_ref()
    }

    /// Diagnostic — visual rect of the most recently rendered drag
    /// chip, or `None` if no chip was drawn. Test-only.
    #[doc(hidden)]
    pub fn last_drag_chip_visual(&self) -> Option<DragChipVisual> {
        self.drag_chip_visual
    }

    /// Record where the pointer is, in the pixel space pointer events use
    /// (origin top-left), or `None` when it leaves the window. `tabs` is this
    /// window's tab bar, the one the next frame draws.
    ///
    /// Returns `true` only when the hovered tab changes, because the hovered
    /// tab is the only hover fact a frame draws and it is part of the frame
    /// key. A move within one tab, across empty bar space or over the terminal
    /// area returns `false` and leaves the frame key alone.
    pub fn set_hover_cursor(&mut self, pos: Option<(f32, f32)>, tabs: &TabBar) -> bool {
        if self.hover_cursor == pos {
            // When: `hover_cursor == pos`, the pointer did not move, so the hovered tab cannot change.
            return false;
        }
        let previous = self.hover_cursor;
        self.hover_cursor = pos;
        self.hovered_tab_index(tabs, previous) != self.hovered_tab_index(tabs, pos)
    }

    /// Where this window's tab bar sits for the hover hit test.
    fn tab_bar_hover_geometry(&self) -> TabBarHoverGeometry {
        TabBarHoverGeometry {
            width_px: self.config.width as f32,
            bar_height_px: self.tab_bar_logical_height(),
            top_offset_px: self.tab_bar_y_offset(),
            visible: self.tab_bar_visible,
        }
    }

    /// The index of the tab under `cursor` in this window's bar, or
    /// `u32::MAX` for none. `render` and `set_hover_cursor` both resolve the
    /// hovered tab here, so the tab drawn hovered and the tab that decided a
    /// redraw are always the same.
    fn hovered_tab_index(&self, tabs: &TabBar, cursor: Option<(f32, f32)>) -> u32 {
        hovered_tab_at(tabs, self.tab_bar_hover_geometry(), cursor)
    }

    /// The vertical band the visible tab bar occupies, `(top, bottom)`, in the
    /// pixel space pointer positions use, or `None` while the bar is hidden.
    /// Each window tests its own recorded pointer against it to decide whether
    /// its tab widths hold.
    #[must_use]
    pub fn tab_bar_band(&self) -> Option<(f32, f32)> {
        let top = self.tab_bar_y_offset();
        self.tab_bar_visible.then_some((top, top + self.tab_bar_logical_height()))
    }

    /// Measure changed tab titles with the tab font and store their widths on
    /// `tabs`, where drawing, hover, hit-testing, drag and tear-out slots,
    /// native drag snapshots and the overflow selector all read them.
    ///
    /// Call it right before [`Self::render`] with the same tabs. Only a tab
    /// whose title, badge, privilege marker, font or scale changed is shaped.
    /// While `hold` is set, a changed title, badge or marker is measured but
    /// laid out later, so no tab moves under the pointer; a font, scale or
    /// width-limit change lays the bar out at once. It measures on the CPU,
    /// through the tab-title font that `set_font` and the scale rebuild update.
    pub fn measure_tab_widths(
        &self,
        fonts: &FrameFonts,
        tabs: &mut TabBar,
        process_privileged: bool,
        hold: bool,
        now: Instant,
    ) -> ContentWidthRefresh {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        self.debug_assert_prepared(fonts);
        self.tab_title_font.measure(tabs, process_privileged, hold, now)
    }

    /// Deprecated close-button color override. The button is no longer
    /// drawn, but accepting the setting keeps older configs harmless.
    pub fn set_tab_close_override(&mut self, color: Option<&str>) -> bool {
        let parsed = color.map(|c| hex_to_premultiplied_rgba(c, 1.0));
        if self.tab_close_override != parsed {
            self.tab_close_override = parsed;
            self.last_frame_key = None;
            true
        } else {
            // When: `self.tab_close_override == parsed` — the override is
            // unchanged, so `false` tells the caller nothing needs redrawing.
            false
        }
    }

    /// the next `render()` call cannot short-circuit through the
    /// fast-path against a now-stale frame.
    pub fn set_font(&mut self, family: &str, size: f32, line_height_mult: f32, weight_scale: f32) {
        let weight_scale = effective_font_weight_scale(weight_scale);
        let dpi = (72.0 * self.scale_factor).round().max(1.0) as usize;
        let new_stacks = renderer_font_stacks(family, size, dpi, weight_scale, &self.font_dirs);
        self.adopt_font_stacks(family, size, line_height_mult, weight_scale, new_stacks);
    }

    /// Test seam: adopt `stack` as the body font under the name `family`, with tab-title and
    /// footer views at their usual sizes, through the same path `set_font` takes. Lets a test
    /// drive a renderer with faces whose coverage it controls.
    #[doc(hidden)]
    pub fn __test_adopt_body_font_stack(
        &mut self,
        family: &str,
        stack: sonicterm_engine::FontStack,
    ) {
        let size = self.font_size;
        let stacks = renderer_font_views(Some(stack), size);
        self.adopt_font_stacks(family, size, self.line_height_mult, self.font_weight_scale, stacks);
    }

    /// Install `new_stacks` for `family` at `size`: recompute cell metrics, swap the stacks (the
    /// body through the waker-attaching seam), and drop every cache built with the old faces.
    fn adopt_font_stacks(
        &mut self,
        family: &str,
        size: f32,
        line_height_mult: f32,
        weight_scale: f32,
        new_stacks: RendererFontStacks,
    ) {
        let (new_cell_w, natural_cell_h) =
            match new_stacks.body.as_ref().and_then(|s| s.cell_metrics_raster_px().ok()) {
                Some(m) => (m.cell_w as f32, m.cell_h as f32),
                None => (self.raster_px(size * 0.6), self.raster_px(size * 1.2)),
            };
        let new_line_h = natural_cell_h * line_height_mult.max(0.0).max(0.01);
        let no_change = self.font_family == family
            && (self.font_size - size).abs() < f32::EPSILON
            && (self.line_height - new_line_h).abs() < f32::EPSILON
            && (self.font_weight_scale - weight_scale).abs() < f32::EPSILON
            && (self.cell_w - new_cell_w).abs() < f32::EPSILON
            && (self.cell_h - new_line_h).abs() < f32::EPSILON;
        if no_change {
            // When: `no_change` — family, size, weight, and both cell metrics
            // all match. The body below drops the atlas and both row caches.
            return;
        }
        self.font_family = family.to_string();
        self.font_size = size;
        self.line_height = new_line_h;
        self.font_weight_scale = weight_scale;
        self.line_height_mult = line_height_mult.max(0.0).max(0.01);
        // A new body stack has a new notice; it gets the same wake so its completions redraw.
        frame_fonts::install_body_stack(
            &mut self.font_stack,
            new_stacks.body,
            self.fallback_waker.as_ref(),
        );
        self.tab_title_font.set_font(family, size, weight_scale, new_stacks.tab_title);
        self.palette_footer_font_stack = new_stacks.palette_footer;
        self.cell_w = new_cell_w;
        self.cell_h = new_line_h;
        self.reset_glyph_atlas_in_place("font_change");
        self.glyph_atlas_retry_without_eviction = false;
        // SwashRasterizer prebake gone. Atlas is now lazily
        // filled by the wezterm rasterizer on the next render.
        self.row_glyph_cache.invalidate_all();
        // Kept titles and chrome runs hold the old faces' glyph ids, whatever their keys say.
        self.chrome_caches.clear_runs();
        self.line_quad_cache.invalidate_all();
        self.last_frame_key = None;
        self.last_pane_layout.clear();
        // Field boundaries were measured with the previous font.
        self.presented_fields.clear();
        tracing::info!(
            "renderer.set_font: family={family} size={size} line_h={} cell={:.2}x{:.2}",
            self.line_height,
            self.cell_w,
            self.cell_h
        );
    }

    /// Apply changed DPI by rebuilding glyph state and raster cell metrics; logical chrome sizes scale with it.
    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        if !scale_factor_rebuild_required(self.scale_factor, scale_factor) {
            // When: `!scale_factor_rebuild_required` — the DPI is unchanged
            // within epsilon, and `rebuild_for_sf` re-rasterizes every glyph.
            return;
        }
        self.rebuild_for_sf(scale_factor);
    }

    /// Force-rebuild atlas + GPU upload for the given DPI multiplier,
    /// regardless of whether the cached value matches. Used by the
    /// tear-out path where `GpuRenderer::new` may have latched the
    /// wrong scale (window not yet placed on a display, so the OS
    /// reports 1.0); once the OS places the new window on its real
    /// Retina display, we must re-rasterize glyphs at the correct
    /// physical em-size or the child window shows blurry tiles +
    /// atlas tofu instead of real text. See the bug report on
    /// torn-out windows rendering with wrong cell width and missing
    /// nerd-font glyphs.
    pub fn force_rebuild_for_scale(&mut self, sf: f32) {
        self.rebuild_for_sf(sf);
    }

    /// Single helper that owns the rasterizer-px target derived
    /// from `font_size * DPI`. Every callsite (grid + chrome) routes
    /// a logical font size through here to obtain the raster-px
    /// em-size the font stack rasterizes at.
    #[inline]
    fn raster_px(&self, font_size: f32) -> f32 {
        font_size * self.scale_factor
    }

    /// Scale a logical-px chrome constant into the renderer's physical/raster-px
    /// coordinate space. Chrome layout literals (badge/search-bar/palette sizes,
    /// paddings, radii, sub-cell thicknesses) are authored at scale-factor 1.0;
    /// route them through this so they track the display DPI like glyphs do.
    /// Window-anchored POSITIONS (edge margins, centering offsets) must NOT use
    /// this — they stay in window space. See.
    #[inline]
    fn chrome_px(&self, logical: f32) -> f32 {
        logical * self.scale_factor
    }

    fn rebuild_for_sf(&mut self, sf: f32) {
        let sf = sf.max(0.1);
        self.scale_factor = sf;
        // Reset glyph contents for the new DPI; atlas tiles are rasterized lazily on demand.
        self.reset_glyph_atlas_in_place("dpi_change");
        self.glyph_atlas_retry_without_eviction = false;
        // Update each stack's DPI before rerasterization; failed metric lookup preserves the prior cell measurement.
        let fs_dpi = (72.0 * sf).round() as usize;
        // Body, tab-title and footer stacks rescale in that order; the tab-title font also
        // records the scale, so the next width measurement shapes every title again.
        if let Some(stack) = self.font_stack.as_ref() {
            stack.change_scaling(stack.get_font_scale(), fs_dpi);
        }
        self.tab_title_font.set_scale_factor(sf, fs_dpi);
        if let Some(stack) = self.palette_footer_font_stack.as_ref() {
            stack.change_scaling(stack.get_font_scale(), fs_dpi);
        }
        if let Some(stack) = self.font_stack.as_ref() {
            if let Ok(m) = stack.cell_metrics_raster_px() {
                self.cell_w = m.cell_w as f32;
                let natural = m.cell_h as f32;
                self.cell_h = natural * self.line_height_mult.max(0.01);
                self.line_height = self.cell_h;
            }
        }
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        // Every stack rescaled in place, so kept titles and runs hold old-size glyphs.
        self.chrome_caches.clear_runs();
        // The GPU-side AtlasUpload owns a texture sized to the old atlas
        // dimensions and a bind group pointing at it. After replacing the
        // CPU `GlyphAtlas` with one of a different size, the next
        // `glyph_upload.sync(...)` would either write out-of-bounds or
        // sample tiles at stale UVs. Rebuild the upload so its texture +
        // bind group match the new atlas dimensions exactly.
        self.rebuild_glyph_upload_if_needed();
        self.last_frame_key = None;
        self.last_pane_layout.clear();
        // Field geometry was measured at the previous DPI.
        self.presented_fields.clear();
        self.request_window_redraw();
        tracing::info!(
            "renderer.rebuild_for_sf: sf={sf} atlas={}x{} raster_px={}",
            self.glyph_atlas.width(),
            self.glyph_atlas.height(),
            self.raster_px(self.font_size),
        );
    }

    /// Apply a new color theme without reconstructing the renderer.
    /// Recomputes every cached wgpu / glyphon color derived from the
    /// theme so the next frame reflects the swap.
    pub fn set_theme(&mut self, theme: &Theme) {
        self.set_theme_with_opacity(theme, self.bg_opacity);
    }

    /// Apply a new color theme and terminal background opacity.
    pub fn set_theme_with_opacity(&mut self, theme: &Theme, opacity: f32) {
        self.bg_opacity = opacity.clamp(0.0, 1.0);
        self.bg = hex_to_wgpu_with_alpha(theme.colors.background.0.as_str(), self.bg_opacity);
        self.fg_default = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        self.cursor_color = cursor_color_from_theme(theme);
        self.bg_rgba = hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
        self.cursor_text_color = cursor_text_color_from_theme(theme);
        self.selection_color = hex_to_premultiplied_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        self.tab_bar_bg = hex_to_premultiplied_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        self.tab_active_bg = hex_to_premultiplied_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        self.tab_inactive_bg =
            hex_to_premultiplied_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        self.tab_active_fg = hex_to_chrome_color(theme.colors.tab.active_fg.0.as_str());
        self.tab_inactive_fg = hex_to_chrome_color(theme.colors.tab.inactive_fg.0.as_str());
        self.tab_separator =
            hex_to_premultiplied_rgba(theme.colors.tab.inactive_fg.0.as_str(), 0.45);
        self.hyperlink_underline = hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 0.9);
        self.splitter_color = splitter_color_from_theme(theme);
        let tint_alpha = match theme.appearance {
            sonicterm_render_model::boundary::cfg::theme::Appearance::Dark => {
                // When: `Appearance::Dark` — dark needs more accent before
                // the hyperlink tint reads as tinted at all.
                0.14
            }
            sonicterm_render_model::boundary::cfg::theme::Appearance::Light => {
                // When: `Appearance::Light` — 0.14 reads as a highlighter
                // stripe over the text rather than a hint beneath it.
                0.10
            }
        };
        self.hyperlink_tint = hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), tint_alpha);
        self.search_highlight =
            hex_to_premultiplied_rgba(theme.colors.bright.yellow.0.as_str(), 0.35);
        self.search_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        self.search_bg = hex_to_premultiplied_rgba(theme.colors.tab.bar_bg.0.as_str(), 0.95);
        // Refresh the kept UI palette now, so the next frame with this theme derives nothing.
        let _palette = self.chrome_caches.palette.palette_for(theme);
        self.last_frame_key = None;
        self.style_rev = self.style_rev.wrapping_add(1);
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        self.log_subpixel_aa_policy();
        tracing::info!("renderer.set_theme: {}", theme.name);
    }

    /// Prepare this frame's fonts before width measurement and frame-key planning: when the body
    /// stack's fallback notice published a newer generation, invalidate every cached placeholder.
    pub fn begin_frame_fonts(&mut self) -> FrameFonts {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        let current = self.font_stack.as_ref().map_or((0, 0), |stack| {
            let notice = stack.fallback_notice();
            (notice.id(), notice.generation())
        });
        frame_fonts::prepare_and_owe(
            &mut self.applied_fonts,
            &mut self.unattributed_apply,
            current,
            frame_fonts::FontApplyTargets {
                row_glyph_cache: &mut self.row_glyph_cache,
                line_quad_cache: &mut self.line_quad_cache,
                style_rev: &mut self.style_rev,
                last_frame_key: &mut self.last_frame_key,
                glyph_atlas: &mut self.glyph_atlas,
                preedit_glyph_cache: &mut self.preedit_glyph_cache,
                fallback_epoch: self.tab_title_font.fallback_epoch_mut(),
                chrome_runs: &mut self.chrome_caches.runs,
            },
        )
    }

    /// Install the App's wake for fallback completions and attach it to the current body stack's
    /// notice. An owed completion is delivered once now; a claim already posted is left alone.
    pub fn set_font_fallback_waker(&mut self, waker: FontFallbackWaker) {
        self.fallback_waker = Some(waker);
        self.attach_fallback_waker();
    }

    /// Attach the stored wake to the body stack's notice. The tab-title and footer stacks are
    /// clones of the body configuration and share its notice, so one attachment covers all three.
    fn attach_fallback_waker(&self) {
        frame_fonts::attach_fallback_waker(self.font_stack.as_ref(), self.fallback_waker.as_ref());
    }

    /// Test seam: hold this renderer's fallback worker inside `hook` while it holds the
    /// pending-handle lock, so a test controls when a found face can merge and publish. The
    /// tab-title and footer stacks share the body configuration, so one hook covers all three.
    #[doc(hidden)]
    pub fn __test_set_fallback_append_hook(&self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        if let Some(stack) = self.font_stack.as_ref() {
            stack.set_fallback_append_hook_for_test(hook);
        }
    }

    /// The id of the fallback notice this renderer's body stack publishes to, if it has a stack.
    #[must_use]
    pub fn font_fallback_notice_id(&self) -> Option<u64> {
        self.font_stack.as_ref().map(|stack| stack.fallback_notice().id())
    }

    /// Handle a delivered fallback wake for `notice_id`: acknowledge it when it is this
    /// renderer's current notice, and return whether a frame is needed to apply its generation.
    /// An event for an older notice touches nothing and needs no frame.
    pub fn acknowledge_font_fallback(&mut self, notice_id: u64) -> bool {
        frame_fonts::acknowledge_fallback_wake(
            self.font_stack.as_ref(),
            self.applied_fonts,
            notice_id,
        )
    }

    /// A frame's measurement and drawing take the token their preparation returned; neither reads
    /// the generation, so they act on exactly what `begin_frame_fonts` applied.
    fn debug_assert_prepared(&self, fonts: &FrameFonts) {
        debug_assert_eq!(
            self.applied_fonts,
            Some((fonts.notice_id(), fonts.generation())),
            "a frame must take the token of this renderer's latest begin_frame_fonts"
        );
    }

    /// Test seam: keep (`true`, the default) or drop the frame scratch and the kept chrome titles
    /// and runs between frames, so a test can compare reuse against cold assembly.
    #[doc(hidden)]
    pub fn __set_frame_reuse(&mut self, reuse: bool) {
        self.frame_scratch.set_reuse(reuse);
        self.chrome_reuse = reuse;
        if !reuse {
            // Reuse is turned off, so kept titles and runs are dropped at once.
            self.chrome_caches.clear_runs();
        }
    }

    /// Test seam: how many times the UI palette was derived after construction.
    #[doc(hidden)]
    #[must_use]
    pub fn __palette_computes(&self) -> u64 {
        self.chrome_caches.palette.computes()
    }

    /// Invalidate row glyphs, line quads, kept chrome titles and runs, and the frame key, bumping
    /// `style_rev` so the next frame reshapes text.
    pub fn clear_shape_cache(&mut self) {
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        self.chrome_caches.clear_runs();
        self.style_rev = self.style_rev.wrapping_add(1);
        self.last_frame_key = None;
        tracing::info!(
            "renderer.clear_shape_cache (async fallback notifier) style_rev={}",
            self.style_rev
        );
    }

    /// Test/diagnostic peek at the renderer's monotonic style
    /// revision. The counter is opaque; tests only care that it
    /// *changes* on theme / `clear_shape_cache` calls.
    #[doc(hidden)]
    #[must_use]
    pub fn style_rev(&self) -> u64 {
        self.style_rev
    }

    /// Record a compatibility attachment without starting or changing font fallback work.
    pub fn set_async_loader(&mut self, _loader: ()) {
        self.async_loader = Some(());
    }

    /// Observe the compatibility attachment; `None` until the setter records `Some(())`.
    #[doc(hidden)]
    #[must_use]
    pub fn async_loader(&self) -> Option<&()> {
        self.async_loader.as_ref()
    }

    /// Translate physical-pixel `(px, py)` (as winit reports) into a
    /// `(row, col)` cell address inside the grid, or `None` if the point
    /// falls outside the grid (in the tab bar, padding, etc.).
    ///
    /// Winit physical pixels match renderer raster pixels; no DPI division is needed.
    ///
    /// pane-aware. After the first `render` call, this resolves
    /// the click against the per-pane layout captured in
    /// `last_pane_layout` and uses that pane's reconstructed
    /// `snapped_cell_x` cache to pick a column. This matters at
    /// fractional DPI (1.25/1.5/1.75) where naive `(x / cell_w).floor()`
    /// disagrees with the device-pixel-snapped edges the renderer
    /// actually drew on — off-by-one column near the right side of wide
    /// grids — and at split layouts where the right pane's column 0 is
    /// not at `padding_left`.
    ///
    /// Before the first render the layout snapshot is empty and we fall
    /// back to the legacy single-grid arithmetic; callers should not
    /// hit-test before rendering, but tests / early input events
    /// previously did and the legacy behaviour is preserved for them.
    pub fn pixel_to_cell(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        self.pixel_to_pane_cell(px, py).map(|(_, row, col)| (row, col))
    }

    /// Translate physical pixels into the exact rendered pane and cell.
    ///
    /// The pane identity and cell coordinates come from one layout snapshot, so
    /// split-pane clicks cannot mix app geometry with device-pixel-snapped edges.
    pub fn pixel_to_pane_cell(&self, px: f32, py: f32) -> Option<(u64, u16, u16)> {
        // Winit physical pixels already match the renderer's raster coordinates.
        // When the tab bar is pinned to the bottom of the window, clicks
        // inside the bar strip must NOT resolve to a phantom grid cell —
        // otherwise selection drags initiated in the bar would extend
        // the underlying grid selection. Reject anything below the
        // grid's content area. Padding is logical-px stored, so scale.
        let surf_h = self.config.height as f32;
        let sf = self.scale_factor;
        let content_bottom = surf_h - self.bottom_inset() - self.padding_bottom * sf;
        if py >= content_bottom {
            // When: `py >= content_bottom` — the point is in the tab-bar strip
            // or bottom padding; a phantom cell would extend grid selection.
            return None;
        }
        if py < self.top_inset() {
            // When: `py < self.top_inset()` — above the grid, in the titlebar
            // band or top padding, so no row corresponds to it.
            return None;
        }
        if self.last_pane_layout.is_empty() {
            // When: `last_pane_layout.is_empty()` — no render has run yet, so
            // legacy single-grid arithmetic (padding + cell_w) is used.
            let (cols, rows) = self.cells();
            let geometry = sonicterm_render_model::pane_content_geometry(
                PixelRect { x: 0, y: 0, w: self.config.width, h: self.config.height },
                [
                    self.padding_left_px(),
                    self.padding_right_px(),
                    self.top_inset(),
                    self.bottom_inset() + self.padding_bottom_px(),
                ],
                self.cell_h,
                rows,
            );
            let x = px - geometry.grid.x;
            let y = py - geometry.grid.y;
            if x < 0.0 || y < 0.0 {
                // When: `x < 0.0 || y < 0.0` — left of or above the grid
                // origin, which floors to a negative cell index.
                return None;
            }
            let col = (x / self.cell_w).floor() as i32;
            let row = (y / self.cell_h).floor() as i32;
            if col < 0 || row < 0 {
                // When: `col < 0 || row < 0` — a fractional origin can still
                // floor below zero after the non-negative check above.
                return None;
            }
            return (row < i32::from(rows) && col < i32::from(cols))
                .then_some((0, row as u16, col as u16));
        }
        // Pane resolution: find the pane whose raster-px rect contains
        // (px, py). Split panes have different origins, so this MUST
        // happen before the column search.
        let pane = self.last_pane_layout.iter().find(|p| {
            px >= p.origin_x_logical
                && px < p.origin_x_logical + p.w_logical
                && py >= p.origin_y_logical
                && py < p.origin_y_logical + p.h_logical
        })?;
        let local_x = px - pane.origin_x_logical;
        let local_y = py - pane.origin_y_logical;
        if local_x < 0.0 || local_y < 0.0 {
            // When: `local_x < 0.0 || local_y < 0.0` — float error at a pane
            // edge can push the point just outside the rect that matched.
            return None;
        }
        // Column: linear scan over the pane's snapped_cell_x edges so we
        // pick the bucket the renderer actually drew. Half-open
        // `edge[col] <= px < edge[col+1]`; boundaries resolve to the
        // RHS cell, which matches `partition_point`'s contract.
        let edges = build_snapped_cell_x(pane.origin_x_logical, pane.cell_w_logical, pane.cols);
        let col = pixel_to_local_col(px, &edges, pane.cols)?;
        // Row: cell_h has no per-cell snapping cache today, so the
        // straight division is correct. Clamp to the pane's grid.
        let row_f = local_y / pane.cell_h_logical;
        if row_f < 0.0 {
            // When: `row_f < 0.0` — a non-positive `cell_h_logical` in the
            // snapshot would invert the division.
            return None;
        }
        let row = row_f.floor() as i32;
        if row < 0 || row >= pane.rows as i32 {
            // When: `row < 0 || row >= pane.rows` — the point is inside the
            // pane rect but below its last text row, in trailing padding.
            return None;
        }
        Some((pane.id, row as u16, col))
    }

    // `render` threads borrowed app state plus one copyable process flag through
    // wgpu submission. A parameter struct would still need separate
    // borrow fields (no win over positional args) or force the App layer
    // to construct an interior-mutable wrapper around its own state —
    // both worse than the current shape. Keep the suppression beside this
    // borrow-shape rationale.
    /// Render one frame: terminal grid + cursor + selection + overlays
    /// (tab bar, search, command palette, IME preedit). Submits to the
    /// wgpu queue and presents the surface. See the parameter comments
    /// above for the lifetime / borrow rationale.
    ///
    /// This compatibility entry point runs [`Self::render_with_outcome`] and maps
    /// its outcome through [`PresentOutcome::into_render_result`], restoring the native
    /// Timeout/Occluded retry for callers without typed scheduling. A frame that presents
    /// nothing is `Ok(())` and a failure keeps its error. Once the renderer's
    /// device stops accepting work, the first call returns an error and later
    /// calls return `Ok(())` without doing any work.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        panes: &mut [sonicterm_render_model::PaneRender<'_>],
        theme: &Theme,
        cursor_visible: bool,
        selection: Option<&Selection>,
        copy_mode: Option<&CopyModeState>,
        tabs: &TabBar,
        process_privileged: bool,
        search: Option<&SearchState>,
        palette: Option<&mut CommandPalette>,
        ime: Option<&ImeState>,
        viewport_top_abs: Option<u64>,
        notification: Option<&NotificationBubble>,
        // Cmd-hovered auto-detected URL cell range (viewport coords),
        // or `None` when no URL is hovered while the open-URL modifier
        // is held. When set on the active pane, the URL's glyphs are
        // recolored with the theme accent (companion to the existing
        // hover underline). Same lifetime/gating as the underline.
        hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
        link_preview: Option<&sonicterm_render_model::inputs::LinkPreview>,
    ) -> Result<()> {
        let fonts = self.begin_frame_fonts();
        let outcome = self.render_with_outcome(
            &fonts,
            panes,
            theme,
            cursor_visible,
            selection,
            copy_mode,
            tabs,
            process_privileged,
            search,
            palette,
            ime,
            viewport_top_abs,
            notification,
            hovered_url_cells,
            link_preview,
        );
        if outcome.requires_legacy_redraw() {
            self.request_window_redraw();
        }
        outcome.into_render_result()
    }

    // Same borrow shape as `render`, whose rationale covers this suppression too.
    /// Render one frame, taking the same arguments as [`Self::render`],
    /// and report what happened to it as a [`PresentOutcome`].
    ///
    /// A compatibility wrapper over [`Self::render_releasing`] with a [`BorrowedSource`]: the caller
    /// keeps its grids, so on [`PresentOutcome::Presented`] the frame's receipts are applied to them
    /// here. A skip, a cached reblit, an atlas or surface retry, a stopped device, and a failure all
    /// keep the dirty rows. It opens no counter scope of its own; `render_releasing` opens one.
    ///
    /// [`BorrowedSource`]: sonicterm_render_model::BorrowedSource
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_outcome(
        &mut self,
        fonts: &FrameFonts,
        panes: &mut [sonicterm_render_model::PaneRender<'_>],
        theme: &Theme,
        cursor_visible: bool,
        selection: Option<&Selection>,
        copy_mode: Option<&CopyModeState>,
        tabs: &TabBar,
        process_privileged: bool,
        search: Option<&SearchState>,
        palette: Option<&mut CommandPalette>,
        ime: Option<&ImeState>,
        viewport_top_abs: Option<u64>,
        notification: Option<&NotificationBubble>,
        hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
        link_preview: Option<&sonicterm_render_model::inputs::LinkPreview>,
    ) -> PresentOutcome {
        let frame = self.render_releasing(
            fonts,
            sonicterm_render_model::BorrowedSource(&mut *panes),
            theme,
            cursor_visible,
            selection,
            copy_mode,
            tabs,
            process_privileged,
            search,
            palette,
            ime,
            viewport_top_abs,
            notification,
            hovered_url_cells,
            link_preview,
        );
        settle_borrowed_frame(frame, panes)
    }

    // Same borrow shape as `render`, whose rationale covers this suppression too.
    /// Render one frame from `source` and report how it ended, with a metadata receipt per pane a
    /// presented frame acknowledges.
    ///
    /// Assembly runs inside `source.lend`; when it returns the source is dropped, so an owning source
    /// releases every parser guard before the surface is acquired, the retained frame is blitted, or
    /// the frame is submitted and presented. Nothing is acknowledged here: the caller applies the
    /// receipts. Assembly and presentation share one `&mut self` borrow, so no resize, font change,
    /// atlas reset or rebind can come between them.
    #[allow(clippy::too_many_arguments)]
    pub fn render_releasing(
        &mut self,
        fonts: &FrameFonts,
        source: impl sonicterm_render_model::FrameSource,
        theme: &Theme,
        cursor_visible: bool,
        selection: Option<&Selection>,
        copy_mode: Option<&CopyModeState>,
        tabs: &TabBar,
        process_privileged: bool,
        search: Option<&SearchState>,
        palette: Option<&mut CommandPalette>,
        ime: Option<&ImeState>,
        viewport_top_abs: Option<u64>,
        notification: Option<&NotificationBubble>,
        hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
        link_preview: Option<&sonicterm_render_model::inputs::LinkPreview>,
    ) -> FrameOutcome {
        let _scope = crate::frame_stats::RenderScope::enter(
            self.frame_sink.as_ref(),
            &mut self.unattributed_apply,
        );
        self.debug_assert_prepared(fonts);
        let frame_start = Instant::now();
        // Read before lending, so assembly itself never reaches the device.
        let subpixel_aa = self.effective_subpixel_aa_mode();
        let accepts_gpu_work = self.device_errors.accepts_gpu_work();
        let assembled = lend_and_assemble(source, accepts_gpu_work, |panes| {
            let mut palette = palette;
            assemble_with_fallback(|force_full| {
                self.assemble_frame(
                    subpixel_aa,
                    panes,
                    theme,
                    cursor_visible,
                    selection,
                    copy_mode,
                    tabs,
                    process_privileged,
                    search,
                    palette.as_deref_mut(),
                    ime,
                    viewport_top_abs,
                    notification,
                    hovered_url_cells,
                    link_preview,
                    force_full,
                )
            })
        });
        // The source is gone here: every arm below runs with no parser guard held.
        self.flush_image_upload_rebuild();
        // Any growth, from this assembly, outside it, or left by a reset retry, resizes the texture
        // here, after release and before any present, so a grown atlas never syncs into a smaller
        // texture and recreating one never holds the parser guards. Assembly reads no texture.
        self.rebuild_glyph_upload_if_needed();
        self.count_glyph_atlas_growths(frame_start);
        self.finalize_growth_episodes_if_device_stopped();
        let assembled = match assembled {
            Ok(assembled) => assembled,
            Err(error) => {
                // When: assembly returned `error`, nothing was drawn; report it as failed, with no
                // receipts, and forget the glyph slot keys its passes staged.
                self.row_glyph_cache.discard_staged();
                return FrameOutcome::without_receipts(PresentOutcome::Failed(error));
            }
        };
        let assembled = match settle_without_renderer(assembled) {
            Ok(outcome) => {
                // When: `settle_without_renderer` settled an empty source, return it with no receipts.
                return outcome;
            }
            Err(assembled) => assembled,
        };
        let outcome = match assembled {
            // Settled above; kept so the match names every exit.
            Assembled::NoPanes => PresentOutcome::Skipped(SkipReason::NoPanes),
            Assembled::Unavailable => self.rendering_unavailable(),
            Assembled::PartialFallback => {
                // Only a forced-Full pass reaches here, and force_full makes a fallback impossible;
                // present nothing and plan the next frame from scratch rather than trust the key.
                self.last_frame_key = None;
                self.row_glyph_cache.discard_staged();
                self.request_window_redraw();
                PresentOutcome::Skipped(SkipReason::Noop)
            }
            Assembled::Unchanged { focus_flash } => {
                // When: `Unchanged`, retain the no-assembly fast path and the Windows cached-frame reblit.
                self.skipped_frames = self.skipped_frames.wrapping_add(1);
                tracing::trace!(skipped = self.skipped_frames, "renderer: skipped unchanged frame");
                let outcome = if let Some(before) = self.prepare_cached_present() {
                    // When: `before` describes a retained CPU frame, admit its reblit at this render boundary.
                    let Some(reblit_scope) = self.device_errors.enter_gpu_work("render.reblit")
                    else {
                        // When: `enter_gpu_work` refuses, keep the stopped exit before the focus-flash redraw.
                        return FrameOutcome::without_receipts(self.rendering_unavailable());
                    };
                    self.present_unchanged_frame(before, reblit_scope)
                        .unwrap_or_else(PresentOutcome::Failed)
                } else {
                    // When: `before` is absent, no cached presenter is available for this unchanged plan.
                    PresentOutcome::Skipped(SkipReason::Unchanged)
                };
                if focus_flash
                    && !matches!(
                        outcome,
                        PresentOutcome::RenderingUnavailable(_) | PresentOutcome::Failed(_)
                    )
                {
                    self.request_window_redraw();
                }
                outcome
            }
            Assembled::Noop(key) => {
                // Nothing drawable changed: remember the key without acknowledging any dirt.
                self.last_frame_key = Some(*key);
                PresentOutcome::Skipped(SkipReason::Noop)
            }
            Assembled::AtlasRetry { stamp, evictions } => {
                // The atlas changed during assembly, so its UVs are stale; each path requests a redraw.
                let after = self.glyph_atlas_stamp();
                if growth_only_change(stamp, after, evictions, self.glyph_atlas.evictions()) {
                    self.retry_after_glyph_atlas_growth();
                } else {
                    // When: growth_only_change is false the atlas was evicted, reset or replaced,
                    // so the frame takes the reset path with eviction disabled for one retry.
                    self.reset_glyph_atlas_after_invalidation(stamp, evictions);
                }
                let _discarded = settle_retained_frame(
                    &mut self.last_frame_key,
                    &mut self.row_ink,
                    &mut self.row_glyph_cache,
                    &PresentOutcome::AtlasRetry,
                    None,
                    Vec::new(),
                );
                PresentOutcome::AtlasRetry
            }
            Assembled::Layers(layers) => {
                // When: `Layers` carries owned batches, present them; only a presented frame returns
                // receipts. A presenter `Err` skips settlement, so it discards staged slot keys here.
                return self.present_layers(*layers).unwrap_or_else(|error| {
                    self.row_glyph_cache.discard_staged();
                    FrameOutcome::without_receipts(PresentOutcome::Failed(error))
                });
            }
        };
        FrameOutcome::without_receipts(outcome)
    }

    // Same borrow shape as `render`, whose rationale covers this suppression too.
    /// Plan and assemble one frame from the lent panes, deciding the typed exits first in their
    /// existing order. The result borrows no grid. Fallible steps use `?`; `render_releasing` turns
    /// an error into `PresentOutcome::Failed`.
    #[allow(clippy::too_many_arguments)]
    fn assemble_frame(
        &mut self,
        subpixel_aa: SubpixelAaMode,
        panes: &mut [sonicterm_render_model::PaneRender<'_>],
        theme: &Theme,
        cursor_visible: bool,
        selection: Option<&Selection>,
        copy_mode: Option<&CopyModeState>,
        tabs: &TabBar,
        process_privileged: bool,
        search: Option<&SearchState>,
        palette: Option<&mut CommandPalette>,
        ime: Option<&ImeState>,
        viewport_top_abs: Option<u64>,
        notification: Option<&NotificationBubble>,
        hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
        link_preview: Option<&sonicterm_render_model::inputs::LinkPreview>,
        force_full: bool,
    ) -> Result<Assembled> {
        // `lend_and_assemble` has already taken the empty and stopped exits, so `panes` is not empty.
        let mut gpu_timing = tracing::enabled!(target: "render_timing", tracing::Level::DEBUG)
            .then(|| {
                let now = Instant::now();
                (now, now, Vec::<(&'static str, f32)>::with_capacity(12))
            });
        macro_rules! gpu_lap {
            ($name:literal) => {
                lap(&mut gpu_timing, $name)
            };
        }
        let now = Instant::now();
        let broadcast_participant_ids: Vec<u64> =
            panes.iter().filter(|pane| pane.is_broadcast_participant).map(|pane| pane.id).collect();
        let retained_inline_media_bytes = panes
            .iter()
            .flat_map(|pane| &pane.inline_images)
            .fold(0usize, |total, image| total.saturating_add(image.bgra.len()));
        self.retained_inline_media_bytes = retained_inline_media_bytes;
        let inline_media_hash = {
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            for pane in panes.iter() {
                pane.id.hash(&mut hash);
                pane.inline_images.len().hash(&mut hash);
                for image in &pane.inline_images {
                    (image.id, image.row, image.col, image.width, image.height).hash(&mut hash);
                }
            }
            hash.finish()
        };

        // Advance the atlas frame counter so LRU eviction can
        // distinguish glyphs touched this frame from cold ones. Cheap
        // (one integer increment) and unconditional — even on a fully
        // cached frame the bump is harmless and keeps the counter in
        // step with wall-clock frames for diagnostic dumps.
        self.glyph_atlas.tick_frame();
        let atlas_stamp_at_frame_start = self.glyph_atlas_stamp();
        let atlas_evictions_at_frame_start = self.glyph_atlas.evictions();
        // Build a fingerprint of every input that can affect the rendered
        // pixels. If it matches the last frame, nothing on screen would
        // change — skip text shaping, quad rebuild and GPU submit.
        // Highlights are drawn on the active pane from the same view top that
        // `FramePlan::build` resolves, so the search identity hashes that slice.
        let search_hash = search
            .map(|s| {
                let active = panes.iter().find(|pane| pane.is_active).unwrap_or(&panes[0]);
                let live_top = active.grid.scrollback_len() as u64;
                let view_top = viewport_top_abs.unwrap_or(live_top).min(live_top);
                s.presentation_hash(view_top, active.grid.rows)
            })
            .unwrap_or(0);
        // Per-component dirty flag for the command palette so that a
        // keystroke into the query box (which changes neither the grid
        // revision nor the active tab) still invalidates the cached frame.
        let palette_hash: u64 = palette
            .as_deref()
            .filter(|p| p.is_open())
            .map(|p| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                // The open-bit is implicit in the filter above; mark with
                // a salt so closed→empty-query opens differ from a stale
                // hash.
                0xC0DE_FA17_u64.hash(&mut h);
                p.query().hash(&mut h);
                p.cursor().hash(&mut h);
                p.selected().hash(&mut h);
                p.len().hash(&mut h);
                p.scroll_offset().hash(&mut h);
                p.presentation_hash().hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        // Likewise for IME preedit — composition changes don't bump grid
        // revision until commit.
        let ime_hash: u64 = ime
            .map(|i| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                i.preedit().hash(&mut h);
                i.is_composing().hash(&mut h);
                // Caret movement changes cursor placement even when the preedit text is unchanged.
                i.cursor().hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        let notification_hash: u64 = notification
            .map(|n| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                n.level.hash(&mut h);
                n.message.hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        // Include every tab's title, order, activity, color, command status, and
        // foreground privilege so inactive-tab changes cannot leave stale chrome.
        // Badges are judged at the instant the tab widths were measured, so the drawn text, the
        // stored widths and this hash all describe one moment.
        let tab_now = tabs.content_measured_at().unwrap_or(now);
        let tab_hash = tab_bar_hash(tabs, tab_now);
        let broadcast_participants_hash: u64 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            broadcast_participant_ids.hash(&mut h);
            h.finish()
        };
        // Blink phase stays outside the key so idle frames do not trigger full text assembly.
        // Compute hover state against the tab bar layout. Done before
        // the FrameKey is built so the cache invalidates as the cursor
        // moves between tabs.
        let hover_tab_idx = self.hovered_tab_index(tabs, self.hover_cursor);
        let quick_select_hint_count = copy_mode
            .and_then(|state| state.quick_select.as_ref())
            .map_or(0, |quick| quick.hints.len() as u32);
        let read_only_mode = copy_mode.is_some_and(CopyModeState::is_read_only);
        let pane_focus_flash_bucket = self.pane_focus_flash_bucket(now);
        let overlay_active = search.is_some()
            || palette.as_deref().is_some_and(CommandPalette::is_open)
            || notification.is_some()
            || link_preview.is_some()
            || ime.is_some_and(|i| i.is_composing() || !i.preedit().is_empty())
            || self.drag_chip.is_some()
            || self.pane_focus_flash.is_some();
        let renderer_hash = {
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            self.font_family.hash(&mut hash);
            self.font_size.to_bits().hash(&mut hash);
            self.scale_factor.to_bits().hash(&mut hash);
            self.font_weight_scale.to_bits().hash(&mut hash);
            self.line_height_mult.to_bits().hash(&mut hash);
            self.tab_bar_visible.hash(&mut hash);
            self.titlebar_inset.to_bits().hash(&mut hash);
            self.panel_padding.to_bits().hash(&mut hash);
            if let Some(preview) = link_preview {
                preview.uri.hash(&mut hash);
                preview.pointer.0.to_bits().hash(&mut hash);
                preview.pointer.1.to_bits().hash(&mut hash);
                preview.available.hash(&mut hash);
            }
            if let Some(chip) = &self.drag_chip {
                chip.title.hash(&mut hash);
                chip.top_left.0.to_bits().hash(&mut hash);
                chip.top_left.1.to_bits().hash(&mut hash);
                chip.scale.to_bits().hash(&mut hash);
                chip.drop_line_x.map(f32::to_bits).hash(&mut hash);
                chip.drop_line_y.0.to_bits().hash(&mut hash);
                chip.drop_line_y.1.to_bits().hash(&mut hash);
                chip.insertion_slot.hash(&mut hash);
                chip.source_tab_idx.hash(&mut hash);
                chip.source_alpha.to_bits().hash(&mut hash);
                chip.ghost_alpha.to_bits().hash(&mut hash);
            }
            hash.finish()
        };
        // The active pane and view the cursor is drawn from, resolved as `FramePlan::build` does.
        let cursor_pane = panes.iter().find(|pane| pane.is_active).unwrap_or(&panes[0]);
        let cursor_live_top = cursor_pane.grid.scrollback_len() as u64;
        let cursor_view_top = viewport_top_abs.unwrap_or(cursor_live_top).min(cursor_live_top);
        // Records are read only when a partial plan is possible: never on the degraded path, on a
        // first frame, or on the forced-Full pass of a fallback.
        let partial_possible =
            !force_full && !self.software_render_degrade && self.last_frame_key.is_some();
        let row_ink = &self.row_ink;
        let mut plan = FramePlan::build(
            FrameFacts {
                window: WindowIdentity {
                    selection: selection.copied(),
                    copy_mode: copy_mode.map(CopyModeIdentity::from),
                    quick_select_hint_count,
                    cursor_visible,
                    tab: tabs.active().map_or(0, |tab| tab.id.0),
                    search_hash,
                    palette_hash,
                    ime_hash,
                    notification_hash,
                    width: self.config.width,
                    height: self.config.height,
                    tab_hash,
                    viewport_top_abs,
                    cursor_shape: self.cursor_shape as u8,
                    cursor_blink: self.cursor_blink,
                    window_focused: self.window_focused,
                    pane_focus_flash_bucket,
                    hover_tab: hover_tab_idx,
                    close_override: u8::from(self.tab_close_override.is_some()),
                    broadcast_participants_hash,
                    inline_media_hash,
                    hovered_url_cells,
                    process_privileged,
                    subpixel_aa,
                    background: [
                        self.bg.r.to_bits(),
                        self.bg.g.to_bits(),
                        self.bg.b.to_bits(),
                        self.bg.a.to_bits(),
                    ],
                    style_rev: self.style_rev,
                    renderer_hash,
                    overlay_active,
                    cursor_cell: drawn_cursor_cell(
                        &*cursor_pane.grid,
                        cursor_pane.id,
                        cursor_view_top,
                        cursor_visible,
                        self.window_focused,
                        read_only_mode,
                    ),
                },
                cell_w: self.cell_w,
                cell_h: self.cell_h,
                padding: [
                    self.padding_left_px(),
                    self.padding_right_px(),
                    self.padding_top_px(),
                    self.padding_bottom_px(),
                ],
                vertical_ink_pad: terminal_vertical_ink_pad(
                    self.cell_h,
                    self.font_stack.as_ref().and_then(|stack| stack.cell_metrics_raster_px().ok()),
                ),
                scrollbar_mode: self.scrollbar_mode,
                degraded: self.software_render_degrade,
                tab_bar_top: self.tab_bar_visible.then(|| self.tab_bar_y_offset()),
                scale: self.scale_factor,
                previous_recolor: self.last_recolor,
            },
            panes.iter().map(|pane| PaneMetadata {
                id: pane.id,
                revision: pane.grid.revision(),
                dirty_generation: pane.grid.dirty_generation(),
                rect: pane.rect_px,
                cols: pane.grid.cols,
                rows: pane.grid.rows,
                scrollback_len: pane.grid.scrollback_len() as u64,
                viewport_top_abs: pane.viewport_top_abs,
                is_active: pane.is_active,
                is_alt: pane.grid.is_alt(),
                scrollbar_alpha: pane.scrollbar_alpha,
                dirty_rows: pane.grid.dirty_rows().collect(),
                row_ink: if partial_possible {
                    row_ink.valid_records(
                        pane.id,
                        pane.grid,
                        crate::frame_plan::resolved_view_top(
                            pane.viewport_top_abs,
                            pane.grid.scrollback_len() as u64,
                        ),
                        pane.grid.rows,
                    )
                } else {
                    // When: `partial_possible` is false, the plan cannot be partial and needs no record.
                    Vec::new()
                },
            }),
            self.last_frame_key.as_ref(),
        );
        self.last_emit_origins =
            plan.panes.iter().map(|pane| (pane.id, [pane.layout.x, pane.layout.y])).collect();
        self.last_pane_layout = plan
            .panes
            .iter()
            .map(|pane| PaneLayoutSnapshot {
                id: pane.id,
                origin_x_logical: pane.layout.x,
                origin_y_logical: pane.layout.y,
                w_logical: pane.layout.w,
                h_logical: pane.layout.h,
                cell_w_logical: self.cell_w,
                cell_h_logical: self.cell_h,
                cols: pane.cols,
                rows: pane.row_count,
            })
            .collect();
        if plan.unchanged {
            // When: `plan.unchanged` holds, retain the no-assembly fast path; the reblit runs after release.
            return Ok(Assembled::Unchanged { focus_flash: pane_focus_flash_bucket != 0 });
        }
        if plan.mode == RenderMode::Noop {
            // When: `plan.mode` is Noop, remember its identity without acknowledging unpresented grid dirt.
            return Ok(Assembled::Noop(Box::new(plan.key)));
        }
        if force_full {
            plan.force_full();
        }
        // Every exit after the unchanged and no-op ones uses the renderer's frame scratch: the
        // lease restores it on any return, and a drawable frame hands it to presentation.
        let mut scratch_lease = self.frame_scratch.lease();
        let scratch = scratch_lease.get();
        let frame_scratch::FrameScratch {
            glyphs: glyph_instances,
            overlay_glyphs: overlay_glyph_instances,
            quads,
            overlay_quads: quads_overlay,
            images: image_glyph_instances,
            row_spans,
            underlines,
            underline_owners,
            staged_ranges,
            missing_tofu,
            pane_rects: pane_rect_scratch,
            snapped,
            snapped_peak,
            row_keys,
        } = scratch;
        let inline_media_changed = self.last_frame_key.as_ref().is_none_or(|previous| {
            previous.window.inline_media_hash != plan.key.window.inline_media_hash
        });
        // The cursor recolors this frame performs, accumulated across the copy-mode and block sites.
        let mut frame_recolor = crate::cursor::RecolorRecord::default();
        pane_rect_scratch.extend(plan.panes.iter().map(|pane| {
            let rect = pane.full_rect;
            (pane.id, PaneRect::new(rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32))
        }));
        let pane_rects = pane_rect_scratch.as_slice();
        struct PaneView<'a> {
            grid: &'a Grid,
            planned: &'a PlannedPane,
            pane_id: u64,
            origin_x: f32,
            origin_y: f32,
            rect_w: f32,
            rect_h: f32,
            scrollbar_alpha: f32,
            inline_images: &'a [sonicterm_render_model::InlineImage],
        }
        let pane_views: Vec<_> = panes
            .iter()
            .zip(&plan.panes)
            .map(|(pane, planned)| PaneView {
                grid: &*pane.grid,
                planned,
                pane_id: planned.id,
                origin_x: planned.layout.x,
                origin_y: planned.layout.y,
                rect_w: planned.layout.w,
                rect_h: planned.layout.h,
                scrollbar_alpha: planned.scrollbar_alpha,
                inline_images: &pane.inline_images,
            })
            .collect();
        let active = &plan.panes[plan.active_index];
        let active_origin_x = active.layout.x;
        let active_origin_y = active.layout.y;
        let active_pane_x = active_origin_x;
        let active_pane_y = active_origin_y;
        let active_pane_w = active.layout.w;
        let active_pane_h = active.layout.h;
        let grid = pane_views[plan.active_index].grid;
        gpu_lap!("frame_key");
        // CPU frame assembly runs from here to the overlays lap: one clock pair per assembled frame.
        let assembly_started = crate::frame_stats::assembly_clock();
        // Note: do NOT cache key here. If prepare()/get_current_texture()
        // fails on a transient surface state we'd cache a key for a frame
        // that never actually got drawn, and the next redraw could
        // early-exit silently. Cache only AFTER successful submit+present.

        // -------- Walk the grid once, emit one glyph instance per
        // visible cell, and send each atlas miss to its rasterizer: the
        // font stack, or the block-glyph path for characters `BlockKey`
        // recognizes. No per-row cache, no rich-text buffer, no glyphon
        // shape pass for the terminal grid.
        let fg_default = self.fg_default;
        // Underline runs collected per pane. We record
        // (origin_x, origin_y, pane_cols, row, col_a, col_b) where
        // origin_{x,y} is the PANE's origin (pad / top_inset) and
        // `pane_cols` is the originating pane's column count, captured at
        // insert time, and each entry carries its own `origin_{x,y}`.
        // Both are needed because the emit loop draws underlines from every
        // pane: without the origin an inactive pane's underlines land under
        // the active pane's coordinates, and without `pane_cols` the
        // per-origin snapped-edge cache is sized from the active pane, so a
        // wider inactive pane has its underlines clamped and truncated.
        // Per entry of `underlines`, the staging index of the row that pushed it, so each
        // underline's quads merge into that row's one ink record.
        // Per drawn pane, the staging indices its glyph rows took, in ascending slot order, so
        // the background loop merges each row's quads into the record the glyph loop staged.
        // Overlay glyph instances — palette text + (future) other modals.
        // Kept separate so they can be drawn AFTER `quad_overlay` paints
        // the modal backdrop, otherwise they'd be hidden by their own
        // background. (— palette text was previously routed through
        // glyphon's TextRenderer which bypassed the device-scale atlas
        // path used by `emit_tab_title_glyphs`, hence the HiDPI blur.)
        // Each emitted terminal row's glyph range in `glyph_instances` and its ink bounds, so
        // highlight recolors on the main list scan only rows whose ink meets the target.
        // Missing-glyph "tofu" outlines collected during the cell walk.
        // Drawn via the quad pipeline after the text instances.
        // Mirror of missing_tofu, recording just the codepoint so tests
        // can assert "no class regressed" without depending on pixel
        // layout. Cleared every frame; published into `self.last_missing_chars`
        // before render() returns.
        let mut missing_chars_this_frame: Vec<char> = Vec::new();
        // The chrome counterpart: every chrome layout below notes its tofu into this scope, which
        // closes with the assembled frame; a frame that fails to assemble discards it on drop.
        let missing_chrome_scope = chrome_text::MissingChromeScope::enter();
        // Geometry is in raster pixels, so px_to_ndc uses the unscaled physical surface dimensions.
        let sw = self.config.width as f32;
        let sh = self.config.height as f32;
        // Each PaneView supplies its own origin instead of the window-level padding or inset.
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        // Baseline offset inside the cell box. Font-stack tiles carry
        // a vertical offset relative to the baseline; we want screen-y
        // relative to the cell top. Using ≈80% of cell height matches
        // a reasonable ascent for monospace fonts at the configured
        // line-height; finer baseline control would require querying
        // font metrics.
        let baseline_y_in_cell = cell_h * 0.8;
        let software_presenter = self.uses_windows_software_presenter();

        let raster_px = self.raster_px(self.font_size);
        {
            // Post-glyphon the grid path is wezterm-only.
            // FontStack is the sole rasterizer; on test fixtures
            // without bundled fonts (FontStack returns None) the grid
            // walk skips per-glyph emission and only paints quads.
            let mut wt_raster = self.font_stack.clone();
            // Theme accent for the Cmd-hovered URL recolor. `UiPalette::accent`
            // is a linear-sRGB `[f32;4]` (alpha 1.0), the same space the
            // per-glyph `color` field carries, so it drops in with no
            // conversion. PERF: `UiPalette::from_theme` does ~20 hex parses +
            // sRGB→linear `powf` conversions; computing it unconditionally
            // every frame added measurable render latency to plain output
            // repaints (e.g. `ls -al`). It's only consumed when a URL is
            // ACTIVE-hovered (modifier held) and glyphs recolor to accent;
            // plain hover draws only a yellow underline, so compute lazily —
            // `[0.0;4]` otherwise. #perf
            let hovered_url_accent: [f32; 4] = if hovered_url_needs_accent(hovered_url_cells) {
                self.chrome_caches.palette.palette_for(theme).accent
            } else {
                // When: `!hovered_url_needs_accent` — plain hover draws only
                // the underline, so the accent is never sampled.
                [0.0, 0.0, 0.0, 0.0]
            };
            // One cache pass and one fresh ink stage per assembly pass, before any pane pins or
            // admits a row; the test fixture starts its passes through the same helper.
            begin_glyph_pass(
                &mut self.row_glyph_cache,
                &mut self.row_ink,
                pane_views.iter().map(|pane| (pane.planned, pane.grid.cols)),
            );
            if let Some(probe) = self.emitted_rows_probe.as_mut() {
                // A test enabled the inspector: this pass records only what it emits itself.
                probe.clear();
            }
            for (pane_index, pv) in
                pane_views.iter().enumerate().filter(|(_, pane)| pane.planned.full_clip.is_some())
            {
                let pane_staged_start = self.row_ink.staged_len();
                // The inspector's record of this pane's emitted slots; `None` in production.
                let mut pane_emitted = self.emitted_rows_probe.as_ref().map(|_| Vec::new());
                // Per-cell device-pixel snapping rounds each cell's left edge independently, which
                // at fractional DPI alternates the cell pitch; every glyph path derives its cell
                // edges from these shared snapped edges, so adjacent cells share an edge.
                // Each pane's glyph edges reuse slot `2 * pane_index` of the frame scratch.
                let edge_slot = 2 * pane_index;
                frame_scratch::fill_snapped_slot(
                    snapped,
                    snapped_peak,
                    edge_slot,
                    pv.origin_x,
                    cell_w,
                    pv.grid.cols,
                );
                let snapped_cell_x: &[f32] = &snapped[edge_slot];
                // Hover recolor applies only to the pane named by the hit-test, so a split at the
                // same row and columns never inherits another pane's accent.
                let pane_hovered_url =
                    hovered_url_cells.filter(|hovered| hovered.pane_id == pv.pane_id);
                assemble_pane_glyph_rows(
                    GlyphShaping {
                        atlas: &mut self.glyph_atlas,
                        row_cache: &mut self.row_glyph_cache,
                        font_stack: self.font_stack.as_ref(),
                        wt_raster: wt_raster.as_mut(),
                        style_rev: self.style_rev,
                        theme,
                        fg_default,
                        raster_px,
                        cell_size: (cell_w, cell_h),
                        surface: (sw, sh),
                        baseline_y_in_cell,
                        hovered_url_accent,
                        software_presenter,
                    },
                    PaneGlyphRows {
                        pane_id: pv.pane_id,
                        grid: pv.grid,
                        planned: pv.planned,
                        origin: (pv.origin_x, pv.origin_y),
                        snapped_cell_x,
                        pane_hovered_url,
                    },
                    GlyphFrame {
                        glyph_instances: &mut *glyph_instances,
                        underlines: &mut *underlines,
                        missing_tofu: &mut *missing_tofu,
                        missing_chars_this_frame: &mut missing_chars_this_frame,
                        row_spans: &mut *row_spans,
                    },
                    PaneGlyphSinks {
                        row_ink: &mut self.row_ink,
                        ink_surface: plan.surface,
                        underline_owners: &mut *underline_owners,
                        injected_row_glyph: self.injected_row_glyph,
                        emitted_slots: pane_emitted.as_mut(),
                        row_keys: &mut *row_keys,
                    },
                );
                staged_ranges.push((pv.pane_id, pane_staged_start..self.row_ink.staged_len()));
                if let (Some(probe), Some(slots)) = (self.emitted_rows_probe.as_mut(), pane_emitted)
                {
                    // The inspector is on: keep the slots this pane's rows were emitted at.
                    probe.push((pv.pane_id, slots));
                }
            } // end per-pane loop
        }
        if std::mem::take(&mut self.fault_assembly_error) {
            // When: `fault_assembly_error` is armed, assembly fails as a real `Err` would, after
            // its glyph rows were staged, so the error arm must discard them.
            return Err(anyhow!("injected assembly failure"));
        }

        // Overlay quads — drawn AFTER terminal text + main quads so that
        // palette / search-input / IME backgrounds visually cover the
        // terminal content underneath. Emitted into the same vector as the
        // main quads, terminal glyphs bleed through overlay dialogs.

        let inline_image_placements: Vec<InlineImagePlacement<'_>> = pane_views
            .iter()
            .flat_map(|pv| pv.inline_images.iter().map(move |image| (image, pv)))
            .enumerate()
            .map(|(painter_order, (image, pv))| InlineImagePlacement {
                image,
                origin_x: pv.origin_x,
                origin_y: pv.origin_y,
                content_clip: pv.planned.content_clip,
                painter_order,
            })
            .collect();
        let has_renderable_inline_media = inline_image_placements
            .iter()
            .any(|placement| placement.visible_rect(cell_w, cell_h, sw, sh).is_some());
        self.inline_media_absent_since = next_inline_media_absent_since(
            self.inline_media_absent_since,
            has_renderable_inline_media,
            Instant::now(),
        );
        self.demote_image_atlas_if_idle(has_renderable_inline_media);
        let image_atlas_promoted = self.promote_image_atlas_if_needed(
            has_renderable_inline_media,
            retained_inline_media_bytes,
        );
        if inline_media_changed
            && !image_atlas_promoted
            && image_atlas_reset_warranted(&self.image_atlas)
        {
            self.reset_image_atlas();
        }
        let skipped_inline_images = emit_inline_image_instances(
            &mut self.image_atlas,
            &mut *image_glyph_instances,
            &inline_image_placements,
            cell_w,
            cell_h,
            sw,
            sh,
        );
        report_inline_image_pressure(
            inline_media_changed,
            skipped_inline_images,
            &self.image_atlas,
        );

        // build the active pane's shared device-pixel-snapped
        // column-edge cache once per frame, hoisted above every overlay
        // path. Every overlay anchored to the active pane (selection,
        // cursor, copy-mode, quick-select, hyperlink, search-highlight,
        // underline-decoration, IME preedit) reads its x edges from
        // this cache so it stays edge-aligned with adjacent glyph cells
        // at fractional DPI. Integer scales (1.0/2.0) are an identity
        // fast path inside `snap_to_device_pixels`, so mac dHash
        // baselines stay green by construction. Per diagnosis,
        // per-pane bg fill builds its OWN cache (see the per-pane bg
        // loop below) — it MUST NOT share the active pane's cache.
        let active_snapped_cell_x: Vec<f32> =
            build_snapped_cell_x(active_origin_x, self.cell_w, grid.cols);

        // Per-cell ANSI background colors. Must be pushed FIRST so that
        // selection / cursor / overlay quads draw on top — otherwise an
        // ANSI-colored cell would obscure the selection highlight. The
        // helper run-length coalesces adjacent same-bg cells into a single
        // wide quad (an 80-col `\033[41m` fill becomes 1 quad, not 80).
        // Cells whose bg resolves to the theme default are skipped: the
        // attachment clear or partial replacement reset already covers that area.
        // Background quads use each pane's own origin, including inactive panes.
        //
        // P2: per-row LineQuadCache. Background quads are a
        // hot QuadInstance source in dense-cell workloads. Each row's
        // emission is keyed on (pane_id,
        // abs_row, content+geom+style+selection hash); on a hit we
        // `extend_from_slice` the cached slice and skip the per-cell
        // run-length-encode walk in `emit_cell_bg_quads_for_row`.
        let sel_bbox_for_quads: Option<(u64, u16, u64, u16)> = selection.map(|s| {
            let (a, b) = s.normalized();
            (a.0, a.1, b.0, b.1)
        });
        let total_visible_rows: u16 = pane_views.iter().map(|pv| pv.grid.rows).sum();
        self.line_quad_cache.resize(total_visible_rows.max(1));
        for (pane_index, pv) in
            pane_views.iter().enumerate().filter(|(_, pane)| pane.planned.full_clip.is_some())
        {
            let pv_grid: &Grid = pv.grid;
            let pane_id: crate::row_quad_cache::PaneId = pv.pane_id;
            let pane_rect = PaneRect { x: pv.origin_x, y: pv.origin_y, w: pv.rect_w, h: pv.rect_h };
            let view_top_abs_bg = pv.planned.view_top_abs;
            let pane_staged = staged_ranges
                .iter()
                .find(|(staged_pane, _)| *staged_pane == pane_id)
                .map(|(_, range)| range.clone());
            // Mirror RowGlyphCache's dirty-row invalidation: drop the absolute
            // rows of every live row the VT thread mutated since the last frame.
            invalidate_planned_quad_rows(&mut self.line_quad_cache, pv.planned);
            let pad_bg = pane_rect.x;
            let top_inset_bg = pane_rect.y;
            let max_cols = pv.planned.background_cols;
            let max_rows = pv.planned.background_rows;
            if max_cols == 0 || max_rows == 0 {
                // When: `max_cols == 0 || max_rows == 0` — the pane rect is
                // thinner than one cell, so no background quad would fit.
                continue;
            }
            // per-pane snapped-edge cache for bg-fill runs. Per
            // diagnosis Recommendation, per-pane bg must NOT reuse the
            // active pane's cache because each split-pane has its own
            // pad and the snapped column edges differ.
            // Each pane's background edges reuse slot `2 * pane_index + 1` of the frame scratch.
            let bg_edge_slot = 2 * pane_index + 1;
            frame_scratch::fill_snapped_slot(
                snapped,
                snapped_peak,
                bg_edge_slot,
                pad_bg,
                cell_w,
                pv_grid.cols,
            );
            let snapped_cell_x_bg: &[f32] = &snapped[bg_edge_slot];
            for (r, row_abs) in pv.planned.rows().take(max_rows as usize) {
                if !pv.planned.emit_rows[usize::from(r)] {
                    // When: the plan does not emit slot `r`, its retained background stays.
                    continue;
                }
                let Some(row_cells) = pv_grid.row_at_abs(row_abs) else {
                    // When: `pv_grid.row_at_abs(row_abs)` is None — the row is
                    // outside the scrollback this pane still retains.
                    continue;
                };
                let quads_before = quads.len();
                let geometry = RowBackgroundGeometry {
                    origin: (pad_bg, top_inset_bg),
                    pane_size: (pane_rect.w, pane_rect.h),
                    cell_size: (cell_w, cell_h),
                    surface: (sw, sh),
                    max_cols,
                };
                let _replayed = emit_row_background(
                    &mut self.line_quad_cache,
                    RowBackgroundRow {
                        pane_id,
                        grid: pv_grid,
                        view_top_abs: view_top_abs_bg,
                        slot: r,
                    },
                    row_cells.iter(),
                    (self.style_rev, theme, sel_bbox_for_quads),
                    &geometry,
                    snapped_cell_x_bg,
                    &mut *quads,
                );
                let mut ink = crate::row_ink::InkEdges::default();
                for quad in &quads[quads_before..] {
                    ink.add_px(crate::cursor::ndc_rect_px(quad.rect, sw, sh));
                }
                let rect = ink.to_rect(plan.surface);
                match pane_staged.clone().and_then(|range| self.row_ink.staged_index(range, r)) {
                    Some(staged) => self.row_ink.merge_staged(staged, rect),
                    None => {
                        // When: `staged_index` finds no glyph-loop record for `r`, stage one here
                        // so the row's background ink is never dropped.
                        let _ = self.row_ink.stage_row(pane_id, r, pv_grid, view_top_abs_bg, rect);
                    }
                }
            }
        }

        if let Some((flash_pane_id, flash_alpha)) = self.pane_focus_flash_alpha(now) {
            if let Some(pv) = pane_views.iter().find(|pv| pv.pane_id == flash_pane_id) {
                let chrome = pv.planned.chrome;
                quads.push(focus_flash_quad(
                    self.bg_rgba,
                    (chrome.x, chrome.y, chrome.w, chrome.h),
                    flash_alpha,
                    (sw, sh),
                ));
            }
        }

        // Per-pane scrollbar emit. Runs after row backgrounds and before
        // selection, cursor, and modal overlays. Auto opacity comes from the
        // app state machine; geometry remains shared with hit-testing.
        for pv in pane_views.iter().filter(|pane| pane.planned.full_clip.is_some()) {
            let pane_rect = pv.planned.chrome;
            let viewport_rows = pv.planned.row_count;
            let total_rows = pv.planned.scrollback_len + u64::from(viewport_rows);
            let view_top = pv.planned.view_top_abs;
            emit_pane_scrollbar(
                &mut *quads_overlay,
                pane_rect,
                viewport_rows,
                total_rows,
                view_top,
                self.scrollbar_mode,
                theme,
                sw,
                sh,
                pv.scrollbar_alpha,
                self.scale_factor,
            );
        }

        if self.injected_test_glyph.is_some() {
            // An injected test glyph joins the terminal glyphs before any recolor reads them.
            self.push_injected_test_glyph(&mut *glyph_instances, sw, sh);
        }

        if let Some(sel) = selection {
            if !sel.is_empty() {
                // Selection highlights are anchored to the active pane's
                // origin. They MUST be clipped to that pane's rect — otherwise
                // a selection that extends past the pane's last visible column
                // (e.g. the user drags across the split into the neighbouring
                // pane) would emit a quad that visually bleeds into the
                // neighbouring pane's grid area. Regression-guard for the
                // bug where dragging in a split-right layout painted the
                // selection across both panes.
                let pane_x = active_origin_x;
                let pane_y = active_origin_y;
                // Pane rect_px is the source of truth — see note above.
                let pane_w = active_pane_w;
                let pane_h = active_pane_h;
                // Selection rows are scrollback-ABSOLUTE; resolve the active
                // pane's view top so `selection_quad_rects` can map them back
                // to viewport rows (so the highlight follows the TEXT when
                // scrolled).
                let sel_view_top_abs = plan.active_view_top_abs;
                push_selection_quads(
                    &mut *quads,
                    sel,
                    &SelectionGeometry {
                        view_top_abs: sel_view_top_abs,
                        grid_size: (grid.rows, grid.cols),
                        origin: (active_origin_x, active_origin_y),
                        cell_size: (self.cell_w, self.cell_h),
                        clip: (pane_x, pane_y, pane_w, pane_h),
                        surface: (sw, sh),
                    },
                    &active_snapped_cell_x,
                    self.selection_color,
                );
            }
        }

        if let Some(copy_mode) = copy_mode {
            if let Some(quick_select) = copy_mode.quick_select.as_ref() {
                self.prepare_quick_select_overlay(
                    quick_select,
                    active_origin_x,
                    active_origin_y,
                    grid.scrollback_len(),
                    grid.rows as usize,
                    theme,
                    sw,
                    sh,
                    &mut *quads_overlay,
                    &active_snapped_cell_x,
                );
            }
            let view_top_abs = plan.active_view_top_abs;
            if let Some((cx, cy)) = Self::emit_copy_mode_quads(
                copy_mode,
                grid,
                view_top_abs,
                active_origin_x,
                active_origin_y,
                self.cell_w,
                self.cell_h,
                sw,
                sh,
                self.selection_color,
                self.cursor_color,
                &mut *quads,
                &active_snapped_cell_x,
            ) {
                let RecolorOutcome { visited, record } = recolor_cursor_glyphs_in(
                    &mut *glyph_instances,
                    row_spans,
                    cx,
                    cy,
                    self.cell_w,
                    self.cell_h,
                    sw,
                    sh,
                    self.cursor_text_color,
                );
                crate::frame_stats::note_recolor_glyphs_visited(|| visited);
                frame_recolor = frame_recolor.merge(record);
            }
        }
        if cursor_visible && self.window_focused && !read_only_mode {
            // Hide the cursor when the viewport is scrolled away from the
            // live region — its absolute row is `scrollback_len + cursor.row`,
            // which sits below the bottom of a scrolled-back view.
            let live_top = grid.scrollback_len() as u64;
            let view_top = plan.active_view_top_abs;
            if terminal_cursor_drawn_at_view(view_top, live_top) {
                // read both cursor cell left edge AND width from the
                // shared snapped-edge cache so the cursor (block / bar /
                // underline) lines up with its glyph cell at fractional DPI.
                // The same columns `drawn_cursor_cell` records in the frame identity.
                let (cur_col, cursor_span) = terminal_cursor_columns(grid);
                let cur_col_clamped = cur_col.min(active_snapped_cell_x.len().saturating_sub(2));
                let end_col = (cur_col_clamped + cursor_span)
                    .min(active_snapped_cell_x.len().saturating_sub(1));
                let mut cx = active_snapped_cell_x
                    .get(cur_col_clamped)
                    .copied()
                    .unwrap_or(active_origin_x + f32::from(grid.cursor.col) * self.cell_w);
                let cw = active_snapped_cell_x
                    .get(end_col)
                    .map(|r| r - cx)
                    .unwrap_or(self.cell_w * cursor_span as f32);
                let cy = active_origin_y + f32::from(grid.cursor.row) * self.cell_h;
                // Visible preedit moves the terminal cursor to its caret only when search does not own composition.
                if search.is_none() {
                    if let Some(i) = ime {
                        let text = i.preedit();
                        // gate on visible ink (NOT just non-empty) and
                        // reuse the shared pure helper, so a whitespace-only
                        // macOS marked string never shoves the cursor block
                        // into empty space with no glyph under it.
                        let caret_byte = i.cursor().map(|(_, e)| e).unwrap_or(text.len());
                        let font_size = self.raster_px(self.font_size);
                        cx += preedit_caret_advance(text, caret_byte, font_size);
                    }
                }
                // Keep every cursor shape at its exact theme color. Alpha
                // fading over the terminal background makes Gruvbox yellow
                // read as olive/green during real redraws.
                let color = active_cursor_color(self.cursor_color);
                // Wezterm cursor shapes:
                //   Block     → full-cell quad, glyph re-rendered in bg
                //   Bar       → 2px vertical bar pinned to the left edge
                //   Underline → 2px horizontal bar pinned to the bottom
                // We pick a ~2px sub-cell thickness rather than something
                // proportional to cell_h so the bar stays crisp on both
                // small and large font sizes (no half-pixel sub-stem).
                // 2 logical px scaled to physical px so the bar/underline
                // keep a constant physical thickness across DPIs (min 1px).
                let subshape_px: f32 = (2.0 * self.scale_factor).round().max(1.0);
                match self.cursor_shape {
                    CursorShape::Block => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy, cw, self.cell_h),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                        let RecolorOutcome { visited, record } = recolor_cursor_glyphs_in(
                            &mut *glyph_instances,
                            row_spans,
                            cx,
                            cy,
                            cw,
                            self.cell_h,
                            sw,
                            sh,
                            self.cursor_text_color,
                        );
                        crate::frame_stats::note_recolor_glyphs_visited(|| visited);
                        frame_recolor = frame_recolor.merge(record);
                    }
                    CursorShape::Bar => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy, subshape_px, self.cell_h),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                    }
                    CursorShape::Underline => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy + self.cell_h - subshape_px, cw, subshape_px),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }

        // OSC 8 hyperlinks are semantic, not visual. Do not tint/underline
        // every hyperlink cell permanently: prompts such as Oh My Posh wrap
        // the path segment in a `file:` hyperlink, and a permanent overlay
        // changes the segment's configured truecolor background compared with
        // Windows Terminal. Link affordance is drawn below only for the
        // currently hovered URL span.

        gpu_lap!("grid_walk");
        // Underline quads — drawn last so they appear on top of the text.
        // SGR 4:n style and SGR 58 colour are stored per-cell and coalesced
        // above, matching WezTerm/xterm underline semantics instead of the
        // old single-colour single-line approximation.
        let underline_thickness = (self.cell_h * 0.08).max(1.0);
        // underlines are collected from every pane (each entry
        // carries its own `origin_x` == pane pad), so memoize a snapped
        // cache per distinct pane pad. Most frames have ≤ 2 panes, so
        // the linear-scan map is cheaper than a HashMap.
        // Step-4 revise (option (a)): each entry also carries the
        // ORIGINATING pane's column count. Previously this loop sized
        // the cache from `grid.cols` (== ACTIVE pane), which clamped
        // and truncated underlines on wider INACTIVE panes. Key the
        // cache by (pad_bits, pane_cols) and size it accordingly.
        let mut underline_caches: Vec<(u32, u16, Vec<f32>)> = Vec::new();
        for (entry, (origin_x, origin_y, pane_cols, row, run)) in underlines.iter().enumerate() {
            let pad_bits = origin_x.to_bits();
            let cache = if let Some((_, _, c)) =
                underline_caches.iter().find(|(b, pc, _)| *b == pad_bits && *pc == *pane_cols)
            {
                c
            } else {
                // When: the `find` returned None — no cache exists yet for this
                // origin and column count, so one is built and memoized.
                let c = build_snapped_cell_x(*origin_x, self.cell_w, *pane_cols);
                underline_caches.push((pad_bits, *pane_cols, c));
                &underline_caches.last().unwrap().2
            };
            let end_exclusive = (run.end_col as usize).saturating_add(1);
            let cache_end = end_exclusive.min(cache.len().saturating_sub(1));
            let col_a_usize = (run.start_col as usize).min(cache_end);
            let x = cache
                .get(col_a_usize)
                .copied()
                .unwrap_or(*origin_x + f32::from(run.start_col) * self.cell_w);
            let w = cache
                .get(cache_end)
                .map(|r| r - x)
                .unwrap_or_else(|| f32::from(run.end_col - run.start_col + 1) * self.cell_w);
            let y = *origin_y + f32::from(*row) * self.cell_h;
            let underline_color =
                chrome_color_to_linear_rgba(color_to_chrome(run.color, theme, self.fg_default));
            let quads_before = quads.len();
            push_underline_quads(
                &mut *quads,
                run.style,
                x,
                y,
                w,
                self.cell_h,
                underline_thickness,
                sw,
                sh,
                underline_color,
            );
            // Dotted and curly underlines reach below the cell, so the drawn quads join the record.
            if let Some(staged) = underline_owners.get(entry) {
                let mut ink = crate::row_ink::InkEdges::default();
                for quad in &quads[quads_before..] {
                    ink.add_px(crate::cursor::ndc_rect_px(quad.rect, sw, sh));
                }
                self.row_ink.merge_staged(*staged, ink.to_rect(plan.surface));
            }
        }

        // Hover target underline. The pane id travels with the hit so an
        // inactive split can reveal its own path without painting another
        // pane's fragments at the same rows and columns.
        if let Some(h) = hovered_url_cells {
            // When: `hovered_url_cells` contains `h`, render every canonical fragment for its owning pane.
            if let Some(hovered_view) = pane_views.iter().find(|view| view.pane_id == h.pane_id) {
                // When: `pane_views` finds `h.pane_id`, project all fragments through that pane's geometry.
                let hov_accent = if h.active {
                    self.chrome_caches.palette.palette_for(theme).accent
                } else {
                    // When: `h.active` is false, render the non-clickable hover hint in the theme's yellow rather than the action accent.
                    hex_to_premultiplied_rgba(theme.colors.ansi.yellow.0.as_str(), 0.9)
                };
                let hovered_grid_cols = hovered_view.grid.cols;
                let hcache =
                    build_snapped_cell_x(hovered_view.origin_x, self.cell_w, hovered_grid_cols);
                for span in h.spans() {
                    let Some((x, y, width, height)) = hovered_url_span_rect(
                        *span,
                        hovered_grid_cols,
                        hovered_view.grid.rows,
                        hovered_view.origin_x,
                        hovered_view.origin_y,
                        self.cell_w,
                        self.cell_h,
                        &hcache,
                    ) else {
                        // When: `span` has no visible coverage in `hovered_view`, emit no stale geometry.
                        continue;
                    };
                    push_underline_quads(
                        &mut *quads,
                        UnderlineStyle::Single,
                        x,
                        y,
                        width,
                        height,
                        underline_thickness,
                        sw,
                        sh,
                        hov_accent,
                    );
                }
            }
        }

        // -------- Missing-glyph tofu fallback ------------------------------
        // For cells whose rasterizer returned no tile (and char isn't
        // whitespace), draw a thin outlined rectangle so the gap is
        // visible. Helps catch font-fallback misses (emoji etc.).
        for (x, y, w, h, col) in missing_tofu.iter() {
            let rgba = with_premultiplied_alpha(chrome_color_to_linear_rgba(*col), 0.55);
            let t = 1.0_f32; // border thickness
                             // Top
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y, *w, t, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Bottom
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y + *h - t, *w, t, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Left
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y, t, *h, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Right
            quads.push(QuadInstance {
                rect: px_to_ndc(*x + *w - t, *y, t, *h, sw, sh),
                color: rgba,
                ..Default::default()
            });
        }

        // -------- Pane splitters + broadcast safety chrome ------------------
        // Splitters are 1px interior seams at the shared OUTER pane boundary.
        // They are not pane borders: no window perimeter is drawn, and the
        // seam sits outside the per-pane cell padding that is applied inside
        // each pane rect by the layout caller.
        if pane_rects.len() > 1 {
            for splitter in splitter_rects_from_panes(pane_rects, 1.0) {
                quads.push(QuadInstance {
                    rect: px_to_ndc(
                        splitter.rect.x,
                        splitter.rect.y,
                        splitter.rect.w,
                        splitter.rect.h,
                        sw,
                        sh,
                    ),
                    color: self.splitter_color,
                    ..Default::default()
                });
            }
        }

        // Safety edges sit above terminal ink, images, and the scrollbar, but below modal chrome.
        emit_broadcast_borders(
            &mut *quads_overlay,
            pane_rects,
            &broadcast_participant_ids,
            hex_to_premultiplied_rgba(theme.colors.bright.red.0.as_str(), 1.0),
            sw,
            sh,
        );
        // -------- Tab bar ---------------------------------------------------
        // The insertion gap below opens 8 px at the current drop slot when a
        // drag is active over this bar.
        // Title glyphs can reach above the padded band, so their ink is measured from here.
        let tab_glyph_start = glyph_instances.len();
        if self.tab_bar_visible {
            // When: `self.tab_bar_visible` — a hidden bar reserves no height,
            // so its strip, tab quads, and titles are all skipped.
            let insertion_slot = self.drag_chip.as_ref().and_then(|c| c.insertion_slot);
            let source_tab_idx = self.drag_chip.as_ref().and_then(|c| c.source_tab_idx);
            let source_alpha = self.drag_chip.as_ref().map(|c| c.source_alpha).unwrap_or(1.0);
            let layout = TabBarLayout::compute_with_insertion_slot(
                tabs,
                sw,
                self.tab_bar_logical_height(),
                insertion_slot,
            )
            .with_top_offset(self.tab_bar_y_offset());
            // Round 3 — premium browser-style chrome.
            // The structural colors come from `ui_tokens`, decoupled from
            // the terminal palette so every theme renders the same modern
            // tab bar. The theme.tab.* colors remain authoritative for
            // the title text (active vs inactive fg) so per-theme accents
            // still read through.
            let ui_palette = self.chrome_caches.palette.palette_for(theme);
            // `tok::BG_BASE` is a hardcoded near-black
            // (`#0B0E14`) that is indistinguishable from most dark
            // themes' `theme.background` — the tab bar drew correctly
            // (diagnostic confirmed 6 quads pinned at NDC
            // bottom with alpha 1.0) but the bar bg was the *same
            // pixel value* as the cell-grid bg, so it disappeared.
            // Switch to `ui_palette.bg_base` which is theme-derived
            // (`theme.background` shifted -8% lightness) so every
            // theme gets visible contrast automatically.
            let bar_bg = ui_palette.bg_base;
            // Theme-driven accent (was hardcoded ACCENT_BLUE — broke gruvbox/etc.).
            let accent_blue = ui_palette.accent;
            let separator = ui_palette.border_subtle;
            emit_tab_bar_quads(
                &mut *quads,
                &layout,
                &TabBarQuadParams {
                    accent: accent_blue,
                    separator,
                    border: bar_bg,
                    hover_tab_idx,
                    surface: (sw, sh),
                    active_panel_marker_alpha: if self.window_focused {
                        ACTIVE_PANEL_MARKER_ALPHA_FOCUSED
                    } else {
                        ACTIVE_PANEL_MARKER_ALPHA_UNFOCUSED
                    },
                },
            );
            for t in &layout.tabs {
                // If this tab is the source of
                // a live drag, overlay a translucent bar-bg quad to
                // dim it to roughly `source_alpha` perceived opacity.
                // The quad is painted AFTER the tab body + close icon
                // so it dims everything in the tab's footprint.
                if source_tab_idx == Some(t.idx) {
                    let dim = ((1.0 - source_alpha.clamp(0.0, 1.0)) * 0.45).clamp(0.0, 1.0);
                    let overlay = with_premultiplied_alpha(bar_bg, dim);
                    quads.push(QuadInstance {
                        rect: px_to_ndc(t.bg_rect.x, t.bg_rect.y, t.bg_rect.w, t.bg_rect.h, sw, sh),
                        color: overlay,
                        ..Default::default()
                    });
                }
            }

            // Tab titles are laid out per-tab so each run can be centered
            // by its measured glyph width instead of approximating with
            // column-padding spaces across one long synthetic string.
            let bar_h = self.tab_bar_logical_height();
            let bar_y = self.tab_bar_y_offset();
            let tab_raster_px = self.tab_title_font.raster_px();
            // Center the title using tab_raster_px in the same raster-pixel space as bar_h and bar_y.
            let title_top = bar_y + ((bar_h - tab_raster_px * 1.2) / 2.0).max(0.0);
            let tab_baseline_y = title_top + tab_raster_px * 0.95;
            let mut tab_rasterizer = self.tab_title_font.stack().cloned();
            // Kept titles are keyed by tab position; a closed tab's slot is dropped.
            let title_font_key = self.tab_title_font.key();
            let title_fallback_epoch = self.tab_title_font.fallback_epoch();
            self.chrome_caches.titles.retain_tabs(layout.tabs.len());
            for (position, t) in layout.tabs.iter().enumerate() {
                let Some(tab) = tabs.tabs().get(t.idx) else {
                    // When: `tabs.tabs().get(t.idx)` is None — the layout
                    // outlived a closed tab, so there is no title to draw.
                    continue;
                };
                let active = layout.active == Some(t.idx);
                let active_panel_focused = active && self.window_focused;
                let hovered = hover_tab_idx == t.idx as u32;
                // The content the tab was measured with, so the drawn text matches its width.
                let content = TabContent::of(tab, tab_now, active, process_privileged);
                let show_privilege_badge = content.privileged;
                let text_px = t.title_rect.w
                    - privilege_marker_reserve_px(show_privilege_badge, self.scale_factor);
                let badge_alpha = if source_tab_idx == Some(t.idx) { source_alpha } else { 1.0 };
                if let (Some(stack), Some(rasterizer)) =
                    (self.tab_title_font.stack(), tab_rasterizer.as_mut())
                {
                    let mut color = tab_title_color(
                        tab.custom_color.as_deref(),
                        active,
                        hovered,
                        active_panel_focused,
                        self.tab_active_fg,
                        self.tab_inactive_fg,
                    );
                    if source_tab_idx == Some(t.idx) {
                        color = scale_chrome_text_alpha(color, source_alpha);
                    }
                    let display = content.display_text();
                    let probe = crate::chrome_cache::TitleProbe {
                        text: &display,
                        font_key: title_font_key,
                        fallback_epoch: title_fallback_epoch,
                        raster_px: tab_raster_px,
                        text_px,
                    };
                    // A warm, unchanged title draws from its kept run with no shaping.
                    let draw = self.chrome_caches.titles.prepare(
                        position,
                        &probe,
                        stack,
                        self.chrome_reuse,
                    );
                    let title = self.chrome_caches.titles.drawn(&draw);
                    let placement = tab_title_block_placement(
                        t.title_rect,
                        title.width_px,
                        show_privilege_badge,
                        self.scale_factor,
                    );
                    if let Some(badge) = placement.badge_rect {
                        emit_privilege_badge_quads(
                            &mut *quads,
                            badge,
                            ui_palette.danger,
                            badge_alpha,
                            (sw, sh),
                        );
                    }
                    let title_clip = Some(ChromeClip {
                        x: placement.text_clip.x,
                        y: placement.text_clip.y,
                        w: placement.text_clip.w,
                        h: placement.text_clip.h,
                    });
                    let title_origin = (placement.text_x, tab_baseline_y);
                    let final_layout = crate::chrome_cache::layout_title(
                        stack,
                        rasterizer,
                        &mut self.glyph_atlas,
                        &title,
                        crate::chrome_cache::TitlePlacement {
                            color,
                            raster_px: tab_raster_px,
                            origin: title_origin,
                            screen: (sw, sh),
                            clip: title_clip,
                        },
                    );
                    glyph_instances.extend(final_layout.glyphs);
                    quads.extend(final_layout.missing_boxes);
                } else if show_privilege_badge {
                    // When: `show_privilege_badge` is true without a title font stack, paint the vector warning alone.
                    let placement =
                        tab_title_block_placement(t.title_rect, 0.0, true, self.scale_factor);
                    if let Some(badge) = placement.badge_rect {
                        emit_privilege_badge_quads(
                            &mut *quads,
                            badge,
                            ui_palette.danger,
                            badge_alpha,
                            (sw, sh),
                        );
                    }
                }
            }
        }
        let tab_ink = crate::cursor::glyph_ink_bounds(&glyph_instances[tab_glyph_start..], sw, sh);
        // -------- Search highlights + badge --------------------------------
        if let Some(s) = search {
            // When: `search` is Some — a search session is live, so its match
            // highlights and status badge belong on this frame.
            let cur_idx = s.current;
            let view_top_abs = plan.active_view_top_abs;
            let match_bg = hex_to_premultiplied_rgba(theme.colors.ansi.yellow.0.as_str(), 1.0);
            let match_fg = hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
            let current_bg = hex_to_premultiplied_rgba(theme.colors.bright.green.0.as_str(), 1.0);
            let current_fg = match_fg;
            // Only walk matches whose row intersects the viewport. `matches`
            // is row-sorted, so this is a binary-search-bounded slice — per
            // frame cost is O(visible matches), not O(total matches), which
            // otherwise grows with scrollback depth. `start`
            // keeps `i` aligned with the full slice for the cur_idx compare.
            let (vis_start, vis_end) = s.visible_match_range(view_top_abs, grid.rows);
            for (i, m) in
                s.matches[vis_start..vis_end].iter().enumerate().map(|(j, m)| (vis_start + j, m))
            {
                if u64::from(m.row) < view_top_abs || m.col_end <= m.col_start {
                    // When: the match row is above the viewport, or the span is
                    // empty — neither yields a highlight quad with area.
                    continue;
                }
                let visible_row = u64::from(m.row) - view_top_abs;
                if visible_row >= u64::from(grid.rows) {
                    // When: `visible_row >= grid.rows` — the binary-searched
                    // slice can include a row just past the last visible one.
                    continue;
                }
                // derive x/w from the active-pane snapped-edge
                // cache so match highlights share device-pixel edges
                // with adjacent glyph cells at fractional DPI.
                let cache_end =
                    (m.col_end as usize).min(active_snapped_cell_x.len().saturating_sub(1));
                let cs = (m.col_start as usize).min(cache_end);
                let x = active_snapped_cell_x
                    .get(cs)
                    .copied()
                    .unwrap_or(active_origin_x + f32::from(m.col_start) * self.cell_w);
                let y = active_origin_y + (visible_row as f32) * self.cell_h;
                let w = active_snapped_cell_x
                    .get(cache_end)
                    .map(|r| r - x)
                    .unwrap_or_else(|| f32::from(m.col_end - m.col_start) * self.cell_w);
                let (bg_color, fg_color) = if Some(i) == cur_idx {
                    (current_bg, current_fg)
                } else {
                    // When: `Some(i) != cur_idx` — one of the other matches,
                    // which stays yellow so the current one reads as selected.
                    (match_bg, match_fg)
                };
                // Clip the match highlight to the active pane — a long
                // match that runs past the pane's
                // last column would otherwise paint into the neighbour.
                if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                    (x, y, w, self.cell_h),
                    active_pane_x,
                    active_pane_y,
                    active_pane_w,
                    active_pane_h,
                ) {
                    quads.push(QuadInstance {
                        rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                        color: bg_color,
                        ..Default::default()
                    });
                    // The tab titles were appended after the rows; they lie outside every
                    // recorded row span, so this scan still examines each of them.
                    let RecolorOutcome { visited, .. } = recolor_cursor_glyphs_in(
                        &mut *glyph_instances,
                        row_spans,
                        qx,
                        qy,
                        qw,
                        qh,
                        sw,
                        sh,
                        fg_color,
                    );
                    crate::frame_stats::note_recolor_glyphs_visited(|| visited);
                }
            }
        }

        // -------- Bottom-right search bar (state-only overlay) -------------
        // This is the lightweight "N/M" badge that lives in the corner,
        // distinct from the legacy full-width status bar above. It shows
        // whenever search state exists, so the user has a persistent
        // affordance while typing.
        let read_only_badge = read_only_mode.then(|| {
            // Content width = icon + gap + "READONLY", in the badge's own
            // (DPI-scaled) font, so the badge hugs its text.
            let badge_font = self.raster_px((self.font_size + 2.0).max(1.0));
            let content_w = estimate_badge_text_width(READ_ONLY_BADGE_ICON, badge_font)
                + self.chrome_px(SEARCH_BAR_ICON_GAP)
                + estimate_badge_text_width(READ_ONLY_BADGE_LABEL, badge_font);
            read_only_badge_rect(sw, sh, self.scale_factor, content_w)
        });
        // Field geometry planned this frame; it replaces the presented record only
        // after a `Presented` outcome, so retries and failures never bless it.
        let field_environment = self.field_environment();
        let mut field_candidates = PresentedFields::default();
        let field_selection_bg =
            hex_to_premultiplied_rgba(theme.colors.selection_bg.0.as_str(), 1.0);
        let field_selection_fg =
            hex_to_premultiplied_rgba(theme.colors.selection_fg.0.as_str(), 1.0);
        let search_font_size = self.raster_px(self.font_size.max(1.0));
        // Search preedit is inserted at the query caret for display only; matches still use committed text.
        let search_preedit: &str =
            search.and(ime).map(|i| i.preedit()).filter(|s| !s.is_empty()).unwrap_or("");
        let search_label = search.map(|s| search_bar_label(s, search_preedit));
        // The overlay's icon and label are looked up in the chrome-run cache: a warm overlay with
        // an unchanged label measures and draws them without shaping.
        let search_run_key = crate::chrome_cache::ChromeRunKey::new(
            crate::chrome_cache::ChromeStack::Body,
            ChromeAttrs::default(),
            search_font_size,
            search_font_size,
        );
        let chrome_reuse = self.chrome_reuse;
        let search_gap_px = self.chrome_px(SEARCH_BAR_ICON_GAP);
        let search_content_width = search_label.as_ref().map(|label| {
            let runs = &mut self.chrome_caches.runs;
            let mut measure = self.font_stack.as_ref().map(|stack| {
                move |text: &str| {
                    let handle = runs.prepare(stack, text, search_run_key, chrome_reuse);
                    runs.view(&handle).map(|view| view.raw_width_px())
                }
            });
            search_badge_content_width(
                SEARCH_BADGE_ICON,
                label,
                search_font_size,
                search_gap_px,
                measure.as_mut().map(|measure| measure as &mut dyn FnMut(&str) -> Option<f32>),
            )
        });
        let search_bar_layout = search_content_width.map(|content_w| {
            if read_only_badge.is_some() {
                SearchBarLayout::compute_at_row(sw, sh, content_w, 1, self.scale_factor)
            } else {
                // When: `read_only_badge` is None — no badge occupies row 0, so
                // the search bar takes the default top row.
                SearchBarLayout::compute(sw, sh, content_w, self.scale_factor)
            }
        });
        // When search is active the inline IME preedit must anchor to the
        // current search-box caret, not the terminal cursor.
        // Populated inside the search-render block below; read by the inline
        // preedit block further down. (cx = caret_left, by = box_top, bh =
        // box_height)
        let mut search_ime_anchor: Option<(f32, f32, f32)> = None;
        if let (Some(_), Some(layout)) = (search_label.as_ref(), search_bar_layout) {
            let search_badge_bg =
                hex_to_premultiplied_rgba(theme.colors.ansi.yellow.0.as_str(), 1.0);
            let search_badge_fg = hex_to_chrome_color(theme.colors.background.0.as_str());
            quads_overlay.push(QuadInstance::rounded(
                px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                search_badge_bg,
                [layout.border.w, layout.border.h],
                self.chrome_px(READ_ONLY_BADGE_RADIUS),
            ));
            // Search-badge overlay text → chrome_text into the
            // overlay glyph instance vec (sits above quad_overlay).
            if let (Some(stack), Some(search_state)) = (self.font_stack.as_ref(), search) {
                let mut wt = stack.clone();
                let icon_measure = self.chrome_caches.runs.prepare(
                    stack,
                    SEARCH_BADGE_ICON,
                    search_run_key,
                    chrome_reuse,
                );
                let icon_w = conservative_badge_text_width(
                    estimate_badge_text_width(SEARCH_BADGE_ICON, search_font_size),
                    self.chrome_caches.runs.view(&icon_measure).map(|view| view.raw_width_px()),
                );
                let icon_x = layout.border.x + self.chrome_px(SEARCH_BAR_PAD_LEFT);
                let text_x = icon_x + icon_w + self.chrome_px(SEARCH_BAR_ICON_GAP);
                let visible_w = (layout.border.x + layout.border.w
                    - self.chrome_px(SEARCH_BAR_PAD_RIGHT)
                    - text_x)
                    .max(0.0);
                let caret_h =
                    (search_font_size * 1.15).min((layout.border.h - self.chrome_px(8.0)).max(4.0));
                let caret_y = layout.border.y + (layout.border.h - caret_h) * 0.5;
                // Only the query between the prompt and the counter is selectable.
                let search_text = FieldText::search(search_state, search_preedit);
                let search_clip =
                    FieldRect { x: text_x, y: layout.border.y, w: visible_w, h: layout.border.h };
                let search_placement = FieldPlacement {
                    kind: FieldKind::Search,
                    clip: search_clip,
                    hit_area: search_clip,
                    caret_y,
                    caret_h,
                    caret_fallback_w: (self.cell_w * 0.70).max(4.0),
                    font_size_px: search_font_size,
                    native_em_px: search_font_size,
                };
                // One run, kept or shaped now, feeds caret, highlight, scroll, and the glyphs.
                let label_handle = self.chrome_caches.runs.prepare(
                    stack,
                    &search_text.label,
                    search_run_key,
                    chrome_reuse,
                );
                // The icon draws only on a drawable surface, as `chrome_text::layout` decides.
                let icon_handle = (sw > 0.0 && sh > 0.0).then(|| {
                    self.chrome_caches.runs.prepare(
                        stack,
                        SEARCH_BADGE_ICON,
                        search_run_key,
                        chrome_reuse,
                    )
                });
                let search_run = self.chrome_caches.runs.view(&label_handle);
                if search_run.is_none() {
                    // An unshaped label paints nothing, so every visible character is missing.
                    chrome_text::note_unshaped_chrome(&search_text.label);
                }
                let search_field = search_run.map(|run| {
                    plan_field(
                        search_placement,
                        &FieldBoundaries::from_view(run),
                        &search_text,
                        search_text.caret,
                        search_text.selection.clone(),
                        field_environment,
                        self.presented_fields.search.as_ref(),
                    )
                });
                field_candidates.search = search_field;
                let baseline = layout.border.y + (layout.border.h + search_font_size * 0.8) * 0.5;
                let icon_clip = Some(ChromeClip {
                    x: layout.border.x,
                    y: layout.border.y,
                    w: layout.border.w,
                    h: layout.border.h,
                });
                let icon_view =
                    icon_handle.as_ref().and_then(|handle| self.chrome_caches.runs.view(handle));
                let icon_layout = match icon_view {
                    // The icon run is kept or shaped, so it draws without shaping again.
                    Some(view) => chrome_text::layout_view(
                        view,
                        &mut wt,
                        &mut self.glyph_atlas,
                        search_badge_fg,
                        (icon_x, baseline),
                        (sw, sh),
                        icon_clip,
                        GlyphRasterVariant::Normal,
                    ),
                    // With no surface or no run, the layout decides as it always did.
                    None => chrome_text::layout(
                        stack,
                        &mut wt,
                        &mut self.glyph_atlas,
                        SEARCH_BADGE_ICON,
                        search_badge_fg,
                        ChromeAttrs::default(),
                        search_font_size,
                        search_font_size,
                        (icon_x, baseline),
                        (sw, sh),
                        icon_clip,
                    ),
                };
                overlay_glyph_instances.extend(icon_layout.glyphs);
                quads_overlay.extend(icon_layout.missing_boxes);
                let label_start = overlay_glyph_instances.len();
                // Tofu outlines of the label; drawn after the selection and caret quads.
                let mut field_tofu: Vec<QuadInstance> = Vec::new();
                if let (Some(run), Some(field)) = (search_run, search_field) {
                    // The label paints from the run its field geometry was measured on.
                    let chrome_layout = chrome_text::layout_view(
                        run,
                        &mut wt,
                        &mut self.glyph_atlas,
                        search_badge_fg,
                        (field.text_x, baseline),
                        (sw, sh),
                        Some(ChromeClip {
                            x: text_x,
                            y: layout.border.y,
                            w: visible_w,
                            h: layout.border.h,
                        }),
                        GlyphRasterVariant::Normal,
                    );
                    overlay_glyph_instances.extend(chrome_layout.glyphs);
                    field_tofu = chrome_layout.missing_boxes;
                }
                // Layout only culls whole glyphs; scrolled glyphs crossing the edge are trimmed here.
                clip_glyphs_to_rect(
                    &mut *overlay_glyph_instances,
                    label_start,
                    search_clip,
                    sw,
                    sh,
                );

                if let Some(field) = search_field {
                    // The label shaped, so caret and highlight come from its measured clusters.
                    search_ime_anchor = Some((field.caret.x, layout.border.y, layout.border.h));
                    let mut marks = Vec::new();
                    if let Some(highlight) = field.selection {
                        // A selected range uses the theme selection pair, which keeps it
                        // legible on the yellow badge and distinct from the inverted caret.
                        marks.push(crate::cursor::FieldMark {
                            rect: (highlight.x, highlight.y, highlight.w, highlight.h),
                            background: field_selection_bg,
                            foreground: field_selection_fg,
                        });
                    }
                    // The badge is already cursor-yellow, so invert locally: a
                    // theme-background block with the covered glyph recolored to
                    // badge yellow. The block overlays existing text and contributes
                    // no advance, matching terminal and command-palette cursors.
                    let caret = field.caret;
                    if caret.w > 0.0 && caret.h > 0.0 {
                        // A field too small to show any caret clips it to zero area,
                        // and then no block is drawn.
                        marks.push(crate::cursor::FieldMark {
                            rect: (caret.x, caret.y, caret.w, caret.h),
                            background: chrome_color_to_linear_rgba(search_badge_fg),
                            foreground: search_badge_bg,
                        });
                    }
                    crate::cursor::paint_field_marks(
                        &mut *quads_overlay,
                        &mut overlay_glyph_instances[label_start..],
                        std::mem::take(&mut field_tofu),
                        &marks,
                        sw,
                        sh,
                    );
                }
                // A label without a field geometry has no marks; its tofu, if any, still draws.
                quads_overlay.extend(field_tofu);
            }
        }

        if let Some((badge_x, badge_y, badge_w, badge_h)) = read_only_badge {
            // When: `read_only_badge` is Some — copy mode is read-only, so the
            // badge announces that typing will not reach the shell.
            let badge_bg = hex_to_premultiplied_rgba(theme.colors.bright.green.0.as_str(), 1.0);
            quads_overlay.push(QuadInstance::rounded(
                px_to_ndc(badge_x, badge_y, badge_w, badge_h, sw, sh),
                badge_bg,
                [badge_w, badge_h],
                self.chrome_px(READ_ONLY_BADGE_RADIUS),
            ));
            if let Some(stack) = self.font_stack.as_ref() {
                // When: `font_stack` is Some — the badge quad is already
                // pushed; without a shaper it draws as a bare rounded rect.
                let native_em = stack
                    .cell_metrics_raster_px()
                    .ok()
                    .map(|m| m.cell_h as f32)
                    .unwrap_or(self.cell_h);
                let mut wt = stack.clone();
                let font_size = self.raster_px((self.font_size + 2.0).max(1.0));
                let text_color = hex_to_chrome_color(theme.colors.background.0.as_str());
                let baseline = badge_y
                    + (badge_h + font_size * 0.8) * 0.5
                    + self.chrome_px(READ_ONLY_BADGE_BASELINE_NUDGE_Y);
                // Pre-scale chrome paddings into locals BEFORE the
                // `&mut self.glyph_atlas` borrow below, so we don't borrow
                // `*self` immutably (chrome_px) while it's mutably borrowed.
                let badge_pad_left = self.chrome_px(SEARCH_BAR_PAD_LEFT);
                let badge_pad_right = self.chrome_px(READ_ONLY_BADGE_PAD_RIGHT);
                let icon_layout = chrome_text::layout(
                    stack,
                    &mut wt,
                    &mut self.glyph_atlas,
                    READ_ONLY_BADGE_ICON,
                    text_color,
                    ChromeAttrs { bold: true, italic: false },
                    font_size,
                    native_em,
                    (badge_x + badge_pad_left, baseline),
                    (sw, sh),
                    Some(ChromeClip { x: badge_x, y: badge_y, w: badge_w, h: badge_h }),
                );
                overlay_glyph_instances.extend(icon_layout.glyphs);
                quads_overlay.extend(icon_layout.missing_boxes);
                // Place the label immediately after the lock icon (no big
                // right-aligned gap): icon_x + icon width + the icon gap.
                let label_x = badge_x
                    + badge_pad_left
                    + icon_layout.width_px
                    + self.chrome_px(SEARCH_BAR_ICON_GAP);
                let _ = badge_pad_right;
                quads_overlay.extend(emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    font_size,
                    native_em,
                    &mut wt,
                    READ_ONLY_BADGE_LABEL,
                    text_color,
                    ChromeAttrs { bold: true, italic: false },
                    label_x,
                    baseline,
                    [badge_x, badge_y, badge_w, badge_h],
                    sw,
                    sh,
                    &mut *overlay_glyph_instances,
                    None,
                ));
                quads_overlay.extend(emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    font_size,
                    native_em,
                    &mut wt,
                    READ_ONLY_BADGE_LABEL,
                    text_color,
                    ChromeAttrs { bold: true, italic: false },
                    label_x + 1.0,
                    baseline,
                    [badge_x, badge_y, badge_w, badge_h],
                    sw,
                    sh,
                    &mut *overlay_glyph_instances,
                    None,
                ));
            }
        }

        if let Some(bubble) = notification {
            let notification_font_size = self.raster_px(self.font_size.max(1.0));
            let row = u8::from(read_only_badge.is_some()) + u8::from(search_bar_layout.is_some());
            let text_layout = self.notification_layout(&bubble.message, (sw, sh), row);
            let layout = text_layout.geometry;
            let bg_hex = match bubble.level {
                NotificationLevel::Info => theme.colors.bright.green.0.as_str(),
                NotificationLevel::Warning => theme.colors.ansi.yellow.0.as_str(),
                NotificationLevel::Error => theme.colors.bright.red.0.as_str(),
            };
            let bubble_bg = hex_to_premultiplied_rgba(bg_hex, 1.0);
            let bubble_fg = hex_to_chrome_color(theme.colors.background.0.as_str());
            quads_overlay.push(QuadInstance::rounded(
                px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                bubble_bg,
                [layout.border.w, layout.border.h],
                self.chrome_px(READ_ONLY_BADGE_RADIUS),
            ));
            if let Some(stack) = self.font_stack.as_ref() {
                let mut wt = stack.clone();
                let text_x = layout.border.x + text_layout.padding;
                // Glyph ink may extend beyond its shaped advance into the trailing padding.
                let text_clip_w = (layout.close.x - text_x).max(0.0);
                let baseline = layout.border.y + text_layout.padding + notification_font_size;
                for (index, line) in text_layout.lines.iter().enumerate() {
                    quads_overlay.extend(emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        notification_font_size,
                        notification_font_size,
                        &mut wt,
                        line,
                        bubble_fg,
                        ChromeAttrs::default(),
                        text_x,
                        baseline + index as f32 * text_layout.line_height,
                        [text_x, layout.border.y, text_clip_w, layout.border.h],
                        sw,
                        sh,
                        &mut *overlay_glyph_instances,
                        None,
                    ));
                }
                let close_w =
                    estimate_badge_text_width(NOTIFICATION_CLOSE_ICON, notification_font_size);
                let close_x = layout.close.x + (layout.close.w - close_w) * 0.5;
                quads_overlay.extend(emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    notification_font_size,
                    notification_font_size,
                    &mut wt,
                    NOTIFICATION_CLOSE_ICON,
                    bubble_fg,
                    ChromeAttrs::default(),
                    close_x,
                    baseline,
                    [layout.close.x, layout.close.y, layout.close.w, layout.close.h],
                    sw,
                    sh,
                    &mut *overlay_glyph_instances,
                    None,
                ));
            }
        }

        if let (Some(preview), Some(stack)) = (link_preview, self.font_stack.as_ref()) {
            use sonicterm_render_model::boundary::ui::overlays::{
                link_preview_text, LinkPreviewLayout,
            };
            let text = link_preview_text(&preview.uri);
            let font_size = self.raster_px(self.font_size.max(1.0));
            let layout = LinkPreviewLayout::compute(
                &text,
                preview.pointer,
                (sw, sh),
                font_size * 1.4,
                self.scale_factor,
                |value| {
                    conservative_badge_text_width(
                        estimate_badge_text_width(value, font_size),
                        crate::frame_stats::shape_request(|| {
                            stack.measure_text_width_for_frame(value)
                        })
                        .ok(),
                    )
                },
            );
            if let Some(layout) = layout {
                let chrome = self.chrome_caches.palette.palette_for(theme);
                let rect = layout.border;
                quads_overlay.push(QuadInstance::rounded(
                    px_to_ndc(rect.x, rect.y, rect.w, rect.h, sw, sh),
                    chrome.bg_surface,
                    [rect.w, rect.h],
                    self.chrome_px(4.0),
                ));
                let color = hex_to_chrome_color(theme.colors.foreground.0.as_str());
                let mut raster = stack.clone();
                for (index, line) in layout.lines.iter().enumerate() {
                    quads_overlay.extend(emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        font_size,
                        font_size,
                        &mut raster,
                        line,
                        color,
                        ChromeAttrs::default(),
                        rect.x + layout.padding,
                        rect.y + layout.padding + font_size + index as f32 * layout.line_height,
                        [rect.x, rect.y, rect.w, rect.h],
                        sw,
                        sh,
                        &mut *overlay_glyph_instances,
                        None,
                    ));
                }
            }
        }

        // -------- Command palette overlay ----------------------------------
        let palette_preedit = ime.map(|i| i.preedit()).unwrap_or("");
        let (palette_layout, palette_field_text) = if let Some(p) = palette {
            // The query text and caret come from the field display, so a literal bar glyph stays text.
            let field_text = FieldText::palette(p, palette_preedit);
            let layout = PaletteLayout::compute(p, sw, sh, self.panel_padding, self.scale_factor);
            (layout, field_text)
        } else {
            // When: `palette` is None — the command palette is closed, so no
            // layout, query, or caret exists for the overlay pass to draw.
            (None, None)
        };
        // Chrome colors are derived from the active theme so the palette
        // tracks the user's chosen palette instead of hardcoded
        // Tokyo Night literals (see UiPalette::from_theme).
        if let Some(layout) = &palette_layout {
            // When: `palette_layout` is Some — the palette is open, so its
            // panel, query row, and result rows all need chrome this frame.
            let palette_chrome = self.chrome_caches.palette.palette_for(theme);
            let accent_rgba = palette_chrome.accent;
            // Full-window scrim — sits below the modal so the underlying
            // terminal recedes visually.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.scrim.x,
                    layout.scrim.y,
                    layout.scrim.w,
                    layout.scrim.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.scrim,
                ..Default::default()
            });
            // Outer 1px border. Rounded radius 16 per spec — the border
            // sits 1px outside `bg`, so its radius equals the panel's
            // plus the border thickness.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.border_subtle,
                size_px: [layout.border.w, layout.border.h],
                radius_px: PALETTE_PANEL_RADIUS + PALETTE_BORDER,
                ..Default::default()
            });
            // Modal background. Rounded radius 16 per spec.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: palette_chrome.bg_elevated,
                size_px: [layout.bg.w, layout.bg.h],
                radius_px: PALETTE_PANEL_RADIUS,
                ..Default::default()
            });
            // Query field background. Slightly smaller radius than the
            // panel reads as nested chrome.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.query_row.x,
                    layout.query_row.y,
                    layout.query_row.w,
                    layout.query_row.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.bg_base,
                size_px: [layout.query_row.w, layout.query_row.h],
                radius_px: PALETTE_QUERY_RADIUS,
                ..Default::default()
            });
            // Selected row highlight — theme accent at low alpha.
            if let Some(sel) = layout.selected_row {
                if let Some(row) = layout.rows.get(sel) {
                    quads_overlay.push(QuadInstance {
                        rect: px_to_ndc(row.rect.x, row.rect.y, row.rect.w, row.rect.h, sw, sh),
                        color: with_premultiplied_alpha(accent_rgba, 0.16),
                        size_px: [row.rect.w, row.rect.h],
                        radius_px: PALETTE_ROW_RADIUS,
                        ..Default::default()
                    });
                }
            }
            // Footer top border — 1px line at the top edge of the footer
            // rect. Kept sharp; a 1px hairline doesn't benefit from
            // SDF rounding.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.footer.x, layout.footer.y, layout.footer.w, 1.0, sw, sh),
                color: palette_chrome.border_subtle,
                ..Default::default()
            });
            // Shape the query row text. The renderer paints either the
            // placeholder (empty query) or the typed text + cursor.
            //
            // emit through the SonicTerm glyph atlas at device pixel
            // scale (mirrors `emit_tab_title_glyphs`) so the palette text
            // is crisp on HiDPI. The previous glyphon TextRenderer path
            // bypassed the DPI multiplier and rendered blurry on Windows.
            // An editable query paints its field display. An empty query paints the
            // placeholder and the colour picker paints its title; neither is selectable.
            let (paint_text, paint_caret, paint_selection) = match &palette_field_text {
                Some(text) if !text.label.is_empty() => {
                    (text.label.clone(), text.caret, text.selection.clone())
                }
                Some(_) => (layout.query_placeholder.clone().unwrap_or_default(), 0, None),
                None => (layout.query_label.clone(), layout.query_label.len(), None),
            };
            let title_text;
            let plan_text: &FieldText = if let Some(text) = palette_field_text.as_ref() {
                text
            } else {
                // When: `palette_field_text` is None — the colour picker title gets a
                // display-only record that places its end caret.
                title_text = FieldText {
                    kind: FieldKind::Palette,
                    label: paint_text.clone(),
                    content: 0..0,
                    caret: paint_text.len(),
                    selection: None,
                    composing: false,
                    variant: u8::MAX,
                };
                &title_text
            };
            let palette_font_size = self.raster_px(self.font_size);
            // Chrome text needs a wezterm FontStack; when one
            // isn't available (test fixtures), the palette quads still
            // render but no text is emitted. Wrap the entire chrome
            // emission in an `if let Some(...)` so the palette path
            // degrades gracefully instead of panicking.
            if let Some(stack) = self.font_stack.as_ref() {
                // When: `font_stack` is Some — palette quads are already
                // pushed; without a shaper the panel draws with no text.
                let palette_native_em = self.raster_px(self.font_size);
                let mut palette_rasterizer = stack.clone();
                // Query: vertically centre inside the query_row chrome.
                let query_origin_x = layout.query_row.x
                    + self.chrome_px(
                        sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                    );
                let query_baseline_y =
                    layout.query_row.y + (layout.query_row.h + palette_font_size * 0.8) * 0.5;
                let caret_h =
                    (palette_font_size * 1.15).min(layout.query_row.h - self.chrome_px(8.0));
                let caret_y = layout.query_row.y + (layout.query_row.h - caret_h) * 0.5;
                let query_pad_x = query_origin_x - layout.query_row.x;
                let query_clip = FieldRect {
                    x: query_origin_x,
                    y: layout.query_row.y,
                    w: (layout.query_row.w - query_pad_x * 2.0).max(0.0),
                    h: layout.query_row.h,
                };
                let query_row_rect = FieldRect {
                    x: layout.query_row.x,
                    y: layout.query_row.y,
                    w: layout.query_row.w,
                    h: layout.query_row.h,
                };
                let palette_placement = FieldPlacement {
                    kind: FieldKind::Palette,
                    clip: query_clip,
                    hit_area: query_row_rect,
                    caret_y,
                    caret_h,
                    caret_fallback_w: (self.cell_w * 0.70).max(4.0),
                    font_size_px: palette_font_size,
                    native_em_px: palette_native_em,
                };
                // One shaped run feeds caret, highlight, scroll, and the painted glyphs.
                let palette_run = chrome_text::ChromeShapedRun::shape(
                    stack,
                    &paint_text,
                    ChromeAttrs::default(),
                    palette_font_size,
                    palette_native_em,
                );
                if palette_run.is_none() {
                    // An unshaped query paints nothing, so every visible character is missing.
                    chrome_text::note_unshaped_chrome(&paint_text);
                }
                let palette_field = palette_run.as_ref().map(|run| {
                    plan_field(
                        palette_placement,
                        &FieldBoundaries::from_run(run),
                        plan_text,
                        paint_caret,
                        paint_selection,
                        field_environment,
                        self.presented_fields.palette.as_ref(),
                    )
                });
                if palette_field_text.is_some() {
                    // Only an editable query's presented geometry serves pointer and IME.
                    field_candidates.palette = palette_field;
                }
                let query_start = overlay_glyph_instances.len();
                // Tofu outlines of the query; drawn after the selection and caret quads.
                let mut query_tofu: Vec<QuadInstance> = Vec::new();
                if let (Some(run), Some(field)) = (palette_run.as_ref(), palette_field) {
                    // The query paints from the run its field geometry was measured on.
                    let query_layout = chrome_text::layout_prepared(
                        run,
                        &mut palette_rasterizer,
                        &mut self.glyph_atlas,
                        self.search_fg,
                        (field.text_x, query_baseline_y),
                        (sw, sh),
                        Some(ChromeClip {
                            x: query_clip.x,
                            y: query_clip.y,
                            w: query_clip.w,
                            h: query_clip.h,
                        }),
                        GlyphRasterVariant::Normal,
                    );
                    overlay_glyph_instances.extend(query_layout.glyphs);
                    query_tofu = query_layout.missing_boxes;
                }
                // Layout only culls whole glyphs; a scrolled glyph crossing the edge is trimmed here.
                clip_glyphs_to_rect(&mut *overlay_glyph_instances, query_start, query_clip, sw, sh);
                if let Some(field) = palette_field {
                    // The run shaped, so caret and highlight use its measured clusters.
                    let mut marks = Vec::new();
                    if let Some(highlight) = field.selection {
                        // A selected range paints under the glyphs with selection colors.
                        marks.push(crate::cursor::FieldMark {
                            rect: (highlight.x, highlight.y, highlight.w, highlight.h),
                            background: field_selection_bg,
                            foreground: field_selection_fg,
                        });
                    }
                    let caret = field.caret;
                    if caret.w > 0.0 && caret.h > 0.0 {
                        // A query row too small to show any caret clips it to zero area,
                        // and then no block is drawn.
                        marks.push(crate::cursor::FieldMark {
                            rect: (caret.x, caret.y, caret.w, caret.h),
                            background: self.cursor_color,
                            foreground: self.cursor_text_color,
                        });
                    }
                    crate::cursor::paint_field_marks(
                        &mut *quads_overlay,
                        &mut overlay_glyph_instances[query_start..],
                        std::mem::take(&mut query_tofu),
                        &marks,
                        sw,
                        sh,
                    );
                }
                // A query without a field geometry has no marks; its tofu, if any, still draws.
                quads_overlay.extend(query_tofu);

                // Rows: emit each visible row label as its own line so the
                // baseline aligns with the row's highlight quad.
                let bounds_bg = [layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h];
                for (i, label) in layout.row_labels.iter().enumerate() {
                    let Some(row) = layout.rows.get(i) else {
                        // When: `layout.rows.get(i)` is None — labels and rects
                        // are parallel vectors that can disagree in length.
                        continue;
                    };
                    let shortcut = layout.row_shortcuts.get(i).and_then(|hint| hint.as_deref());
                    let detail = layout.row_details.get(i).and_then(|text| text.as_deref());
                    let disabled = layout.row_disabled.get(i).copied().unwrap_or(false);
                    let mut label_color = self.search_fg;
                    if disabled {
                        label_color.a = 150;
                    }
                    let swatch = layout.row_swatches.get(i).and_then(|v| v.as_deref());
                    let shortcut_font_size = palette_font_size;
                    let shortcut_w = shortcut
                        .map(|hint| hint.chars().count() as f32 * shortcut_font_size * 0.62);
                    let mut origin_x = row.rect.x
                        + self.chrome_px(
                            sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                        );
                    if let Some(hex) = swatch {
                        let color = hex_to_premultiplied_rgba(hex, 1.0);
                        let line_h = self.chrome_px(2.0).max(1.0);
                        quads_overlay.push(QuadInstance::sharp(
                            px_to_ndc(row.rect.x, row.rect.y, row.rect.w, line_h, sw, sh),
                            color,
                        ));
                        let size = (row.rect.h * 0.55).max(8.0);
                        let swatch_x = origin_x;
                        let swatch_y = row.rect.y + (row.rect.h - size) * 0.5;
                        quads_overlay.push(QuadInstance::rounded(
                            px_to_ndc(swatch_x, swatch_y, size, size, sw, sh),
                            color,
                            [size, size],
                            size * 0.25,
                        ));
                        origin_x += size + self.chrome_px(8.0);
                    }
                    // Center the combined text block in raster pixels so both outer margins scale together.
                    let detail_font_size = self.raster_px(palette_footer_font_size(self.font_size));
                    let (label_baseline, detail_top, detail_baseline) = palette_detail_positions(
                        row.rect.h,
                        palette_font_size,
                        detail_font_size,
                        self.chrome_px(4.0),
                    );
                    let baseline_y = row.rect.y
                        + if detail.is_some() {
                            label_baseline
                        } else {
                            // When: detail is absent, retain the single-line row baseline.
                            (row.rect.h + palette_font_size * 0.8) * 0.5
                        };
                    let label_clip_h = if detail.is_some() {
                        detail_top.min(row.rect.h)
                    } else {
                        // When: detail is absent, the label owns the full row clip.
                        row.rect.h
                    };
                    let label_bounds_w = match shortcut_w {
                        Some(w) => (row.rect.w
                            - w
                            - self.chrome_px(sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X) * 2.0
                            - self.chrome_px(sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_COLUMN_GAP))
                        .max(0.0),
                        None => row.rect.w,
                    };
                    quads_overlay.extend(emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        palette_font_size,
                        palette_native_em,
                        &mut palette_rasterizer,
                        label,
                        label_color,
                        ChromeAttrs::default(),
                        origin_x,
                        baseline_y,
                        [row.rect.x, row.rect.y, label_bounds_w, label_clip_h],
                        sw,
                        sh,
                        &mut *overlay_glyph_instances,
                        None,
                    ));
                    if let (Some(hint), Some(width)) = (shortcut, shortcut_w) {
                        let hint_origin_x = row.rect.x + row.rect.w
                            - self.chrome_px(
                                sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                            )
                            - width;
                        let mut hint_color = self.search_fg;
                        hint_color.a = if disabled { 120 } else { 165 };
                        quads_overlay.extend(emit_overlay_text_glyphs(
                            &mut self.glyph_atlas,
                            stack,
                            shortcut_font_size,
                            palette_native_em,
                            &mut palette_rasterizer,
                            hint,
                            hint_color,
                            ChromeAttrs { bold: false, italic: true },
                            hint_origin_x,
                            baseline_y,
                            [row.rect.x, row.rect.y, row.rect.w, row.rect.h],
                            sw,
                            sh,
                            &mut *overlay_glyph_instances,
                            None,
                        ));
                    }
                    if let (Some(detail), Some(detail_stack)) =
                        (detail, self.palette_footer_font_stack.as_ref())
                    {
                        // Details share the footer's smaller native strike and atlas identity.
                        let mut detail_rasterizer = detail_stack.clone();
                        let mut detail_color = self.search_fg;
                        detail_color.a = if disabled { 120 } else { 165 };
                        let detail_width = (row.rect.x + row.rect.w
                            - origin_x
                            - self.chrome_px(
                                sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                            ))
                        .max(0.0);
                        let detail_layout = chrome_text::layout_with_raster_variant(
                            detail_stack,
                            &mut detail_rasterizer,
                            &mut self.glyph_atlas,
                            detail,
                            detail_color,
                            ChromeAttrs::default(),
                            detail_font_size,
                            detail_font_size,
                            (origin_x, row.rect.y + detail_baseline),
                            (sw, sh),
                            Some(ChromeClip {
                                x: origin_x,
                                y: row.rect.y + detail_top,
                                w: detail_width,
                                h: (row.rect.h - detail_top).max(0.0),
                            }),
                            GlyphRasterVariant::PaletteFooter,
                        );
                        overlay_glyph_instances.extend(detail_layout.glyphs);
                        quads_overlay.extend(detail_layout.missing_boxes);
                    }
                }
                // Empty-state placeholder + hint.
                if let Some(ph) = &layout.empty_label {
                    let empty_x = layout.bg.x
                        + self.chrome_px(self.panel_padding)
                        + self.chrome_px(
                            sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                        );
                    let empty_y_top = layout.query_row.y
                        + layout.query_row.h
                        + self.chrome_px(self.panel_padding);
                    // No row rect here (empty state), so derive the scaled row
                    // height via chrome_px — same DPI basis as the row path. #palette
                    let empty_row_h = self.chrome_px(
                        sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_HEIGHT,
                    );
                    let empty_baseline_y =
                        empty_y_top + (empty_row_h + palette_font_size * 0.8) * 0.5;
                    quads_overlay.extend(emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        palette_font_size,
                        palette_native_em,
                        &mut palette_rasterizer,
                        ph,
                        self.search_fg,
                        ChromeAttrs::default(),
                        empty_x,
                        empty_baseline_y,
                        bounds_bg,
                        sw,
                        sh,
                        &mut *overlay_glyph_instances,
                        None,
                    ));
                    if let Some(hint) = &layout.empty_hint {
                        let hint_baseline_y = empty_baseline_y
                            + sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_HEIGHT
                            + sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_GAP;
                        quads_overlay.extend(emit_overlay_text_glyphs(
                            &mut self.glyph_atlas,
                            stack,
                            palette_font_size,
                            palette_native_em,
                            &mut palette_rasterizer,
                            hint,
                            self.search_fg,
                            ChromeAttrs::default(),
                            empty_x,
                            hint_baseline_y,
                            bounds_bg,
                            sw,
                            sh,
                            &mut *overlay_glyph_instances,
                            None,
                        ));
                    }
                }

                if let Some(footer_stack) = self.palette_footer_font_stack.as_ref() {
                    let footer_font_size = self.raster_px(palette_footer_font_size(self.font_size));
                    let footer_native_em = footer_font_size;
                    let mut footer_rasterizer = footer_stack.clone();
                    let footer_origin_x = layout.footer.x + self.chrome_px(PALETTE_FOOTER_INSET_X);
                    let footer_baseline_y =
                        layout.footer.y + (layout.footer.h + footer_font_size * 0.8) * 0.5;
                    let mut footer_color = self.search_fg;
                    footer_color.a = 165;
                    let footer_width =
                        (layout.footer.w - self.chrome_px(PALETTE_FOOTER_INSET_X) * 2.0).max(0.0);
                    let footer_layout = chrome_text::layout_with_raster_variant(
                        footer_stack,
                        &mut footer_rasterizer,
                        &mut self.glyph_atlas,
                        &layout.footer_label,
                        footer_color,
                        ChromeAttrs::default(),
                        footer_font_size,
                        footer_native_em,
                        (footer_origin_x, footer_baseline_y),
                        (sw, sh),
                        Some(ChromeClip {
                            x: footer_origin_x,
                            y: layout.footer.y,
                            w: footer_width,
                            h: layout.footer.h,
                        }),
                        GlyphRasterVariant::PaletteFooter,
                    );
                    overlay_glyph_instances.extend(footer_layout.glyphs);
                    quads_overlay.extend(footer_layout.missing_boxes);
                }
            }
        }

        // Inline IME preedit at the TERMINAL CURSOR (WezTerm-style): macOS does
        // NOT draw the in-flight composition for a terminal, so the app must.
        // When SEARCH is active the preedit is instead spliced into the search
        // label (see search_bar_label above) and rendered as part of that
        // string — so we skip the self-drawn overlay here to avoid drawing it
        // twice / overlapping the ` · N/M` suffix. This block only handles the
        // terminal-cursor case. Per-frame overlay (not row-cached); the
        // FrameKey hashes `i.preedit()` so composition changes re-render.
        let search_active = search_ime_anchor.is_some();
        let palette_active = palette_layout.is_some();
        if !search_active
            && !palette_active
            && ime.map(|i| preedit_has_visible_ink(i.preedit())).unwrap_or(false)
        {
            // When: `!search_active && !palette_active` and the preedit has ink
            // — the other two anchor their own caret, leaving the cursor case.
            if let (Some(i), Some(stack)) = (ime, self.font_stack.as_ref()) {
                // When: both `ime` and `font_stack` are Some — composing text
                // needs a shaper; without one no preedit glyphs are emitted.
                let text = i.preedit();
                // Body-matched, DPI-scaled font size (same as terminal text).
                let font_size = self.raster_px(self.font_size);
                let start_x = active_snapped_cell_x
                    .get(grid.cursor.col as usize)
                    .copied()
                    .unwrap_or(active_origin_x + f32::from(grid.cursor.col) * self.cell_w);
                let top_y = active_origin_y + f32::from(grid.cursor.row) * self.cell_h;
                let line_h = self.cell_h;
                // Per-char advance estimate mirrors the body/badge text path.
                let pre_w = estimate_badge_text_width(text, font_size).max(self.cell_w);
                let clip_to_pane = true;

                // (0) Opaque background behind the composing run.
                //
                // The inline preedit is drawn over whatever the app already
                // painted in these cells. When an app shows placeholder/hint
                // text at an empty input (e.g. Claude Code's `Try "edit …"`),
                // the first CJK char's in-flight pinyin would otherwise layer
                // on top of that hint and both become illegible. Lay down the
                // plain terminal `bg` first so the composing glyphs sit on a
                // clean surface — mirroring what the search-bar preedit path
                // already does. This is NOT a highlight color (preserving the
                // "no highlight background" preference); it only
                // masks the cells the preedit actually occupies. Pushed to
                // `quads_overlay`, which draws beneath `overlay_glyph_instances`
                // (see `draw_layers` order), so it never covers the glyphs.
                {
                    // Cover the full preedit footprint: from the cell start
                    // (start_x) across `pre_w` plus the small `text_pad` nudge
                    // the glyphs are shifted right by (emit_x = start_x +
                    // text_pad). Using the same width the glyphs use keeps the
                    // mask matched to the run without bleeding onto adjacent
                    // cells.
                    let pad = self.chrome_px(2.0);
                    let bg_rect = preedit_bg_rect(start_x, top_y, pre_w, pad, line_h);
                    if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                        bg_rect,
                        active_pane_x,
                        active_pane_y,
                        active_pane_w,
                        active_pane_h,
                    ) {
                        quads_overlay.push(QuadInstance {
                            rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                            color: self.bg_rgba,
                            ..Default::default()
                        });
                    }
                }

                // Keep `font_size_px == native_em_px`: line spacing in `cell_h` would shrink preedit below the body text size.
                let native_em = font_size;
                let mut wt = stack.clone();
                let text_pad = self.chrome_px(2.0);
                let baseline_y = top_y + (line_h + font_size * 0.8) * 0.5;
                let preedit_fg = self.search_fg;
                // Preedit UVs are reusable only on the same device, allocation and atlas contents.
                let emit_x = start_x + text_pad;
                let color_bits = (u32::from(preedit_fg.r) << 24)
                    | (u32::from(preedit_fg.g) << 16)
                    | (u32::from(preedit_fg.b) << 8)
                    | u32::from(preedit_fg.a);
                let atlas_stamp = self.glyph_atlas_stamp();
                let cache_hit = self.preedit_glyph_cache.as_ref().is_some_and(|c| {
                    c.matches(text, font_size, emit_x, baseline_y, color_bits, atlas_stamp)
                });
                if cache_hit {
                    // The qualified content stamp rejects UVs from a reset or replaced atlas.
                    let cached = self.preedit_glyph_cache.as_ref().unwrap();
                    overlay_glyph_instances.extend(cached.glyphs.iter().copied());
                    quads_overlay.extend(cached.missing_boxes.iter().copied());
                    for &missing in &cached.missing_chrome_chars {
                        // A hit draws the cached tofu without a layout, so it is noted here.
                        chrome_text::note_missing_chrome(missing);
                    }
                } else {
                    // When: `!cache_hit` — text, placement, colour, or atlas
                    // epoch changed, so the run is re-shaped and re-cached.
                    let before = overlay_glyph_instances.len();
                    // A nested scope captures the run's tofu for the cache; it is noted again
                    // into the frame's scope below.
                    let preedit_scope = chrome_text::MissingChromeScope::enter();
                    let preedit_boxes = emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        font_size,
                        native_em,
                        &mut wt,
                        text,
                        preedit_fg,
                        ChromeAttrs::default(),
                        emit_x,
                        baseline_y,
                        [start_x, top_y, pre_w, line_h],
                        sw,
                        sh,
                        &mut *overlay_glyph_instances,
                        None,
                    );
                    quads_overlay.extend(preedit_boxes.iter().copied());
                    let preedit_missing = preedit_scope.finish();
                    for &missing in &preedit_missing {
                        chrome_text::note_missing_chrome(missing);
                    }
                    // The frame-end stamp check rejects and clears this cache if emission recycled any UVs.
                    self.preedit_glyph_cache = Some(PreeditGlyphCache {
                        text: text.to_string(),
                        font_size,
                        start_x: emit_x,
                        top_y: baseline_y,
                        color_bits,
                        atlas_stamp: self.glyph_atlas_stamp(),
                        glyphs: overlay_glyph_instances[before..].to_vec(),
                        missing_boxes: preedit_boxes,
                        missing_chrome_chars: preedit_missing,
                    });
                }

                // (3) NO underline under the composing run.
                //
                // macOS routes ordinary typing through IME preedit whenever a
                // CJK/Pinyin input source is active (even plain Latin romaji),
                // so a per-keystroke composing underline reads as a stray
                // cursor-colored bar that flashes and "follows the cursor" as
                // you type (user-reported). The committed text is unaffected;
                // the in-flight glyphs above already show what is being
                // composed, so the underline added noise without information.
                // Drawn intentionally as nothing — keep the block for the
                // `clip_to_pane`/geometry context the glyph emit above uses.
                let _ = (clip_to_pane, pre_w);
            }
        }

        // Drag-chip overlay: translucent ~120×24 quad that follows the
        // cursor while a tab is held. Drawn AFTER ime/search so it
        // sits on top of everything.
        if let Some(chip) = self.drag_chip.clone() {
            const CHIP_W: f32 = 120.0;
            const CHIP_H: f32 = 24.0;
            // Two independent multipliers compose here.
            // `chip.scale` is the tear-out ANIMATION ease (1.0 in-bar, 1.02
            // on tear); `dpi` is the display scale factor. The chip's logical
            // size + decorations must scale by DPI so it keeps a constant
            // physical size across displays. `top_left` is already in physical
            // px (cursor-relative, from the app layer) so it is NOT scaled —
            // only the size and the size-derived centering offset are.
            let dpi = self.scale_factor;
            let scale = chip.scale.clamp(0.5, 2.0) * dpi;
            let w = CHIP_W * scale;
            let h = CHIP_H * scale;
            // Re-center the scaled chip so growth is centered around
            // the original anchor point (cursor-relative offset is
            // preserved by the caller in `top_left`).
            let cx = chip.top_left.0 + CHIP_W * 0.5 * dpi;
            let cy = chip.top_left.1 + CHIP_H * 0.5 * dpi;
            let x0 = cx - w * 0.5;
            let y0 = cy - h * 0.5;

            // Soft drop shadow: stack two dimmer quads with growing
            // offset to fake an 8px blur without a fragment shader.
            // Offsets are logical px → scale by DPI.
            for (off, alpha) in [(2.0_f32, 0.18_f32), (4.0_f32, 0.10_f32), (8.0_f32, 0.05_f32)] {
                let off = off * dpi;
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(x0 + off, y0 + off, w, h, sw, sh),
                    color: [0.0, 0.0, 0.0, alpha],
                    ..Default::default()
                });
            }

            // Drop-line indicator (in-bar reorder cue). Drawn BEFORE
            // the chip so the chip floats on top if they overlap.
            if let Some(lx) = chip.drop_line_x {
                let (ly0, ly1) = chip.drop_line_y;
                let lh = (ly1 - ly0).max(2.0 * dpi);
                // Drop-line accent — theme-driven (was hardcoded ACCENT_BLUE).
                let line_color = with_premultiplied_alpha(
                    self.chrome_caches.palette.palette_for(theme).accent,
                    0.95,
                );
                // 3px line centered on lx; both the half-width offset and the
                // width are logical px scaled by DPI.
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(lx - 1.5 * dpi, ly0, 3.0 * dpi, lh, sw, sh),
                    color: line_color,
                    ..Default::default()
                });
            }

            // Ghost body: alpha controlled by
            // `chip.ghost_alpha` (spec 0.5). The historical chip
            // rendered at 0.7; the spec ghost is more
            // translucent so the bar underneath stays legible.
            let chip_color = with_premultiplied_alpha(self.tab_active_bg, chip.ghost_alpha);
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(x0, y0, w, h, sw, sh),
                color: chip_color,
                ..Default::default()
            });

            // Drag-chip title text → chrome_text.
            //
            // Scale the
            // text color alpha by `chip.ghost_alpha` (spec 0.5) so
            // the GHOST TITLE matches the ghost body translucency.
            if !chip.title.is_empty() {
                let ghost_fg =
                    scale_chrome_text_alpha(self.tab_active_fg, chip.ghost_alpha.clamp(0.0, 1.0));
                if let Some(stack) = self.font_stack.as_ref() {
                    let native_em = stack
                        .cell_metrics_raster_px()
                        .ok()
                        .map(|m| m.cell_h as f32)
                        .unwrap_or(self.cell_h);
                    let mut wt = stack.clone();
                    // Match the legacy TextArea geometry: left = x0 + 6,
                    // top = y0 + (h - font_size*0.85*1.2) * 0.5, clip to
                    // chip body inset 4px. Font size goes through raster_px and
                    // the insets through dpi so the ghost title scales with the
                    // chip on HiDPI displays.
                    let chip_font_size = self.raster_px(self.font_size * 0.85);
                    let top = y0 + ((h - chip_font_size * 1.2).max(0.0)) * 0.5;
                    let baseline_y = top + chip_font_size * 0.8;
                    let layout = chrome_text::layout(
                        stack,
                        &mut wt,
                        &mut self.glyph_atlas,
                        &chip.title,
                        ghost_fg,
                        ChromeAttrs::default(),
                        chip_font_size,
                        native_em,
                        (x0 + 6.0 * dpi, baseline_y),
                        (sw, sh),
                        Some(ChromeClip { x: x0 + 4.0 * dpi, y: y0, w: w - 8.0 * dpi, h }),
                    );
                    overlay_glyph_instances.extend(layout.glyphs);
                    quads_overlay.extend(layout.missing_boxes);
                }
            }
            self.drag_chip_visual = Some(DragChipVisual { top_left: (x0, y0), size: (w, h) });
        } else {
            // When: `self.drag_chip` is None — no tab is being dragged, so the
            // recorded visual is cleared rather than left stale for tests.
            self.drag_chip_visual = None;
        }

        // Glyphon `Resolution` / `TextArea` / `TextBounds` /
        // `text_renderer.prepare` are gone. Every chrome string already
        // landed in `glyph_instances` (pre-overlay: search status bar,
        // tab titles) or `overlay_glyph_instances` (modal chrome:
        // palette, IME preedit, drag-chip title, quick-select hints) via `chrome_text::layout`
        // earlier in this function. The atlas upload + per-pass draw
        // calls below carry those instances to the GPU.

        // Quick-select hint overlay → chrome_text into the overlay
        // glyph instance vec. Each hint is anchored at its (row, col)
        // cell origin so the hint character sits exactly inside the
        // chosen cell.
        if quick_select_hint_count > 0 {
            // Reconstruct the hint string the legacy
            // `prepare_quick_select_overlay` routed through
            // `self.quick_select_buffer`. The hint set is sparse so
            // emitting per-hint via chrome_text avoids materializing
            // the full padded string.
            if let Some(qs) = copy_mode.and_then(|cm| cm.quick_select.as_ref()) {
                let bg_color = hex_to_chrome_color(theme.colors.background.0.as_str());
                if let Some(stack) = self.font_stack.as_ref() {
                    let native_em = stack
                        .cell_metrics_raster_px()
                        .ok()
                        .map(|m| m.cell_h as f32)
                        .unwrap_or(self.cell_h);
                    let mut wt = stack.clone();
                    for hint in &qs.hints {
                        let x = active_origin_x + hint.col_start as f32 * self.cell_w;
                        let y = active_origin_y + hint.row as f32 * self.cell_h;
                        let s = hint.hint.to_string();
                        let l = chrome_text::layout(
                            stack,
                            &mut wt,
                            &mut self.glyph_atlas,
                            &s,
                            bg_color,
                            ChromeAttrs::default(),
                            self.font_size,
                            native_em,
                            (x, y + self.font_size * 0.8),
                            (sw, sh),
                            None,
                        );
                        overlay_glyph_instances.extend(l.glyphs);
                        quads_overlay.extend(l.missing_boxes);
                    }
                }
            }
        }

        gpu_lap!("overlays");
        crate::frame_stats::note_assembly(assembly_started);

        if std::mem::take(&mut self.fault_atlas_change_during_assembly) {
            // The test seam stands in for an atlas reset during assembly: only its identity moves.
            self.glyph_atlas_generation = self.glyph_atlas_generation.wrapping_add(1);
        }
        let pass_end = PassEnd {
            atlas_stamp_at_start: atlas_stamp_at_frame_start,
            atlas_stamp_now: self.glyph_atlas_stamp(),
            atlas_evictions_at_start: atlas_evictions_at_frame_start,
            previous_recolor: self.last_recolor,
            current_recolor: frame_recolor,
            previous_tab_ink: self.last_tab_ink,
            current_tab_ink: tab_ink,
        };
        let receipts = match Self::finish_assembly_pass(&mut plan, panes, pass_end) {
            Ok(receipts) => receipts,
            Err(early_exit) => {
                // When: the atlas changed or the partial plan reached unemitted ink, the helper
                // decided the exit: `AtlasRetry` or `PartialFallback`.
                return Ok(early_exit);
            }
        };

        #[cfg(debug_assertions)]
        {
            crate::quad::debug_assert_premultiplied_quads("base", quads);
            crate::quad::debug_assert_premultiplied_quads("overlay", quads_overlay);
        }

        // The pass assembled a drawable frame, so the restore trims by its use; the lease then
        // travels inside the layers, so no exit up to the end of presentation loses the scratch.
        scratch_lease.complete();
        Ok(Assembled::Layers(Box::new(AssembledLayers {
            surface_width: sw,
            surface_height: sh,
            subpixel_aa,
            scratch: scratch_lease,
            field_candidates,
            missing_chars: missing_chars_this_frame,
            missing_chrome_chars: missing_chrome_scope.finish(),
            gpu_timing,
            plan,
            receipts,
            recolor: frame_recolor,
            tab_ink,
        })))
    }

    /// End one assembly pass with the decisions every pass takes, in this order, so production
    /// and the fallback tests run the same code: an atlas whose content stamp moved since the
    /// pass started has stale UVs, so the pass is `AtlasRetry`; otherwise the plan's damage is
    /// widened by this pass's recolors and tab-title ink against the last presented frame's; a
    /// partial plan whose widened damage reaches a row it did not emit is `PartialFallback`,
    /// since the scissor would erase that row's ink; otherwise the plan's receipts are read under
    /// the same guards the plan was built from. `Err` carries the early exit.
    fn finish_assembly_pass(
        plan: &mut FramePlan,
        panes: &[sonicterm_render_model::PaneRender<'_>],
        pass: PassEnd,
    ) -> std::result::Result<Vec<sonicterm_render_model::AckReceipt>, Assembled> {
        if atlas_changed_during_frame(pass.atlas_stamp_at_start, pass.atlas_stamp_now) {
            // When: atlas_changed_during_frame detects stale UVs, discard them after the source is released.
            return Err(Assembled::AtlasRetry {
                stamp: pass.atlas_stamp_at_start,
                evictions: pass.atlas_evictions_at_start,
            });
        }
        plan.widen_for_recolor(pass.previous_recolor, pass.current_recolor);
        plan.widen_for_tab_ink(pass.previous_tab_ink, pass.current_tab_ink);
        if plan.partial_reaches_unemitted_ink() {
            // When: `partial_reaches_unemitted_ink` holds, the scissor would erase a skipped row's ink.
            return Err(Assembled::PartialFallback);
        }
        Ok(presented_receipts(plan, panes))
    }

    /// Hand assembled batches to the presenter; on `Presented`, finish the frame and return its receipts.
    fn present_layers(&mut self, assembled: AssembledLayers) -> Result<FrameOutcome> {
        let AssembledLayers {
            surface_width,
            surface_height,
            subpixel_aa,
            scratch,
            field_candidates,
            missing_chars,
            missing_chrome_chars,
            mut gpu_timing,
            plan,
            receipts,
            recolor,
            tab_ink,
        } = assembled;
        // The lease returns the scratch to the renderer when it drops, on every outcome, including
        // a presenter `Err` and unwinding.
        let batches = scratch.held();
        // The presenter borrows only the owned drawable layers; no grid or parser guard is held.
        let layers = FrameLayers {
            surface_width,
            surface_height,
            first_frame: plan.first_frame,
            damage: plan.damage,
            partial: plan.mode == RenderMode::Partial,
            subpixel_aa,
            batches: FrameBatches {
                quads: &batches.quads,
                images: &batches.images,
                glyphs: &batches.glyphs,
                overlay_quads: &batches.overlay_quads,
                overlay_glyphs: &batches.overlay_glyphs,
            },
        };
        if std::mem::take(&mut self.fault_present_error) {
            // When: `fault_present_error` is armed, the presenter call fails as a real `Err` would.
            return Err(anyhow!("injected presentation failure"));
        }
        let outcome = self.present_frame(&layers, &mut gpu_timing)?;
        // Only a presented frame's field geometry is what the user sees and may be hit-tested.
        self.presented_fields
            .settle(field_candidates, matches!(outcome, PresentOutcome::Presented));
        if !matches!(outcome, PresentOutcome::Presented) {
            // When: `matches!` finds any outcome but `Presented`, no receipt is issued, so its dirty rows stay.
            self.finalize_growth_episodes_if_device_stopped();
            let _discarded = settle_retained_frame(
                &mut self.last_frame_key,
                &mut self.row_ink,
                &mut self.row_glyph_cache,
                &outcome,
                None,
                receipts,
            );
            return Ok(FrameOutcome::without_receipts(outcome));
        }
        // Only a presented frame's recolors are on screen, so only it becomes the next baseline.
        self.last_recolor = recolor;
        self.last_tab_ink = tab_ink;
        let receipts = self.finish_successful_frame(
            plan,
            receipts,
            missing_chars,
            missing_chrome_chars,
            gpu_timing,
        );
        Ok(FrameOutcome { outcome: PresentOutcome::Presented, receipts })
    }

    /// Raise one test fault on this renderer's device.
    ///
    /// Compiled into every build so the release smoke can prove containment.
    #[doc(hidden)]
    pub fn __inject_gpu_fault(&mut self, kind: GpuFaultKind) {
        match kind {
            GpuFaultKind::IsolatedOperation => {
                if let Some(_scope) = self.device_errors.enter_gpu_work("fault.isolated") {
                    // A stopped device refuses the scope, so it takes no isolated fault.
                    let device = &self.device;
                    run_isolated_validation(device, &self.device_errors, || {
                        let _invalid = device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("sonic-isolated-fault"),
                            size: 4,
                            usage: wgpu::BufferUsages::empty(),
                            mapped_at_creation: false,
                        });
                    });
                }
            }
            GpuFaultKind::RetainedResourceCreation => {
                self.fault_invalid_glyph_upload = true;
            }
            GpuFaultKind::FrameValidation => {
                self.fault_frame_probe = self
                    .device_errors
                    .gpu_work("fault.frame_probe", || create_frame_fault_probe(&self.device));
            }
            GpuFaultKind::DestroyDevice => {
                destroy_and_await_loss(&self.device, &self.device_errors);
            }
        }
        // The next frame must take the full path so its outcome is observable.
        self.last_frame_key = None;
        self.request_window_redraw();
    }

    /// Finish a presented frame: record its statistics and settle its plan, returning the receipts
    /// the settlement keeps.
    fn finish_successful_frame(
        &mut self,
        plan: FramePlan,
        receipts: Vec<sonicterm_render_model::AckReceipt>,
        missing_chars_this_frame: Vec<char>,
        missing_chrome_chars: Vec<char>,
        gpu_timing: Option<(Instant, Instant, Vec<(&'static str, f32)>)>,
    ) -> Vec<sonicterm_render_model::AckReceipt> {
        let render_mode = plan.mode;
        let damaged_rows = plan.damaged_rows;
        let (surface_width, surface_height) = (self.config.width, self.config.height);
        crate::frame_stats::note_damage(|| {
            crate::frame_stats::damage_permille(&plan.damage, surface_width, surface_height)
        });
        // The single rectangle's waste over the parts it unions decides whether a rect list pays.
        crate::frame_stats::note_damage_waste(|| {
            crate::frame_stats::damage_waste_permille(
                &plan.damage,
                &plan.damage_parts,
                surface_width,
                surface_height,
            )
        });
        // The frame counts by the presenter `present_frame` used, via the same predicate.
        let software_presenter =
            crate::frame_stats::presents_software(self.software_render_degrade);
        crate::frame_stats::note_frame(software_presenter);
        self.successful_frame_count = self.successful_frame_count.saturating_add(1);
        // The successful-present seam both presenters share closes any pending growth timing.
        self.growth_episodes.present();
        self.finish_glyph_atlas_retry();
        // The certificate reads the presented plan's scene and the atlas as it is at present.
        let atlas_dims = (self.glyph_atlas.width(), self.glyph_atlas.height());
        let stamp = self.glyph_atlas_stamp();
        crate::completeness::record_presented(
            &mut self.completeness,
            crate::completeness::Presented {
                full: render_mode == RenderMode::Full,
                scene: plan.key.scene(),
                stamp,
                atlas_dims,
                missing_terminal: &missing_chars_this_frame,
                missing_chrome: &missing_chrome_chars,
            },
        );
        self.last_missing_chars = missing_chars_this_frame;
        self.last_missing_chrome_chars = missing_chrome_chars;
        self.presented_damage.record(|| PresentedDamage {
            first_frame: plan.first_frame,
            damage: plan.damage,
            surface: PixelRect { x: 0, y: 0, w: surface_width.max(1), h: surface_height.max(1) },
        });
        // The presented rows' records replace theirs; records of undrawn or vanished rows are pruned.
        let receipts = settle_retained_frame(
            &mut self.last_frame_key,
            &mut self.row_ink,
            &mut self.row_glyph_cache,
            &PresentOutcome::Presented,
            Some(plan),
            receipts,
        );
        if self.pane_focus_flash.is_some() {
            self.request_window_redraw();
        }
        if let Some((start, last, mut parts)) = gpu_timing {
            let now = Instant::now();
            parts.push(("cleanup", now.saturating_duration_since(last).as_secs_f32() * 1000.0));
            let total_ms = now.saturating_duration_since(start).as_secs_f32() * 1000.0;
            let mode = match render_mode {
                RenderMode::Full => "full",
                RenderMode::Partial => "partial",
                RenderMode::Noop => "noop",
            };
            let mut line = format!(
                "[gpu_render_timing] window={} mode={mode} damaged_rows={damaged_rows} total={total_ms:.2}ms",
                self.render_timing_label
            );
            for (name, ms) in parts {
                line.push_str(&format!(" {name}={ms:.2}ms"));
            }
            tracing::debug!(target: "render_timing", %line);
        }
        receipts
    }

    /// This function only emits the quick-select hint background
    /// quads now. The legacy `quick_select_buffer` text path is gone;
    /// the per-hint text is laid out via `chrome_text::layout` later
    /// in `render()` so it shares the wezterm atlas with the rest of
    /// the chrome.
    #[allow(clippy::too_many_arguments)]
    fn prepare_quick_select_overlay(
        &mut self,
        quick_select: &QuickSelectState,
        origin_x: f32,
        origin_y: f32,
        scrollback_len: usize,
        visible_rows: usize,
        _theme: &Theme,
        sw: f32,
        sh: f32,
        quads_overlay: &mut Vec<QuadInstance>,
        snapped_cell_x: &[f32],
    ) {
        // derive each hint cell's x/w from the shared snapped-edge
        // cache so quick-select hint backgrounds share device-pixel
        // edges with adjacent glyph cells at fractional DPI.
        let raw_fallback = snapped_cell_x.is_empty();
        for hint in &quick_select.hints {
            let Some(visible_row) = hint.row.checked_sub(scrollback_len) else {
                // When: `hint.row.checked_sub(scrollback_len)` is None — the
                // hint sits above the viewport, in scrolled-off history.
                continue;
            };
            if visible_row >= visible_rows {
                // When: `visible_row >= visible_rows` — below the viewport, so
                // its background quad would land outside the pane.
                continue;
            }
            let (x, w) = if raw_fallback {
                (origin_x + hint.col_start as f32 * self.cell_w, self.cell_w)
            } else {
                // When: `!raw_fallback` — a real snapped-edge cache, so hint
                // backgrounds share device-pixel edges with glyph cells.
                let col = (hint.col_start).min(snapped_cell_x.len().saturating_sub(2));
                let lo = snapped_cell_x[col];
                let hi = snapped_cell_x[col + 1];
                (lo, hi - lo)
            };
            let y = origin_y + visible_row as f32 * self.cell_h;
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(x, y, w, self.cell_h, sw, sh),
                color: self.cursor_color,
                ..Default::default()
            });
        }
    }

    /// Shape one style run of `cells` into position-free row records: glyph records into
    /// `records.glyphs`, missing-glyph boxes into `records.tofu` and their codepoints into
    /// `records.missing_chars`. Projection onto the surface happens afterwards, once per row,
    /// on a hit and on a miss alike.
    ///
    /// Returns whether the run is complete. A run is incomplete when an attempted glyph was
    /// refused by the atlas (a `None` from `get_or_insert`, read before `drawable_or_tofu` folds
    /// it with the stable missing sentinel), when a shaped glyph with a real id rasterized nothing
    /// or is too large for the atlas (both also listed in `records.missing_chars`), when a block
    /// glyph drew nothing, when shaping failed, or when no shaper or rasterizer was available; such
    /// outcomes depend on atlas or font state, not on content, so the row they belong to is drawn
    /// but never cached. Intentional empty work is complete: an empty run, a run of wide
    /// continuations, whitespace and zero-area non-block tiles, and a character-fallback glyph the
    /// atlas caches as missing, which draws tofu.
    ///
    /// Non-ASCII clusters are shaped through the font stack. Each cluster's lead cell dispatches
    /// on [`sonicterm_block_glyph::BlockKey::from_char`]: on `Some`, the atlas holds a
    /// [`sonicterm_block_glyph::block_sprite`] tile keyed under the block-glyph sentinel
    /// (`GlyphKey { font_slot: 0xFF, glyph_id: <hashed SizedBlockKey>, .. }`), so the shaped
    /// path and the block-sprite path share the atlas without colliding; on `None`, the cluster
    /// takes the normal rasterize path. Box drawing, Powerline, Sextant, Octant and Braille all
    /// reach the renderer through this dispatch.
    // Hot inner-loop helper called per style run per row. Every argument is an exclusive `&mut`
    // borrow of a different renderer field or a per-run value; bundling them into one struct
    // would conflict with the surrounding loop's own borrows.
    #[allow(clippy::too_many_arguments)]
    fn build_shape_run(
        glyph_atlas: &mut GlyphAtlas,
        records: &mut sonicterm_text::row_glyph_cache::CachedRow,
        row: u16,
        style: RunStyle,
        // The run's cells borrowed from the grid, in strictly increasing column order.
        cells: &[(u16, &Cell)],
        theme: &Theme,
        fg_default: ChromeColor,
        cell_w: f32,
        cell_h: f32,
        top_inset: f32,
        snapped_cell_x: &[f32],
        // The sole shape entry point; `None` only in test fixtures without bundled fonts, where
        // non-ASCII runs emit nothing and are reported incomplete.
        font_stack: Option<&sonicterm_engine::FontStack>,
        // The rasterizer; `None` only in test fixtures, where no glyph is drawn and the run is
        // reported incomplete, while background, cursor and underline quads remain.
        mut wt_raster: Option<&mut sonicterm_engine::FontStack>,
        // This row's hover span, already gated to the drawing pane. An ACTIVE hover recolors the
        // glyphs inside it to `hovered_url_accent`; it is folded into the row's content key.
        hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
        hovered_url_accent: [f32; 4],
        software_presenter: bool,
    ) -> bool {
        use sonicterm_text::row_glyph_cache::{RowGlyphKind, RowTofu};
        if cells.is_empty() {
            // When: `cells.is_empty()` — the run carries no cells, so there is
            // nothing to shape and nothing missing.
            return true;
        }

        let style_span = tracing::enabled!(target: "render_timing", tracing::Level::DEBUG).then(|| {
            tracing::debug_span!(target: "render_timing", "font_style", bold = style.bold, italic = style.italic, row)
        });
        let _entered = style_span.as_ref().map(tracing::Span::enter);

        // A monochrome glyph's foreground in linear sRGB, swapped for the theme accent inside an
        // ACTIVE hover span; a plain hint leaves the colour and is marked by its underline only.
        let resolve_fg = |col: u16, base: ChromeColor| -> [f32; 4] {
            match hovered_url_cells {
                Some(h) if h.active && h.contains(row, col) => hovered_url_accent,
                _ => chrome_color_to_linear_rgba(base),
            }
        };
        // The tofu box of a cell `width` raster pixels wide, inset from the cell edges.
        let tofu_box = |lead_col: u16, width: f32, color: ChromeColor| {
            let inset = (cell_h * 0.12).max(1.0);
            RowTofu {
                lead_col,
                inset,
                width: width - inset * 2.0,
                height: cell_h - inset * 2.0,
                color: [color.r(), color.g(), color.b(), color.a()],
            }
        };
        let mut complete = true;

        // ASCII fast path: every cell is printable ASCII with no cluster extras and no ligature
        // trigger, so the shaper would map 1:1 anyway; the atlas is driven from each cell's key.
        // These codepoints never overlap the `BlockKey` ranges, so block dispatch is skipped.
        if run_is_ascii_fast(cells) {
            // When: `run_is_ascii_fast` — every cell is 0x20..=0x7E, which
            // cannot shape or need `BlockKey`, so each maps 1:1 to a tile.
            for (col, cell) in cells {
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch: cell.ch,
                    font_slot: 0,
                    weight_bold: style.bold,
                    italic: style.italic,
                    glyph_id: 0,
                    raster_variant: GlyphRasterVariant::Normal,
                };
                let Some(wt) = wt_raster.as_deref_mut() else {
                    // When: `wt_raster` is None — a test fixture with no FontStack; the glyph is
                    // skipped and the row is not cached.
                    complete = false;
                    continue;
                };
                let inserted = glyph_atlas.get_or_insert(key, &mut CountingRasterizer::new(wt));
                complete &= inserted.is_some();
                let Some(info) = drawable_or_tofu(inserted) else {
                    // When: `drawable_or_tofu` is None — the atlas refused the glyph or cached it
                    // as missing, so a printable cell draws the same outline box as the shaped path.
                    if !cell.ch.is_whitespace() {
                        // Blanks are intentionally tile-less and are not reported as missing.
                        records.tofu.push(tofu_box(*col, cell_w, cell_fg(cell, theme, fg_default)));
                        records.missing_chars.push(cell.ch);
                    }
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    // When: either axis of `info.px_size` is 0 — a zero-area
                    // tile, which is what a space rasterizes to.
                    continue;
                }
                let color = cell_fg(cell, theme, fg_default);
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    // When: `!info.is_color` — a monochrome mask, so the cell
                    // foreground and any hover recolor apply to it.
                    resolve_fg(*col, color)
                };
                if glyph_draw_is_degenerate(&info) {
                    // When: `glyph_draw_is_degenerate` — the tile has area but
                    // its UVs or metrics cannot produce a visible draw.
                    tracing::debug!(
                        target: "sonic::render::glyph",
                        ch = ?cell.ch,
                        is_color = info.is_color,
                        px_size = ?info.px_size,
                        uv = ?info.uv,
                        site = "ascii",
                        "skipped a degenerate glyph draw that would sample the atlas origin"
                    );
                    continue;
                }
                records.glyphs.push(tile_record(RowGlyphKind::Natural, *col, &info, rgba));
            }
            return complete;
        }

        // The non-ASCII path maps UTF-8 cluster offsets back to terminal columns before shaping.
        let Some(stack) = font_stack else {
            // When: `font_stack` is None — no shaper, so non-ASCII clusters
            // emit nothing and the row is not cached.
            return false;
        };
        // Only a run that reaches the shaper has its text and byte-to-column map built.
        let (text, cell_cols) = crate::row_runs::materialize_run_text(cells);
        if text.is_empty() {
            // When: `text.is_empty()` — every cell in the run was a wide
            // continuation, so the shaper has no bytes to work on.
            return true;
        }

        let shaped_text = crate::frame_stats::shape_request(|| {
            stack.shape_text_for_frame(&text, style.bold, style.italic)
        });
        let infos = match inject_shape_failure(shaped_text) {
            Ok(infos) => infos,
            Err(_) => {
                // When: `inject_shape_failure` passes on an `Err` — the face rejected the run or the
                // frame's shaping allowance ran out, so the run draws nothing and is not cached.
                return false;
            }
        };

        // Per-cell attributes (colour, WIDE flag, the codepoint for tofu diagnostics) are read
        // from the borrowed run by the shaped output's `lead_col`; a column the run lacks reads
        // as one default cell made per run, as the former owned lookup's fallback did.
        let default_cell = Cell::default();

        // Shaped output projects straight into the shaped-glyph record; cluster byte offsets map
        // back through `cell_cols`.
        let mut shaped = Vec::with_capacity(infos.len());
        let mut last_col: u16 = cell_cols.first().copied().unwrap_or(0);
        for info in infos {
            let cluster_byte = info.cluster as usize;
            let lead_col = cell_cols
                .get(cluster_byte)
                .copied()
                .or_else(|| (0..=cluster_byte).rev().find_map(|i| cell_cols.get(i).copied()))
                .unwrap_or(last_col);
            last_col = lead_col;
            let lead_ch =
                run_cell_at(cells, lead_col).map(|c| c.ch).or(info.only_char).unwrap_or(' ');
            let cluster_cells = (info.num_cells as u16).max(1);
            shaped.push(sonicterm_text::shape::ShapedGlyph {
                lead_col,
                cluster_cells,
                font_slot: u8::try_from(info.font_idx).unwrap_or(u8::MAX),
                glyph_id: info.glyph_pos,
                x_advance: info.x_advance.get() as f32,
                x_offset: info.x_offset.get() as f32,
                y_offset: info.y_offset.get() as f32,
                ch: lead_ch,
            });
        }

        #[cfg(debug_assertions)]
        debug_assert!(shaped_glyph_columns_are_monotonic(&shaped));

        let mut positioned_cluster_col = None;
        let mut positioned_cluster_pen_x = 0.0;
        for g in &shaped {
            let lead_cell: &Cell = run_cell_at(cells, g.lead_col).unwrap_or(&default_cell);
            let is_wide = lead_cell.flags.contains(CellFlags::WIDE);
            let cluster_cells = g.cluster_cells.max(1) as usize;
            let cells_to_span = if is_wide { 2 } else { cluster_cells };
            let cell_pixel_width = cell_w * cells_to_span as f32;

            // Block dispatch at the cluster lead cell: box drawing, block elements, Powerline,
            // Sextant, Octant and Braille draw from vendored geometry under the block-glyph
            // sentinel key, never from the font's own glyph.
            if let Some(block_key) = sonicterm_block_glyph::BlockKey::from_char(lead_cell.ch) {
                // When: `BlockKey::from_char` is Some — a box/block codepoint,
                // drawn from vendored geometry rather than the font's glyph.
                let cell_left_px = snapped_cell_x[g.lead_col as usize];
                let cell_top_px = top_inset + f32::from(row) * cell_h;
                let span = if is_wide { 2usize } else { cluster_cells };
                let end_col = ((g.lead_col as usize) + span).min(snapped_cell_x.len() - 1);
                let cell_right = snapped_cell_x[end_col];
                // The software presenter rasterizes to the exact integer destination so atlas
                // sampling stays one-to-one; that destination depends on the row's position,
                // which a cache lookup revalidates. The GPU keeps the fractional cell geometry.
                let (target_w, target_h) = if software_presenter {
                    let cell_bottom = top_inset + (f32::from(row) + 1.0) * cell_h;
                    let (_, _, width, height) = software_block_glyph_target_rect(
                        cell_left_px,
                        cell_top_px,
                        cell_right,
                        cell_bottom,
                    );
                    (width, height)
                } else {
                    // When: `!software_presenter` — the GPU path keeps the
                    // established fractional font-cell geometry unchanged.
                    (cell_w, cell_h)
                };
                let cell_w_i = target_w.round().max(1.0) as isize;
                let cell_h_i = target_h.round().max(1.0) as isize;
                // The outline stroke width comes from the font's underline thickness, as
                // WezTerm's sprite utilities take it; a hardcoded single pixel made box-drawing
                // strokes nearly invisible. With no font stack, a 1/16-cell heuristic is used.
                let underline_h_isize: isize = font_stack
                    .and_then(|s| s.cell_metrics_raster_px().ok())
                    .map(|m| m.underline_h.round().max(1.0) as isize)
                    .unwrap_or_else(|| ((cell_h / 16.0).round().max(1.0)) as isize);
                let size = sonicterm_block_glyph::glue::Size::new(cell_w_i, cell_h_i);
                let sized_key = sonicterm_block_glyph::SizedBlockKey { block: block_key, size };
                // Block tiles are size- and stroke-sensitive, so the atlas id hashes the sized key
                // and the stroke width; only collision resistance among the few hundred block
                // glyphs a frame touches is needed.
                let glyph_id_u32: u32 = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    sized_key.hash(&mut h);
                    underline_h_isize.hash(&mut h);
                    let h64 = h.finish();
                    // Fold to u32 by xoring the halves so all 64 bits
                    // contribute to the atlas key.
                    ((h64 >> 32) as u32) ^ (h64 as u32)
                };
                // Block geometry ignores bold and italic, so those bits are collapsed.
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch: lead_cell.ch,
                    font_slot: 0xFF,
                    weight_bold: false,
                    italic: false,
                    glyph_id: glyph_id_u32,
                    raster_variant: GlyphRasterVariant::Normal,
                };
                // A thin `Rasterizer` around `block_sprite`, so the atlas computes the sprite only
                // on a miss; identity is captured by `key`.
                struct BlockSpriteRasterizer {
                    sized_key: sonicterm_block_glyph::SizedBlockKey,
                    underline_h: isize,
                }
                impl sonicterm_text::glyph_atlas::Rasterizer for BlockSpriteRasterizer {
                    fn rasterize(
                        &mut self,
                        _key: sonicterm_types::glyph_key::GlyphKey,
                    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
                        // Cell metrics derive from the sized key; the underline height comes from
                        // the font, and anti-aliasing matches WezTerm's default.
                        let block_tile = sonicterm_block_glyph::block_sprite_with_cell_metrics(
                            self.sized_key,
                            self.underline_h,
                            true,
                        )
                        .ok()?;
                        // Coverage alpha becomes the monochrome mask while the block tile's raster geometry is preserved.
                        let alpha_mask: Vec<u8> =
                            block_tile.coverage.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
                        Some(sonicterm_text::glyph_atlas::RasterTile {
                            width: block_tile.width,
                            height: block_tile.height,
                            offset_x: block_tile.offset_x,
                            offset_y: block_tile.offset_y,
                            advance: block_tile.advance,
                            coverage: alpha_mask,
                            // Block geometry is a mask for the cell foreground, not a self-coloured
                            // emoji, so icons inherit the SGR foreground.
                            is_color: false,
                            is_subpixel: false,
                        })
                    }
                }
                let mut block_raster =
                    BlockSpriteRasterizer { sized_key, underline_h: underline_h_isize };
                let Some(info) =
                    glyph_atlas.get_or_insert(key, &mut CountingRasterizer::new(&mut block_raster))
                else {
                    // When: `glyph_atlas.get_or_insert` is None — the block
                    // sprite could not be packed; the row is not cached.
                    complete = false;
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    // When: either axis of `info.px_size` is 0 — the sprite passed the atlas's
                    // placement limit here, so the block drew nothing and its row is not cached.
                    complete = false;
                    continue;
                }
                let color = cell_fg(lead_cell, theme, fg_default);
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    // When: `!info.is_color` — a monochrome mask, so the cell
                    // foreground and any hover recolor are applied to it.
                    resolve_fg(g.lead_col, color)
                };
                tracing::debug!(
                    target: "sonic::render::glyph",
                    ch = ?lead_cell.ch,
                    codepoint = format!("U+{:04X}", lead_cell.ch as u32),
                    code_u32 = lead_cell.ch as u32,
                    target_size = ?(target_w, target_h),
                    final_rgba = ?rgba,
                    is_color = info.is_color,
                    path = "block_sprite",
                    "glyph render emit (block-glyph)"
                );
                let mut record = tile_record(RowGlyphKind::Block, g.lead_col, &info, rgba);
                record.raster_offset = [0.0; 2];
                record.raster_size = [target_w, target_h];
                record.end_col = end_col as u16;
                records.glyphs.push(record);
                continue;
            }

            // ── Normal shape path (non-block cluster) ──
            let shape_x_offset = shaped_cluster_x_offset(
                &mut positioned_cluster_col,
                &mut positioned_cluster_pen_x,
                g,
            );
            let marker_fit = status_marker_fit_eligible(
                lead_cell.ch,
                cluster_cells,
                is_wide,
                lead_cell.extras().is_some(),
            );
            if g.glyph_id == 0 {
                // When: `g.glyph_id == 0` — the shaper found no glyph for this
                // cluster, so the char-fallback path runs instead.
                let ch = lead_cell.ch;
                if ch == '\0' || ch.is_whitespace() {
                    // When: `ch == '\0' || ch.is_whitespace()` — both are
                    // legitimately glyph-less and must not draw tofu.
                    continue;
                }
                // The rasterizer finds a face itself for slot 0 when glyph_id is 0.
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch,
                    font_slot: 0,
                    weight_bold: style.bold,
                    italic: style.italic,
                    glyph_id: 0,
                    raster_variant: GlyphRasterVariant::Normal,
                };
                let Some(wt) = wt_raster.as_deref_mut() else {
                    // When: `wt_raster` is None — a test fixture with no
                    // FontStack, so the fallback char cannot be rasterized.
                    complete = false;
                    continue;
                };
                let inserted = glyph_atlas.get_or_insert(key, &mut CountingRasterizer::new(wt));
                complete &= inserted.is_some();
                let Some(info) = drawable_or_tofu(inserted) else {
                    // When: `drawable_or_tofu` is None — true tofu: the atlas refused the glyph or
                    // cached it as missing, so an outline box is drawn instead.
                    records.tofu.push(tofu_box(
                        g.lead_col,
                        cell_pixel_width,
                        cell_fg(lead_cell, theme, fg_default),
                    ));
                    records.missing_chars.push(ch);
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    // When: either axis of `info.px_size` is 0 — the fallback
                    // face produced a zero-area tile, which has no pixels.
                    continue;
                }
                let color = cell_fg(lead_cell, theme, fg_default);
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    // When: `!info.is_color` — a monochrome fallback glyph, so
                    // it takes the cell foreground like ordinary text.
                    resolve_fg(g.lead_col, color)
                };
                if glyph_draw_is_degenerate(&info) {
                    // When: `glyph_draw_is_degenerate` — the tile has area but
                    // its UVs or metrics cannot produce a visible draw.
                    tracing::warn!(
                        target: "sonic::render::glyph",
                        ch = ?lead_cell.ch,
                        codepoint = format!("U+{:04X}", lead_cell.ch as u32),
                        is_color = info.is_color,
                        px_size = ?info.px_size,
                        uv = ?info.uv,
                        site = "shaped_run",
                        "skipped a degenerate glyph draw that would sample the atlas origin"
                    );
                    continue;
                }
                let mut record = tile_record(RowGlyphKind::Fallback, g.lead_col, &info, rgba);
                set_shaped_bits(
                    &mut record,
                    [shape_x_offset, g.y_offset],
                    marker_fit,
                    g,
                    is_wide,
                    lead_cell,
                );
                records.glyphs.push(record);
                continue;
            }

            let key = sonicterm_types::glyph_key::GlyphKey::shaped(
                g.ch,
                g.font_slot,
                g.glyph_id,
                style.bold,
                style.italic,
            );
            let Some(wt) = wt_raster.as_deref_mut() else {
                // When: `wt_raster` is None — a test fixture with no FontStack,
                // so the shaped glyph cannot be rasterized.
                complete = false;
                continue;
            };
            let Some(info) = glyph_atlas.get_or_insert(key, &mut CountingRasterizer::new(wt))
            else {
                // When: `glyph_atlas.get_or_insert` is None — the atlas refused this shaped
                // glyph: it draws nothing, so it is reported missing and the row is not cached.
                records.missing_chars.push(lead_cell.ch);
                complete = false;
                continue;
            };
            if info.missing || info.oversize {
                // When: a real glyph id rasterized nothing or its tile can never be placed, it draws
                // nothing although it should, so it is reported missing and the row is not cached.
                records.missing_chars.push(lead_cell.ch);
                complete = false;
                continue;
            }
            if info.px_size[0] == 0 || info.px_size[1] == 0 {
                // When: either axis of `info.px_size` is 0 — an intentionally empty glyph,
                // which has no pixels to blit.
                continue;
            }
            // Multi-cell ligature halves keep their natural overhang so paired
            // glyphs such as `=>` continue to fuse across adjacent cells.
            let color = cell_fg(lead_cell, theme, fg_default);
            let rgba = if info.is_color {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                // When: `!info.is_color` — a monochrome mask, so the ligature
                // takes the cell foreground like ordinary text.
                resolve_fg(g.lead_col, color)
            };
            if glyph_draw_is_degenerate(&info) {
                // When: `glyph_draw_is_degenerate` — the tile has area but its
                // UVs or metrics cannot produce a visible draw.
                tracing::warn!(
                    target: "sonic::render::glyph",
                    ch = ?lead_cell.ch,
                    codepoint = format!("U+{:04X}", lead_cell.ch as u32),
                    is_color = info.is_color,
                    px_size = ?info.px_size,
                    uv = ?info.uv,
                    site = "ligature",
                    "skipped a degenerate glyph draw that would sample the atlas origin"
                );
                continue;
            }
            let mut record = tile_record(RowGlyphKind::Shaped, g.lead_col, &info, rgba);
            set_shaped_bits(
                &mut record,
                [shape_x_offset, g.y_offset],
                marker_fit,
                g,
                is_wide,
                lead_cell,
            );
            records.glyphs.push(record);
        }
        complete
    }
}

// Lifecycle: `GpuRenderer` releases its `LIVE_RENDERERS` slot here — the sole
// decrement, paired with the increment in `new_async`.
// Lifecycle: teardown counts any glyph atlas growth not yet counted and abandons a pending
// growth timing, since no later frame can present it.
impl Drop for GpuRenderer {
    // Ordering: `LIVE_RENDERERS.fetch_sub(1, Ordering::AcqRel)`, pairing with
    // the `Ordering::AcqRel` increment in `new_async`. Publishes no payload.
    fn drop(&mut self) {
        self.finalize_frame_stats();
        // Paired with the increment in `new`. Together they make the live
        // count return to its starting value across balanced open/close
        // churn, and stay above it when a renderer survives.
        LIVE_RENDERERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Would drawing this glyph sample the atlas outside its own tile?
///
/// The atlas caches an empty or failed rasterization as a sentinel with a
/// zero-area UV, `[0.0, 0.0, 0.0, 0.0]`, and its comment states the renderer
/// skips such a draw. Nothing did. `(0, 0)` is not "nowhere": it is the
/// atlas's top-left texel, which the shelf packer hands to the first glyph of
/// the session, so a zero-area sample reads that glyph's corner ink.
///
/// Monochrome block glyphs take the cell foreground, but a degenerate UV can
/// sample opaque coverage from the atlas origin and paint a stray
/// foreground-colored pixel.
///
/// Returns true when the instance must not be emitted.
#[must_use]
fn glyph_draw_is_degenerate(info: &sonicterm_text::glyph_atlas::GlyphInfo) -> bool {
    info.px_size[0] == 0
        || info.px_size[1] == 0
        || info.uv[2] <= info.uv[0]
        || info.uv[3] <= info.uv[1]
}

/// Fraction a dim/faint (`SGR 2`) cell's foreground is blended toward its
/// background. `0.45` lands roughly at the ~55% perceived intensity that
/// xterm/VTE/WezTerm use for faint text — enough that editor inline
/// predictions / ghost text read as clearly fainter than committed text
/// without becoming unreadable. See.
const DIM_BLEND: f32 = 0.45;

/// The terminal's atlas result for a fallback character: `None` draws tofu, both when the atlas
/// refused the glyph and when it cached the glyph as missing; an empty glyph, and the zero-area
/// sentinel of a tile too large to place, is returned, so the caller skips it without a box.
#[must_use]
fn drawable_or_tofu(
    info: Option<sonicterm_text::glyph_atlas::GlyphInfo>,
) -> Option<sonicterm_text::glyph_atlas::GlyphInfo> {
    info.filter(|info| !info.missing)
}

fn cell_fg(cell: &Cell, theme: &Theme, default: ChromeColor) -> ChromeColor {
    // Resolve the foreground and the cell's effective background. INVERSE
    // swaps the two (foreground is painted in the bg color and vice versa),
    // so resolve both consistently and let DIM blend toward whichever
    // background the glyph is actually drawn over.
    let (fg, bg) = if cell.flags.contains(CellFlags::INVERSE) {
        let default_bg = hex_to_chrome_color(theme.colors.background.0.as_str());
        let default_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        (color_to_chrome(cell.bg, theme, default_bg), color_to_chrome(cell.fg, theme, default_fg))
    } else {
        // When: no `INVERSE` flag — the ordinary case, where the cell's own
        // fg and bg are used as written.
        let default_bg = hex_to_chrome_color(theme.colors.background.0.as_str());
        (color_to_chrome(cell.fg, theme, default), color_to_chrome(cell.bg, theme, default_bg))
    };
    // Dim / faint (SGR 2): pull the foreground toward its background so
    // faint text is visibly de-emphasized instead of identical to normal
    // text. Stored but previously unread.
    if cell.flags.contains(CellFlags::DIM) {
        dim_toward(fg, bg, DIM_BLEND)
    } else {
        // When: no `DIM` flag — normal intensity, so the resolved foreground
        // is returned unblended.
        fg
    }
}

#[cfg(debug_assertions)]
fn shaped_glyph_columns_are_monotonic(glyphs: &[sonicterm_text::shape::ShapedGlyph]) -> bool {
    glyphs.windows(2).all(|pair| pair[0].lead_col <= pair[1].lead_col)
}

fn color_to_chrome(color: Color, theme: &Theme, default: ChromeColor) -> ChromeColor {
    match color {
        Color::Default => default,
        Color::Rgb(r, g, b) => ChromeColor::rgb(r, g, b),
        Color::Indexed(i) => indexed(i, theme).unwrap_or(default),
    }
}

#[allow(clippy::too_many_arguments)]
#[derive(Clone, Copy)]
struct InlineImagePlacement<'a> {
    image: &'a sonicterm_render_model::InlineImage,
    origin_x: f32,
    origin_y: f32,
    content_clip: PaneRect,
    painter_order: usize,
}

fn report_inline_image_pressure(changed: bool, skipped: usize, atlas: &GlyphAtlas) {
    if changed && skipped > 0 {
        // Explain omitted images only after a media change, without repeating an idle warning.
        tracing::warn!(
            target: "sonic::glyph_atlas",
            skipped,
            resident = atlas.len(),
            width = atlas.width(),
            height = atlas.height(),
            "inline image atlas full; skipped older images without evicting text glyphs"
        );
    }
}

impl InlineImagePlacement<'_> {
    fn visible_rect(&self, cell_w: f32, cell_h: f32, sw: f32, sh: f32) -> Option<PaneRect> {
        let image = self.image;
        if image.width == 0 || image.height == 0 || image.bgra.is_empty() {
            // When: `image` has no decoded pixels, neither residency nor emission can use its rectangle.
            return None;
        }
        let x = self.origin_x + image.col as f32 * cell_w;
        let y = self.origin_y + image.row as f32 * cell_h;
        let left = x.max(self.content_clip.x).max(0.0);
        let top = y.max(self.content_clip.y).max(0.0);
        let right = (x + image.width as f32).min(self.content_clip.x + self.content_clip.w).min(sw);
        let bottom =
            (y + image.height as f32).min(self.content_clip.y + self.content_clip.h).min(sh);
        if right <= left || bottom <= top {
            // When: `right <= left` or `bottom <= top`, the clipped image is empty and needs no atlas allocation.
            return None;
        }
        Some(PaneRect::new(left, top, right - left, bottom - top))
    }
}

fn emit_inline_image_instances(
    image_atlas: &mut GlyphAtlas,
    out: &mut Vec<ImageInstance>,
    placements: &[InlineImagePlacement<'_>],
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
) -> usize {
    let mut skipped = 0usize;
    let mut allocation_order: Vec<&InlineImagePlacement<'_>> = placements.iter().collect();
    allocation_order.sort_unstable_by_key(|placement| std::cmp::Reverse(placement.image.id));
    let mut emitted = Vec::with_capacity(placements.len());
    // Prefer the globally newest retained images when the bounded atlas
    // cannot hold the entire history, regardless of which pane owns them.
    for placement in allocation_order {
        let image = placement.image;
        let Some(visible) = placement.visible_rect(cell_w, cell_h, sw, sh) else {
            // When: `visible_rect` is absent, this image cannot contribute pixels or need an atlas tile.
            continue;
        };
        let x = placement.origin_x + image.col as f32 * cell_w;
        let y = placement.origin_y + image.row as f32 * cell_h;
        let key = sonicterm_types::glyph_key::GlyphKey {
            ch: '\u{fffc}',
            font_slot: 0xFE,
            weight_bold: false,
            italic: false,
            glyph_id: fold_u64_to_u32(image.id),
            raster_variant: GlyphRasterVariant::Normal,
        };
        let Some(info) =
            image_atlas.get_or_insert_lazy_without_eviction(key, image.width, image.height, || {
                sonicterm_text::glyph_atlas::RasterTile {
                    width: image.width,
                    height: image.height,
                    offset_x: 0,
                    offset_y: 0,
                    advance: image.width as f32,
                    coverage: image.bgra.as_ref().to_vec(),
                    is_color: true,
                    is_subpixel: false,
                }
            })
        else {
            // When: `get_or_insert_lazy_without_eviction` is None — the image
            // does not fit the bounded atlas; older ones are dropped first.
            skipped += 1;
            continue;
        };
        let [u0, v0, u1, v1] = info.uv;
        let du = (u1 - u0) / image.width as f32;
        let dv = (v1 - v0) / image.height as f32;
        emitted.push((
            placement.painter_order,
            ImageInstance {
                rect_px: [visible.x, visible.y, visible.w, visible.h],
                uv: [
                    u0 + (visible.x - x) * du,
                    v0 + (visible.y - y) * dv,
                    u0 + (visible.x + visible.w - x) * du,
                    v0 + (visible.y + visible.h - y) * dv,
                ],
                sample_uv: info.uv,
            },
        ));
    }
    emitted.sort_unstable_by_key(|(painter_order, _)| *painter_order);
    out.extend(emitted.into_iter().map(|(_, instance)| instance));
    skipped
}

fn emit_broadcast_borders(
    quads_overlay: &mut Vec<QuadInstance>,
    pane_rects: &[(u64, PaneRect)],
    participants: &[u64],
    warning: [f32; 4],
    sw: f32,
    sh: f32,
) {
    for (id, r) in pane_rects {
        if !participants.contains(id) || r.w <= 0.0 || r.h <= 0.0 {
            // When: id is not a participant or r has no area, no safety edge belongs in this pane.
            continue;
        }
        // Physical-pixel edges stay thin regardless of font size and cannot overlap on tiny panes.
        let t = 2.0_f32.min(r.w / 2.0).min(r.h / 2.0);
        for rect in [
            PaneRect::new(r.x, r.y, r.w, t),
            PaneRect::new(r.x, r.y + r.h - t, r.w, t),
            PaneRect::new(r.x, r.y + t, t, r.h - 2.0 * t),
            PaneRect::new(r.x + r.w - t, r.y + t, t, r.h - 2.0 * t),
        ] {
            quads_overlay.push(QuadInstance::sharp(
                px_to_ndc(rect.x, rect.y, rect.w, rect.h, sw, sh),
                warning,
            ));
        }
    }
}

fn fold_u64_to_u32(value: u64) -> u32 {
    ((value >> 32) as u32) ^ (value as u32)
}

fn underline_key(cell: &Cell) -> Option<(UnderlineStyle, Color)> {
    cell.flags
        .contains(CellFlags::UNDERLINE)
        .then(|| (cell.underline_style(), cell.underline_color().unwrap_or(cell.fg)))
}

#[allow(clippy::too_many_arguments)]
fn push_underline_quads(
    out: &mut Vec<QuadInstance>,
    style: UnderlineStyle,
    x: f32,
    y: f32,
    w: f32,
    cell_h: f32,
    thickness: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
) {
    if w <= 0.0 {
        // When: `w <= 0.0` — an empty or inverted span, which would emit a
        // quad with no area.
        return;
    }
    let bottom_y = y + cell_h - thickness;
    match style {
        UnderlineStyle::Single => {
            out.push(QuadInstance::sharp(px_to_ndc(x, bottom_y, w, thickness, sw, sh), color));
        }
        UnderlineStyle::Double => {
            let gap = thickness.max(1.0);
            let y1 = (bottom_y - gap - thickness).max(y);
            out.push(QuadInstance::sharp(px_to_ndc(x, y1, w, thickness, sw, sh), color));
            out.push(QuadInstance::sharp(px_to_ndc(x, bottom_y, w, thickness, sw, sh), color));
        }
        UnderlineStyle::Dotted => {
            let dot = (thickness * 1.6).max(1.0);
            let step = dot * 2.0;
            let mut dx = 0.0;
            while dx < w {
                let size = dot.min(w - dx);
                out.push(QuadInstance::rounded(
                    px_to_ndc(x + dx, bottom_y, size, dot, sw, sh),
                    color,
                    [size, dot],
                    dot * 0.5,
                ));
                dx += step;
            }
        }
        UnderlineStyle::Dashed => {
            let dash = (thickness * 4.0).max(4.0);
            let gap = (thickness * 2.0).max(2.0);
            let mut dx = 0.0;
            while dx < w {
                let len = dash.min(w - dx);
                out.push(QuadInstance::sharp(
                    px_to_ndc(x + dx, bottom_y, len, thickness, sw, sh),
                    color,
                ));
                dx += dash + gap;
            }
        }
        UnderlineStyle::Curly => {
            let amp = (thickness * 1.4).max(1.0);
            let step = (thickness * 4.0).max(4.0);
            let mid_y = y + cell_h - thickness - amp;
            let mut sx = x;
            let mut up = true;
            while sx < x + w {
                let ex = (sx + step).min(x + w);
                // When: up flips once per segment, so each curl stroke starts where the previous ended and the row reads as one wave, not dashes.
                let sy = if up { mid_y + amp } else { mid_y - amp };
                // When: up drives the end point to the opposite side of mid_y, giving this segment the inverse slope of its neighbour.
                let ey = if up { mid_y - amp } else { mid_y + amp };
                push_line_segment_px(out, sx, sy, ex, ey, thickness, sw, sh, color);
                sx = ex;
                up = !up;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_line_segment_px(
    out: &mut Vec<QuadInstance>,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    thickness: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
) {
    let pad = thickness * 0.5 + 1.0;
    let x0 = ax.min(bx) - pad;
    let y0 = ay.min(by) - pad;
    let x1 = ax.max(bx) + pad;
    let y1 = ay.max(by) + pad;
    let w = (x1 - x0).max(1.0);
    let h = (y1 - y0).max(1.0);
    let cx = x0 + w * 0.5;
    let cy = y0 + h * 0.5;
    out.push(QuadInstance::line(
        px_to_ndc(x0, y0, w, h, sw, sh),
        color,
        [w, h],
        [ax - cx, ay - cy],
        [bx - cx, by - cy],
        thickness,
    ));
}

/// Resolve a cell's background to a linear-space `[r,g,b,a]` suitable for the
/// quad pipeline, OR `None` if the cell should fall through to the surface
/// clear color (`theme.colors.background`).
///
/// Returning `None` for the default-bg case lets the per-row emit loop skip
/// pushing a quad over every blank cell — the attachment clear or partial
/// replacement reset already supplies the configured background.
///
/// Note on color space: the wgpu surface is `Bgra8UnormSrgb`, so the quad
/// fragment shader's output is sRGB-encoded on write. Inputs MUST therefore
/// be in linear-light space, otherwise gamma is applied twice and the result
/// looks washed out (same trap documented in `color.rs::hex_to_premultiplied_rgba`). The
/// sRGB→linear LUT here is bit-exact with the one feeding `hex_to_premultiplied_rgba`, so
/// `Color::Indexed(1)` (ANSI red) ends up identical to the theme's `ansi.red`
/// rendered through the LoadOp clear path.
#[doc(hidden)]
pub fn cell_bg_rgba(cell: &Cell, theme: &Theme) -> Option<[f32; 4]> {
    let color = if cell.flags.contains(CellFlags::INVERSE) {
        let default_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        color_to_chrome(cell.fg, theme, default_fg)
    } else {
        // When: INVERSE is clear, so the cell keeps its own bg and the glyph keeps fg; swapping them here too would cancel out reverse-video runs.
        match cell.bg {
            Color::Default => {
                // When: `Color::Default` is used, the attachment clear or replacement reset supplies this cell's background.
                return None;
            }
            bg => color_to_chrome(bg, theme, ChromeColor::rgb(0, 0, 0)),
        }
    };
    let lut = super::color::srgb_u8_to_linear_lut();
    Some([lut[color.r() as usize], lut[color.g() as usize], lut[color.b() as usize], 1.0])
}

/// Walk the visible rows of `grid`, emit one `QuadInstance` per maximal run
/// of horizontally-adjacent cells that share the same non-default background
/// color. Cells whose `bg` resolves to the theme default are skipped — the
/// attachment clear or partial replacement reset already covers them.
///
/// Run-length coalescing is essential: a single `\033[41m` color-fill of an
/// 80-column row would otherwise produce 80 quads where 1 suffices. The
/// renderer can hit tens of thousands of background cells per frame during
/// e.g. `htop` or `vim` syntax highlighting; per-cell quads would blow the
/// instance buffer and tank fill-rate.
///
/// `WIDE_CONT` cells (the right half of a wide CJK cell) inherit the lead
/// cell's bg via the parser, so they participate in the same run naturally.
///
/// The emitted quads are sharp-edged (no SDF) and pushed onto `out` in row-
/// major order. Caller is responsible for placing this BEFORE selection /
/// cursor / overlay quads in the draw vector so those still paint on top.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)] // mirrors flush_shape_run / collect_hyperlink_runs siblings — all geometry must be threaded in explicitly to keep this a free function (testable without a full GpuRenderer)
pub fn emit_cell_bg_quads(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pad: f32,
    top_inset: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    out: &mut Vec<QuadInstance>,
) {
    emit_cell_bg_quads_clipped(
        grid,
        view_top_abs,
        theme,
        PaneRect {
            x: pad,
            y: top_inset,
            w: f32::from(grid.cols) * cell_w,
            h: f32::from(grid.rows) * cell_h,
        },
        cell_w,
        cell_h,
        sw,
        sh,
        out,
    );
}

/// Like [`emit_cell_bg_quads`] but clips runs to a pane sub-rect. This is
/// the production split-pane path: a pane whose grid is wider than its
/// current tile must never emit quads into its neighbour's rectangle.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_cell_bg_quads_clipped(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pane_rect: PaneRect,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    out: &mut Vec<QuadInstance>,
) {
    let pad = pane_rect.x;
    let top_inset = pane_rect.y;
    let max_cols = ((pane_rect.w / cell_w).floor() as i32).clamp(0, i32::from(grid.cols)) as u16;
    let max_rows = ((pane_rect.h / cell_h).floor() as i32).clamp(0, i32::from(grid.rows)) as u16;
    if max_cols == 0 || max_rows == 0 {
        // When: max_cols or max_rows floors to zero, the pane tile cannot hold one whole cell; emitting anyway would paint bg into the neighbour pane.
        return;
    }
    // Snap this pane's raster-pixel edges so its background aligns with its glyphs, not the active pane's grid.
    let snapped_cell_x = build_snapped_cell_x(pad, cell_w, grid.cols);
    for r in 0..max_rows {
        emit_cell_bg_quads_for_row(
            grid,
            view_top_abs,
            theme,
            pad,
            top_inset,
            cell_w,
            cell_h,
            sw,
            sh,
            max_cols,
            r,
            out,
            &snapped_cell_x,
        );
    }
}

/// shared device-pixel-snapped column-edge cache. Returns
/// `cols + 1` entries where slot `c` is the snapped left edge of cell
/// `c`, and slot `c + span` is its right edge. Every overlay/glyph
/// path that derives a horizontal rect from a column index must read
/// from this cache so adjacent overlays share an exact device-pixel
/// edge with the glyph cells they cover.
///
/// Raster-pixel inputs use scale 1.0 for integer device-pixel rounding, including at fractional display DPI.
#[doc(hidden)]
#[must_use]
pub fn build_snapped_cell_x(origin_x: f32, cell_w: f32, cols: u16) -> Vec<f32> {
    let mut edges = Vec::with_capacity(usize::from(cols) + 1);
    fill_snapped_cell_x(&mut edges, origin_x, cell_w, cols);
    edges
}

/// [`build_snapped_cell_x`] into a reused buffer: `edges` is cleared and refilled with the same
/// `cols + 1` snapped edges, keeping its allocation across frames.
pub(crate) fn fill_snapped_cell_x(edges: &mut Vec<f32>, origin_x: f32, cell_w: f32, cols: u16) {
    edges.clear();
    edges.extend((0..=cols).map(|col| {
        sonicterm_render_model::geometry::snap_to_device_pixels(
            (origin_x + (col as f32) * cell_w, 0.0, 0.0, 0.0),
            1.0,
        )
        .0
    }));
}

/// Pure column-from-pixel lookup that mirrors the renderer's
/// device-pixel-snapped edge cache. `edges` is the output of
/// `build_snapped_cell_x` for the pane in question (length `cols + 1`).
/// Returns `Some(col)` for any `px` in `[edges[0], edges[cols])` using
/// half-open buckets `edges[col] <= px < edges[col+1]` — boundary px
/// resolve to the RHS cell, matching the renderer's draw bias.
///
/// Returns `None` if `px` is left of `edges[0]` or `>= edges[cols]`
/// (caller already gated negatives via the pane resolution step, but
/// this is defensive). Returns `None` if `edges` is malformed
/// (`len < 2`) — that only happens for a 0-col pane, which has no
/// addressable cell to begin with.
#[doc(hidden)]
#[must_use]
pub fn pixel_to_local_col(px: f32, edges: &[f32], cols: u16) -> Option<u16> {
    if cols == 0 || edges.len() < 2 {
        // When: cols is zero or edges is malformed (len < 2), the pane has no addressable cell, so no pixel can resolve to a column.
        return None;
    }
    if px < edges[0] {
        // When: px sits left of edges[0], it lands in the window padding rather than the grid, so no column owns it.
        return None;
    }
    if px >= edges[cols as usize] {
        // When: px reaches edges[cols], it is past the grid's right edge in trailing padding; the half-open buckets end at that exact value.
        return None;
    }
    // Linear scan: half-open buckets edges[i] <= px < edges[i+1].
    // Cell counts are bounded (<= a few hundred) so a scan beats the
    // branch overhead of binary search at typical widths. For very wide
    // grids (cols >> 200) this could switch to `partition_point` — the
    // input is monotone non-decreasing by construction.
    for i in 0..cols as usize {
        if px < edges[i + 1] {
            // When: px falls under edges[i + 1], bucket i contains it; a boundary px resolves to the right-hand cell, matching the renderer's draw bias.
            return Some(i as u16);
        }
    }
    // Unreachable given the `>= edges[cols]` guard above, but keep the
    // total function obvious.
    None
}

/// A glyph record of `kind` at `lead_col` from an atlas tile: its region, raster offset and size,
/// `rgba` and its colour and subpixel flags, with no shaping offset and no marker fit.
fn tile_record(
    kind: sonicterm_text::row_glyph_cache::RowGlyphKind,
    lead_col: u16,
    info: &sonicterm_text::glyph_atlas::GlyphInfo,
    rgba: [f32; 4],
) -> sonicterm_text::row_glyph_cache::RowGlyph {
    sonicterm_text::row_glyph_cache::RowGlyph {
        uv: info.uv,
        color: rgba,
        raster_offset: [info.px_offset[0] as f32, info.px_offset[1] as f32],
        shape_offset: [0.0; 2],
        raster_size: [info.px_size[0] as f32, info.px_size[1] as f32],
        lead_col,
        end_col: lead_col.saturating_add(1),
        kind_and_bits: sonicterm_text::row_glyph_cache::RowGlyphBits {
            kind,
            is_color: info.is_color,
            is_subpixel: info.is_subpixel,
            marker_fit_eligible: false,
            is_wide: false,
            has_extras: false,
            cluster_cells: 1,
        }
        .pack(),
    }
}

/// Record a shaped or fallback glyph's resolved shaping offset and the inputs of its marker fit,
/// decided now from the lead cell and applied only at projection.
fn set_shaped_bits(
    record: &mut sonicterm_text::row_glyph_cache::RowGlyph,
    shape_offset: [f32; 2],
    marker_fit_eligible: bool,
    shaped: &sonicterm_text::shape::ShapedGlyph,
    is_wide: bool,
    lead_cell: &Cell,
) {
    let mut bits = record.bits();
    bits.marker_fit_eligible = marker_fit_eligible;
    bits.is_wide = is_wide;
    bits.has_extras = lead_cell.extras().is_some();
    bits.cluster_cells = shaped.cluster_cells.max(1);
    record.kind_and_bits = bits.pack();
    record.shape_offset = shape_offset;
}

#[cfg(test)]
thread_local! {
    /// Test seam: the next frame-shaping call on this thread fails once.
    static FAIL_NEXT_SHAPE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test seam: make the next `shape_text_for_frame` call on this thread return `Err` once.
#[cfg(test)]
pub(crate) fn fail_next_shape_for_test() {
    FAIL_NEXT_SHAPE.with(|armed| armed.set(true));
}

/// `shaped`, or an error in its place when the test seam is armed, consuming the seam.
#[cfg(test)]
fn inject_shape_failure<T>(shaped: Result<T>) -> Result<T> {
    if FAIL_NEXT_SHAPE.with(|armed| armed.replace(false)) {
        // When: the test seam is armed, this shaping call fails as a real error would.
        return Err(anyhow!("injected shaping failure"));
    }
    shaped
}

/// Production never injects a shaping failure: the shaping result passes through.
#[cfg(not(test))]
#[inline(always)]
fn inject_shape_failure<T>(shaped: Result<T>) -> Result<T> {
    shaped
}

/// The cell at `col` in a style run whose columns strictly increase, found by binary search;
/// `None` when the run holds no cell at that column.
fn run_cell_at<'cell>(cells: &[(u16, &'cell Cell)], col: u16) -> Option<&'cell Cell> {
    cells.binary_search_by_key(&col, |(cell_col, _)| *cell_col).ok().map(|index| cells[index].1)
}

/// Where one cached row is drawn this frame: its slot, origin, column edges, cell size, baseline,
/// surface and presenter. Everything the content key omits lives here, so one row's records
/// project correctly wherever it is drawn.
#[derive(Clone, Copy)]
pub(crate) struct RowPlacement<'edges> {
    pub(crate) slot: u16,
    pub(crate) origin: (f32, f32),
    pub(crate) cols: u16,
    pub(crate) snapped_cell_x: &'edges [f32],
    pub(crate) cell_size: (f32, f32),
    pub(crate) baseline_y_in_cell: f32,
    pub(crate) surface: (f32, f32),
}

/// Project one glyph record onto the surface at `at`, per kind, in the order the shaping paths
/// built rectangles before records existed: cell origin, raster offset (natural position),
/// shaping offset, marker fit when eligible, device-pixel snap, then NDC. A block fills the
/// columns it spans: on the GPU at the fractional cell height, on the software presenter at the
/// integer target its tile was rasterized for.
pub(crate) fn project_row_glyph(
    glyph: &sonicterm_text::row_glyph_cache::RowGlyph,
    at: &RowPlacement<'_>,
    software_presenter: bool,
) -> GlyphInstance {
    use sonicterm_text::row_glyph_cache::RowGlyphKind;
    let bits = glyph.bits();
    let (cell_w, cell_h) = at.cell_size;
    let (surface_width_px, surface_height_px) = at.surface;
    let top_inset = at.origin.1;
    let cell_left_px = at.snapped_cell_x[usize::from(glyph.lead_col)];
    let cell_top_px = top_inset + f32::from(at.slot) * cell_h;
    let rect = match bits.kind {
        RowGlyphKind::Block => {
            let cell_right = at.snapped_cell_x[usize::from(glyph.end_col)];
            if software_presenter {
                let cell_bottom = top_inset + (f32::from(at.slot) + 1.0) * cell_h;
                software_block_glyph_target_rect(cell_left_px, cell_top_px, cell_right, cell_bottom)
            } else {
                // When: `software_presenter` is false, the block keeps the fractional cell box.
                (cell_left_px, cell_top_px, cell_right - cell_left_px, cell_h)
            }
        }
        RowGlyphKind::Natural => {
            let natural = (
                cell_left_px + glyph.raster_offset[0],
                cell_top_px + at.baseline_y_in_cell + glyph.raster_offset[1],
                glyph.raster_size[0],
                glyph.raster_size[1],
            );
            sonicterm_render_model::geometry::snap_to_device_pixels(natural, 1.0)
        }
        RowGlyphKind::Fallback | RowGlyphKind::Shaped => {
            let natural = (
                cell_left_px + glyph.raster_offset[0],
                cell_top_px + at.baseline_y_in_cell + glyph.raster_offset[1],
                glyph.raster_size[0],
                glyph.raster_size[1],
            );
            let positioned =
                positioned_shaped_glyph_rect(natural, glyph.shape_offset[0], glyph.shape_offset[1]);
            let fitted = if bits.marker_fit_eligible {
                // A standalone status marker is fitted inside its own cell.
                let cell_right = at
                    .snapped_cell_x
                    .get(usize::from(glyph.lead_col) + 1)
                    .copied()
                    .unwrap_or(cell_left_px + cell_w);
                fit_status_marker_rect(
                    positioned,
                    (cell_left_px, cell_top_px, cell_right - cell_left_px, cell_h),
                )
            } else {
                // When: `marker_fit_eligible` is false, the glyph keeps its natural overhang, as ligature halves must.
                positioned
            };
            sonicterm_render_model::geometry::snap_to_device_pixels(fitted, 1.0)
        }
    };
    GlyphInstance {
        rect: px_to_ndc(rect.0, rect.1, rect.2, rect.3, surface_width_px, surface_height_px),
        uv: glyph.uv,
        color: glyph.color,
        flags: glyph_flags(bits.is_color, bits.is_subpixel),
    }
}

/// Append one cached row to the frame at `at`: underlines at its slot, projected glyphs as the
/// row's own span, tofu boxes and missing characters. The hit path and the miss path both call
/// this, so a replayed row draws exactly what shaping it again would.
pub(crate) fn project_cached_row(
    row: &sonicterm_text::row_glyph_cache::CachedRow,
    at: &RowPlacement<'_>,
    software_presenter: bool,
    frame: GlyphFrame<'_>,
) {
    let (pad, top_inset) = at.origin;
    for run in &row.underlines {
        frame.underlines.push((pad, top_inset, at.cols, at.slot, *run));
    }
    let glyph_base = frame.glyph_instances.len();
    frame
        .glyph_instances
        .extend(row.glyphs.iter().map(|glyph| project_row_glyph(glyph, at, software_presenter)));
    let cell_h = at.cell_size.1;
    for tofu in &row.tofu {
        frame.missing_tofu.push((
            at.snapped_cell_x[usize::from(tofu.lead_col)] + tofu.inset,
            top_inset + f32::from(at.slot) * cell_h + tofu.inset,
            tofu.width,
            tofu.height,
            ChromeColor::from(tofu.color),
        ));
    }
    frame.missing_chars_this_frame.extend_from_slice(&row.missing_chars);
    let (surface_width_px, surface_height_px) = at.surface;
    frame.row_spans.push(RowGlyphSpan::new(
        frame.glyph_instances,
        glyph_base..frame.glyph_instances.len(),
        surface_width_px,
        surface_height_px,
    ));
}

/// Whether every block record of a software-presenter row still rasterizes at its stored size
/// when drawn at `at`. The software target rectangle is integer, so a fractional cell height or
/// column edge can make the same block one pixel taller or wider at another position; such a
/// row misses and is shaped again. The current top, the separately computed bottom and the
/// current snapped column edges are used in that order, as shaping uses them.
pub(crate) fn software_blocks_fit(
    row: &sonicterm_text::row_glyph_cache::CachedRow,
    at: &RowPlacement<'_>,
) -> bool {
    use sonicterm_text::row_glyph_cache::RowGlyphKind;
    let (_, cell_h) = at.cell_size;
    let top_inset = at.origin.1;
    row.glyphs.iter().filter(|glyph| glyph.bits().kind == RowGlyphKind::Block).all(|glyph| {
        let (Some(&left), Some(&right)) = (
            at.snapped_cell_x.get(usize::from(glyph.lead_col)),
            at.snapped_cell_x.get(usize::from(glyph.end_col)),
        ) else {
            // When: `lead_col` or `end_col` is outside `snapped_cell_x`, the record cannot be placed here.
            return false;
        };
        let top = top_inset + f32::from(at.slot) * cell_h;
        let bottom = top_inset + (f32::from(at.slot) + 1.0) * cell_h;
        let (_, _, width, height) = software_block_glyph_target_rect(left, top, right, bottom);
        [width, height] == glyph.raster_size
    })
}

/// The renderer's glyph inputs for one row: atlas, row cache, fonts and shaping configuration.
pub(crate) struct GlyphShaping<'frame> {
    pub(crate) atlas: &'frame mut GlyphAtlas,
    pub(crate) row_cache: &'frame mut sonicterm_text::row_glyph_cache::RowGlyphCache,
    pub(crate) font_stack: Option<&'frame sonicterm_engine::FontStack>,
    pub(crate) wt_raster: Option<&'frame mut sonicterm_engine::FontStack>,
    pub(crate) style_rev: u64,
    pub(crate) theme: &'frame Theme,
    pub(crate) fg_default: ChromeColor,
    pub(crate) raster_px: f32,
    pub(crate) cell_size: (f32, f32),
    pub(crate) surface: (f32, f32),
    pub(crate) baseline_y_in_cell: f32,
    pub(crate) hovered_url_accent: [f32; 4],
    pub(crate) software_presenter: bool,
}

impl GlyphShaping<'_> {
    /// The same inputs for one row of a pane pass, reborrowed so the pass can use them again.
    fn reborrow(&mut self) -> GlyphShaping<'_> {
        GlyphShaping {
            atlas: &mut *self.atlas,
            row_cache: &mut *self.row_cache,
            font_stack: self.font_stack,
            wt_raster: self.wt_raster.as_deref_mut(),
            style_rev: self.style_rev,
            theme: self.theme,
            fg_default: self.fg_default,
            raster_px: self.raster_px,
            cell_size: self.cell_size,
            surface: self.surface,
            baseline_y_in_cell: self.baseline_y_in_cell,
            hovered_url_accent: self.hovered_url_accent,
            software_presenter: self.software_presenter,
        }
    }
}

/// Where one glyph row sits: its pane, grid, viewport slot and origin, hover, and the content
/// key its pane pass computed for it before any admission.
pub(crate) struct GlyphRow<'grid> {
    pub(crate) pane_id: sonicterm_text::row_glyph_cache::PaneId,
    pub(crate) grid: &'grid Grid,
    pub(crate) view_top_abs: u64,
    pub(crate) slot: u16,
    pub(crate) origin: (f32, f32),
    pub(crate) snapped_cell_x: &'grid [f32],
    pub(crate) pane_hovered_url: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    /// The row's content key from [`emitted_row_key`]; 0 for a row the grid no longer holds.
    pub(crate) key: u64,
}

/// The frame buffers one glyph row appends to.
pub(crate) struct GlyphFrame<'frame> {
    pub(crate) glyph_instances: &'frame mut Vec<GlyphInstance>,
    pub(crate) underlines:
        &'frame mut Vec<(f32, f32, u16, u16, sonicterm_text::row_glyph_cache::UnderlineRun)>,
    pub(crate) missing_tofu: &'frame mut Vec<(f32, f32, f32, f32, ChromeColor)>,
    pub(crate) missing_chars_this_frame: &'frame mut Vec<char>,
    pub(crate) row_spans: &'frame mut Vec<RowGlyphSpan>,
}

impl GlyphFrame<'_> {
    /// The same buffers for one row, reborrowed so the pass can append the next row too.
    fn reborrow(&mut self) -> GlyphFrame<'_> {
        GlyphFrame {
            glyph_instances: &mut *self.glyph_instances,
            underlines: &mut *self.underlines,
            missing_tofu: &mut *self.missing_tofu,
            missing_chars_this_frame: &mut *self.missing_chars_this_frame,
            row_spans: &mut *self.row_spans,
        }
    }
}

/// The content key of the row at `slot` of a view whose top is `view_top_abs`: its cells and
/// every non-positional shaping input, with the row's active hover fragment. Counts the row's
/// cells as hashed. Returns 0 when the grid no longer holds that row.
pub(crate) fn emitted_row_key(
    shaping: &GlyphShaping<'_>,
    grid: &Grid,
    view_top_abs: u64,
    slot: u16,
    pane_hovered_url: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
) -> u64 {
    let Some(row) = grid.row_at_abs(view_top_abs.saturating_add(u64::from(slot))) else {
        // When: the absolute row is outside the scrollback still held, nothing is emitted for it.
        return 0;
    };
    crate::frame_stats::note_row_cells_hashed(|| row.iter().len());
    let inputs = sonicterm_text::row_glyph_cache::RowKeyInputs {
        style_rev: shaping.style_rev,
        cell_w: shaping.cell_size.0,
        cell_h: shaping.cell_size.1,
        baseline_y_in_cell: shaping.baseline_y_in_cell,
        raster_px: shaping.raster_px,
        software_presenter: shaping.software_presenter,
        hover_span: hovered_url_row_key_span(pane_hovered_url, slot),
    };
    shaping.row_cache.content_key(row.iter(), grid.cols, &inputs)
}

/// Append one row's glyphs, underlines and tofu to the frame, replaying them from the row cache
/// when its key and atlas identity match (and, for software blocks, their sizes still hold),
/// else shaping them into records and admitting the row when it is complete. Both paths project
/// through [`project_cached_row`]. Returns whether the row was replayed; a row the grid no longer
/// holds draws nothing and is not a replay.
pub(crate) fn emit_row_glyphs(
    shaping: GlyphShaping<'_>,
    placement: GlyphRow<'_>,
    mut frame: GlyphFrame<'_>,
) -> bool {
    let GlyphShaping {
        atlas,
        row_cache,
        font_stack,
        mut wt_raster,
        theme,
        fg_default,
        cell_size: (cell_w, cell_h),
        surface,
        baseline_y_in_cell,
        hovered_url_accent,
        software_presenter,
        ..
    } = shaping;
    let GlyphRow {
        pane_id,
        grid,
        view_top_abs,
        slot,
        origin,
        snapped_cell_x,
        pane_hovered_url,
        key,
    } = placement;
    let row_abs = view_top_abs.saturating_add(u64::from(slot));
    let Some(row) = grid.row_at_abs(row_abs) else {
        // When: `grid.row_at_abs(row_abs)` is None — that
        // absolute row is outside the scrollback still held.
        return false;
    };
    let at = RowPlacement {
        slot,
        origin,
        cols: grid.cols,
        snapped_cell_x,
        cell_size: (cell_w, cell_h),
        baseline_y_in_cell,
        surface,
    };
    let atlas_identity = row_cache_atlas_identity(atlas);
    let cached_row = row_cache.get(pane_id, key, atlas_identity, |cached| {
        !software_presenter || software_blocks_fit(cached, &at)
    });
    crate::frame_stats::note_row_cache(cached_row.is_some());
    if let Some(cached) = cached_row {
        // When: `cached_row` is Some — the content key and atlas identity match and any software
        // block still fits, so the shaped records are projected at this row's position.
        project_cached_row(cached, &at, software_presenter, frame.reborrow());
        return true;
    }
    // Miss: build position-free records in a row-local buffer, project them, and admit them when
    // complete. Keeping the row's work local is what lets it be cached without rescanning the
    // frame buffers.
    let mut records = sonicterm_text::row_glyph_cache::CachedRow::default();
    let mut ul_start: Option<(u16, UnderlineStyle, Color)> = None;
    let mut last_visible_col: u16 = 0;
    // First pass: per-cell underline coalescing; underlines are a cell-level decoration,
    // independent of shaping.
    for (col, cell) in row.iter().enumerate() {
        if cell.flags.contains(CellFlags::WIDE_CONT) {
            // When: `WIDE_CONT` — the trailing half of a wide
            // glyph, whose decoration belongs to its lead cell.
            continue;
        }
        last_visible_col = col as u16;
        if let Some((style, color)) = underline_key(cell) {
            match ul_start {
                Some((_, active_style, active_color))
                    if active_style == style && active_color == color =>
                {
                    // When: the guard holds — same style and
                    // colour, so the open run simply continues.
                }
                Some((s, active_style, active_color)) => {
                    records.underlines.push(sonicterm_text::row_glyph_cache::UnderlineRun {
                        start_col: s,
                        end_col: (col as u16).saturating_sub(1),
                        style: active_style,
                        color: active_color,
                    });
                    ul_start = Some((col as u16, style, color));
                }
                None => {
                    ul_start = Some((col as u16, style, color));
                }
            }
        } else if let Some((s, style, color)) = ul_start.take() {
            // When: `ul_start.take()` is Some — this cell has no
            // underline, so the open run ends and is emitted.
            records.underlines.push(sonicterm_text::row_glyph_cache::UnderlineRun {
                start_col: s,
                end_col: (col as u16).saturating_sub(1),
                style,
                color,
            });
        }
    }
    if let Some((s, style, color)) = ul_start.take() {
        records.underlines.push(sonicterm_text::row_glyph_cache::UnderlineRun {
            start_col: s,
            end_col: last_visible_col,
            style,
            color,
        });
    }

    // Second pass: group cells into style runs and shape each one. Every run is built, whatever
    // an earlier run reported, and its completeness is then folded into the row's: a failed run
    // keeps the row out of the cache but never stops a later valid run from drawing.
    let row_hovered_url = hovered_url_for_pane_row(pane_hovered_url, pane_id, slot);
    let mut complete = true;
    // Visible cells borrowed from the grid; each style run is a slice of this list.
    let cells = crate::row_runs::visible_cells(row);
    for run in crate::row_runs::row_shape_runs(&cells) {
        let run_complete = GpuRenderer::build_shape_run(
            atlas,
            &mut records,
            slot,
            run.style,
            run.cells,
            theme,
            fg_default,
            cell_w,
            cell_h,
            origin.1,
            snapped_cell_x,
            font_stack,
            wt_raster.as_deref_mut(),
            row_hovered_url,
            hovered_url_accent,
            software_presenter,
        );
        complete &= run_complete;
    }
    project_cached_row(&records, &at, software_presenter, frame.reborrow());
    if complete {
        // Every run shaped and every attempted glyph reached a stable atlas outcome, so the row
        // is admitted; an incomplete row was drawn and is shaped again next time.
        row_cache.insert(pane_id, key, row_cache_atlas_identity(atlas), records);
    }
    false
}

/// One pane's glyph rows for a pass: its grid, plan, origin, column edges and hover.
pub(crate) struct PaneGlyphRows<'pane> {
    pub(crate) pane_id: sonicterm_text::row_glyph_cache::PaneId,
    pub(crate) grid: &'pane Grid,
    pub(crate) planned: &'pane PlannedPane,
    pub(crate) origin: (f32, f32),
    pub(crate) snapped_cell_x: &'pane [f32],
    pub(crate) pane_hovered_url: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
}

/// Where a pane pass records each emitted row's ink and its underline owners.
pub(crate) struct PaneGlyphSinks<'sink> {
    pub(crate) row_ink: &'sink mut crate::row_ink::RowInkTable,
    pub(crate) ink_surface: PixelRect,
    pub(crate) underline_owners: &'sink mut Vec<usize>,
    pub(crate) injected_row_glyph: Option<InjectedRowGlyph>,
    /// Test inspector: each slot is appended where its row is emitted; `None` in production.
    pub(crate) emitted_slots: Option<&'sink mut Vec<u16>>,
    /// Reused buffer for one pane's row keys while its rows are pinned.
    pub(crate) row_keys: &'sink mut Vec<u64>,
}

/// Start one glyph assembly pass over `panes`, each a planned pane with its grid's columns:
/// one row-cache pass for the panes the plan draws (it releases panes not drawn or resized,
/// clears every stage and pin list, and tracks new panes) and a fresh ink stage, so records an
/// unpresented frame staged are discarded. Production and the fallback tests both start every
/// pass here, before any pane pins or admits a row.
pub(crate) fn begin_glyph_pass<'plan>(
    row_cache: &mut sonicterm_text::row_glyph_cache::RowGlyphCache,
    row_ink: &mut crate::row_ink::RowInkTable,
    panes: impl IntoIterator<Item = (&'plan PlannedPane, u16)>,
) {
    let drawn: Vec<(sonicterm_text::row_glyph_cache::PaneId, u16, u16)> = panes
        .into_iter()
        .filter(|(planned, _)| planned.full_clip.is_some())
        .map(|(planned, cols)| (planned.id, planned.row_count, cols))
        .collect();
    row_cache.begin_frame(&drawn);
    row_ink.begin_frame();
}

/// Assemble one pane's emitted glyph rows in two phases. The pin phase computes the content key
/// of every row the plan emits, once, and pins those keys with the pane's committed slot keys
/// before any admission, so eviction can never drop a row this pass or the presented frame
/// shows. The emit phase then, in slot order, replays or shapes each row, stages its key for the
/// slot, appends the test row-glyph seam and stages the row's ink record. The cache's
/// `begin_frame` for this pass must already have run.
pub(crate) fn assemble_pane_glyph_rows(
    mut shaping: GlyphShaping<'_>,
    pane: PaneGlyphRows<'_>,
    mut frame: GlyphFrame<'_>,
    mut sinks: PaneGlyphSinks<'_>,
) {
    let PaneGlyphRows { pane_id, grid, planned, origin, snapped_cell_x, pane_hovered_url } = pane;
    let view_top_abs = planned.view_top_abs;
    // Pin phase, into the frame scratch's key buffer, borrowed in place so an unwind leaves it
    // in the scratch.
    let keys: &mut Vec<u64> = &mut *sinks.row_keys;
    keys.clear();
    keys.extend(planned.rows().map(|(slot, _)| {
        if planned.emit_rows[usize::from(slot)] {
            emitted_row_key(&shaping, grid, view_top_abs, slot, pane_hovered_url)
        } else {
            // When: the plan does not emit `slot`, its key is never needed this pass.
            0
        }
    }));
    shaping.row_cache.pin(pane_id, keys);
    // Emit phase.
    for (slot, _) in planned.rows() {
        if !planned.emit_rows[usize::from(slot)] {
            // When: the plan does not emit `slot`, its retained pixels and record stay.
            continue;
        }
        let key = keys[usize::from(slot)];
        let (spans_before, tofu_before, underlines_before) =
            (frame.row_spans.len(), frame.missing_tofu.len(), frame.underlines.len());
        let _replayed = emit_row_glyphs(
            shaping.reborrow(),
            GlyphRow {
                pane_id,
                grid,
                view_top_abs,
                slot,
                origin,
                snapped_cell_x,
                pane_hovered_url,
                key,
            },
            frame.reborrow(),
        );
        if let Some(slots) = sinks.emitted_slots.as_deref_mut() {
            // A test inspector is attached: this slot's row was just emitted.
            slots.push(slot);
        }
        if key != 0 {
            // The row exists, so its key is this slot's staged key until the frame settles.
            shaping.row_cache.stage_slot(pane_id, slot, key);
        }
        // The row's ink: its glyphs' union and its tofu outlines. Its underlines join when they
        // are drawn below.
        push_injected_row_glyph(
            shaping.atlas,
            sinks.injected_row_glyph,
            pane_id,
            slot,
            frame.glyph_instances,
            frame.row_spans,
            shaping.surface,
        );
        let ink = crate::row_ink::emitted_row_ink(
            &frame.row_spans[spans_before..],
            frame.missing_tofu[tofu_before..]
                .iter()
                .map(|(left, top, width, height, _)| (*left, *top, *width, *height)),
        );
        let staged = sinks.row_ink.stage_row(
            pane_id,
            slot,
            grid,
            view_top_abs,
            ink.to_rect(sinks.ink_surface),
        );
        sinks.underline_owners.extend((underlines_before..frame.underlines.len()).map(|_| staged));
    }
}

/// The pane-focus flash: the pane's chrome rectangle, lifted 0.07 above the background, at `alpha`.
pub(crate) fn focus_flash_quad(
    bg_rgba: [f32; 4],
    chrome: (f32, f32, f32, f32),
    alpha: f32,
    (sw, sh): (f32, f32),
) -> QuadInstance {
    let flash_rgb =
        [(bg_rgba[0] + 0.07).min(1.0), (bg_rgba[1] + 0.07).min(1.0), (bg_rgba[2] + 0.07).min(1.0)];
    QuadInstance {
        rect: px_to_ndc(chrome.0, chrome.1, chrome.2, chrome.3, sw, sh),
        color: premultiply([flash_rgb[0], flash_rgb[1], flash_rgb[2], alpha]),
        ..Default::default()
    }
}

/// Where a selection is drawn: its pane's viewport, grid size, origin, cells, clip and surface.
pub(crate) struct SelectionGeometry {
    pub(crate) view_top_abs: u64,
    pub(crate) grid_size: (u16, u16),
    pub(crate) origin: (f32, f32),
    pub(crate) cell_size: (f32, f32),
    pub(crate) clip: (f32, f32, f32, f32),
    pub(crate) surface: (f32, f32),
}

/// Push the selection highlight quads, clipped to the pane so a drag across a split cannot bleed.
pub(crate) fn push_selection_quads(
    out: &mut Vec<QuadInstance>,
    sel: &sonicterm_render_model::boundary::ui::selection::Selection,
    geometry: &SelectionGeometry,
    snapped_cell_x: &[f32],
    color: [f32; 4],
) {
    let (clip_x, clip_y, clip_w, clip_h) = geometry.clip;
    for rect in selection_quad_rects(
        sel,
        geometry.view_top_abs,
        geometry.grid_size.0,
        geometry.grid_size.1,
        geometry.origin.0,
        geometry.origin.1,
        geometry.cell_size.0,
        geometry.cell_size.1,
        snapped_cell_x,
    )
    .into_iter()
    .filter_map(|rect| clip_rect_to_pane(rect, clip_x, clip_y, clip_w, clip_h))
    {
        out.push(QuadInstance {
            rect: px_to_ndc(rect.0, rect.1, rect.2, rect.3, geometry.surface.0, geometry.surface.1),
            color,
            ..Default::default()
        });
    }
}

/// Pane geometry one row's background quads depend on, in raster pixels.
pub(crate) struct RowBackgroundGeometry {
    pub(crate) origin: (f32, f32),
    pub(crate) pane_size: (f32, f32),
    pub(crate) cell_size: (f32, f32),
    pub(crate) surface: (f32, f32),
    pub(crate) max_cols: u16,
}

/// Which row of which pane is being emitted.
pub(crate) struct RowBackgroundRow<'grid> {
    pub(crate) pane_id: crate::row_quad_cache::PaneId,
    pub(crate) grid: &'grid Grid,
    pub(crate) view_top_abs: u64,
    pub(crate) slot: u16,
}

/// Append one row's background quads to `out`, replaying them from `cache` when the row's key
/// (contents, slot, style, geometry and selection overlap) matches, else emitting and caching
/// them. Returns whether the row was replayed. Grid dirt is not consulted: a key that covers
/// every input is what makes a replay equal a fresh emission.
pub(crate) fn emit_row_background<'cell, Cells>(
    cache: &mut crate::row_quad_cache::LineQuadCache,
    row: RowBackgroundRow<'_>,
    cells: Cells,
    (style_rev, theme, selection): (u64, &Theme, Option<(u64, u16, u64, u16)>),
    geometry: &RowBackgroundGeometry,
    snapped_cell_x: &[f32],
    out: &mut Vec<QuadInstance>,
) -> bool
where
    Cells: IntoIterator<Item = &'cell Cell>,
{
    let row_abs = row.view_top_abs + u64::from(row.slot);
    // Cell dimensions already encode DPI in raster pixels; the separate scale key stays 1.0.
    let key = crate::row_quad_cache::row_quad_hash_cells(
        row.view_top_abs,
        row.slot as usize,
        cells,
        style_rev,
        geometry.cell_size.0,
        geometry.cell_size.1,
        geometry.origin.0,
        geometry.origin.1,
        geometry.pane_size.0,
        geometry.pane_size.1,
        selection,
    );
    if let Some(cached) = cache.get(row.pane_id, row_abs, key) {
        // When: `cache.get` is Some — the row's contents, slot, style, geometry and selection overlap are unchanged.
        out.extend_from_slice(&cached.quads);
        return true;
    }
    let base = out.len();
    emit_cell_bg_quads_for_row(
        row.grid,
        row.view_top_abs,
        theme,
        geometry.origin.0,
        geometry.origin.1,
        geometry.cell_size.0,
        geometry.cell_size.1,
        geometry.surface.0,
        geometry.surface.1,
        geometry.max_cols,
        row.slot,
        out,
        snapped_cell_x,
    );
    let quads = out[base..].to_vec();
    cache.insert(row.pane_id, row_abs, key, crate::row_quad_cache::CachedRowQuads { quads });
    false
}

/// Emit background quads for a single visible row. Extracted so the
/// `LineQuadCache` miss path (P2) can call it for one row
/// at a time and capture the resulting quads into the cache.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_cell_bg_quads_for_row(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pad: f32,
    top_inset: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    max_cols: u16,
    r: u16,
    out: &mut Vec<QuadInstance>,
    snapped_cell_x: &[f32],
) {
    {
        let row_abs = view_top_abs + r as u64;
        let Some(row) = grid.row_at_abs(row_abs) else {
            // When: row_at_abs finds no row for row_abs, that absolute line has aged out of scrollback, so this viewport row has no cells to shade.
            return;
        };
        // Run-length encode adjacent same-bg cells into one quad.
        let mut run_start: Option<u16> = None;
        let mut run_color: Option<[f32; 4]> = None;
        let mut col: u16 = 0;
        // derive x/w from the shared snapped-edge cache so bg
        // runs share device-pixel edges with adjacent glyph cells at
        // fractional DPI. Falls back to raw arithmetic if the cache is
        // empty (defensive — production always passes the full cache).
        let raw_fallback = snapped_cell_x.is_empty();
        let flush =
            |start: u16, end_exclusive: u16, color: [f32; 4], out: &mut Vec<QuadInstance>| {
                let clipped_end = end_exclusive.min(max_cols);
                if clipped_end <= start {
                    // When: max_cols pulls clipped_end back to or before start, the run lies outside the pane tile and would push a zero-width quad.
                    return;
                }
                let (x, w) = if raw_fallback {
                    (pad + f32::from(start) * cell_w, f32::from(clipped_end - start) * cell_w)
                } else {
                    // When: raw_fallback is off, the run takes its edges from the shared snapped cache so bg meets the glyph cells exactly at fractional DPI.
                    let lo = snapped_cell_x[start as usize];
                    let hi = snapped_cell_x[clipped_end as usize];
                    (lo, hi - lo)
                };
                let y = top_inset + f32::from(r) * cell_h;
                out.push(QuadInstance::sharp(px_to_ndc(x, y, w, cell_h, sw, sh), color));
            };
        for cell in row.iter().take(max_cols as usize) {
            let bg = cell_bg_rgba(cell, theme);
            match (run_color, bg) {
                (Some(prev), Some(cur)) if prev == cur => {
                    // When: cur repeats prev, so the cell joins the open run and an 80-column fill stays one quad instead of eighty.
                    // extend run
                }
                (Some(prev), _) => {
                    // PANIC: safe — `run_color` and `run_start` are written
                    // together (search this fn for `run_start = ` to see they
                    // are always assigned in the same statement-pair). Matching
                    // `run_color == Some(_)` therefore proves `run_start ==
                    // Some(_)`. Hot per-frame path: no Result conversion.
                    let start = run_start.expect("run_start set when run_color is");
                    flush(start, col, prev, out);
                    run_start = bg.map(|_| col);
                    run_color = bg;
                }
                (None, Some(_)) => {
                    run_start = Some(col);
                    run_color = bg;
                }
                (None, None) => {
                    // When: `run_color` and `bg` are absent, the attachment clear or replacement reset already covers this cell.
                }
            }
            col = col.saturating_add(1);
        }
        if let (Some(start), Some(color)) = (run_start, run_color) {
            flush(start, col, color, out);
        }
    }
}

fn indexed(i: u8, theme: &Theme) -> Option<ChromeColor> {
    let p = &theme.colors;
    let pick = |h: &str| hex_to_chrome_color(h);
    match i {
        0 => Some(pick(p.ansi.black.0.as_str())),
        1 => Some(pick(p.ansi.red.0.as_str())),
        2 => Some(pick(p.ansi.green.0.as_str())),
        3 => Some(pick(p.ansi.yellow.0.as_str())),
        4 => Some(pick(p.ansi.blue.0.as_str())),
        5 => Some(pick(p.ansi.magenta.0.as_str())),
        6 => Some(pick(p.ansi.cyan.0.as_str())),
        7 => Some(pick(p.ansi.white.0.as_str())),
        8 => Some(pick(p.bright.black.0.as_str())),
        9 => Some(pick(p.bright.red.0.as_str())),
        10 => Some(pick(p.bright.green.0.as_str())),
        11 => Some(pick(p.bright.yellow.0.as_str())),
        12 => Some(pick(p.bright.blue.0.as_str())),
        13 => Some(pick(p.bright.magenta.0.as_str())),
        14 => Some(pick(p.bright.cyan.0.as_str())),
        15 => Some(pick(p.bright.white.0.as_str())),
        16..=231 => {
            let v = i - 16;
            let r = v / 36;
            let g = (v / 6) % 6;
            let b = v % 6;
            // When: c is nonzero, the xterm 6x6x6 cube spaces levels at 55 + 40c rather than evenly, so 256-color ramps match other terminals.
            let to8bit = |c: u8| if c == 0 { 0 } else { c * 40 + 55 };
            Some(ChromeColor::rgb(to8bit(r), to8bit(g), to8bit(b)))
        }
        232..=255 => {
            let g = (i - 232) * 10 + 8;
            Some(ChromeColor::rgb(g, g, g))
        }
    }
}

#[cfg(test)]
pub(crate) use core_tests::warm_and_cold_row_glyphs;
#[cfg(test)]
#[path = "core_tests.rs"]
mod core_tests;
// `hex_to_glyphon` and
// `scale_glyphon_alpha` have moved into `crate::color` under the
// renamed `hex_to_chrome_color` / `scale_chrome_text_alpha` names and
// now consume `ChromeColor` instead of `legacy chrome color`.
// Re-export them at the legacy path so callers that imported
// `sonicterm_gpu::core::scale_glyphon_alpha` can switch to the new
// identifier (see `crates/sonicterm-app/tests/drag_visual_feedback.rs`
// for the port). The legacy names are gone from this file entirely;
// any caller that lingers on them will fail to compile (intentional —
// it's the must-pass #4 grep gate's job to catch survivors).
pub use crate::color::scale_chrome_text_alpha;

// `terminal_font_attrs` re-export removed. It returned
// `legacy chrome attrs` which carried per-span family/weight; the
// chrome-text path replaces it with `ChromeAttrs { bold, italic }`
// constructed per-span at the call site. Downstream callers
// (`sonicterm-ui::tab_spans`) build `(text, ChromeColor, ChromeAttrs)`
// span tuples directly. The grid/chrome shape calls reach the loaded
// wezterm font via `FontStack::default_font()` — there is no per-span
// font attribute layer in this path.

/// Walk the grid and collect runs of contiguous cells that share a hyperlink
/// id, per row. Wide-cell continuations don't break a run (they inherit the
/// lead cell's hyperlink). Returns `(row, col_start, col_end_inclusive)`.
#[doc(hidden)]
pub fn collect_hyperlink_runs(grid: &Grid) -> Vec<(u16, u16, u16)> {
    let mut runs = Vec::new();
    for r in 0..grid.rows {
        let row = grid.row(r);
        let mut start: Option<u16> = None;
        let mut current: Option<sonicterm_render_model::boundary::grid::hyperlink::HyperlinkId> =
            None;
        let mut last_col: u16 = 0;
        for (col, cell) in row.iter().enumerate() {
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                // When: WIDE_CONT marks the trailing half of a wide cell, which inherits the lead cell's hyperlink, so it extends the run instead of breaking it.
                if start.is_some() {
                    last_col = col as u16;
                }
                continue;
            }
            match (cell.hyperlink(), current) {
                (Some(hid), Some(cur)) if hid == cur => {
                    last_col = col as u16;
                }
                (Some(hid), _) => {
                    if let (Some(s), Some(_)) = (start, current) {
                        runs.push((r, s, last_col));
                    }
                    start = Some(col as u16);
                    current = Some(hid);
                    last_col = col as u16;
                }
                (None, Some(_)) => {
                    if let Some(s) = start.take() {
                        runs.push((r, s, last_col));
                    }
                    current = None;
                }
                (None, None) => {
                    // When: the cell carries no hyperlink and current is unset, there is no run to open or close, so the walk just advances.
                }
            }
        }
        if let (Some(s), Some(_)) = (start, current) {
            runs.push((r, s, last_col));
        }
    }
    runs
}

/// Stable fingerprint of a tab's command chrome: the status kind and the badge drawn for it at
/// `now` on a tab whose activity is `is_active`. It changes only when the drawn badge appears,
/// changes or disappears, or when the status kind changes, never on an undrawn elapsed second.
#[doc(hidden)]
pub fn command_status_hash(
    status: &sonicterm_render_model::boundary::ui::tabs::CommandStatus,
    now: Instant,
    is_active: bool,
) -> u64 {
    use sonicterm_render_model::boundary::ui::tabs::CommandStatus;
    use std::hash::{Hash, Hasher};
    // The kind keeps Idle and an unbadged running tab apart; `exit` picks the drawn mark.
    let kind: u8 = match status {
        CommandStatus::Idle => 0,
        CommandStatus::Running(_) => 1,
        CommandStatus::Done { .. } => 2,
    };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    kind.hash(&mut hash);
    status.clone().badge(now, is_active).hash(&mut hash);
    hash.finish()
}

/// Compute the per-row selection quad rects (in physical pixels) that the
/// renderer would emit for `sel` against a grid of `rows` × `cols`, anchored
/// at `(origin_x, origin_y)` with `cell_w × cell_h` cells.
///
/// Pure helper, no clipping applied — pair with [`clip_rect_to_pane`] before
/// pushing to the GPU. Exposed so integration tests can verify the
/// pre-clip / post-clip relationship without standing up a real surface.
///
/// Each returned tuple is `(x, y, w, h)` in physical pixels.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn selection_quad_rects(
    sel: &sonicterm_render_model::boundary::ui::selection::Selection,
    view_top_abs: u64,
    rows: u16,
    cols: u16,
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    snapped_cell_x: &[f32],
) -> Vec<(f32, f32, f32, f32)> {
    if sel.is_empty() {
        // When: sel covers no cells, returning an empty vec drops the previous frame's highlight rather than leaving a stale rect on screen.
        return Vec::new();
    }
    let (a, b) = sel.normalized();
    let mut out = Vec::with_capacity(usize::from(rows));
    // derive each row's x/w from the shared snapped-edge cache so
    // selection rects share device-pixel edges with adjacent glyph
    // cells at fractional DPI. Empty-cache fallback preserves the old
    // raw-arithmetic behavior for callers (debug/test helpers) that
    // don't carry a real cache; integer scales make the two identical.
    let raw_fallback = snapped_cell_x.is_empty();
    // Selection rows are scrollback-ABSOLUTE. Only the absolute rows that
    // intersect the viewport produce quads, so bound the walk to
    // `[max(a.0, view_top_abs) ..= min(b.0, view_top_abs + rows - 1)]`.
    // This keeps per-frame cost O(viewport rows) even when the selection
    // spans a huge multi-screen region of scrollback. The first/last-row
    // column tests still compare against the true `a.0`/`b.0` (which may
    // sit off-screen), so partial first/last rows render correctly.
    if rows == 0 {
        // When: rows is zero the viewport has no line to highlight, and `rows as u64 - 1` below would underflow into a bogus bottom bound.
        return out;
    }
    let view_bottom_abs = view_top_abs + (rows as u64 - 1);
    let first_abs = a.0.max(view_top_abs);
    let last_abs = b.0.min(view_bottom_abs);
    if first_abs > last_abs {
        // When: first_abs passes last_abs, no absolute selection row intersects the viewport, so a selection deep in scrollback costs no per-row walk.
        return out; // selection entirely above or below the viewport
    }
    for abs_r in first_abs..=last_abs {
        let vr = (abs_r - view_top_abs) as u16;
        // When: abs_r is past a.0 the row starts mid-selection, so col_a falls back to 0 and the highlight spans from the left edge.
        let col_a = if abs_r == a.0 { a.1 } else { 0 };
        // Note: do NOT clamp `col_b` to `cols - 1` here. The selection may
        // legitimately reach the grid's last column, and the per-pane clip
        // below trims any pixel overhang. Clamping pre-clip would silently
        // shrink the selection on the last row when the user dragged past
        // the rightmost cell — which is precisely the path that hides
        // bugs like the split-pane bleed-through.

        // When: abs_r sits before b.0 the row ends mid-selection, so col_b runs to the last column and the highlight reads as continuous.
        let col_b = if abs_r == b.0 { b.1 } else { cols.saturating_sub(1) };
        if col_b < col_a {
            // When: col_b lands left of col_a the row holds no selected span, and `end_exclusive - col_a` would wrap on u16.
            continue;
        }
        let end_exclusive = col_b.saturating_add(1);
        let (x, w) = if raw_fallback {
            (origin_x + f32::from(col_a) * cell_w, f32::from(end_exclusive - col_a) * cell_w)
        } else {
            // When: raw_fallback is off, the rect takes its edges from the shared snapped cache so selection meets the glyph cells exactly at fractional DPI.

            // Clamp the right edge to the cache bounds (`cols + 1`); a
            // selection that touches col `cols - 1` reads `snapped[cols]`.
            let cache_end = end_exclusive.min((snapped_cell_x.len() - 1) as u16);
            if cache_end <= col_a {
                // When: the cache clamp pulls cache_end back to or before col_a, the row's span falls outside the cached edges and would be zero-width.
                continue;
            }
            let lo = snapped_cell_x[col_a as usize];
            let hi = snapped_cell_x[cache_end as usize];
            (lo, hi - lo)
        };
        let y = origin_y + f32::from(vr) * cell_h;
        out.push((x, y, w, cell_h));
    }
    out
}

/// Clip a quad rect (in physical pixels) to the active pane's bounding box.
/// Returns `None` if the rect is entirely outside the pane.
///
/// Selection / cursor / overlay quads are anchored to the active pane's
/// origin and can extend past its right or bottom edge when the user drags
/// beyond the pane (or the cursor temporarily sits outside the grid bounds
/// due to a resize race). Pushing the unclipped quad would paint into the
/// neighbouring pane in a split layout — see the regression test for
/// the split-right drag-select bug.
#[doc(hidden)]
pub fn clip_rect_to_pane(
    rect: (f32, f32, f32, f32),
    pane_x: f32,
    pane_y: f32,
    pane_w: f32,
    pane_h: f32,
) -> Option<(f32, f32, f32, f32)> {
    let (x, y, w, h) = rect;
    let clipped_x = x.max(pane_x);
    let clipped_right = (x + w).min(pane_x + pane_w);
    let clipped_y = y.max(pane_y);
    let clipped_bottom = (y + h).min(pane_y + pane_h);
    let clipped_w = clipped_right - clipped_x;
    let clipped_h = clipped_bottom - clipped_y;
    if clipped_w > 0.0 && clipped_h > 0.0 {
        Some((clipped_x, clipped_y, clipped_w, clipped_h))
    } else {
        // When: clipped_w or clipped_h collapses to zero, the rect lies wholly outside the pane; returning nothing keeps it out of the neighbour's tile.
        None
    }
}
