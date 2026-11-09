//! The `window` segment: the native title's text, its `#N` key, redaction and click.

use super::*;
use crate::i18n::test_translator as translator;
use crate::status_line::status_line_tests::*;

/// Window #2's title input with `shown` and its provenance.
fn titled(shown: &str, custom: bool) -> StatusLineInputs {
    StatusLineInputs {
        window: Some(WindowTitleInput {
            number: 2,
            shown: shown.to_owned(),
            custom,
            app_name: "SonicTerm",
        }),
        ..idle_inputs()
    }
}

/// The developer preset shows the native title's text in the centre: a dim `#2 ` and the name,
/// spoken as the same text, with Rename Window as the click.
#[test]
fn the_window_segment_shows_the_native_titles_text() {
    let view = build_view(
        &developer(),
        false,
        &titled("code/sonicterm", false),
        &translator("en"),
        NumberStyle::POINT,
    );
    let segment = view.segments.iter().find(|segment| segment.id == StatusSegmentId::Window);
    let segment = segment.expect("window is in the developer centre");
    assert_eq!(text_of(&segment.parts), "#2 code/sonicterm");
    assert_eq!(segment.parts[0].tone, StatusTone::Name, "the number is dim");
    assert_eq!(segment.parts[1].tone, StatusTone::Value);
    assert!(segment.parts[1].ellipsize, "the name may middle-ellipsize");
    assert_eq!(text_of(segment.compact.as_deref().unwrap()), "code/sonicterm", "#N goes first");
    assert_eq!(segment.click, Some(SegmentClick::Action(Action::RenameWindow)));
    assert_eq!(segment.spoken.name, "Window");
    assert_eq!(segment.spoken.value, "#2 code/sonicterm");
}

/// `number = false` leaves only the name, with no compact form to fall back to.
#[test]
fn number_off_shows_only_the_name() {
    let mut resolved = developer();
    resolved.window.number = false;
    let view =
        build_view(&resolved, false, &titled("Work", true), &translator("en"), NumberStyle::POINT);
    assert_eq!(shown(&view, StatusSegmentId::Window).as_deref(), Some("Work"));
    let segment = view.segments.iter().find(|segment| segment.id == StatusSegmentId::Window);
    assert!(segment.unwrap().compact.is_none());
}

/// `hide_sensitive` replaces a tab's title, which can hold a path or host, with the application
/// name, in the drawn and the spoken text; a user's own window name stays.
#[test]
fn hide_sensitive_redacts_a_tab_title_but_keeps_a_custom_name() {
    let i18n = translator("en");
    let view = build_view(
        &developer(),
        true,
        &titled("ssh prod-db:/var/lib", false),
        &i18n,
        NumberStyle::POINT,
    );
    let segment = view.segments.iter().find(|segment| segment.id == StatusSegmentId::Window);
    let segment = segment.expect("the window segment stays");
    assert_eq!(text_of(&segment.parts), "#2 SonicTerm");
    assert_eq!(segment.spoken.value, "#2 SonicTerm");

    let view = build_view(&developer(), true, &titled("Work", true), &i18n, NumberStyle::POINT);
    assert_eq!(shown(&view, StatusSegmentId::Window).as_deref(), Some("#2 Work"));
}

/// A window with no number (no title input) shows no window segment.
#[test]
fn no_title_input_shows_no_window_segment() {
    let view =
        build_view(&developer(), false, &idle_inputs(), &translator("en"), NumberStyle::POINT);
    assert!(shown(&view, StatusSegmentId::Window).is_none());
}

/// The integrated band projects `window` first into the centre of lists that omit it, keeping
/// its configured click, and never duplicates one a list already places, on any side.
#[test]
fn the_band_projects_the_window_segment_once() {
    use sonicterm_cfg::status_line::{ClickBinding, StatusLineConfig};
    let explicit: StatusLineConfig = toml::from_str::<sonicterm_cfg::config::Config>(
        "[status_line]\ntitlebar_segments = \"all\"\nleft = [\"mode\"]\ncenter = [\"link\"]\n\
         [status_line.segment.window]\nclick = \"none\"\n",
    )
    .unwrap()
    .status_line;
    let resolved = explicit.resolve().unwrap();
    let projected = with_window_projected(&resolved);
    let order: Vec<_> = projected.segments.iter().map(|segment| segment.id).collect();
    assert_eq!(order[..3], [StatusSegmentId::Mode, StatusSegmentId::Window, StatusSegmentId::Link]);
    let window = projected.segment(StatusSegmentId::Window).unwrap();
    assert_eq!((window.side, &window.click), (StatusSide::Center, &ClickBinding::None));
    assert!(projected.unlisted.iter().all(|segment| segment.id != StatusSegmentId::Window));
    assert!(resolved.segment(StatusSegmentId::Window).is_none(), "the user's lists are untouched");

    let mut on_the_right = developer();
    on_the_right.titlebar_segments = sonicterm_cfg::status_line::TitlebarSegments::All;
    on_the_right.segments.retain(|segment| segment.id != StatusSegmentId::Window);
    on_the_right.segments.push(sonicterm_cfg::status_line::ResolvedSegment {
        side: StatusSide::Right,
        ..developer().segment(StatusSegmentId::Window).unwrap().clone()
    });
    let projected = with_window_projected(&on_the_right);
    let windows: Vec<_> =
        projected.segments.iter().filter(|segment| segment.id == StatusSegmentId::Window).collect();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].side, StatusSide::Right, "an explicit place is kept");

    let band = window_only(&developer());
    assert_eq!(band.segments.len(), 1);
    assert_eq!(band.segments[0].id, StatusSegmentId::Window);
}

