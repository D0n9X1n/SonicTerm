//! Layout + hit-testing for the browser-style tab bar drawn at the top of
//! the window. Pure logic, no GPU calls — easy to unit-test.
//!
//! Coordinate system: physical pixels, origin top-left, matching what the
//! renderer / winit cursor events use.

use crate::tabs::TabBar;

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

/// Maximum width of a single tab (a long-title tab is clamped to this).
pub const TAB_MAX_WIDTH: f32 = 400.0;

/// Preferred minimum width of a single tab. Acts as a soft floor: when the
/// equal-share allocation per tab is ≥ this value, each tab is held at or
/// above `TAB_MIN_WIDTH` (so the common 2–4 tab case at 1000 px wide keeps
/// shell titles like `Administrator: cmd.exe` / `pwsh` readable). When the
/// tab count grows large enough that holding the floor would overflow the
/// `+` button gutter, the floor yields and tabs shrink to share the
/// available space evenly — preserving the invariant that the strip never
/// extends past the new-tab button.
pub const TAB_MIN_WIDTH: f32 = 200.0;

/// Width of the `+` new-tab button drawn after the last tab.
/// Browser-style 28×28 hit/visual target with radius 8 (issue #112 Round 3).
pub const NEW_TAB_BUTTON_WIDTH: f32 = 28.0;

/// Height of the `+` new-tab button (square; centered vertically in bar).
pub const NEW_TAB_BUTTON_HEIGHT: f32 = 28.0;

/// Size of the close `×` square inside each tab.
pub const CLOSE_BUTTON_SIZE: f32 = 14.0;

/// Inset between tabs and from the right edge before the `+` button.
pub const TAB_GAP: f32 = 6.0;

/// Padding on the left edge of the bar before the first tab.
pub const BAR_LEFT_PAD: f32 = 12.0;

/// Internal horizontal padding inside each tab, between the edge of the tab
/// rect and the start of the title / the close button. The modern browser-
/// style chrome wants more breathing room around the title block.
pub const TAB_INNER_PAD: f32 = 10.0;

/// Vertical inset between the bar's top edge and the tab background rect
/// (and equivalently the bottom edge). The tab rect is `bar_h - 2 *
/// TAB_VERT_INSET` tall — leaving 4px of bar chrome above and below the
/// pill so the active tab's elevated BG visibly floats on the bar.
pub const TAB_VERT_INSET: f32 = 4.0;

/// Corner radius of the tab background pill, in logical pixels.
pub const TAB_CORNER_RADIUS: f32 = 8.0;

/// Height of the 2px top accent bar drawn on the active tab.
pub const ACTIVE_TOP_ACCENT_H: f32 = 2.0;

/// Horizontal inset (each side) of the active-tab top accent bar relative
/// to the tab background rect — so the accent is `tab_w - 2 * inset` wide.
pub const ACTIVE_TOP_ACCENT_INSET: f32 = 6.0;

/// Rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
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
    Close(usize),
}

/// Layout and interaction model for a single tab. The tab owns one
/// background rect for all interaction; the close `×` is only a sub-rect
/// inside that widget.
#[derive(Debug, Clone, PartialEq)]
pub struct TabWidget {
    pub idx: usize,
    pub bg_rect: Rect,
    pub close_x_rect: Rect,
    /// Title rect (inside the tab, to the left of the close button).
    pub title_rect: Rect,
    pub title: String,
    pub active: bool,
    pub hover: TabHover,
    /// Back-compat public field alias for the tab index. Prefer `idx`.
    pub index: usize,
    /// Back-compat public field alias for the tab background rect. Prefer `bg_rect`.
    pub bg: Rect,
    /// Back-compat public field alias for the close sub-rect. Prefer `close_x_rect`.
    pub close: Rect,
}

impl TabWidget {
    /// Hit-test this tab as one whole widget. Any point inside `bg_rect`
    /// activates the tab except points inside the visual close sub-rect,
    /// which close the tab.
    pub fn hit(&self, p: Point) -> Option<TabAction> {
        if !self.bg_rect.contains(p.x, p.y) {
            return None;
        }
        if self.close_x_rect.contains(p.x, p.y) {
            Some(TabAction::Close(self.idx))
        } else {
            Some(TabAction::Activate(self.idx))
        }
    }

