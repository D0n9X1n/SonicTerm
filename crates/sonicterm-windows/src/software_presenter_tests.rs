use super::*;

#[test]
fn force_prefers_software_presenter() {
    let pref = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Force);
    assert!(pref.should_use(false));
    assert!(pref.forces_opaque_window());
}

#[test]
fn auto_follows_detection() {
    let pref = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Auto);
    assert!(pref.should_use(true));
    assert!(!pref.should_use(false));
    assert!(!pref.forces_opaque_window());
}

#[test]
fn off_never_uses_software_presenter() {
    let pref = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Off);
    assert!(!pref.should_use(true));
    assert!(!pref.should_use(false));
}

/// The app layer and the Windows presenter each decide the software path
/// from their own copy of the `(mode, detected)` pair. A mode honored by one
/// layer and ignored by the other is a half-degraded renderer, so the two
/// decisions must stay identical over the whole input domain.
#[test]
fn presenter_and_app_degrade_decisions_agree_across_all_modes() {
    for mode in [SoftwareRenderMode::Auto, SoftwareRenderMode::Force, SoftwareRenderMode::Off] {
        for detected in [false, true] {
            let presenter =
                WindowsSoftwarePresenterPreference::from_config(mode).should_use(detected);
            let app = sonicterm_app::app::should_degrade_for_software_render(mode, detected);
            assert_eq!(
                presenter, app,
                "presenter and app disagree for mode {mode:?} with detected={detected}"
            );
        }
    }
}

/// Pins the truth table both layers are required to implement: `Auto` defers
/// to detection, `Force` degrades regardless, `Off` never degrades. Guards
/// against the two implementations drifting together into a shared mistake,
/// which the agreement test alone would not catch.
#[test]
fn degrade_decision_truth_table_is_pinned() {
    let expected = [
        (SoftwareRenderMode::Auto, false, false),
        (SoftwareRenderMode::Auto, true, true),
        (SoftwareRenderMode::Force, false, true),
        (SoftwareRenderMode::Force, true, true),
        (SoftwareRenderMode::Off, false, false),
        (SoftwareRenderMode::Off, true, false),
    ];
    for (mode, detected, want) in expected {
        assert_eq!(
            WindowsSoftwarePresenterPreference::from_config(mode).should_use(detected),
            want,
            "presenter should_use({mode:?}, {detected})"
        );
        assert_eq!(
            sonicterm_app::app::should_degrade_for_software_render(mode, detected),
            want,
            "app should_degrade_for_software_render({mode:?}, {detected})"
        );
    }
}

/// Characterizes the deliberate asymmetry between the two questions the
/// presenter preference answers. `should_use` reports whether the software
/// path applies at all and follows detection under `Auto`; only `Force`
/// additionally overrides the configured DWM backdrop to opaque. `Auto`
/// therefore leaves a user's translucent backdrop intact even when it
/// software-presents, and `forces_opaque_window` is independent of detection.
#[test]
fn only_force_overrides_the_configured_backdrop() {
    for detected in [false, true] {
        let auto = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Auto);
        assert_eq!(auto.should_use(detected), detected);
        assert!(!auto.forces_opaque_window());

        let force = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Force);
        assert!(force.should_use(detected));
        assert!(force.forces_opaque_window());

        let off = WindowsSoftwarePresenterPreference::from_config(SoftwareRenderMode::Off);
        assert!(!off.should_use(detected));
        assert!(!off.forces_opaque_window());
    }
}

#[test]
fn dirty_rect_clips_to_surface() {
    assert_eq!(
        DirtyRect { x: 8, y: 9, w: 8, h: 8 }.clipped(10, 12),
        Some(DirtyRect { x: 8, y: 9, w: 2, h: 3 })
    );
    assert_eq!(DirtyRect { x: 10, y: 0, w: 1, h: 1 }.clipped(10, 12), None);
}

