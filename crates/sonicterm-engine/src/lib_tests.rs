//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::CellMetricsPx;

#[test]
fn exports_font_metric_contract() {
    let metrics = CellMetricsPx { cell_w: 8.0, cell_h: 16.0, underline_h: 1.0, descender: -3.0 };
    assert_eq!(metrics.cell_w, 8.0);
    assert_eq!(metrics.descender, -3.0);
}
