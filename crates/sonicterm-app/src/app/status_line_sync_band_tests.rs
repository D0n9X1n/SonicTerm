//! The name band and the strip's roles in an integrated window, through the production builders:
//! exactly one name, the applied title and its redaction, and the pass order that feeds them.

use super::super::status_line_tests::*;
use super::*;
use sonicterm_cfg::status_line::{StatusLineConfig, StatusSegmentId};
use sonicterm_ui::status_line::WindowTitleInput;

/// Window #3's applied title `shown`, a tab's when `custom` is false.
fn applied(shown: &str, custom: bool) -> Option<WindowTitleInput> {
    Some(WindowTitleInput { number: 3, shown: shown.to_owned(), custom, app_name: "SonicTerm" })
}

/// How many segments of `view` are the window name.
fn names(view: &StatusLineView) -> usize {
    view.segments.iter().filter(|segment| segment.id == StatusSegmentId::Window).count()
}

/// The separate name band shows the applied title, redacted under `hide_sensitive` unless it is
/// the user's own name; with no separate band it draws nothing.
#[test]
fn the_name_band_shows_the_applied_title_and_redacts_a_tab_title() {
    let app = app_with_strip(false);
    let settings = sonicterm_ui::status_line::window_only(&app.status_line.resolved);
    let numbers = NumberStyle::for_locale("en");
    let shown = |view: &StatusLineView| -> String {
        view.segments[0].parts.iter().map(|part| part.text.as_str()).collect()
    };
    let view =
        band_view_for(&settings, true, false, applied("ssh prod", false), &app.i18n, numbers);
    assert_eq!((names(&view), shown(&view)), (1, "#3 ssh prod".to_owned()));
    let view = band_view_for(&settings, true, true, applied("ssh prod", false), &app.i18n, numbers);
    assert_eq!(shown(&view), "#3 SonicTerm", "a tab title is redacted");
    let view = band_view_for(&settings, true, true, applied("Work", true), &app.i18n, numbers);
    assert_eq!(shown(&view), "#3 Work", "the user's name stays");
    let none = band_view_for(&settings, false, false, applied("Work", true), &app.i18n, numbers);
    assert!(none.segments.is_empty(), "merged or native: the band draws nothing");
}

/// Across the strip's roles exactly one surface shows the name: a strip that is the band shows it
/// once even when the user's lists omit it, a strip below a band never does, and a native window's
/// strip shows the user's lists as they are.
#[test]
fn exactly_one_surface_shows_the_name() {
    let mut app = app_with_strip(true);
    let window_id = WindowId::dummy();
    let inputs = StatusLineInputs { window: applied("Work", true), ..Default::default() };
    let numbers = NumberStyle::for_locale("en");
    let mut shows = StripShows::default();
    let i18n = sonicterm_ui::i18n::I18n::new(Some("en"));
    let mut build = |app: &mut App, role| {
        app.status_line.built.clear();
        app.status_line
            .rebuild_window_strip(window_id, inputs.clone(), role, &i18n, numbers, &mut shows)
            .expect("a first build")
    };
    assert_eq!(names(&build(&mut app, StripRole::Band)), 1);
    assert_eq!(names(&build(&mut app, StripRole::BelowBand)), 0);
    assert_eq!(names(&build(&mut app, StripRole::Plain)), 1, "the developer preset lists it");
    let omitted = StatusLineConfig {
        enabled: true,
        center: Some(vec![StatusSegmentId::Link]),
        ..StatusLineConfig::default()
    };
    app.status_line.resolved = omitted.resolve().unwrap();
    assert_eq!(names(&build(&mut app, StripRole::Band)), 1, "the band projects an omitted name");
    assert_eq!(names(&build(&mut app, StripRole::Plain)), 0, "a native strip keeps the omission");
}

/// The event-loop pass writes native titles, then applies the status line's settings, then builds
/// the name band, so a reload's privacy and text size reach the band in the same pass; and the
/// band sync hands the strip's text size to the renderer even while the strip is off.
#[test]
fn the_pass_applies_settings_before_the_name_band() {
    let pass = include_str!("event_loop_wait.rs");
    let title = pass.find("self.native_title_wake(now)").expect("native titles");
    let status = pass.find("self.sync_status_line(now)").expect("status line");
    let band = pass.find("self.sync_title_bands()").expect("name band");
    assert!(title < status && status < band, "titles, then settings, then the band");
    let sync = include_str!("status_line_sync.rs");
    let body = &sync[sync.find("fn sync_title_bands").expect("band sync")..];
    assert!(body.contains("set_status_line_font_scale"), "the band follows font_scale");
}
