//! Wezterm-driven chrome text helper.
//!
//! Every chrome string — tab titles, search input, palette query/rows/footer,
//! IME preedit, broadcast banner, drag-chip title — flows through the same
//! `FontStack` raster path, `GlyphAtlas`, and `WeztermPipeline` the terminal grid
//! uses. One font system, one atlas, one render pass: chrome and grid text
//! cannot drift apart in shaping, hinting, or colour handling, and a glyph
//! rasterized for either is already warm for the other.
//!
//! ## Pipeline
//!
//! 1. `WtChromeRun::layout`  — sonicterm-font shapes the text run.
//! 2. `GlyphAtlas::get_or_insert` caches the rasterized tile under font,
//!    style, glyph, and native-raster-role identity. Body, footer, and tab-title
//!    tiles coexist in one atlas without sharing differently sized bitmaps.
//! 3. `GlyphInstance` records are pushed into a caller-owned `Vec`;
//!    the caller hands the vec to the production
//!    [`crate::wezterm_pipeline::WeztermPipeline`] for the draw call.
//!
//! The chrome ride-shares the terminal text pipeline: a separate vec, the
//! same pipeline, so no extra wgpu binding setup, render pass, or shader.
//!
//! ## Font size scaling
//!
//! Most chrome uses the body `FontStack` and normal raster role. Surfaces with
//! a distinct native size use a matching stack and raster role, keeping
//! `font_size_px == native_em_px` so their atlas tiles project 1:1.

use sonicterm_engine::FontStack;
use sonicterm_text::glyph_atlas::{GlyphAtlas, Rasterizer};
use sonicterm_text::GlyphInstance;
use sonicterm_types::{GlyphKey, GlyphRasterVariant};
use unicode_width::UnicodeWidthChar;

use crate::color::{chrome_color_to_linear_rgba, ChromeColor};
use crate::frame_stats::CountingRasterizer;
use crate::quad::{with_premultiplied_alpha, QuadInstance};

#[cfg(test)]
#[path = "chrome_text_tests.rs"]
mod chrome_text_tests;

/// Result of laying out a chrome text run into atlas glyph instances.
///
/// `glyphs` is ready to be appended to the caller's `Vec<GlyphInstance>`
/// before it is handed to [`crate::wezterm_pipeline::WeztermPipeline`].
/// `width_px` / `height_px` are the raster-px bounding box (origin
/// inclusive) of the laid-out text — useful for centering / right-align
/// callers that need to know where the run ended.
#[derive(Debug, Clone)]
pub struct ChromeTextLayout {
    /// One `GlyphInstance` per visible chrome glyph, in left-to-right
    /// order. Already in screen-px NDC via the supplied `(sw, sh)`.
    pub glyphs: Vec<GlyphInstance>,
    /// Total advance in raster px from the origin to the right edge of
    /// the last glyph. Zero when no glyphs were emitted (empty text,
    /// or every glyph fell outside the clip bounds).
    pub width_px: f32,
    /// Vertical extent in raster px (max glyph height encountered),
    /// useful for sizing a caller-drawn background quad.
    pub height_px: f32,
    /// Outline quads for glyphs the atlas cached as missing (tofu), already in NDC.
    /// Callers push them into the quad list drawn under this text's glyph pass.
    pub missing_boxes: Vec<QuadInstance>,
}

thread_local! {
    /// Characters chrome layouts drew as tofu in the innermost open [`MissingChromeScope`];
    /// `None` when no scope is open on this thread.
    static MISSING_CHROME: std::cell::RefCell<Option<Vec<char>>> =
        const { std::cell::RefCell::new(None) };
}

/// One frame's collection of chrome characters drawn without a glyph, the chrome counterpart of
/// the terminal rows' missing list. While it is open, every chrome layout on this thread notes the
/// characters it draws as tofu or drops, so call sites need no plumbing. Scopes nest: an inner
/// scope keeps its own list and restores the enclosing one when it closes.
#[must_use = "a scope collects only while it is held"]
pub(crate) struct MissingChromeScope {
    /// The enclosing scope's list, restored when this scope closes.
    enclosing: Option<Option<Vec<char>>>,
}

impl MissingChromeScope {
    /// Open an empty collection on this thread.
    pub(crate) fn enter() -> Self {
        let enclosing = MISSING_CHROME.with(|cell| cell.replace(Some(Vec::new())));
        Self { enclosing: Some(enclosing) }
    }

    /// Close the scope and return what it collected, in drawing order.
    pub(crate) fn finish(mut self) -> Vec<char> {
        self.close()
    }

