//! Status line strip: its height, colors and drawing.
//!
//! The strip sits at the top content edge, above the panes and below any
//! titlebar band, or with `position = "bottom"` between the panes and the tab
//! bar (at the bottom edge when the tab bar is hidden). It shares the tab bar's
//! `bg_base` background and has a hairline only on its terminal side. Segments come from
//! `sonicterm-ui::status_line` as a [`StatusLineView`]; this module measures
//! and fits them with the chrome font, then emits ordinary quads and glyphs,
//! so the GDI software path composites the strip unchanged.

use sonicterm_render_model::boundary::cfg::status_line::{StatusFontScale, StatusPosition};
use sonicterm_render_model::boundary::cfg::theme::Theme;
use sonicterm_render_model::boundary::ui::status_line::{
    fit_sized, separators, FitMetrics, PlacedPart, PlacedSegment, SegmentClick, StatusLineView,
    StatusTone, SEGMENT_SEPARATOR,
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
/// Smallest strip type size, in logical pt: FreeType raises any character size under 1 pt to
/// 1 pt, so the stack and the geometry both use the size it really draws.
const MIN_TEXT_PT: f32 = 1.0;
/// Contrast a caution or failure color needs against the strip background.
const MIN_CONTRAST: f32 = 3.0;
/// Contrast the quiet text of a caption band needs against it: the WCAG AA floor for small text.
pub(crate) const BAND_TEXT_CONTRAST: f32 = 4.5;

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

/// An integrated titlebar band's button height in raster px: `button_height` logical px at `scale`,
/// rounded to an even number of pixels so its midline, where the buttons centre, is a whole pixel.
pub fn band_button_px(button_height: f32, scale: f32) -> f32 {
    let raster = (button_height * scale).max(0.0);
    (raster / 2.0).round() * 2.0
}

/// How the integrated band and the strip share the window's top for one frame, in raster px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandLayout {
    /// The strip is shown at the top of an integrated window, so it is the band.
    pub merged: bool,
    /// Height the band reserves above the panes, and the strip's placement's titlebar term: the
    /// separate name band's height, or 0 when merged (the strip reserves it) or not integrated.
    pub separate_px: f32,
    /// The strip's drawn height: the merged band's, at least the buttons'; otherwise its own.
    pub strip_px: f32,
}

