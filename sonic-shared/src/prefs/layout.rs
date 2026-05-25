//! Layout for the preferences window.
//!
//! Window is divided into:
//! - A fixed-width left sidebar listing categories (vertical menu).
//! - A right-hand form panel inset with padding, leaving space at the
//!   bottom for the Apply / Cancel buttons.
//!
//! All numbers are in *logical* pixels — the renderer multiplies by the
//! window's scale factor when emitting quads.

use super::controls::Rect;
use super::{PREFS_WIN_H, PREFS_WIN_W};

/// Category list shown in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    General,
    Appearance,
    Font,
    Keymap,
    Behavior,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::General => "General",
            Category::Appearance => "Appearance",
            Category::Font => "Font",
            Category::Keymap => "Keymap",
            Category::Behavior => "Behavior",
        }
    }
}

/// All categories in display order.
pub const CATEGORIES: &[Category] = &[
    Category::General,
    Category::Appearance,
    Category::Font,
    Category::Keymap,
    Category::Behavior,
];

/// Tunables — kept as `const` so layout is deterministic and testable.
pub const SIDEBAR_W: f32 = 160.0;
pub const SIDEBAR_ROW_H: f32 = 32.0;
pub const SIDEBAR_PAD: f32 = 8.0;
pub const FORM_PAD: f32 = 20.0;
pub const FOOTER_H: f32 = 56.0;
pub const BUTTON_W: f32 = 96.0;
pub const BUTTON_H: f32 = 28.0;
pub const ROW_H: f32 = 36.0;
pub const LABEL_W: f32 = 140.0;

/// Pre-computed rectangles for the window's static chrome. The form's
/// individual control rects are produced separately from this anchor.
#[derive(Debug, Clone, Copy)]
pub struct PrefsLayout {
    pub width: f32,
    pub height: f32,
    pub sidebar: Rect,
    pub form: Rect,
    pub footer: Rect,
    pub apply_button: Rect,
    pub cancel_button: Rect,
}

impl PrefsLayout {
    /// Build a layout for the given window size (clamped to a sane minimum).
    pub fn new(width: f32, height: f32) -> Self {
        let width = width.max(SIDEBAR_W + 240.0);
        let height = height.max(FOOTER_H + 240.0);
        let sidebar = Rect::new(0.0, 0.0, SIDEBAR_W, height);
        let form_top = 0.0;
        let form_h = height - FOOTER_H;
        let form = Rect::new(SIDEBAR_W, form_top, width - SIDEBAR_W, form_h);
        let footer = Rect::new(SIDEBAR_W, height - FOOTER_H, width - SIDEBAR_W, FOOTER_H);
        let buttons_y = footer.y + (FOOTER_H - BUTTON_H) / 2.0;
        let apply_x = footer.x + footer.w - BUTTON_W - FORM_PAD;
        let cancel_x = apply_x - BUTTON_W - 12.0;
        let apply_button = Rect::new(apply_x, buttons_y, BUTTON_W, BUTTON_H);
        let cancel_button = Rect::new(cancel_x, buttons_y, BUTTON_W, BUTTON_H);
        Self { width, height, sidebar, form, footer, apply_button, cancel_button }
    }

    /// Default-size layout (matches the values in [`PREFS_WIN_W`] / `_H`).
    pub fn default_size() -> Self {
        Self::new(PREFS_WIN_W, PREFS_WIN_H)
    }

    /// Row rect for a category in the sidebar (0-based).
    pub fn category_row(&self, index: usize) -> Rect {
        Rect::new(
            self.sidebar.x + SIDEBAR_PAD,
            self.sidebar.y + SIDEBAR_PAD + index as f32 * SIDEBAR_ROW_H,
            self.sidebar.w - SIDEBAR_PAD * 2.0,
            SIDEBAR_ROW_H,
        )
    }

    /// Hit-test the sidebar: returns the category clicked, if any.
    pub fn hit_category(&self, x: f32, y: f32) -> Option<Category> {
        for (i, c) in CATEGORIES.iter().enumerate() {
            if self.category_row(i).contains(x, y) {
                return Some(*c);
            }
        }
        None
    }

