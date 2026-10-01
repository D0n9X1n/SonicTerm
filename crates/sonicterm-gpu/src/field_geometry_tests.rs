use super::*;
use crate::chrome_text::shaped_advances;
use sonicterm_render_model::boundary::ui::command_palette::CommandPalette;
use sonicterm_render_model::boundary::ui::search::SearchState;

/// Hold the shared font fixture even after a sibling test poisoned it.
fn font_fixture_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::lib_tests::TRACKED_FONT_STACK_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Uniform advances, one per scalar, for deterministic pure geometry tests.
fn uniform_boundaries(text: &str, advance_px: f32) -> FieldBoundaries {
    let advances: Vec<(usize, f32)> =
        text.char_indices().map(|(offset, _)| (offset, advance_px)).collect();
    FieldBoundaries::from_advances(text, &advances)
}

/// A placement whose clip and hit area are the same rectangle.
fn placement(kind: FieldKind, clip: FieldRect) -> FieldPlacement {
    FieldPlacement {
        kind,
        clip,
        hit_area: clip,
        caret_y: clip.y,
        caret_h: clip.h,
        caret_fallback_w: 6.0,
        font_size_px: 15.0,
        native_em_px: 15.0,
    }
}

/// A plain field text over `label` whose whole label is the query.
fn plain_text(label: &str, caret: usize, selection: Option<Range<usize>>) -> FieldText {
    FieldText {
        kind: FieldKind::Palette,
        label: label.to_string(),
        content: 0..label.len(),
        caret,
        selection,
        composing: false,
        variant: 0,
    }
}

/// Plan without a presented frame, preserving the stateless clipping fixtures.
fn stateless_field(
    placement: FieldPlacement,
    boundaries: &FieldBoundaries,
    text: &FieldText,
    caret: usize,
    selection: Option<Range<usize>>,
    environment: u64,
) -> FieldGeometry {
    plan_field(placement, boundaries, text, caret, selection, environment, None)
}

fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!((actual - expected).abs() < 1e-4, "{what}: {actual} vs {expected}");
}

/// Glyphs sharing a cluster merge into one boundary, a cluster offset inside a
/// scalar snaps back to its UTF-8 start, and a caret inside a combining cluster
/// draws at the cluster's leading edge.
#[test]
fn boundaries_merge_clusters_on_utf8_edges() {
    let text = "e\u{301}中b";
    // Two glyphs for the combining cluster at 0, a stray cluster inside 中 at byte 4.
    let advances = [(0, 10.0), (0, 0.0), (3, 20.0), (4, 0.0), (6, 8.0)];
    let boundaries = FieldBoundaries::from_advances(text, &advances);

    assert_eq!(boundaries.stops, vec![(0, 0.0), (3, 10.0), (6, 30.0)]);
    assert_close(boundaries.total_width(), 38.0, "total");
    assert_close(boundaries.caret_x(1), 0.0, "caret inside combining cluster");
    assert_close(boundaries.caret_x(4), 10.0, "caret inside a CJK scalar");
    assert_close(boundaries.caret_x(text.len()), 38.0, "caret at run end");
    assert_eq!(boundaries.caret_width(2), Some(10.0));
    assert_eq!(boundaries.caret_width(text.len()), None);
}

/// Selection spans widen to whole clusters, normalize reversed ranges, and are
/// empty for collapsed ranges.
#[test]
fn selection_spans_cover_whole_clusters_in_either_direction() {
    let text = "e\u{301}中b";
    let boundaries = FieldBoundaries::from_advances(text, &[(0, 10.0), (3, 20.0), (6, 8.0)]);

    assert_eq!(boundaries.span_x(1..4), Some((0.0, 30.0)), "partial clusters widen");
    assert_eq!(boundaries.span_x(3..6), Some((10.0, 30.0)));
    #[allow(clippy::reversed_empty_ranges)]
    let reversed = 6..3;
    assert_eq!(boundaries.span_x(reversed), Some((10.0, 30.0)), "reverse drag normalizes");
    assert_eq!(boundaries.span_x(6..text.len()), Some((30.0, 38.0)));
    assert_eq!(boundaries.span_x(4..4), None, "a collapsed selection has no highlight");
}

/// Pointer x snaps to the nearest cluster boundary, the earlier one on a tie,
/// and never to a byte inside a cluster.
#[test]
fn nearest_boundary_snaps_to_cluster_edges() {
    let text = "e\u{301}中b";
    let boundaries = FieldBoundaries::from_advances(text, &[(0, 10.0), (3, 20.0), (6, 8.0)]);

    assert_eq!(boundaries.nearest_boundary(-5.0), 0);
    assert_eq!(boundaries.nearest_boundary(4.9), 0);
    assert_eq!(boundaries.nearest_boundary(5.0), 0, "tie keeps the earlier boundary");
    assert_eq!(boundaries.nearest_boundary(5.1), 3);
    assert_eq!(boundaries.nearest_boundary(25.0), 6);
    assert_eq!(boundaries.nearest_boundary(100.0), text.len());
}