/// Lay out a window whose integrated band needs `button_px` (0 for none or a native titlebar) and
/// whose strip is `strip_px` tall at `position` while `strip_visible`. A merged band is
/// `max(buttons, strip)`; a separate name band is the same height, so a large font never clips it.
pub fn band_layout(
    integrated: bool,
    button_px: f32,
    strip_visible: bool,
    position: StatusPosition,
    strip_px: f32,
) -> BandLayout {
    let shown_strip = if strip_visible { strip_px } else { 0.0 };
    if !integrated {
        // When: integrated is false, the strip keeps its own height and there is no band.
        return BandLayout { merged: false, separate_px: 0.0, strip_px: shown_strip };
    }
    let merged = strip_visible && position == StatusPosition::Top;
    if merged {
        // When: merged, the strip is the band, at least the buttons' height.
        return BandLayout { merged, separate_px: 0.0, strip_px: button_px.max(strip_px) };
    }
    if button_px <= 0.0 {
        // When: button_px is 0 (full screen), there is no titlebar to share: no band at all.
        return BandLayout { merged, separate_px: 0.0, strip_px: shown_strip };
    }
    // A name-only band is at least the strip's height, so a large font never clips the name.
    BandLayout { merged, separate_px: button_px.max(strip_px), strip_px: shown_strip }
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

/// Black or white, whichever reads better on `background`: the last band text fallback, which
/// always reaches 4.5:1 since one of the two is at least that far from any color.
fn most_readable_extreme(background: [f32; 4]) -> ChromeColor {
    let black = ChromeColor::rgb(0, 0, 0);
    if contrast_ratio(ChromeColor::WHITE, background) >= contrast_ratio(black, background) {
        ChromeColor::WHITE
    } else {
        // When: black reads better than white on background, the band text is black.
        black
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
    /// Quiet text of a caption band (Windows): the name color when it reads at 4.5:1 on the band,
    /// else the theme's secondary text, its foreground, its `bright.white` or its active tab color,
    /// the first that does; else black or white, whichever reads better.
    pub band_name: ChromeColor,
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
        let secondary = palette.text_secondary.map(crate::color::linear_channel_to_srgb_u8);
        let secondary = ChromeColor::rgb(secondary[0], secondary[1], secondary[2]);
        let bright_white = hex_to_chrome_color(theme.colors.bright.white.0.as_str());
        let band_name = [tab_inactive_fg, secondary, foreground, bright_white, tab_active_fg]
            .into_iter()
            .find(|color| contrast_ratio(*color, background) >= BAND_TEXT_CONTRAST)
            .unwrap_or_else(|| most_readable_extreme(background));
        Self {
            chip: readable_or(tab_active_fg, foreground, background),
            value: tab_active_fg,
            name: tab_inactive_fg,
            band_name,
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

    /// These colors for a surface drawn in a band with `buttons` (`in_band`), in a window that is
    /// `focused` or not: a caption band draws quiet text in `band_name`, and an unfocused one its
    /// values too, as Windows dims an inactive title. Cautions, chips and every other surface,
    /// macOS's band included, keep their colors.
    pub fn for_band_focus(
        self,
        buttons: sonicterm_render_model::boundary::ui::window_band::BandButtons,
        in_band: bool,
        focused: bool,
    ) -> Self {
        use sonicterm_render_model::boundary::ui::window_band::BandButtons;
        if !(in_band && buttons == BandButtons::Caption) {
            // When: not in_band with Caption buttons, the surface keeps every color.
            return self;
        }
        let value = if focused {
            self.value
        } else {
            // An inactive window's values quieten too.
            self.band_name
        };
        Self { value, name: self.band_name, ..self }
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
    /// The terminal font's live raster size; the strip text is `font_scale` × the tab titles' size,
    /// and exactly their size in a band.
    pub font_raster_px: f32,
    /// `[status_line] font_scale`: the strip text's share of that size.
    pub font_scale: f32,
    /// Cap height of the strip font at [`Self::text_px`], raster px; see [`strip_cap_height`].
    pub cap_height: f32,
    /// Where an integrated titlebar band puts the strip; the default is an ordinary strip.
    pub band: StripBand,
}

/// What an integrated titlebar band changes about a strip drawn in it, in raster px.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StripBand {
    /// The strip is an integrated titlebar band: its text centres on the whole band's midline,
    /// which is the window buttons' centre line, instead of the area outside the hairline.
    pub in_band: bool,
    /// Width reserved at the left edge for the traffic lights.
    pub left_inset: f32,
    /// Width reserved at the right edge for the caption buttons.
    pub right_inset: f32,
}

impl StripGeometry {
    /// The strip's text raster size at the display scale, at least 1 pt: the tab titles' size in an
    /// integrated titlebar band, so the titlebar reads like the tab bar, and otherwise
    /// `font_scale` × that size, which is what [`status_line_font_size`] asks the stack for.
    pub fn text_px(&self) -> f32 {
        let scale = self.scale_factor.max(0.5);
        let font_scale = if self.band.in_band { 1.0 } else { self.font_scale };
        (status_line_font_size(self.font_raster_px / scale, font_scale) * scale).max(1.0)
    }

    /// The atlas raster variant the strip's full-size text is drawn on: the tab titles' in a band,
    /// whose text is their size, so its tiles are theirs; the strip's own otherwise.
    pub fn raster_variant(&self) -> GlyphRasterVariant {
        if self.band.in_band {
            // When: band.in_band, the text is the tab titles' size and shares their tiles.
            return GlyphRasterVariant::TabTitle;
        }
        GlyphRasterVariant::StatusLine
    }

    /// The chrome run cache's stack for the strip's full-size text, matching
    /// [`Self::raster_variant`].
    pub fn chrome_stack(&self) -> crate::chrome_cache::ChromeStack {
        use crate::chrome_cache::ChromeStack;
        if self.band.in_band {
            // When: band.in_band, runs are shaped with the tab-title stack.
            return ChromeStack::TabTitle;
        }
        ChromeStack::StatusLine
    }

    /// Fit metrics for this geometry.
    pub fn fit_metrics(&self) -> FitMetrics {
        let scale = self.scale_factor.max(0.5);
        FitMetrics {
            width: self.screen.0,
            edge_pad: 12.0 * scale,
            segment_gap: 20.0 * scale,
            // Small side text sits closer: two adjacent small segments of a list are 16 px apart.
            small_gap: 16.0 * scale,
            separator_width: 0.0,
            left_inset: self.band.left_inset,
            right_inset: self.band.right_inset,
        }
    }
}

/// A fitted strip: the presented segments and the `|` between adjacent ones of a list. Only the
/// segments are hit boxes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FittedStrip {
    /// The presented segments, in view order.
    pub segments: Vec<PlacedSegment>,
    /// The separators between them; drawn, never routed or read aloud.
    pub separators: Vec<PlacedPart>,
}

/// The smaller text a band's side segments are drawn in: the palette footer's stack, rasterizer
/// and raster size. It uses that stack's own glyph variant, so its tiles never collide with the
/// strip's, which are keyed without a size.
pub struct SmallText<'stack, Raster> {
    /// The palette footer's stack.
    pub font_stack: &'stack sonicterm_engine::FontStack,
    /// Its rasterizer.
    pub rasterizer: &'stack mut Raster,
    /// Raster size of the small text, under the strip's own.
    pub text_px: f32,
}

/// Fit `view` into the strip, measuring each run and the separator with the chrome font; a small
/// segment's runs are measured in `small` when there is one.
pub fn fit_strip<Raster: Rasterizer>(
    view: &StatusLineView,
    geometry: &StripGeometry,
    font_stack: &sonicterm_engine::FontStack,
    rasterizer: &mut Raster,
    atlas: &mut GlyphAtlas,
    mut small: Option<SmallText<'_, Raster>>,
) -> FittedStrip {
    let text_px = geometry.text_px();
    let mut measure = |text: &str, is_small: bool| {
        let (stack, raster, size, variant) = match small.as_mut() {
            Some(small) if is_small => (
                small.font_stack,
                &mut *small.rasterizer,
                small.text_px,
                GlyphRasterVariant::PaletteFooter,
            ),
            // A full-size segment's text, or no small text: the strip's own size.
            _ => (font_stack, &mut *rasterizer, text_px, geometry.raster_variant()),
        };
        chrome_text::layout_with_raster_variant(
            stack,
            raster,
            atlas,
            text,
            ChromeColor::WHITE,
            ChromeAttrs { bold: false, italic: false },
            size,
            size,
            (0.0, 0.0),
            geometry.screen,
            None,
            variant,
        )
        .width_px
    };
    let metrics =
        FitMetrics { separator_width: measure(SEGMENT_SEPARATOR, false), ..geometry.fit_metrics() };
    let segments = fit_sized(view, &metrics, measure);
    let separators = separators(view, &segments, &metrics);
    FittedStrip { segments, separators }
}

/// Emit the strip's band, hairline and glyphs; chips are colored text with no background. Glyphs are
/// clipped to the band so no ink lands outside it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_strip<Raster: Rasterizer>(
    placed: &[PlacedSegment],
    separators: &[PlacedPart],
    geometry: &StripGeometry,
    colors: &StatusColors,
    font_stack: Option<&sonicterm_engine::FontStack>,
    rasterizer: Option<&mut Raster>,
    mut small: Option<SmallText<'_, Raster>>,
    atlas: &mut GlyphAtlas,
    runs: &mut crate::chrome_cache::ChromeRunCache,
    reuse: bool,
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
    // Every run shares the strip's baseline, so small side text sits on the name's line.
    let baseline = strip_baseline(geometry);
    let first_glyph = glyphs.len();
    let parts = placed
        .iter()
        .flat_map(|segment| segment.parts.iter().map(move |part| (part, segment.small)))
        .chain(separators.iter().map(|part| (part, false)));
    for (part, is_small) in parts {
        if part.text.is_empty() || screen_w <= 0.0 || screen_h <= 0.0 {
            // When: the part has no text or the surface has no area, nothing is shaped or drawn.
            continue;
        }
        // A chip has no background: its tone colors its text and `is_chip` makes it bold. Each part
        // is kept by text, bold, stack and size (DPI is in `text_px`), so an unchanged strip is not
        // reshaped; every face replacement clears the kept runs.
        let attrs = ChromeAttrs { bold: part.tone.is_chip(), italic: false };
        let (stack, raster, size, chrome_stack, variant) = match small.as_mut() {
            Some(small) if is_small => (
                small.font_stack,
                &mut *small.rasterizer,
                small.text_px,
                crate::chrome_cache::ChromeStack::PaletteFooter,
                GlyphRasterVariant::PaletteFooter,
            ),
            // A full-size segment's part, a separator, or no small text: the strip's own size.
            _ => (
                font_stack,
                &mut *rasterizer,
                text_px,
                geometry.chrome_stack(),
                geometry.raster_variant(),
            ),
        };
        let key = crate::chrome_cache::ChromeRunKey::new(chrome_stack, attrs, size, size);
        let handle = runs.prepare(stack, &part.text, key, reuse);
        let Some(view) = runs.view(&handle) else {
            // When: `runs.view(&handle)` is None, shaping failed; the part is dropped and reported.
            chrome_text::note_unshaped_chrome(&part.text);
            continue;
        };
        let run = chrome_text::layout_view(
            view,
            raster,
            atlas,
            colors.text(part.tone),
            (part.left.round(), baseline),
            geometry.screen,
            None,
            variant,
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

/// Compare the strip tiles a resized renderer holds, `live`, with those of a cold renderer built at
/// the same final size drawing the same text, `cold`, each as `(glyph, tile width, tile height)`.
///
/// # Errors
/// Every glyph whose tile size differs, and every glyph only one side holds.
#[doc(hidden)]
pub fn compare_strip_tile_sizes<Glyph>(
    live: &[(Glyph, u32, u32)],
    cold: &[(Glyph, u32, u32)],
) -> Result<(), String>
where
    Glyph: std::hash::Hash + Eq + std::fmt::Debug,
{
    /// Each glyph's tile size, keyed by the glyph.
    fn sizes<Glyph: std::hash::Hash + Eq>(
        tiles: &[(Glyph, u32, u32)],
    ) -> std::collections::HashMap<&Glyph, [u32; 2]> {
        tiles.iter().map(|(glyph, width, height)| (glyph, [*width, *height])).collect()
    }
    let (live, cold) = (sizes(live), sizes(cold));
    // A glyph both sides hold at different sizes is a stale tile; one only the live atlas holds is
    // a leftover; one only the cold renderer holds was never redrawn.
    let resized = cold.iter().filter_map(|(glyph, [cold_w, cold_h])| {
        let [live_w, live_h] = live.get(glyph)?;
        ([*live_w, *live_h] != [*cold_w, *cold_h]).then(|| {
            format!("{glyph:?} is {live_w}x{live_h}, a cold renderer draws {cold_w}x{cold_h}")
        })
    });
    let absent = cold
        .keys()
        .filter(|glyph| !live.contains_key(*glyph))
        .map(|glyph| format!("{glyph:?} is not resident; a cold renderer draws it"));
    let leftover = live
        .keys()
        .filter(|glyph| !cold.contains_key(*glyph))
        .map(|glyph| format!("{glyph:?} is resident, but a cold renderer never draws it"));
    let mut problems: Vec<String> = resized.chain(absent).chain(leftover).collect();
    // Hash order varies from run to run, so the report is sorted to read the same each time.
    problems.sort();
    problems.is_empty().then_some(()).ok_or_else(|| problems.join("; "))
}

#[cfg(test)]
#[path = "status_strip_tests.rs"]
mod status_strip_tests;

#[cfg(test)]
#[path = "status_strip_band_tests.rs"]
mod status_strip_band_tests;

#[path = "status_strip_text.rs"]
mod text;

pub use text::{
    default_font_scale, kept_cap_height, resolve_cap_height, status_line_font_size, strip_baseline,
    strip_cap_height,
};
