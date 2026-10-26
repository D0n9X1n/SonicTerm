use super::super::grid_test_support::GRID_SOURCE as GRID_CHECKOUT;
use super::*;
use crate::grid::grid_test_support::*;

fn content_rows_since(grid: &Grid, seq: u64) -> Vec<usize> {
    grid.visible_rows_changed_since(seq).collect()
}

#[test]
fn content_sequence_excludes_cursor_and_presentation_only_dirtiness() {
    let mut grid = Grid::new(8, 3);
    let before = grid.content_seq();

    grid.goto(1, 2);
    grid.carriage_return();
    grid.tab();
    grid.backspace();
    grid.mark_all_dirty();

    assert_eq!(grid.content_seq(), before);
    assert!(content_rows_since(&grid, before).is_empty());
}

#[test]
fn content_sequence_marks_only_the_row_whose_cells_changed() {
    let mut grid = Grid::new(8, 3);
    grid.goto(1, 0);
    let before = grid.content_seq();

    grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());

    assert!(grid.content_seq() > before);
    assert_eq!(content_rows_since(&grid, before), vec![1]);
}

#[test]
fn primary_scroll_keeps_existing_rows_at_their_absolute_content_identity() {
    let mut grid = Grid::new(4, 3);
    grid.goto(0, 0);
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    grid.goto(1, 0);
    grid.put_char('b', Color::Default, Color::Default, CellFlags::empty());
    grid.goto(2, 0);
    grid.put_char('c', Color::Default, Color::Default, CellFlags::empty());
    let before = grid.content_seq();

    grid.scroll_up(1);

    assert_eq!(grid.scrollback_len(), 1);
    assert_eq!(text(&grid, 0).chars().next(), Some('b'));
    assert_eq!(content_rows_since(&grid, before), vec![2]);
}

#[test]
fn alternate_screen_scroll_marks_every_fixed_screen_position_changed() {
    let mut grid = Grid::new(4, 3);
    grid.enter_alt_screen();
    let before = grid.content_seq();

    grid.scroll_up(1);

    assert_eq!(grid.scrollback_len(), 0);
    assert_eq!(content_rows_since(&grid, before), vec![0, 1, 2]);
}

#[test]
fn region_scroll_marks_only_the_region_content_changed() {
    let mut grid = Grid::new(4, 5);
    let before = grid.content_seq();

    grid.scroll_region_up(1, 3, 1);

    assert_eq!(content_rows_since(&grid, before), vec![1, 2, 3]);
}

#[test]
fn row_content_stamp_storage_stays_bounded_by_visible_rows() {
    let mut grid = Grid::new(8, 3);
    grid.set_scrollback_limit(5_000);
    for _ in 0..5_000 {
        grid.scroll_up(1);
    }

    assert_eq!(grid.row_content_seq.len(), grid.visible.len());
    assert!(
        grid.row_content_seq.capacity() <= grid.visible.capacity().max(grid.visible.len() * 2),
        "content identity storage follows the visible grid, not scrollback depth"
    );
}

#[test]
fn region_scroll_down_marks_only_the_region_content_changed() {
    let mut grid = Grid::new(4, 5);
    let before = grid.content_seq();

    grid.scroll_region_down(1, 3, 1);

    assert_eq!(content_rows_since(&grid, before), vec![1, 2, 3]);
}

#[test]
fn full_region_primary_scroll_preserves_survivor_identity() {
    let mut grid = Grid::new(4, 3);
    for row in 0..3 {
        grid.goto(row, 0);
        grid.put_char(
            char::from(b'a' + row as u8),
            Color::Default,
            Color::Default,
            CellFlags::empty(),
        );
    }
    let before = grid.content_seq();

    grid.scroll_region_up(0, 2, 1);

    assert_eq!(grid.scrollback_len(), 1);
    assert_eq!(content_rows_since(&grid, before), vec![2]);
}

/// A line replaced through `row_mut` keeps its change stamp when it scrolls into history.
#[test]
fn a_replaced_line_keeps_its_change_stamp_in_history() {
    let mut grid = Grid::new(4, 2);
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    let seq = grid.content_seq();
    *grid.row_mut(0) = Row::from_flat(vec![Cell::default(); 4]);
    assert_eq!(grid.visible_rows_changed_since(seq).collect::<Vec<_>>(), [0]);
    grid.scroll_up(1);
    assert_eq!(grid.scrollback_rows_changed_since(seq).collect::<Vec<_>>(), [0]);
}

