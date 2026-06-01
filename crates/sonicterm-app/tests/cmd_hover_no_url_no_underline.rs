//! Cmd+hover affordance must NOT trigger on plain text (no URL).

use sonicterm_app::app::hovered_url::{
    compute_hovered_url_underline, hovered_from_row, CellMetrics, ModifierState,
};

#[test]
fn hover_over_plain_text_with_modifier_emits_no_underline() {
    // No scheme anywhere on the row.
    let row = "just some plain prompt text, nothing clickable";
    for col in 0..row.chars().count() as u16 {
        let hovered = hovered_from_row(row, 0, col);
        assert!(hovered.is_none(), "col {col} unexpectedly matched a URL");
    }
    let rect = compute_hovered_url_underline(
        None,
        ModifierState { open_url_modifier_held: true },
        CellMetrics::new(10.0, 20.0),
    );
    assert!(rect.is_none(), "no URL → no underline, got {rect:?}");
}

#[test]
fn url_like_but_invalid_scheme_does_not_underline() {
    // `ftp://` is intentionally not on the allow-list — must not match.
    let row = "log: ftp://example.com/x done";
    let col = 10_u16; // inside the "ftp://..." span
    assert!(hovered_from_row(row, 0, col).is_none());
}
