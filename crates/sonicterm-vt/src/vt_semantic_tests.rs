//! Pins the OSC 133 annotations (L, N, P, I and the options on A) and ConEmu's OSC 9;12: they
//! change only the per-screen semantic state, never prompt regions or command events; a valid
//! token admits them without latching; fresh-line scrolls like a line feed; options reset on each
//! prompt; and command text comes only from an authenticated C, decoded and bounded.

use sonicterm_grid::grid::{
    ClickEvents, ClickLine, PromptClick, PromptKind, PromptOptions, PromptRedraw, SemanticMode,
};

use super::*;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A parser with no shell token.
fn plain() -> Parser {
    Parser::new(Grid::new(30, 6))
}

/// A parser for a pane whose shell was given `TOKEN`.
fn tokened() -> Parser {
    let mut parser = plain();
    parser.set_shell_token(Some(TOKEN.to_owned()));
    parser
}

/// OSC 133 `kind`, with `extra` parameters, terminated by BEL.
fn mark(kind: &str, extra: &[&str]) -> Vec<u8> {
    let mut text = format!("\x1b]133;{kind}");
    for param in extra {
        text.push(';');
        text.push_str(param);
    }
    text.push('\x07');
    text.into_bytes()
}

/// The token parameter a SonicTerm shell integration appends.
fn good() -> String {
    format!("sonicterm_token={TOKEN}")
}

/// Whether `events` hold any OSC 133 command event.
fn has_command_event(events: &[VtEvent]) -> bool {
    events.iter().any(|event| matches!(event, VtEvent::Command(_)))
}

/// The cursor as `(row, col)`.
fn cursor(parser: &Parser) -> (u16, u16) {
    (parser.grid().cursor.row, parser.grid().cursor.col)
}

#[test]
fn standalone_p_creates_semantic_anchor_without_region() {
    // P with no A before it starts a semantic generation at the cursor row, but opens no prompt
    // region and raises no command event, so MCP readiness never sees it.
    let mut parser = plain();
    parser.advance(b"out\r\n");
    let events = parser.advance(&mark("P", &["k=i"]));
    let state = parser.grid().semantic();
    assert_eq!(state.anchor, Some(1));
    assert_eq!(state.mode, SemanticMode::Prompt(PromptKind::Initial));
    assert_eq!(state.generation, 1);
    assert_eq!(parser.grid().prompts_len(), 0);
    assert!(!has_command_event(&events));
}

#[test]
fn secondary_p_joins_the_anchor_or_acts_as_initial() {
    // P k=c or k=s under an anchor records its row in the same generation; without an anchor it
    // acts as k=i. P never moves the cursor.
    let mut parser = plain();
    parser.advance(b"ab");
    parser.advance(&mark("P", &["k=s"]));
    assert_eq!(cursor(&parser), (0, 2), "P does no fresh-line");
    assert_eq!(parser.grid().semantic().mode, SemanticMode::Prompt(PromptKind::Initial));
    assert_eq!(parser.grid().semantic().generation, 1);
    parser.advance(b"\r\n");
    parser.advance(&mark("P", &["k=c"]));
    let state = parser.grid().semantic();
    assert_eq!((state.anchor, state.last_secondary_row), (Some(0), Some(1)));
    assert_eq!(state.mode, SemanticMode::Prompt(PromptKind::Continuation));
    assert_eq!(state.generation, 1, "a continuation keeps the generation");
    parser.advance(&mark("P", &["k=r"]));
    assert_eq!(parser.grid().semantic().generation, 2, "k=r starts a generation like A");
    assert_eq!(parser.grid().semantic().mode, SemanticMode::Prompt(PromptKind::Right));
}

