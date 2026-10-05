use sonicterm_vt::vt::{CaptureStagingPool, Parser};

use super::*;

/// The grids the handshake is checked on: PR A's 250 x 70 reference, the hosted macOS (237 x 43)
/// and Windows (281 x 58) grids, and the 120 x 22 floor.
const GEOMETRIES: [(u16, u16); 4] = [(250, 70), (237, 43), (281, 58), (120, 22)];

/// The frozen geometry of a contract grid.
fn geometry(cols: u16, rows: u16) -> RowRunGeometry {
    RowRunGeometry::measured(cols, rows).expect("a contract grid")
}

/// A real VT parser on `geometry`'s grid that has parsed `updates` of `kind` in order.
fn parsed(kind: RowRunWorkload, geometry: RowRunGeometry, updates: &[u32]) -> Parser {
    let mut parser = Parser::new_with_staging_pool(
        Grid::new(geometry.cols, geometry.rows),
        None,
        CaptureStagingPool::new(),
    );
    for update in updates {
        parser.advance(&kind.update_bytes(*update, geometry));
    }
    parser
}

const KINDS: [RowRunWorkload; 3] =
    [RowRunWorkload::Powerline, RowRunWorkload::CjkTui, RowRunWorkload::Unique];

/// Ordering 1, the scan first: on every contract grid the whole update is seen before the frame that
/// draws it presents. The first later presentation marks it and cancels the owed forced presentation;
/// a repeat frame count marks nothing, and the next update is awaited after.
#[test]
fn an_update_seen_before_its_presentation_is_marked_by_the_next_frame() {
    for (cols, rows) in GEOMETRIES {
        let frozen = geometry(cols, rows);
        let kind = RowRunWorkload::Powerline;
        let mut handshake = PresentedUpdates::new(kind, frozen, 2);
        let mut parser = parsed(kind, frozen, &[0]);
        handshake.observe_grid(parser.grid(), 10);
        assert_eq!(
            handshake.observe_frames(10),
            None,
            "{cols}x{rows}: no frame since the sighting"
        );
        assert_eq!(handshake.observe_frames(11), Some(0), "{cols}x{rows}");
        assert!(!handshake.take_redraw_request(), "{cols}x{rows}: the presentation settled it");
        handshake.observe_grid(parser.grid(), 11);
        assert_eq!(handshake.observe_frames(12), None, "{cols}x{rows}: update 0 is not update 1");
        parser.advance(&kind.update_bytes(1, frozen));
        handshake.observe_grid(parser.grid(), 12);
        assert_eq!(handshake.observe_frames(13), Some(1), "{cols}x{rows}");
        assert!(handshake.done());
    }
}

/// Ordering 2, the presentation first: on every contract grid the frame that drew the update
/// presented before the first scan saw it, and nothing presents again on its own. The late sighting
/// owes exactly one forced presentation, once, and that presentation marks the update.
#[test]
fn a_late_first_sighting_owes_one_forced_presentation() {
    for (cols, rows) in GEOMETRIES {
        let frozen = geometry(cols, rows);
        let kind = RowRunWorkload::CjkTui;
        let mut handshake = PresentedUpdates::new(kind, frozen, 1);
        let parser = parsed(kind, frozen, &[0]);
        // Frame 11 already showed update 0; every scan after it reads 11.
        for _ in 0..20 {
            handshake.observe_grid(parser.grid(), 11);
            assert_eq!(handshake.observe_frames(11), None, "an unchanged count marks nothing");
        }
        assert!(handshake.take_redraw_request(), "{cols}x{rows}: a forced presentation is owed");
        assert!(!handshake.take_redraw_request(), "{cols}x{rows}: at most once per sighting");
        handshake.observe_grid(parser.grid(), 11);
        assert!(!handshake.take_redraw_request(), "{cols}x{rows}: a repeat scan owes nothing more");
        assert_eq!(
            handshake.observe_frames(12),
            Some(0),
            "{cols}x{rows}: forced presentation marks"
        );
    }
}

