//! Layout + hit-testing for the browser-style tab bar drawn at the top of
//! the window. Pure logic, no GPU calls — easy to unit-test.
//!
//! Coordinate system: physical pixels, origin top-left, matching what the
//! renderer / winit cursor events use.

use crate::tabs::TabBar;
use std::sync::atomic::{AtomicU32, Ordering};

/// Pixel coordinate in tab-bar layout space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// Default height of the tab bar strip, in logical pixels. This is the
/// historical hard-coded value used when no font_size is plumbed in (tests,
/// pure-layout call sites). Renderer-driven layouts should prefer
/// [`tab_bar_height`] so the bar height scales with the user's configured
/// font size — matching WezTerm fancy-mode's `window_frame.font_size × 2`
/// rhythm.
pub const TAB_BAR_HEIGHT: f32 = 40.0;

/// Compute the tab bar height for a given terminal font size.
///
/// Formula: `font_size * 2.0 + 12.0` clamped to a `36.0` floor so the bar
/// always has 8px of vertical breathing room above and below the title
/// text (text height is ~`font_size * 0.85 * 1.2`). At `font_size = 14`
/// this returns `40.0`, matching the WezTerm fancy-mode roomier default.
/// At `font_size = 15` it returns `42.0`.
pub fn tab_bar_height(font_size: f32) -> f32 {
    (font_size * 2.0 + 12.0).max(36.0)
}

/// Default maximum width of a single tab (a long-title tab is clamped to
/// this when the bar has room). User config can override the active value at
/// runtime via [`set_max_tab_width`]; this constant is the built-in fallback
/// and the value an unconfigured install renders with.
pub const TAB_MAX_WIDTH: f32 = 240.0;

/// Process-global active max tab width, in logical pixels, stored as the bit
/// pattern of an `f32`. Seeded to [`TAB_MAX_WIDTH`] and updated on config load
/// and hot-reload. Held globally (rather than threaded through every
/// `compute*` call site) so the rendered bar and the hit-tested bar always
/// read the same value with no signature churn.
static MAX_TAB_WIDTH_BITS: AtomicU32 = AtomicU32::new(TAB_MAX_WIDTH.to_bits());

/// Override the active maximum tab width. Called from config apply on startup
/// and hot-reload. Non-finite or non-positive values are ignored so a bad
/// config never collapses the tab bar.
// Ordering: MAX_TAB_WIDTH_BITS stores with Relaxed; the width stands alone and
// publishes no companion state, so no reader needs an acquire pairing.
pub fn set_max_tab_width(width: f32) {
    if width.is_finite() && width > 0.0 {
        MAX_TAB_WIDTH_BITS.store(width.to_bits(), Ordering::Relaxed);
    }
}

/// Read the active maximum tab width, in logical pixels.
// Ordering: MAX_TAB_WIDTH_BITS loads with Relaxed; a layout pass only needs the
// latest width, with no dependent state to acquire alongside it.
#[must_use]
pub fn max_tab_width() -> f32 {
    f32::from_bits(MAX_TAB_WIDTH_BITS.load(Ordering::Relaxed))
}

/// Inset between tabs and from the right edge of the bar.
pub const TAB_GAP: f32 = 4.0;

/// Padding on the left edge of the bar before the first tab.
/// 0 = first tab flush against the window edge (per user preference;
/// matches WezTerm fancy-mode when `tab_bar_at_bottom + window_padding = 0`).
pub const BAR_LEFT_PAD: f32 = 0.0;

/// Internal horizontal padding inside each tab, between the edge of the tab
/// rect and the centered title block.
pub const TAB_INNER_PAD: f32 = 10.0;

/// Vertical inset between the bar's top edge and the tab background rect
/// (and equivalently the bottom edge). The tab rect is `bar_h - 2 *
/// TAB_VERT_INSET` tall — leaving 4px of bar chrome above and below the
/// pill so the active tab's elevated BG visibly floats on the bar.
pub const TAB_VERT_INSET: f32 = 2.0;

/// Corner radius of the tab background pill, in logical pixels.
pub const TAB_CORNER_RADIUS: f32 = 8.0;

/// Height of the 2px top accent bar drawn on the active tab.
pub const ACTIVE_TOP_ACCENT_H: f32 = 2.0;

/// Horizontal inset (each side) of the active-tab top accent bar relative
/// to the tab background rect — so the accent is nearly full-width.
pub const ACTIVE_TOP_ACCENT_INSET: f32 = 2.0;