#[test]
fn i_ends_on_lf_ind_nel() {
    // Input an I opened ends at the next line feed, IND or NEL; the anchor and generation stay.
    for ending in [&b"\n"[..], b"\x1bD", b"\x1bE"] {
        let mut parser = plain();
        parser.advance(&mark("A", &[]));
        parser.advance(&mark("I", &[]));
        assert_eq!(parser.grid().semantic().mode, SemanticMode::Input { until_eol: true });
        parser.advance(ending);
        let state = parser.grid().semantic();
        assert_eq!(state.mode, SemanticMode::Output, "{ending:?}");
        assert_eq!((state.anchor, state.generation), (Some(0), 1));
    }
}

#[test]
fn b_survives_newline() {
    // Input a B opened runs past line feeds; only C, D or a prompt ends it.
    let mut parser = plain();
    parser.advance(&mark("A", &[]));
    parser.advance(&mark("B", &[]));
    parser.advance(b"line one\r\nline two\r\n");
    assert_eq!(parser.grid().semantic().mode, SemanticMode::Input { until_eol: false });
}

#[test]
fn c_and_d_return_to_output_keep_anchor() {
    // C and D return to output; the anchor and generation stay until the next prompt.
    for end in ["C", "D"] {
        let mut parser = plain();
        parser.advance(&mark("A", &[]));
        parser.advance(&mark("B", &[]));
        parser.advance(&mark(end, &[]));
        let state = parser.grid().semantic();
        assert_eq!(
            (state.mode, state.anchor, state.generation),
            (SemanticMode::Output, Some(0), 1)
        );
    }
}

#[test]
fn ris_resets_both_screens_and_bumps_generation() {
    // RIS returns the primary to output with no anchor in a new generation, and drops the
    // alternate screen's state with the screen, so a later alternate screen starts clean.
    let mut parser = plain();
    parser.advance(&mark("A", &["redraw=0"]));
    parser.advance(b"\x1b[?1049h");
    parser.advance(&mark("A", &[]));
    parser.advance(b"\x1bc");
    let state = parser.grid().semantic();
    assert!(!parser.grid().is_alt());
    assert_eq!((state.mode, state.anchor), (SemanticMode::Output, None));
    assert_eq!(state.generation, 2);
    assert_eq!(state.options, PromptOptions::default());
    parser.advance(b"\x1b[?1049h");
    assert_eq!(parser.grid().semantic().anchor, None);
}

#[test]
fn alt_screen_state_independent() {
    // An annotation on the alternate screen changes only that screen's state.
    let mut parser = plain();
    parser.advance(&mark("A", &[]));
    let primary = parser.grid().semantic().clone();
    parser.advance(b"\x1b[?1049h");
    parser.advance(&mark("N", &[]));
    parser.advance(&mark("I", &[]));
    assert_eq!(parser.grid().primary_screen().semantic(), &primary);
    parser.advance(b"\x1b[?1049l");
    assert_eq!(parser.grid().semantic(), &primary);
}

#[test]
fn fresh_line_at_bottom_scrolls_and_evicts() {
    // A prompt mark off column 0 starts a fresh line; at the bottom margin that scrolls like a
    // line feed, evicting the oldest history row, and the anchor sits on the new line.
    let mut parser = Parser::new(Grid::new(10, 3));
    parser.grid_mut().set_scrollback_limit(1);
    parser.advance(b"one\r\ntwo\r\nthree\r\nxy");
    assert_eq!(parser.grid().scrollback_len(), 1);
    let evicted = parser.grid().scrollback_evicted();
    parser.advance(&mark("A", &[]));
    assert_eq!(parser.grid().scrollback_evicted(), evicted + 1);
    assert_eq!(cursor(&parser), (2, 0));
    assert_eq!(crate::vt::vt_test_support::row_text(&parser, 1).trim_end(), "xy");
    assert_eq!(parser.grid().semantic().anchor, Some(3));
    assert_eq!(parser.grid().prompts().last().and_then(|region| region.start_row), Some(3));
}

