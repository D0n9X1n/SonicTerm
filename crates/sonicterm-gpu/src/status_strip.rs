//! Status line strip: its height, colors and drawing.
//!
//! The strip sits at the top content edge, above the panes and below any
//! titlebar band, or with `position = "bottom"` between the panes and the tab
//! bar (at the bottom edge when the tab bar is hidden). It shares the tab bar's
//! `bg_base` background and has a hairline only on its terminal side. Segments come from
//! `sonicterm-ui::status_line` as a [`StatusLineView`]; this module measures
//! and fits them with the chrome font, then emits ordinary quads and glyphs,
//! so the GDI software path composites the strip unchanged.

use sonicterm_render_model::boundary::cfg::status_line::StatusPosition;
use sonicterm_render_model::boundary::cfg::theme::Theme;
use sonicterm_render_model::boundary::ui::status_line::{
    fit, FitMetrics, PlacedSegment, SegmentClick, StatusLineView, StatusTone,
};
use sonicterm_render_model::boundary::ui::tabbar_view::tab_bar_height;
use sonicterm_text::glyph_atlas::{GlyphAtlas, Rasterizer};
use sonicterm_text::GlyphInstance;
use sonicterm_types::GlyphRasterVariant;

use crate::chrome_text::{self, ChromeAttrs};
use crate::color::{hex_to_chrome_color, ChromeColor};
use crate::quad::{px_to_ndc, QuadInstance};

/// Strip height as a share of the tab bar height.
const HEIGHT_SHARE: f32 = 0.6;
/// Smallest strip height, in logical px.
const MIN_HEIGHT_LOGICAL: f32 = 20.0;
/// Strip type size as a share of the live terminal font size.
const TEXT_SHARE: f32 = 0.6;
/// Smallest strip type size, in logical pt: FreeType raises any character size under 1 pt to
/// 1 pt, so the stack and the geometry both use the size it really draws.
const MIN_TEXT_PT: f32 = 1.0;
/// Contrast a caution or failure color needs against the strip background.
const MIN_CONTRAST: f32 = 3.0;

/// Strip height in raster px for `font_size` (logical pt) at `scale_factor`:
/// 0.6 × the tab bar height with a 20 logical px floor, rounded to whole px.
pub fn status_line_height(font_size: f32, scale_factor: f32) -> f32 {
    let logical = (tab_bar_height(font_size) * HEIGHT_SHARE).max(MIN_HEIGHT_LOGICAL);
    (logical * scale_factor).round()
}

/// Where the strip sits and what it reserves, in raster px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripPlacement {
    /// Top edge of the band.
    pub top: f32,
    /// Height reserved above the panes, below the titlebar band.
    pub reserved_top: f32,
    /// Height reserved below the panes, above the tab bar.
    pub reserved_bottom: f32,
}

/// Place a strip of `strip_h` raster px at `position` on a `surface_h` surface whose titlebar
/// band is `titlebar_px` and whose tab bar is `tab_bar_px` (0 while hidden). A hidden strip has
/// `strip_h` 0 and reserves nothing.
pub fn strip_placement(
    position: StatusPosition,
    surface_h: f32,
    titlebar_px: f32,
    tab_bar_px: f32,
    strip_h: f32,
) -> StripPlacement {
    match position {
        StatusPosition::Top => {
            StripPlacement { top: titlebar_px, reserved_top: strip_h, reserved_bottom: 0.0 }
        }
        StatusPosition::Bottom => StripPlacement {
            top: (surface_h - tab_bar_px - strip_h).max(0.0),
            reserved_top: 0.0,
            reserved_bottom: strip_h,
        },
    }
}

/// Top edge of the hairline `thickness` px thick on the band's terminal side: the band's bottom
/// edge at the top of the window, its top edge at the bottom.
pub fn hairline_top(position: StatusPosition, top: f32, height: f32, thickness: f32) -> f32 {
    match position {
        StatusPosition::Top => top + height - thickness,
        StatusPosition::Bottom => top,
    }
}