/// Reserved drop zone at the right edge of the tab bar, in logical pixels.
/// This region is subtracted from the per-tab allocation width so there is
/// always an empty slice between the last tab's right edge and the bar's
/// right edge that the user can drop a dragged tab into to request
/// "append at the end" (insertion slot `tabs.len()`). The bar background
/// still extends the full window width — no extra drawing is required;
/// the reserved zone simply shows the existing tab-bar background.
pub const TAB_END_DROP_ZONE_PX: f32 = 96.0;

/// Rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// Whether `(px, py)` lies inside this rect, counting the left and top
    /// edges as inside and the right and bottom edges as outside.
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// Hover state for a whole tab widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabHover {
    None,
    Body,
    Close,
}

/// Action produced by a whole-tab hit-test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAction {
    Activate(usize),
    /// Legacy variant kept for API compatibility; tab close buttons are no
    /// longer emitted or hit-tested.
    Close(usize),
}

/// Layout and interaction model for a single tab. The tab owns one
/// background rect for all interaction.
#[derive(Debug, Clone, PartialEq)]
pub struct TabWidget {
    pub idx: usize,
    pub bg_rect: Rect,
    pub close_x_rect: Rect,
    /// Title rect (inside the tab, centered horizontally by the renderer).
    pub title_rect: Rect,
    pub title: String,
    pub custom_color: Option<String>,
    pub active: bool,
    pub hover: TabHover,
    /// Back-compat public field alias for the tab index. Prefer `idx`.
    pub index: usize,
    /// Back-compat public field alias for the tab background rect. Prefer `bg_rect`.
    pub bg: Rect,
    /// Back-compat public field alias for the retired close sub-rect.
    pub close: Rect,
}

impl TabWidget {
    /// Hit-test this tab as one whole widget. Any point inside `bg_rect`
    /// activates the tab; close buttons are no longer part of the tab chrome.
    pub fn hit(&self, p: Point) -> Option<TabAction> {
        if !self.bg_rect.contains(p.x, p.y) {
            // When: the point misses bg_rect, so this tab claims no action and
            // the caller keeps testing the remaining tabs.
            return None;
        }
        Some(TabAction::Activate(self.idx))
    }

    /// Hover state for this tab under the given cursor position, reporting
    /// `TabHover::None` when the cursor is absent or outside the tab.
    #[must_use]
    pub fn hover_at(&self, p: Option<Point>) -> TabHover {
        let Some(p) = p else {
            // When: p is absent because the cursor left the window entirely,
            // so no part of this tab is hovered.
            return TabHover::None;
        };
        match self.hit(p) {
            Some(TabAction::Close(_)) => TabHover::Close,
            Some(TabAction::Activate(_)) => TabHover::Body,
            None => TabHover::None,
        }
    }
}

/// Back-compat alias for older render/test call sites. New code should treat
/// each value as one whole [`TabWidget`].
pub type TabRect = TabWidget;

/// What part of the tab bar was clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabHit {
    Activate(usize),
    /// Open the attached window's complete live-tab selector.
    Overflow,
    /// Legacy variant kept for API compatibility; tab close buttons are no
    /// longer emitted or hit-tested.
    Close(usize),
}

impl From<TabAction> for TabHit {
    fn from(action: TabAction) -> Self {
        match action {
            TabAction::Activate(idx) => Self::Activate(idx),
            TabAction::Close(idx) => Self::Close(idx),
        }
    }
}

/// Minimum outside distance from either vertical tab-bar edge, in raster pixels.
pub const TEAR_OUT_THRESHOLD_PX: f32 = 40.0;

/// Result of evaluating a mouse-drag against the tab bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TearOut {
    /// Index of the tab that was originally pressed.
    pub tab_index: usize,
    /// Pixel position where the drag was released / current cursor.
    pub drop_position: (f32, f32),
}

/// Detect an inclusive vertical departure from the live bar; callers gate drag-start and button ownership.
pub fn detect_tear_out(
    press_tab_index: usize,
    current_pos: (f32, f32),
    bar: &TabBarLayout,
) -> Option<TearOut> {
    let (_, cy) = current_pos;
    let (top, bottom) = bar.bar_y_range();
    let outside_distance = (top - cy).max(cy - bottom).max(0.0);
    (outside_distance >= TEAR_OUT_THRESHOLD_PX)
        .then_some(TearOut { tab_index: press_tab_index, drop_position: current_pos })
}