    /// Restore the enclosing list and take this scope's; empty after the first call.
    fn close(&mut self) -> Vec<char> {
        let Some(enclosing) = self.enclosing.take() else {
            // When: `enclosing` was already taken, the scope is closed and holds nothing.
            return Vec::new();
        };
        MISSING_CHROME.with(|cell| cell.replace(enclosing)).unwrap_or_default()
    }
}

// Lifecycle: an unfinished MissingChromeScope calls close on drop: its list is discarded and
// the enclosing one restored.
impl Drop for MissingChromeScope {
    fn drop(&mut self) {
        let _discarded = self.close();
    }
}

/// Note `missing` as drawn without a glyph when a scope is open; nothing happens otherwise.
pub(crate) fn note_missing_chrome(missing: char) {
    MISSING_CHROME.with(|cell| {
        if let Some(list) = cell.borrow_mut().as_mut() {
            // An open scope collects the character into this frame's chrome readout.
            list.push(missing);
        }
    });
}

/// Note every visible character of a chrome run that could not be shaped and so drew nothing.
/// Blanks are intentionally empty and are not missing, as on the terminal rows.
pub(crate) fn note_unshaped_chrome(text: &str) {
    for character in text.chars().filter(|character| !character.is_whitespace()) {
        note_missing_chrome(character);
    }
}

/// Opacity of a chrome tofu outline, matching the terminal grid's missing-glyph box.
const MISSING_BOX_ALPHA: f32 = 0.55;
/// Share of the run's font size a tofu outline stands above the baseline, as an ascent.
const MISSING_BOX_ASCENT_RATIO: f32 = 0.8;

/// Push a one-pixel outline box `(x, y, width, height)` in raster px as four edge quads, each
/// cut to `clip` (snapped to whole pixels, as field glyphs are) and dropped when nothing is left.
/// Returns whether any edge was pushed.
fn push_missing_box(
    out: &mut Vec<QuadInstance>,
    rect_px: [f32; 4],
    rgba: [f32; 4],
    screen: (f32, f32),
    clip: Option<ChromeClip>,
) -> bool {
    let [left, top, width, height] = rect_px;
    let (sw, sh) = screen;
    let thickness = 1.0_f32;
    let bounds = clip.map(|area| {
        let (clip_left, clip_top) = (area.x.round(), area.y.round());
        [clip_left, clip_top, (area.x + area.w).round(), (area.y + area.h).round()]
    });
    let mut pushed = false;
    for [edge_left, edge_top, edge_width, edge_height] in [
        [left, top, width, thickness],
        [left, top + height - thickness, width, thickness],
        [left, top, thickness, height],
        [left + width - thickness, top, thickness, height],
    ] {
        let (mut from_x, mut from_y) = (edge_left, edge_top);
        let (mut to_x, mut to_y) = (edge_left + edge_width, edge_top + edge_height);
        // A set clip cuts the edge to it, so a box straddling a scrolled field edge never
        // paints outside the field.
        if let Some([clip_left, clip_top, clip_right, clip_bottom]) = bounds {
            (from_x, from_y) = (from_x.max(clip_left), from_y.max(clip_top));
            (to_x, to_y) = (to_x.min(clip_right), to_y.min(clip_bottom));
        }
        if to_x <= from_x || to_y <= from_y {
            // When: to_x <= from_x or to_y <= from_y, no part of the edge is inside the clip.
            continue;
        }
        out.push(QuadInstance {
            rect: px_to_ndc(from_x, from_y, to_x - from_x, to_y - from_y, sw, sh),
            color: rgba,
            ..Default::default()
        });
        pushed = true;
    }
    pushed
}

/// Optional clip rect for chrome runs that paint inside a modal
/// (palette, IME). Glyphs that fall entirely outside this rect are
/// skipped. Coordinates are raster px in the same frame as `origin`.
///
/// Pass `None` for chrome that paints anywhere in the window
/// (tab titles, drag chip).
#[derive(Debug, Clone, Copy)]
pub struct ChromeClip {
    /// Left edge, raster px.
    pub x: f32,
    /// Top edge, raster px.
    pub y: f32,
    /// Width, raster px.
    pub w: f32,
    /// Height, raster px.
    pub h: f32,
}

/// Single attribute bundle for a chrome run — the bits a font shaper
/// re-resolves the face for. Color is per-instance (passed separately)
/// so two runs that share `(bold, italic)` can still paint in different
/// colors without re-shaping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChromeAttrs {
    /// True when the run should be shaped as bold.
    pub bold: bool,
    /// True when the run should be shaped as italic.
    pub italic: bool,
}