/// A long query scrolls just enough to keep the whole caret visible, and the
/// selection highlight is clipped to the field with the same scroll.
#[test]
fn long_field_scrolls_caret_and_clips_selection() {
    let label = "abcdefghij";
    let boundaries = uniform_boundaries(label, 10.0);
    let clip = FieldRect { x: 100.0, y: 5.0, w: 45.0, h: 20.0 };
    let place = placement(FieldKind::Palette, clip);

    let at_end = stateless_field(place, &boundaries, &plain_text(label, 10, None), 10, None, 1);
    // prefix 100 + fallback 6 - visible 45 = scroll 61.
    assert_close(at_end.text_x, 39.0, "scrolled origin");
    assert_close(at_end.caret.x, 139.0, "caret stays fully inside the clip");
    assert_close(at_end.caret.right(), clip.right(), "caret touches the right edge");

    let tail = stateless_field(
        place,
        &boundaries,
        &plain_text(label, 10, Some(8..10)),
        10,
        Some(8..10),
        1,
    );
    let tail_rect = tail.selection.expect("visible selection");
    assert_close(tail_rect.x, 119.0, "tail selection left");
    assert_close(tail_rect.w, 20.0, "tail selection width");

    let crossing =
        stateless_field(place, &boundaries, &plain_text(label, 10, Some(3..7)), 10, Some(3..7), 1);
    let crossing_rect = crossing.selection.expect("selection crossing the left edge");
    assert_close(crossing_rect.x, clip.x, "clipped at the field's left edge");
    assert_close(crossing_rect.w, 9.0, "only the visible part is highlighted");

    let hidden =
        stateless_field(place, &boundaries, &plain_text(label, 10, Some(0..2)), 10, Some(0..2), 1);
    assert_eq!(hidden.selection, None, "a scrolled-away selection paints nothing");

    let at_start = stateless_field(place, &boundaries, &plain_text(label, 0, None), 0, None, 1);
    assert_close(at_start.text_x, clip.x, "no scroll with the caret at the start");
    assert_close(at_start.caret.w, 10.0, "caret covers its cluster");
}

/// Repainting a reverse drag inside the visible tail must preserve both the
/// text origin and the byte under a stationary pointer.
#[test]
fn reverse_drag_redraw_retains_presented_origin_and_hit() {
    let label = "abcdefghij";
    let boundaries = uniform_boundaries(label, 10.0);
    let clip = FieldRect { x: 100.0, y: 5.0, w: 45.0, h: 20.0 };
    let place = placement(FieldKind::Palette, clip);
    let presented = stateless_field(place, &boundaries, &plain_text(label, 10, None), 10, None, 1);
    let pointer_x = 120.0;
    let offset = presented.hit_offset(&boundaries, pointer_x).expect("visible caret");
    assert_eq!(offset, 8);
    let dragged = plain_text(label, offset, Some(offset..10));
    let redrawn = plan_field(
        place,
        &boundaries,
        &dragged,
        offset,
        dragged.selection.clone(),
        1,
        Some(&presented),
    );
    assert_eq!(redrawn.text_x, presented.text_x, "reverse drag must not move visible text");
    assert_eq!(redrawn.hit_offset(&boundaries, pointer_x), Some(offset));
    assert!(redrawn.selection.is_some(), "the selected tail must remain visible");
}

/// Clipped edge clusters cannot pull the viewport along with an outside drag;
/// only boundaries whose entire effective caret fits are eligible.
#[test]
fn partial_edge_hits_snap_to_fully_visible_carets() {
    let label = "abcdefghij";
    let boundaries = uniform_boundaries(label, 10.0);
    let clip = FieldRect { x: 100.0, y: 5.0, w: 45.0, h: 20.0 };
    let mut shown = stateless_field(
        placement(FieldKind::Palette, clip),
        &boundaries,
        &plain_text(label, 10, None),
        10,
        None,
        1,
    );
    assert_eq!(
        shown.hit_offset(&boundaries, -100.0),
        Some(7),
        "byte 6 is partly hidden on the left"
    );
    shown.text_x = 75.0;
    assert_eq!(
        shown.hit_offset(&boundaries, 999.0),
        Some(6),
        "byte 7's caret is outside the right edge"
    );
}

