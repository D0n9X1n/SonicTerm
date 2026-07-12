use super::*;

/// Device-pixel edges are compared with a small tolerance: the snap math
/// round-trips through `1.0 / scale`, so exact f32 equality is too strict.
fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

/// A logical edge is device-aligned when `edge * scale` is (very near) an
/// integer — that is the whole point of snapping.
fn is_device_aligned(edge: f32, scale: f32) -> bool {
    let d = edge * scale;
    (d - d.round()).abs() < 1e-3
}

#[test]
fn right_and_bottom_are_edges_for_normal_rects() {
    let r = PixelRect { x: 10, y: 20, w: 30, h: 40 };
    assert_eq!(r.right(), 40);
    assert_eq!(r.bottom(), 60);
}

#[test]
fn right_saturates_instead_of_wrapping_when_x_plus_w_overflows() {
    // x + w exceeds i32::MAX; right() must clamp to i32::MAX, never wrap
    // to a negative coordinate.
    let r = PixelRect { x: i32::MAX - 5, y: 0, w: 100, h: 1 };
    assert_eq!(r.right(), i32::MAX);
}

#[test]
fn bottom_saturates_when_width_field_exceeds_i32_range() {
    // A u32 dimension larger than i32::MAX must not sign-flip when narrowed;
    // the edge saturates at i32::MAX.
    let r = PixelRect { x: 0, y: 0, w: 1, h: u32::MAX };
    assert_eq!(r.bottom(), i32::MAX);
}

#[test]
fn is_empty_is_driven_by_either_zero_dimension() {
    assert!(PixelRect { x: 0, y: 0, w: 0, h: 5 }.is_empty());
    assert!(PixelRect { x: 0, y: 0, w: 5, h: 0 }.is_empty());
    assert!(!PixelRect { x: 0, y: 0, w: 5, h: 5 }.is_empty());
}

#[test]
fn intersect_overlap_returns_shared_region() {
    let a = PixelRect { x: 0, y: 0, w: 10, h: 10 };
    let b = PixelRect { x: 5, y: 5, w: 10, h: 10 };
    assert_eq!(a.intersect(b), Some(PixelRect { x: 5, y: 5, w: 5, h: 5 }));
}

#[test]
fn intersect_containment_returns_inner_rect() {
    let outer = PixelRect { x: 0, y: 0, w: 100, h: 100 };
    let inner = PixelRect { x: 10, y: 10, w: 10, h: 10 };
    assert_eq!(outer.intersect(inner), Some(inner));
}

#[test]
fn intersect_touching_edges_is_none_not_a_zero_area_rect() {
    // b starts exactly where a ends on x: they share only an edge, which is
    // zero-area and must be reported as no overlap.
    let a = PixelRect { x: 0, y: 0, w: 10, h: 10 };
    let b = PixelRect { x: 10, y: 0, w: 10, h: 10 };
    assert_eq!(a.intersect(b), None);
}

#[test]
fn intersect_disjoint_is_none() {
    let a = PixelRect { x: 0, y: 0, w: 5, h: 5 };
    let b = PixelRect { x: 100, y: 100, w: 5, h: 5 };
    assert_eq!(a.intersect(b), None);
}

#[test]
fn union_covers_both_disjoint_rects() {
    let a = PixelRect { x: 0, y: 0, w: 10, h: 10 };
    let b = PixelRect { x: 20, y: 20, w: 10, h: 10 };
    assert_eq!(a.union(b), PixelRect { x: 0, y: 0, w: 30, h: 30 });
}

#[test]
fn union_is_order_independent() {
    let a = PixelRect { x: -100, y: -100, w: 50, h: 50 };
    let b = PixelRect { x: 100, y: 100, w: 50, h: 50 };
    assert_eq!(a.union(b), b.union(a));
    // Spans from a.x=-100 to b.right()=150 → width 250; same on y.
    assert_eq!(a.union(b), PixelRect { x: -100, y: -100, w: 250, h: 250 });
}

