//! Pins every OSC form SonicTerm ignores, each followed by a visible sentinel: nothing of the
//! payload prints, no host event or reply is raised, and no parser state differs from a parser
//! that saw only the sentinel.

use super::*;
use crate::vt::vt_test_support::*;

/// What an ignored sequence must leave exactly as a sentinel-only parser leaves it.
#[derive(Debug, PartialEq)]
struct Observed {
    rows: Vec<String>,
    history: usize,
    cursor: (u16, u16),
    titles: (Option<String>, Option<String>),
    cwd: (Option<String>, u64),
    semantic: sonicterm_grid::grid::SemanticState,
    prompts: usize,
    modes: (u64, u8),
    status: StatusSummary,
    user_vars: Arc<UserVars>,
}

/// The observable state of `parser`.
fn observe(parser: &Parser) -> Observed {
    let grid = parser.grid();
    Observed {
        rows: (0..grid.rows).map(|row| row_text(parser, row)).collect(),
        history: grid.scrollback_len(),
        cursor: (grid.cursor.row, grid.cursor.col),
        titles: (parser.title().map(str::to_owned), parser.icon_title().map(str::to_owned)),
        cwd: (parser.cwd().map(str::to_owned), parser.cwd_revision()),
        semantic: grid.semantic().clone(),
        prompts: grid.prompts().count(),
        modes: (parser.keyboard_input_snapshot(), parser.pointer_input_snapshot()),
        status: parser.program_status_summary(),
        user_vars: parser.user_vars(),
    }
}

/// A parser with history, a title, a working directory and a prompt, and its reply channel.
fn seeded() -> (Parser, crossbeam_channel::Receiver<Vec<u8>>) {
    let (reply_tx, replies) = crossbeam_channel::unbounded();
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(20, 4), Some(reply_tx), pool);
    parser.advance(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n\x1b]2;title\x07");
    parser.advance(b"\x1b]7;file:///tmp\x07\x1b]133;A\x07$ A");
    (parser, replies)
}

/// Feed OSC `payload` (BEL-terminated) then the sentinel `Z`: no event, no reply, and the state a
/// parser fed only `Z` has.
fn assert_ignored(payload: &[u8]) {
    let (mut control, _) = seeded();
    control.advance(b"Z");
    let (mut parser, replies) = seeded();
    let mut bytes = b"\x1b]".to_vec();
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(b"\x07Z");
    let events = parser.advance(&bytes);
    let shown = String::from_utf8_lossy(payload).into_owned();
    assert!(events.is_empty(), "{shown}: {events:?}");
    assert!(replies.try_recv().is_err(), "{shown}: a reply");
    assert_eq!(observe(&parser), observe(&control), "{shown}");
}

/// OSC 133 `C` with `cmdline_url` is not an ignored form: it still marks output, and nothing of its
/// payload prints.
#[test]
fn osc133_c_with_cmdline_url_still_marks_output() {
    let (mut parser, _) = seeded();
    parser.advance(b"\x1b]133;B\x07ls\x1b]133;C;cmdline_url=ls%20-l\x07Z");
    assert_eq!(parser.grid().semantic().mode, SemanticMode::Output);
    assert_eq!(row_text(&parser, 3).trim_end(), "$ AlsZ");
}

/// OSC 3 (X property) is ignored.
#[test]
fn ignores_osc_3() {
    assert_ignored(b"3;key=value");
}

/// OSC 5 (special color) is ignored.
#[test]
fn ignores_osc_5() {
    assert_ignored(b"5;0;?");
}

/// OSC 6 (special color enable) is ignored.
#[test]
fn ignores_osc_6() {
    assert_ignored(b"6;1;bg;red;brightness;255");
}

/// OSC 13 (pointer foreground) is ignored.
#[test]
fn ignores_osc_13() {
    assert_ignored(b"13;?");
}

/// OSC 14 (pointer background) is ignored.
#[test]
fn ignores_osc_14() {
    assert_ignored(b"14;?");
}

/// OSC 15 (Tektronix foreground) is ignored.
#[test]
fn ignores_osc_15() {
    assert_ignored(b"15;?");
}

/// OSC 16 (Tektronix background) is ignored.
#[test]
fn ignores_osc_16() {
    assert_ignored(b"16;?");
}

/// OSC 18 (Tektronix cursor) is ignored.
#[test]
fn ignores_osc_18() {
    assert_ignored(b"18;?");
}

/// OSC 46 (log file) is ignored.
#[test]
fn ignores_osc_46() {
    assert_ignored(b"46;/tmp/log");
}

/// OSC 50 (font) is ignored.
#[test]
fn ignores_osc_50() {
    assert_ignored(b"50;?");
}

/// OSC 51 (Emacs) is ignored.
#[test]
fn ignores_osc_51() {
    assert_ignored(b"51;x");
}

/// OSC 60 (xterm feature query) is ignored.
#[test]
fn ignores_osc_60() {
    assert_ignored(b"60");
}

