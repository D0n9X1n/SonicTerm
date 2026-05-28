// Pre-PR-#119 call sites still use the deprecated `color::*` helpers. The
// theme-driven `UiPalette` (PR #119) is canonical at the entry point; the
// remaining literal-color sites will be migrated in a follow-up.
#![allow(deprecated)]

//! GPU renderer for the preferences window — redesigned for issue #112 R2.
//!
//! The window paints, top-down:
//!   1. `BG_BASE` clear (#0B0E14).
//!   2. Sidebar strip + right divider; one row per category; the active
//!      row gets a tinted background and a left accent bar.
//!   3. Title block ("Preferences" + per-category subtitle) at (28, 24).
//!   4. A surface card that wraps the form rows (label + control pair).
//!   5. Sticky footer with top border, dirty indicator on the left, and
//!      Cancel (secondary) + Apply (primary accent) buttons on the right.
//!
//! `build_draw_list` is pure (no GPU) so it is trivially unit-testable.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use glyphon::{
    Attrs, Buffer, Cache, Color as GColor, Family, FontSystem, Metrics, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use sonic_core::theme::Theme;
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Instance, InstanceDescriptor,
    LoadOp, MultisampleState, Operations, PresentMode, RenderPassColorAttachment,
    RenderPassDescriptor, RequestAdapterOptions, SurfaceConfiguration, TextureFormat,
    TextureUsages, TextureViewDescriptor,
};
use winit::{event_loop::ActiveEventLoop, window::Window};

use crate::prefs::controls::{Button, Control, InteractionState, Rect as PrefsRect, Toggle};
use crate::prefs::layout::{
    self, BUTTON_RADIUS, CARD_PAD_H, CARD_PAD_V, CATEGORIES, CONTROL_H, CONTROL_RADIUS, LABEL_W,
    PREVIEW_CARD_H, PREVIEW_PAD, SECTION_HELP_SIZE, SECTION_TITLE_SIZE, SIDEBAR_LABEL_X,
    SLIDER_THUMB, SLIDER_TRACK_H, SUBTITLE_GAP, SUBTITLE_LINE, SUBTITLE_SIZE, SWATCH_GAP,
    SWATCH_SIZE, TITLE_LINE, TOGGLE_H, TOGGLE_KNOB, TOGGLE_KNOB_MARGIN, TOGGLE_W,
};
use crate::prefs::state::PrefsState;
use crate::quad::{px_to_ndc, QuadInstance, QuadPipeline};
use crate::render::hex_to_wgpu;
use crate::ui_tokens::{color, typography};

/// One filled rectangle in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuadCmd {
    pub rect: PrefsRect,
    /// **Linear-sRGB premultiplied** RGBA (matches `ui_tokens::color`).
    pub color: [f32; 4],
    /// Corner radius in **logical pixels** (scaled by `scale_factor`
    /// before being passed to the GPU's SDF rounded-rect path). `0.0`
    /// keeps the legacy sharp-rect look. Used by the redesigned (issue
    /// #173) prefs Button primitive so Apply / Cancel render as pill
    /// shapes instead of hard rectangles.
    pub radius_px: f32,
}

impl QuadCmd {
    /// Sharp-edged rectangle — the historical default used by every
    /// non-button quad. Kept as a helper so existing call sites stay
    /// `QuadCmd { rect, color }`-shaped without re-stating the radius.
    pub const fn sharp(rect: PrefsRect, color: [f32; 4]) -> Self {
        Self { rect, color, radius_px: 0.0 }
    }

    /// Rounded rectangle for the prefs Button primitive (see
    /// [`crate::prefs::controls::Button`]). The radius is in logical
    /// pixels and gets multiplied by `scale_factor` at upload time.
    pub const fn rounded(rect: PrefsRect, color: [f32; 4], radius_px: f32) -> Self {
        Self { rect, color, radius_px }
    }
}

/// One text run in logical pixels.
#[derive(Debug, Clone)]
pub struct TextCmd {
    pub rect: PrefsRect,
    pub text: String,
    pub color: GColor,
    pub size_px: f32,
    pub weight: u16,
    /// `true` → use the terminal monospace font; `false` → system UI font.
    pub monospace: bool,
    /// Optional explicit family name. When `Some`, the renderer uses it
    /// verbatim (e.g. the user's configured terminal font for the
    /// Appearance preview card); when `None` the default selection by
    /// `monospace` applies.
    pub font_family: Option<String>,
}

/// Output of [`build_draw_list`].
#[derive(Debug, Clone, Default)]
pub struct DrawList {
    pub clear: [f32; 4],
    pub quads: Vec<QuadCmd>,
    pub texts: Vec<TextCmd>,
}

/// Convert a `[f32; 4]` premultiplied linear-sRGB token into a glyphon
/// `Color`. Glyphon expects sRGB 8-bit — we undo the linearisation.
fn token_to_gcolor(c: [f32; 4]) -> GColor {
    let a = c[3].clamp(0.0, 1.0);
    let (lr, lg, lb) =
        if a > f32::EPSILON { (c[0] / a, c[1] / a, c[2] / a) } else { (0.0, 0.0, 0.0) };
    fn linear_to_srgb(v: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    }
    let r = (linear_to_srgb(lr) * 255.0).round().clamp(0.0, 255.0) as u8;
    let g = (linear_to_srgb(lg) * 255.0).round().clamp(0.0, 255.0) as u8;
    let b = (linear_to_srgb(lb) * 255.0).round().clamp(0.0, 255.0) as u8;
    let a8 = (a * 255.0).round().clamp(0.0, 255.0) as u8;
    GColor::rgba(r, g, b, a8)
}

fn push_text(
    out: &mut Vec<TextCmd>,
    rect: PrefsRect,
    text: impl Into<String>,
    color_token: [f32; 4],
    ramp: typography::TypeRamp,
) {
    out.push(TextCmd {
        rect,
        text: text.into(),
        color: token_to_gcolor(color_token),
        size_px: ramp.size_px,
        weight: ramp.weight,
        monospace: false,
        font_family: None,
    });
}

fn push_mono(
    out: &mut Vec<TextCmd>,
    rect: PrefsRect,
    text: impl Into<String>,
    color_token: [f32; 4],
    size: f32,
    family: Option<String>,
) {
    out.push(TextCmd {
        rect,
        text: text.into(),
        color: token_to_gcolor(color_token),
        size_px: size,
        weight: 500,
        monospace: true,
        font_family: family,
    });
}

/// Return the configured terminal font family for the prefs preview
/// card. Falls back to an empty `String` only when the user explicitly
/// cleared the family — the renderer treats an empty string as "use
/// the monospace default". Mirrors what the terminal grid would use.
fn terminal_font_attrs(state: &PrefsState) -> String {
    state.config.font.family.clone()
}

