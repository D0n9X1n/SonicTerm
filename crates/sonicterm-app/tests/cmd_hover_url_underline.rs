//! Cmd+hover URL underline contract (v1.0).
//!
//! Verifies the pure helper in `sonicterm_app::app::hovered_url` correctly
//! gates the underline rect on the modifier state and the hover hit.

use sonicterm_app::app::hovered_url::{
    compute_hovered_url_underline, hovered_from_row, CellMetrics, ModifierState,
};

const ROW: u16 = 3;
const URL: &str = "https://example.com/path";
// Prefix and suffix around the URL on the row.
const PREFIX: &str = "see ";
const SUFFIX: &str = " thanks";

fn row_text() -> String {
    format!("{PREFIX}{URL}{SUFFIX}")
}

fn metrics() -> CellMetrics {
    CellMetrics::new(10.0, 20.0)
}

#[test]
fn hover_over_url_without_modifier_emits_no_underline() {
    let s = row_text();
    let col_in_url = (PREFIX.chars().count() + 2) as u16;
    let hovered = hovered_from_row(&s, ROW, col_in_url);
    assert!(hovered.is_some(), "fixture must put the col inside the URL");
    let rect = compute_hovered_url_underline(
        hovered.as_ref(),
        ModifierState { open_url_modifier_held: false },
        metrics(),
    );
    assert!(rect.is_none(), "no underline without the modifier; got {rect:?}");
}

#[test]
fn hover_over_url_with_modifier_emits_underline_spanning_url_cols() {
    let s = row_text();
    let col_in_url = (PREFIX.chars().count() + 5) as u16;
    let hovered = hovered_from_row(&s, ROW, col_in_url).expect("col inside URL");
    let start_col = PREFIX.chars().count() as u16;
    let end_col = (PREFIX.chars().count() + URL.chars().count()) as u16;
    assert_eq!(hovered.start_col, start_col);
    assert_eq!(hovered.end_col, end_col);
    let m = metrics();
    let rect = compute_hovered_url_underline(
        Some(&hovered),
        ModifierState { open_url_modifier_held: true },
        m,
    )
    .expect("underline emitted with modifier held");
    assert!((rect.x - f32::from(start_col) * m.cell_w).abs() < 0.001);
    assert!((rect.w - f32::from(end_col - start_col) * m.cell_w).abs() < 0.001);
    assert!((rect.h - 2.0).abs() < 0.001, "2px-thick per spec; got {}", rect.h);
    // Underline sits at the row's baseline (bottom of cell row).
    let expected_y = f32::from(ROW) * m.cell_h + (m.cell_h - 2.0);
    assert!((rect.y - expected_y).abs() < 0.001);
}

#[test]
fn release_modifier_clears_underline_even_with_hover_still_set() {
    let s = row_text();
    let col_in_url = (PREFIX.chars().count() + 3) as u16;
    let hovered = hovered_from_row(&s, ROW, col_in_url).expect("col inside URL");
    // Modifier held → underline present.
    assert!(compute_hovered_url_underline(
        Some(&hovered),
        ModifierState { open_url_modifier_held: true },
        metrics(),
    )
    .is_some());
    // Modifier released, hover unchanged → cleared.
    assert!(compute_hovered_url_underline(
        Some(&hovered),
        ModifierState { open_url_modifier_held: false },
        metrics(),
    )
    .is_none());
}

#[test]
fn cursor_moves_off_url_clears_underline_even_with_modifier_held() {
    let s = row_text();
    // Column 0 is in PREFIX, not on the URL.
    let off_url = hovered_from_row(&s, ROW, 0);
    assert!(off_url.is_none(), "col 0 is in prefix, not on URL");
    let rect = compute_hovered_url_underline(
        off_url.as_ref(),
        ModifierState { open_url_modifier_held: true },
        metrics(),
    );
    assert!(rect.is_none(), "no underline once the hover hit is gone; got {rect:?}");
}