#[test]
fn annotation_with_token_never_latches() {
    // The right token admits an annotation but never latches the pane, and it leaves the observed
    // shell state as it was; only A, B, C and D latch.
    let mut parser = tokened();
    for (kind, extra) in [
        ("L", vec![good()]),
        ("N", vec![good()]),
        ("P", vec!["k=i".into(), good()]),
        ("I", vec![good()]),
    ] {
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        parser.advance(&mark(kind, &extra));
        assert!(!parser.shell_markers_authenticated(), "{kind} latched the pane");
        assert_eq!(parser.shell_integration_state(), ShellIntegrationState::None, "{kind}");
    }
    assert_eq!(parser.grid().semantic().mode, SemanticMode::Input { until_eol: true });
    parser.advance(&mark("A", &[&good()]));
    assert!(parser.shell_markers_authenticated());
    parser.advance(&mark("N", &[&good()]));
    assert_eq!(parser.shell_integration_state(), ShellIntegrationState::Verified);
}

#[test]
fn tokenless_annotation_after_latch_refused_and_downgrades() {
    // Before the latch a tokenless annotation is admitted; after it, one is refused, changes no
    // state, and marks the observed shell state unverified.
    let mut parser = tokened();
    parser.advance(&mark("N", &[]));
    assert_eq!(parser.grid().semantic().generation, 1, "admitted before the latch");
    assert_eq!(parser.shell_integration_state(), ShellIntegrationState::Unverified);
    parser.advance(&mark("A", &[&good()]));
    let before = parser.grid().semantic().clone();
    parser.advance(&mark("I", &[]));
    parser.advance(&mark("P", &["k=i"]));
    assert_eq!(parser.grid().semantic(), &before);
    assert_eq!(parser.shell_integration_state(), ShellIntegrationState::Unverified);
}

#[test]
fn refused_annotation_no_fresh_line() {
    // A refused N or L (wrong token) moves no cursor and changes no state.
    let mut parser = tokened();
    parser.advance(b"ab");
    let bad = "sonicterm_token=ffffffffffffffffffffffffffffffff";
    parser.advance(&mark("N", &[bad]));
    parser.advance(&mark("L", &[bad]));
    assert_eq!(cursor(&parser), (0, 2));
    assert_eq!(parser.grid().semantic().generation, 0);
    assert_eq!(parser.shell_integration_state(), ShellIntegrationState::Unverified);
}

#[test]
fn l_bare_and_tokened_only() {
    // L is a fresh-line alone: bare, or with only the pane's token. Any other parameter makes it
    // invalid, so it moves nothing; L never touches the semantic state.
    let mut parser = tokened();
    parser.advance(b"ab");
    parser.advance(&mark("L", &[]));
    assert_eq!(cursor(&parser), (1, 0));
    parser.advance(b"cd");
    parser.advance(&mark("L", &[&good()]));
    assert_eq!(cursor(&parser), (2, 0));
    parser.advance(b"ef");
    parser.advance(&mark("L", &["aid=1"]));
    parser.advance(&mark("L", &[&good(), "aid=1"]));
    assert_eq!(cursor(&parser), (2, 2));
    parser.advance(&mark("L", &[]));
    assert_eq!(cursor(&parser), (3, 0));
    parser.advance(&mark("L", &[]));
    assert_eq!(cursor(&parser), (3, 0), "at column 0 there is nothing to do");
    assert_eq!(parser.grid().semantic().generation, 0);
    assert!(!parser.shell_markers_authenticated());
}

