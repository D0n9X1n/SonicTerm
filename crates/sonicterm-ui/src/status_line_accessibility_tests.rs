//! Behavior contracts for accessibility status presentation.

use super::*;
use crate::i18n::test_translator as translator;
use crate::i18n::SHIPPED_LOCALES;
use crate::status_line::status_line_tests::*;
use sonicterm_cfg::status_line::StatusLineConfig;

/// Committed hit boxes for `ids`, laid out left to right, 100 px apart and 80 px wide; `clickable`
/// is taken from the view, as the presented strip records it.
fn committed(view: &StatusLineView, ids: &[StatusSegmentId]) -> Vec<CommittedSegment> {
    ids.iter()
        .enumerate()
        .map(|(index, id)| CommittedSegment {
            id: *id,
            left_px: index as f32 * 100.0,
            width: 80.0,
            clickable: view
                .segments
                .iter()
                .any(|segment| segment.id == *id && segment.click.is_some()),
        })
        .collect()
}

/// The accessible strip in English for `committed` segments of `view`.
fn accessible(view: &StatusLineView, committed: &[CommittedSegment]) -> AccessibleStrip {
    accessible_strip(committed, view, false, &translator("en"))
}

#[test]
fn every_catalog_variant_is_listed_and_has_an_accessible_name_in_every_locale() {
    // A new segment id does not compile until this match lists it, and the listed ids are exactly
    // `ALL`, so every catalog segment in the build has a spoken name, enforced per locale.
    let listed = |id: StatusSegmentId| match id {
        StatusSegmentId::Alerts
        | StatusSegmentId::Health
        | StatusSegmentId::Mem
        | StatusSegmentId::Mode
        | StatusSegmentId::Input
        | StatusSegmentId::Clip
        | StatusSegmentId::PathCheck
        | StatusSegmentId::Link
        | StatusSegmentId::Paste
        | StatusSegmentId::Sel
        | StatusSegmentId::Ime
        | StatusSegmentId::Scroll
        | StatusSegmentId::Cpu
        | StatusSegmentId::Frame
        | StatusSegmentId::Out
        | StatusSegmentId::TabMem
        | StatusSegmentId::Glyphs
        | StatusSegmentId::History
        | StatusSegmentId::Font
        | StatusSegmentId::Zoom
        | StatusSegmentId::Size => true,
    };
    assert!(StatusSegmentId::ALL.iter().all(|id| listed(*id)));
    for locale in SHIPPED_LOCALES {
        let i18n = translator(locale);
        for key in
            ["status-accessible-strip", "status-accessible-warning", "status-accessible-error"]
        {
            assert_ne!(i18n.translate(key), key, "{locale} {key}");
        }
        for id in StatusSegmentId::ALL {
            let name = i18n.translate(id.meta().spoken_name_key);
            assert!(!name.trim().is_empty(), "{locale} {id:?}");
        }
    }
}

#[test]
fn names_are_plain_words_with_warning_cues_and_no_sparkline() {
    // A segment's name is its spoken name and value; a warning says so in words, and the
    // diagnostics sparkline never reaches a name.
    let i18n = translator("en");
    let mut resolved = StatusLineConfig {
        preset: sonicterm_cfg::status_line::StatusPreset::Diagnostics,
        ..Default::default()
    }
    .resolve()
    .unwrap();
    resolved.mem.sparkline = true;
    let mut history = [None; SPARK_CELLS];
    history[6] = Some(400 * MIB);
    history[7] = Some(412 * MIB);
    let inputs = StatusLineInputs {
        mem: MemInput { bytes: Some(412 * MIB), previous_bytes: Some(300 * MIB), history },
        health: HealthInput { render: RenderHealth::SoftwareForced, ..Default::default() },
        ..Default::default()
    };
    let view = build_view(&resolved, false, &inputs, &i18n, NumberStyle::POINT);
    assert!(
        shown(&view, StatusSegmentId::Mem).unwrap().contains('▇')
            || shown(&view, StatusSegmentId::Mem).unwrap().contains('█')
    );
    let strip =
        accessible(&view, &committed(&view, &[StatusSegmentId::Health, StatusSegmentId::Mem]));
    let name = |id| strip.segments.iter().find(|segment| segment.id == id).unwrap().name.clone();
    assert_eq!(name(StatusSegmentId::Mem), "Memory 412 MB rising");
    assert!(
        name(StatusSegmentId::Health).starts_with("Warning: Health"),
        "{}",
        name(StatusSegmentId::Health)
    );
    for segment in &strip.segments {
        assert!(
            !segment.name.chars().any(|character| "▁▂▃▄▅▆▇█↑↓⚠✗".contains(character)),
            "{}",
            segment.name
        );
    }
}