    /// Row rect for the `n`th control inside the form (0-based).
    pub fn form_row(&self, n: usize) -> Rect {
        Rect::new(
            self.form.x + FORM_PAD,
            self.form.y + FORM_PAD + n as f32 * ROW_H,
            self.form.w - FORM_PAD * 2.0,
            ROW_H - 8.0,
        )
    }

    /// The portion of a form row right of the label, where the actual
    /// control widget is rendered.
    pub fn control_slot(&self, n: usize) -> Rect {
        let row = self.form_row(n);
        Rect::new(row.x + LABEL_W, row.y, row.w - LABEL_W, row.h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_size_matches_constants() {
        let l = PrefsLayout::default_size();
        assert_eq!(l.width, PREFS_WIN_W);
        assert_eq!(l.height, PREFS_WIN_H);
    }

    #[test]
    fn layout_clamps_to_minimum() {
        let l = PrefsLayout::new(50.0, 50.0);
        assert!(l.width > SIDEBAR_W);
        assert!(l.height > FOOTER_H);
    }

    #[test]
    fn sidebar_is_left_strip() {
        let l = PrefsLayout::default_size();
        assert_eq!(l.sidebar.x, 0.0);
        assert_eq!(l.sidebar.w, SIDEBAR_W);
        assert_eq!(l.sidebar.h, PREFS_WIN_H);
    }

    #[test]
    fn form_starts_right_of_sidebar() {
        let l = PrefsLayout::default_size();
        assert_eq!(l.form.x, SIDEBAR_W);
        assert_eq!(l.form.w, PREFS_WIN_W - SIDEBAR_W);
        // Form ends where footer starts.
        assert!((l.form.y + l.form.h - l.footer.y).abs() < 1e-5);
    }

    #[test]
    fn footer_sits_at_bottom() {
        let l = PrefsLayout::default_size();
        assert!((l.footer.y + l.footer.h - PREFS_WIN_H).abs() < 1e-5);
        assert_eq!(l.footer.h, FOOTER_H);
    }

    #[test]
    fn apply_is_rightmost_button() {
        let l = PrefsLayout::default_size();
        assert!(l.apply_button.x > l.cancel_button.x);
        // Both within the footer horizontally.
        assert!(l.apply_button.x + l.apply_button.w <= l.footer.x + l.footer.w);
        assert!(l.cancel_button.x >= l.footer.x);
    }

    #[test]
    fn category_rows_stack_vertically_inside_sidebar() {
        let l = PrefsLayout::default_size();
        for (i, _) in CATEGORIES.iter().enumerate() {
            let r = l.category_row(i);
            assert!(r.x >= l.sidebar.x);
            assert!(r.x + r.w <= l.sidebar.x + l.sidebar.w);
            assert!(r.y >= l.sidebar.y);
        }
        // Rows do not overlap.
        let a = l.category_row(0);
        let b = l.category_row(1);
        assert!(b.y >= a.y + a.h - 1e-5);
    }

    #[test]
    fn hit_category_finds_clicked_row() {
        let l = PrefsLayout::default_size();
        let r0 = l.category_row(0);
        let r2 = l.category_row(2);
        assert_eq!(l.hit_category(r0.x + 1.0, r0.y + 1.0), Some(Category::General));
        assert_eq!(l.hit_category(r2.x + 1.0, r2.y + 1.0), Some(Category::Font));
        // Click outside the sidebar rows returns None.
        assert_eq!(l.hit_category(500.0, 500.0), None);
    }

    #[test]
    fn control_slot_is_inset_by_label_width() {
        let l = PrefsLayout::default_size();
        let row = l.form_row(0);
        let slot = l.control_slot(0);
        assert!((slot.x - (row.x + LABEL_W)).abs() < 1e-5);
        assert!(slot.w < row.w);
    }

    #[test]
    fn category_labels_are_unique() {
        let labels: Vec<_> = CATEGORIES.iter().map(|c| c.label()).collect();
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len());
    }
}