#[test]
fn options_first_match_reset_each_a() {
    // The first occurrence of an option wins, a malformed value is unset, and each A, N or 9;12
    // resets every option before reading its own; P keeps them.
    let mut parser = plain();
    parser.advance(&mark(
        "A",
        &["redraw=last", "redraw=0", "click_events=2", "cl=v", "special_key=1", "k=r"],
    ));
    let expected = PromptOptions {
        redraw: PromptRedraw::LastLine,
        click: PromptClick {
            events: Some(ClickEvents::Relative),
            line_motion: Some(ClickLine::ConservativeVertical),
            special_key: true,
        },
    };
    assert_eq!(parser.grid().semantic().options, expected);
    assert_eq!(parser.grid().semantic().mode, SemanticMode::Prompt(PromptKind::Right));
    parser.advance(&mark("P", &["k=i", "redraw=0"]));
    assert_eq!(parser.grid().semantic().options, expected, "P keeps the options");
    parser.advance(&mark("A", &["click_events=3", "cl=line"]));
    let reset = PromptOptions {
        click: PromptClick { line_motion: Some(ClickLine::Line), ..PromptClick::default() },
        ..PromptOptions::default()
    };
    assert_eq!(parser.grid().semantic().options, reset);
    parser.advance(&mark("N", &["redraw=0", "cl=m", "special_key=yes"]));
    let state = parser.grid().semantic();
    assert_eq!(state.options.redraw, PromptRedraw::Off);
    assert_eq!(state.options.click.line_motion, Some(ClickLine::Multiple));
    assert!(!state.options.click.special_key, "a malformed value is unset");
    parser.advance(b"\x1b]9;12\x07");
    assert_eq!(parser.grid().semantic().options, PromptOptions::default());
}

#[test]
fn fish_click_events_prompt_fixture() {
    // fish emits `133;A;click_events=1` as its prompt start: one region and absolute clicks.
    let mut parser = plain();
    parser.advance(b"\x1b]133;A;click_events=1\x1b\\");
    assert_eq!(parser.grid().prompts_len(), 1);
    assert_eq!(parser.grid().semantic().options.click.events, Some(ClickEvents::Absolute));
}

#[test]
fn wezterm_d_with_aid_keeps_exit() {
    // WezTerm's `133;D;<code>;aid=…` keeps the positional exit code.
    let mut parser = plain();
    parser.advance(&mark("A", &[]));
    parser.advance(&mark("C", &[]));
    let events = parser.advance(&mark("D", &["7", "aid=12"]));
    assert!(events
        .iter()
        .any(|event| matches!(event, VtEvent::Command(CommandEvent::CmdEnd(Some(7))))));
    assert_eq!(parser.grid().prompts().last().and_then(|region| region.exit_code), Some(7));
}

/// The command text `params` carry, as the C parser reads it.
fn cmdline_of(params: &[&str]) -> Option<String> {
    let params: Vec<&[u8]> = params.iter().map(|param| param.as_bytes()).collect();
    parse_cmdline(&params)
}

#[test]
fn cmdline_precedence_and_decode_table() {
    // `cmdline=` wins over `cmdline_url=` in any position; `cmdline=` is bash `%q`-unquoted and
    // `cmdline_url=` percent-decoded; a malformed value drops the text.
    let table: &[(&[&str], Option<&str>)] = &[
        (&["cmdline_url=ls%20-l", "cmdline=pwd"], Some("pwd")),
        (&["cmdline=pwd", "cmdline_url=ls"], Some("pwd")),
        (&["cmdline_url=ls%20-l%3Bx"], Some("ls -l;x")),
        (&["cmdline_url=bad%2"], None),
        (&["cmdline_url=bad%zz"], None),
        (&["cmdline=echo\\ hi"], Some("echo hi")),
        (&["cmdline=echo 'a b'"], Some("echo a b")),
        (&["cmdline=$'a\\tb\\n'"], Some("a\tb\n")),
        (&["cmdline=$'\\x41\\101\\e\\E\\\\\\'\\\"'"], Some("AA\x1b\x1b\\'\"")),
        (&["cmdline=$'\\a\\b\\f\\r\\v'"], Some("\x07\x08\x0c\r\x0b")),
        (&["cmdline=trailing\\"], None),
        (&["cmdline='open"], None),
        (&["cmdline=$'\\q'"], None),
        (&["k=i"], None),
    ];
    for (params, expected) in table {
        assert_eq!(cmdline_of(params).as_deref(), *expected, "{params:?}");
    }
}