#[test]
fn sensitive_segments_are_absent_while_hidden_even_when_committed() {
    // `hide_sensitive` removes the link from the tree, not only from the screen, even before a
    // frame drawn without it is committed.
    let inputs = StatusLineInputs {
        link: Some(LinkInput::from_url("https://a.example/b")),
        ..idle_inputs()
    };
    let view = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
    let layout = committed(&view, &[StatusSegmentId::Link, StatusSegmentId::Mem]);
    let shown_strip = accessible_strip(&layout, &view, false, &translator("en"));
    assert!(shown_strip.segments.iter().any(|segment| segment.id == StatusSegmentId::Link));
    let hidden = accessible_strip(&layout, &view, true, &translator("en"));
    assert!(hidden.segments.iter().all(|segment| segment.id != StatusSegmentId::Link));
    assert_eq!(hidden.segments.len(), 1);
}

#[test]
fn structure_follows_the_committed_layout_and_values_follow_the_inputs() {
    // Structure (which segments, their order and bounds) comes only from the committed layout,
    // and each id is the segment's own, whatever its position; values come from the current view.
    let i18n = translator("en");
    let inputs = StatusLineInputs { scroll_lines: 340, ..idle_inputs() };
    let view = build_view(&developer(), false, &inputs, &i18n, NumberStyle::POINT);
    let ids = |strip: &AccessibleStrip| {
        strip.segments.iter().map(|segment| segment.id).collect::<Vec<_>>()
    };
    let ordered =
        accessible(&view, &committed(&view, &[StatusSegmentId::Scroll, StatusSegmentId::Mem]));
    assert_eq!(ids(&ordered), [StatusSegmentId::Scroll, StatusSegmentId::Mem]);
    let mut reversed = committed(&view, &[StatusSegmentId::Mem, StatusSegmentId::Scroll]);
    reversed.reverse();
    let reordered = accessible(&view, &reversed);
    assert_eq!(ids(&reordered), [StatusSegmentId::Mem, StatusSegmentId::Scroll], "visual order");
    // A fitted strip that dropped `scroll` exposes only `mem`, with the same id.
    let fitted = accessible(&view, &committed(&view, &[StatusSegmentId::Mem]));
    assert_eq!(ids(&fitted), [StatusSegmentId::Mem]);
    // A segment with a value but never drawn is absent: structure is only the committed layout.
    let never_drawn = accessible(&view, &committed(&view, &[StatusSegmentId::Mem]));
    assert!(never_drawn.segments.iter().all(|segment| segment.id != StatusSegmentId::Scroll));
    // A drawn segment the current inputs no longer format keeps its node, named by its spoken
    // name alone, so structure never depends on the latest visual view.
    let quiet = build_view(&developer(), false, &idle_inputs(), &i18n, NumberStyle::POINT);
    let stale =
        accessible(&quiet, &committed(&view, &[StatusSegmentId::Scroll, StatusSegmentId::Mem]));
    assert_eq!(ids(&stale), [StatusSegmentId::Scroll, StatusSegmentId::Mem]);
    let scroll_name = i18n.translate(StatusSegmentId::Scroll.meta().spoken_name_key);
    assert_eq!(stale.segments[0].name, scroll_name);
    // The same layout with a new memory figure changes the name and the hash.
    let rising = StatusLineInputs {
        mem: MemInput {
            bytes: Some(500 * MIB),
            previous_bytes: Some(412 * MIB),
            ..Default::default()
        },
        ..inputs
    };
    let later = build_view(&developer(), false, &rising, &i18n, NumberStyle::POINT);
    let layout = committed(&view, &[StatusSegmentId::Scroll, StatusSegmentId::Mem]);
    let before = accessible(&view, &layout);
    let after = accessible(&later, &layout);
    assert_ne!(before.segments[1].name, after.segments[1].name);
    assert_ne!(before.hash, after.hash);
}

#[test]
fn the_hash_ignores_color_and_clickable_segments_are_buttons() {
    // A color-only change keeps the hash; only clickable segments are buttons, and every segment
    // carries its bounds.
    let i18n = translator("en");
    let inputs = StatusLineInputs { scroll_lines: 3, ..idle_inputs() };
    let view = build_view(&developer(), false, &inputs, &i18n, NumberStyle::POINT);
    let layout = committed(&view, &[StatusSegmentId::Scroll, StatusSegmentId::Mem]);
    let base = accessible(&view, &layout);
    let mut recolored = view.clone();
    for segment in &mut recolored.segments {
        for part in &mut segment.parts {
            part.tone = StatusTone::Name;
        }
    }
    let recolored = StatusLineView::new(recolored.segments);
    assert_ne!(recolored.pixel_hash, view.pixel_hash);
    assert_eq!(accessible(&recolored, &layout).hash, base.hash);
    let mut inert = layout.clone();
    inert[0].clickable = false;
    let inert_strip = accessible(&view, &inert);
    assert_ne!(inert_strip.hash, base.hash, "an action change is announced");
    assert_eq!(base.segments[0].role, AccessibleRole::Button);
    assert_eq!(inert_strip.segments[0].role, AccessibleRole::Label);
    for segment in &inert_strip.segments {
        assert!(segment.width > 0.0, "{:?} has bounds", segment.id);
    }
    assert_eq!((inert_strip.segments[1].left_px, inert_strip.segments[1].width), (100.0, 80.0));
}