/// A split PTY delivery, byte by byte: on every contract grid and workload, update 1 is never seen
/// whole until its final byte (the last byte of the footer text) is parsed, so no prefix is marked, and every cell
/// of the grid is final when it is. This is the in-row case: on 250 x 70, Unique's digits all match
/// at byte 8,775 of 8,828 while nine cells of the last body row are still stale.
#[test]
fn a_byte_split_update_is_marked_only_once_every_cell_is_final() {
    for (cols, rows) in GEOMETRIES {
        let frozen = geometry(cols, rows);
        for kind in KINDS {
            let full = parsed(kind, frozen, &[0, 1]);
            let mut parser = parsed(kind, frozen, &[0]);
            let mut handshake = PresentedUpdates::new(kind, frozen, 2);
            handshake.observe_grid(parser.grid(), 1);
            assert_eq!(handshake.observe_frames(2), Some(0));
            let bytes = kind.update_bytes(1, frozen);
            let mut frames = 2;
            let mut marked_at = None;
            for (index, byte) in bytes.iter().enumerate() {
                parser.advance(std::slice::from_ref(byte));
                handshake.observe_grid(parser.grid(), frames);
                frames += 1;
                if handshake.observe_frames(frames).is_some() {
                    marked_at = Some(index + 1);
                    break;
                }
            }
            let at = format!("{kind:?} {cols}x{rows}");
            assert_eq!(marked_at, Some(bytes.len()), "{at}: marked before the update's final byte");
            let stale = (0..rows)
                .flat_map(|row| {
                    let (actual, wanted) = (parser.grid().row(row), full.grid().row(row));
                    (0..usize::from(cols)).filter(move |col| actual[*col] != wanted[*col])
                })
                .count();
            assert_eq!(stale, 0, "{at}: every cell, styles included, is final when marked");
        }
    }
}

/// A whole sighting that disappears before a later frame starts over, withdrawing the forced
/// presentation it owed; seen again, it is marked by the next frame after the new sighting.
#[test]
fn a_sighting_that_disappears_starts_over() {
    let frozen = geometry(120, 22);
    let mut handshake = PresentedUpdates::new(RowRunWorkload::Unique, frozen, 1);
    handshake.observe(true, 5);
    handshake.observe(false, 5);
    assert!(!handshake.take_redraw_request(), "the withdrawn sighting owes nothing");
    assert_eq!(handshake.observe_frames(6), None, "the sighting was withdrawn");
    handshake.observe(true, 7);
    assert_eq!(handshake.observe_frames(7), None, "seen again, at frame 7");
    assert_eq!(handshake.observe_frames(8), Some(0));
}

/// A grid whose dimensions differ from the frozen geometry never shows an update whole, even when
/// the same update was parsed on it: the handshake waits, and the step invalidates the change.
#[test]
fn a_grid_with_other_dimensions_is_never_whole() {
    let frozen = geometry(237, 43);
    for (cols, rows) in [(237, 44), (238, 43), (250, 70)] {
        let other = geometry(cols, rows);
        let kind = RowRunWorkload::Powerline;
        let parser = parsed(kind, other, &[0]);
        assert!(!PresentedUpdates::complete(kind, frozen, parser.grid(), 0), "{cols}x{rows}");
    }
}

/// The stream phase keeps counting past the warm updates, so once every warm update is marked a grid
/// showing the next update owes no forced presentation and marks nothing.
#[test]
fn a_done_handshake_ignores_the_streams_later_updates() {
    let frozen = geometry(281, 58);
    let kind = RowRunWorkload::Powerline;
    let mut handshake = PresentedUpdates::new(kind, frozen, 1);
    handshake.observe_grid(parsed(kind, frozen, &[0]).grid(), 1);
    assert_eq!(handshake.observe_frames(2), Some(0));
    handshake.observe(true, 2);
    assert!(!handshake.take_redraw_request(), "a stream update owes no presentation");
    assert_eq!(handshake.observe_frames(3), None, "a stream update is not marked");
}

/// Every workload's digits differ between successive warm updates on every body row of every
/// contract grid, so each row of the awaited update differs from the same row of the one before.
#[test]
fn successive_warm_updates_have_distinct_fingerprints_on_every_body_row() {
    for (cols, rows) in GEOMETRIES {
        for kind in KINDS {
            for update in 0..crate::scenarios::ROW_RUN_WARM_UPDATES {
                for row in geometry(cols, rows).body_screen_rows() {
                    assert_ne!(
                        PresentedUpdates::fingerprint(kind, row, update),
                        PresentedUpdates::fingerprint(kind, row, update + 1),
                        "{kind:?} {cols}x{rows} row {row} update {update}"
                    );
                }
            }
        }
    }
}
