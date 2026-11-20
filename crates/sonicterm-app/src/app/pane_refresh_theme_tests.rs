//! Theme updates and the color-scheme reports a program that set DEC mode 2031 receives.

use super::*;

use sonicterm_cfg::theme::{Appearance, Hex, Theme};

/// A copy of the default theme with `background` and `appearance`.
fn theme_with(background: &str, appearance: Appearance) -> Theme {
    let mut theme = Theme::default();
    theme.colors.background = Hex(background.into());
    theme.appearance = appearance;
    theme
}

/// The first seed answers DSR 996 with the theme's scheme and reports nothing unsolicited, even
/// when the program already set mode 2031.
#[test]
fn creation_seed_answers_dsr_996_without_a_report() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?2031h");
    seed_parser_theme_colors(&mut parser, &theme_with("#101010", Appearance::Dark));
    let (_, _, reply) = parser.advance_with_replies(b"\x1b[?996n");
    assert_eq!(reply, b"\x1b[?997;1n");
}

/// A later theme update returns the report for the host to send when mode 2031 is set: for a
/// change of scheme and for another theme of the same scheme, never for the same theme again.
#[test]
fn reseed_returns_a_report_for_each_theme_change() {
    let mut parser = Parser::new(Grid::new(8, 2));
    seed_parser_theme_colors(&mut parser, &theme_with("#101010", Appearance::Dark));
    parser.advance(b"\x1b[?2031h");
    let report = reseed_parser_theme_colors(&mut parser, &theme_with("#fafafa", Appearance::Light));
    assert_eq!(report.as_deref(), Some(&b"\x1b[?997;2n"[..]));
    let report = reseed_parser_theme_colors(&mut parser, &theme_with("#f0f0f0", Appearance::Light));
    assert_eq!(report.as_deref(), Some(&b"\x1b[?997;2n"[..]), "same scheme, new palette");
    let report = reseed_parser_theme_colors(&mut parser, &theme_with("#f0f0f0", Appearance::Light));
    assert_eq!(report, None, "an unchanged theme reports nothing");
}

/// Without mode 2031 a theme change returns no report.
#[test]
fn reseed_without_mode_2031_reports_nothing() {
    let mut parser = Parser::new(Grid::new(8, 2));
    seed_parser_theme_colors(&mut parser, &theme_with("#101010", Appearance::Dark));
    let report = reseed_parser_theme_colors(&mut parser, &theme_with("#fafafa", Appearance::Light));
    assert_eq!(report, None);
}

/// Propagation reseeds every pane and collects one report per pane that set mode 2031, keyed by
/// pane, so the host can send each through that pane's reply path after the parser locks end.
#[test]
fn propagation_collects_one_report_per_listening_pane() {
    let mut app =
        App::new(theme_with("#101010", Appearance::Dark), Config::default(), Keymap::default());
    let listening = app.__test_seed_tab("listening");
    let quiet = app.__test_seed_tab("quiet");
    for pane_id in [listening, quiet] {
        assert!(app.__test_seed_pane_theme_colors(pane_id));
    }
    assert!(app.__test_advance_pane_parser(listening, b"\x1b[?2031h"));
    let panes = &app.main().expect("main window").panes;
    let reports = propagate_theme_to_pane_parsers(panes, &theme_with("#fafafa", Appearance::Light));
    assert_eq!(reports, vec![(listening, b"\x1b[?997;2n".to_vec())]);
}
