//! Tab geometry responsibilities of the renderer, preserving its single owner.

use super::*;

pub(super) fn tab_bar_hash(tabs: &TabBar, now: Instant) -> u64 {
    // The limits the strip is laid out with, so a reload repaints it in the frame that draws it.
    let (min_px, max_px) =
        sonicterm_render_model::boundary::ui::tabbar_view::tab_width_limits_of(tabs);
    tab_bar_hash_with_limits(tabs, now, min_px, max_px)
}

/// [`tab_bar_hash`] with explicit tab width limits, in logical pixels.
pub(super) fn tab_bar_hash_with_limits(
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
        // A marked inactive tab draws `●`, so a mark appearing or clearing repaints the bar.
        (index != active_index && tab.marks.is_marked()).hash(&mut hash);
        command_status_hash(&tab.command, now, index == active_index).hash(&mut hash);
    }
    hash.finish()
}

/// Resolve the title text colour for a single tab, honouring hover.
///
/// Default tabs use the active foreground when active or hovered. Custom
/// colours stay full-strength while active, hovered, or panel-focused and
/// otherwise recede to the standard unfocused alpha.
pub(super) fn tab_title_color(
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

pub(super) const PRIVILEGE_BADGE_SIZE_PX: f32 = 18.0;

pub(super) const PRIVILEGE_BADGE_GAP_PX: f32 = 6.0;

#[cfg(test)]
pub(super) const PRIVILEGE_BADGE_QUAD_COUNT: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TabTitleBlockPlacement {
    pub(super) badge_rect: Option<TabTitleRect>,
    pub(super) text_x: f32,
    pub(super) text_clip: TabTitleRect,
}

pub(super) fn scaled_privilege_badge_metrics(scale: f32) -> (f32, f32) {
    let scale = scale.max(0.1);
    (PRIVILEGE_BADGE_SIZE_PX * scale, PRIVILEGE_BADGE_GAP_PX * scale)
}

/// Raster-pixel width reserved for the privilege marker and its gap before the
/// title text, or zero when no marker is drawn.
pub(super) fn privilege_marker_reserve_px(privileged: bool, scale: f32) -> f32 {
    if privileged {
        let (badge, gap) = scaled_privilege_badge_metrics(scale);
        badge + gap
    } else {
        // When: `privileged` is false, no marker is drawn and the title keeps the whole rect.
        0.0
    }
}

/// Drawn width of one tab's content in raster pixels: the privilege-marker
/// reserve plus the shaped advance of the badge and title in the tab font.
/// Returns `None` when the text cannot be shaped; with no tab font only the
/// reserve counts, because no text is drawn.
pub(super) fn tab_content_width_px(
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
pub(super) fn tab_font_key(
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

pub(super) fn tab_title_block_placement(
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

pub(super) fn privilege_lock_color(danger: [f32; 4]) -> [f32; 4] {
    if danger[0] * 0.2126 + danger[1] * 0.7152 + danger[2] * 0.0722 > 0.179 {
        [0.0, 0.0, 0.0, danger[3]]
    } else {
        // When: danger luminance is at most 0.179, white lock geometry has stronger contrast.
        [danger[3], danger[3], danger[3], danger[3]]
    }
}

#[cfg(test)]
pub(super) fn linear_contrast_ratio(a: [f32; 4], b: [f32; 4]) -> f32 {
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

pub(super) fn privilege_lock_rects(badge: TabTitleRect) -> [TabTitleRect; 4] {
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

pub(super) fn emit_privilege_badge_quads(
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

#[cfg(test)]
#[path = "core_tab_geometry_tests.rs"]
mod core_tab_geometry_tests;