/// Layout one chrome text run into the supplied atlas + glyph vec.
///
/// Arguments:
///
/// - `font_stack`: SonicTerm's WezTerm-compatible font stack.
/// - `wt_raster`: rasterizer that backs the atlas. Normal chrome uses the
///   body stack; native-size roles use the matching title/footer stack.
/// - `atlas`: shared glyph atlas. Chrome and grid coexist in one atlas.
/// - `text`: the text to lay out. UTF-8; handled per codepoint (no
///   ligature shaping across span boundaries — call once per styled
///   span).
/// - `color`: foreground for monochrome glyphs. Color-glyph runs
///   (emoji) ignore this and paint from the strike's own colors.
/// - `attrs`: bold / italic — only used to derive the atlas key for
///   now; wezterm's font selection happens through the loaded face.
/// - `font_size_px`: requested chrome glyph size in raster px.
/// - `native_em_px`: the supplied stack's native em in raster px. Native-size
///   callers pass the same value as `font_size_px` for 1:1 projection.
/// - `origin`: `(x, baseline_y)` of the run in raster px. The
///   baseline matches the row that grid text would paint on.
/// - `screen`: `(sw, sh)` raster-px dimensions of the surface, used
///   to project rects into NDC.
/// - `clip`: optional bounding rect that culls glyphs outside the
///   modal (palette, IME). Pass `None` for tab titles / drag chip.
///
/// Returns a [`ChromeTextLayout`] whose `glyphs` are ready for
/// [`crate::wezterm_pipeline::WeztermPipeline::draw_frame`].
#[allow(clippy::too_many_arguments)]
pub fn layout(
    font_stack: &FontStack,
    wt_raster: &mut impl Rasterizer,
    atlas: &mut GlyphAtlas,
    text: &str,
    color: ChromeColor,
    attrs: ChromeAttrs,
    font_size_px: f32,
    native_em_px: f32,
    origin: (f32, f32),
    screen: (f32, f32),
    clip: Option<ChromeClip>,
) -> ChromeTextLayout {
    layout_with_raster_variant(
        font_stack,
        wt_raster,
        atlas,
        text,
        color,
        attrs,
        font_size_px,
        native_em_px,
        origin,
        screen,
        clip,
        GlyphRasterVariant::Normal,
    )
}

/// Layout one chrome run with an explicit native raster role.
#[allow(clippy::too_many_arguments)]
pub fn layout_with_raster_variant(
    font_stack: &FontStack,
    wt_raster: &mut impl Rasterizer,
    atlas: &mut GlyphAtlas,
    text: &str,
    color: ChromeColor,
    attrs: ChromeAttrs,
    font_size_px: f32,
    native_em_px: f32,
    origin: (f32, f32),
    screen: (f32, f32),
    clip: Option<ChromeClip>,
    raster_variant: GlyphRasterVariant,
) -> ChromeTextLayout {
    layout_with_raster_variant_impl(
        font_stack,
        wt_raster,
        atlas,
        text,
        color,
        attrs,
        font_size_px,
        native_em_px,
        origin,
        screen,
        clip,
        raster_variant,
    )
}

/// Combine the shaped pen, raster bearing, and HarfBuzz offset before pixel snapping.
fn positioned_glyph_origin(
    pen_x: f32,
    raster_offset_x: f32,
    shape_offset_x: f32,
    baseline_y: f32,
    raster_offset_y: f32,
    shape_offset_y: f32,
) -> (f32, f32) {
    (
        (pen_x + raster_offset_x + shape_offset_x).round(),
        (baseline_y + raster_offset_y + shape_offset_y).round(),
    )
}

/// Pen advance of a blank cluster the shaper reported as notdef: the shaped
/// advance, or half an em per display column when the shaper reported none,
/// so the next glyph does not overlap the blank.
fn blank_pen_advance(shaped_px: f32, lead_ch: char, font_size_px: f32) -> f32 {
    if shaped_px > 0.0 {
        shaped_px
    } else {
        // When: `shaped_px` is zero because some shapers report no advance for an ASCII
        // space, the unicode-width estimate of `lead_ch` spaces the run instead.
        UnicodeWidthChar::width(lead_ch).unwrap_or(0) as f32 * font_size_px * 0.5
    }
}

/// Ratio that projects atlas-native tiles and advances to the requested size.
fn projection_scale(font_size_px: f32, native_em_px: f32) -> f32 {
    if native_em_px > 0.0 {
        font_size_px / native_em_px
    } else {
        // When: `native_em_px` is not positive the ratio would be non-finite, so tiles
        // project at their rasterized size instead of collapsing the whole run.
        1.0
    }
}

