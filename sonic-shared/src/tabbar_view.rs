//! Layout + hit-testing for the browser-style tab bar drawn at the top of
//! the window. Pure logic, no GPU calls — easy to unit-test.
//!
//! Coordinate system: physical pixels, origin top-left, matching what the
//! renderer / winit cursor events use.

use crate::tabs::TabBar;

/// Height of the tab bar strip, in physical pixels. Wezterm's fancy mode
/// uses a slightly taller bar than the chrome-style 32px default — gives
/// the per-tab Nerd Font icons room to breathe at typical font sizes.
pub const TAB_BAR_HEIGHT: f32 = 34.0;

/// Maximum width of a single tab (a long-title tab is clamped to this).
pub const TAB_MAX_WIDTH: f32 = 220.0;

/// Minimum width of a single tab — below this we just clip the title.
pub const TAB_MIN_WIDTH: f32 = 80.0;

/// Width of the `+` new-tab button drawn after the last tab.
pub const NEW_TAB_BUTTON_WIDTH: f32 = 32.0;

/// Size of the close `×` square inside each tab.
pub const CLOSE_BUTTON_SIZE: f32 = 16.0;

/// Inset between tabs and from the right edge before the `+` button.
pub const TAB_GAP: f32 = 2.0;

/// Padding on the left edge of the bar before the first tab.
pub const BAR_LEFT_PAD: f32 = 4.0;

/// Internal horizontal padding inside each tab, between the edge of the tab
/// rect and the start of the title / the close button.
pub const TAB_INNER_PAD: f32 = 8.0;

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

/// Layout of a single tab inside the bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabRect {
    pub index: usize,
    pub bg: Rect,
    pub close: Rect,
    /// Title rect (inside the tab, to the left of the close button).
    pub title: Rect,
}

/// What part of the tab bar was clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabHit {
    Activate(usize),
    Close(usize),
    NewTab,
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
    pub tabs: Vec<TabRect>,
    pub new_tab: Rect,
    pub active: Option<usize>,
    /// When `false`, the tab bar is hidden and [`Self::hit`] /
    /// [`Self::point_over_bar`] always return as if the cursor missed
    /// the bar — so a hidden bar never silently captures clicks.
    pub visible: bool,
}

impl TabBarLayout {
    /// Compute the layout for the bar at the top of a window `window_width`
    /// pixels wide.
    pub fn compute(bar: &TabBar, window_width: f32) -> Self {
        let bar_rect = Rect { x: 0.0, y: 0.0, w: window_width.max(0.0), h: TAB_BAR_HEIGHT };

        let new_tab = Rect {
            x: (window_width - NEW_TAB_BUTTON_WIDTH).max(0.0),
            y: 0.0,
            w: NEW_TAB_BUTTON_WIDTH.min(window_width),
            h: TAB_BAR_HEIGHT,
        };

        let n = bar.len();
        let mut tabs: Vec<TabRect> = Vec::with_capacity(n);
        if n == 0 {
            return Self { bar: bar_rect, tabs, new_tab, active: None, visible: true };
        }

        // Region available for tabs is from BAR_LEFT_PAD to the left edge of
        // the new-tab button, minus a gap before the +.
        let tabs_region = (window_width - BAR_LEFT_PAD - NEW_TAB_BUTTON_WIDTH - TAB_GAP).max(0.0);
        let total_gaps = TAB_GAP * (n as f32 - 1.0).max(0.0);
        let raw = ((tabs_region - total_gaps) / n as f32).max(1.0);
        let per_tab = raw.min(TAB_MAX_WIDTH);
        let _ = TAB_MIN_WIDTH; // advisory; tabs shrink below this when many

        let mut x = BAR_LEFT_PAD;
        for index in 0..n {
            let bg = Rect { x, y: 2.0, w: per_tab, h: TAB_BAR_HEIGHT - 4.0 };
            let close = Rect {
                x: bg.x + bg.w - TAB_INNER_PAD - CLOSE_BUTTON_SIZE,
                y: bg.y + (bg.h - CLOSE_BUTTON_SIZE) / 2.0,
                w: CLOSE_BUTTON_SIZE,
                h: CLOSE_BUTTON_SIZE,
            };
            let title_x = bg.x + TAB_INNER_PAD;
            let title_right = close.x - TAB_INNER_PAD / 2.0;
            let title = Rect { x: title_x, y: bg.y, w: (title_right - title_x).max(0.0), h: bg.h };
            tabs.push(TabRect { index, bg, close, title });
            x += per_tab + TAB_GAP;
        }

        Self { bar: bar_rect, tabs, new_tab, active: Some(bar.active_index()), visible: true }
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
            t.bg.y += dy;
            t.close.y += dy;
            t.title.y += dy;
        }
        self
    }

    /// Map a pixel position to a tab-bar action. Returns `None` when the
    /// click is outside the bar entirely (caller should treat it as a
    /// terminal-area click in that case).
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
        for t in &self.tabs {
            if t.close.contains(px, py) {
                return Some(TabHit::Close(t.index));
            }
            if t.bg.contains(px, py) {
                return Some(TabHit::Activate(t.index));
            }
        }
        // Clicked the bar background between/around tabs — swallow the
        // click so it doesn't fall through to the terminal grid.
        Some(TabHit::Activate(self.active.unwrap_or(0)))
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
            let midx = t.bg.x + t.bg.w * 0.5;
            if px < midx {
                return t.index;
            }
        }
        self.tabs.len()
    }
}
