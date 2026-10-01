//! Exact geometry of the editable overlay query fields.
//!
//! The command-palette query and the search-bar query are painted from one
//! shaped run each. This module turns that run's shaped advances into cluster
//! boundaries, plans the caret, selection highlight, and horizontal scroll from
//! them, and maps a pointer x back to a query byte offset with the same
//! boundaries. The renderer keeps only the constant-size [`FieldGeometry`](crate::field_geometry::FieldGeometry) of
//! the last presented frame; per-cluster boundaries are rebuilt transiently for
//! each pointer query, so no per-character state outlives a call.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use sonicterm_engine::FontStack;
use sonicterm_render_model::boundary::ui::command_palette::{CommandPalette, CommandPaletteMode};
use sonicterm_render_model::boundary::ui::overlays::{
    command_palette_query_display, search_bar_label, search_query_display, SEARCH_BAR_PROMPT,
};
use sonicterm_render_model::boundary::ui::search::SearchState;
use sonicterm_text::GlyphInstance;

use crate::chrome_text::{ChromeAttrs, ChromeShapedRun};

#[cfg(test)]
#[path = "field_geometry_tests.rs"]
mod field_geometry_tests;

/// Which overlay query field a geometry record describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldKind {
    /// The command-palette query row (commands, rename tab, rename window).
    Palette,
    /// The search-bar query between the `/ ` prompt and the match counter.
    Search,
}

/// Axis-aligned rectangle in physical (raster) pixels, origin top-left.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FieldRect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl FieldRect {
    /// Right edge (`x + w`).
    #[must_use]
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// Bottom edge (`y + h`).
    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Whether the point lies inside the rectangle, left/top inclusive.
    #[must_use]
    pub fn contains(&self, point_x: f32, point_y: f32) -> bool {
        point_x >= self.x && point_x < self.right() && point_y >= self.y && point_y < self.bottom()
    }
}

/// The text one query field paints, and where its selectable query sits in it.
///
/// Byte offsets address `label`. For search, `label` is the whole bar label and
/// `content` excludes the `/ ` prompt and the match counter; for the palette,
/// `label` is the query alone. An empty palette query paints its placeholder,
/// which is not part of `label` and so is never selectable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldText {
    /// Field this text belongs to.
    pub kind: FieldKind,
    /// Painted label, preedit spliced in, with no caret marker.
    pub label: String,
    /// Byte range of the query inside `label`.
    pub content: Range<usize>,
    /// Caret byte offset in `label`.
    pub caret: usize,
    /// Highlighted byte range in `label`, if any.
    pub selection: Option<Range<usize>>,
    /// True while an IME preedit is spliced into `label`.
    pub composing: bool,
    /// Mode discriminant that changes the painted field without changing its text.
    pub variant: u8,
}

impl FieldText {
    /// Field text for an open, editable command palette; `None` when closed or
    /// in the colour picker, whose title row is not an editable field.
    #[must_use]
    pub fn palette(palette: &CommandPalette, preedit: &str) -> Option<Self> {
        if !palette.is_open() || palette.mode() == CommandPaletteMode::TabColor {
            // When: the palette is closed or shows the colour picker title, no query is editable.
            return None;
        }
        let display = command_palette_query_display(palette, preedit);
        let len = display.text.len();
        Some(Self {
            kind: FieldKind::Palette,
            content: 0..len,
            caret: display.caret.min(len),
            selection: display.selection,
            composing: !preedit.is_empty(),
            variant: palette_variant(palette),
            label: display.text,
        })
    }

    /// Field text for the search bar; offsets shift past the `/ ` prompt.
    #[must_use]
    pub fn search(search: &SearchState, preedit: &str) -> Self {
        let display = search_query_display(search, preedit);
        let prompt = SEARCH_BAR_PROMPT.len();
        Self {
            kind: FieldKind::Search,
            label: search_bar_label(search, preedit),
            content: prompt..prompt + display.text.len(),
            caret: prompt + display.caret,
            selection: display.selection.map(|range| range.start + prompt..range.end + prompt),
            composing: !preedit.is_empty(),
            variant: 0,
        }
    }

