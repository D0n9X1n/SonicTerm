//! Differential tests for the fused history pass: a grid resized by the fused code must equal one
//! resized by the reference (pre-fusion) code in every row's cells, storage form, wrap bit and
//! content stamp, in retained bytes and in revision, across seeded scripts of writes, wraps,
//! scrolls, alternate-screen swaps and resizes.

use super::*;
use crate::hyperlink::{HyperlinkId, HyperlinkRegistry};

/// A small deterministic generator, so a failing seed reproduces.
struct Seeded(u64);

impl Seeded {
    /// The next value in `0..bound`.
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % bound
    }
}

/// One script step, applied identically to both grids.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// Print `count` characters of `glyph` with `background`, wrapping at the margin.
    Print { glyph: char, count: u16, background: Color, linked: bool },
    /// A combining mark on the previous cell.
    Combine,
    /// Carriage return and line feed.
    Newline,
    /// Scroll the screen up by `lines`, pushing rows into history.
    Scroll { lines: u16 },
    /// Enter or leave the alternate screen.
    ToggleAlt,
    /// Resize to `cols × rows`.
    Resize { cols: u16, rows: u16 },
}

/// A seeded script of `length` steps biased toward resizes between bursts of output.
fn script(seed: u64, length: usize) -> Vec<Step> {
    let mut random = Seeded(seed);
    let glyphs = ['a', 'Z', ' ', '中', '😀', '-'];
    let backgrounds = [Color::Default, Color::Indexed(4), Color::Rgb(10, 20, 30)];
    (0..length)
        .map(|_| match random.below(10) {
            0..=3 => Step::Print {
                glyph: glyphs[random.below(glyphs.len() as u64) as usize],
                count: 1 + random.below(40) as u16,
                background: backgrounds[random.below(backgrounds.len() as u64) as usize],
                linked: random.below(4) == 0,
            },
            4 => Step::Combine,
            5 => Step::Newline,
            6 => Step::Scroll { lines: 1 + random.below(6) as u16 },
            7 => Step::ToggleAlt,
            _ => Step::Resize {
                // Height-only, narrowing and widening all occur: columns repeat often.
                cols: [4, 7, 12, 12, 20, 33][random.below(6) as usize],
                rows: [2, 3, 5, 8][random.below(4) as usize],
            },
        })
        .collect()
}

/// Apply `step` to `grid`; `fused` picks the resize and alternate-screen return under test. A
/// linked print uses `link`, one id shared by both grids so their cells compare equal.
fn apply(grid: &mut Grid, link_id: HyperlinkId, step: Step, fused: bool) {
    match step {
        Step::Print { glyph, count, background, linked } => {
            let link = linked.then_some(link_id);
            for _ in 0..count {
                grid.put_char_linked(glyph, Color::Default, background, CellFlags::empty(), link);
            }
        }
        Step::Combine => {
            grid.put_char('\u{301}', Color::Default, Color::Default, CellFlags::empty())
        }
        Step::Newline => {
            grid.carriage_return();
            grid.linefeed();
        }
        Step::Scroll { lines } => grid.scroll_up(lines),
        Step::ToggleAlt if grid.is_alt() && fused => grid.leave_alt_screen(),
        Step::ToggleAlt if grid.is_alt() => grid.reference_leave_alt_screen(),
        Step::ToggleAlt => grid.enter_alt_screen(),
        Step::Resize { cols, rows } if fused => grid.resize(cols, rows),
        Step::Resize { cols, rows } => grid.reference_resize(cols, rows),
    }
}

/// `stamps` ranked densely among themselves. The fused code draws fewer sequence numbers than the
/// reference, and a saved primary draws from its own, so raw values drift apart; which rows share a
/// stamp and how all stamps order must not. A step that restamps a row it should not, or misses
/// one, changes the ranks.
fn ranked(stamps: &[u64]) -> Vec<usize> {
    let mut distinct = stamps.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    stamps
        .iter()
        .map(|stamp| distinct.binary_search(stamp).expect("a stamp from the list"))
        .collect()
}

/// Every row of one screen and its history as comparable values (cells, storage form and wrap
/// bit), plus every row stamp, rows then the visible screen's metadata, [`ranked`].
fn screen_rows(grid: &Grid) -> (Vec<(Vec<Cell>, bool, bool, bool)>, Vec<usize>) {
    let rows = grid
        .visible
        .iter()
        .chain(grid.scrollback.iter())
        .map(|line| {
            (
                line.iter().cloned().collect(),
                line.is_clustered(),
                line.is_trimmed(),
                line.soft_wrapped_from_previous(),
            )
        })
        .collect();
    let stamps: Vec<u64> = grid
        .visible
        .iter()
        .chain(grid.scrollback.iter())
        .map(Line::content_seq)
        .chain(grid.row_content_seq.iter().copied())
        .collect();
    (rows, ranked(&stamps))
}

/// Each screen's revision before a step, then any saved primary's, so the revision can be checked
/// for advancing.
fn revisions(grid: &Grid) -> Vec<u64> {
    let mut values = vec![grid.revision()];
    if let Some(primary) = grid.alt_screen.as_deref() {
        values.extend(revisions(primary));
    }
    values
}