/// A positive but narrow clip between two shaped starts offers no full caret;
/// neither presses nor drags may invent an offset, including in a zero-area clip.
#[test]
fn field_hit_without_a_fitting_boundary_is_unavailable() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let text = plain_text("WW", 0, None);
    let boundaries = FieldBoundaries::shape(&stack, &text.label, 15.0, 15.0).unwrap();
    let clip = FieldRect { x: 100.0, y: 0.0, w: 1.0, h: 10.0 };
    let mut shown =
        stateless_field(placement(FieldKind::Palette, clip), &boundaries, &text, 0, None, 1);
    shown.text_x = 100.0 - boundaries.caret_x(1) * 0.5;
    for width in [1.0, 0.0] {
        shown.placement.clip.w = width;
        for mode in [FieldHitMode::Press, FieldHitMode::Drag] {
            assert_eq!(
                field_hit(Some(&shown), &text, 1, Some(&stack), (100.5, 5.0), mode),
                FieldHit::Unavailable,
                "clip width {width}, mode {mode:?}"
            );
        }
    }
}

/// Fractional placements and advances must not accumulate origin or hit drift
/// after repeated redraws, even at either outside edge and at an exact-fill caret.
#[test]
fn repeated_hits_keep_fractional_and_wide_viewports_stable() {
    let label = "a中▏bcdef";
    let boundaries = FieldBoundaries::from_advances(
        label,
        &[(0, 7.3), (1, 22.7), (4, 5.1), (7, 9.2), (8, 8.1), (9, 7.7), (10, 8.3)],
    );
    for clip_width in [3.1, 22.7, 45.3] {
        for clip_left in [0.1, 100.3, 601.7, 100_000.3] {
            let clip = FieldRect { x: clip_left, y: 0.0, w: clip_width, h: 20.0 };
            let place = placement(FieldKind::Palette, clip);
            for caret in [0, 1, 4, 7, label.len()] {
                let text = plain_text(label, caret, None);
                let initial = stateless_field(place, &boundaries, &text, caret, None, 1);
                for pointer in [clip.x - 100.0, clip.x + clip.w * 0.5, clip.right() + 100.0] {
                    let mut shown = initial;
                    let Some(offset) = shown.hit_offset(&boundaries, pointer) else { continue };
                    for _ in 0..8 {
                        let selected =
                            plain_text(label, offset, Some(offset.min(caret)..offset.max(caret)));
                        let next = plan_field(
                            place,
                            &boundaries,
                            &selected,
                            offset,
                            selected.selection.clone(),
                            1,
                            Some(&shown),
                        );
                        assert_eq!(
                            next.text_x.to_bits(),
                            initial.text_x.to_bits(),
                            "origin changed at {clip:?}, caret {caret}, hit {offset}"
                        );
                        assert_eq!(next.hit_offset(&boundaries, pointer), Some(offset));
                        shown = next;
                    }
                }
            }
        }
    }
}

/// Even fractional exact-fill carets expose their own boundary to hit testing;
/// a positive clip must not lose that boundary to arithmetic roundoff.
#[test]
fn fractional_exact_fill_caret_remains_hittable() {
    let label = "abcdefghij";
    let boundaries = uniform_boundaries(label, 7.3);
    for clip_left in [0.1, 100.3, 601.7, 100_000.3] {
        let clip = FieldRect { x: clip_left, y: 0.0, w: 3.1, h: 20.0 };
        for caret in 0..=label.len() {
            let shown = stateless_field(
                placement(FieldKind::Palette, clip),
                &boundaries,
                &plain_text(label, caret, None),
                caret,
                None,
                1,
            );
            assert_eq!(
                shown.hit_offset(&boundaries, shown.caret.x),
                Some(caret),
                "{clip:?}, caret {caret}: {shown:?}"
            );
            let next = plan_field(
                shown.placement,
                &boundaries,
                &plain_text(label, caret, None),
                caret,
                None,
                1,
                Some(&shown),
            );
            assert_eq!(next.text_x.to_bits(), shown.text_x.to_bits(), "exact-fill origin retained");
        }
    }
}