    /// Identity of the painted text and its selectable range under `environment`.
    ///
    /// Caret and selection are excluded, so a drag that only moves them keeps
    /// mapping against the frame the user is looking at.
    #[must_use]
    pub fn content_hash(&self, environment: u64) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.kind.hash(&mut hasher);
        self.label.hash(&mut hasher);
        self.content.hash(&mut hasher);
        self.composing.hash(&mut hasher);
        self.variant.hash(&mut hasher);
        environment.hash(&mut hasher);
        hasher.finish()
    }

    /// Identity of the whole presented field, including caret and selection.
    #[must_use]
    pub fn state_hash(&self, environment: u64) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.content_hash(environment).hash(&mut hasher);
        self.caret.hash(&mut hasher);
        self.selection.hash(&mut hasher);
        hasher.finish()
    }
}

/// Encode the palette mode and tabs-only filter, which change the placeholder.
fn palette_variant(palette: &CommandPalette) -> u8 {
    let mode = match palette.mode() {
        CommandPaletteMode::Commands => 0,
        CommandPaletteMode::RenameTab => 1,
        CommandPaletteMode::RenameWindow => 2,
        CommandPaletteMode::TabColor => 3,
    };
    mode * 2 + u8::from(palette.tabs_only())
}

/// Largest UTF-8 character boundary at or below `offset`, clamped to the text.
fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut boundary = offset.min(text.len());
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

/// Cluster start offsets of one shaped run and the pen x at each, left to right.
///
/// Built transiently from a [`ChromeShapedRun`]; never stored by the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldBoundaries {
    /// `(cluster start byte, pen x)`, strictly increasing by byte.
    stops: Vec<(usize, f32)>,
    /// Byte length of the shaped text; the run end is a boundary too.
    text_len: usize,
    /// Pen x at the end of the run.
    total_px: f32,
}

impl FieldBoundaries {
    /// Accumulate shaped `(cluster byte, advance)` pairs into cluster boundaries.
    ///
    /// Glyphs sharing a cluster merge into one stop; a cluster that does not
    /// advance past the previous one merges into it rather than reordering x.
    /// Every stop is a UTF-8 character boundary of `text`.
    #[must_use]
    pub fn from_advances(text: &str, advances: &[(usize, f32)]) -> Self {
        Self::from_advance_iter(text, advances.iter().copied())
    }

    /// Build boundaries from a prepared run's own advances.
    ///
    /// The painted glyphs of [`crate::chrome_text::layout_prepared`] move the pen
    /// by these same advances, so geometry and glyphs share one shaping result.
    #[must_use]
    pub fn from_run(run: &ChromeShapedRun<'_>) -> Self {
        Self::from_advance_iter(run.text(), run.advances())
    }

    /// Shared accumulator behind [`Self::from_advances`] and [`Self::from_run`].
    fn from_advance_iter(text: &str, advances: impl Iterator<Item = (usize, f32)>) -> Self {
        let mut stops: Vec<(usize, f32)> = Vec::with_capacity(advances.size_hint().0 + 1);
        let mut pen_px = 0.0_f32;
        for (cluster, advance_px) in advances {
            let start = floor_char_boundary(text, cluster);
            let merges = stops.last().is_some_and(|&(last, _)| start <= last);
            if !merges {
                // A glyph that opens a later cluster makes its pen x a boundary.
                stops.push((start, pen_px));
            }
            pen_px += advance_px;
        }
        if stops.first().is_none_or(|&(start, _)| start != 0) {
            // Without a glyph at byte zero, the field start is still a boundary.
            stops.insert(0, (0, 0.0));
        }
        Self { stops, text_len: text.len(), total_px: pen_px }
    }

    /// Shape `text` with the chrome shaper and build its boundaries.
    #[must_use]
    pub fn shape(
        font_stack: &FontStack,
        text: &str,
        font_size_px: f32,
        native_em_px: f32,
    ) -> Option<Self> {
        ChromeShapedRun::shape(font_stack, text, ChromeAttrs::default(), font_size_px, native_em_px)
            .map(|run| Self::from_run(&run))
    }