/// WCAG relative luminance of a linear-sRGB color.
fn luminance_linear(rgb: [f32; 3]) -> f32 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// One sRGB-encoded channel to linear light.
fn srgb_channel_to_linear(channel: u8) -> f32 {
    let value = f32::from(channel) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        // When: `value` is above the sRGB linear segment, the 2.4 power curve applies.
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG contrast ratio of `color` against a linear-sRGB opaque `background`.
pub fn contrast_ratio(color: ChromeColor, background: [f32; 4]) -> f32 {
    let text = luminance_linear([color.r, color.g, color.b].map(srgb_channel_to_linear));
    let back = luminance_linear([background[0], background[1], background[2]]);
    let (light, dark) = (text.max(back), text.min(back));
    (light + 0.05) / (dark + 0.05)
}

/// `preferred` when it reaches 3:1 against `background`, else `fallback`;
/// the symbol beside the value still carries the meaning.
pub fn readable_or(
    preferred: ChromeColor,
    fallback: ChromeColor,
    background: [f32; 4],
) -> ChromeColor {
    if contrast_ratio(preferred, background) >= MIN_CONTRAST {
        preferred
    } else {
        // When: `preferred` is under 3:1 on the strip, use the theme foreground instead.
        fallback
    }
}

/// Colors of the strip's tones, resolved once per frame from the theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatusColors {
    /// Values: the active tab title color.
    pub value: ChromeColor,
    /// Names and quiet words: the inactive tab title color.
    pub name: ChromeColor,
    /// Caution: `ansi.yellow`, or `foreground` under 3:1.
    pub warn: ChromeColor,
    /// Failure: `ansi.red`, or `foreground` under 3:1.
    pub error: ChromeColor,
    /// Mode chip text: the active tab title color, or `foreground` under 3:1. A chip has no
    /// background of its own, so its text is checked against the strip like the cautions.
    pub chip: ChromeColor,
    /// Strip background (linear premultiplied).
    pub background: [f32; 4],
    /// Hairline between the terminal and the strip.
    pub hairline: [f32; 4],
}

impl StatusColors {
    /// Resolve the strip colors from `theme` and the chrome palette entries.
    pub fn resolve(
        theme: &Theme,
        tab_active_fg: ChromeColor,
        tab_inactive_fg: ChromeColor,
        palette: &sonicterm_render_model::boundary::ui::ui_tokens::UiPalette,
    ) -> Self {
        let foreground = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        let background = palette.bg_base;
        let warn = readable_or(
            hex_to_chrome_color(theme.colors.ansi.yellow.0.as_str()),
            foreground,
            background,
        );
        Self {
            chip: readable_or(tab_active_fg, foreground, background),
            value: tab_active_fg,
            name: tab_inactive_fg,
            warn,
            error: readable_or(
                hex_to_chrome_color(theme.colors.ansi.red.0.as_str()),
                foreground,
                background,
            ),
            background,
            hairline: palette.border_subtle,
        }
    }

    /// Text color of `tone`.
    fn text(&self, tone: StatusTone) -> ChromeColor {
        match tone {
            StatusTone::Name => self.name,
            StatusTone::Value => self.value,
            StatusTone::Chip => self.chip,
            StatusTone::Warn | StatusTone::ChipWarn => self.warn,
            StatusTone::Error => self.error,
        }
    }
}

/// One presented segment's hit box, in raster px along the strip.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentedStatusSegment {
    /// Which segment.
    pub id: sonicterm_render_model::boundary::cfg::status_line::StatusSegmentId,
    /// Left edge.
    pub x: f32,
    /// Width.
    pub width: f32,
    /// What a click does; `None` is inert.
    pub click: Option<SegmentClick>,
}

/// The strip a frame placed and the one the last presented frame showed, each whole: band top,
/// height and segment rects. Assembly stages; only a presented frame commits, so routing and
/// accessibility never see an undrawn band or layout.
#[derive(Debug, Clone, Default)]
pub struct StripLayouts {
    /// Placed by the frame being assembled; `None` once settled or when no strip was placed.
    staged: Option<StripHit>,
    /// What the last presented frame drew; `None` before one did or while the strip is hidden.
    presented: Option<StripHit>,
}