/// The character whose UTF-8 bytes hold `byte`, or the last character when
/// `byte` lies past the text, the way the layout pen resolves a cluster's lead.
fn cluster_lead_char(text: &str, byte: usize) -> char {
    text.char_indices()
        .take_while(|(start, _)| *start <= byte)
        .last()
        .map_or(' ', |(_, character)| character)
}

/// One shaped glyph of a [`ChromeShapedRun`], already projected to the requested size.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ChromeShapedGlyph {
    /// Cluster start byte in the run text.
    cluster: usize,
    /// Lead character of the cluster, used for blank detection and notdef keys.
    lead_ch: char,
    /// Fallback font slot, saturated to `u8::MAX`.
    font_idx: u8,
    /// Shaper glyph id; zero is notdef.
    glyph_pos: u32,
    /// Projected shaper advance, before the blank-cluster fallback.
    x_advance_px: f32,
    /// Projected shaper x offset.
    x_offset_px: f32,
    /// Projected shaper y offset, positive down.
    y_offset_px: f32,
}

impl ChromeShapedGlyph {
    /// Whether this glyph is a notdef blank (space, control, NUL) that draws no tile.
    fn is_blank(&self) -> bool {
        self.glyph_pos == 0 && (self.lead_ch == '\0' || self.lead_ch.is_whitespace())
    }
}

/// One chrome run shaped once, reusable for both measurement and painting.
///
/// A caller that needs field geometry and painted glyphs for the same text
/// shapes it once here, builds [`crate::field_geometry::FieldBoundaries`] with
/// `from_run`, and paints with [`layout_prepared`], so caret, highlight, and
/// glyphs cannot come from two different shaping results. The run is transient:
/// it borrows the text and is dropped at the end of the frame.
#[derive(Debug, Clone)]
pub struct ChromeShapedRun<'text> {
    /// Text the run was shaped from.
    text: &'text str,
    /// Style the run was shaped with; it keys the atlas tiles.
    attrs: ChromeAttrs,
    /// Requested size the advances are projected to.
    font_size_px: f32,
    /// Atlas-native to requested size ratio.
    scale: f32,
    /// Shaped glyphs in left-to-right order.
    glyphs: Vec<ChromeShapedGlyph>,
}

impl<'text> ChromeShapedRun<'text> {
    /// Shape `text` with `font_stack`, projecting advances from `native_em_px` to
    /// `font_size_px`. Returns `None` when the run cannot be shaped; an empty text
    /// is a valid empty run.
    #[must_use]
    pub fn shape(
        font_stack: &FontStack,
        text: &'text str,
        attrs: ChromeAttrs,
        font_size_px: f32,
        native_em_px: f32,
    ) -> Option<Self> {
        let scale = projection_scale(font_size_px, native_em_px);
        let shaped = if text.is_empty() {
            // An empty `text` has nothing to shape; the run is valid and zero-width.
            Vec::new()
        } else {
            // When: `text` is not empty, shape it; a shaping failure means no run at all.
            let shaped = crate::frame_stats::shape_request(|| {
                font_stack.shape_text_for_frame(text, attrs.bold, attrs.italic)
            });
            inject_chrome_shape_failure(shaped).ok()?
        };
        let glyphs = shaped
            .iter()
            .map(|glyph| {
                let cluster = glyph.cluster as usize;
                ChromeShapedGlyph {
                    cluster,
                    lead_ch: cluster_lead_char(text, cluster),
                    font_idx: u8::try_from(glyph.font_idx).unwrap_or(u8::MAX),
                    glyph_pos: glyph.glyph_pos,
                    x_advance_px: glyph.x_advance.get() as f32 * scale,
                    x_offset_px: glyph.x_offset.get() as f32 * scale,
                    y_offset_px: glyph.y_offset.get() as f32 * scale,
                }
            })
            .collect();
        Some(Self { text, attrs, font_size_px, scale, glyphs })
    }