#[test]
fn fill_rect_updates_pixels_and_dirty_set() {
    let mut surface = SoftwareSurface::new(4, 3);
    surface.fill_rect_bgra(DirtyRect { x: 1, y: 1, w: 2, h: 1 }, [1, 2, 3, 4]);
    assert_eq!(surface.dirty_rects(), &[DirtyRect { x: 1, y: 1, w: 2, h: 1 }]);
    let stride = surface.width() as usize * 4;
    assert_eq!(&surface.pixels()[stride + 4..stride + 8], &[1, 2, 3, 4]);
    assert_eq!(&surface.pixels()[stride + 8..stride + 12], &[1, 2, 3, 4]);
    assert_eq!(&surface.pixels()[0..4], &[0, 0, 0, 0]);
}

#[test]
fn software_surface_size_is_checked_and_bounded() {
    assert_eq!(pixel_len(7680, 4320), Some(7680 * 4320 * 4));
    assert_eq!(pixel_len(8192, 4320), Some(8192 * 4320 * 4));
    assert_eq!(pixel_len(8192, 8192), None);
    assert_eq!(pixel_len(u32::MAX, u32::MAX), None);
}

#[test]
fn software_surface_shrink_releases_capacity() {
    let mut surface = SoftwareSurface::try_new(1024, 1024).expect("valid surface");
    let old_capacity = surface.pixels.capacity();

    assert!(surface.try_resize(2, 2));

    assert!(surface.pixels.capacity() < old_capacity / 2);
}

#[test]
fn software_surface_growth_uses_exact_validated_capacity() {
    let mut surface = SoftwareSurface::try_new(2, 2).expect("valid surface");

    assert!(surface.try_resize(100, 100));

    assert_eq!(surface.pixels.capacity(), 100 * 100 * 4);
}

/// Independent half-open intersection of `[x, x + w) x [y, y + h)` with
/// `[0, width) x [0, height)`, computed in `u64` so no clamping or wrapping can
/// hide an error in the `u32` implementation under test.
fn reference_clip(
    rect: DirtyRect,
    width: u32,
    height: u32,
) -> Option<DirtyRect> {
    let x1 = u64::from(rect.x);
    let y1 = u64::from(rect.y);
    let x2 = (x1 + u64::from(rect.w)).min(u64::from(width));
    let y2 = (y1 + u64::from(rect.h)).min(u64::from(height));
    if x1 >= x2 || y1 >= y2 {
        return None;
    }
    Some(DirtyRect {
        x: rect.x,
        y: rect.y,
        w: (x2 - x1) as u32,
        h: (y2 - y1) as u32,
    })
}

// Odd, non-round surface dimensions used throughout the edge tests. Round
// numbers can mask an off-by-one that only shows at a coordinate a power of two
// does not land on.
const ODD_W: u32 = 803;
const ODD_H: u32 = 597;

#[test]
fn dirty_rect_new_rejects_zero_extent() {
    assert_eq!(DirtyRect::new(0, 0, 0, 5), None);
    assert_eq!(DirtyRect::new(0, 0, 5, 0), None);
    assert_eq!(DirtyRect::new(0, 0, 0, 0), None);
    assert_eq!(DirtyRect::new(7, 9, 0, 0), None);
    // A one-pixel rect is the smallest accepted extent.
    assert_eq!(DirtyRect::new(7, 9, 1, 1), Some(DirtyRect { x: 7, y: 9, w: 1, h: 1 }));
}

#[test]
fn dirty_rect_clipped_keeps_fully_contained_rect_unchanged() {
    let rect = DirtyRect { x: 100, y: 200, w: 50, h: 60 };
    assert_eq!(rect.clipped(ODD_W, ODD_H), Some(rect));
}

