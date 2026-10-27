//! Behavior contracts for fit status presentation.

use super::*;
use crate::i18n::test_translator as translator;
use crate::status_line::status_line_tests::*;
use sonicterm_cfg::status_line::ResolvedSegment;

/// A view of named single-part segments for fitting tests.
fn fit_view(segments: &[(StatusSegmentId, StatusSide, u16, bool, &str)]) -> StatusLineView {
    StatusLineView::new(
        segments
            .iter()
            .map(|(id, side, priority, protected, text)| StatusSegmentView {
                id: *id,
                side: *side,
                priority: *priority,
                protected: *protected,
                parts: vec![StatusPart::new(*text, StatusTone::Value)],
                compact: None,
                click: None,
                spoken: SpokenSegment::default(),
            })
            .collect(),
    )
}

/// Ids of the presented segments.
fn presented(view: &StatusLineView, placed: &[PlacedSegment]) -> Vec<StatusSegmentId> {
    placed.iter().map(|segment| view.segments[segment.index].id).collect()
}

#[test]
fn fitting_drops_whole_segments_by_priority_down_to_empty() {
    // Narrowing drops lowest priority first, protected last, and never overflows.
    use StatusSegmentId as Seg;
    let view = fit_view(&[
        (Seg::Mode, StatusSide::Left, 800, false, "COPY"),
        (Seg::Scroll, StatusSide::Left, 400, false, "Scroll: ↑ 3"),
        (Seg::Health, StatusSide::Right, 950, true, "Config: ✗ line 1"),
        (Seg::Mem, StatusSide::Right, 900, false, "Mem: 1 MB"),
    ]);
    let full = 4.0 + 1.0 + 11.0 + 1.0 + 16.0 + 1.0 + 9.0;
    assert_eq!(presented(&view, &fit(&view, &tight(full), columns)).len(), 4);
    assert_eq!(
        presented(&view, &fit(&view, &tight(full - 1.0), columns)),
        [Seg::Mode, Seg::Health, Seg::Mem]
    );
    assert_eq!(
        presented(&view, &fit(&view, &tight(16.0 + 1.0 + 9.0), columns)),
        [Seg::Health, Seg::Mem]
    );
    assert_eq!(
        presented(&view, &fit(&view, &tight(16.0), columns)),
        [Seg::Health],
        "protected outlives priority 900"
    );
    assert!(fit(&view, &tight(3.0), columns).is_empty());
    for width in 0..60 {
        let placed = fit(&view, &tight(width as f32), columns);
        for segment in &placed {
            assert!(
                segment.left >= 0.0 && segment.left + segment.width <= width as f32 + 0.001,
                "width {width}"
            );
        }
    }
}

#[test]
fn equal_priorities_drop_the_segment_nearest_the_center_first() {
    // Ties go to the segment nearest the center: the center list, then the inner side segments.
    use StatusSegmentId as Seg;
    let view = fit_view(&[
        (Seg::Mode, StatusSide::Left, 500, false, "aaaa"),
        (Seg::Scroll, StatusSide::Left, 500, false, "bbbb"),
        (Seg::Link, StatusSide::Center, 500, false, "cccc"),
        (Seg::Mem, StatusSide::Right, 500, false, "dddd"),
    ]);
    assert_eq!(
        presented(&view, &fit(&view, &tight(14.0), columns)),
        [Seg::Mode, Seg::Scroll, Seg::Mem]
    );
    assert_eq!(presented(&view, &fit(&view, &tight(9.0), columns)), [Seg::Mode, Seg::Mem]);
}

#[test]
fn compact_forms_and_ellipsis_precede_dropping() {
    // A compact form is used before any drop; a text value shortens in the middle before it goes.
    use StatusSegmentId as Seg;
    let mut view = fit_view(&[(Seg::Mem, StatusSide::Right, 900, false, "Mem: 1 MB ↑")]);
    view.segments[0].compact = Some(vec![StatusPart::new("Mem: 1 MB", StatusTone::Value)]);
    let placed = fit(&view, &tight(9.0), columns);
    assert_eq!(placed[0].parts[0].text, "Mem: 1 MB");
    let mut link = fit_view(&[(Seg::Link, StatusSide::Center, 600, true, "Link: ")]);
    link.segments[0].parts.push(StatusPart::new("github.com", StatusTone::Value));
    link.segments[0]
        .parts
        .push(StatusPart::new("/D0n9X1n/SonicTerm/pull/1670", StatusTone::Value).ellipsized());
    let placed = fit(&link, &tight(30.0), columns);
    let text: String = placed[0].parts.iter().map(|part| part.text.as_str()).collect();
    assert!(text.starts_with("Link: github.com/") && text.contains('…'), "{text}");
    assert!(placed[0].width <= 30.0);
    assert!(fit(&link, &tight(18.0), columns).is_empty(), "the host is never cut");
}