impl StripLayouts {
    /// Record the strip the frame being assembled placed.
    pub fn stage(&mut self, strip: StripHit) {
        self.staged = Some(strip);
    }

    /// Settle the staged strip at the presenter's outcome: commit it when the frame presented,
    /// discard it otherwise.
    pub fn settle(&mut self, presented: bool) {
        if let Some(staged) = self.staged.take().filter(|_| presented) {
            self.presented = Some(staged);
        }
    }

    /// Drop both strips, as hiding the strip does.
    pub fn clear(&mut self) {
        self.staged = None;
        self.presented = None;
    }

    /// The strip the last presented frame drew.
    pub fn presented(&self) -> Option<&StripHit> {
        self.presented.as_ref()
    }
}

/// Where the strip is and how it is drawn this frame.
#[derive(Debug, Clone, Copy)]
pub struct StripGeometry {
    /// Top edge, raster px.
    pub top: f32,
    /// Height, raster px.
    pub height: f32,
    /// The window edge the band sits on, which puts its hairline on the terminal side.
    pub position: StatusPosition,
    /// Surface size, raster px.
    pub screen: (f32, f32),
    /// Display scale factor.
    pub scale_factor: f32,
    /// The terminal font's live raster size, which the strip text is 60% of.
    pub font_raster_px: f32,
    /// Cap height of the strip font at [`Self::text_px`], raster px; see [`strip_cap_height`].
    pub cap_height: f32,
}

/// The cap height the strip centers by, from the drawn capital's ink box when there is one:
/// `drawn` is its `(top, height)` relative to the baseline, so the result is twice its center's
/// height above the baseline. Ink that reaches below the baseline therefore lowers it instead of
/// shifting the capitals a whole pixel. Without a drawn capital it is the font's own measurement
/// `reported`, and without either, 0.7 × `text_px`.
pub fn resolve_cap_height(drawn: Option<(f32, f32)>, reported: Option<f32>, text_px: f32) -> f32 {
    match (drawn, reported) {
        (Some((top, height)), _) => -(2.0 * top + height),
        (None, Some(reported)) => reported,
        (None, None) => text_px * 0.7,
    }
}

/// The cap height for `text_px`: `kept` when it was measured at that size, otherwise `measure`'s
/// answer. Returns the `(text size bits, cap height)` the renderer keeps, so the same size is
/// measured once and a new size never reuses another size's value.
pub fn kept_cap_height(
    kept: Option<(u32, f32)>,
    text_px: f32,
    measure: impl FnOnce() -> f32,
) -> (u32, f32) {
    let bits = text_px.to_bits();
    match kept {
        Some((kept_bits, cap_height)) if kept_bits == bits => (bits, cap_height),
        _ => (bits, measure()),
    }
}

/// The strip font's cap height at `text_px`, in raster px: a capital `H` rasterized through the
/// strip on baseline 0 and measured by its ink box, so it describes exactly what the strip draws
/// on every rasterizer. The font's own measurement is used when no `H` is drawn.
pub fn strip_cap_height(
    font_stack: &sonicterm_engine::FontStack,
    rasterizer: &mut impl Rasterizer,
    atlas: &mut GlyphAtlas,
    text_px: f32,
) -> f32 {
    let screen = (text_px * 4.0, text_px * 4.0);
    let reference = chrome_text::layout_with_raster_variant(
        font_stack,
        rasterizer,
        atlas,
        "H",
        ChromeColor::WHITE,
        ChromeAttrs { bold: false, italic: false },
        text_px,
        text_px,
        (0.0, 0.0),
        screen,
        None,
        GlyphRasterVariant::StatusLine,
    );
    let drawn = reference.glyphs.first().map(|glyph| {
        let (_, ink_top, _, ink_height) =
            crate::cursor::ndc_rect_px(glyph.rect, screen.0, screen.1);
        (ink_top, ink_height)
    });
    let reported = font_stack.cap_height_raster_px().map(|cap_height| cap_height as f32);
    resolve_cap_height(drawn, reported, text_px)
}