#[test]
fn dirty_rect_clipped_at_exact_edge_is_not_truncated() {
    // x + w == width and y + h == height is the classic off-by-one boundary:
    // the rect exactly fills up to the last valid pixel and must survive whole.
    let flush_right = DirtyRect { x: ODD_W - 10, y: 5, w: 10, h: 4 };
    assert_eq!(flush_right.clipped(ODD_W, ODD_H), Some(flush_right));

    let flush_bottom = DirtyRect { x: 5, y: ODD_H - 7, w: 4, h: 7 };
    assert_eq!(flush_bottom.clipped(ODD_W, ODD_H), Some(flush_bottom));

    let flush_corner = DirtyRect { x: ODD_W - 1, y: ODD_H - 1, w: 1, h: 1 };
    assert_eq!(flush_corner.clipped(ODD_W, ODD_H), Some(flush_corner));

    let full_surface = DirtyRect { x: 0, y: 0, w: ODD_W, h: ODD_H };
    assert_eq!(full_surface.clipped(ODD_W, ODD_H), Some(full_surface));
}

#[test]
fn dirty_rect_clipped_trims_overhang_past_right_and_bottom_edges() {
    // One pixel past the right edge loses exactly one column.
    assert_eq!(
        DirtyRect { x: ODD_W - 10, y: 5, w: 11, h: 4 }.clipped(ODD_W, ODD_H),
        Some(DirtyRect { x: ODD_W - 10, y: 5, w: 10, h: 4 })
    );
    // One pixel past the bottom edge loses exactly one row.
    assert_eq!(
        DirtyRect { x: 5, y: ODD_H - 7, w: 4, h: 8 }.clipped(ODD_W, ODD_H),
        Some(DirtyRect { x: 5, y: ODD_H - 7, w: 4, h: 7 })
    );
    // Overhanging both edges at once trims both axes independently.
    assert_eq!(
        DirtyRect { x: ODD_W - 3, y: ODD_H - 2, w: 400, h: 400 }.clipped(ODD_W, ODD_H),
        Some(DirtyRect { x: ODD_W - 3, y: ODD_H - 2, w: 3, h: 2 })
    );
    // A rect starting at the origin and far larger than the surface clamps to it.
    assert_eq!(
        DirtyRect { x: 0, y: 0, w: 100_000, h: 100_000 }.clipped(ODD_W, ODD_H),
        Some(DirtyRect { x: 0, y: 0, w: ODD_W, h: ODD_H })
    );
}

#[test]
fn dirty_rect_clipped_rejects_rects_outside_the_surface() {
    // First column/row past the edge is already fully outside.
    assert_eq!(DirtyRect { x: ODD_W, y: 0, w: 1, h: 1 }.clipped(ODD_W, ODD_H), None);
    assert_eq!(DirtyRect { x: 0, y: ODD_H, w: 1, h: 1 }.clipped(ODD_W, ODD_H), None);
    assert_eq!(DirtyRect { x: ODD_W, y: ODD_H, w: 50, h: 50 }.clipped(ODD_W, ODD_H), None);
    // Far outside on either axis.
    assert_eq!(DirtyRect { x: 5_000, y: 5, w: 10, h: 10 }.clipped(ODD_W, ODD_H), None);
    assert_eq!(DirtyRect { x: 5, y: 5_000, w: 10, h: 10 }.clipped(ODD_W, ODD_H), None);
    // A zero-extent rect that bypassed `new` clips away rather than surviving.
    assert_eq!(DirtyRect { x: 5, y: 5, w: 0, h: 10 }.clipped(ODD_W, ODD_H), None);
    assert_eq!(DirtyRect { x: 5, y: 5, w: 10, h: 0 }.clipped(ODD_W, ODD_H), None);
}

#[test]
fn dirty_rect_clipped_saturates_instead_of_wrapping_near_u32_max() {
    // Without a saturating add, `x + w` wraps and the rect would appear to start
    // inside the surface with a tiny width. It must clip away entirely instead.
    assert_eq!(DirtyRect { x: u32::MAX - 1, y: 0, w: 10, h: 10 }.clipped(ODD_W, ODD_H), None);
    assert_eq!(DirtyRect { x: 0, y: u32::MAX - 1, w: 10, h: 10 }.clipped(ODD_W, ODD_H), None);
    assert_eq!(
        DirtyRect { x: u32::MAX, y: u32::MAX, w: u32::MAX, h: u32::MAX }.clipped(ODD_W, ODD_H),
        None
    );
    // Saturation on the far edge only, with the origin still inside, keeps the
    // whole surface rather than collapsing it.
    assert_eq!(
        DirtyRect { x: 0, y: 0, w: u32::MAX, h: u32::MAX }.clipped(ODD_W, ODD_H),
        Some(DirtyRect { x: 0, y: 0, w: ODD_W, h: ODD_H })
    );
    assert_eq!(
        DirtyRect { x: ODD_W - 1, y: ODD_H - 1, w: u32::MAX, h: u32::MAX }.clipped(ODD_W, ODD_H),
        Some(DirtyRect { x: ODD_W - 1, y: ODD_H - 1, w: 1, h: 1 })
    );
}