/// Every dirty-bit mutation path advances `dirty_generation`, even when the bits it sets were
/// already set; `clear_dirty` and `clear_dirty_rows` never do. A renderer's acknowledgement relies
/// on this to tell the dirt it drew from dirt written after it assembled.
#[test]
fn every_dirty_mutation_advances_the_dirty_generation_and_clearing_does_not() {
    let clean = || {
        let mut grid = Grid::new(6, 6);
        grid.clear_dirty();
        grid
    };
    let advances = |label: &str, grid: &mut Grid, mutate: &dyn Fn(&mut Grid)| {
        let before = grid.dirty_generation();
        mutate(grid);
        assert!(grid.dirty_generation() > before, "{label} advances the dirty generation");
    };

    let mut grid = clean();
    advances("mark_all_dirty", &mut grid, &|grid| grid.mark_all_dirty());
    // Already all dirty: a second mark still advances, so a repeated mark is never mistaken for none.
    advances("a repeated mark_all_dirty", &mut grid, &|grid| grid.mark_all_dirty());
    advances("a printed character", &mut clean(), &|grid| {
        grid.put_char('x', Color::Default, Color::Default, CellFlags::empty())
    });
    advances("scroll_region_up_with on a subregion", &mut clean(), &|grid| {
        grid.scroll_region_up_with(1, 3, 1, Cell::default())
    });
    advances("scroll_region_down_with on a subregion", &mut clean(), &|grid| {
        grid.scroll_region_down_with(1, 3, 1, Cell::default())
    });
    advances("erase_below_with", &mut clean(), &|grid| {
        grid.goto(2, 1);
        grid.erase_below_with(Cell::default())
    });
    advances("erase_above_with", &mut clean(), &|grid| {
        grid.goto(2, 1);
        grid.erase_above_with(Cell::default())
    });
    let wrapped = || {
        let mut grid = clean();
        grid.set_soft_wrapped_from_previous(2, true);
        grid.clear_dirty();
        grid
    };
    advances("clear_soft_wraps_in_visible_range", &mut wrapped(), &|grid| {
        grid.clear_soft_wraps_in_visible_range(0, 6)
    });
    advances("clear_all_soft_wraps", &mut wrapped(), &|grid| grid.clear_all_soft_wraps());
    advances("resize", &mut clean(), &|grid| grid.resize(8, 4));
    let mut alternate = clean();
    advances("enter_alt_screen", &mut alternate, &|grid| grid.enter_alt_screen());
    alternate.clear_dirty();
    advances("leave_alt_screen", &mut alternate, &|grid| grid.leave_alt_screen());

    let mut cleared = clean();
    cleared.mark_all_dirty();
    let generation = cleared.dirty_generation();
    cleared.clear_dirty_rows(&[1].into_iter().collect());
    cleared.clear_dirty();
    assert_eq!(cleared.dirty_generation(), generation, "clearing never advances the generation");
}

/// `clear_dirty_rows` clears exactly the listed rows, ignores slots past the visible rows, keeps
/// every other dirty bit and the generation, and with every row listed equals `clear_dirty`.
#[test]
fn clear_dirty_rows_clears_only_the_listed_rows() {
    let mut grid = Grid::new(4, 4);
    grid.mark_all_dirty();
    let generation = grid.dirty_generation();
    grid.clear_dirty_rows(&[1, 3, 9].into_iter().collect());
    assert_eq!(grid.dirty_rows().collect::<Vec<_>>(), [0, 2]);
    assert_eq!(grid.dirty_generation(), generation);

    let mut every = Grid::new(4, 4);
    every.mark_all_dirty();
    every.clear_dirty_rows(&(0..4).collect());
    let mut whole = Grid::new(4, 4);
    whole.mark_all_dirty();
    whole.clear_dirty();
    assert_eq!(every.dirty_rows().collect::<Vec<_>>(), whole.dirty_rows().collect::<Vec<_>>());
    assert_eq!(every.dirty_count(), 0);
}

