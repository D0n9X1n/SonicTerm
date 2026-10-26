use super::*;
use sonicterm_types::{CellFlags, Color, HyperlinkId};

pub(super) fn blank() -> Cell {
    Cell::default()
}

pub(super) fn plain_cell(character: char) -> Cell {
    Cell::plain(character, Color::Default, Color::Default, CellFlags::empty())
}

pub(super) fn ch_bold(character: char) -> Cell {
    Cell::plain(character, Color::Default, Color::Default, CellFlags::BOLD)
}

/// A cell carrying every "fat"/flag channel we want to prove survives
/// storage transitions: wide flag, truecolor fg, indexed bg, a hyperlink id,
/// and trailing zero-width extras.
pub(super) fn rich(character: char) -> Cell {
    let mut cell =
        Cell::plain(character, Color::Rgb(10, 20, 30), Color::Indexed(4), CellFlags::WIDE);
    cell.set_hyperlink(Some(HyperlinkId(7)));
    cell.set_extras(Some("\u{0301}".to_string().into_boxed_str())); // combining acute
    cell
}

/// Build a Flat and a content-equal Cluster line from the same cell run list.
pub(super) fn flat_and_cluster(runs: &[(Cell, usize)]) -> (Line, Line) {
    let mut flat_cells = Vec::new();
    let mut clusters = Vec::new();
    for (cell, count) in runs {
        for _ in 0..*count {
            flat_cells.push(cell.clone());
        }
        clusters.push(Cluster { cell: cell.clone(), count: *count });
    }
    (Line::from_flat(flat_cells), Line::from_clusters(clusters))
}

/// Source bans cover the row facade, storage operations and borrowed iterators together.
pub(super) const LINE_SOURCE: &str = concat!(
    include_str!("line.rs"),
    "\n",
    include_str!("line_storage.rs"),
    "\n",
    include_str!("line_iter.rs")
);
