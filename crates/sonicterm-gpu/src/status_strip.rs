//! Status line strip: its height, colors and drawing.
//!
//! The strip sits between the panes and the tab bar (at the bottom edge when
//! the tab bar is hidden), shares the tab bar's `bg_base` background, and has
//! a hairline only on its terminal side. Segments come from
//! `sonicterm-ui::status_line` as a [`StatusLineView`]; this module measures
//! and fits them with the chrome font, then emits ordinary quads and glyphs,
//! so the GDI software path composites the strip unchanged.

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
/// Strip type size as a share of the tab title size, so it reads as secondary.
const TEXT_SHARE: f32 = 0.9;
/// Contrast a caution or failure color needs against the strip background.
const MIN_CONTRAST: f32 = 3.0;

/// Strip height in raster px for `font_size` (logical pt) at `scale_factor`:
/// 0.6 × the tab bar height with a 20 logical px floor, rounded to whole px.
pub fn status_line_height(font_size: f32, scale_factor: f32) -> f32 {
    let logical = (tab_bar_height(font_size) * HEIGHT_SHARE).max(MIN_HEIGHT_LOGICAL);
    (logical * scale_factor).round()
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
    /// Strip background (linear premultiplied).
    pub background: [f32; 4],
    /// Hairline between the terminal and the strip.
    pub hairline: [f32; 4],
    /// Chip background.
    pub chip: [f32; 4],
    /// Background of a chip that needs attention.
    pub chip_warn: [f32; 4],
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
            chip: palette.bg_active,
            chip_warn: crate::color::hex_to_premultiplied_rgba(
                theme.colors.ansi.yellow.0.as_str(),
                0.18,
            ),
        }
    }

    /// Text color of `tone`.
    fn text(&self, tone: StatusTone) -> ChromeColor {
        match tone {
            StatusTone::Name => self.name,
            StatusTone::Value | StatusTone::Chip => self.value,
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
    /// Surface size, raster px.
    pub screen: (f32, f32),
    /// Display scale factor.
    pub scale_factor: f32,
    /// Tab title raster size the strip text is derived from.
    pub title_raster_px: f32,
}

impl StripGeometry {
    /// The strip's text raster size.
    pub fn text_px(&self) -> f32 {
        (self.title_raster_px * TEXT_SHARE).max(1.0)
    }

    /// Fit metrics for this geometry.
    pub fn fit_metrics(&self) -> FitMetrics {
        let scale = self.scale_factor.max(0.5);
        FitMetrics {
            width: self.screen.0,
            edge_pad: 12.0 * scale,
            segment_gap: 20.0 * scale,
            chip_pad: 5.0 * scale,
            chip_gap: 4.0 * scale,
        }
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
            GlyphRasterVariant::TabTitle,
        )
        .width_px
    })
}

/// Emit the strip's band, hairline, chip backgrounds and glyphs. Glyphs are
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
    quads.push(QuadInstance {
        rect: px_to_ndc(0.0, geometry.top, screen_w, scale.round().max(1.0), screen_w, screen_h),
        color: colors.hairline,
        ..Default::default()
    });
    let (Some(font_stack), Some(rasterizer)) = (font_stack, rasterizer) else {
        // When: `font_stack` or `rasterizer` is missing, the chrome font is not loaded and the strip is a bare band.
        return;
    };
    let text_px = geometry.text_px();
    let baseline =
        geometry.top + ((geometry.height - text_px * 1.2) / 2.0).max(0.0) + text_px * 0.95;
    let chip_inset = (3.0 * scale).min(geometry.height / 4.0);
    let chip_pad = geometry.fit_metrics().chip_pad;
    let first_glyph = glyphs.len();
    for part in placed.iter().flat_map(|segment| &segment.parts) {
        let text_x = if part.tone.is_chip() {
            // A chip, paint its rounded background and inset the text by its padding.
            let chip_h = (geometry.height - 2.0 * chip_inset).max(1.0);
            let background =
                if part.tone == StatusTone::ChipWarn { colors.chip_warn } else { colors.chip };
            quads.push(QuadInstance::rounded(
                px_to_ndc(
                    part.left,
                    geometry.top + chip_inset,
                    part.width,
                    chip_h,
                    screen_w,
                    screen_h,
                ),
                background,
                [part.width, chip_h],
                3.0 * scale,
            ));
            part.left + chip_pad
        } else {
            // When: `is_chip` is false for the part's tone, its text starts at `part.left`.
            part.left
        };
        let run = chrome_text::layout_with_raster_variant(
            font_stack,
            rasterizer,
            atlas,
            &part.text,
            colors.text(part.tone),
            ChromeAttrs { bold: part.tone.is_chip(), italic: false },
            text_px,
            text_px,
            (text_x.round(), baseline),
            geometry.screen,
            None,
            GlyphRasterVariant::TabTitle,
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