    /// Pen x at the end of the run.
    #[must_use]
    pub fn total_width(&self) -> f32 {
        self.total_px
    }

    /// Index of the cluster that contains `offset`.
    fn cluster_index(&self, offset: usize) -> usize {
        self.stops.partition_point(|&(start, _)| start <= offset).saturating_sub(1)
    }

    /// Byte and pen x where cluster `index` ends.
    fn cluster_end(&self, index: usize) -> (usize, f32) {
        self.stops.get(index + 1).copied().unwrap_or((self.text_len, self.total_px))
    }

    /// Caret x for a byte offset: the start of its cluster, or the run end.
    ///
    /// A scalar offset inside a multi-scalar cluster draws at that cluster's
    /// leading edge, so the caret never splits a rendered cluster.
    #[must_use]
    pub fn caret_x(&self, offset: usize) -> f32 {
        if offset >= self.text_len {
            // When: `offset` reaches `text_len`, the caret sits after the last byte and draws at the run end.
            return self.total_px;
        }
        self.stops[self.cluster_index(offset)].1
    }

    /// Width of the cluster under the caret, or `None` at the run end.
    #[must_use]
    pub fn caret_width(&self, offset: usize) -> Option<f32> {
        if offset >= self.text_len {
            // When: `offset` reaches `text_len`, the caret is past the last cluster with no glyph to cover.
            return None;
        }
        let index = self.cluster_index(offset);
        Some(self.cluster_end(index).1 - self.stops[index].1)
    }

    /// Horizontal extent of a byte range, widened to whole clusters.
    #[must_use]
    pub fn span_x(&self, range: Range<usize>) -> Option<(f32, f32)> {
        let start = range.start.min(range.end).min(self.text_len);
        let end = range.start.max(range.end).min(self.text_len);
        if start == end {
            // When: `start` equals `end`, the range is collapsed and there is nothing to highlight.
            return None;
        }
        let left_px = self.caret_x(start);
        let right_px = if end >= self.text_len {
            self.total_px
        } else {
            // When: the range ends inside the run, a partly covered cluster widens to its end.
            let index = self.cluster_index(end);
            let (cluster_start, cluster_x) = self.stops[index];
            if cluster_start == end {
                cluster_x
            } else {
                // When: `end` falls inside a cluster, the whole cluster is selected visually.
                self.cluster_end(index).1
            }
        };
        (right_px > left_px).then_some((left_px, right_px))
    }

    /// Boundary byte offset nearest to `local_x`, earlier boundary on a tie.
    #[must_use]
    pub fn nearest_boundary(&self, local_x: f32) -> usize {
        let mut best = (0, f32::INFINITY);
        let end = std::iter::once((self.text_len, self.total_px));
        for (offset, stop_x) in self.stops.iter().copied().chain(end) {
            let distance = (stop_x - local_x).abs();
            if distance < best.1 {
                best = (offset, distance);
            }
        }
        best.0
    }
}

/// Frame-local placement of one query field, supplied by the renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldPlacement {
    /// Field the placement belongs to.
    pub kind: FieldKind,
    /// Visible text area; text, caret, and highlight are clipped to it.
    pub clip: FieldRect,
    /// Area whose press starts a field gesture.
    pub hit_area: FieldRect,
    /// Caret top edge.
    pub caret_y: f32,
    /// Caret height.
    pub caret_h: f32,
    /// Caret width when no cluster lies under it.
    pub caret_fallback_w: f32,
    /// Shaping size the run was painted with.
    pub font_size_px: f32,
    /// Native em of the shaping stack.
    pub native_em_px: f32,
}

/// Constant-size record of one presented query field.
///
/// It carries identities and rectangles only; the per-cluster boundary map is
/// rebuilt from the same shaping inputs when a pointer asks for an offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldGeometry {
    /// Frame-local placement the field was planned with.
    pub placement: FieldPlacement,
    /// [`FieldText::content_hash`] of the painted text.
    pub content_hash: u64,
    /// [`FieldText::state_hash`] of the painted text.
    pub state_hash: u64,
    /// Pen origin x of the run after scrolling.
    pub text_x: f32,
    /// Caret rectangle, clipped into the field.
    pub caret: FieldRect,
    /// Selection highlight rectangle, clipped into the field.
    pub selection: Option<FieldRect>,
    /// Query start byte in the label.
    pub content_start: usize,
    /// Query end byte in the label.
    pub content_end: usize,
}