/// The strip's text baseline: capitals centered in the band area outside the hairline, so the
/// baseline is that area's top plus half of its height and the cap height together.
pub fn strip_baseline(geometry: &StripGeometry) -> f32 {
    let thickness = geometry.scale_factor.max(0.5).round().max(1.0);
    let area_top = match geometry.position {
        StatusPosition::Top => geometry.top,
        StatusPosition::Bottom => geometry.top + thickness,
    };
    let area_h = (geometry.height - thickness).max(0.0);
    area_top + (area_h + geometry.cap_height) / 2.0
}

/// The strip's native font size in logical points for a `body_font_size` live terminal font:
/// exactly 60% of it. Times the display scale it is [`StripGeometry::text_px`], so the strip's
/// stack rasterizes at exactly the size it draws.
pub fn status_line_font_size(body_font_size: f32) -> f32 {
    (body_font_size * TEXT_SHARE).max(MIN_TEXT_PT)
}

impl StripGeometry {
    /// The strip's text raster size: 60% of the live terminal font size, at least 1 pt at the
    /// display scale, which is what [`status_line_font_size`] asks the stack for.
    pub fn text_px(&self) -> f32 {
        (self.font_raster_px * TEXT_SHARE).max(MIN_TEXT_PT * self.scale_factor.max(0.5)).max(1.0)
    }

    /// Fit metrics for this geometry.
    pub fn fit_metrics(&self) -> FitMetrics {
        let scale = self.scale_factor.max(0.5);
        FitMetrics { width: self.screen.0, edge_pad: 12.0 * scale, segment_gap: 20.0 * scale }
    }
}

/// Fit `view` into the strip, measuring each run with the chrome font.
pub fn fit_strip(
    view: &StatusLineView,
    geometry: &StripGeometry,
    font_stack: &sonicterm_engine::FontStack,
    rasterizer: &mut impl Rasterizer,
    atlas: &mut GlyphAtlas,
) -> Vec<PlacedSegment> {
    let text_px = geometry.text_px();
    fit(view, &geometry.fit_metrics(), |text| {
        chrome_text::layout_with_raster_variant(
            font_stack,
            rasterizer,
            atlas,
            text,
            ChromeColor::WHITE,
            ChromeAttrs { bold: false, italic: false },
            text_px,
            text_px,
            (0.0, 0.0),
            geometry.screen,
            None,
            GlyphRasterVariant::StatusLine,
        )
        .width_px
    })
}