/// Only the two exact planner identities normalize edge roundoff: a single ULP
/// beyond both, nonfinite geometry, and empty clips remain ineligible.
#[test]
fn roundoff_fit_rejects_clipped_and_invalid_edges() {
    let label = "ab";
    let boundaries = uniform_boundaries(label, 7.3);
    for clip_left in [0.1, 100.3, 601.7, 100_000.3] {
        let clip = FieldRect { x: clip_left, y: 0.0, w: 3.1, h: 20.0 };
        for caret in [1, label.len()] {
            let text = plain_text(label, caret, None);
            let base = stateless_field(
                placement(FieldKind::Palette, clip),
                &boundaries,
                &text,
                caret,
                None,
                1,
            );
            let left_origin = clip.x - boundaries.caret_x(caret);
            let right_origin = (clip.right() - clip.w) - boundaries.caret_x(caret);
            for origin in [left_origin, right_origin] {
                let snapped = FieldGeometry { text_x: origin, ..base };
                assert_eq!(
                    snapped.hit_offset(&boundaries, clip.x),
                    Some(caret),
                    "{clip:?}, origin {origin}"
                );
                let next =
                    plan_field(base.placement, &boundaries, &text, caret, None, 1, Some(&snapped));
                assert_eq!(next.text_x.to_bits(), origin.to_bits());
            }
            for shifted in [
                left_origin.min(right_origin).next_down(),
                left_origin.max(right_origin).next_up(),
                f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
            ] {
                let invalid = FieldGeometry { text_x: shifted, ..base };
                assert_eq!(invalid.hit_offset(&boundaries, clip.x), None, "origin {shifted}");
            }
            for invalid_clip in [
                FieldRect { w: 0.0, ..clip },
                FieldRect { h: 0.0, ..clip },
                FieldRect { h: f32::INFINITY, ..clip },
                FieldRect { y: f32::NAN, ..clip },
            ] {
                let invalid = FieldGeometry {
                    placement: placement(FieldKind::Palette, invalid_clip),
                    ..base
                };
                assert_eq!(invalid.hit_offset(&boundaries, clip.x), None, "{invalid_clip:?}");
            }
        }
    }
}

/// The endpoint bound accepts the planner's representable tail origin, never
/// one ULP beyond it; short fields accept only zero scroll.
#[test]
fn prior_endpoint_bound_accepts_planned_origin_but_not_overscroll() {
    let label = "abcdefghij";
    let boundaries = uniform_boundaries(label, 7.3);
    let place = placement(FieldKind::Palette, FieldRect { x: 601.7, y: 0.0, w: 22.7, h: 20.0 });
    let base = stateless_field(place, &boundaries, &plain_text(label, 10, None), 10, None, 1);
    let text = plain_text(label, 8, None);
    let retained = plan_field(place, &boundaries, &text, 8, None, 1, Some(&base));
    assert_eq!(
        retained.text_x.to_bits(),
        base.text_x.to_bits(),
        "tail bound preserves planned origin"
    );
    let overscrolled = FieldGeometry { text_x: base.text_x.next_down(), ..base };
    assert_eq!(
        plan_field(place, &boundaries, &text, 8, None, 1, Some(&overscrolled)),
        stateless_field(place, &boundaries, &text, 8, None, 1)
    );
    let short = plain_text("ab", 1, None);
    let short_boundaries = uniform_boundaries("ab", 7.3);
    let base = stateless_field(place, &short_boundaries, &short, 1, None, 1);
    assert_eq!(base.text_x, place.clip.x);
    assert_eq!(
        plan_field(place, &short_boundaries, &short, 1, None, 1, Some(&base)).text_x,
        base.text_x
    );
    let invalid = FieldGeometry { text_x: base.text_x - 0.002, ..base };
    assert_eq!(
        plan_field(place, &short_boundaries, &short, 1, None, 1, Some(&invalid)).text_x,
        base.text_x
    );
}

/// Keyboard movement retains the viewport until an edge is crossed, then moves
/// just enough to reveal the caret; narrow clips use the trimmed block width.
#[test]
fn keyboard_scroll_changes_only_at_caret_edges() {
    let label = "abcdefghij";
    let boundaries = uniform_boundaries(label, 10.0);
    let clip = FieldRect { x: 100.0, y: 0.0, w: 45.0, h: 20.0 };
    let place = placement(FieldKind::Palette, clip);
    let mut shown = stateless_field(place, &boundaries, &plain_text(label, 10, None), 10, None, 1);
    for (caret, origin) in
        [(9, 39.0), (7, 39.0), (6, 40.0), (5, 50.0), (9, 45.0), (10, 39.0), (0, 100.0)]
    {
        shown = plan_field(
            place,
            &boundaries,
            &plain_text(label, caret, None),
            caret,
            None,
            1,
            Some(&shown),
        );
        assert_eq!(shown.text_x, origin, "caret {caret}");
    }
    let narrow = placement(FieldKind::Palette, FieldRect { w: 3.0, ..clip });
    for caret in [0, 1, 10] {
        let shown =
            stateless_field(narrow, &boundaries, &plain_text(label, caret, None), caret, None, 1);
        assert_eq!(shown.caret.w, 3.0);
        assert_eq!(shown.hit_offset(&boundaries, clip.x), Some(caret), "exact-fill caret {caret}");
    }
}