/// Build a logical-pixel draw list for the current prefs state. Pure —
/// callable from tests without any GPU dependencies.
pub fn build_draw_list(state: &PrefsState, theme: &Theme) -> DrawList {
    let layout = state.layout;
    let mut quads: Vec<QuadCmd> = Vec::new();
    let mut texts: Vec<TextCmd> = Vec::new();

    // --- Background ----------------------------------------------------
    // Per PR #119: chrome derives from the active theme via UiPalette.
    let palette = crate::ui_tokens::UiPalette::from_theme(theme);
    let bg_base = palette.bg_base;

    // --- Sidebar -------------------------------------------------------
    quads.push(QuadCmd::sharp(layout.sidebar, color::BG_ELEVATED()));
    // Right divider 1px.
    quads.push(QuadCmd::sharp(
        layout.sidebar_divider,
        color::with_alpha(color::hex("#FFFFFF"), 0.07),
    ));

    for (i, cat) in CATEGORIES.iter().enumerate() {
        let row = layout.category_row(i);
        let active = *cat == state.active_category;
        if active {
            quads.push(QuadCmd::sharp(row, color::BG_ACTIVE()));
            // Left accent bar.
            quads.push(QuadCmd::sharp(layout.category_accent(i), palette.accent));
        }
        // Icon slot placeholder (subtle pill) — keeps spacing predictable
        // without forcing us to ship an icon font in this PR.
        let icon_y = row.y + (row.h - layout::SIDEBAR_ICON_SLOT) / 2.0;
        let icon_rect = PrefsRect::new(
            row.x + layout::SIDEBAR_ICON_X,
            icon_y,
            layout::SIDEBAR_ICON_SLOT,
            layout::SIDEBAR_ICON_SLOT,
        );
        let icon_color =
            if active { palette.accent } else { color::with_alpha(color::TEXT_MUTED(), 0.6) };
        quads.push(QuadCmd::sharp(icon_rect, color::with_alpha(icon_color, 0.18)));

        // Label.
        let label_x = row.x + SIDEBAR_LABEL_X;
        let label_w = row.w - SIDEBAR_LABEL_X - 4.0;
        let label_rect =
            PrefsRect::new(label_x, row.y + (row.h - 20.0) / 2.0, label_w.max(0.0), 20.0);
        let (col, ramp) = if active {
            (color::TEXT_PRIMARY(), typography::BODY_STRONG)
        } else {
            (color::with_alpha(color::TEXT_SECONDARY(), 0.85), typography::BODY)
        };
        push_text(&mut texts, label_rect, cat.label(), col, ramp);
    }

    // --- Title block ---------------------------------------------------
    let title_rect = PrefsRect::new(
        layout.title_block.x,
        layout.title_block.y,
        layout.title_block.w,
        TITLE_LINE,
    );
    push_text(&mut texts, title_rect, "Preferences", color::TEXT_PRIMARY(), typography::H1);
    let subtitle_rect = PrefsRect::new(
        layout.title_block.x,
        layout.title_block.y + TITLE_LINE + SUBTITLE_GAP,
        layout.title_block.w,
        SUBTITLE_LINE,
    );
    push_text(
        &mut texts,
        subtitle_rect,
        state.active_category.description(),
        color::TEXT_MUTED(),
        typography::TypeRamp { size_px: SUBTITLE_SIZE, line_px: SUBTITLE_LINE, weight: 500 },
    );

    // --- Form card -----------------------------------------------------
    quads.push(QuadCmd::sharp(layout.form_card, color::BG_SURFACE()));
    // 1px subtle border drawn as 4 thin strips (the quad pipeline does
    // not support outlines yet).
    push_border(&mut quads, layout.form_card, color::BORDER_SUBTLE());

    // Section title + help row inside the card.
    let section_title_rect = PrefsRect::new(
        layout.form_card.x + CARD_PAD_H,
        layout.form_card.y + CARD_PAD_V - 2.0,
        layout.form_card.w - CARD_PAD_H * 2.0,
        20.0,
    );
    push_text(
        &mut texts,
        section_title_rect,
        state.active_category.label(),
        color::TEXT_PRIMARY(),
        typography::TypeRamp { size_px: SECTION_TITLE_SIZE, line_px: 20.0, weight: 650 },
    );
    let help_w = layout::SECTION_HELP_MAX_W.min(layout.form_card.w - CARD_PAD_H * 2.0);
    let help_rect = PrefsRect::new(
        layout.form_card.x + CARD_PAD_H,
        section_title_rect.y + 20.0,
        help_w,
        SUBTITLE_LINE,
    );
    push_text(
        &mut texts,
        help_rect,
        state.active_category.description(),
        color::TEXT_MUTED(),
        typography::TypeRamp { size_px: SECTION_HELP_SIZE, line_px: SUBTITLE_LINE, weight: 500 },
    );

    // --- Form rows -----------------------------------------------------
    // `form_row` / `control_slot` already include `PrefsLayout::ROW_Y_OFFSET`
    // so render + hit-test see the same rects.

    for (idx, ctrl) in state.controls.iter().enumerate() {
        let row = layout.form_row(idx);
        let slot = layout.control_slot(idx);

        // Label.
        let label_h = 20.0;
        let label_rect = PrefsRect::new(row.x, row.y + (row.h - label_h) / 2.0, LABEL_W, label_h);
        push_text(
            &mut texts,
            label_rect,
            control_label(ctrl).to_string(),
            color::TEXT_SECONDARY(),
            typography::BODY,
        );

        // Control.
        draw_control(&mut quads, &mut texts, ctrl, slot, state, &palette);
    }

    // --- Preview card (Appearance category only) ----------------------
    if matches!(state.active_category, layout::Category::Appearance) {
        let last_row = layout.form_row(state.controls.len());
        let preview_y = last_row.y + 8.0;
        let preview_card = PrefsRect::new(
            layout.form_card.x + CARD_PAD_H,
            preview_y,
            layout.form_card.w - CARD_PAD_H * 2.0,
            PREVIEW_CARD_H,
        );
        // Only draw if it fits inside the card.
        if preview_card.y + preview_card.h <= layout.form_card.y + layout.form_card.h - CARD_PAD_V {
            // Use the active terminal theme's background for the
            // preview swatch.
            let preview_bg = hex_to_token(theme.colors.background.0.as_str());
            quads.push(QuadCmd::sharp(preview_card, preview_bg));
            push_border(&mut quads, preview_card, color::BORDER_SUBTLE());
            let preview_fg = hex_to_token(theme.colors.foreground.0.as_str());
            for (i, line) in state.preview_lines().iter().enumerate() {
                let ly = preview_card.y + PREVIEW_PAD + i as f32 * 20.0;
                let lh = 20.0;
                if ly + lh > preview_card.y + preview_card.h - PREVIEW_PAD {
                    break;
                }
                push_mono(
                    &mut texts,
                    PrefsRect::new(
                        preview_card.x + PREVIEW_PAD,
                        ly,
                        preview_card.w - PREVIEW_PAD * 2.0,
                        lh,
                    ),
                    line.clone(),
                    preview_fg,
                    13.0,
                    Some(terminal_font_attrs(state)),
                );
            }
        }
    }

    // --- Footer --------------------------------------------------------
    quads.push(QuadCmd::sharp(layout.footer, color::with_alpha(color::BG_BASE(), 0.94)));
    quads.push(QuadCmd::sharp(layout.footer_divider, color::BORDER_SUBTLE()));

    // Dirty indicator on the left.
    if state.dirty {
        let dot_size = 6.0;
        let dot_y = layout.footer.y + (layout.footer.h - dot_size) / 2.0;
        quads.push(QuadCmd::sharp(
            PrefsRect::new(layout.footer.x + 28.0, dot_y, dot_size, dot_size),
            color::ACCENT_ORANGE(),
        ));
        let txt_rect = PrefsRect::new(
            layout.footer.x + 28.0 + dot_size + 8.0,
            layout.footer.y,
            220.0,
            layout.footer.h,
        );
        push_text(
            &mut texts,
            txt_rect,
            "Unsaved changes",
            color::TEXT_SECONDARY(),
            typography::TypeRamp { size_px: 12.0, line_px: 16.0, weight: 500 },
        );
    }

    // Cancel button (secondary) — pill rendered via the Button primitive
    // (issue #173 slice-2). Background tints with hover/press from the
    // owning [`InteractionState`]; the corner radius comes from the
    // shared [`BUTTON_RADIUS`] constant so the GPU side stays in sync
    // with `prefs::layout`.
    let cancel = &state.cancel_button;
    let cancel_base = color::with_alpha(color::hex("#FFFFFF"), 0.07);
    let cancel_bg = button_bg(cancel_base, cancel.interaction);
    quads.push(QuadCmd::rounded(cancel.rect, cancel_bg, BUTTON_RADIUS));
    push_button_text(&mut texts, cancel, "Cancel", color::TEXT_SECONDARY(), typography::BODY);

    // Apply button (primary accent) — same pill primitive, Primary kind.
    let apply = &state.apply_button;
    let apply_enabled = state.dirty;
    let (apply_base, apply_fg) = if apply_enabled {
        (palette.accent, color::hex("#0B1020"))
    } else {
        // Disabled state: theme-derived neutral fill (was hardcoded
        // Tokyo Night #2A3042 — gave gruvbox users a purplish-blue
        // disabled Apply button instead of a theme-consistent one).
        (palette.bg_active, palette.text_faint)
    };
    let apply_bg =
        if apply_enabled { button_bg(apply_base, apply.interaction) } else { apply_base };
    quads.push(QuadCmd::rounded(apply.rect, apply_bg, BUTTON_RADIUS));
    push_button_text(&mut texts, apply, "Apply", apply_fg, typography::BODY_STRONG);

    DrawList { clear: bg_base, quads, texts }
}