impl FieldGeometry {
    /// Map a pointer x to a query-relative byte offset.
    ///
    /// Clamp the pointer to the clip, then choose the nearest query boundary
    /// whose entire effective caret fits (earlier on a tie). Partial edge
    /// clusters snap inward; an empty clip or no fitting boundary returns `None`.
    #[must_use]
    pub fn hit_offset(&self, boundaries: &FieldBoundaries, point_x: f32) -> Option<usize> {
        let clip = self.placement.clip;
        if !point_x.is_finite() || !clip.x.is_finite() || !clip.right().is_finite() {
            // When: the pointer or clip is non-finite, no measured distance can authorize a hit.
            return None;
        }
        let clamped_x = point_x.max(clip.x).min(clip.right());
        let mut best = None;
        let mut best_distance = f32::INFINITY;
        let end = std::iter::once((self.content_end, boundaries.caret_x(self.content_end)));
        for (offset, prefix_px) in boundaries.stops.iter().copied().chain(end) {
            if offset < self.content_start || offset > self.content_end {
                // When: offset lies outside content_start..content_end, prompt and counter cannot be selected.
                continue;
            }
            let width = effective_caret_width(self.placement, boundaries, offset, self.content_end);
            let Some((left, _)) = normalized_caret_edges(self.text_x, prefix_px, width, clip)
            else {
                // When: normalized_caret_edges cannot fit a complete caret, this boundary cannot move the viewport.
                continue;
            };
            let distance = (left - clamped_x).abs();
            if distance < best_distance {
                best = Some(offset - self.content_start);
                best_distance = distance;
            }
        }
        best
    }
}

/// Intersect `rect` with `clip`, collapsing to zero area at the clip when disjoint.
///
/// Non-finite edges are treated as empty, so a degenerate placement never yields
/// a rectangle (or native IME caret) outside the field. `max`/`min` are used
/// instead of `clamp`, which panics on inverted or NaN bounds.
fn intersect_rect(rect: FieldRect, clip: FieldRect) -> FieldRect {
    let finite = |value: f32| if value.is_finite() { value } else { 0.0 };
    let clip_left = finite(clip.x);
    let clip_top = finite(clip.y);
    let clip_right = finite(clip.right()).max(clip_left);
    let clip_bottom = finite(clip.bottom()).max(clip_top);
    let left = finite(rect.x).max(clip_left).min(clip_right);
    let top = finite(rect.y).max(clip_top).min(clip_bottom);
    let right = finite(rect.right()).min(clip_right).max(left);
    let bottom = finite(rect.bottom()).min(clip_bottom).max(top);
    FieldRect { x: left, y: top, w: right - left, h: bottom - top }
}

/// Caret width shared by layout and hits; the query endpoint never covers the counter.
fn effective_caret_width(
    placement: FieldPlacement,
    boundaries: &FieldBoundaries,
    offset: usize,
    content_end: usize,
) -> f32 {
    let width = if offset >= content_end {
        placement.caret_fallback_w
    } else {
        // When: offset precedes content_end, the caret covers its query cluster rather than the counter.
        boundaries.caret_width(offset).map_or(placement.caret_fallback_w, |width| width.max(4.0))
    };
    width.max(0.0).min(placement.clip.w.max(0.0))
}