/// A generation-bounded clear keeps exactly the rows dirtied after the generation it is given:
/// the dirt a renderer drew is cleared, while a row written after assembly, even one the frame
/// also drew, stays dirty. Clearing never advances the generation.
#[test]
fn a_bounded_clear_keeps_rows_dirtied_after_its_generation() {
    let mut grid = Grid::new(6, 4);
    grid.clear_dirty();
    grid.goto(0, 0);
    grid.put_char('a', Color::Default, Color::Default, CellFlags::empty());
    grid.goto(2, 0);
    grid.put_char('b', Color::Default, Color::Default, CellFlags::empty());
    let assembled = grid.dirty_generation();
    grid.goto(2, 1);
    grid.put_char('c', Color::Default, Color::Default, CellFlags::empty());
    grid.goto(3, 0);
    grid.put_char('d', Color::Default, Color::Default, CellFlags::empty());
    let generation = grid.dirty_generation();
    grid.clear_dirty_through(assembled);
    assert_eq!(dirty_rows_vec(&grid), vec![2, 3], "rows 2 and 3 were written after assembly");
    assert_eq!(grid.dirty_generation(), generation);

    // Moving the cursor marks the rows it leaves and enters, so the generation is read after it.
    let mut limited = Grid::new(6, 4);
    limited.goto(1, 0);
    let assembled = limited.dirty_generation();
    limited.put_char('e', Color::Default, Color::Default, CellFlags::empty());
    limited.clear_dirty_rows_through(&[0, 1, 9].into_iter().collect(), assembled);
    assert_eq!(dirty_rows_vec(&limited), vec![1, 2, 3], "row 0 cleared; 1 newer; 2 and 3 unlisted");

    // A whole-grid mark after assembly, such as a scroll, keeps every row.
    let mut scrolled = Grid::new(6, 4);
    let assembled = scrolled.dirty_generation();
    scrolled.mark_all_dirty();
    scrolled.clear_dirty_through(assembled);
    assert_eq!(dirty_rows_vec(&scrolled), vec![0, 1, 2, 3]);

    // A resize stamps the rows it adds as new dirt, never as older than any receipt.
    let mut grown = Grid::new(6, 2);
    let assembled = grown.dirty_generation();
    grown.resize(6, 4);
    grown.clear_dirty_through(assembled);
    assert_eq!(dirty_rows_vec(&grown), vec![0, 1, 2, 3]);
}

/// Range marks stamp their rows too: after a region scroll or an erase below, a receipt taken
/// before them keeps exactly the rows they touched, as a whole-grid and as a subset clear, and
/// clears the rows dirtied before assembly that they left alone.
#[test]
fn a_bounded_clear_keeps_rows_a_range_mark_touched_after_its_generation() {
    // Each case starts fully dirty before assembly, then applies its range mark after it.
    let scrolled = || {
        let mut grid = Grid::new(6, 6);
        grid.goto(0, 0);
        grid.mark_all_dirty();
        let assembled = grid.dirty_generation();
        grid.scroll_region_up_with(1, 3, 1, Cell::default());
        (grid, assembled)
    };
    let (mut whole, assembled) = scrolled();
    whole.clear_dirty_through(assembled);
    assert_eq!(dirty_rows_vec(&whole), vec![1, 2, 3], "the scrolled region stays dirty");
    let (mut subset, assembled) = scrolled();
    subset.clear_dirty_rows_through(&[2, 3, 4].into_iter().collect(), assembled);
    assert_eq!(dirty_rows_vec(&subset), vec![0, 1, 2, 3, 5], "row 4 clears; 2 and 3 scrolled");

    let erased = || {
        let mut grid = Grid::new(6, 6);
        grid.goto(3, 0);
        grid.mark_all_dirty();
        let assembled = grid.dirty_generation();
        grid.erase_below_with(Cell::default());
        (grid, assembled)
    };
    let (mut whole, assembled) = erased();
    whole.clear_dirty_through(assembled);
    assert_eq!(dirty_rows_vec(&whole), vec![3, 4, 5], "the erased rows stay dirty");
    let (mut subset, assembled) = erased();
    subset.clear_dirty_rows_through(&[1, 4].into_iter().collect(), assembled);
    assert_eq!(dirty_rows_vec(&subset), vec![0, 2, 3, 4, 5], "row 1 clears; 4 was erased");
}