#[test]
fn union_at_extreme_coordinates_saturates_without_overflow_panic() {
    // Left rect anchored at i32::MIN, right rect's edge saturates at
    // i32::MAX. The full span (i32::MAX - i32::MIN) exceeds i32 range; a
    // narrow subtraction would overflow-panic in debug. The widened path
    // must clamp the dimension to u32::MAX instead.
    let a = PixelRect { x: i32::MIN, y: i32::MIN, w: 10, h: 10 };
    let b = PixelRect { x: i32::MAX - 10, y: i32::MAX - 10, w: 10, h: 10 };
    let u = a.union(b);
    assert_eq!(u.x, i32::MIN);
    assert_eq!(u.y, i32::MIN);
    assert_eq!(u.w, u32::MAX);
    assert_eq!(u.h, u32::MAX);
}

#[test]
fn damage_starts_empty() {
    assert_eq!(DamageRect::empty().rect(), None);
}

#[test]
fn damage_add_inside_bounds_records_the_rect() {
    let bounds = PixelRect { x: 0, y: 0, w: 100, h: 100 };
    let mut d = DamageRect::empty();
    d.add_clipped(PixelRect { x: 10, y: 10, w: 20, h: 20 }, bounds);
    assert_eq!(d.rect(), Some(PixelRect { x: 10, y: 10, w: 20, h: 20 }));
}

#[test]
fn damage_add_fully_outside_bounds_is_ignored() {
    let bounds = PixelRect { x: 0, y: 0, w: 100, h: 100 };
    let mut d = DamageRect::empty();
    d.add_clipped(PixelRect { x: 200, y: 200, w: 10, h: 10 }, bounds);
    assert_eq!(d.rect(), None);
}

#[test]
fn damage_add_straddling_bounds_is_clipped_to_bounds() {
    let bounds = PixelRect { x: 0, y: 0, w: 50, h: 50 };
    let mut d = DamageRect::empty();
    // Rect pokes past both the top-left and stays inside bottom-right.
    d.add_clipped(PixelRect { x: -10, y: -10, w: 30, h: 30 }, bounds);
    assert_eq!(d.rect(), Some(PixelRect { x: 0, y: 0, w: 20, h: 20 }));
}

#[test]
fn damage_accumulates_union_across_adds() {
    let bounds = PixelRect { x: 0, y: 0, w: 100, h: 100 };
    let mut d = DamageRect::empty();
    d.add_clipped(PixelRect { x: 10, y: 10, w: 10, h: 10 }, bounds);
    d.add_clipped(PixelRect { x: 50, y: 50, w: 10, h: 10 }, bounds);
    assert_eq!(d.rect(), Some(PixelRect { x: 10, y: 10, w: 50, h: 50 }));
}

#[test]
fn snap_is_identity_on_integer_scales() {
    // Mac Retina (2.0) and Windows 100 % (1.0) take the fast path untouched
    // so font-derived fractional cell widths are not perturbed.
    let rect = (8.4, 1.2, 8.4, 16.0);
    assert_eq!(snap_to_device_pixels(rect, 1.0), rect);
    assert_eq!(snap_to_device_pixels(rect, 2.0), rect);
}

#[test]
fn snap_moves_edges_onto_device_pixels_at_fractional_scale() {
    let scale = 1.5;
    let (x, y, w, h) = snap_to_device_pixels((0.3, 0.0, 1.0, 1.0), scale);
    // The left edge was not device-aligned (0.3 * 1.5 = 0.45); after
    // snapping every edge lands on an integer device pixel.
    assert!(is_device_aligned(x, scale));
    assert!(is_device_aligned(y, scale));
    assert!(is_device_aligned(x + w, scale));
    assert!(is_device_aligned(y + h, scale));
    // And it actually changed (rounded 0.45 down to device pixel 0).
    assert!(!approx(x, 0.3));
}

#[test]
fn snap_keeps_adjacent_cells_seamless_at_fractional_scale() {
    // Two horizontally-adjacent cells sharing an edge must still share it
    // after snapping — edge-based snapping guarantees the right edge of A
    // equals the left edge of B, so no gap or overlap opens between glyph
    // quads across a row.
    let scale = 1.5;
    let cell_w = 8.4;
    let a = snap_to_device_pixels((0.0, 0.0, cell_w, 16.0), scale);
    let b = snap_to_device_pixels((cell_w, 0.0, cell_w, 16.0), scale);
    let a_right = a.0 + a.2;
    let b_left = b.0;
    assert!(approx(a_right, b_left), "a_right={a_right} b_left={b_left}");
    assert!(is_device_aligned(a_right, scale));
}
