use super::*;
use sonicterm_types::{Cell, CellFlags, Color};

/// A row of `width` cells, every `link_every`th carrying a rare-attribute box (0 for none).
fn row(width: usize, glyph: char, link_every: usize) -> Line {
    let cells = (0..width)
        .map(|column| {
            let mut cell = Cell::plain(glyph, Color::Default, Color::Default, CellFlags::empty());
            if link_every > 0 && column % link_every == 0 {
                // A hyperlink is a rare attribute, so the cell allocates its box.
                cell.set_hyperlink(Some(crate::hyperlink::HyperlinkId(7)));
            }
            cell
        })
        .collect();
    Line::from_flat(cells)
}

/// Pushes, front drains (consumed or not), in-place changes and shrinks keep the totals equal to
/// a fresh sum over the rows after every operation, for flat, clustered and trimmed rows with and
/// without rare-attribute boxes.
#[test]
fn every_operation_keeps_the_totals_exact() {
    let mut history = History::default();
    assert!(history.totals_match_rows());
    for index in 0..40 {
        let mut line = row(10 + index % 7, if index % 3 == 0 { ' ' } else { 'x' }, index % 4);
        if index % 5 == 0 {
            // Clustered storage, as the scroll path stores a uniform row.
            let _ = line.compress_releasing();
        } else if index % 5 == 1 {
            // Trimmed storage, as the scroll path stores a row with a blank tail.
            let _ = line.try_trim();
        }
        history.push_back(line);
        assert!(history.totals_match_rows(), "after push {index}");
    }
    // A drain counts from its creation, whether its rows are consumed, reused or dropped.
    let reused = history.drain_front(3).next();
    assert!(history.totals_match_rows(), "after a partially consumed drain");
    drop(history.drain_front(2));
    assert!(history.totals_match_rows(), "after an unconsumed drain");
    history.push_back(reused.expect("a drained row"));
    assert!(history.totals_match_rows(), "after pushing a reused row");
    history.update_each(|line| {
        line.resize(31, Cell::default());
        false
    });
    assert!(history.totals_match_rows(), "after widening every row");
    history.update_each(|line| {
        line.resize(4, Cell::plain('w', Color::Indexed(2), Color::Default, CellFlags::empty()));
        false
    });
    assert!(history.totals_match_rows(), "after narrowing every row");
    history.shrink_rows();
    assert!(history.totals_match_rows(), "after shrinking every row");
    history.compact_spine();
    assert!(history.totals_match_rows(), "after compacting the spine");
    drop(history.drain_front(history.len()));
    assert_eq!(history.totals(), RowCost::default(), "an emptied history retains nothing");
}

/// `update_each` reports whether any call returned true, so a caller can tell whether its change
/// touched a row.
#[test]
fn update_each_reports_whether_any_row_changed() {
    let mut history = History::from_rows([row(4, 'a', 0), row(4, 'b', 0)].into_iter().collect());
    assert!(!history.update_each(|_| false));
    assert!(history.update_each(|line| line[0].ch == 'b'));
}