/// The problems with `source`'s writers of `dirty_rows`: every write that sets a bit sits in
/// `mark_row`, `mark_all`, `mark_range` or `resize`, each of which advances the generation, or in
/// the two struct literals that start a grid at generation 0; `clear_dirty` and `clear_dirty_rows`
/// only store `false` and never advance it; nothing else writes the bits or the generation.
fn dirty_writer_problems(source: &str) -> Vec<String> {
    const SETTERS: [&str; 4] = ["mark_row", "mark_all", "mark_range", "resize"];
    const CLEARERS: [&str; 4] =
        ["clear_dirty", "clear_dirty_rows", "clear_dirty_through", "clear_dirty_rows_through"];
    const LITERALS: [&str; 2] = ["new", "enter_alt_screen"];
    let source = source.replace("\r\n", "\n");
    let production = source.split("#[cfg(test)]\n#[path = \"grid_tests.rs\"]").next().unwrap_or("");
    let mut functions: Vec<(String, Vec<&str>)> = Vec::new();
    for line in production.lines() {
        let trimmed = line.trim_start();
        let header = ["pub fn ", "pub(crate) fn ", "pub(super) fn ", "fn "]
            .iter()
            .find_map(|prefix| trimmed.strip_prefix(prefix));
        if let Some(rest) = header {
            let name: String = rest
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            functions.push((name, Vec::new()));
        } else if let Some((_, body)) = functions.last_mut() {
            if !trimmed.starts_with("//") {
                body.push(trimmed);
            }
        }
    }
    // References a function can take into `dirty_rows`; a write through any binding of one counts.
    const ALIASES: [&str; 6] = [
        "dirty_rows.get_mut(",
        "dirty_rows.iter_mut(",
        "dirty_rows.split_at_mut(",
        "dirty_rows.last_mut(",
        "dirty_rows.first_mut(",
        "&mutself.dirty_rows",
    ];
    let mut problems = Vec::new();
    for (name, body) in &functions {
        // The body without whitespace, so a call or a write split across lines reads as one.
        let code: String = body
            .iter()
            .flat_map(|line| line.chars())
            .filter(|character| !character.is_whitespace())
            .collect();
        let aliased = ALIASES.iter().any(|alias| code.contains(alias));
        let writes_through_alias = |value: &str| {
            aliased
                && code.match_indices('*').any(|(at, _)| {
                    let rest = &code[at + 1..];
                    let binding: String = rest
                        .chars()
                        .take_while(|character| character.is_alphanumeric() || *character == '_')
                        .collect();
                    !binding.is_empty() && rest[binding.len()..].starts_with(&format!("={value}"))
                })
        };
        let sets = code.contains("dirty_rows.fill(true)")
            || code.contains("dirty_rows.resize(")
            || code.contains("dirty_rows:vec![true")
            || code.contains("dirty_rows=vec![true")
            || (code.contains("dirty_rows[") && code.contains("]=true"))
            || writes_through_alias("true");
        let stores_false = code.contains("dirty_rows.fill(false)") || writes_through_alias("false");
        let bumps = code.contains("self.dirty_generation=");
        // Writes to the per-row stamps: only the setters keep them, so a receipt reads true stamps.
        let stamps = [
            "dirty_stamps.fill(",
            "dirty_stamps.get_mut(",
            "dirty_stamps.resize(",
            "dirty_stamps.iter_mut(",
            "&mutself.dirty_stamps",
        ]
        .iter()
        .any(|write| code.contains(write));
        let literal = code.contains("dirty_rows:vec![true");
        let name = name.as_str();
        if SETTERS.contains(&name) {
            let advances = bumps || (name == "resize" && code.contains("self.mark_all()"));
            if sets && !advances {
                problems.push(format!("{name} sets dirty bits without advancing the generation"));
            }
            let stamped = stamps || (name == "resize" && code.contains("self.mark_all()"));
            if sets && !stamped {
                problems.push(format!("{name} sets dirty bits without stamping their rows"));
            }
        } else if CLEARERS.contains(&name) {
            if sets || bumps {
                problems
                    .push(format!("{name} must only store false and never advance the generation"));
            }
            if !stores_false {
                problems.push(format!("{name} no longer clears"));
            }
        } else if LITERALS.contains(&name) && literal {
            if !code.contains("dirty_generation:0") {
                problems.push(format!("{name}'s grid literal does not start at generation 0"));
            }
        } else if sets || bumps || stores_false || stamps {
            problems.push(format!("{name} writes dirty bits outside the admitted writers"));
        }
    }
    problems
}

/// `source` with `fixture` added as a production `Grid` method, before the test module.
fn with_writer(source: &str, fixture: &str) -> String {
    let marker = "\n#[cfg(test)]\n#[path = \"grid_tests.rs\"]";
    let at = source.find(marker).expect("grid.rs declares its test module");
    format!("{}\nimpl Grid {{\n{fixture}\n}}\n{}", &source[..at], &source[at..])
}

