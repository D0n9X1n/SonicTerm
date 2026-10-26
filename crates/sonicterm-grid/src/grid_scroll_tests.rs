use super::*;
use crate::grid::grid_test_support::*;

#[test]
fn scrollback_limit_zero_recycles_rows_without_history() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(0);
    for character in "abcd".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.linefeed();
    for character in "efgh".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.linefeed();

    assert_eq!(grid.scrollback_len(), 0);
    assert_eq!(text(&grid, 0), "efgh");
    assert_eq!(text(&grid, 1), "    ");
}

#[test]
fn region_scroll_does_not_touch_scrollback_for_partial_region() {
    let mut grid = Grid::new(4, 4);
    for (row, label) in ["1111", "2222", "3333", "4444"].into_iter().enumerate() {
        grid.goto(row as u16, 0);
        for character in label.chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }

    grid.scroll_region_up(1, 2, 1);

    assert_eq!(grid.scrollback_len(), 0);
    assert_eq!(text(&grid, 0), "1111");
    assert_eq!(text(&grid, 1), "3333");
    assert_eq!(text(&grid, 2), "    ");
    assert_eq!(text(&grid, 3), "4444");
}

#[test]
fn full_region_scroll_routes_to_scrollback() {
    let mut grid = Grid::new(4, 2);
    for character in "abcd".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.goto(1, 0);
    for character in "efgh".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }

    grid.scroll_region_up(0, 1, 1);

    assert_eq!(grid.scrollback_len(), 1);
    assert_eq!(grid.scrollback_row(0).unwrap()[0].ch, 'a');
    assert_eq!(text(&grid, 0), "efgh");
}

/// Fixed-position and partial-region scrolling clear every uncertain wrap boundary.
#[test]
fn structural_scrolls_clear_uncertain_wrap_provenance() {
    let mut down = Grid::new(4, 4);
    down.set_soft_wrapped_from_previous(1, true);
    down.set_soft_wrapped_from_previous(2, true);
    down.scroll_down(1);
    assert!(down.rows_iter().all(|row| !row.soft_wrapped_from_previous()));

    let mut region = Grid::new(4, 5);
    for row in 1..5 {
        region.set_soft_wrapped_from_previous(row, true);
    }
    region.scroll_region_up(1, 3, 1);
    assert!(!region.row(1).soft_wrapped_from_previous());
    assert!(!region.row(2).soft_wrapped_from_previous());
    assert!(!region.row(3).soft_wrapped_from_previous());
    assert!(!region.row(4).soft_wrapped_from_previous());
}

/// Recycled blank rows drop stale provenance while an evicted predecessor remains detectable.
#[test]
fn scroll_recycling_resets_blanks_and_preserves_missing_predecessor_evidence() {
    let mut grid = Grid::new(2, 2);
    grid.set_scrollback_limit(1);
    grid.set_soft_wrapped_from_previous(1, true);

    grid.scroll_up(1);
    assert!(!grid.row(1).soft_wrapped_from_previous());

    grid.set_soft_wrapped_from_previous(1, true);
    grid.scroll_up(1);

    assert_eq!(grid.scrollback_evicted(), 1);
    assert!(grid.scrollback_row(0).unwrap().soft_wrapped_from_previous());
    assert!(!grid.row(1).soft_wrapped_from_previous());
}

#[test]
fn custom_fill_is_used_for_scroll_rows() {
    let mut grid = Grid::new(3, 1);
    let fill = Cell::plain('.', Color::Indexed(1), Color::Indexed(2), CellFlags::empty());

    grid.scroll_up_with(1, fill.clone());

    assert_eq!(text(&grid, 0), "...");
    assert_eq!(grid.row(0)[0].fg, fill.fg);
    assert_eq!(grid.row(0)[0].bg, fill.bg);
}

/// A plain cell holding `character`.
fn plain(character: char) -> Cell {
    Cell::plain(character, Color::Default, Color::Default, CellFlags::empty())
}