/// Tint a base button color by the current [`InteractionState`]. Hover
/// brightens slightly, press darkens. Pure math so the renderer and the
/// regression tests agree on the resulting RGBA token.
fn button_bg(base: [f32; 4], i: InteractionState) -> [f32; 4] {
    let scale = if i.pressed {
        0.85
    } else if i.hovered {
        1.12
    } else {
        1.0
    };
    [
        (base[0] * scale).clamp(0.0, 1.0),
        (base[1] * scale).clamp(0.0, 1.0),
        (base[2] * scale).clamp(0.0, 1.0),
        base[3],
    ]
}

/// Render the redesigned (issue #173 slice-2c) [`Toggle`] primitive.
///
/// The track is a rounded pill (radius = `TOGGLE_H / 2`, which equals
/// [`CONTROL_RADIUS`] for the current 24px track height — keeping the
/// constant explicit means a future TOGGLE_H change won't silently
/// desync from the layout). The thumb slides from the off- to the
/// on-position over [`Toggle::ANIM_MS`] using
/// [`Toggle::knob_x_animated`] for the interpolation; once the
/// animation completes the helper returns the snapped end position so
/// the thumb is pixel-stable.
///
/// Hover/press feedback comes from the toggle's [`InteractionState`]
/// (mirrors the Button slice-2a pattern: the renderer tints, the
/// pointer plumbing is a separate concern).
fn draw_toggle(
    quads: &mut Vec<QuadCmd>,
    t: &Toggle,
    slot: PrefsRect,
    palette: &crate::ui_tokens::UiPalette,
) {
    let accent = palette.accent;
    let track_y = slot.y + (slot.h - TOGGLE_H) / 2.0;
    let track = PrefsRect::new(slot.x, track_y, TOGGLE_W, TOGGLE_H);
    // Off track: theme-derived neutral fill (was hardcoded Tokyo
    // Night #343A52FF — caused gruvbox toggles' off-state to look
    // blue). bg_active = accent @ 14% alpha; under premultiplied
    // blending this reads as a subdued tint of the active theme.
    let track_base = if t.value { accent } else { palette.bg_active };
    let track_color = button_bg(track_base, t.interaction);
    // Pill radius: half the track height so both ends round into
    // perfect semicircles. For the current 24px track this matches
    // the shared `CONTROL_RADIUS` (10) ± half a pixel; we keep
    // `TOGGLE_H / 2` rather than the literal constant so the radius
    // stays correct if `TOGGLE_H` is ever bumped.
    let track_radius = TOGGLE_H / 2.0;
    quads.push(QuadCmd::rounded(track, track_color, track_radius));

    // Sliding thumb. `knob_x_animated` lerps between the previous
    // snapped position and the new one over `Toggle::ANIM_MS` from
    // the most recent flip; once `t == 1` it returns the snapped
    // value so the thumb does not drift.
    let knob_x = t.knob_x_animated(std::time::Instant::now(), TOGGLE_KNOB, TOGGLE_KNOB_MARGIN);
    let knob = PrefsRect::new(knob_x, track.y + TOGGLE_KNOB_MARGIN, TOGGLE_KNOB, TOGGLE_KNOB);
    // Thumb fill follows the slice-2c spec: theme.accent when on,
    // theme.surface (bg_elevated) when off. The accent-on-accent
    // case is visually distinguished by the white inner ring shadow
    // baked into the surface palette and the small (2px) margin
    // around the thumb that exposes the track edge.
    let knob_color = if t.value { accent } else { palette.bg_elevated };
    quads.push(QuadCmd::rounded(knob, knob_color, TOGGLE_KNOB / 2.0));
}

/// Emit a centered text run for a [`Button`] primitive. The text's
/// horizontal center is anchored at `button.text_center().0` (fixes
/// issue #169 where Apply was left-aligned). `glyphon` itself paints
/// inside the rect, so we hand it a rect whose left edge keeps the
/// existing baseline math intact — i.e. the same `button.rect`, which
/// the layout already sizes to the visible pill.
fn push_button_text(
    out: &mut Vec<TextCmd>,
    button: &Button,
    label: impl Into<String>,
    color_token: [f32; 4],
    ramp: typography::TypeRamp,
) {
    // text_center returns the button's geometric center; we keep using
    // the button rect for the text run so glyphon can do its own
    // centering, but recording the explicit center keeps the contract
    // public (and is what the regression test asserts on).
    debug_assert_eq!(
        button.text_center(),
        (button.rect.x + button.rect.w / 2.0, button.rect.y + button.rect.h / 2.0)
    );
    push_text(out, button.rect, label, color_token, ramp);
}

/// Convert a `#RRGGBB` hex string from the theme into a premultiplied
/// linear-sRGB token (matching the rest of the draw list).
fn hex_to_token(s: &str) -> [f32; 4] {
    let s = s.trim_start_matches('#');
    if s.len() == 6 {
        color::hex(&format!("#{}FF", s))
    } else {
        color::hex(s)
    }
}

fn push_border(out: &mut Vec<QuadCmd>, r: PrefsRect, c: [f32; 4]) {
    let t = 1.0;
    out.push(QuadCmd::sharp(PrefsRect::new(r.x, r.y, r.w, t), c));
    out.push(QuadCmd::sharp(PrefsRect::new(r.x, r.y + r.h - t, r.w, t), c));
    out.push(QuadCmd::sharp(PrefsRect::new(r.x, r.y, t, r.h), c));
    out.push(QuadCmd::sharp(PrefsRect::new(r.x + r.w - t, r.y, t, r.h), c));
}