/// Normalize only exact planner edge origins, never a nearby clipped boundary.
/// Adding prefix_px back can lose an edge to cancellation; recognizing its exact
/// inverse avoids an epsilon that would admit neighboring, genuinely clipped stops.
fn normalized_caret_edges(
    text_x: f32,
    prefix_px: f32,
    width: f32,
    clip: FieldRect,
) -> Option<(f32, f32)> {
    if ![text_x, prefix_px, width, clip.x, clip.y, clip.w, clip.h, clip.right(), clip.bottom()]
        .iter()
        .all(|value| value.is_finite())
        || width <= 0.0
        || clip.w <= 0.0
        || clip.h <= 0.0
    {
        // When: any edge operand is non-finite or width/clip has no area, no visible boundary exists.
        return None;
    }
    let (left, right) = if text_x == clip.x - prefix_px {
        (clip.x, clip.x + width)
    } else if text_x == (clip.right() - width) - prefix_px {
        // When: text_x exactly matches the right-edge inverse, undo its cancellation without an epsilon.
        ((clip.right() - width).max(clip.x), clip.right())
    } else {
        // When: text_x matches neither exact edge origin, raw roundoff fails closed rather than inventing a snap.
        let left = text_x + prefix_px;
        (left, left + width)
    };
    (left >= clip.x && right <= clip.right() && right > left).then_some((left, right))
}

/// Keep a visible caret's origin exact; otherwise reveal only its crossed edge.
fn fitted_origin(text_x: f32, prefix_px: f32, width: f32, clip: FieldRect) -> f32 {
    if normalized_caret_edges(text_x, prefix_px, width, clip).is_some() {
        // When: normalized_caret_edges fits, preserve text_x bits instead of reconstructing its scroll.
        return text_x;
    }
    let left = text_x + prefix_px;
    if left < clip.x {
        clip.x - prefix_px
    } else if left + width > clip.right() {
        // When: left + width crosses clip.right(), reveal only the hidden part of the caret block.
        (clip.right() - width) - prefix_px
    } else {
        // When: left and width cross neither edge, a degenerate clip needs no additional scroll.
        text_x
    }
}

/// Plan scroll, caret, and selection for one field from its painted boundaries.
///
/// The caret and highlight are intersected with the clip, so a field narrower
/// or shorter than its caret reports a trimmed (possibly zero-area) caret
/// rather than one extending outside the field; callers skip zero-area quads.
///
/// `caret` and `selection` address the painted text; the placeholder case
/// passes caret zero and no selection with the placeholder's boundaries.
/// A compatible `presented` frame retains its exact text origin while the caret
/// fits. Text, environment, placement changes and empty queries reset the origin.
#[must_use]
pub fn plan_field(
    placement: FieldPlacement,
    boundaries: &FieldBoundaries,
    text: &FieldText,
    caret: usize,
    selection: Option<Range<usize>>,
    environment: u64,
    presented: Option<&FieldGeometry>,
) -> FieldGeometry {
    let clip = placement.clip;
    let caret_w = effective_caret_width(placement, boundaries, caret, text.content.end);
    let prefix_px = boundaries.caret_x(caret);
    let initial_x = fitted_origin(clip.x, prefix_px, caret_w, clip);
    let end_width =
        effective_caret_width(placement, boundaries, text.content.end, text.content.end);
    let end_prefix = boundaries.caret_x(text.content.end);
    // Recover the bound from the same representable origin as layout, not an algebraic rearrangement.
    let end_origin = fitted_origin(clip.x, end_prefix, end_width, clip);
    // An exact-fill caret has two legitimate inverse-edge representations.
    let left_end_origin = clip.x - end_prefix;
    let min_origin = if end_origin < clip.x
        && end_width == clip.w
        && normalized_caret_edges(left_end_origin, end_prefix, end_width, clip).is_some()
    {
        end_origin.min(left_end_origin)
    } else {
        // When: end_origin has no scrolling or left_end_origin does not fit, keep the ordinary tail bound.
        end_origin
    };
    let prior = presented.filter(|prior| {
        let scroll = clip.x - prior.text_x;
        !text.content.is_empty()
            && prior.placement == placement
            && prior.content_hash == text.content_hash(environment)
            && prior.text_x.is_finite()
            && scroll.is_finite()
            && scroll >= 0.0
            && prior.text_x >= min_origin
    });
    let text_x =
        prior.map_or(initial_x, |prior| fitted_origin(prior.text_x, prefix_px, caret_w, clip));
    let (caret_left, caret_right) =
        normalized_caret_edges(text_x, prefix_px, caret_w, clip).unwrap_or((clip.x, clip.x));
    // The same normalized horizontal edges serve hit eligibility and painting; only height still clips.
    let caret_rect = intersect_rect(
        FieldRect {
            x: caret_left,
            y: placement.caret_y,
            w: caret_right - caret_left,
            h: placement.caret_h,
        },
        clip,
    );
    let selection_rect =
        selection.and_then(|range| boundaries.span_x(range)).and_then(|(left_px, right_px)| {
            let highlight = intersect_rect(
                FieldRect {
                    x: text_x + left_px,
                    y: placement.caret_y,
                    w: right_px - left_px,
                    h: placement.caret_h,
                },
                clip,
            );
            // A highlight scrolled away or inside an empty clip is not painted.
            (highlight.w > 0.0 && highlight.h > 0.0).then_some(highlight)
        });
    FieldGeometry {
        placement,
        content_hash: text.content_hash(environment),
        state_hash: text.state_hash(environment),
        text_x,
        caret: caret_rect,
        selection: selection_rect,
        content_start: text.content.start,
        content_end: text.content.end,
    }
}

