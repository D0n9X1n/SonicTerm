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

/// Every seed tells the parser the theme's selection background, selection foreground and cursor
/// text, so OSC 17, OSC 19 and the OSC 21 `cursor_text` key answer the theme until a program sets
/// its own; a reload's new values are answered after the reseed.
#[test]
fn seeds_answer_the_theme_selection_and_cursor_text() {
    let mut theme = Theme::default();
    theme.colors.selection_bg = Hex("#102030".into());
    theme.colors.selection_fg = Hex("#405060".into());
    theme.colors.cursor_text = Hex("#708090".into());
    let mut parser = Parser::new(Grid::new(8, 2));
    seed_parser_theme_colors(&mut parser, &theme);
    // The chain walks 17, 18 and 19; 18 is not kept, so 17 and 19 answer.
    let (_, _, reply) = parser.advance_with_replies(b"\x1b]17;?;?;?\x07");
    assert_eq!(reply, b"\x1b]17;rgb:1010/2020/3030\x07\x1b]19;rgb:4040/5050/6060\x07");
    let (_, _, reply) = parser.advance_with_replies(b"\x1b]21;cursor_text=?\x07");
    assert_eq!(reply, b"\x1b]21;cursor_text=rgb:70/80/90\x07");
    theme.colors.selection_bg = Hex("#a0b0c0".into());
    let _ = reseed_parser_theme_colors(&mut parser, &theme);
    let (_, _, reply) = parser.advance_with_replies(b"\x1b]17;?\x07");
    assert_eq!(reply, b"\x1b]17;rgb:a0a0/b0b0/c0c0\x07");
}

/// A theme change that alters only the selection or cursor-text colors still sends the mode 2031
/// report, because OSC 17, OSC 19 and the OSC 21 `cursor_text` key read those colors back.
#[test]
fn selection_or_cursor_text_change_alone_sends_the_report() {
    let base = theme_with("#101010", Appearance::Dark);
    let mut parser = Parser::new(Grid::new(8, 2));
    seed_parser_theme_colors(&mut parser, &base);
    parser.advance(b"\x1b[?2031h");
    let mut selection = base.clone();
    selection.colors.selection_bg = Hex("#123456".into());
    let report = reseed_parser_theme_colors(&mut parser, &selection);
    assert_eq!(report.as_deref(), Some(&b"\x1b[?997;1n"[..]), "selection background");
    let mut cursor_text = selection.clone();
    cursor_text.colors.cursor_text = Hex("#abcdef".into());
    let report = reseed_parser_theme_colors(&mut parser, &cursor_text);
    assert_eq!(report.as_deref(), Some(&b"\x1b[?997;1n"[..]), "cursor text");
}

/// A theme whose selection or cursor-text color does not parse reseeds the color the renderer
/// draws for it, opaque black, instead of keeping the previous theme's value: OSC 17, 19 and the
/// OSC 21 `cursor_text` key answer black, and mode 2031 reports the change.
#[test]
fn an_unparsable_selection_color_reseeds_the_drawn_fallback() {
    let mut parser = Parser::new(Grid::new(8, 2));
    let mut valid = theme_with("#101010", Appearance::Dark);
    valid.colors.selection_bg = Hex("#336699".into());
    valid.colors.selection_fg = Hex("#ddeeff".into());
    valid.colors.cursor_text = Hex("#123456".into());
    seed_parser_theme_colors(&mut parser, &valid);
    parser.advance(b"\x1b[?2031h");
    let mut invalid = valid.clone();
    for hex in [
        &mut invalid.colors.selection_bg,
        &mut invalid.colors.selection_fg,
        &mut invalid.colors.cursor_text,
    ] {
        *hex = Hex("not a color".into());
    }
    let report = reseed_parser_theme_colors(&mut parser, &invalid);
    assert_eq!(report.as_deref(), Some(&b"\x1b[?997;1n"[..]), "the drawn defaults changed");
    let mut replies = Vec::new();
    let mut remaining: &[u8] = b"\x1b]17;?\x07\x1b]19;?\x07\x1b]21;cursor_text=?\x07";
    while !remaining.is_empty() {
        let (consumed, _, reply) = parser.advance_with_replies(remaining);
        replies.extend(reply);
        remaining = &remaining[consumed..];
    }
    assert_eq!(
        replies,
        b"\x1b]17;rgb:0000/0000/0000\x07\x1b]19;rgb:0000/0000/0000\x07\x1b]21;cursor_text=rgb:00/00/00\x07"
    );
}
