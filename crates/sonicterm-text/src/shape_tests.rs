use super::*;
use sonicterm_types::Color;

/// Build a run cell at column `col` carrying `ch` and `flags`. Colors are
/// irrelevant to the ASCII fast-path gate, so they stay default.
fn cell(col: u16, ch: char, flags: CellFlags) -> (u16, Cell) {
    (col, Cell::plain(ch, Color::Default, Color::Default, flags))
}

/// A plain-ASCII cell with no flags — the common fast-path case.
fn ascii(col: u16, ch: char) -> (u16, Cell) {
    cell(col, ch, CellFlags::empty())
}

#[test]
fn empty_run_is_vacuously_fast() {
    assert!(run_is_ascii_fast(&[]));
}

#[test]
fn pure_ascii_word_without_triggers_takes_fast_path() {
    let run = [ascii(0, 'h'), ascii(1, 'e'), ascii(2, 'l'), ascii(3, 'l'), ascii(4, 'o')];
    assert!(run_is_ascii_fast(&run));
}

#[test]
fn printable_ascii_boundaries_space_and_tilde_are_fast() {
    // 0x20 (space) and 0x7E (~) are the inclusive ends of the fast range.
    assert!(run_is_ascii_fast(&[ascii(0, ' ')]));
    assert!(run_is_ascii_fast(&[ascii(0, '~')]));
}

#[test]
fn control_char_just_below_space_defeats_fast_path() {
    // 0x1F is one below the 0x20 lower bound.
    assert!(!run_is_ascii_fast(&[ascii(0, '\u{1f}')]));
}

#[test]
fn del_just_above_tilde_defeats_fast_path() {
    // 0x7F (DEL) is one above the 0x7E upper bound.
    assert!(!run_is_ascii_fast(&[ascii(0, '\u{7f}')]));
}

#[test]
fn non_ascii_codepoint_defeats_fast_path() {
    assert!(!run_is_ascii_fast(&[ascii(0, 'é')]));
    assert!(!run_is_ascii_fast(&[ascii(0, '你')]));
}

#[test]
fn every_ligature_trigger_forces_the_slow_path() {
    // These are all printable ASCII yet must defer to the shaper so GSUB
    // ligatures (=>, !=, ->, ::, ||, &&, **, <-, >=, __) can compose.
    for t in ['=', '!', '<', '>', '-', '_', ':', '|', '&', '*'] {
        assert!(!run_is_ascii_fast(&[ascii(0, t)]), "trigger {t:?} must not be fast");
    }
}

#[test]
fn one_trigger_taints_an_otherwise_ascii_run() {
    // `a=b` is pure ASCII but the `=` must send the whole run to the shaper.
    let run = [ascii(0, 'a'), ascii(1, '='), ascii(2, 'b')];
    assert!(!run_is_ascii_fast(&run));
}

#[test]
fn non_trigger_punctuation_stays_on_the_fast_path() {
    // `+`, `.`, `/`, `(` are printable ASCII and not in the trigger set.
    for p in ['+', '.', '/', '(', ')', '%'] {
        assert!(run_is_ascii_fast(&[ascii(0, p)]), "punct {p:?} should be fast");
    }
}

#[test]
fn extras_cluster_defeats_fast_path_even_for_plain_ascii_lead() {
    // A combining/ZWJ cluster hangs off `extras`; the fast path cannot
    // represent it and must defer to the shaper.
    let mut c = Cell::plain('a', Color::Default, Color::Default, CellFlags::empty());
    c.set_extras(Some("\u{300}".to_string().into_boxed_str()));
    assert!(!run_is_ascii_fast(&[(0, c)]));
}

#[test]
fn wide_flag_defeats_fast_path() {
    assert!(!run_is_ascii_fast(&[cell(0, 'W', CellFlags::WIDE)]));
}

#[test]
fn wide_cont_flag_defeats_fast_path() {
    assert!(!run_is_ascii_fast(&[cell(0, ' ', CellFlags::WIDE_CONT)]));
}

#[test]
fn plain_style_flags_do_not_defeat_fast_path() {
    // Bold/italic/underline change the face selection, not the fast-path
    // eligibility: an ASCII bold word still skips shaping.
    let flags = CellFlags::BOLD | CellFlags::ITALIC | CellFlags::UNDERLINE;
    assert!(run_is_ascii_fast(&[cell(0, 'A', flags), cell(1, 'B', flags)]));
}

#[test]
fn run_style_reads_plain_cell_as_neither_bold_nor_italic() {
    let c = Cell::plain('a', Color::Default, Color::Default, CellFlags::empty());
    let s = RunStyle::from_cell(&c);
    assert!(!s.bold);
    assert!(!s.italic);
}

#[test]
fn run_style_extracts_each_axis_independently() {
    let bold = Cell::plain('a', Color::Default, Color::Default, CellFlags::BOLD);
    assert_eq!(RunStyle::from_cell(&bold), RunStyle { bold: true, italic: false });

    let italic = Cell::plain('a', Color::Default, Color::Default, CellFlags::ITALIC);
    assert_eq!(RunStyle::from_cell(&italic), RunStyle { bold: false, italic: true });
}

#[test]
fn run_style_captures_bold_italic_combination() {
    let c = Cell::plain('a', Color::Default, Color::Default, CellFlags::BOLD | CellFlags::ITALIC);
    assert_eq!(RunStyle::from_cell(&c), RunStyle { bold: true, italic: true });
}

#[test]
fn run_style_ignores_non_face_flags() {
    // Underline/inverse/wide do not re-resolve the face, so they must not
    // leak into the (bold, italic) run style.
    let flags = CellFlags::UNDERLINE | CellFlags::INVERSE | CellFlags::WIDE;
    let c = Cell::plain('a', Color::Default, Color::Default, flags);
    assert_eq!(RunStyle::from_cell(&c), RunStyle { bold: false, italic: false });
}