#[test]
fn center_is_centered_and_shifts_between_the_sides() {
    // Center content sits in the middle until a side group pushes it aside.
    use StatusSegmentId as Seg;
    let view = fit_view(&[(Seg::Link, StatusSide::Center, 600, false, "xxxx")]);
    assert_eq!(fit(&view, &tight(20.0), columns)[0].left, 8.0);
    let crowded = fit_view(&[
        (Seg::Mode, StatusSide::Left, 800, false, "aaaaaaaaaa"),
        (Seg::Link, StatusSide::Center, 600, false, "xxxx"),
    ]);
    let placed = fit(&crowded, &tight(20.0), columns);
    assert_eq!(placed[1].left, 11.0);
}

#[test]
fn chips_measure_as_their_text() {
    // A chip has no background, so it takes exactly its text's width, like any value; the space
    // part between two chips is the only thing that separates them.
    let mut view = fit_view(&[(StatusSegmentId::Mode, StatusSide::Left, 800, false, "COPY")]);
    view.segments[0].parts[0].tone = StatusTone::Chip;
    view.segments[0].parts.push(StatusPart::new(" ", StatusTone::Name));
    view.segments[0].parts.push(StatusPart::new("MOUSE", StatusTone::Chip));
    let metrics =
        FitMetrics { width: 100.0, edge_pad: 2.0, segment_gap: 3.0, separator_width: 0.0 };
    let placed = fit(&view, &metrics, columns);
    assert_eq!(
        placed[0].parts[0],
        PlacedPart { text: "COPY".into(), tone: StatusTone::Chip, left: 2.0, width: 4.0 }
    );
    assert_eq!((placed[0].parts[2].left, placed[0].parts[2].width), (7.0, 5.0));
    assert_eq!(placed[0].width, 10.0);
}

#[test]
fn width_ellipsis_keeps_grapheme_boundaries() {
    // Middle ellipsis never splits a grapheme and stops at the minimum kept length.
    let mut measure = columns;
    assert_eq!(ellipsize_middle_to_width("abcdefghij", 7.0, &mut measure), "abc…hij");
    assert_eq!(ellipsize_middle_to_width("abcdefghij", 1.0, &mut measure), "ab…ij");
    assert_eq!(ellipsize_middle_to_width("short", 1.0, &mut measure), "short");
    let combined = "e\u{301}".repeat(10);
    let cut = ellipsize_middle_to_width(&combined, 7.0, &mut measure);
    assert!(cut.graphemes(true).all(|grapheme| grapheme == "e\u{301}" || grapheme == "…"), "{cut}");
}

/// Two segments on each side and one in the center, with a 2-column gap and a 1-column `|`.
fn separated_view() -> (StatusLineView, FitMetrics) {
    use StatusSegmentId as Seg;
    let view = fit_view(&[
        (Seg::Mode, StatusSide::Left, 800, false, "aa"),
        (Seg::Scroll, StatusSide::Left, 400, false, "bb"),
        (Seg::Link, StatusSide::Center, 600, false, "cc"),
        (Seg::Health, StatusSide::Right, 950, false, "dd"),
        (Seg::Mem, StatusSide::Right, 900, false, "ee"),
    ]);
    (view, FitMetrics { width: 40.0, edge_pad: 0.0, segment_gap: 2.0, separator_width: 1.0 })
}

/// Each separator is a `|` in the name tone that sits between two presented segments of one group
/// and overlaps neither; returns the side of each, for counting.
fn separator_sides(
    view: &StatusLineView,
    placed: &[PlacedSegment],
    found: &[PlacedPart],
) -> Vec<StatusSide> {
    found
        .iter()
        .map(|separator| {
            assert_eq!((separator.text.as_str(), separator.tone), ("|", StatusTone::Name));
            let center = separator.left + separator.width / 2.0;
            let before =
                placed.iter().filter(|segment| segment.left + segment.width <= separator.left);
            let after =
                placed.iter().filter(|segment| segment.left >= separator.left + separator.width);
            let before = before.max_by(|first, second| first.left.total_cmp(&second.left)).unwrap();
            let after = after.min_by(|first, second| first.left.total_cmp(&second.left)).unwrap();
            let side = view.segments[before.index].side;
            assert_eq!(side, view.segments[after.index].side, "a separator never joins two groups");
            let gap_center = (before.left + before.width + after.left) / 2.0;
            assert!(
                (center - gap_center).abs() < 1e-4,
                "centered in its gap: {center} {gap_center}"
            );
            side
        })
        .collect()
}