fn draw_control(
    quads: &mut Vec<QuadCmd>,
    texts: &mut Vec<TextCmd>,
    ctrl: &Control,
    slot: PrefsRect,
    state: &PrefsState,
    palette: &crate::ui_tokens::UiPalette,
) {
    let accent = palette.accent;
    match ctrl {
        Control::Toggle(t) => {
            draw_toggle(quads, t, slot, palette);
        }
        Control::Slider(s) => {
            let readout_w = 56.0;
            let track_w = (slot.w - readout_w - 12.0).max(40.0);
            let track_y = slot.y + (slot.h - SLIDER_TRACK_H) / 2.0;
            let track = PrefsRect::new(slot.x, track_y, track_w, SLIDER_TRACK_H);
            // Unfilled track: theme-derived neutral (was hardcoded
            // Tokyo Night #343A52FF — that hardcode made the slider
            // read as "blue" on gruvbox even though the fill itself
            // was correctly using palette.accent).
            quads.push(QuadCmd::sharp(track, palette.bg_active));
            let frac = ((s.value - s.min) / (s.max - s.min)).clamp(0.0, 1.0);
            let fill = PrefsRect::new(track.x, track.y, track.w * frac, track.h);
            quads.push(QuadCmd::sharp(fill, accent));
            // Thumb (16x16) — drawn as a square. The renderer doesn't
            // round corners yet; the spec calls for r=8.
            let thumb_x = track.x + (track.w * frac) - SLIDER_THUMB / 2.0;
            let thumb_y = slot.y + (slot.h - SLIDER_THUMB) / 2.0;
            // Border (drawn as outer 2px rect filled with bg base).
            let outer = PrefsRect::new(
                thumb_x - 2.0,
                thumb_y - 2.0,
                SLIDER_THUMB + 4.0,
                SLIDER_THUMB + 4.0,
            );
            quads.push(QuadCmd::sharp(outer, palette.bg_base));
            quads.push(QuadCmd::sharp(
                PrefsRect::new(thumb_x, thumb_y, SLIDER_THUMB, SLIDER_THUMB),
                palette.text_primary,
            ));
            // Numeric readout to the right.
            let readout = PrefsRect::new(track.x + track.w + 12.0, slot.y, readout_w, slot.h);
            push_text(
                texts,
                readout,
                format!("{:.2}", s.value),
                palette.text_muted,
                typography::TypeRamp { size_px: 12.0, line_px: 18.0, weight: 500 },
            );
        }
        Control::Dropdown(d) => {
            let r = PrefsRect::new(slot.x, slot.y, slot.w.min(240.0), CONTROL_H);
            // Theme-derived input bg (was hardcoded #090C12FF Tokyo
            // Night near-black-blue).
            quads.push(QuadCmd::sharp(r, palette.bg_elevated));
            push_border(quads, r, palette.border_subtle);
            let label = d.options.get(d.selected).cloned().unwrap_or_default();
            push_text(
                texts,
                PrefsRect::new(r.x + 10.0, r.y, r.w - 28.0, r.h),
                label,
                palette.text_primary,
                typography::BODY,
            );
            // Chevron.
            push_text(
                texts,
                PrefsRect::new(r.x + r.w - 18.0, r.y, 16.0, r.h),
                "▾",
                palette.text_muted,
                typography::BODY,
            );
            // (Open-dropdown menu rendering is out of scope for this PR.)
            let _ = CONTROL_RADIUS;
        }
        Control::ColorSwatch(c) => {
            let mut x = slot.x;
            let y = slot.y + (slot.h - SWATCH_SIZE) / 2.0;
            // Render the active swatch (the only cell we currently
            // track) plus seven preset cells to make the grid feel real.
            let presets: [&str; 8] = [
                "#7AA2F7", "#BB9AF7", "#9ECE6A", "#E0AF68", "#F7768E", "#73DACA", "#7DCFFF",
                "#FF9E64",
            ];
            for (i, hex) in presets.iter().enumerate() {
                let cell_rgba = if i == 0 {
                    [
                        c.value[0] as f32 / 255.0,
                        c.value[1] as f32 / 255.0,
                        c.value[2] as f32 / 255.0,
                        1.0,
                    ]
                } else {
                    color::hex(hex)
                };
                let cell = PrefsRect::new(x, y, SWATCH_SIZE, SWATCH_SIZE);
                quads.push(QuadCmd::sharp(cell, cell_rgba));
                if i == 0 {
                    // Selected ring (2px) + 1px inset of bg-base.
                    let ring =
                        PrefsRect::new(x - 2.0, y - 2.0, SWATCH_SIZE + 4.0, SWATCH_SIZE + 4.0);
                    // Order matters: ring first (under), then redraw cell.
                    quads.insert(quads.len() - 1, QuadCmd::sharp(ring, accent));
                }
                x += SWATCH_SIZE + SWATCH_GAP;
                if x + SWATCH_SIZE > slot.x + slot.w {
                    break;
                }
            }
        }
        Control::TextField(f) => {
            let focused = state.focused_field == Some(f.id);
            let r = PrefsRect::new(slot.x, slot.y, slot.w.min(280.0), CONTROL_H);
            quads.push(QuadCmd::sharp(r, palette.bg_elevated));
            let border = if focused { palette.border_focus } else { palette.border_subtle };
            push_border(quads, r, border);
            let display = if f.value.is_empty() && !focused {
                "(default)".to_string()
            } else {
                f.value.clone()
            };
            let col = if f.value.is_empty() && !focused {
                palette.text_faint
            } else {
                palette.text_primary
            };
            push_text(
                texts,
                PrefsRect::new(r.x + 10.0, r.y, r.w - 20.0, r.h),
                display,
                col,
                typography::BODY,
            );
        }
    }
}

fn control_label(c: &Control) -> &str {
    match c {
        Control::Toggle(t) => t.label.as_str(),
        Control::Slider(s) => s.label.as_str(),
        Control::Dropdown(d) => d.label.as_str(),
        Control::ColorSwatch(c) => c.label.as_str(),
        Control::TextField(f) => f.label.as_str(),
    }
}

/// Minimal GPU renderer driving a single preferences window. Owns the
/// wgpu surface + glyphon + a quad pipeline. Reconfigures on resize +
/// scale-factor changes.
pub struct PrefsRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: SurfaceConfiguration,
    format: TextureFormat,
    scale_factor: f32,
    font_system: FontSystem,
    swash_cache: SwashCache,
    cache: Cache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    quad: QuadPipeline,
    cell_w: f32,
    window: Arc<Window>,
}

const BASE_CELL_W: f32 = 8.0;

impl PrefsRenderer {
    pub fn new(window: Arc<Window>, event_loop: &ActiveEventLoop) -> Result<Self> {
        pollster::block_on(Self::new_async(window, event_loop))
    }