/// Write `text_len` cycling letters, offset by `seed`, from column 0 of the cursor row, then CR LF.
fn print_short_line(grid: &mut Grid, seed: usize, text_len: usize) {
    for column in 0..text_len {
        let letter = char::from(b'a' + ((seed + column) % 26) as u8);
        grid.put_char(letter, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.carriage_return();
    grid.linefeed();
}

/// The text of a row, every column.
fn row_text(row: &Line) -> String {
    row.iter().map(|cell| cell.ch).collect()
}

/// Rewrite the bottom visible row as one of three eject shapes: 0 uniform (compresses), 1 thirty
/// letters then blanks (trims), 2 a letter in every column (stays flat).
fn write_bottom_row_kind(grid: &mut Grid, kind: usize) {
    let cols = usize::from(grid.cols);
    let bottom = grid.rows - 1;
    let row = grid.row_mut(bottom);
    match kind {
        0 => row.fill_range(0, cols, plain('=')),
        1 => {
            for column in 0..30 {
                assert!(row.set(column, plain(char::from(b'a' + (column % 26) as u8))));
            }
            row.fill_range(30, cols, Cell::default());
        }
        _ => {
            for column in 0..cols {
                assert!(row.set(column, plain(char::from(b'a' + (column % 26) as u8))));
            }
        }
    }
}

/// Short output in history keeps only its content: 1,000 rows of 40 characters at 200 columns
/// retain at most half their flat figure, every one is stored trimmed, and each reads back.
#[test]
fn short_rows_in_history_retain_at_most_half_their_flat_bytes() {
    let mut grid = Grid::new(200, 24);
    let history_rows = 1_000;
    let mut seed = 0;
    while grid.scrollback_len() < history_rows {
        print_short_line(&mut grid, seed, 40);
        seed += 1;
    }
    assert_eq!(grid.scrollback_len(), history_rows);
    assert_eq!(grid.scrollback_trimmed_rows(), history_rows);
    let flat_bytes = history_rows * 200 * std::mem::size_of::<Cell>();
    let history_bytes = grid.retained_amount_by_region().history.bytes;
    assert!(
        history_bytes * 2 <= flat_bytes,
        "history retained {history_bytes} bytes against a flat figure of {flat_bytes}"
    );
    for (index, row) in grid.scrollback_iter().enumerate() {
        let expected: String =
            (0..200)
                .map(|column| {
                    if column < 40 {
                        char::from(b'a' + ((index + column) % 26) as u8)
                    } else {
                        ' '
                    }
                })
                .collect();
        assert_eq!(row_text(row), expected, "history row {index}");
    }
}

/// Trimmed history whose fill is the blank a resize pads with survives a ±1 column drag: it stays
/// trimmed at each width, reads back unchanged, and its retained bytes return exactly.
#[test]
fn trimmed_history_survives_an_adjacent_resize_round_trip() {
    let mut grid = Grid::new(80, 24);
    for seed in 0..600 {
        print_short_line(&mut grid, seed, 20);
    }
    let trimmed = grid.scrollback_trimmed_rows();
    assert!(trimmed >= 500, "precondition: history must be trimmed ({trimmed} rows)");
    let history_bytes = grid.retained_amount_by_region().history.bytes;
    let texts: Vec<String> = grid.scrollback_iter().map(row_text).collect();

    grid.resize(81, 24);
    assert_eq!(grid.scrollback_trimmed_rows(), trimmed, "widening by the pad keeps the form");
    grid.resize(80, 24);

    assert_eq!(grid.scrollback_trimmed_rows(), trimmed);
    assert_eq!(grid.retained_amount_by_region().history.bytes, history_bytes);
    assert_eq!(grid.scrollback_iter().map(row_text).collect::<Vec<_>>(), texts);
}

/// A trimmed row whose fill differs from the blank a resize pads with cannot stay trimmed once
/// widened: it becomes flat at exactly the new width, and after a ±1 drag it reads back as before
/// and its history is no larger than the same rows stored flat at the wider width.
#[test]
fn widening_a_trimmed_row_with_a_coloured_fill_stores_it_flat_at_exact_width() {
    let coloured = Cell::plain(' ', Color::Default, Color::Indexed(4), CellFlags::empty());
    let mut grid = Grid::new(80, 24);
    let bottom = grid.rows - 1;
    for _ in 0..400 {
        let row = grid.row_mut(bottom);
        for (column, character) in "prompt$".chars().enumerate() {
            assert!(row.set(column, plain(character)));
        }
        row.fill_range(7, 80, coloured.clone());
        grid.scroll_up(1);
    }
    let written = grid.scrollback_trimmed_rows();
    assert!(written >= 370, "precondition: the written rows must be trimmed ({written})");
    let cells_before: Vec<Vec<Cell>> =
        grid.scrollback_iter().map(|row| row.iter().cloned().collect()).collect();

    grid.resize(81, 24);
    assert_eq!(grid.scrollback_trimmed_rows(), 0);
    for row in grid.scrollback_iter().filter(|row| !row.is_clustered()) {
        let crate::line::LineStorage::Flat(cells) = row.storage() else {
            panic!("widened rows are flat")
        };
        assert_eq!(cells.capacity(), 81, "one exact reserve, no doubling");
    }
    grid.resize(80, 24);

    let cells_after: Vec<Vec<Cell>> =
        grid.scrollback_iter().map(|row| row.iter().cloned().collect()).collect();
    assert_eq!(cells_after, cells_before);
    let flat_at_widest = grid.scrollback_len() * 81 * std::mem::size_of::<Cell>();
    assert!(grid.retained_amount_by_region().history.bytes <= flat_at_widest);
}

/// Every eject leaves a clean bottom row: flat, exactly the grid's width, every cell the scroll
/// fill, no soft wrap, and a fresh content stamp — whether the ejected row was compressed, trimmed
/// or kept flat, below history capacity and at it. Each ejected row keeps the form its shape earns.
#[test]
fn every_eject_path_leaves_a_clean_blank_row() {
    let fill = Cell::plain(' ', Color::Default, Color::Indexed(2), CellFlags::empty());
    for limit in [1_000usize, 6] {
        let mut grid = Grid::new(200, 5);
        grid.set_scrollback_limit(limit);
        let bottom = grid.rows - 1;
        for step in 0..60usize {
            write_bottom_row_kind(&mut grid, step % 3);
            grid.row_mut(bottom).set_soft_wrapped_from_previous(true);
            let seq_before = grid.content_seq();
            grid.scroll_up_with(1, fill.clone());

            let blank = grid.row(bottom);
            assert!(blank.storage().is_flat(), "limit {limit} step {step}");
            assert_eq!(blank.len(), 200);
            assert!(blank.iter().all(|cell| *cell == fill), "limit {limit} step {step}");
            assert!(!blank.soft_wrapped_from_previous());
            assert!(blank.content_seq() > seq_before);
            assert!(grid.scrollback_len() <= limit);
            // The row ejected now was written `rows - 1` scrolls ago; earlier ejects are the
            // grid's initial blank rows.
            if let Some(written_at) = step.checked_sub(usize::from(grid.rows) - 1) {
                let Some(ejected) = grid.scrollback_iter().last() else { panic!("history") };
                match written_at % 3 {
                    0 => assert!(ejected.is_clustered(), "uniform rows compress"),
                    1 => assert!(ejected.is_trimmed(), "short rows trim"),
                    _ => assert!(ejected.storage().is_flat(), "full rows stay flat"),
                }
            }
        }
    }
}

/// Scrolling creates or grows at most one row buffer per scrolled row, for uniform, trimmed, flat
/// and mixed streams, below history capacity and at it (where the recycled oldest row is never
/// rebuilt from its old cells).
#[test]
fn scrolling_takes_at_most_one_row_buffer_per_row() {
    for limit in [100_000usize, 50] {
        for pattern in [[0usize, 0, 0], [1, 1, 1], [2, 2, 2], [0, 1, 2]] {
            let mut grid = Grid::new(200, 5);
            grid.set_scrollback_limit(limit);
            let warmup = if limit == 50 { 80 } else { 0 };
            for step in 0..warmup {
                write_bottom_row_kind(&mut grid, pattern[step % 3]);
                grid.scroll_up(1);
            }
            let start = grid.row_storage_allocs();
            let scrolled = 300u64;
            for step in 0..scrolled as usize {
                write_bottom_row_kind(&mut grid, pattern[step % 3]);
                grid.scroll_up(1);
            }
            let taken = grid.row_storage_allocs() - start;
            assert!(
                taken <= scrolled,
                "limit {limit} pattern {pattern:?}: {taken} row buffers for {scrolled} rows"
            );
        }
    }
}