/// A `|` sits between adjacent shown segments of a group, centered in their gap; none at a group
/// edge or between groups, and a group of one has none.
#[test]
fn separators_sit_only_between_adjacent_segments_of_a_group() {
    let (view, metrics) = separated_view();
    let placed = fit(&view, &metrics, columns);
    assert_eq!(placed.len(), 5);
    let found = separators(&view, &placed, &metrics);
    assert_eq!(separator_sides(&view, &placed, &found), [StatusSide::Left, StatusSide::Right]);
    // Left: `aa` 0..2, the 3-column gap holds `|` at 3, `bb` 5..7.
    assert_eq!((found[0].left, placed[1].left), (3.0, 5.0));
}

/// Fitting counts each separator, so dropping a segment drops its separator and leaves no doubled
/// or dangling `|`.
#[test]
fn dropping_a_segment_drops_its_separator() {
    let (view, mut metrics) = separated_view();
    // Everything needs 7 + 2 + 7 columns plus two 2-column gaps between groups: 20.
    metrics.width = 20.0;
    assert_eq!(fit(&view, &metrics, columns).len(), 5, "the separators fit exactly");
    metrics.width = 19.0;
    let placed = fit(&view, &metrics, columns);
    use StatusSegmentId as Seg;
    assert_eq!(presented(&view, &placed), [Seg::Mode, Seg::Link, Seg::Health, Seg::Mem]);
    let found = separators(&view, &placed, &metrics);
    assert_eq!(
        separator_sides(&view, &placed, &found),
        [StatusSide::Right],
        "left has one segment"
    );
}

/// A separator is no segment's hit box: a press on it finds nothing.
#[test]
fn a_press_on_a_separator_hits_no_segment() {
    let (view, metrics) = separated_view();
    let placed = fit(&view, &metrics, columns);
    for separator in separators(&view, &placed, &metrics) {
        let center = separator.left + separator.width / 2.0;
        let hit = placed
            .iter()
            .find(|segment| center >= segment.left && center < segment.left + segment.width);
        assert_eq!(hit, None, "{center}");
    }
}

/// A segment that fitting empties is not shown: `mode` with only `MOUSE` loses it to its compact
/// form, and then neither a `|` nor a separator gap is left before `Mem: 412 MB`.
#[test]
fn a_segment_fitting_empties_takes_no_separator() {
    use StatusSegmentId as Seg;
    let mut resolved = developer();
    resolved.segments = [Seg::Mode, Seg::Mem]
        .into_iter()
        .map(|id| ResolvedSegment {
            id,
            side: StatusSide::Left,
            show: if id == Seg::Mem { ShowMode::Always } else { ShowMode::Active },
            priority: id.meta().default_priority,
            click: ClickBinding::Default,
        })
        .collect();
    let inputs = StatusLineInputs {
        mode: ModeInput { mouse_captured: true, ..Default::default() },
        ..idle_inputs()
    };
    let view = build_view(&resolved, false, &inputs, &translator("en"), NumberStyle::POINT);
    let metrics =
        |width| FitMetrics { width, edge_pad: 0.0, segment_gap: 2.0, separator_width: 1.0 };
    let wide = fit(&view, &metrics(19.0), columns);
    assert_eq!(presented(&view, &wide), [Seg::Mode, Seg::Mem], "MOUSE | Mem: 412 MB fits in 19");
    assert_eq!(separators(&view, &wide, &metrics(19.0)).len(), 1);
    let narrow = fit(&view, &metrics(14.0), columns);
    assert_eq!(presented(&view, &narrow), [Seg::Mem], "the emptied mode is not shown");
    assert!(separators(&view, &narrow, &metrics(14.0)).is_empty(), "no dangling `|`");
    assert_eq!(narrow[0].left, 0.0, "no separator width reserved before Mem");
}
