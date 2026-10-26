//! Renderer copy mode contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

#[test]
fn selection_quads_follow_absolute_text_through_viewport_scroll() {
    let selection = selection_for_rows(10, 11);
    let at_ten = selection_quad_rects(&selection, 10, 4, 8, 0.0, 0.0, 10.0, 20.0, &[]);
    let at_nine = selection_quad_rects(&selection, 9, 4, 8, 0.0, 0.0, 10.0, 20.0, &[]);

    assert_eq!(at_ten.len(), 2);
    assert_eq!(at_nine.len(), 2);
    assert_eq!(at_nine[0].1 - at_ten[0].1, 20.0);
    assert_eq!(at_nine[1].1 - at_ten[1].1, 20.0);
}

#[test]
fn selection_quads_are_absent_when_the_range_is_outside_the_viewport() {
    let above = selection_for_rows(2, 4);
    let below = selection_for_rows(20, 22);

    assert!(selection_quad_rects(&above, 10, 4, 8, 0.0, 0.0, 10.0, 20.0, &[]).is_empty());
    assert!(selection_quad_rects(&below, 10, 4, 8, 0.0, 0.0, 10.0, 20.0, &[]).is_empty());
}

#[test]
fn selection_quad_walk_is_bounded_by_viewport_rows() {
    let huge = selection_for_rows(0, u64::MAX);
    let rects = selection_quad_rects(&huge, 1_000_000, 12, 8, 0.0, 0.0, 10.0, 20.0, &[]);
    assert_eq!(rects.len(), 12);
}

#[test]
fn copy_mode_rows_use_the_transposed_coordinate_slot() {
    let mut copy = CopyModeState::new_at((7, 11));
    copy.start_select();
    copy.cursor = (9, 13);
    let (start, end) = copy.selected_range().expect("mutable copy selection");

    assert_eq!((start.1, end.1), (11, 13), "copy-mode rows live in tuple slot one");
    assert_eq!(GpuRenderer::viewport_relative_row(start.1, 10, 8), Some(1));
    assert_eq!(GpuRenderer::viewport_relative_row(start.0, 10, 8), None);
}