/// Computed bar background, visible tab segment with absolute indices, and optional overflow control.
#[derive(Debug, Clone)]
pub struct TabBarLayout {
    pub bar: Rect,
    pub tabs: Vec<TabWidget>,
    pub active: Option<usize>,
    /// Total live tabs, including those outside the visible segment.
    pub total_tabs: usize,
    /// Fixed control that opens the complete live-tab selector when tabs overflow.
    pub overflow: Option<Rect>,
    /// When `false`, the tab bar is hidden and [`Self::hit`] /
    /// [`Self::point_over_bar`] always return as if the cursor missed
    /// the bar — so a hidden bar never silently captures clicks.
    pub visible: bool,
}

impl TabBarLayout {
    /// Compute the layout for the bar at the top of a window `window_width`
    /// pixels wide, using the historical [`TAB_BAR_HEIGHT`] constant.
    pub fn compute(bar: &TabBar, window_width: f32) -> Self {
        Self::compute_with_height(bar, window_width, TAB_BAR_HEIGHT)
    }

    /// Width of the insertion gap in logical px.
    /// Tabs at or after the insertion slot shift right by this amount
    /// while a drag is in progress, previewing the drop position.
    pub const INSERTION_GAP_PX: f32 = 8.0;

    /// Like [`Self::compute_with_height`] but also opens an
    /// insertion gap of [`Self::INSERTION_GAP_PX`] pixels at
    /// `insertion_slot` so the user can see where a dragged tab will
    /// land. `insertion_slot` is in `[0, n]` (insertion-slot
    /// semantics); `None` falls through to the regular layout.
    ///
    /// The gap is applied by shifting every tab at index ≥ slot right
    /// by [`Self::INSERTION_GAP_PX`] logical pixels — the close-box
    /// and title sub-rects shift with their parent so hit-testing
    /// stays internally consistent.
    pub fn compute_with_insertion_slot(
        bar: &TabBar,
        window_width: f32,
        bar_height: f32,
        insertion_slot: Option<usize>,
    ) -> Self {
        let mut layout = Self::compute_with_height(bar, window_width, bar_height);
        let Some(slot) = insertion_slot else {
            // When: insertion_slot is absent because no drag is in progress, so
            // the layout is returned with no preview gap opened.
            return layout;
        };
        let dx = Self::INSERTION_GAP_PX;
        for t in layout.tabs.iter_mut() {
            if t.idx >= slot {
                t.bg_rect.x += dx;
                t.close_x_rect.x += dx;
                t.title_rect.x += dx;
                t.bg.x += dx;
                t.close.x += dx;
            }
            if let Some(overflow) = layout.overflow {
                t.bg_rect.x = t.bg_rect.x.min(overflow.x);
                t.bg_rect.w = t.bg_rect.w.min((overflow.x - t.bg_rect.x).max(0.0));
                t.title_rect.x = t.title_rect.x.min(t.bg_rect.x + t.bg_rect.w);
                t.title_rect.w =
                    t.title_rect.w.min((t.bg_rect.x + t.bg_rect.w - t.title_rect.x).max(0.0));
                t.close_x_rect.x = t.bg_rect.x + t.bg_rect.w;
                t.bg = t.bg_rect;
                t.close = t.close_x_rect;
            }
        }
        layout
    }

    /// Like [`Self::compute`] but with an explicit bar height — used by the
    /// renderer so the painted bar and the hit-tested bar always agree on
    /// height when the user's font size differs from the default.
    pub fn compute_with_height(bar: &TabBar, window_width: f32, bar_height: f32) -> Self {
        Self::compute_at_y(bar, window_width, bar_height, 0.0)
    }