/// OSC 61 (xterm) is ignored.
#[test]
fn ignores_osc_61() {
    assert_ignored(b"61;x");
}

/// OSC 66 (text sizing) is ignored.
#[test]
fn ignores_osc_66() {
    assert_ignored(b"66;w=2;x");
}

/// OSC 72 (rxvt) is ignored.
#[test]
fn ignores_osc_72() {
    assert_ignored(b"72;x");
}

/// OSC 105 (special color reset) is ignored.
#[test]
fn ignores_osc_105() {
    assert_ignored(b"105;0");
}

/// OSC 106 (special color enable reset) is ignored.
#[test]
fn ignores_osc_106() {
    assert_ignored(b"106;0;1");
}

/// OSC 113 (pointer foreground reset) is ignored.
#[test]
fn ignores_osc_113() {
    assert_ignored(b"113");
}

/// OSC 114 (pointer background reset) is ignored.
#[test]
fn ignores_osc_114() {
    assert_ignored(b"114");
}

/// OSC 115 (Tektronix foreground reset) is ignored.
#[test]
fn ignores_osc_115() {
    assert_ignored(b"115");
}

/// OSC 116 (Tektronix background reset) is ignored.
#[test]
fn ignores_osc_116() {
    assert_ignored(b"116");
}

/// OSC 118 (Tektronix cursor reset) is ignored.
#[test]
fn ignores_osc_118() {
    assert_ignored(b"118");
}

/// OSC 176 (rxvt app name) is ignored.
#[test]
fn ignores_osc_176() {
    assert_ignored(b"176;x");
}

/// OSC 440 (sound) is ignored.
#[test]
fn ignores_osc_440() {
    assert_ignored(b"440;x");
}

/// OSC 441 (sound) is ignored.
#[test]
fn ignores_osc_441() {
    assert_ignored(b"441;x");
}

/// OSC 633 (VS Code prompt mark) is ignored.
#[test]
fn ignores_osc_633() {
    assert_ignored(b"633;A");
}

/// OSC 666 (unknown) is ignored.
#[test]
fn ignores_osc_666() {
    assert_ignored(b"666;x");
}

/// OSC 697 (Fig) is ignored.
#[test]
fn ignores_osc_697() {
    assert_ignored(b"697;x");
}

/// OSC 701 (rxvt locale) is ignored.
#[test]
fn ignores_osc_701() {
    assert_ignored(b"701;x");
}

/// OSC 3008 (context) is ignored.
#[test]
fn ignores_osc_3008() {
    assert_ignored(b"3008;start=1;type=shell");
}

/// OSC 5113 (kitty file transfer) is ignored.
#[test]
fn ignores_osc_5113() {
    assert_ignored(b"5113;CloseReply=1");
}

/// OSC 7704 (rxvt) is ignored.
#[test]
fn ignores_osc_7704() {
    assert_ignored(b"7704;x");
}

/// OSC 7721 (rxvt) is ignored.
#[test]
fn ignores_osc_7721() {
    assert_ignored(b"7721;x");
}

/// OSC 7750 (shell integration) is ignored.
#[test]
fn ignores_osc_7750() {
    assert_ignored(b"7750;x");
}

/// OSC 7770 (rxvt) is ignored.
#[test]
fn ignores_osc_7770() {
    assert_ignored(b"7770;x");
}

/// OSC 7771 (rxvt) is ignored.
#[test]
fn ignores_osc_7771() {
    assert_ignored(b"7771;x");
}

/// OSC 7777 (rxvt) is ignored.
#[test]
fn ignores_osc_7777() {
    assert_ignored(b"7777;x");
}

/// OSC 9001 (Windows Terminal) is ignored.
#[test]
fn ignores_osc_9001() {
    assert_ignored(b"9001;x");
}

/// OSC 30001 (push colors) is ignored.
#[test]
fn ignores_osc_30001() {
    assert_ignored(b"30001");
}

/// OSC 30101 (pop colors) is ignored.
#[test]
fn ignores_osc_30101() {
    assert_ignored(b"30101");
}

/// OSC 77119 (WezTerm) is ignored.
#[test]
fn ignores_osc_77119() {
    assert_ignored(b"77119;x");
}

/// OSC 4242, an unknown number, (unknown number) is ignored.
#[test]
fn ignores_osc_unknown() {
    assert_ignored(b"4242;x");
}

/// ConEmu `9;1;100` (sleep) is ignored.
#[test]
fn ignores_conemu_9_1() {
    assert_ignored(b"9;1;100");
}

/// ConEmu `9;2;msg` (message box) is ignored.
#[test]
fn ignores_conemu_9_2() {
    assert_ignored(b"9;2;msg");
}

/// ConEmu `9;3;title` (tab title) is ignored.
#[test]
fn ignores_conemu_9_3() {
    assert_ignored(b"9;3;title");
}