#[test]
fn dirty_rect_clipped_is_idempotent() {
    // Presenting a rect that was already clipped must not shrink it again.
    for &(x, y, w, h) in &[
        (0_u32, 0_u32, ODD_W, ODD_H),
        (ODD_W - 3, ODD_H - 2, 400, 400),
        (11, 13, 7, 5),
        (0, 0, u32::MAX, u32::MAX),
    ] {
        let once = DirtyRect { x, y, w, h }.clipped(ODD_W, ODD_H).expect("rect intersects surface");
        assert_eq!(once.clipped(ODD_W, ODD_H), Some(once));
    }
}

#[test]
fn dirty_rect_clipped_matches_reference_intersection_exhaustively() {
    // Sweep every origin and extent in a window that straddles all four edges of
    // a small odd surface, comparing against a widened-arithmetic reference.
    const W: u32 = 7;
    const H: u32 = 5;
    for x in 0..=W + 2 {
        for y in 0..=H + 2 {
            for w in 0..=W + 3 {
                for h in 0..=H + 3 {
                    let rect = DirtyRect { x, y, w, h };
                    let actual = rect.clipped(W, H);
                    assert_eq!(
                        actual,
                        reference_clip(rect, W, H),
                        "clip mismatch for {rect:?} against {W}x{H}"
                    );
                    if let Some(clipped) = actual {
                        assert!(
                            clipped.x + clipped.w <= W && clipped.y + clipped.h <= H,
                            "clipped rect {clipped:?} escapes {W}x{H}"
                        );
                        assert!(clipped.w > 0 && clipped.h > 0, "empty rect survived clipping");
                    }
                }
            }
        }
    }
}

#[test]
fn dirty_rect_clipped_matches_reference_along_odd_surface_edges() {
    // Same equivalence check at the odd dimensions the surface actually uses,
    // sampled along each edge and past both extremes rather than exhaustively.
    let coords = [0, 1, 2, ODD_W / 2, ODD_W - 2, ODD_W - 1, ODD_W, ODD_W + 1, u32::MAX - 1, u32::MAX];
    let extents = [0, 1, 2, 3, ODD_H - 1, ODD_H, ODD_H + 1, ODD_W, u32::MAX - 1, u32::MAX];
    for &x in &coords {
        for &y in &coords {
            for &w in &extents {
                for &h in &extents {
                    let rect = DirtyRect { x, y, w, h };
                    let actual = rect.clipped(ODD_W, ODD_H);
                    assert_eq!(
                        actual,
                        reference_clip(rect, ODD_W, ODD_H),
                        "clip mismatch for {rect:?} against {ODD_W}x{ODD_H}"
                    );
                    if let Some(clipped) = actual {
                        assert!(clipped.x + clipped.w <= ODD_W && clipped.y + clipped.h <= ODD_H);
                    }
                }
            }
        }
    }
}

#[test]
fn mark_dirty_records_only_clipped_rects() {
    let mut surface = SoftwareSurface::try_new(ODD_W, ODD_H).expect("valid surface");

    // Fully outside contributes nothing.
    surface.mark_dirty(DirtyRect { x: ODD_W, y: 0, w: 4, h: 4 });
    surface.mark_dirty(DirtyRect { x: 0, y: ODD_H, w: 4, h: 4 });
    surface.mark_dirty(DirtyRect { x: u32::MAX - 1, y: 0, w: 10, h: 10 });
    assert!(surface.dirty_rects().is_empty());

    // Overhanging is trimmed to the surface before being recorded.
    surface.mark_dirty(DirtyRect { x: ODD_W - 2, y: ODD_H - 3, w: 999, h: 999 });
    assert_eq!(
        surface.dirty_rects(),
        &[DirtyRect { x: ODD_W - 2, y: ODD_H - 3, w: 2, h: 3 }]
    );
}