/// Reusing a scalar origin requires the entire content/environment and placement
/// identity; malformed origins and placeholder text are never trusted.
#[test]
fn prior_origin_requires_matching_identity_and_bounded_scroll() {
    let label = "abcdefghij";
    let boundaries = uniform_boundaries(label, 10.0);
    let clip = FieldRect { x: 100.0, y: 0.0, w: 45.0, h: 20.0 };
    let place = placement(FieldKind::Palette, clip);
    let prior = stateless_field(place, &boundaries, &plain_text(label, 10, None), 10, None, 1);
    let text = plain_text(label, 8, Some(8..10));
    let expect_reset =
        |place: FieldPlacement, text: &FieldText, environment, prior: &FieldGeometry| {
            assert_eq!(
                plan_field(
                    place,
                    &boundaries,
                    text,
                    text.caret,
                    text.selection.clone(),
                    environment,
                    Some(prior)
                ),
                stateless_field(
                    place,
                    &boundaries,
                    text,
                    text.caret,
                    text.selection.clone(),
                    environment
                )
            );
        };
    for invalid_x in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 101.0, 38.0] {
        expect_reset(place, &text, 1, &FieldGeometry { text_x: invalid_x, ..prior });
    }
    let mut changed = text.clone();
    changed.label = "abcdefghik".to_string();
    expect_reset(place, &changed, 1, &prior);
    changed = text.clone();
    changed.composing = true;
    expect_reset(place, &changed, 1, &prior);
    changed = text.clone();
    changed.variant = 1;
    expect_reset(place, &changed, 1, &prior);
    changed = text.clone();
    changed.content = 1..10;
    expect_reset(place, &changed, 1, &prior);
    changed = text.clone();
    changed.kind = FieldKind::Search;
    expect_reset(place, &changed, 1, &prior);
    expect_reset(place, &text, 2, &prior);
    for changed_place in [
        FieldPlacement { kind: FieldKind::Search, ..place },
        FieldPlacement { clip: FieldRect { x: 101.0, ..clip }, ..place },
        FieldPlacement { clip: FieldRect { w: 44.0, ..clip }, ..place },
        FieldPlacement { hit_area: FieldRect { w: 46.0, ..clip }, ..place },
        FieldPlacement { caret_y: 1.0, ..place },
        FieldPlacement { caret_h: 19.0, ..place },
        FieldPlacement { caret_fallback_w: 7.0, ..place },
        FieldPlacement { font_size_px: 16.0, ..place },
        FieldPlacement { native_em_px: 16.0, ..place },
    ] {
        expect_reset(changed_place, &text, 1, &prior);
    }
    let empty = plain_text("", 0, None);
    let placeholder_prior =
        FieldGeometry { content_hash: empty.content_hash(1), text_x: 80.0, ..prior };
    expect_reset(place, &empty, 1, &placeholder_prior);
}

/// A failed candidate cannot become the next drag's viewport; only settlement
/// of a successful presentation changes the scalar retained origin.
#[test]
fn failed_present_keeps_scroll_origin_for_next_drag() {
    let label = "abcdefghij";
    let boundaries = uniform_boundaries(label, 10.0);
    let place = placement(FieldKind::Palette, FieldRect { x: 100.0, y: 0.0, w: 45.0, h: 20.0 });
    let old = stateless_field(place, &boundaries, &plain_text(label, 10, None), 10, None, 1);
    let mut presented = PresentedFields { palette: Some(old), search: None };
    let candidate = plan_field(
        place,
        &boundaries,
        &plain_text(label, 0, None),
        0,
        None,
        1,
        presented.palette.as_ref(),
    );
    presented.settle(PresentedFields { palette: Some(candidate), search: None }, false);
    let next = plan_field(
        place,
        &boundaries,
        &plain_text(label, 8, Some(8..10)),
        8,
        Some(8..10),
        1,
        presented.palette.as_ref(),
    );
    assert_eq!(next.text_x, old.text_x);
    assert!(next.selection.is_some());
    presented.settle(PresentedFields { palette: Some(candidate), search: None }, true);
    let after = plan_field(
        place,
        &boundaries,
        &plain_text(label, 2, None),
        2,
        None,
        1,
        presented.palette.as_ref(),
    );
    assert_eq!(after.text_x, candidate.text_x);
}

/// Search endpoint width comes from the fallback, never the counter's cluster;
/// a changed counter is changed label content and resets retained scroll.
#[test]
fn search_counter_resets_origin_and_endpoint_uses_fallback() {
    let label = "/ abcdefghij · 0/0";
    let boundaries = uniform_boundaries(label, 10.0);
    let mut text = plain_text(label, 12, None);
    text.kind = FieldKind::Search;
    text.content = 2..12;
    let place = placement(FieldKind::Search, FieldRect { x: 100.0, y: 0.0, w: 45.0, h: 20.0 });
    let shown = stateless_field(place, &boundaries, &text, 12, None, 1);
    assert_eq!(shown.caret.w, place.caret_fallback_w);
    assert_eq!(shown.hit_offset(&boundaries, 999.0), Some(10));
    text.caret = 10;
    let retained = plan_field(place, &boundaries, &text, 10, None, 1, Some(&shown));
    assert_eq!(retained.text_x, shown.text_x);
    text.label = "/ abcdefghij · 1/1".to_string();
    let reset = plan_field(place, &boundaries, &text, 10, None, 1, Some(&shown));
    assert_eq!(reset.text_x, stateless_field(place, &boundaries, &text, 10, None, 1).text_x);
    assert_ne!(reset.text_x, shown.text_x);
}