    #[must_use]
    pub fn hover_at(&self, p: Option<Point>) -> TabHover {
        let Some(p) = p else { return TabHover::None };
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
    Close(usize),
    NewTab,
}

impl From<TabAction> for TabHit {
    fn from(action: TabAction) -> Self {
        match action {
            TabAction::Activate(idx) => Self::Activate(idx),
            TabAction::Close(idx) => Self::Close(idx),
        }
    }
}

/// Minimum vertical drag distance below the bottom of the tab bar to
/// promote a tab press into a tear-out gesture. Matches Firefox/Chrome.
pub const TEAR_OUT_THRESHOLD_PX: f32 = 40.0;

/// Result of evaluating a mouse-drag against the tab bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TearOut {
    /// Index of the tab that was originally pressed.
    pub tab_index: usize,
    /// Pixel position where the drag was released / current cursor.
    pub drop_position: (f32, f32),
}

/// Pure helper: was the press-then-move gesture a tear-out?
///
/// A tear-out fires when the cursor leaves the tab bar vertically by
/// at least [`TEAR_OUT_THRESHOLD_PX`] pixels while the mouse button is
/// still held. The caller owns the "is the mouse still down?" check —
/// this function is mode-free so it can be unit-tested without winit.
pub fn detect_tear_out(press_tab_index: usize, current_pos: (f32, f32)) -> Option<TearOut> {
    let (cx, cy) = current_pos;
    // The tab bar lives at y in [0, TAB_BAR_HEIGHT). A tear-out fires
    // once the cursor moves at least THRESHOLD pixels below the bottom
    // of the bar, regardless of horizontal position (so the user can
    // drag straight down OR off to the side).
    if cy >= TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX {
        Some(TearOut { tab_index: press_tab_index, drop_position: (cx, cy) })
    } else {
        None
    }
}

/// Computed layout for the entire bar — bar background, every tab, and the
/// `+` button.
#[derive(Debug, Clone)]
pub struct TabBarLayout {
    pub bar: Rect,
    pub tabs: Vec<TabWidget>,
    pub new_tab: Rect,
    pub active: Option<usize>,
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

    /// Like [`Self::compute`] but with an explicit bar height — used by the
    /// renderer so the painted bar and the hit-tested bar always agree on
    /// height when the user's font size differs from the default.
    pub fn compute_with_height(bar: &TabBar, window_width: f32, bar_height: f32) -> Self {
        let bar_h = bar_height.max(1.0);
        let bar_rect = Rect { x: 0.0, y: 0.0, w: window_width.max(0.0), h: bar_h };

        // Browser-style `+` button: a 28x28 square hit target floated at
        // the right edge of the bar, vertically centered. The hit region
        // returned in the layout stays 28x28 so cursor-shape and click
        // routing land exactly on the visual.
        //
        // On Windows, the integrated titlebar paints three caption buttons
        // (min/max/close, 46x32 logical px each = 138 px total) anchored
        // to the right edge of the bar. The `+` button must sit to the
        // LEFT of the caption-button strip, otherwise it overlaps them
        // and either click target swallows the other (issue #189).
        let nt_w = NEW_TAB_BUTTON_WIDTH.min(window_width.max(0.0));
        let nt_h = NEW_TAB_BUTTON_HEIGHT.min(bar_h);
        let right_reserved = caption_strip_reserved_width();
        let nt_x = (window_width - nt_w - BAR_LEFT_PAD - right_reserved).max(0.0);
        let nt_y = ((bar_h - nt_h) * 0.5).max(0.0);
        let new_tab = Rect { x: nt_x, y: nt_y, w: nt_w, h: nt_h };

        let n = bar.len();
        let mut tabs: Vec<TabWidget> = Vec::with_capacity(n);
        if n == 0 {
            return Self { bar: bar_rect, tabs, new_tab, active: None, visible: true };
        }

        // Region available for tabs is from BAR_LEFT_PAD to the left edge of
        // the new-tab button, minus a gap before the +. On Windows we also
        // subtract the caption-button strip width reserved on the right
        // (see new_tab.x computation above).
        let tabs_region = (window_width
            - BAR_LEFT_PAD
            - NEW_TAB_BUTTON_WIDTH
            - BAR_LEFT_PAD
            - TAB_GAP
            - caption_strip_reserved_width())
        .max(0.0);
        let total_gaps = TAB_GAP * (n as f32 - 1.0).max(0.0);
        let raw = ((tabs_region - total_gaps) / n as f32).max(1.0);
        // TAB_MIN_WIDTH is a *soft* floor (a preferred minimum, not a hard
        // clamp): tabs shrink to share the available space when the equal-
        // share allocation falls below it, so the strip never overflows the
        // `+` button gutter. When the strip has surplus, use the available
        // equal share up to TAB_MAX_WIDTH so maximized windows can show long
        // titles instead of staying pinned to the old narrow cap.
        let per_tab = raw.min(TAB_MAX_WIDTH);

        let bg_y = TAB_VERT_INSET;
        let bg_h = (bar_h - 2.0 * TAB_VERT_INSET).max(1.0);
        let mut x = BAR_LEFT_PAD;
        for index in 0..n {
            let bg = Rect { x, y: bg_y, w: per_tab, h: bg_h };
            let close = Rect {
                x: bg.x + bg.w - TAB_INNER_PAD - CLOSE_BUTTON_SIZE,
                y: bg.y + (bg.h - CLOSE_BUTTON_SIZE) / 2.0,
                w: CLOSE_BUTTON_SIZE,
                h: CLOSE_BUTTON_SIZE,
            };
            let title_x = bg.x + TAB_INNER_PAD;
            let title_right = close.x - TAB_INNER_PAD / 2.0;
            let title = Rect { x: title_x, y: bg.y, w: (title_right - title_x).max(0.0), h: bg.h };
            let tab = &bar.tabs()[index];
            tabs.push(TabWidget {
                idx: index,
                bg_rect: bg,
                close_x_rect: close,
                title_rect: title,
                title: tab.title.clone(),
                active: index == bar.active_index(),
                hover: TabHover::None,
                index,
                bg,
                close,
            });
            x += per_tab + TAB_GAP;
        }

        Self { bar: bar_rect, tabs, new_tab, active: Some(bar.active_index()), visible: true }
    }