#[test]
fn cmdline_limits_and_non_utf8_dropped() {
    // Raw text past 4 KiB or decoded text past 1 KiB is dropped whole, as is text that is not
    // UTF-8 once decoded.
    let decoded_long = format!("cmdline_url={}", "a".repeat(MAX_CMDLINE_BYTES + 1));
    assert_eq!(cmdline_of(&[&decoded_long]), None);
    let at_limit = format!("cmdline_url={}", "a".repeat(MAX_CMDLINE_BYTES));
    assert_eq!(cmdline_of(&[&at_limit]).map(|text| text.len()), Some(MAX_CMDLINE_BYTES));
    let raw_long = format!("cmdline=a{}", "''".repeat(2048));
    assert_eq!(cmdline_of(&[&raw_long]), None, "raw text past 4 KiB");
    assert_eq!(cmdline_of(&["cmdline_url=%FF%FE"]), None);
    assert_eq!(cmdline_of(&["cmdline=$'\\xff'"]), None);
}

#[test]
fn authenticated_c_stores_cmdline_and_next_prompt_clears_it() {
    // An authenticated C stores its decoded command text on the screen's state; the next prompt
    // clears it.
    let mut parser = tokened();
    parser.advance(&mark("A", &[&good()]));
    parser.advance(&mark("C", &["cmdline_url=make%20test", &good()]));
    let text = parser.grid().semantic().cmdline.clone();
    assert_eq!(text.as_ref().map(CmdlineText::as_str), Some("make test"));
    parser.advance(&mark("D", &["0", &good()]));
    assert!(parser.grid().semantic().cmdline.is_some(), "D keeps it");
    parser.advance(&mark("A", &[&good()]));
    assert!(parser.grid().semantic().cmdline.is_none());
}

#[test]
fn unauthenticated_c_cmdline_not_stored() {
    // Command text is taken only from an authenticated C: never without a pane token, and never
    // from a tokenless C admitted before the latch.
    let mut plain = plain();
    plain.advance(&mark("A", &[]));
    plain.advance(&mark("C", &["cmdline_url=ls"]));
    assert!(plain.grid().semantic().cmdline.is_none());
    let mut parser = tokened();
    parser.advance(&mark("A", &[]));
    parser.advance(&mark("C", &["cmdline_url=ls"]));
    assert!(parser.grid().semantic().cmdline.is_none());
    assert_eq!(parser.grid().semantic().mode, SemanticMode::Output, "the C itself still applies");
}

#[test]
fn osc9_12_bare_and_data() {
    // ConEmu's `9;12`, bare or with data after it, is a prompt mark: a fresh-line and a new
    // generation with default options, with no notification, region or command event.
    for sequence in [&b"\x1b]9;12\x07"[..], b"\x1b]9;12;ignored\x1b\\"] {
        let mut parser = plain();
        parser.advance(b"ab");
        let events = parser.advance(sequence);
        assert!(events.is_empty(), "{events:?}");
        assert_eq!(cursor(&parser), (1, 0));
        let state = parser.grid().semantic();
        assert_eq!((state.anchor, state.generation), (Some(1), 1));
        assert_eq!(state.mode, SemanticMode::Prompt(PromptKind::Initial));
        assert_eq!(parser.grid().prompts_len(), 0);
    }
}

#[test]
fn conemu_prompt_fixture_refused_after_latch() {
    // The ConEmu PowerShell profile's `ESC]9;12 BEL` carries no token, so once a tokened shell
    // latched the pane it is refused and moves nothing.
    let mut parser = tokened();
    parser.advance(&mark("A", &[&good()]));
    parser.advance(b"ab");
    let before = parser.grid().semantic().clone();
    parser.advance(b"\x1b]9;12\x07");
    assert_eq!(cursor(&parser), (0, 2));
    assert_eq!(parser.grid().semantic(), &before);
    assert_eq!(parser.shell_integration_state(), ShellIntegrationState::Unverified);
}