    /// Compute the tab bar layout anchored at an explicit `bar_y` position.
    /// The bottom-bar renderer passes `window_h - bar_h` so the strip, active
    /// indicator, and title hit zones all share bottom coordinates.
    ///
    /// All sizes (`bar_height`, `bar_y`, `window_width`) live in the
    /// **same coordinate system** the renderer uses (raster px).
    /// The logical-px constants (`TAB_GAP`,
    /// `BAR_LEFT_PAD`, `TAB_INNER_PAD`) are auto-scaled to that system
    /// by treating `bar_height` as the reference: the default unscaled
    /// `tab_bar_height(14.0)` is `40.0`, so `bar_height / 40.0` gives
    /// the geometric scale factor regardless of DPR or font size.
    pub fn compute_at_y(bar: &TabBar, window_width: f32, bar_height: f32, bar_y: f32) -> Self {
        let bar_h = bar_height.max(1.0);
        let bar_y = bar_y.max(0.0);
        let bar_rect = Rect { x: 0.0, y: bar_y, w: window_width.max(0.0), h: bar_h };

        let n = bar.len();
        if n == 0 {
            // When: n is zero, the bar holds no tabs, so only the background is present.
            return Self {
                bar: bar_rect,
                tabs: Vec::new(),
                active: None,
                total_tabs: 0,
                overflow: None,
                visible: true,
            };
        }

        // Per-bar-height geometric scale factor. At the default
        // `tab_bar_height(14.0) = 40.0` this is 1.0 and the constants
        // act as their unscaled logical-px values. On 2x Retina with
        // the same font size, bar_h is 80 and the scale is 2.0 so the
        // chrome (gaps and paddings) grows in lockstep with
        // the bar itself. Same story for non-default font sizes.
        let scale = bar_h / 40.0;
        let mut tab_gap = TAB_GAP * scale;
        let bar_left_pad = (BAR_LEFT_PAD * scale).min(bar_rect.w * 0.25);
        let inner_pad = TAB_INNER_PAD * scale;
        let end_drop = TAB_END_DROP_ZONE_PX * scale;
        let minimum = bar_h * 2.0 + inner_pad * 2.0;
        let maximum = (max_tab_width() * scale).max(minimum);
        let tabs_region = (bar_rect.w - bar_left_pad * 2.0 - end_drop).max(0.0);
        let total_gaps = tab_gap * (n as f32 - 1.0).max(0.0);
        let raw = ((tabs_region - total_gaps) / n as f32).max(0.0);
        let crowded = n > 1 && raw < minimum;
        let (first, count, per_tab, overflow) = if crowded {
            let control_w = bar_h.min(bar_rect.w * 0.5);
            let control = Rect { x: bar_rect.w - control_w, y: bar_y, w: control_w, h: bar_h };
            tab_gap = tab_gap.min(control.x * 0.1);
            let available = (control.x - bar_left_pad - tab_gap).max(0.0);
            let count =
                (((available + tab_gap) / (minimum + tab_gap)).floor() as usize).max(1).min(n - 1);
            let first = bar.active_index().saturating_sub(count / 2).min(n - count);
            let width = ((available - tab_gap * count.saturating_sub(1) as f32) / count as f32)
                .max(0.0)
                .min(maximum);
            (first, count, width, Some(control))
        } else {
            // When: `crowded` is false, keep every tab visible and retain the ordinary end-drop region.
            let available = if n == 1 && raw < minimum { bar_rect.w } else { raw };
            (0, n, available.min(maximum), None)
        };
        let mut tabs: Vec<TabWidget> = Vec::with_capacity(count);

        let bg_y = bar_y + TAB_VERT_INSET * scale;
        let bg_h = (bar_h - 2.0 * TAB_VERT_INSET * scale).max(1.0);
        let mut x = bar_left_pad;
        for index in first..first + count {
            let bg = Rect { x, y: bg_y, w: per_tab, h: bg_h };
            let close = Rect { x: bg.x + bg.w, y: bg.y + bg.h * 0.5, w: 0.0, h: 0.0 };
            let title_pad = inner_pad.min(bg.w * 0.5);
            let title_x = bg.x + title_pad;
            let title_right = bg.x + bg.w - title_pad;
            let title = Rect { x: title_x, y: bg.y, w: (title_right - title_x).max(0.0), h: bg.h };
            let tab = &bar.tabs()[index];
            tabs.push(TabWidget {
                idx: index,
                bg_rect: bg,
                close_x_rect: close,
                title_rect: title,
                title: tab.title.clone(),
                custom_color: tab.custom_color.clone(),
                active: index == bar.active_index(),
                hover: TabHover::None,
                index,
                bg,
                close,
            });
            x += per_tab + tab_gap;
        }

        Self {
            bar: bar_rect,
            tabs,
            active: Some(bar.active_index()),
            total_tabs: n,
            overflow,
            visible: true,
        }
    }