    /// Rect (in the same coordinate space as `self.tabs`) at which the
    /// renderer should paint the active-tab top-accent bar. Returns
    /// `None` when there is no active tab in the layout (empty bar) or
    /// when the active index points past the laid-out tabs (defensive —
    /// stale state must never paint an accent floating in the new-tab
    /// gutter, which was the user-reported bug in issue #171).
    ///
    /// The rect is anchored to the active tab's own `bg.x`/`bg.y` — it
    /// MUST NOT be derived from `active_idx * tab_w`, because the bar
    /// applies a left padding `BAR_LEFT_PAD` and per-tab spacing
    /// `TAB_GAP` that the naive multiplication ignores.
    #[must_use]
    pub fn active_accent_rect(&self) -> Option<Rect> {
        let t = self.active_widget()?;
        // Issue #257: the active indicator must be clipped to the active
        // tab's post-layout width. Do not derive it from the whole strip or
        // shrink/grow it independently; wide two-tab Windows layouts exposed
        // that drift as an orange line overshooting into empty chrome.
        Some(Rect {
            x: t.bg_rect.x,
            y: t.bg_rect.y,
            w: t.bg_rect.w.max(0.0),
            h: ACTIVE_TOP_ACCENT_H,
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
        self.tabwidgets().get(idx)
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
            return self;
        }
        self.bar.y += dy;
        self.new_tab.y += dy;
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
            return None;
        }
        if !self.bar.contains(px, py) {
            return None;
        }
        if self.new_tab.contains(px, py) {
            return Some(TabHit::NewTab);
        }
        self.tabwidgets().iter().find_map(|t| t.hit(Point { x: px, y: py })).map(Into::into)
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

    /// Compute the insertion slot for a tab dropped at `(px, py)`. The
    /// slot is an index in `[0, n]` where `n == self.tabs.len()`:
    ///   * left of tab `i`'s horizontal midpoint → slot `i`
    ///   * right of the last tab's midpoint (or in the empty bar) → `n`
    ///
    /// The vertical coordinate is ignored beyond a coarse "is roughly
    /// over the bar" check — for cross-window merge we already gated on
    /// `point_over_bar` before getting here, and the bar is thin enough
    /// that any vertical position inside it should map to the obvious
    /// horizontal slot.
    pub fn drop_slot(&self, px: f32, _py: f32) -> usize {
        if self.tabs.is_empty() {
            return 0;
        }
        for t in &self.tabs {
            let midx = t.bg_rect.x + t.bg_rect.w * 0.5;
            if px < midx {
                return t.idx;
            }
        }
        self.tabs.len()
    }

    /// X coordinate in logical/physical pixels at which the drop-line
    /// indicator should be drawn for an insertion slot in `[0, n]`.
    ///
    ///   * slot `0`            → just left of the first tab
    ///   * slot `i` (mid)      → in the gap before tab `i`
    ///   * slot `n` (== len)   → just right of the last tab
    ///
    /// Returns `None` when the bar is empty or hidden — there's no
    /// meaningful insertion gap to render.
    pub fn insertion_x(&self, slot: usize) -> Option<f32> {
        if !self.visible || self.tabs.is_empty() {
            return None;
        }
        let n = self.tabs.len();
        let slot = slot.min(n);
        if slot == 0 {
            // Just inside the bar's left padding.
            return Some(self.tabs[0].bg_rect.x - TAB_GAP * 0.5);
        }
        if slot == n {
            // After the last tab — between its right edge and the
            // `+` button (if any).
            let last = &self.tabs[n - 1];
            return Some(last.bg_rect.x + last.bg_rect.w + TAB_GAP * 0.5);
        }
        // Between tab `slot - 1` and tab `slot`: halfway between
        // their adjacent edges, which is what the rendered gap is.
        let prev = &self.tabs[slot - 1];
        let next = &self.tabs[slot];
        let right = prev.bg_rect.x + prev.bg_rect.w;
        let left = next.bg_rect.x;
        Some((right + left) * 0.5)
    }

    /// Vertical span `(top, bottom)` of the bar's background rect in
    /// the same coordinate space as [`Self::insertion_x`]. Used by
    /// callers that want to size the drop-line accent flush with the
    /// bar chrome.
    pub fn bar_y_range(&self) -> (f32, f32) {
        (self.bar.y, self.bar.y + self.bar.h)
    }
}

/// Width (logical px) of a single caption button (min/max/close).
/// Matches the Win11 standard 46x32 caption button hit/visual target.
pub const CAPTION_BUTTON_WIDTH: f32 = 46.0;

/// Height of the caption-button strip (logical px). Matches
/// [`crate::app::WINDOWS_INTEGRATED_TITLEBAR_INSET`].
pub const CAPTION_BUTTON_HEIGHT: f32 = 32.0;

/// Logical-pixel width reserved on the right edge of the tab bar for the
/// integrated Win11 caption-button strip (min + max + close = 3 * 46 px).
/// Returns 0.0 on non-Windows platforms, where the tab bar extends to the
/// right edge of the window. Used by [`TabBarLayout::compute_with_height`]
/// to place the `+` new-tab button to the LEFT of the caption buttons so
/// they never overlap (issue #189).
#[must_use]
pub fn caption_strip_reserved_width() -> f32 {
    #[cfg(target_os = "windows")]
    {
        CAPTION_BUTTON_WIDTH * 3.0
    }
    #[cfg(not(target_os = "windows"))]
    {
        0.0
    }
}

/// Returns the [min, max, close] caption-button rects, in **physical
/// pixels**, anchored to the right edge of a window of the given width.
///
/// `width` is the window's physical pixel width; `dpi` is the scale
/// factor (1.0 on standard displays, 1.5/2.0 on HiDPI). The buttons are
/// laid out right-to-left as close ▶ max ▶ min, matching the Win11
/// titlebar order.
#[must_use]
pub fn caption_button_rects(width: u32, dpi: f32) -> [Rect; 3] {
    let bw = CAPTION_BUTTON_WIDTH * dpi;
    let bh = CAPTION_BUTTON_HEIGHT * dpi;
    let right = width as f32;
    let close = Rect { x: right - bw, y: 0.0, w: bw, h: bh };
    let max = Rect { x: right - bw * 2.0, y: 0.0, w: bw, h: bh };
    let min = Rect { x: right - bw * 3.0, y: 0.0, w: bw, h: bh };
    [min, max, close]
}

// Unit tests live in `tests/src_tabbar_view.rs`.