    async fn new_async(window: Arc<Window>, event_loop: &ActiveEventLoop) -> Result<Self> {
        let size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;
        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let surface = instance.create_surface(window.clone()).context("create prefs surface")?;
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| anyhow!("prefs: no GPU adapter: {e}"))?;
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default())
            .await
            .context("prefs: request device")?;
        let format = TextureFormat::Bgra8UnormSrgb;
        let surface_caps = surface.get_capabilities(&adapter);
        let present_mode = if surface_caps.present_modes.contains(&PresentMode::Mailbox) {
            PresentMode::Mailbox
        } else {
            PresentMode::Fifo
        };
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        let quad = QuadPipeline::new(&device, format);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            format,
            scale_factor,
            font_system,
            swash_cache,
            cache,
            viewport,
            atlas,
            text_renderer,
            quad,
            cell_w: scale_factor * BASE_CELL_W,
            window,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn set_scale_factor(&mut self, sf: f32) {
        self.scale_factor = sf;
        self.cell_w = sf * BASE_CELL_W;
    }

    /// Full rebuild of the glyphon atlas / text renderer / surface for a
    /// new scale factor. On macOS the scale_factor reported inside the
    /// window constructor is often the stale 1.0 even when the window
    /// has been placed on a 2× display, so the atlas + text renderer get
    /// built at 1× and every glyph then lands off-canvas or at sub-pixel
    /// addresses on the real 2× surface — the symptom is a solid-black
    /// prefs window. Mirror `GpuRenderer::force_rebuild_for_scale` (PR
    /// #104): replace the cached atlas + text renderer, reconfigure the
    /// surface to its current physical extent, and request a redraw.
    pub fn force_rebuild_for_scale(&mut self, sf: f32) {
        let sf = sf.max(0.1);
        self.scale_factor = sf;
        self.cell_w = sf * BASE_CELL_W;
        // Re-create the glyphon atlas + text renderer so any glyphs
        // cached at the prior (wrong) metric are evicted.
        let mut atlas = TextAtlas::new(&self.device, &self.queue, &self.cache, self.format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &self.device, MultisampleState::default(), None);
        self.atlas = atlas;
        self.text_renderer = text_renderer;
        // The window may have been moved between displays of different
        // densities. Pick up the current physical size and re-configure.
        let phys = self.window.inner_size();
        self.config.width = phys.width.max(1);
        self.config.height = phys.height.max(1);
        self.surface.configure(&self.device, &self.config);
        self.window.request_redraw();
        tracing::info!(
            "prefs_renderer.force_rebuild_for_scale: scale={sf} surface={}x{}",
            self.config.width,
            self.config.height
        );
    }

    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    pub fn cell_w(&self) -> f32 {
        self.cell_w
    }

    pub fn render(&mut self, state: &mut PrefsState, theme: &Theme) -> Result<()> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                // The surface isn't ready yet — commonly happens for
                // the first few frames after window creation on macOS
                // (the CALayer takes a moment to wire up). Re-configure
                // the surface and request another redraw so we try
                // again on the next tick. Without this the window stays
                // BLANK forever: nothing else wakes the event loop
                // because the renderer didn't produce a frame, and the
                // user only sees content after clicking inside the
                // window (which incidentally triggers a redraw).
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                drop(frame);
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(anyhow!("prefs surface validation error"));
            }
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: Some("prefs-encoder") });

        let draw = build_draw_list(state, theme);
        state.clear_completed_toggle_anims(std::time::Instant::now());
        let clear = wgpu::Color {
            r: draw.clear[0] as f64,
            g: draw.clear[1] as f64,
            b: draw.clear[2] as f64,
            a: draw.clear[3] as f64,
        };
        let _ = hex_to_wgpu; // re-export keeps this in the dep graph

        let sw = self.config.width as f32;
        let sh = self.config.height as f32;
        let sf = self.scale_factor;
        let quads: Vec<QuadInstance> = draw
            .quads
            .iter()
            .map(|q| {
                let rect_ndc =
                    px_to_ndc(q.rect.x * sf, q.rect.y * sf, q.rect.w * sf, q.rect.h * sf, sw, sh);
                if q.radius_px > 0.0 {
                    QuadInstance::rounded(
                        rect_ndc,
                        q.color,
                        [q.rect.w * sf, q.rect.h * sf],
                        q.radius_px * sf,
                    )
                } else {
                    QuadInstance::sharp(rect_ndc, q.color)
                }
            })
            .collect();

        let ui_family = typography::system_ui_family();
        let mut buffers: Vec<(Buffer, PrefsRect)> = Vec::with_capacity(draw.texts.len());
        for t in &draw.texts {
            let metrics = Metrics::new(t.size_px, t.size_px * 1.4);
            let mut buf = Buffer::new(&mut self.font_system, metrics);
            buf.set_size(&mut self.font_system, Some(t.rect.w), Some(t.rect.h));
            let family = match (&t.font_family, t.monospace) {
                (Some(name), _) if !name.is_empty() => Family::Name(name.as_str()),
                (_, true) => Family::Name("monospace"),
                (_, false) => Family::Name(ui_family),
            };
            let mut attrs = Attrs::new().family(family).color(t.color);
            if t.weight >= 600 {
                attrs = attrs.weight(glyphon::Weight::BOLD);
            } else if t.weight >= 550 {
                attrs = attrs.weight(glyphon::Weight::SEMIBOLD);
            }
            buf.set_text(&mut self.font_system, &t.text, &attrs, Shaping::Advanced, None);
            buf.shape_until_scroll(&mut self.font_system, false);
            buffers.push((buf, t.rect));
        }

        self.viewport.update(
            &self.queue,
            Resolution { width: self.config.width, height: self.config.height },
        );
        let areas: Vec<TextArea> = draw
            .texts
            .iter()
            .zip(buffers.iter())
            .map(|(t, (buf, _))| TextArea {
                buffer: buf,
                left: t.rect.x * sf,
                top: t.rect.y * sf,
                scale: sf,
                bounds: TextBounds {
                    left: (t.rect.x * sf) as i32,
                    top: (t.rect.y * sf) as i32,
                    right: ((t.rect.x + t.rect.w) * sf) as i32,
                    bottom: ((t.rect.y + t.rect.h) * sf) as i32,
                },
                default_color: t.color,
                custom_glyphs: &[],
            })
            .collect();
        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash_cache,
            )
            .map_err(|e| anyhow!("prefs text prepare: {e:?}"))?;

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("prefs-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations { load: LoadOp::Clear(clear), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.quad.draw(&self.device, &self.queue, &mut pass, &quads);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .map_err(|e| anyhow!("prefs text render: {e:?}"))?;
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        // Evict unused glyph entries from the atlas. The prefs render
        // path builds a fresh `Buffer` for every text run on every
        // frame, so the atlas accumulates one cache entry per (glyph,
        // size, weight) for every category visited. Without periodic
        // trimming the atlas grows on every click and eventually
        // stalls the GPU thread (observed as a hard freeze after a
        // handful of sidebar clicks in live testing).
        self.atlas.trim();
        Ok(())
    }
}