/// An empty field keeps its caret at the clip start with the fallback width,
/// and a zero-width clip never produces a selection.
#[test]
fn empty_field_and_zero_width_clip_are_degenerate_safely() {
    let boundaries = FieldBoundaries::from_advances("", &[]);
    let clip = FieldRect { x: 20.0, y: 0.0, w: 80.0, h: 10.0 };
    let empty = stateless_field(
        placement(FieldKind::Palette, clip),
        &boundaries,
        &plain_text("", 0, None),
        0,
        None,
        3,
    );
    assert_close(empty.caret.x, 20.0, "empty caret x");
    assert_close(empty.caret.w, 6.0, "fallback caret width");
    assert_eq!(empty.hit_offset(&boundaries, 90.0), Some(0));

    let narrow = FieldRect { x: 20.0, y: 0.0, w: 0.0, h: 10.0 };
    let text = uniform_boundaries("ab", 10.0);
    let squeezed = stateless_field(
        placement(FieldKind::Palette, narrow),
        &text,
        &plain_text("ab", 2, Some(0..2)),
        2,
        Some(0..2),
        3,
    );
    assert_eq!(squeezed.selection, None);
    assert!(squeezed.caret.x.is_finite());
}

/// The caret and highlight never extend past a field narrower or shorter than
/// them: both are intersected with the clip, so the native IME caret rectangle
/// stays inside the field, and an empty clip yields zero area rather than a
/// rectangle hanging outside it.
#[test]
fn caret_and_selection_stay_inside_narrow_short_or_empty_clips() {
    let label = "ab";
    let boundaries = uniform_boundaries(label, 10.0);
    let within = |rect: FieldRect, clip: FieldRect| {
        rect.x >= clip.x
            && rect.right() <= clip.right()
            && rect.y >= clip.y
            && rect.bottom() <= clip.bottom()
    };
    let clips = [
        // Narrower than both the 10 px cluster and the 6 px fallback caret.
        FieldRect { x: 20.0, y: 5.0, w: 3.0, h: 10.0 },
        FieldRect { x: 20.0, y: 5.0, w: 0.0, h: 10.0 },
        // Shorter than the planned caret.
        FieldRect { x: 20.0, y: 5.0, w: 40.0, h: 4.0 },
        FieldRect { x: 20.0, y: 5.0, w: 0.0, h: 0.0 },
    ];
    for clip in clips {
        let mut place = placement(FieldKind::Palette, clip);
        // A caret planned taller than the field, as a row shorter than its font yields.
        place.caret_y = clip.y - 3.0;
        place.caret_h = 16.0;
        for caret in [0, 1, 2] {
            let text = plain_text(label, caret, Some(0..2));
            let geometry = stateless_field(place, &boundaries, &text, caret, Some(0..2), 1);
            let rect = geometry.caret;
            let drawable = clip.w > 0.0 && clip.h > 0.0;
            assert!(rect.w >= 0.0 && rect.h >= 0.0, "{clip:?} caret {caret}: {rect:?}");
            assert!(within(rect, clip), "{clip:?} caret {caret} escapes: {rect:?}");
            // A non-empty clip still shows a caret; an empty one shows nothing.
            assert_eq!(rect.w > 0.0 && rect.h > 0.0, drawable, "{clip:?} caret {caret}: {rect:?}");
            if let Some(highlight) = geometry.selection {
                assert!(highlight.w > 0.0 && highlight.h > 0.0, "{clip:?}: {highlight:?}");
                assert!(within(highlight, clip), "{clip:?} highlight escapes: {highlight:?}");
            }
        }
    }
}

/// Search hits map only onto the query: the `/ ` prompt clamps to its start and
/// the match counter clamps to its end, and offsets are query-relative.
#[test]
fn search_hits_exclude_prompt_and_counter() {
    let label = "/ ab · 0/0";
    let boundaries = uniform_boundaries(label, 10.0);
    let text = FieldText {
        kind: FieldKind::Search,
        label: label.to_string(),
        content: 2..4,
        caret: 2,
        selection: None,
        composing: false,
        variant: 0,
    };
    let clip = FieldRect { x: 0.0, y: 0.0, w: 200.0, h: 10.0 };
    let geometry =
        stateless_field(placement(FieldKind::Search, clip), &boundaries, &text, 2, None, 1);

    assert_eq!(geometry.hit_offset(&boundaries, 0.0), Some(0), "prompt clamps to query start");
    assert_eq!(geometry.hit_offset(&boundaries, 31.0), Some(1));
    assert_eq!(geometry.hit_offset(&boundaries, 90.0), Some(2), "counter clamps to query end");
    assert_eq!(geometry.hit_offset(&boundaries, 9_999.0), Some(2), "drag past the field clamps");
}