/// Whether a field press is a new gesture or a drag continuing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldHitMode {
    /// A press: points outside the hit area are [`FieldHit::Outside`].
    Press,
    /// A drag: any point maps, clamped to the query.
    Drag,
}

/// Result of mapping a pointer onto a presented query field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldHit {
    /// Query-relative UTF-8 byte offset on a cluster boundary.
    Offset(usize),
    /// A press outside the field's hit area.
    Outside,
    /// The presented frame shows different text or environment; redraw first.
    Stale,
    /// No exact mapping: no fitting visible boundary, no shaping, or a live preedit.
    Unavailable,
}

/// Map a pointer onto the presented field, refusing stale or estimated geometry.
#[must_use]
pub fn field_hit(
    presented: Option<&FieldGeometry>,
    text: &FieldText,
    environment: u64,
    font_stack: Option<&FontStack>,
    point: (f32, f32),
    mode: FieldHitMode,
) -> FieldHit {
    let Some(geometry) = presented.filter(|geometry| geometry.placement.kind == text.kind) else {
        // When: this field was never presented, a press cannot have targeted it; a drag redraws.
        return if mode == FieldHitMode::Press { FieldHit::Outside } else { FieldHit::Stale };
    };
    if mode == FieldHitMode::Press && !geometry.placement.hit_area.contains(point.0, point.1) {
        // When: a `Press` lands outside the presented `hit_area`, it belongs to whatever lies beneath.
        return FieldHit::Outside;
    }
    if text.composing {
        // When: `text` is `composing` an IME preedit, display offsets do not address the committed query.
        return FieldHit::Unavailable;
    }
    let Some(font_stack) = font_stack else {
        // When: no `font_stack` exists, nothing was measured, so no hit may be fabricated.
        return FieldHit::Unavailable;
    };
    if geometry.content_hash != text.content_hash(environment) {
        // When: the presented `content_hash` differs (text, mode, or environment), the old map is not trusted.
        return FieldHit::Stale;
    }
    let placement = geometry.placement;
    let Some(boundaries) = FieldBoundaries::shape(
        font_stack,
        &text.label,
        placement.font_size_px,
        placement.native_em_px,
    ) else {
        // When: the label no longer shapes, there is no exact boundary map.
        return FieldHit::Unavailable;
    };
    geometry.hit_offset(&boundaries, point.0).map_or(FieldHit::Unavailable, FieldHit::Offset)
}

/// Caret rectangle of the presented field when it shows exactly `text`.
#[must_use]
pub fn field_caret_rect(
    presented: Option<&FieldGeometry>,
    text: &FieldText,
    environment: u64,
) -> Option<FieldRect> {
    presented
        .filter(|geometry| geometry.state_hash == text.state_hash(environment))
        .map(|geometry| geometry.caret)
}

/// Last presented geometry of both query fields.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PresentedFields {
    /// Command-palette query field.
    pub palette: Option<FieldGeometry>,
    /// Search-bar query field.
    pub search: Option<FieldGeometry>,
}