/// ConEmu `9;5` (wait for input) is ignored.
#[test]
fn ignores_conemu_9_5() {
    assert_ignored(b"9;5");
}

/// ConEmu `9;6;macro` (GUI macro) is ignored.
#[test]
fn ignores_conemu_9_6() {
    assert_ignored(b"9;6;macro");
}

/// ConEmu `9;7;cmd` (run process) is ignored.
#[test]
fn ignores_conemu_9_7() {
    assert_ignored(b"9;7;cmd");
}

/// ConEmu `9;8;VAR` (environment variable) is ignored.
#[test]
fn ignores_conemu_9_8() {
    assert_ignored(b"9;8;VAR");
}

/// ConEmu `9;10;1` (xterm emulation) is ignored.
#[test]
fn ignores_conemu_9_10() {
    assert_ignored(b"9;10;1");
}

/// ConEmu `9;11;comment` (comment) is ignored.
#[test]
fn ignores_conemu_9_11() {
    assert_ignored(b"9;11;comment");
}

/// OSC 1337 `SetMark` is ignored.
#[test]
fn ignores_osc_1337_set_mark() {
    assert_ignored(b"1337;SetMark");
}

/// OSC 1337 `RemoteHost` is ignored.
#[test]
fn ignores_osc_1337_remote_host() {
    assert_ignored(b"1337;RemoteHost=user@host");
}

/// OSC 1337 `StealFocus` is ignored.
#[test]
fn ignores_osc_1337_steal_focus() {
    assert_ignored(b"1337;StealFocus");
}

/// OSC 1337 `ClearScrollback` is ignored.
#[test]
fn ignores_osc_1337_clear_scrollback() {
    assert_ignored(b"1337;ClearScrollback");
}

/// OSC 1337 `SetProfile` is ignored.
#[test]
fn ignores_osc_1337_set_profile() {
    assert_ignored(b"1337;SetProfile=Default");
}

/// OSC 1337 `SetBadgeFormat` is ignored.
#[test]
fn ignores_osc_1337_set_badge_format() {
    assert_ignored(b"1337;SetBadgeFormat=Zm9v");
}

/// OSC 1337 `RequestAttention` is ignored.
#[test]
fn ignores_osc_1337_request_attention() {
    assert_ignored(b"1337;RequestAttention=yes");
}

/// OSC 1337 `CopyToClipboard` is ignored.
#[test]
fn ignores_osc_1337_copy_to_clipboard() {
    assert_ignored(b"1337;CopyToClipboard=");
}

/// OSC 1337 `EndCopy` is ignored.
#[test]
fn ignores_osc_1337_end_copy() {
    assert_ignored(b"1337;EndCopy");
}

/// OSC 1337 `HighlightCursorLine` is ignored.
#[test]
fn ignores_osc_1337_highlight_cursor_line() {
    assert_ignored(b"1337;HighlightCursorLine=yes");
}

/// OSC 1337 `MultipartFile` is ignored.
#[test]
fn ignores_osc_1337_multipart_file() {
    assert_ignored(b"1337;MultipartFile=name=Zm9v");
}

/// OSC 1337 `SetColors` is ignored.
#[test]
fn ignores_osc_1337_set_colors() {
    assert_ignored(b"1337;SetColors=fg=ff0000");
}

/// OSC 1337 `SetKeyLabel` is ignored.
#[test]
fn ignores_osc_1337_set_key_label() {
    assert_ignored(b"1337;SetKeyLabel=F1=x");
}

/// OSC 1337 `ReportCellSize` is ignored.
#[test]
fn ignores_osc_1337_report_cell_size() {
    assert_ignored(b"1337;ReportCellSize");
}

/// OSC 1337 `UnicodeVersion` is ignored.
#[test]
fn ignores_osc_1337_unicode_version() {
    assert_ignored(b"1337;UnicodeVersion=9");
}

/// OSC 1337 `NoSuchKey` is ignored.
#[test]
fn ignores_osc_1337_unknown_key() {
    assert_ignored(b"1337;NoSuchKey=1");
}

/// OSC 777 `preexec`, a subcommand other than `notify`, is ignored.
#[test]
fn ignores_osc_777_preexec() {
    assert_ignored(b"777;preexec");
}

/// OSC 777 `precmd`, a subcommand other than `notify`, is ignored.
#[test]
fn ignores_osc_777_precmd() {
    assert_ignored(b"777;precmd");
}

/// OSC 777 `other`, a subcommand other than `notify`, is ignored.
#[test]
fn ignores_osc_777_other() {
    assert_ignored(b"777;other;x");
}

/// Sun's `OSC l` (window title) is ignored.
#[test]
fn ignores_sun_osc_l() {
    assert_ignored(b"lsun title");
}

/// Sun's `OSC L` (icon title) is ignored.
#[test]
fn ignores_sun_osc_upper_l() {
    assert_ignored(b"Lsun icon");
}