/// Emit the strip's band, hairline and glyphs; chips are colored text with no background. Glyphs are
/// clipped to the band so no ink lands outside it.
#[allow(clippy::too_many_arguments)]
pub fn emit_strip(
    placed: &[PlacedSegment],
    geometry: &StripGeometry,
    colors: &StatusColors,
    font_stack: Option<&sonicterm_engine::FontStack>,
    rasterizer: Option<&mut impl Rasterizer>,
    atlas: &mut GlyphAtlas,
    quads: &mut Vec<QuadInstance>,
    glyphs: &mut Vec<GlyphInstance>,
) {
    let (screen_w, screen_h) = geometry.screen;
    let scale = geometry.scale_factor.max(0.5);
    quads.push(QuadInstance {
        rect: px_to_ndc(0.0, geometry.top, screen_w, geometry.height, screen_w, screen_h),
        color: colors.background,
        ..Default::default()
    });
    let thickness = scale.round().max(1.0);
    let hairline = hairline_top(geometry.position, geometry.top, geometry.height, thickness);
    quads.push(QuadInstance {
        rect: px_to_ndc(0.0, hairline, screen_w, thickness, screen_w, screen_h),
        color: colors.hairline,
        ..Default::default()
    });
    let (Some(font_stack), Some(rasterizer)) = (font_stack, rasterizer) else {
        // When: `font_stack` or `rasterizer` is missing, the chrome font is not loaded and the strip is a bare band.
        return;
    };
    let text_px = geometry.text_px();
    let baseline = strip_baseline(geometry);
    let first_glyph = glyphs.len();
    for part in placed.iter().flat_map(|segment| &segment.parts) {
        // A chip has no background: its tone colors its text and `is_chip` makes it bold.
        let run = chrome_text::layout_with_raster_variant(
            font_stack,
            rasterizer,
            atlas,
            &part.text,
            colors.text(part.tone),
            ChromeAttrs { bold: part.tone.is_chip(), italic: false },
            text_px,
            text_px,
            (part.left.round(), baseline),
            geometry.screen,
            None,
            GlyphRasterVariant::StatusLine,
        );
        glyphs.extend(run.glyphs);
        // Missing-glyph boxes are quads, which the glyph clip below does not reach.
        quads.extend(run.missing_boxes.into_iter().filter_map(|mut boxed| {
            let (left, top, width, height) =
                crate::cursor::ndc_rect_px(boxed.rect, screen_w, screen_h);
            let clipped_top = top.max(geometry.top);
            let clipped_bottom = (top + height).min(geometry.top + geometry.height);
            (clipped_bottom > clipped_top).then(|| {
                boxed.rect = px_to_ndc(
                    left,
                    clipped_top,
                    width,
                    clipped_bottom - clipped_top,
                    screen_w,
                    screen_h,
                );
                boxed
            })
        }));
    }
    crate::field_geometry::clip_glyphs_to_rect(
        glyphs,
        first_glyph,
        crate::field_geometry::FieldRect {
            x: 0.0,
            y: geometry.top,
            w: screen_w,
            h: geometry.height,
        },
        screen_w,
        screen_h,
    );
}

/// Thickness in raster px of the keyboard focus ring at display scale `scale`: two logical pixels,
/// never under one raster pixel, twice as thick with `[accessibility] strong_focus`.
pub fn focus_ring_thickness(scale: f32, strong: bool) -> f32 {
    let base = (2.0 * scale).max(1.0);
    if strong {
        base * 2.0
    } else {
        // When: `strong` is false, the ring keeps its base thickness.
        base
    }
}

/// The focus ring's four edges as `[left, top, width, height]` raster-px rects, inside the box
/// `left..left + width` by `top..top + height`; an edge never exceeds half the box.
pub fn focus_ring_rects(
    left: f32,
    width: f32,
    top: f32,
    height: f32,
    thickness: f32,
) -> [[f32; 4]; 4] {
    let edge = thickness.min(width / 2.0).min(height / 2.0).max(0.0);
    [
        [left, top, width, edge],
        [left, top + height - edge, width, edge],
        [left, top, edge, height],
        [left + width - edge, top, edge, height],
    ]
}

/// The strip band and the segments the last drawn frame presented, in raster px; what pointer
/// routing tests against. Plain data, so a window keeps a copy without its renderer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StripHit {
    /// Top edge of the band.
    pub top: f32,
    /// Height of the band.
    pub height: f32,
    /// Hit boxes of the presented segments.
    pub segments: Vec<PresentedStatusSegment>,
}

impl StripHit {
    /// Whether raster-px `y` is inside the band.
    pub fn contains(&self, y: f32) -> bool {
        y >= self.top && y < self.top + self.height
    }

    /// The presented segment under `(x, y)`; `None` outside the band or between segments.
    pub fn segment_at(&self, x: f32, y: f32) -> Option<&PresentedStatusSegment> {
        self.contains(y).then(|| segment_at(&self.segments, x)).flatten()
    }
}

/// Hit-test `x` against presented segments; `None` between segments.
pub fn segment_at(presented: &[PresentedStatusSegment], x: f32) -> Option<&PresentedStatusSegment> {
    presented.iter().find(|segment| x >= segment.x && x < segment.x + segment.width)
}

#[cfg(test)]
#[path = "status_strip_tests.rs"]
mod status_strip_tests;