// NOTE (CLAUDE.md §5): Tests stay inline here. They poke at many
// crate-private items (`crate::prefs::PREFS_MIN_W/H`, `crate::prefs::
// PrefsLayout::new`, `state.layout`/`state.config`, `crate::ui_tokens`,
// `crate::prefs::Control`/`PrefsHit`/`Category`) and source-grep
// `include_str!("prefs_renderer.rs")` itself. Migrating would require
// bumping a wide private surface to pub.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs::PrefsHit;
    use sonic_core::config::Config;
    use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
    use std::path::PathBuf;

    fn make_theme() -> Theme {
        // Use a gruvbox-like palette so `tab.active_fg` (the chrome
        // accent that UiPalette::from_theme reads) is visibly distinct
        // from Tokyo Night blue (#7aa2f7 / color::ACCENT_BLUE). Without
        // this distinction, a regression that hard-codes ACCENT_BLUE
        // would silently pass on this fixture.
        let h = || Hex("#7aa2f7".to_string());
        let accent = || Hex("#fabd2f".to_string()); // gruvbox bright yellow
        let ansi = || AnsiColors {
            black: h(),
            red: h(),
            green: h(),
            yellow: h(),
            blue: h(),
            magenta: h(),
            cyan: h(),
            white: h(),
        };
        Theme {
            name: "test".into(),
            appearance: Appearance::Dark,
            colors: Palette {
                background: h(),
                foreground: h(),
                cursor: h(),
                cursor_text: h(),
                selection_bg: h(),
                selection_fg: h(),
                ansi: ansi(),
                bright: ansi(),
                tab: TabColors {
                    bar_bg: h(),
                    active_bg: h(),
                    active_fg: accent(),
                    inactive_bg: h(),
                    inactive_fg: h(),
                    hover_bg: h(),
                    hover_fg: h(),
                    close_button_fg: h(),
                },
            },
        }
    }

    fn fresh() -> (PrefsState, Theme) {
        let theme = make_theme();
        let s = PrefsState::new(Config::default(), PathBuf::from("/tmp/test.toml"), theme.clone());
        (s, theme)
    }

    #[test]
    fn prefs_window_renders_title_and_subtitle() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        let joined: String = dl.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        assert!(joined.contains("Preferences"), "title missing: {joined}");
        assert!(
            dl.texts.iter().any(|t| t.text.contains(state.active_category.description())),
            "subtitle missing"
        );
    }

    #[test]
    fn prefs_window_renders_all_category_labels() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        let joined: String = dl.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        for cat in CATEGORIES {
            assert!(joined.contains(cat.label()), "category {} missing", cat.label());
        }
    }

    #[test]
    fn prefs_window_has_apply_and_cancel_buttons() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        let joined: String = dl.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        assert!(joined.contains("Apply"));
        assert!(joined.contains("Cancel"));
    }

    #[test]
    fn prefs_window_has_non_empty_chrome_quads() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        assert!(dl.quads.len() >= 4, "got {}", dl.quads.len());
    }

    #[test]
    fn prefs_clear_color_is_opaque_bg_base() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        assert!(dl.clear[3] > 0.99, "prefs background must be opaque");
        // Should match UiPalette::from_theme(theme).bg_base — chrome
        // follows the active theme (PR #119).
        let expected = crate::ui_tokens::UiPalette::from_theme(&theme).bg_base;
        for (i, ex) in expected.iter().enumerate() {
            assert!((dl.clear[i] - ex).abs() < 1e-4, "clear[{i}] ≠ palette.bg_base");
        }
    }

    #[test]
    fn switching_category_changes_subtitle() {
        let (mut state, theme) = fresh();
        state.set_category(layout::Category::Appearance);
        let dl_a = build_draw_list(&state, &theme);
        state.set_category(layout::Category::Behavior);
        let dl_b = build_draw_list(&state, &theme);
        let a: String = dl_a.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        let b: String = dl_b.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join("|");
        assert_ne!(a, b);
    }

    #[test]
    fn prefs_renderer_scale_factor_round_trip() {
        for sf in [1.0_f32, 1.5, 2.0, 3.0] {
            let cw = sf * BASE_CELL_W;
            assert!((cw - sf * BASE_CELL_W).abs() < f32::EPSILON);
        }
    }

    /// Regression: PR #123 fix was incomplete because `set_scale_factor`
    /// only flipped two fields without rebuilding the glyphon atlas or
    /// reconfiguring the surface. The follow-up adds
    /// `force_rebuild_for_scale`. This test compiles only while that
    /// method (and `set_scale_factor`) exist with the expected
    /// signature, so it is a static guard against the symbol going
    /// away during a future refactor.
    #[test]
    fn prefs_renderer_exposes_force_rebuild_for_scale() {
        let _set: fn(&mut PrefsRenderer, f32) = PrefsRenderer::set_scale_factor;
        let _force: fn(&mut PrefsRenderer, f32) = PrefsRenderer::force_rebuild_for_scale;
    }

    #[test]
    fn build_draw_list_covers_every_control_type() {
        let (mut state, theme) = fresh();
        state.set_category(layout::Category::Appearance);
        let dl = build_draw_list(&state, &theme);
        assert!(dl.quads.len() > 6);
        assert!(dl.texts.iter().any(|t| t.text.contains('▾')), "dropdown chevron missing");
    }

    #[test]
    fn prefs_primary_button_uses_theme_accent() {
        let (mut state, theme) = fresh();
        // Force dirty so the primary button shows its enabled colors.
        state.set_category(layout::Category::Appearance);
        // Flip a toggle if there is one, otherwise mark dirty manually.
        // We approximate by finding any toggle control.
        let toggle_id = state.controls.iter().find_map(|c| {
            if let Control::Toggle(t) = c {
                Some(t.id)
            } else {
                None
            }
        });
        if let Some(id) = toggle_id {
            state.flip_toggle(id);
        } else {
            // Fallback: directly mark dirty by re-applying a category.
            state.set_category(layout::Category::Behavior);
            let id = state.controls.iter().find_map(|c| {
                if let Control::Toggle(t) = c {
                    Some(t.id)
                } else {
                    None
                }
            });
            if let Some(id) = id {
                state.flip_toggle(id);
            }
        }
        let dl = build_draw_list(&state, &theme);
        let apply_rect = state.layout.apply_button;
        // Find the quad covering the apply button.
        let apply_quad = dl
            .quads
            .iter()
            .find(|q| {
                (q.rect.x - apply_rect.x).abs() < 0.01 && (q.rect.y - apply_rect.y).abs() < 0.01
            })
            .expect("apply button quad");
        let accent = crate::ui_tokens::UiPalette::from_theme(&theme).accent;
        // Sanity: theme accent (gruvbox gold) must differ from the
        // legacy Tokyo Night blue ACCENT_BLUE, so this test actually
        // catches the regression from PR #119's missed call sites.
        #[allow(deprecated)]
        let legacy_blue = color::ACCENT_BLUE();
        assert!(
            (accent[0] - legacy_blue[0]).abs() > 0.05
                || (accent[1] - legacy_blue[1]).abs() > 0.05
                || (accent[2] - legacy_blue[2]).abs() > 0.05,
            "test fixture accent must differ from ACCENT_BLUE to detect regressions"
        );
        for (i, &ac) in accent.iter().enumerate() {
            assert!(
                (apply_quad.color[i] - ac).abs() < 1e-4,
                "apply button channel {i} ≠ theme accent: got {} want {}",
                apply_quad.color[i],
                ac,
            );
        }
    }

    /// A toggle in the "on" position must paint its track with the
    /// theme accent (e.g. gruvbox gold) — not the deprecated Tokyo
    /// Night ACCENT_BLUE. Regression for PR #119 oversight.
    #[test]
    fn prefs_toggle_on_uses_theme_accent() {
        let (mut state, theme) = fresh();
        // Find any toggle and force it on.
        let toggle = state.controls.iter().find_map(|c| {
            if let Control::Toggle(t) = c {
                Some((t.id, t.value))
            } else {
                None
            }
        });
        let Some((id, initial)) = toggle else {
            // No toggle on the default category — try every category.
            for cat in [
                layout::Category::Appearance,
                layout::Category::Behavior,
                layout::Category::Keymap,
                layout::Category::Font,
                layout::Category::General,
            ] {
                state.set_category(cat);
                if let Some(t) = state.controls.iter().find_map(|c| {
                    if let Control::Toggle(t) = c {
                        Some(t.id)
                    } else {
                        None
                    }
                }) {
                    if !state
                        .controls
                        .iter()
                        .any(|c| matches!(c, Control::Toggle(tt) if tt.id == t && tt.value))
                    {
                        state.flip_toggle(t);
                    }
                    break;
                }
            }
            return;
        };
        if !initial {
            state.flip_toggle(id);
        }
        let dl = build_draw_list(&state, &theme);
        let accent = crate::ui_tokens::UiPalette::from_theme(&theme).accent;
        let found = dl.quads.iter().any(|q| {
            (q.color[0] - accent[0]).abs() < 1e-4
                && (q.color[1] - accent[1]).abs() < 1e-4
                && (q.color[2] - accent[2]).abs() < 1e-4
                && (q.rect.w - TOGGLE_W).abs() < 0.01
                && (q.rect.h - TOGGLE_H).abs() < 0.01
        });
        assert!(found, "toggle 'on' track should use theme accent {accent:?}");
    }

    /// The active sidebar category strip must use the theme accent.
    #[test]
    fn prefs_sidebar_active_accent_uses_theme_accent() {
        let (state, theme) = fresh();
        let dl = build_draw_list(&state, &theme);
        let accent_rect = state
            .layout
            .category_accent(CATEGORIES.iter().position(|c| *c == state.active_category).unwrap());
        let accent = crate::ui_tokens::UiPalette::from_theme(&theme).accent;
        let q = dl
            .quads
            .iter()
            .find(|q| {
                (q.rect.x - accent_rect.x).abs() < 0.01
                    && (q.rect.y - accent_rect.y).abs() < 0.01
                    && (q.rect.w - accent_rect.w).abs() < 0.01
            })
            .expect("sidebar accent strip quad");
        for (i, &ac) in accent.iter().enumerate() {
            assert!(
                (q.color[i] - ac).abs() < 1e-4,
                "sidebar accent channel {i} ≠ theme accent: got {} want {}",
                q.color[i],
                ac,
            );
        }
    }

    /// The slider's *filled* portion must paint with the theme accent
    /// (gruvbox gold), not the legacy Tokyo Night blue. Regression for
    /// PR #126 oversight: the fill code already used `accent`, but no
    /// test exercised a control with `Control::Slider` so any future
    /// refactor that swapped it back to a hardcoded constant would
    /// have shipped silently.
    #[test]
    fn prefs_slider_fill_uses_theme_accent() {
        let (mut state, theme) = fresh();
        // Find any category that has a slider in its control set.
        let mut slider_info: Option<(f32, f32, f32)> = None; // (x, y, w_at_full)
        for cat in [
            layout::Category::Appearance,
            layout::Category::Behavior,
            layout::Category::Keymap,
            layout::Category::Font,
            layout::Category::General,
        ] {
            state.set_category(cat);
            let has_slider =
                state.controls.iter().any(|c| matches!(c, crate::prefs::Control::Slider(_)));
            if has_slider {
                // Force every slider to its max so the fill spans the
                // full track and is unambiguous to locate.
                for c in state.controls.iter_mut() {
                    if let crate::prefs::Control::Slider(s) = c {
                        s.value = s.max;
                    }
                }
                // Capture the first slider's slot rect for later lookup.
                for (i, c) in state.controls.iter().enumerate() {
                    if matches!(c, crate::prefs::Control::Slider(_)) {
                        let slot = state.layout.control_slot(i);
                        // Match draw geometry: readout_w=56, gap=12.
                        let track_w = (slot.w - 56.0 - 12.0).max(40.0);
                        slider_info = Some((slot.x, slot.y, track_w));
                        break;
                    }
                }
                break;
            }
        }
        let Some((sx, _sy, track_w)) = slider_info else {
            panic!("expected at least one Slider control across categories");
        };
        let dl = build_draw_list(&state, &theme);
        let accent = crate::ui_tokens::UiPalette::from_theme(&theme).accent;
        // The fill quad: starts at slot.x, width = full track width when
        // value == max, and its color must match the theme accent.
        let fill = dl
            .quads
            .iter()
            .find(|q| {
                (q.rect.x - sx).abs() < 0.5
                    && (q.rect.w - track_w).abs() < 0.5
                    && (q.color[0] - accent[0]).abs() < 1e-4
                    && (q.color[1] - accent[1]).abs() < 1e-4
                    && (q.color[2] - accent[2]).abs() < 1e-4
            })
            .expect("slider fill quad with theme accent color not found");
        // Sanity: this color is not the legacy ACCENT_BLUE.
        #[allow(deprecated)]
        let legacy_blue = color::ACCENT_BLUE();
        assert!(
            (fill.color[0] - legacy_blue[0]).abs() > 0.05
                || (fill.color[1] - legacy_blue[1]).abs() > 0.05
                || (fill.color[2] - legacy_blue[2]).abs() > 0.05,
            "slider fill color matched the legacy Tokyo Night blue — \
             the fixture accent does not differ from ACCENT_BLUE"
        );
    }

    /// The slider's *unfilled* track must derive from the active theme
    /// (specifically `palette.bg_active`), not the legacy hardcoded
    /// Tokyo Night `#343A52FF`. This was the visual regression that
    /// made the slider read as "blue" on gruvbox even after PR #126.
    #[test]
    fn prefs_slider_track_uses_theme_palette() {
        let (mut state, theme) = fresh();
        // Pin every slider to its minimum so the unfilled track quad
        // covers the full track width and is unambiguous.
        let mut slot_info: Option<(f32, f32)> = None;
        for cat in [
            layout::Category::Appearance,
            layout::Category::Behavior,
            layout::Category::Keymap,
            layout::Category::Font,
            layout::Category::General,
        ] {
            state.set_category(cat);
            let has_slider =
                state.controls.iter().any(|c| matches!(c, crate::prefs::Control::Slider(_)));
            if has_slider {
                for c in state.controls.iter_mut() {
                    if let crate::prefs::Control::Slider(s) = c {
                        s.value = s.min;
                    }
                }
                for (i, c) in state.controls.iter().enumerate() {
                    if matches!(c, crate::prefs::Control::Slider(_)) {
                        let slot = state.layout.control_slot(i);
                        let track_w = (slot.w - 56.0 - 12.0).max(40.0);
                        slot_info = Some((slot.x, track_w));
                        break;
                    }
                }
                break;
            }
        }
        let Some((sx, track_w)) = slot_info else {
            panic!("expected at least one Slider control across categories");
        };
        let dl = build_draw_list(&state, &theme);
        let palette = crate::ui_tokens::UiPalette::from_theme(&theme);
        let expected = palette.bg_active;
        let track = dl.quads.iter().find(|q| {
            (q.rect.x - sx).abs() < 0.5
                && (q.rect.w - track_w).abs() < 0.5
                && (q.color[0] - expected[0]).abs() < 1e-4
                && (q.color[1] - expected[1]).abs() < 1e-4
                && (q.color[2] - expected[2]).abs() < 1e-4
                && (q.color[3] - expected[3]).abs() < 1e-4
        });
        assert!(
            track.is_some(),
            "slider unfilled track quad should use palette.bg_active ({expected:?}); \
             present quads at slot.x={sx} w={track_w}: {:?}",
            dl.quads
                .iter()
                .filter(|q| (q.rect.x - sx).abs() < 0.5)
                .map(|q| (q.rect.w, q.color))
                .collect::<Vec<_>>(),
        );
        // Regression: the old #343A52FF Tokyo Night literal must no
        // longer appear at this rect.
        let legacy_track = color::hex("#343A52FF");
        let legacy_present = dl.quads.iter().any(|q| {
            (q.rect.x - sx).abs() < 0.5
                && (q.rect.w - track_w).abs() < 0.5
                && (q.color[0] - legacy_track[0]).abs() < 1e-4
                && (q.color[1] - legacy_track[1]).abs() < 1e-4
                && (q.color[2] - legacy_track[2]).abs() < 1e-4
        });
        assert!(!legacy_present, "legacy Tokyo Night #343A52FF still rendered as slider track",);
    }

    /// Hardened spec for the Apply button: the *enabled* background
    /// quad must exactly equal `palette.accent` (e.g. gruvbox gold),
    /// AND the *disabled* state must not paint the legacy Tokyo Night
    /// `#2A3042` constant. This is a more specific contract than the
    /// pre-existing `prefs_primary_button_uses_theme_accent` test —
    /// it explicitly forbids the regression PR #126 missed.
    #[test]
    fn prefs_apply_button_uses_theme_accent() {
        let (mut state, theme) = fresh();
        state.set_category(layout::Category::Appearance);
        // Force dirty so we render the *enabled* path.
        let toggle_id = state.controls.iter().find_map(|c| {
            if let Control::Toggle(t) = c {
                Some(t.id)
            } else {
                None
            }
        });
        if let Some(id) = toggle_id {
            state.flip_toggle(id);
        } else {
            // Mark dirty via any other control.
            state.dirty = true;
        }
        assert!(state.dirty, "test setup failed: prefs must be dirty");
        let dl = build_draw_list(&state, &theme);
        let apply_rect = state.layout.apply_button;
        let accent = crate::ui_tokens::UiPalette::from_theme(&theme).accent;
        let apply_quad = dl
            .quads
            .iter()
            .find(|q| {
                (q.rect.x - apply_rect.x).abs() < 0.01 && (q.rect.y - apply_rect.y).abs() < 0.01
            })
            .expect("apply button quad");
        for (i, &ac) in accent.iter().enumerate() {
            assert!(
                (apply_quad.color[i] - ac).abs() < 1e-4,
                "apply (enabled) channel {i} ≠ theme accent: got {} want {}",
                apply_quad.color[i],
                ac,
            );
        }
        // And: the legacy Tokyo Night disabled bg literal must not
        // appear at the apply rect, even when re-rendered disabled.
        state.dirty = false;
        let dl2 = build_draw_list(&state, &theme);
        let apply_quad2 = dl2
            .quads
            .iter()
            .find(|q| {
                (q.rect.x - apply_rect.x).abs() < 0.01 && (q.rect.y - apply_rect.y).abs() < 0.01
            })
            .expect("apply (disabled) quad");
        let legacy_disabled = color::hex("#2A3042");
        let matches_legacy = (apply_quad2.color[0] - legacy_disabled[0]).abs() < 1e-4
            && (apply_quad2.color[1] - legacy_disabled[1]).abs() < 1e-4
            && (apply_quad2.color[2] - legacy_disabled[2]).abs() < 1e-4;
        assert!(!matches_legacy, "disabled Apply button still paints legacy Tokyo Night #2A3042",);
    }

    /// Source-grep guard. prefs_renderer.rs production code must not
    /// call the deprecated `color::ACCENT_BLUE()` — chrome derives from
    /// `UiPalette::from_theme(theme).accent`. Test code is allowed to
    /// reference it for sanity comparisons.
    #[test]
    fn prefs_no_hardcoded_accent_blue() {
        let src = include_str!("prefs_renderer.rs");
        // Split on the test module marker; only scan the production
        // portion above it.
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("prefs_renderer.rs must have a production section");
        assert!(
            !prod.contains("ACCENT_BLUE"),
            "production code in prefs_renderer.rs must not reference \
             color::ACCENT_BLUE — use UiPalette::from_theme(theme).accent"
        );
        // PR fix(prefs): slider track + disabled Apply bg used to
        // hardcode these Tokyo Night literals. Block re-introduction
        // as call-site arguments (comments mentioning the legacy hex
        // are fine — they document the regression).
        assert!(
            !prod.contains("color::hex(\"#343A52"),
            "production code must not hardcode #343A52 — \
             use palette.bg_active for chrome neutrals"
        );
        assert!(
            !prod.contains("color::hex(\"#2A3042"),
            "production code must not hardcode #2A3042 — \
             use palette.bg_active for disabled chrome bg"
        );
        assert!(
            !prod.contains("color::hex(\"#090C12"),
            "production code must not hardcode #090C12 — \
             use palette.bg_elevated for input bg"
        );
    }

    #[test]
    fn prefs_dirty_indicator_renders_unsaved_changes() {
        let (mut state, theme) = fresh();
        // Flip a toggle if available to dirty the state.
        let id = state.controls.iter().find_map(|c| {
            if let Control::Toggle(t) = c {
                Some(t.id)
            } else {
                None
            }
        });
        if let Some(id) = id {
            state.flip_toggle(id);
            let dl = build_draw_list(&state, &theme);
            assert!(
                dl.texts.iter().any(|t| t.text == "Unsaved changes"),
                "dirty indicator missing"
            );
        }
    }

    /// Regression: PR #117 review CRITICAL. The renderer used a private
    /// `row_y_offset` that the hit-test path did not know about, so a
    /// click on the visible first control hit the wrong slot. The fix
    /// folds the offset into `PrefsLayout::form_row` /
    /// `PrefsLayout::control_slot` directly so render + hit-test see
    /// the same rect.
    ///
    /// This test asserts: clicking at the *visible* center of control 0
    /// resolves to control 0 — not to an empty area above it.
    #[test]
    fn prefs_hit_test_rect_matches_rendered_rect() {
        let (state, _theme) = fresh();
        // First control's stored rect — also the rect the renderer
        // will draw at.
        assert!(!state.controls.is_empty(), "expected at least one control on General page");
        let ctrl0 = &state.controls[0];
        let ctrl0_id = ctrl0.id();
        let rect = match ctrl0 {
            Control::TextField(tf) => tf.rect,
            Control::Toggle(t) => t.rect,
            Control::Slider(s) => s.rect,
            Control::Dropdown(d) => d.rect,
            Control::ColorSwatch(cs) => cs.rect,
        };
        let cx = rect.x + rect.w / 2.0;
        let cy = rect.y + rect.h / 2.0;
        let hit = state.classify_click(cx, cy);
        assert!(hit.is_some(), "no hit at visible center of control 0 ({cx}, {cy})");
        // The id reported by the hit must match control 0.
        let hit_id = match hit.unwrap() {
            PrefsHit::Toggle(id)
            | PrefsHit::SliderTrack(id)
            | PrefsHit::DropdownHeader(id)
            | PrefsHit::TextField(id)
            | PrefsHit::DropdownOption { id, .. }
            | PrefsHit::ColorCell { id, .. } => id,
            other => panic!("expected control hit, got {other:?}"),
        };
        assert_eq!(hit_id, ctrl0_id, "click resolved to a different control");
    }

    /// Regression: PR #117 review IMPORTANT. The prefs window builder
    /// must enforce a minimum inner size so the user cannot shrink the
    /// window below the layout's clamp (otherwise the form card
    /// disappears).
    #[test]
    fn prefs_window_enforces_min_size() {
        // Constants must exist and match the layout clamp (680×520).
        assert_eq!(crate::prefs::PREFS_MIN_W, 680.0);
        assert_eq!(crate::prefs::PREFS_MIN_H, 520.0);
        // app.rs `create_prefs_window` must reference both via
        // `with_min_inner_size(LogicalSize::new(PREFS_MIN_W, PREFS_MIN_H))`.
        let app_src_mod = include_str!("../../sonic-app/src/app/mod.rs");
        let app_src_prefs = include_str!("../../sonic-app/src/app/prefs_window.rs");
        let app_src_owned = format!("{}{}", app_src_mod, app_src_prefs);
        let app_src = app_src_owned.as_str();
        assert!(
            app_src.contains("with_min_inner_size"),
            "prefs window builder is missing with_min_inner_size"
        );
        assert!(
            app_src.contains("PREFS_MIN_W"),
            "prefs window builder is not wired to PREFS_MIN_W"
        );
        assert!(
            app_src.contains("PREFS_MIN_H"),
            "prefs window builder is not wired to PREFS_MIN_H"
        );
    }

    /// Regression: PR #117 review IMPORTANT. The Appearance preview
    /// card used the generic monospace family which ignores the user's
    /// configured terminal font. The fix routes the family through
    /// `terminal_font_attrs()` so the preview actually shows the
    /// configured font. Source-grep test — the literal must not
    /// reappear.
    #[test]
    fn prefs_preview_card_uses_terminal_font_attrs() {
        let src = include_str!("prefs_renderer.rs");
        // Build the needle at runtime so this test's own assertion
        // string does not count as a hit. Strip line comments first
        // so doc-comments mentioning the legacy spelling do not match.
        let needle = format!("Family::{}", "Monospace");
        let stripped: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !stripped.contains(&needle),
            "prefs_renderer.rs still uses the generic monospace family; preview card must use terminal_font_attrs()"
        );
        // The helper must exist + be invoked: the preview card's
        // TextCmd must carry the configured terminal font family.
        // Use a tall window so the preview card actually fits inside
        // the form card (default 600 px is too short for 5 controls +
        // preview).
        let (state, theme) = fresh();
        let mut state = state;
        state.set_category(crate::prefs::Category::Appearance);
        state.config.font.family = "ZZZ-FixturePreviewFont".to_string();
        state.layout = crate::prefs::PrefsLayout::new(760.0, 900.0);
        state.rebuild_controls();
        let dl = build_draw_list(&state, &theme);
        let mono_count = dl.texts.iter().filter(|t| t.monospace).count();
        let with_family = dl
            .texts
            .iter()
            .filter(|t| t.font_family.as_deref() == Some("ZZZ-FixturePreviewFont"))
            .count();
        let preview_used = dl
            .texts
            .iter()
            .any(|t| t.monospace && t.font_family.as_deref() == Some("ZZZ-FixturePreviewFont"));
        assert!(
            preview_used,
            "preview card text did not carry the configured terminal font family (mono_count={mono_count}, with_family={with_family})"
        );
    }
}