/// A titlebar band shows the name and the right list by default, every list with
/// `titlebar_segments = "all"`, and the name outranks everything in it.
#[test]
fn the_band_keeps_the_name_and_the_right_list() {
    use sonicterm_cfg::status_line::TitlebarSegments;
    let band = with_window_projected(&developer());
    assert!(band.in_band);
    assert!(band
        .segments
        .iter()
        .all(|segment| segment.id == StatusSegmentId::Window || segment.side == StatusSide::Right));
    assert_eq!(
        band.segment(StatusSegmentId::Window).unwrap().priority,
        sonicterm_cfg::status_line::MAX_PRIORITY
    );
    let mut every = developer();
    every.titlebar_segments = TitlebarSegments::All;
    let band = with_window_projected(&every);
    assert!(band.segments.iter().any(|segment| segment.side == StatusSide::Left));
    assert!(!developer().in_band, "the user's lists are not a band");
}

/// In a band a name too long for the strip hides the right segments first, then drops `#N`, and
/// only then shortens; on an ordinary strip `#N` is the compact form fitting tries first.
#[test]
fn a_long_band_name_hides_the_right_segments_first() {
    let long = "a-very-long-window-title-that-fills-the-titlebar";
    let inputs = StatusLineInputs { mem: idle_inputs().mem, ..titled(long, true) };
    let i18n = translator("en");
    let view =
        build_view(&with_window_projected(&developer()), false, &inputs, &i18n, NumberStyle::POINT);
    let ids = |placed: &[PlacedSegment]| -> Vec<StatusSegmentId> {
        placed.iter().map(|segment| view.segments[segment.index].id).collect()
    };
    let full = columns(&format!("#2 {long}")) + 1.0 + columns("Mem: 412 MB");
    assert_eq!(
        ids(&fit(&view, &tight(full), columns)),
        [StatusSegmentId::Window, StatusSegmentId::Mem]
    );
    let only_name = fit(&view, &tight(columns(&format!("#2 {long}"))), columns);
    assert_eq!(ids(&only_name), [StatusSegmentId::Window], "Mem goes before the name shortens");
    assert_eq!(text_of_placed(&only_name[0]), format!("#2 {long}"));
    let no_number = fit(&view, &tight(columns(long)), columns);
    assert_eq!(text_of_placed(&no_number[0]), long, "then #N goes");
    let shorter = fit(&view, &tight(20.0), columns);
    assert!(text_of_placed(&shorter[0]).contains('…'), "then the name ellipsizes");
}

/// The drawn text of a placed segment.
fn text_of_placed(segment: &PlacedSegment) -> String {
    segment.parts.iter().map(|part| part.text.as_str()).collect()
}

/// The band's name outranks even a protected segment raised to the top priority: such a segment
/// is dropped before the name loses `#N` or shortens.
#[test]
fn the_band_name_outlasts_a_top_priority_protected_segment() {
    let mut resolved = developer();
    for segment in &mut resolved.segments {
        if segment.id == StatusSegmentId::Health {
            segment.priority = sonicterm_cfg::status_line::MAX_PRIORITY;
        }
    }
    let long = "a-very-long-window-title-that-fills-the-titlebar";
    let inputs = StatusLineInputs {
        health: HealthInput { config: Some(ReloadFailure { line: Some(3) }), ..Default::default() },
        ..titled(long, true)
    };
    let view = build_view(
        &with_window_projected(&resolved),
        false,
        &inputs,
        &translator("en"),
        NumberStyle::POINT,
    );
    let placed = fit(&view, &tight(columns(&format!("#2 {long}"))), columns);
    let ids: Vec<StatusSegmentId> =
        placed.iter().map(|segment| view.segments[segment.index].id).collect();
    assert_eq!(ids, [StatusSegmentId::Window], "the protected health error went first");
    assert_eq!(text_of_placed(&placed[0]), format!("#2 {long}"), "the name is whole");
}
