use super::*;
use sonicterm_render_model::boundary::grid::grid::Color;

/// A cell of `character` with `flags` and foreground `fg`, no extras.
fn cell(character: char, flags: CellFlags, fg: Color) -> Cell {
    Cell::plain(character, fg, Color::Default, flags)
}

/// A wide character's continuation cell, as the grid stores it after the lead.
fn continuation(flags: CellFlags) -> Cell {
    cell(' ', flags | CellFlags::WIDE_CONT, Color::Default)
}

/// The run boundaries, styles and columns of a row whose colours change inside one style:
/// - `ab` normal, `c` bold red, `d` bold blue, `e` italic, a wide `中` italic (its continuation
///   dropped), `f` bold italic.
///
/// Colour does not split a run, the continuation is not a visible cell, and every visible cell
/// keeps its original column.
#[test]
fn runs_split_on_bold_and_italic_only_and_keep_original_columns() {
    let italic = CellFlags::ITALIC;
    let row = Row::from_flat(vec![
        cell('a', CellFlags::empty(), Color::Default),
        cell('b', CellFlags::empty(), Color::Default),
        cell('c', CellFlags::BOLD, Color::Indexed(1)),
        cell('d', CellFlags::BOLD, Color::Indexed(4)),
        cell('e', italic, Color::Default),
        cell('中', italic | CellFlags::WIDE, Color::Default),
        continuation(italic),
        cell('f', CellFlags::BOLD | italic, Color::Default),
    ]);
    let cells = visible_cells(&row);
    let columns: Vec<u16> = cells.iter().map(|(col, _)| *col).collect();
    assert_eq!(columns, [0, 1, 2, 3, 4, 5, 7], "the continuation at column 6 is dropped");

    let runs: Vec<(Vec<u16>, bool, bool)> = row_shape_runs(&cells)
        .map(|run| {
            let cols = run.cells.iter().map(|(col, _)| *col).collect();
            (cols, run.style.bold, run.style.italic)
        })
        .collect();
    assert_eq!(
        runs,
        [
            (vec![0, 1], false, false),
            (vec![2, 3], true, false),
            (vec![4, 5], false, true),
            (vec![7], true, true),
        ]
    );
}

/// An empty row has no runs, and a row of one style is one run holding every visible cell.
#[test]
fn an_empty_row_has_no_runs_and_one_style_is_one_run() {
    let empty = Row::from_flat(Vec::new());
    assert_eq!(row_shape_runs(&visible_cells(&empty)).count(), 0);
    let plain = Row::from_flat(vec![cell('x', CellFlags::empty(), Color::Default); 5]);
    let cells = visible_cells(&plain);
    let runs: Vec<usize> = row_shape_runs(&cells).map(|run| run.cells.len()).collect();
    assert_eq!(runs, [5]);
}

/// The materializer appends each cell's character then its extras and records the cell's
/// original column once per UTF-8 byte: `e` plus a combining acute (1 + 2 bytes) at column 3,
/// then a wide `中` (3 bytes) at column 4 whose continuation is not a visible cell, then `z`.
#[test]
fn materialized_text_keeps_extras_and_maps_every_byte_to_its_column() {
    let mut accented = cell('e', CellFlags::empty(), Color::Default);
    accented.set_extras(Some("\u{301}".into()));
    let row = Row::from_flat(vec![
        cell(' ', CellFlags::empty(), Color::Default),
        cell(' ', CellFlags::empty(), Color::Default),
        cell(' ', CellFlags::empty(), Color::Default),
        accented,
        cell('中', CellFlags::WIDE, Color::Default),
        continuation(CellFlags::empty()),
        cell('z', CellFlags::empty(), Color::Default),
    ]);
    let cells = visible_cells(&row);
    let (text, cell_cols) = materialize_run_text(&cells[3..]);
    assert_eq!(text, "e\u{301}中z");
    assert_eq!(cell_cols, [3, 3, 3, 4, 4, 4, 6]);
}

/// A cell carrying 32 combining marks (one lead byte plus 64 extras bytes) materializes its whole
/// cluster, and maps every one of its 65 bytes to the cell's column.
#[test]
fn a_long_combining_cluster_materializes_whole() {
    let mut heavy = cell('e', CellFlags::empty(), Color::Default);
    let marks = "\u{301}".repeat(32);
    heavy.set_extras(Some(marks.as_str().into()));
    let row = Row::from_flat(vec![cell(' ', CellFlags::empty(), Color::Default), heavy]);
    let cells = visible_cells(&row);
    let (text, cell_cols) = materialize_run_text(&cells[1..]);
    assert_eq!(text, format!("e{marks}"));
    assert_eq!(text.len(), 65);
    assert_eq!(cell_cols, vec![1u16; 65]);
}

/// The inspector reports each run's exact text, padding included, its style and whether the
/// emitter takes the ASCII fast path for it:
/// - a bold plain-letter run is fast;
/// - a normal run holding a wide `中` is shaped, and the padding after it joins it;
/// - a normal ligature trigger (`->`) is shaped even though it is ASCII.
#[test]
fn the_inspector_reports_texts_styles_and_ascii_fast_flags() {
    let normal = CellFlags::empty();
    let mut cells = vec![
        cell('a', CellFlags::BOLD, Color::Default),
        cell('b', CellFlags::BOLD, Color::Default),
    ];
    cells.push(cell('中', CellFlags::WIDE, Color::Default));
    cells.push(continuation(normal));
    cells.extend(std::iter::repeat_n(cell(' ', normal, Color::Default), 3));
    assert_eq!(
        __row_shape_runs(&Row::from_flat(cells)),
        [("ab".to_owned(), true, false, true), ("中   ".to_owned(), false, false, false)]
    );
    let arrow = Row::from_flat(vec![
        cell('x', CellFlags::ITALIC, Color::Default),
        cell('-', normal, Color::Default),
        cell('>', normal, Color::Default),
        cell(' ', normal, Color::Default),
    ]);
    assert_eq!(
        __row_shape_runs(&arrow),
        [("x".to_owned(), false, true, true), ("-> ".to_owned(), false, false, false)]
    );
}