/// A glyph crossing the clip is trimmed with its UVs so the visible part keeps
/// its texel mapping; glyphs inside are untouched and glyphs outside are dropped.
#[test]
fn clip_glyph_trims_rect_and_uv_together() {
    let (sw, sh) = (200.0, 100.0);
    // Pixel rect x=10 y=20 w=20 h=10 in the chrome NDC encoding.
    let glyph = GlyphInstance {
        rect: [-0.9, 0.4, 0.2, 0.2],
        uv: [0.1, 0.2, 0.3, 0.4],
        color: [1.0; 4],
        flags: [0.0; 4],
    };

    let clip = FieldRect { x: 15.0, y: 0.0, w: 100.0, h: 24.0 };
    let clipped = clip_glyph(glyph, clip, sw, sh).expect("partly visible glyph");
    let expected_rect = [-0.85, 0.52, 0.15, 0.08];
    let expected_uv = [0.15, 0.2, 0.3, 0.28];
    for index in 0..4 {
        assert_close(clipped.rect[index], expected_rect[index], "clipped rect");
        assert_close(clipped.uv[index], expected_uv[index], "clipped uv");
    }

    let inside = FieldRect { x: 0.0, y: 0.0, w: 200.0, h: 100.0 };
    let untouched = clip_glyph(glyph, inside, sw, sh).expect("inside glyph");
    assert_eq!(untouched.rect, glyph.rect);
    assert_eq!(untouched.uv, glyph.uv);

    let outside = FieldRect { x: 40.0, y: 0.0, w: 10.0, h: 100.0 };
    assert!(clip_glyph(glyph, outside, sw, sh).is_none());

    let mut run = vec![glyph, glyph];
    clip_glyphs_to_rect(&mut run, 1, outside, sw, sh);
    assert_eq!(run.len(), 1, "only glyphs from `first` onward are clipped");
}

/// Content identity ignores caret and selection; state identity includes them;
/// both change with the environment stamp.
#[test]
fn hashes_separate_content_from_selection_state() {
    let base = plain_text("abc", 1, None);
    let moved = plain_text("abc", 2, Some(0..2));
    assert_eq!(base.content_hash(9), moved.content_hash(9));
    assert_ne!(base.state_hash(9), moved.state_hash(9));
    assert_ne!(base.content_hash(9), base.content_hash(10));
    assert_ne!(base.content_hash(9), plain_text("abd", 1, None).content_hash(9));
}

/// A frame that was not presented never replaces the presented geometry.
#[test]
fn only_presented_frames_commit_field_geometry() {
    let boundaries = uniform_boundaries("ab", 10.0);
    let clip = FieldRect { x: 0.0, y: 0.0, w: 50.0, h: 10.0 };
    let old = stateless_field(
        placement(FieldKind::Palette, clip),
        &boundaries,
        &plain_text("ab", 0, None),
        0,
        None,
        1,
    );
    let new = stateless_field(
        placement(FieldKind::Palette, clip),
        &boundaries,
        &plain_text("ab", 2, None),
        2,
        None,
        1,
    );
    let mut presented = PresentedFields { palette: Some(old), search: None };

    presented.settle(PresentedFields { palette: Some(new), search: None }, false);
    assert_eq!(presented.palette, Some(old), "retry or failure keeps the old record");
    presented.settle(PresentedFields { palette: Some(new), search: None }, true);
    assert_eq!(presented.palette, Some(new));
    presented.settle(PresentedFields::default(), true);
    assert_eq!(presented.palette, None, "a presented frame without the field forgets it");
    presented.palette = Some(old);
    presented.clear();
    assert_eq!(presented, PresentedFields::default());
}

/// Real shaping keeps a literal `▏` in the query as a drawn cluster, and every
/// boundary of a mixed ASCII/CJK/emoji run lies on a UTF-8 edge and maps back to itself.
#[test]
fn real_shaping_keeps_literal_bar_and_unicode_boundaries() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);

    let mut palette = CommandPalette::new();
    palette.open();
    palette.set_query("a▏b");
    let text = FieldText::palette(&palette, "").expect("editable palette");
    assert_eq!(text.label, "a▏b", "the literal bar is query text, not a caret marker");
    let bar = FieldBoundaries::shape(&stack, &text.label, 15.0, 15.0).expect("shapes");
    assert!(bar.caret_x(1) > 0.0);
    assert!(bar.caret_x(1 + '▏'.len_utf8()) > bar.caret_x(1), "the bar has its own advance");

    let mixed = "a中🙂b";
    let boundaries = FieldBoundaries::shape(&stack, mixed, 15.0, 15.0).expect("shapes");
    let advances = shaped_advances(&stack, mixed, ChromeAttrs::default(), 15.0, 15.0).unwrap();
    let sum: f32 = advances.iter().map(|(_, advance)| advance).sum();
    assert_close(boundaries.total_width(), sum, "boundaries use the drawn advances");
    for &(offset, stop_x) in &boundaries.stops {
        assert!(mixed.is_char_boundary(offset), "{offset} splits a scalar");
        assert_eq!(boundaries.nearest_boundary(stop_x), offset);
    }
}

