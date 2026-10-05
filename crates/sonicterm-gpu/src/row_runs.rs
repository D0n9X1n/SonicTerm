//! A row's style runs: the grouping the row emitter shapes, shared with a diagnostic inspector.
//!
//! A row is reduced to its visible cells (wide continuations dropped, original columns kept), and
//! those cells are cut into runs wherever bold or italic changes; colour never splits a run. A run
//! is a borrowed slice of the visible cells, so grouping allocates nothing per run. Only a run that
//! is actually shaped has its text built, by [`materialize_run_text`]; the emitter calls it after
//! the ASCII fast path and the no-font-stack check, so neither of those builds any text.

use sonicterm_render_model::boundary::grid::grid::{Cell, CellFlags, Row};
use sonicterm_text::shape::{run_is_ascii_fast, RunStyle};

/// One style run: its cells, borrowed from the row's visible cells in increasing column order,
/// and the bold/italic style every one of them carries.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShapeRun<'row, 'cells> {
    /// The run's `(column, cell)` entries, a slice of [`visible_cells`]'s list.
    pub(crate) cells: &'cells [(u16, &'row Cell)],
    /// The style that bounds the run.
    pub(crate) style: RunStyle,
}

/// The row's visible cells with their original column indices: every cell except a wide
/// character's continuation, which belongs to the cell before it.
pub(crate) fn visible_cells(row: &Row) -> Vec<(u16, &Cell)> {
    row.iter()
        .enumerate()
        .filter(|(_, cell)| !cell.flags.contains(CellFlags::WIDE_CONT))
        .map(|(col, cell)| (col as u16, cell))
        .collect()
}

/// Cut `cells` into style runs, in order. Each run extends while bold and italic hold and ends at
/// the first cell of another style; foreground, background and other attributes do not split it.
/// Every cell lands in exactly one run, and no run is empty.
pub(crate) fn row_shape_runs<'row, 'cells>(
    cells: &'cells [(u16, &'row Cell)],
) -> impl Iterator<Item = ShapeRun<'row, 'cells>> {
    let mut run_start = 0;
    std::iter::from_fn(move || {
        let first = cells.get(run_start)?;
        let style = RunStyle::from_cell(first.1);
        let run_end = cells[run_start..]
            .iter()
            .position(|(_, cell)| RunStyle::from_cell(cell) != style)
            .map_or(cells.len(), |offset| run_start + offset);
        let run = ShapeRun { cells: &cells[run_start..run_end], style };
        run_start = run_end;
        Some(run)
    })
}

#[cfg(test)]
thread_local! {
    /// Runs materialized on this thread; tests read it to pin which paths build text.
    static MATERIALIZED_RUNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Runs [`materialize_run_text`] has built on this thread, for tests.
#[cfg(test)]
pub(crate) fn materialized_runs() -> u64 {
    MATERIALIZED_RUNS.with(std::cell::Cell::get)
}

/// The text a run is shaped from and, for each of its UTF-8 bytes, the column of the cell that
/// byte came from: each cell's character then its extras, in order, the cell's original column
/// recorded once per byte appended.
pub(crate) fn materialize_run_text(cells: &[(u16, &Cell)]) -> (String, Vec<u16>) {
    #[cfg(test)]
    MATERIALIZED_RUNS.with(|count| count.set(count.get() + 1));
    let mut text = String::with_capacity(cells.len() * 2);
    let mut cell_cols: Vec<u16> = Vec::with_capacity(cells.len() * 2);
    for (col, cell) in cells {
        let start = text.len();
        text.push(cell.ch);
        if let Some(extras) = cell.extras() {
            for ch in extras.chars() {
                text.push(ch);
            }
        }
        let appended = text.len() - start;
        for _ in 0..appended {
            cell_cols.push(*col);
        }
    }
    (text, cell_cols)
}

/// Test hook: the style runs the row emitter cuts `row` into, as `(text, bold, italic,
/// ascii_fast)`, through the emitter's own grouping, ASCII fast-path predicate and text
/// materializer. Every run's text is built here, ASCII runs included, which the emitter never
/// does for them; nothing is resolved or shaped. Not a supported API.
#[doc(hidden)]
#[must_use]
pub fn __row_shape_runs(row: &Row) -> Vec<(String, bool, bool, bool)> {
    let cells = visible_cells(row);
    row_shape_runs(&cells)
        .map(|run| {
            let (text, _) = materialize_run_text(run.cells);
            (text, run.style.bold, run.style.italic, run_is_ascii_fast(run.cells))
        })
        .collect()
}

#[cfg(test)]
#[path = "row_runs_tests.rs"]
mod row_runs_tests;
