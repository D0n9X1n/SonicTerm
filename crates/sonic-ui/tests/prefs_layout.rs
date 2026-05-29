//! Integration tests for `sonic_ui::prefs::layout`.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in PR 5 of the
//! workspace refactor (issue #121) per CLAUDE.md §5.

use sonic_ui::prefs::layout::{
    Category, PrefsLayout, BUTTON_H, CARD_RADIUS, CATEGORIES, CONTROL_H, FOOTER_H, LABEL_W,
    PRIMARY_BUTTON_W, SECONDARY_BUTTON_W, SIDEBAR_ACCENT_W, SIDEBAR_LABEL_X, SIDEBAR_ROW_GAP,
    SIDEBAR_ROW_H, SIDEBAR_W,
};
use sonic_ui::prefs::{PREFS_WIN_H, PREFS_WIN_W};

#[test]
fn default_size_matches_constants() {
    let l = PrefsLayout::default_size();
    assert_eq!(l.width, PREFS_WIN_W);
    assert_eq!(l.height, PREFS_WIN_H);
}

#[test]
fn prefs_window_default_size_760x600() {
    assert_eq!(PREFS_WIN_W, 760.0);
    assert_eq!(PREFS_WIN_H, 600.0);
}

#[test]
fn prefs_sidebar_width_188() {
    assert_eq!(SIDEBAR_W, 188.0);
    let l = PrefsLayout::default_size();
    assert_eq!(l.sidebar.w, 188.0);
}

#[test]
fn prefs_footer_height_64() {
    assert_eq!(FOOTER_H, 64.0);
    let l = PrefsLayout::default_size();
    assert_eq!(l.footer.h, 64.0);
}

#[test]
fn prefs_card_radius_14() {
    assert_eq!(CARD_RADIUS, 14.0);
}

#[test]
fn prefs_min_window_size_680x520() {
    let l = PrefsLayout::new(100.0, 100.0);
    assert_eq!(l.width, 680.0);
    assert_eq!(l.height, 520.0);
}

#[test]
fn layout_clamps_to_minimum() {
    let l = PrefsLayout::new(50.0, 50.0);
    assert!(l.width >= 680.0);
    assert!(l.height >= 520.0);
}

#[test]
fn sidebar_is_left_strip() {
    let l = PrefsLayout::default_size();
    assert_eq!(l.sidebar.x, 0.0);
    assert_eq!(l.sidebar.w, SIDEBAR_W);
    assert_eq!(l.sidebar.h, PREFS_WIN_H);
}

#[test]
fn content_starts_right_of_sidebar() {
    let l = PrefsLayout::default_size();
    assert_eq!(l.content.x, SIDEBAR_W);
    assert_eq!(l.content.w, PREFS_WIN_W - SIDEBAR_W);
}

#[test]
fn footer_sits_at_bottom_full_width() {
    let l = PrefsLayout::default_size();
    assert!((l.footer.y + l.footer.h - PREFS_WIN_H).abs() < 1e-5);
    assert_eq!(l.footer.h, FOOTER_H);
    assert_eq!(l.footer.w, PREFS_WIN_W);
}

#[test]
fn apply_is_rightmost_button_and_wider_than_cancel() {
    let l = PrefsLayout::default_size();
    assert!(l.apply_button.x > l.cancel_button.x);
    assert_eq!(l.apply_button.w, PRIMARY_BUTTON_W);
    assert_eq!(l.cancel_button.w, SECONDARY_BUTTON_W);
    assert!(l.apply_button.w > l.cancel_button.w);
    assert_eq!(l.apply_button.h, BUTTON_H);
    assert!(l.apply_button.x + l.apply_button.w <= l.footer.x + l.footer.w);
}

#[test]
fn category_rows_have_correct_height_and_gap() {
    let l = PrefsLayout::default_size();
    let r0 = l.category_row(0);
    let r1 = l.category_row(1);
    assert_eq!(r0.h, SIDEBAR_ROW_H);
    assert!((r1.y - r0.y - (SIDEBAR_ROW_H + SIDEBAR_ROW_GAP)).abs() < 1e-5);
}

#[test]
fn prefs_active_category_row_has_left_accent() {
    let l = PrefsLayout::default_size();
    let row = l.category_row(0);
    let accent = l.category_accent(0);
    assert_eq!(accent.w, SIDEBAR_ACCENT_W);
    assert!(accent.x < row.x + SIDEBAR_LABEL_X);
    let row_mid = row.y + row.h / 2.0;
    let acc_mid = accent.y + accent.h / 2.0;
    assert!((row_mid - acc_mid).abs() < 1.0);
}