    /// Rect (in the same coordinate space as `self.tabs`) at which the
    /// renderer should paint the active-tab top-accent bar. Returns
    /// `None` when there is no active tab in the layout (empty bar) or
    /// when the active index points past the laid-out tabs (defensive —
    /// stale state must never paint an accent floating in the empty
    /// right-edge area of the bar).
    ///
    /// The rect is anchored to the active tab's own `bg.x`/`bg.y` — it
    /// MUST NOT be derived from `active_idx * tab_w`, because the bar
    /// applies a left padding `BAR_LEFT_PAD` and per-tab spacing
    /// `TAB_GAP` that the naive multiplication ignores.
    #[must_use]
    pub fn active_accent_rect(&self) -> Option<Rect> {
        let t = self.active_widget()?;
        // The active indicator must be clipped to the active tab's post-layout
        // width. Do not derive it from the whole strip or shrink/grow it
        // independently; wide two-tab Windows layouts exposed that drift as an
        // orange line overshooting into empty chrome.
        let scale = (t.bg_rect.h / (TAB_BAR_HEIGHT - 2.0 * TAB_VERT_INSET)).max(0.1);
        let inset = ACTIVE_TOP_ACCENT_INSET * scale;
        Some(Rect {
            x: t.bg_rect.x + inset,
            y: t.bg_rect.y + 1.0 * scale,
            w: (t.bg_rect.w - inset * 2.0).max(0.0),
            h: ACTIVE_TOP_ACCENT_H * scale,
        })
    }

    /// Full background rect for the active tab indicator/widget. This is the
    /// canonical rect for active-tab ownership; paint variants such as the
    /// 2px top accent derive from it instead of recomputing from tab index.
    #[must_use]
    pub fn active_indicator_rect(&self) -> Option<Rect> {
        Some(self.active_widget()?.bg_rect)
    }

    fn active_widget(&self) -> Option<&TabWidget> {
        let idx = self.active?;
        self.tabwidgets().iter().find(|tab| tab.idx == idx)
    }

    /// Builder-style helper to mark the layout as hidden. A hidden
    /// layout reports no hits and no over-bar containment, so callers
    /// can pass the layout through unchanged and still get correct
    /// click routing when the tab bar is toggled off.
    #[must_use]
    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Shift every rectangle in the layout down by `dy` logical/physical
    /// pixels. Used to push the tab bar below the macOS native titlebar
    /// when `with_fullsize_content_view(true)` extends our content under
    /// the traffic lights — otherwise both hit-testing and the painted
    /// chrome would overlap the OS titlebar.
    ///
    /// `dy` of 0 is a no-op (non-macOS / non-integrated styles).
    /// Negative values are clamped to 0 so callers can pass raw deltas
    /// without worrying about sign.
    #[must_use]
    pub fn with_top_offset(mut self, dy: f32) -> Self {
        let dy = dy.max(0.0);
        if dy == 0.0 {
            // When: dy clamps to zero on non-macOS or non-integrated titlebar
            // styles, so every rect already sits at its final position.
            return self;
        }
        self.bar.y += dy;
        if let Some(overflow) = &mut self.overflow {
            overflow.y += dy;
        }
        for t in &mut self.tabs {
            t.bg_rect.y += dy;
            t.close_x_rect.y += dy;
            t.title_rect.y += dy;
            t.bg.y += dy;
            t.close.y += dy;
        }
        self
    }

    /// Map a pixel position to a tab-bar action. Returns `None` when the
    /// click is outside the bar entirely (caller should treat it as a
    /// terminal-area click in that case).
    ///
    /// The activation hit-zone is the FULL bar height for each tab's
    /// horizontal range `[bg.x, bg.x + bg.w)` — not just the inset
    /// background rect. This matches user expectation that the entire
    /// chrome strip belonging to a tab is clickable; previously the
    /// 2px sliver above/below the visible `bg` rect would fall through
    /// to the "click between tabs → activate currently-active tab"
    /// default, making the user feel they had to aim at the title text.
    pub fn hit(&self, px: f32, py: f32) -> Option<TabHit> {
        if !self.visible {
            // When: visible is false, the bar is toggled off and must not
            // capture a click that belongs to the terminal area.
            return None;
        }
        if !self.bar.contains(px, py) {
            // When: the point falls outside bar, so the click belongs to the
            // terminal area rather than to any tab.
            return None;
        }
        if self.overflow.is_some_and(|control| control.contains(px, py)) {
            // When: `overflow` owns the pointer, open the selector rather than choosing a hidden tab index.
            return Some(TabHit::Overflow);
        }
        self.tabwidgets()
            .iter()
            .find(|tab| px >= tab.bg_rect.x && px < tab.bg_rect.x + tab.bg_rect.w)
            .map(|tab| TabHit::Activate(tab.idx))
    }