impl PresentedFields {
    /// Replace the presented record with a frame's candidates only if that
    /// frame was presented; an unpresented frame never blesses its geometry.
    pub fn settle(&mut self, candidates: PresentedFields, presented: bool) {
        if presented {
            // A frame that reached the screen holds the fields the user sees.
            *self = candidates;
        }
    }

    /// Forget both fields after a font, scale, surface, or device change.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Clip one glyph to `clip`, trimming its rectangle and atlas UVs together.
///
/// `glyph.rect` is the NDC encoding of [`crate::chrome_text::layout`]; the
/// visible part keeps the same texel-to-pixel mapping. Returns `None` when no
/// part of the glyph is inside `clip`.
#[must_use]
pub fn clip_glyph(
    glyph: GlyphInstance,
    clip: FieldRect,
    sw: f32,
    sh: f32,
) -> Option<GlyphInstance> {
    if sw <= 0.0 || sh <= 0.0 {
        // When: `sw` or `sh` is not positive, the surface has no extent to map NDC back to pixels.
        return None;
    }
    // Invert the chrome NDC encoding: rect.y is the bottom edge, rect.w/h positive extents.
    let [ndc_x, ndc_y, ndc_w, ndc_h] = glyph.rect;
    let left = (ndc_x + 1.0) * 0.5 * sw;
    let width = ndc_w * 0.5 * sw;
    let height = ndc_h * 0.5 * sh;
    let top = (1.0 - ndc_y - ndc_h) * 0.5 * sh;
    if width <= 0.0 || height <= 0.0 {
        // When: the glyph `width` or `height` is degenerate, it paints nothing and is dropped.
        return None;
    }
    let right = left + width;
    let bottom = top + height;
    let visible_left = left.max(clip.x);
    let visible_right = right.min(clip.right());
    let visible_top = top.max(clip.y);
    let visible_bottom = bottom.min(clip.bottom());
    if visible_right <= visible_left || visible_bottom <= visible_top {
        // When: the `visible_right`/`visible_bottom` span is empty, no glyph pixel is in the clip; drop it.
        return None;
    }
    if visible_left == left
        && visible_right == right
        && visible_top == top
        && visible_bottom == bottom
    {
        // When: every `visible_left` edge matches the glyph, it is wholly inside; keep its encoding bit for bit.
        return Some(glyph);
    }
    // uv = [u0, v0, u1, v1]; the pipeline samples (u0, v0) at the top-left pixel corner.
    let [u_left, v_top, u_right, v_bottom] = glyph.uv;
    let u_per_px = (u_right - u_left) / width;
    let v_per_px = (v_bottom - v_top) / height;
    let visible_w = visible_right - visible_left;
    let visible_h = visible_bottom - visible_top;
    let mut clipped = glyph;
    clipped.rect = [
        visible_left / sw * 2.0 - 1.0,
        1.0 - visible_top / sh * 2.0 - visible_h / sh * 2.0,
        visible_w / sw * 2.0,
        visible_h / sh * 2.0,
    ];
    clipped.uv = [
        u_left + (visible_left - left) * u_per_px,
        v_top + (visible_top - top) * v_per_px,
        u_left + (visible_right - left) * u_per_px,
        v_top + (visible_bottom - top) * v_per_px,
    ];
    Some(clipped)
}

/// Clip every glyph from `first` onward to `clip`, dropping glyphs wholly outside.
///
/// Clip edges are snapped to whole pixels so a 1:1 glyph stays texel aligned.
pub fn clip_glyphs_to_rect(
    glyphs: &mut Vec<GlyphInstance>,
    first: usize,
    clip: FieldRect,
    sw: f32,
    sh: f32,
) {
    let left = clip.x.round();
    let top = clip.y.round();
    let snapped = FieldRect {
        x: left,
        y: top,
        w: clip.right().round() - left,
        h: clip.bottom().round() - top,
    };
    let start = first.min(glyphs.len());
    let mut write = start;
    for read in start..glyphs.len() {
        if let Some(clipped) = clip_glyph(glyphs[read], snapped, sw, sh) {
            // A partly visible glyph is kept, compacted in draw order.
            glyphs[write] = clipped;
            write += 1;
        }
    }
    glyphs.truncate(write);
}