    /// Text the run was shaped from.
    #[must_use]
    pub fn text(&self) -> &'text str {
        self.text
    }

    /// A borrowed view of this run, the form [`layout_view`] and field geometry consume.
    #[must_use]
    pub fn view(&self) -> ChromeRunView<'_> {
        ChromeRunView {
            text: self.text,
            attrs: self.attrs,
            font_size_px: self.font_size_px,
            scale: self.scale,
            glyphs: &self.glyphs,
        }
    }

    /// `(cluster byte, lead character, atlas key)` for each glyph in left-to-right order, keyed
    /// as [`layout_prepared`] keys it for `raster_variant`; the key is `None` for a notdef blank,
    /// which draws no tile.
    pub fn tile_keys(
        &self,
        raster_variant: GlyphRasterVariant,
    ) -> impl Iterator<Item = (usize, char, Option<GlyphKey>)> + '_ {
        self.glyphs.iter().map(move |glyph| {
            (glyph.cluster, glyph.lead_ch, glyph_tile_key(glyph, self.attrs, raster_variant))
        })
    }

    /// `(cluster byte, pen advance)` pairs in left-to-right order, exactly as
    /// [`layout_prepared`] moves the pen.
    pub fn advances(&self) -> impl Iterator<Item = (usize, f32)> + '_ {
        let font_size_px = self.font_size_px;
        self.glyphs.iter().map(move |glyph| (glyph.cluster, pen_advance(glyph, font_size_px)))
    }
}

/// Pen advance of one glyph under the drawing rules: blank notdef clusters fall back to their
/// display width when the shaper reports none.
fn pen_advance(glyph: &ChromeShapedGlyph, font_size_px: f32) -> f32 {
    if glyph.is_blank() {
        // A blank notdef cluster is skipped without a tile, so its width may be estimated.
        blank_pen_advance(glyph.x_advance_px, glyph.lead_ch, font_size_px)
    } else {
        // When: `glyph` is not blank, the shaper's advance moves the pen unchanged.
        glyph.x_advance_px
    }
}

/// A borrowed, copyable view of one shaped chrome run: its text, style, size and glyphs.
///
/// Both a transient [`ChromeShapedRun`] and a run a renderer cache keeps hand out this view, so
/// [`layout_view`] and [`crate::field_geometry::FieldBoundaries::from_view`] draw and measure
/// either one without copying glyphs.
#[derive(Debug, Clone, Copy)]
pub struct ChromeRunView<'run> {
    /// Text the run was shaped from.
    text: &'run str,
    /// Style the run was shaped with; it keys the atlas tiles.
    attrs: ChromeAttrs,
    /// Requested size the advances are projected to.
    font_size_px: f32,
    /// Atlas-native to requested size ratio.
    scale: f32,
    /// Shaped glyphs in left-to-right order.
    glyphs: &'run [ChromeShapedGlyph],
}

impl<'run> ChromeRunView<'run> {
    /// Text the run was shaped from.
    #[must_use]
    pub fn text(&self) -> &'run str {
        self.text
    }

    /// `(cluster byte, pen advance)` pairs in left-to-right order, exactly as [`layout_view`]
    /// moves the pen: blank notdef clusters use their display-width estimate. For drawing and
    /// field geometry; measures use [`Self::raw_width_px`].
    pub fn advances(&self) -> impl Iterator<Item = (usize, f32)> + 'run {
        let font_size_px = self.font_size_px;
        self.glyphs.iter().map(move |glyph| (glyph.cluster, pen_advance(glyph, font_size_px)))
    }

    /// Sum of the projected shaper advances, in glyph order and with no blank-cluster estimate:
    /// the sum `FontStack::measure_text_width_for_frame` takes when the run was shaped at its
    /// native size (scale 1). Only measures read it; drawing uses [`Self::advances`].
    #[must_use]
    pub fn raw_width_px(&self) -> f32 {
        self.glyphs.iter().map(|glyph| glyph.x_advance_px).sum()
    }

    /// Number of shaped glyphs in the run.
    #[must_use]
    pub fn glyph_count(&self) -> usize {
        self.glyphs.len()
    }

    /// Test seam: the shaper glyph ids in order; 0 is notdef.
    #[cfg(test)]
    pub(crate) fn glyph_ids_for_test(&self) -> Vec<u32> {
        self.glyphs.iter().map(|glyph| glyph.glyph_pos).collect()
    }
}

/// A shaped chrome run that owns its text and glyphs, so a renderer cache can keep it across
/// frames. It holds exactly one text allocation and one glyph slice.
#[derive(Debug, Clone)]
pub(crate) struct PreparedChromeRun {
    /// Text the run was shaped from; the run's only text allocation.
    text: Box<str>,
    /// Style the run was shaped with.
    attrs: ChromeAttrs,
    /// Requested size the advances are projected to.
    font_size_px: f32,
    /// Atlas-native to requested size ratio.
    scale: f32,
    /// Shaped glyphs in left-to-right order.
    glyphs: Box<[ChromeShapedGlyph]>,
}