/// The end-to-end hit path maps onto the presented palette field and refuses
/// stale text or environment, live preedit, and missing shaping.
#[test]
fn palette_hit_refuses_stale_composing_or_unshaped_geometry() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut palette = CommandPalette::new();
    palette.open();
    palette.set_query("abc");
    let text = FieldText::palette(&palette, "").expect("editable palette");
    let boundaries = FieldBoundaries::shape(&stack, &text.label, 15.0, 15.0).expect("shapes");
    let clip = FieldRect { x: 10.0, y: 0.0, w: 300.0, h: 30.0 };
    let geometry = stateless_field(
        placement(FieldKind::Palette, clip),
        &boundaries,
        &text,
        text.caret,
        None,
        7,
    );
    let inside = (geometry.text_x + boundaries.caret_x(1) + 0.4, 10.0);

    let hit = |presented, text: &FieldText, environment, font, point, mode| {
        field_hit(presented, text, environment, font, point, mode)
    };
    assert_eq!(
        hit(Some(&geometry), &text, 7, Some(&stack), inside, FieldHitMode::Press),
        FieldHit::Offset(1)
    );
    assert_eq!(
        hit(Some(&geometry), &text, 8, Some(&stack), inside, FieldHitMode::Press),
        FieldHit::Stale
    );
    // A never-presented field cannot have been pressed; a drag waits for a redraw.
    assert_eq!(hit(None, &text, 7, Some(&stack), inside, FieldHitMode::Press), FieldHit::Outside);
    assert_eq!(hit(None, &text, 7, Some(&stack), inside, FieldHitMode::Drag), FieldHit::Stale);
    assert_eq!(
        hit(Some(&geometry), &text, 7, None, inside, FieldHitMode::Press),
        FieldHit::Unavailable
    );
    let below = (inside.0, 100.0);
    assert_eq!(
        hit(Some(&geometry), &text, 7, Some(&stack), below, FieldHitMode::Press),
        FieldHit::Outside
    );
    assert_eq!(
        hit(Some(&geometry), &text, 7, Some(&stack), below, FieldHitMode::Drag),
        FieldHit::Offset(1)
    );

    let composing = FieldText::palette(&palette, "x").expect("editable palette");
    assert_eq!(
        hit(Some(&geometry), &composing, 7, Some(&stack), inside, FieldHitMode::Press),
        FieldHit::Unavailable
    );

    palette.set_query("abcd");
    let edited = FieldText::palette(&palette, "").expect("editable palette");
    assert_eq!(
        hit(Some(&geometry), &edited, 7, Some(&stack), inside, FieldHitMode::Press),
        FieldHit::Stale
    );

    // A selection-only change keeps mapping against the presented frame.
    palette.select_all();
    let selected = FieldText::palette(&palette, "").expect("editable palette");
    let reselected = FieldText {
        selection: selected.selection.clone(),
        caret: selected.caret,
        ..edited.clone()
    };
    let presented_edit = stateless_field(
        placement(FieldKind::Palette, clip),
        &FieldBoundaries::shape(&stack, &edited.label, 15.0, 15.0).unwrap(),
        &edited,
        edited.caret,
        None,
        7,
    );
    assert!(matches!(
        hit(Some(&presented_edit), &reselected, 7, Some(&stack), inside, FieldHitMode::Drag),
        FieldHit::Offset(_)
    ));
    assert_eq!(
        field_caret_rect(Some(&presented_edit), &reselected, 7),
        None,
        "caret rect needs the exact state"
    );
    assert_eq!(field_caret_rect(Some(&presented_edit), &edited, 7), Some(presented_edit.caret));
}

/// Search field text addresses the full bar label and shifts query offsets past the prompt.
#[test]
fn search_field_text_offsets_skip_the_prompt() {
    let mut search = SearchState::new();
    search.query = "a中b".to_string();
    search.set_cursor(1);
    let text = FieldText::search(&search, "");
    let prompt = SEARCH_BAR_PROMPT.len();
    assert!(text.label.starts_with(SEARCH_BAR_PROMPT));
    assert_eq!(text.content, prompt..prompt + "a中b".len());
    assert_eq!(text.caret, prompt + 1);
    assert!(!text.composing);
    assert!(FieldText::search(&search, "漢").composing);
}