#[test]
fn fill_rect_bgra_writes_only_inside_the_clipped_region() {
    // A surface small enough to verify pixel by pixel, with odd extents so the
    // last column and row are not aligned to anything.
    const W: u32 = 7;
    const H: u32 = 5;
    let requested = DirtyRect { x: W - 2, y: H - 2, w: 100, h: 100 };
    let expected = reference_clip(requested, W, H).expect("rect intersects surface");

    let mut surface = SoftwareSurface::try_new(W, H).expect("valid surface");
    surface.fill_rect_bgra(requested, [9, 8, 7, 6]);

    assert_eq!(surface.dirty_rects(), &[expected]);

    let stride = W as usize * 4;
    for y in 0..H {
        for x in 0..W {
            let offset = y as usize * stride + x as usize * 4;
            let inside = x >= expected.x
                && x < expected.x + expected.w
                && y >= expected.y
                && y < expected.y + expected.h;
            let want: [u8; 4] = if inside { [9, 8, 7, 6] } else { [0, 0, 0, 0] };
            assert_eq!(
                &surface.pixels()[offset..offset + 4],
                &want,
                "pixel ({x},{y}) should be {}",
                if inside { "filled" } else { "untouched" }
            );
        }
    }
}

#[test]
fn fill_rect_bgra_never_indexes_out_of_bounds() {
    // The pixel indexing is unchecked slice arithmetic, so an unclipped or
    // over-clipped rect would panic rather than render wrong. Sweep origins and
    // extents that straddle every edge and confirm the write stays in bounds.
    const W: u32 = 7;
    const H: u32 = 5;
    let len = (W * H * 4) as usize;
    for x in 0..=W + 2 {
        for y in 0..=H + 2 {
            for w in [0, 1, 2, W, W + 5, u32::MAX - 1, u32::MAX] {
                for h in [0, 1, 2, H, H + 5, u32::MAX - 1, u32::MAX] {
                    let mut surface = SoftwareSurface::try_new(W, H).expect("valid surface");
                    surface.fill_rect_bgra(DirtyRect { x, y, w, h }, [1, 2, 3, 4]);
                    assert_eq!(surface.pixels().len(), len);
                    for rect in surface.dirty_rects() {
                        assert!(
                            rect.x + rect.w <= W && rect.y + rect.h <= H,
                            "recorded rect {rect:?} escapes {W}x{H}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn fill_rect_bgra_ignores_rects_outside_the_surface() {
    let mut surface = SoftwareSurface::try_new(ODD_W, ODD_H).expect("valid surface");
    surface.fill_rect_bgra(DirtyRect { x: ODD_W, y: ODD_H, w: 10, h: 10 }, [255, 255, 255, 255]);

    assert!(surface.dirty_rects().is_empty());
    assert!(surface.pixels().iter().all(|&b| b == 0), "no pixel should have been written");
}

#[test]
fn fill_rect_bgra_reaches_the_final_pixel_at_an_odd_size() {
    // The very last pixel of an odd-sized surface is the one an off-by-one in
    // either the clip or the row stride would miss.
    let mut surface = SoftwareSurface::try_new(ODD_W, ODD_H).expect("valid surface");
    surface.fill_rect_bgra(DirtyRect { x: ODD_W - 1, y: ODD_H - 1, w: 1, h: 1 }, [1, 2, 3, 4]);

    let len = surface.pixels().len();
    assert_eq!(&surface.pixels()[len - 4..], &[1, 2, 3, 4]);
    assert_eq!(surface.dirty_rects(), &[DirtyRect { x: ODD_W - 1, y: ODD_H - 1, w: 1, h: 1 }]);
}

#[test]
fn fill_rect_bgra_covering_whole_odd_surface_writes_every_pixel() {
    let mut surface = SoftwareSurface::try_new(ODD_W, ODD_H).expect("valid surface");
    surface.fill_rect_bgra(DirtyRect { x: 0, y: 0, w: u32::MAX, h: u32::MAX }, [4, 3, 2, 1]);

    assert_eq!(surface.dirty_rects(), &[DirtyRect { x: 0, y: 0, w: ODD_W, h: ODD_H }]);
    assert!(
        surface.pixels().chunks_exact(4).all(|px| px == [4, 3, 2, 1]),
        "every pixel of the surface should have been written"
    );
}

#[test]
fn try_resize_to_same_size_is_a_no_op() {
    let mut surface = SoftwareSurface::try_new(ODD_W, ODD_H).expect("valid surface");
    assert!(surface.try_resize(ODD_W, ODD_H));

    assert_eq!(surface.width(), ODD_W);
    assert_eq!(surface.height(), ODD_H);
    // No reallocation and no repaint is requested when nothing changed.
    assert!(surface.dirty_rects().is_empty());
}

#[test]
fn try_resize_marks_the_full_new_area_dirty() {
    let mut surface = SoftwareSurface::try_new(64, 64).expect("valid surface");
    surface.clear_dirty();

    assert!(surface.try_resize(ODD_W, ODD_H));

    assert_eq!(surface.width(), ODD_W);
    assert_eq!(surface.height(), ODD_H);
    assert_eq!(surface.pixels().len(), (ODD_W * ODD_H * 4) as usize);
    assert_eq!(surface.dirty_rects(), &[DirtyRect { x: 0, y: 0, w: ODD_W, h: ODD_H }]);
}

#[test]
fn try_resize_clamps_zero_dimensions_to_one_pixel() {
    let mut surface = SoftwareSurface::try_new(32, 32).expect("valid surface");
    surface.clear_dirty();

    assert!(surface.try_resize(0, 0));

    assert_eq!((surface.width(), surface.height()), (1, 1));
    assert_eq!(surface.pixels().len(), 4);
    assert_eq!(surface.dirty_rects(), &[DirtyRect { x: 0, y: 0, w: 1, h: 1 }]);
}

#[test]
fn try_resize_rejects_sizes_past_the_safety_limit() {
    let mut surface = SoftwareSurface::try_new(64, 48).expect("valid surface");
    surface.clear_dirty();

    assert!(!surface.try_resize(16_384, 16_384));

    // A rejected resize leaves the surface entirely untouched, so the retained
    // pixels still match the dimensions the presenter will read them with.
    assert_eq!((surface.width(), surface.height()), (64, 48));
    assert_eq!(surface.pixels().len(), 64 * 48 * 4);
    assert!(surface.dirty_rects().is_empty());
}

#[test]
fn try_resize_reuses_allocation_within_the_hysteresis_band() {
    // Shrinking to at least half the current capacity keeps the allocation, so
    // a drag-resize does not reallocate on every intermediate size.
    let mut surface = SoftwareSurface::try_new(100, 100).expect("valid surface");
    let capacity = surface.pixels.capacity();

    assert!(surface.try_resize(100, 50));

    assert_eq!(surface.pixels().len(), 100 * 50 * 4);
    assert_eq!(surface.pixels.capacity(), capacity, "half-size shrink should reuse the buffer");
}

#[test]
fn try_resize_reallocates_below_half_capacity() {
    // Dropping under half the capacity releases it rather than retaining a
    // buffer far larger than the surface.
    let mut surface = SoftwareSurface::try_new(100, 100).expect("valid surface");
    let capacity = surface.pixels.capacity();

    assert!(surface.try_resize(100, 49));

    assert_eq!(surface.pixels().len(), 100 * 49 * 4);
    assert!(surface.pixels.capacity() < capacity, "sub-half shrink should release capacity");
}

#[test]
fn try_resize_growth_within_capacity_reuses_allocation() {
    // Growing back into a buffer that is still large enough reuses it.
    let mut surface = SoftwareSurface::try_new(100, 100).expect("valid surface");
    let capacity = surface.pixels.capacity();
    assert!(surface.try_resize(100, 60));
    assert_eq!(surface.pixels.capacity(), capacity);

    assert!(surface.try_resize(100, 80));

    assert_eq!(surface.pixels().len(), 100 * 80 * 4);
    assert_eq!(surface.pixels.capacity(), capacity, "growth under capacity should reuse the buffer");
}

#[test]
fn resized_surface_still_clips_fills_to_the_new_bounds() {
    // After a shrink the old extents must no longer be writable, otherwise the
    // unchecked fill indexing would run past the reallocated buffer.
    let mut surface = SoftwareSurface::try_new(ODD_W, ODD_H).expect("valid surface");
    assert!(surface.try_resize(101, 97));
    surface.clear_dirty();

    surface.fill_rect_bgra(DirtyRect { x: 0, y: 0, w: ODD_W, h: ODD_H }, [1, 1, 1, 1]);

    assert_eq!(surface.pixels().len(), 101 * 97 * 4);
    assert_eq!(surface.dirty_rects(), &[DirtyRect { x: 0, y: 0, w: 101, h: 97 }]);
    assert!(surface.pixels().chunks_exact(4).all(|px| px == [1, 1, 1, 1]));
}

#[test]
fn shrinking_discards_dirty_rects_recorded_at_the_previous_size() {
    // A rect recorded before the resize was clipped against the old, larger
    // dimensions. Presenting it against the reallocated buffer asks GDI to
    // read a region the buffer no longer covers, so the resize must drop it.
    let mut surface = SoftwareSurface::try_new(ODD_W, ODD_H).expect("valid surface");
    surface.mark_dirty(DirtyRect { x: 0, y: 0, w: ODD_W, h: ODD_H });

    assert!(surface.try_resize(101, 97));

    assert_eq!(
        surface.dirty_rects(),
        &[DirtyRect { x: 0, y: 0, w: 101, h: 97 }],
        "only the full-area rect for the new size may remain"
    );
}

/// Every retained rect fits the surface it will be presented against.
///
/// Stated as the invariant rather than as one expected list, so it holds for
/// resize sequences the explicit cases above do not enumerate. The shrink,
/// grow, and shrink-again ordering matters: a fix that cleared only on shrink
/// would pass a shrink-only test.
#[test]
fn no_dirty_rect_outlives_the_surface_it_was_recorded_against() {
    let mut surface = SoftwareSurface::try_new(ODD_W, ODD_H).expect("valid surface");

    for (w, h) in [(101u32, 97u32), (809, 601), (17, 13), (1, 1), (640, 480)] {
        surface.mark_dirty(DirtyRect {
            x: 0,
            y: 0,
            w: surface.width(),
            h: surface.height(),
        });
        assert!(surface.try_resize(w, h));

        for rect in surface.dirty_rects() {
            assert!(
                rect.x + rect.w <= surface.width() && rect.y + rect.h <= surface.height(),
                "dirty rect {rect:?} exceeds the {}x{} surface it will be presented against",
                surface.width(),
                surface.height(),
            );
        }
    }
}

/// A no-op resize keeps pending rects.
///
/// The clear belongs to a real reallocation. Dropping rects on a resize to the
/// same size would discard a pending partial update and lose the frame — the
/// opposite defect, and one a same-size caller would hit on every frame.
#[test]
fn resize_to_the_same_size_keeps_pending_dirty_rects() {
    let mut surface = SoftwareSurface::try_new(200, 100).expect("valid surface");
    surface.mark_dirty(DirtyRect { x: 10, y: 20, w: 30, h: 40 });

    assert!(surface.try_resize(200, 100));

    assert_eq!(
        surface.dirty_rects(),
        &[DirtyRect { x: 10, y: 20, w: 30, h: 40 }],
        "a resize that changes nothing must not discard a pending update"
    );
}