/// The writer pin over one checkout of `grid.rs`: it passes on the file and rejects a setter that does
/// not advance the generation, a clearing helper that stores `true`, a direct write in a new place,
/// and aliased writes through a reference taken from `dirty_rows` under any binding name. Every
/// fixture is built from the LF-normalized text, so the checks mean the same on a CRLF checkout.
fn check_dirty_writer_pin(checkout: &str) {
    let source = checkout.replace("\r\n", "\n");
    assert_eq!(dirty_writer_problems(checkout), Vec::<String>::new());
    let rejects = |mutated: &str, writer: &str| {
        assert_ne!(mutated, source, "precondition: the {writer} fixture changed the source");
        let problems = dirty_writer_problems(&mutated.replace('\n', "\r\n"));
        assert!(
            problems.iter().any(|problem| problem.starts_with(writer)),
            "{writer}: {problems:?}"
        );
        let problems = dirty_writer_problems(mutated);
        assert!(
            problems.iter().any(|problem| problem.starts_with(writer)),
            "{writer}: {problems:?}"
        );
    };
    rejects(
        &source.replacen(
            "            *slot = true;\n        }\n        self.dirty_generation = self.dirty_generation.wrapping_add(1);",
            "            *slot = true;\n        }",
            1,
        ),
        "mark_range",
    );
    // A range mark that advances the generation but leaves its rows' stamps behind.
    rejects(
        &source.replacen(
            "        for stamp in &mut self.dirty_stamps[low..=high] {\n            *stamp = self.dirty_generation;\n        }\n",
            "",
            1,
        ),
        "mark_range",
    );
    rejects(
        &source.replacen("self.dirty_rows.fill(false);", "self.dirty_rows.fill(true);", 1),
        "clear_dirty ",
    );
    rejects(
        &source.replacen(
            "                self.mark_row(index as u16);\n            }\n        }\n        self.bump();",
            "                self.dirty_rows[index] = true;\n            }\n        }\n        self.bump();",
            1,
        ),
        "clear_soft_wraps_in_visible_range",
    );
    rejects(
        &with_writer(
            &source,
            "    fn aliased_get(&mut self) {\n        let dirty = self\n            .dirty_rows\n            .get_mut(0);\n        if let Some(dirty) = dirty {\n            *dirty = true;\n        }\n    }",
        ),
        "aliased_get",
    );
    rejects(
        &with_writer(
            &source,
            "    fn aliased_for_each(&mut self) {\n        self.dirty_rows.iter_mut().for_each(|flag| *flag = true);\n    }",
        ),
        "aliased_for_each",
    );
    rejects(
        &with_writer(
            &source,
            "    fn aliased_loop(&mut self) {\n        for bit in self.dirty_rows.iter_mut() {\n            *bit = true;\n        }\n    }",
        ),
        "aliased_loop",
    );
}

/// The dirty-bit writers stay closed on an LF and on a CRLF checkout, as Windows CI checks it out.
#[test]
fn dirty_bits_are_set_only_by_writers_that_advance_the_generation() {
    let lf_source = GRID_CHECKOUT.replace("\r\n", "\n");
    check_dirty_writer_pin(&lf_source);
    check_dirty_writer_pin(&lf_source.replace('\n', "\r\n"));
}

/// A renderer's per-row ink record stays valid only while its row's content stamp is unchanged:
/// `row_content_seq_at_abs` reads a visible row's stamp and, once the row scrolls into history,
/// the same stamp from the history line; a write moves only the written row's stamp, and an
/// absolute row past the visible bottom has none.
#[test]
fn row_content_seq_at_abs_follows_a_row_from_the_screen_into_history() {
    let mut grid = Grid::new(4, 3);
    grid.goto(1, 0);
    grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
    let written = grid.row_content_seq_at_abs(1).expect("visible row 1");
    assert!(written > grid.row_content_seq_at_abs(0).expect("visible row 0"));
    assert_eq!(grid.row_content_seq_at_abs(2), grid.row_content_seq_at_abs(0));
    assert_eq!(grid.row_content_seq_at_abs(3), None, "past the visible bottom");

    grid.scroll_up(2);
    assert_eq!(grid.scrollback_len(), 2);
    // Absolute row 1 is now history; it keeps the stamp it had on screen.
    assert_eq!(grid.row_content_seq_at_abs(1), Some(written));
    let before = grid.row_content_seq_at_abs(2);
    grid.goto(1, 0);
    grid.put_char('y', Color::Default, Color::Default, CellFlags::empty());
    assert_eq!(grid.row_content_seq_at_abs(2), before, "an unwritten row keeps its stamp");
    assert!(grid.row_content_seq_at_abs(3) > before, "the written row's stamp advances");
    assert_eq!(grid.row_content_seq_at_abs(1), Some(written), "history is untouched");
}