    /// Whole-tab widgets for one-pass interaction/rendering decisions.
    #[must_use]
    pub fn tabwidgets(&self) -> &[TabWidget] {
        &self.tabs
    }

    /// True if `(px, py)` falls anywhere inside the bar background,
    /// regardless of which specific tab/control it hits. Used by the
    /// cross-window drag-merge flow to decide "is the cursor currently
    /// over THIS window's bar?".
    pub fn point_over_bar(&self, px: f32, py: f32) -> bool {
        self.visible && self.bar.contains(px, py)
    }

    /// Resolve an absolute tab slot: visible gaps insert locally; a drop on overflow appends globally.
    pub fn drop_slot(&self, px: f32, _py: f32) -> usize {
        if self.overflow.is_some_and(|control| px >= control.x) {
            // When: `overflow` owns a tab drop, preserve the global append affordance.
            return self.total_tabs;
        }
        for tab in &self.tabs {
            let midpoint = tab.bg_rect.x + tab.bg_rect.w * 0.5;
            if px < midpoint {
                // When: `px` precedes this visible midpoint, insert before its absolute tab index.
                return tab.idx;
            }
        }
        self.tabs.last().map_or(0, |tab| (tab.idx + 1).min(self.total_tabs))
    }

    /// Locate a visible absolute insertion gap or overflow's global-append marker; hidden gaps return None.
    pub fn insertion_x(&self, slot: usize) -> Option<f32> {
        if !self.visible || self.tabs.is_empty() {
            // When: `visible` is false or `tabs` is empty, no displayed insertion gap exists.
            return None;
        }
        if slot == self.total_tabs {
            // When: `slot` names the global end, prefer the overflow affordance over a hidden trailing gap.
            if let Some(control) = self.overflow {
                // When: the global slot belongs to `overflow`, distinguish it from the visible trailing gap.
                return Some((control.x + control.w - 1.0).max(control.x));
            }
        }
        let last = self.tabs.last()?;
        if slot == last.idx + 1 {
            // When: `slot` follows the last visible tab, draw exactly the gap the drop resolves to.
            return Some(last.bg_rect.x + last.bg_rect.w + TAB_GAP * 0.5);
        }
        let position = self.tabs.iter().position(|tab| tab.idx == slot)?;
        let next = &self.tabs[position];
        if position == 0 {
            // When: `position` is the leading visible widget, its absolute slot has no visible predecessor.
            return Some(next.bg_rect.x - TAB_GAP * 0.5);
        }
        let previous = &self.tabs[position - 1];
        Some((previous.bg_rect.x + previous.bg_rect.w + next.bg_rect.x) * 0.5)
    }

    /// Vertical span `(top, bottom)` of the bar's background rect in
    /// the same coordinate space as [`Self::insertion_x`]. Used by
    /// callers that want to size the drop-line accent flush with the
    /// bar chrome.
    pub fn bar_y_range(&self) -> (f32, f32) {
        (self.bar.y, self.bar.y + self.bar.h)
    }
}

/// Pure helper computing the top inset reserved above the grid for both
/// the OS titlebar band (when an integrated titlebar pushes the content
/// view under the native chrome) and the tab bar. Returns the titlebar
/// inset alone when the tab bar is hidden, so the grid recovers the row
/// the bar used to take. Exposed so tests can validate visibility wiring
/// without needing a live GPU context.
pub fn tab_bar_top_inset(visible: bool, padding: f32) -> f32 {
    tab_bar_top_inset_with_titlebar(visible, padding, 0.0)
}

/// Same as [`tab_bar_top_inset`] but adds a reserved titlebar band on top.
/// `titlebar_inset` is the height in logical pixels the OS reserves at the
/// top of the content view (e.g. macOS traffic-lights strip when
/// `with_fullsize_content_view(true)`). Pass 0 when the OS already keeps
/// our content below its chrome.
pub fn tab_bar_top_inset_with_titlebar(visible: bool, padding: f32, titlebar_inset: f32) -> f32 {
    let bar = if visible {
        TAB_BAR_HEIGHT + padding
    } else {
        // When: visible is false, the bar reserves nothing, so the grid
        // recovers the row it used to occupy and only padding remains.
        padding
    };
    titlebar_inset + bar
}

#[cfg(test)]
#[path = "tabbar_view_tests.rs"]
mod tabbar_view_tests;
