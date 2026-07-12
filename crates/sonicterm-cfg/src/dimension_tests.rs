use super::*;

fn context() -> DimensionContext {
    DimensionContext { dpi: 144.0, pixel_max: 800.0, pixel_cell: 12.5 }
}

#[test]
fn pixel_values_floor_positive_and_negative_inputs() {
    assert_eq!(Dimension::Pixels(10.9).evaluate_as_pixels(context()), 10.0);
    assert_eq!(Dimension::Pixels(-0.1).evaluate_as_pixels(context()), -1.0);
}

#[test]
fn points_scale_by_dpi_then_floor() {
    assert_eq!(Dimension::Points(12.0).evaluate_as_pixels(context()), 24.0);
    assert_eq!(Dimension::Points(7.25).evaluate_as_pixels(context()), 14.0);
}

#[test]
fn percent_uses_pixel_max_and_cells_use_cell_width() {
    assert_eq!(Dimension::Percent(0.25).evaluate_as_pixels(context()), 200.0);
    assert_eq!(Dimension::Cells(2.5).evaluate_as_pixels(context()), 31.0);
}
