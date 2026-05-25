//! Layout + hit-testing for the browser-style tab bar drawn at the top of
//! the window. Pure logic, no GPU calls — easy to unit-test.
//!
//! Coordinate system: physical pixels, origin top-left, matching what the
//! renderer / winit cursor events use.

use crate::tabs::TabBar;

/// Height of the tab bar strip, in physical pixels.
pub const TAB_BAR_HEIGHT: f32 = 32.0;

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

/// Computed layout for the entire bar — bar background, every tab, and the
/// `+` button.
#[derive(Debug, Clone)]
pub struct TabBarLayout {
    pub bar: Rect,
    pub tabs: Vec<TabRect>,
    pub new_tab: Rect,
    pub active: Option<usize>,
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
            return Self { bar: bar_rect, tabs, new_tab, active: None };
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

        Self { bar: bar_rect, tabs, new_tab, active: Some(bar.active_index()) }
    }

    /// Map a pixel position to a tab-bar action. Returns `None` when the
    /// click is outside the bar entirely (caller should treat it as a
    /// terminal-area click in that case).
    pub fn hit(&self, px: f32, py: f32) -> Option<TabHit> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::Tab;

    fn bar_with(n: usize) -> TabBar {
        let mut b = TabBar::new();
        for i in 0..n {
            b.push(Tab::new(format!("tab{i}")));
        }
        b
    }

    #[test]
    fn empty_bar_still_has_new_tab_button() {
        let bar = TabBar::new();
        let layout = TabBarLayout::compute(&bar, 800.0);
        assert!(layout.tabs.is_empty());
        assert_eq!(layout.hit(790.0, 10.0), Some(TabHit::NewTab));
    }

    #[test]
    fn click_inside_tab_returns_activate() {
        let bar = bar_with(3);
        let layout = TabBarLayout::compute(&bar, 800.0);
        let t0 = layout.tabs[0];
        let cx = t0.bg.x + t0.bg.w / 2.0 - CLOSE_BUTTON_SIZE;
        let cy = t0.bg.y + t0.bg.h / 2.0;
        assert_eq!(layout.hit(cx, cy), Some(TabHit::Activate(0)));
    }

    #[test]
    fn click_on_close_button_returns_close() {
        let bar = bar_with(2);
        let layout = TabBarLayout::compute(&bar, 800.0);
        let t1 = layout.tabs[1];
        let cx = t1.close.x + t1.close.w / 2.0;
        let cy = t1.close.y + t1.close.h / 2.0;
        assert_eq!(layout.hit(cx, cy), Some(TabHit::Close(1)));
    }

    #[test]
    fn click_on_plus_button_returns_new_tab() {
        let bar = bar_with(2);
        let layout = TabBarLayout::compute(&bar, 800.0);
        let cx = layout.new_tab.x + 4.0;
        let cy = layout.new_tab.y + 4.0;
        assert_eq!(layout.hit(cx, cy), Some(TabHit::NewTab));
    }

    #[test]
    fn click_below_bar_returns_none() {
        let bar = bar_with(2);
        let layout = TabBarLayout::compute(&bar, 800.0);
        assert!(layout.hit(100.0, TAB_BAR_HEIGHT + 4.0).is_none());
    }

    #[test]
    fn tab_widths_shrink_when_many_tabs() {
        let bar = bar_with(20);
        let layout = TabBarLayout::compute(&bar, 800.0);
        let last = layout.tabs.last().unwrap();
        assert!(last.bg.x + last.bg.w <= layout.new_tab.x + 1.0);
    }

    #[test]
    fn tab_widths_clamp_at_max() {
        let bar = bar_with(1);
        let layout = TabBarLayout::compute(&bar, 4000.0);
        assert!((layout.tabs[0].bg.w - TAB_MAX_WIDTH).abs() < 0.5);
    }

    #[test]
    fn rect_contains_is_half_open() {
        let r = Rect { x: 10.0, y: 10.0, w: 20.0, h: 20.0 };
        assert!(r.contains(10.0, 10.0));
        assert!(r.contains(29.999, 29.999));
        assert!(!r.contains(30.0, 20.0));
        assert!(!r.contains(20.0, 30.0));
    }

    #[test]
    fn bar_background_click_between_tabs_swallows_to_active() {
        let mut bar = bar_with(3);
        bar.activate(1);
        let layout = TabBarLayout::compute(&bar, 800.0);
        let gap_x = layout.tabs[0].bg.x + layout.tabs[0].bg.w + TAB_GAP / 2.0;
        let hit = layout.hit(gap_x, 1.0);
        assert_eq!(hit, Some(TabHit::Activate(1)));
    }
}
