use sonicterm_engine::CellMetricsPx;

#[test]
fn exports_font_metric_contract() {
    let metrics = CellMetricsPx { cell_w: 8.0, cell_h: 16.0, underline_h: 1.0, descender: -3.0 };
    assert_eq!(metrics.cell_w, 8.0);
    assert_eq!(metrics.descender, -3.0);
}