#[test]
fn hit_category_finds_clicked_row() {
    let l = PrefsLayout::default_size();
    let r0 = l.category_row(0);
    let r2 = l.category_row(2);
    assert_eq!(l.hit_category(r0.x + 1.0, r0.y + 1.0), Some(Category::Font));
    assert_eq!(l.hit_category(r2.x + 1.0, r2.y + 1.0), Some(Category::Keymap));
    assert_eq!(l.hit_category(500.0, 500.0), None);
}

#[test]
fn control_slot_is_inset_by_label_width() {
    let l = PrefsLayout::default_size();
    let row = l.form_row(0);
    let slot = l.control_slot(0);
    assert!((slot.x - (row.x + LABEL_W)).abs() < 1e-5);
    assert!(slot.w < row.w);
    assert_eq!(slot.h, CONTROL_H);
}

#[test]
fn category_labels_are_unique() {
    let labels: Vec<_> = CATEGORIES.iter().map(|c| c.label()).collect();
    let mut sorted = labels.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), labels.len());
}

#[test]
fn every_category_has_description() {
    for c in CATEGORIES {
        assert!(!c.description().is_empty(), "category {:?} missing description", c.label());
    }
}

#[test]
fn every_category_has_a_distinct_svg_icon() {
    let mut keys: Vec<&'static str> = CATEGORIES.iter().map(|c| c.icon().key).collect();
    let original_len = keys.len();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), original_len, "category icons must be unique");
    for c in CATEGORIES {
        let icon = c.icon();
        assert!(!icon.key.is_empty(), "category {:?} icon key is empty", c.label());
        assert!(icon.svg.contains("<svg"), "category {:?} icon is not SVG", c.label());
    }
}

#[test]
fn form_card_fits_inside_content() {
    let l = PrefsLayout::default_size();
    assert!(l.form_card.x >= l.content.x);
    assert!(l.form_card.x + l.form_card.w <= l.content.x + l.content.w + 1.0);
    assert!(l.form_card.y + l.form_card.h <= l.footer.y + 1.0);
}

// ---- Issue #173 slice-2 redesign: two-pane geometry + restyled controls ----

#[test]
fn two_pane_layout_sidebar_and_content_tile_with_no_gap() {
    // Slice-2 spec: sidebar.x + sidebar.w == content.x — the two panes
    // tile edge-to-edge with no gap and no overlap.
    let l = PrefsLayout::default_size();
    assert!(
        (l.sidebar.x + l.sidebar.w - l.content.x).abs() < 1e-5,
        "sidebar.x ({}) + sidebar.w ({}) must equal content.x ({})",
        l.sidebar.x,
        l.sidebar.w,
        l.content.x,
    );
    // And the union must span the full window width.
    assert!((l.sidebar.w + l.content.w - PREFS_WIN_W).abs() < 1e-5);
    // Same property must hold at minimum size.
    let l2 = PrefsLayout::new(680.0, 520.0);
    assert!((l2.sidebar.x + l2.sidebar.w - l2.content.x).abs() < 1e-5);
}

#[test]
fn redesigned_control_radius_is_ten_for_pill_family() {
    // Slice-2 spec: button + combobox share radius 10 (issue #169).
    use sonic_ui::prefs::layout::{BUTTON_RADIUS, CONTROL_RADIUS};
    assert_eq!(CONTROL_RADIUS, 10.0);
    assert_eq!(BUTTON_RADIUS, 10.0);
    assert_eq!(BUTTON_RADIUS, CONTROL_RADIUS, "button and combobox share the pill radius");
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn focus_ring_constants_present_for_halo() {
    // Slice-2 spec: every focusable control gets a 4-6 px theme.accent halo.
    use sonic_ui::prefs::layout::{FOCUS_RING_HALO, FOCUS_RING_THICKNESS};
    assert!(
        (4.0..=6.0).contains(&FOCUS_RING_HALO),
        "focus halo {} px should be in spec range 4–6",
        FOCUS_RING_HALO
    );
    assert!(FOCUS_RING_THICKNESS > 0.0, "thickness {FOCUS_RING_THICKNESS} must be positive");
}