/// Assert `fused` and `reference` hold the same rows, stamps and accounting on both screens.
/// `before` holds each grid's [`revisions`] from before the step.
fn assert_same(fused: &Grid, reference: &Grid, before: (&[u64], &[u64]), context: &str) {
    let (fused_rows, fused_stamps) = screen_rows(fused);
    let (reference_rows, reference_stamps) = screen_rows(reference);
    assert_eq!(fused_rows, reference_rows, "{context}: rows differ");
    assert_eq!(fused_stamps, reference_stamps, "{context}: stamps differ");
    // A resize can advance the revision once less: the reference also counted wraps in rows the
    // resize then dropped. The revision is a coarse change signal, so only "advanced" must agree.
    assert_eq!(
        fused.revision() > before.0.first().copied().unwrap_or(0),
        reference.revision() > before.1.first().copied().unwrap_or(0),
        "{context}: revision advanced on one side only"
    );
    assert_eq!(fused.retained_amount(), reference.retained_amount(), "{context}: retained");
    // History's running totals equal a fresh walk on both sides, so every step a script takes,
    // eviction and alternate-screen swaps included, keeps them exact.
    assert!(fused.scrollback.totals_match_rows(), "{context}: fused history totals drifted");
    assert!(
        reference.scrollback.totals_match_rows(),
        "{context}: reference history totals drifted"
    );
    assert_eq!(fused.scrollback_heap_bytes(), reference.scrollback_heap_bytes(), "{context}: heap");
    match (fused.alt_screen.as_deref(), reference.alt_screen.as_deref()) {
        (Some(fused_primary), Some(reference_primary)) => {
            let rest = (before.0.get(1..).unwrap_or(&[]), before.1.get(1..).unwrap_or(&[]));
            assert_same(
                fused_primary,
                reference_primary,
                rest,
                &format!("{context} (saved primary)"),
            );
        }
        (None, None) => {}
        _ => panic!("{context}: one grid is on the alternate screen and the other is not"),
    }
}

/// Seeded scripts of output and resizes leave the fused and reference grids identical after every
/// step: cells, storage forms, wrap bits, the order of every stamp (so a missed or extra restamp
/// shows), whether the revision advanced, and retained bytes, on the primary screen and on a saved primary under the alternate
/// screen.
#[test]
fn fused_resize_matches_the_reference_on_seeded_scripts() {
    for seed in 0..48 {
        let mut fused = Grid::new(12, 4);
        let mut reference = Grid::new(12, 4);
        fused.set_scrollback_limit(64);
        reference.set_scrollback_limit(64);
        let link_id = HyperlinkRegistry::default().intern(None, "https://example.com");
        for (index, step) in script(seed, 160).into_iter().enumerate() {
            let before = (revisions(&fused), revisions(&reference));
            apply(&mut fused, link_id, step, true);
            apply(&mut reference, link_id, step, false);
            let context = format!("seed {seed} step {index} {step:?}");
            assert_same(&fused, &reference, (&before.0, &before.1), &context);
        }
    }
}

/// A height-only resize clears history wraps and stamps only the rows it cleared: every other
/// history row keeps its exact stamp, and the sequence is drawn once.
#[test]
fn a_height_only_resize_restamps_only_the_unwrapped_history_rows() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(16);
    for character in "abcdefgh".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.carriage_return();
    grid.linefeed();
    grid.scroll_up(2);
    let before: Vec<(bool, u64)> = grid
        .scrollback
        .iter()
        .map(|line| (line.soft_wrapped_from_previous(), line.content_seq()))
        .collect();
    assert!(before.iter().any(|(wrapped, _)| *wrapped), "the setup put a wrap into history");
    assert!(before.iter().any(|(wrapped, _)| !*wrapped), "the setup kept an unwrapped history row");
    grid.resize(4, 3);
    for ((was_wrapped, stamp), line) in before.iter().zip(grid.scrollback.iter()) {
        assert!(!line.soft_wrapped_from_previous());
        if *was_wrapped {
            assert!(line.content_seq() > *stamp, "a cleared row kept its stamp");
        } else {
            assert_eq!(line.content_seq(), *stamp, "an untouched row was restamped");
        }
    }
}

/// A column change with no wrapped row anywhere still resizes and restamps every history row.
#[test]
fn a_column_change_without_wraps_still_resizes_and_restamps_history() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(16);
    for _ in 0..4 {
        grid.put_char('x', Color::Default, Color::Default, CellFlags::empty());
        grid.carriage_return();
        grid.linefeed();
    }
    assert!(grid.scrollback.iter().all(|line| !line.soft_wrapped_from_previous()));
    let stamp = grid.scrollback.iter().map(Line::content_seq).max().unwrap();
    grid.resize(9, 2);
    for line in &grid.scrollback {
        assert_eq!(line.len(), 9, "a history row kept its old width");
        assert!(line.content_seq() > stamp, "a history row kept its old stamp");
    }
}

/// One resize walks history once for wraps, widths and stamps, where the separate passes walked it
/// up to four times; a saved primary under the alternate screen adds its own one pass.
#[test]
fn one_resize_walks_history_once() {
    let mut grid = Grid::new(4, 2);
    grid.set_scrollback_limit(16);
    for character in "abcdefghij".chars() {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    grid.scroll_up(3);
    let passes = || super::grid_dirty::HISTORY_PASSES.with(std::cell::Cell::get);
    let before = passes();
    grid.resize(7, 3);
    assert_eq!(passes() - before, 1, "a primary resize");
    grid.enter_alt_screen();
    let before = passes();
    grid.resize(5, 3);
    assert_eq!(passes() - before, 2, "the alternate screen and its saved primary");
}