impl PreparedChromeRun {
    /// Take ownership of a transient run: one text allocation, glyphs moved into a boxed slice.
    pub(crate) fn from_run(run: ChromeShapedRun<'_>) -> Self {
        Self {
            text: Box::from(run.text),
            attrs: run.attrs,
            font_size_px: run.font_size_px,
            scale: run.scale,
            glyphs: run.glyphs.into_boxed_slice(),
        }
    }

    /// A borrowed view of the kept run.
    pub(crate) fn view(&self) -> ChromeRunView<'_> {
        ChromeRunView {
            text: &self.text,
            attrs: self.attrs,
            font_size_px: self.font_size_px,
            scale: self.scale,
            glyphs: &self.glyphs,
        }
    }

    /// Heap bytes the run retains: its text and its glyph slice.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.text.len() + self.glyphs.len() * std::mem::size_of::<ChromeShapedGlyph>()
    }
}

/// Size of one kept shaped glyph, for retention envelopes outside this module.
#[cfg(test)]
pub(crate) const CHROME_SHAPED_GLYPH_BYTES: usize = std::mem::size_of::<ChromeShapedGlyph>();

#[cfg(test)]
thread_local! {
    /// Test seam: how many more chrome shaping calls on this thread succeed before one fails;
    /// `None` when no failure is armed.
    static FAIL_CHROME_SHAPE_AFTER: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

/// Test seam: let `successes` more [`ChromeShapedRun::shape`] calls on this thread succeed, then
/// fail the next one once, after its request is counted, as a real shaping error would.
#[cfg(test)]
pub(crate) fn fail_chrome_shape_after_for_test(successes: usize) {
    FAIL_CHROME_SHAPE_AFTER.with(|armed| armed.set(Some(successes)));
}

/// `shaped`, or an error in its place when the armed seam reaches this call, consuming it.
#[cfg(test)]
fn inject_chrome_shape_failure<T>(shaped: anyhow::Result<T>) -> anyhow::Result<T> {
    let fails = FAIL_CHROME_SHAPE_AFTER.with(|armed| match armed.get() {
        Some(0) => {
            armed.set(None);
            true
        }
        Some(remaining) => {
            armed.set(Some(remaining - 1));
            false
        }
        None => false,
    });
    if fails {
        // When: the armed seam reached this call, the chrome shape fails as a real error would.
        return Err(anyhow::anyhow!("injected chrome shaping failure"));
    }
    shaped
}

/// Production never injects a chrome shaping failure: the result passes through.
#[cfg(not(test))]
#[inline(always)]
fn inject_chrome_shape_failure<T>(shaped: anyhow::Result<T>) -> anyhow::Result<T> {
    shaped
}

/// The atlas key `glyph` draws with under `attrs` and `raster_variant`, or `None` for a notdef
/// blank. A real glyph id keys by `(font slot, glyph id)`, as the grid path does; a visible notdef
/// keys by `(char, slot 0)` so the rasterizer resolves it through the charmap.
fn glyph_tile_key(
    glyph: &ChromeShapedGlyph,
    attrs: ChromeAttrs,
    raster_variant: GlyphRasterVariant,
) -> Option<GlyphKey> {
    let key = if glyph.glyph_pos != 0 {
        GlyphKey::shaped(glyph.lead_ch, glyph.font_idx, glyph.glyph_pos, attrs.bold, attrs.italic)
    } else if glyph.is_blank() {
        // When: glyph.is_blank() is a notdef space, control or NUL, it has no pixels and no slot.
        return None;
    } else {
        // When: glyph_pos is zero for a visible character, the charmap resolves it from slot 0.
        GlyphKey::with_slot(glyph.lead_ch, 0, attrs.bold, attrs.italic)
    };
    Some(key.with_raster_variant(raster_variant))
}

/// Pen advances of one chrome run as `(cluster byte offset, advance)` pairs in
/// left-to-right order.
///
/// They come from the same shaping call, projection scale and pen rules that
/// [`layout`] draws with, so on a drawable surface they sum to its
/// `width_px`, and a glyph missing from the atlas still advances the pen.
/// Returns `None` when the run cannot be shaped, in which case nothing is drawn.
pub fn shaped_advances(
    font_stack: &FontStack,
    text: &str,
    attrs: ChromeAttrs,
    font_size_px: f32,
    native_em_px: f32,
) -> Option<Vec<(usize, f32)>> {
    ChromeShapedRun::shape(font_stack, text, attrs, font_size_px, native_em_px)
        .map(|run| run.advances().collect())
}

#[allow(clippy::too_many_arguments)]
fn layout_with_raster_variant_impl(
    font_stack: &FontStack,
    wt_raster: &mut impl Rasterizer,
    atlas: &mut GlyphAtlas,
    text: &str,
    color: ChromeColor,
    attrs: ChromeAttrs,
    font_size_px: f32,
    native_em_px: f32,
    origin: (f32, f32),
    screen: (f32, f32),
    clip: Option<ChromeClip>,
    raster_variant: GlyphRasterVariant,
) -> ChromeTextLayout {
    let empty = ChromeTextLayout {
        glyphs: Vec::new(),
        width_px: 0.0,
        height_px: 0.0,
        missing_boxes: Vec::new(),
    };
    if text.is_empty() || screen.0 <= 0.0 || screen.1 <= 0.0 {
        // When: there is no text or no drawable surface, nothing is shaped and the
        // run measures zero, as `layout_prepared` would report for it.
        return empty;
    }
    match ChromeShapedRun::shape(font_stack, text, attrs, font_size_px, native_em_px) {
        Some(run) => {
            layout_prepared(&run, wt_raster, atlas, color, origin, screen, clip, raster_variant)
        }
        // A failed shaping leaves no glyph identities to key the atlas by, so the
        // run is dropped rather than painted from guessed ids, and reported as missing.
        None => {
            note_unshaped_chrome(text);
            empty
        }
    }
}

/// Paint an already shaped run into atlas glyph instances.
///
/// The pen moves by exactly [`ChromeShapedRun::advances`], so a caller that
/// measured field boundaries from the same run paints glyphs on those
/// boundaries. Arguments other than the run match [`layout_with_raster_variant`].
#[allow(clippy::too_many_arguments)]
pub fn layout_prepared(
    run: &ChromeShapedRun<'_>,
    wt_raster: &mut impl Rasterizer,
    atlas: &mut GlyphAtlas,
    color: ChromeColor,
    origin: (f32, f32),
    screen: (f32, f32),
    clip: Option<ChromeClip>,
    raster_variant: GlyphRasterVariant,
) -> ChromeTextLayout {
    layout_view(run.view(), wt_raster, atlas, color, origin, screen, clip, raster_variant)
}

/// Paint a borrowed run view into atlas glyph instances: the body behind [`layout_prepared`],
/// also used for runs a renderer cache keeps. The pen moves by exactly
/// [`ChromeRunView::advances`], so field boundaries built from the same view line up.
#[allow(clippy::too_many_arguments)]
pub fn layout_view(
    run: ChromeRunView<'_>,
    wt_raster: &mut impl Rasterizer,
    atlas: &mut GlyphAtlas,
    color: ChromeColor,
    origin: (f32, f32),
    screen: (f32, f32),
    clip: Option<ChromeClip>,
    raster_variant: GlyphRasterVariant,
) -> ChromeTextLayout {
    let mut out = ChromeTextLayout {
        glyphs: Vec::new(),
        width_px: 0.0,
        height_px: 0.0,
        missing_boxes: Vec::new(),
    };
    let (sw, sh) = screen;
    if run.glyphs.is_empty() || sw <= 0.0 || sh <= 0.0 {
        // When: the run is empty, or sw or sh is not positive so px_to_ndc would divide by
        // zero, the run is dropped rather than emitting glyphs at non-finite NDC coordinates.
        return out;
    }
    let attrs = run.attrs;
    let scale = run.scale;
    let rgba = chrome_color_to_linear_rgba(color);
    // Apply the requested alpha. `chrome_color_to_linear_rgba` returns `a = 1.0`, so
    // dimmed chrome (the drag-chip ghost) multiplies its reduced `color.a` through for
    // the premultiplied blend the pipeline expects.
    let alpha = color.a() as f32 / 255.0;

    // The pen is fractional and driven by the shaped advances (matches the grid path and
    // preserves ligature widths); only each glyph's draw origin is snapped.
    let mut pen_x = origin.0;
    let baseline_y = origin.1;
    let mut max_y_extent: f32 = 0.0;

    for glyph in run.glyphs {
        let advance = pen_advance(glyph, run.font_size_px);
        let Some(key) = glyph_tile_key(glyph, attrs, raster_variant) else {
            // When: a notdef blank (space, control, NUL) has no pixels, the pen advances
            // without consuming an atlas slot that a visible glyph needs.
            pen_x += advance;
            continue;
        };

        let Some(info) = atlas.get_or_insert(key, &mut CountingRasterizer::new(wt_raster)) else {
            // When: get_or_insert returns None the tile was not placed; the glyph is dropped
            // this frame but the pen still advances, matching the shaped advances.
            note_missing_chrome(glyph.lead_ch);
            pen_x += advance;
            continue;
        };
        if info.missing {
            // When: info.missing marks a character no face resolved, an outline box one
            // advance wide and one ascent tall shows the gap, and the pen still advances.
            note_missing_chrome(glyph.lead_ch);
            let width = advance.max(1.0);
            let height = (run.font_size_px * MISSING_BOX_ASCENT_RATIO).max(1.0);
            let (left, top) = (pen_x, baseline_y - height);
            if push_missing_box(
                &mut out.missing_boxes,
                [left, top, width, height],
                with_premultiplied_alpha(rgba, MISSING_BOX_ALPHA * alpha),
                screen,
                clip,
            ) {
                max_y_extent = max_y_extent.max(height);
            }
            pen_x += advance;
            continue;
        }
        if info.oversize {
            // When: info.oversize marks a tile the atlas can never place, the glyph draws nothing
            // although it should, so it is noted as missing chrome; the pen still advances.
            note_missing_chrome(glyph.lead_ch);
            pen_x += advance;
            continue;
        }
        if info.px_size[0] == 0 || info.px_size[1] == 0 {
            // When: px_size is zero the tile covers no pixels, yet it still carries the
            // shaper's advance, so the pen must move or the rest of the run shifts left.
            pen_x += advance;
            continue;
        }

        // Project the atlas-native tile to the requested chrome size.
        let gw = info.px_size[0] as f32 * scale;
        let gh = info.px_size[1] as f32 * scale;
        let off_x = info.px_offset[0] as f32 * scale;
        let off_y = info.px_offset[1] as f32 * scale;
        // Snap the draw origin, not the pen, to whole device pixels: the atlas uses nearest
        // filtering, so a pixel-aligned origin samples 1:1 without accumulated drift.
        let (gx, gy) = positioned_glyph_origin(
            pen_x,
            off_x,
            glyph.x_offset_px,
            baseline_y,
            off_y,
            glyph.y_offset_px,
        );

        if let Some(c) = clip {
            // When: clip is set the run paints inside a modal, so each tile is tested
            // against c before it can paint across the palette or IME border.
            if gx + gw < c.x || gx > c.x + c.w || gy + gh < c.y || gy > c.y + c.h {
                // When: the tile falls wholly outside c it cannot paint, but the advance
                // still applies or every later glyph in the run shifts left.
                pen_x += advance;
                continue;
            }
        }

        let inst_color = if info.is_color {
            [1.0, 1.0, 1.0, 1.0]
        } else {
            // When: is_color is false the tile holds coverage rather than colour, so the
            // chrome rgba supplies the hue and the coverage only scales it.
            rgba
        };
        out.glyphs.push(GlyphInstance {
            rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
            uv: info.uv,
            color: [inst_color[0] * alpha, inst_color[1] * alpha, inst_color[2] * alpha, alpha],
            flags: crate::core::glyph_flags(info.is_color, info.is_subpixel),
        });
        max_y_extent = max_y_extent.max(gh);
        pen_x += advance;
    }

    out.width_px = (pen_x - origin.0).max(0.0);
    out.height_px = max_y_extent;
    out
}

/// Convert a `(x, y, w, h)` rect in raster pixels (origin top-left,
/// y-down) into the NDC quad `[x0, y0, w_ndc, h_ndc]` the
/// [`crate::wezterm_pipeline::WeztermPipeline`] WGSL expects. Must match
/// `quad::px_to_ndc` byte-for-byte — the text shader interprets
/// `rect.y` as the BOTTOM corner of the quad in NDC (smaller NDC y) and
/// `rect.w` as a positive upward extent, because its UV mix uses
/// `uv.w` (the texture's larger v, i.e. the BOTTOM of the bitmap) at
/// `c.y = 0` and `uv.y` (the texture's smaller v, i.e. the TOP of the
/// bitmap) at `c.y = 1` — so the corner with the smaller v sample MUST
/// be the smaller-NDC-y corner. Returning `y0 = top_NDC` with a negative
/// `h_ndc` instead would put `c.y = 0` at the visual TOP of the quad
/// while sampling the BOTTOM of the bitmap, mirroring every chrome
/// glyph vertically.
#[inline]
fn px_to_ndc(px_x: f32, px_y: f32, px_w: f32, px_h: f32, sw: f32, sh: f32) -> [f32; 4] {
    let nx = (px_x / sw) * 2.0 - 1.0;
    let ny = 1.0 - (px_y / sh) * 2.0 - (px_h / sh) * 2.0;
    let nw = (px_w / sw) * 2.0;
    let nh = (px_h / sh) * 2.0;
    [nx, ny, nw, nh]
}
