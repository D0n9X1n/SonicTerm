use super::*;

/// The body row text a grid would hold for `update`: every body row's segments, without styles.
fn grid(kind: RowRunWorkload, update: u32) -> Vec<String> {
    ROW_RUN_BODY_ROWS
        .map(|row| kind.body_segments(row, update).into_iter().map(|(_, _, text)| text).collect())
        .collect()
}

/// Ordering 1, the scan first: the update is seen complete before the frame that draws it presents.
/// The first later presentation marks it, cancels the owed forced presentation, and the next update
/// is awaited after; a repeat frame count marks nothing.
#[test]
fn an_update_seen_before_its_presentation_is_marked_by_the_next_frame() {
    let kind = RowRunWorkload::Powerline;
    let mut handshake = PresentedUpdates::new(kind, 2);
    handshake.observe_grid(&grid(kind, 0), 10);
    assert_eq!(handshake.observe_frames(10), None, "no frame presented since the sighting");
    assert_eq!(handshake.observe_frames(11), Some(0));
    assert!(!handshake.take_redraw_request(), "the presentation settled the owed redraw");
    assert_eq!(handshake.observe_frames(12), None, "update 1 is not seen yet");
    handshake.observe_grid(&grid(kind, 1), 12);
    assert_eq!(handshake.observe_frames(13), Some(1));
    assert!(handshake.done());
    handshake.observe_grid(&grid(kind, 1), 20);
    assert_eq!(handshake.observe_frames(21), None, "nothing after the last update");
}

/// Ordering 2, the presentation first: the frame that drew the update presented before the first
/// scan saw it, and nothing presents again on its own. The sighting owes exactly one forced
/// presentation, and that presentation marks the update; without it the handshake would stall.
#[test]
fn an_update_seen_after_its_presentation_owes_one_forced_presentation() {
    let kind = RowRunWorkload::CjkTui;
    let mut handshake = PresentedUpdates::new(kind, 1);
    // Frame 11 already showed update 0; every scan after it reads 11.
    for _ in 0..50 {
        handshake.observe_grid(&grid(kind, 0), 11);
        assert_eq!(handshake.observe_frames(11), None, "an unchanged count marks nothing");
    }
    assert!(handshake.take_redraw_request(), "the sighting owes a forced presentation");
    assert!(!handshake.take_redraw_request(), "at most once per sighting");
    handshake.observe_grid(&grid(kind, 0), 11);
    assert!(
        !handshake.take_redraw_request(),
        "a repeat scan of the same sighting owes nothing more"
    );
    assert_eq!(handshake.observe_frames(12), Some(0), "the forced presentation marks it");
}

/// A split PTY delivery: only the first rows of the update have reached the grid. No sighting is
/// recorded, so no presentation marks it, until every body row shows the update.
#[test]
fn a_partially_delivered_update_is_not_marked() {
    for kind in [RowRunWorkload::Powerline, RowRunWorkload::CjkTui, RowRunWorkload::Unique] {
        let mut handshake = PresentedUpdates::new(kind, 2);
        handshake.observe_grid(&grid(kind, 0), 1);
        assert_eq!(handshake.observe_frames(2), Some(0), "{kind:?}");
        // Update 1's first half arrived; the rest of the screen still shows update 0.
        let (new, old) = (grid(kind, 1), grid(kind, 0));
        let half = new.len() / 2;
        let split: Vec<String> = new[..half].iter().chain(&old[half..]).cloned().collect();
        handshake.observe_grid(&split, 2);
        assert!(!handshake.take_redraw_request(), "{kind:?}: a partial update owes nothing");
        assert_eq!(handshake.observe_frames(3), None, "{kind:?}: a partial update is not marked");
        // Only the last body row is stale: still incomplete.
        let mut last_stale = new.clone();
        *last_stale.last_mut().unwrap() = old.last().unwrap().clone();
        handshake.observe_grid(&last_stale, 3);
        assert_eq!(handshake.observe_frames(4), None, "{kind:?}: the last row is still stale");
        handshake.observe_grid(&new, 4);
        assert_eq!(handshake.observe_frames(5), Some(1), "{kind:?}: the whole update arrived");
    }
}

/// A sighting that disappears before a later frame starts over: the update must be seen again,
/// and the forced presentation it owed is withdrawn with it.
#[test]
fn a_sighting_that_disappears_starts_over() {
    let kind = RowRunWorkload::Unique;
    let mut handshake = PresentedUpdates::new(kind, 1);
    handshake.observe_grid(&grid(kind, 0), 5);
    handshake.observe_grid(&vec![String::new(); PresentedUpdates::body_row_count()], 5);
    assert!(!handshake.take_redraw_request(), "the withdrawn sighting owes nothing");
    assert_eq!(handshake.observe_frames(6), None, "the rows were cleared");
    handshake.observe_grid(&grid(kind, 0), 7);
    assert_eq!(handshake.observe_frames(7), None, "seen again, at frame 7");
    assert_eq!(handshake.observe_frames(8), Some(0));
}

/// The stream phase keeps counting past the warm updates, so once every warm update is marked a grid
/// showing the next update owes no forced presentation and marks nothing: the stream is not perturbed.
#[test]
fn a_done_handshake_ignores_the_streams_later_updates() {
    let kind = RowRunWorkload::Powerline;
    let mut handshake = PresentedUpdates::new(kind, 1);
    handshake.observe_grid(&grid(kind, 0), 1);
    assert_eq!(handshake.observe_frames(2), Some(0));
    handshake.observe_grid(&grid(kind, 1), 2);
    assert!(!handshake.take_redraw_request(), "a stream update owes no presentation");
    assert_eq!(handshake.observe_frames(3), None, "a stream update is not marked");
}

/// A grid with fewer rows than the body never counts as complete, whatever its rows show.
#[test]
fn an_undersized_grid_is_never_complete() {
    let kind = RowRunWorkload::Powerline;
    let mut handshake = PresentedUpdates::new(kind, 1);
    let mut short = grid(kind, 0);
    short.pop();
    handshake.observe_grid(&short, 1);
    assert_eq!(handshake.observe_frames(2), None);
}

/// Every workload's digits differ between successive warm updates on every body row, so the probe
/// can tell each row of the awaited update from the same row of the one before it.
#[test]
fn successive_warm_updates_have_distinct_fingerprints_on_every_body_row() {
    for kind in [RowRunWorkload::Powerline, RowRunWorkload::CjkTui, RowRunWorkload::Unique] {
        for update in 0..crate::scenarios::ROW_RUN_WARM_UPDATES {
            for row in ROW_RUN_BODY_ROWS {
                assert_ne!(
                    PresentedUpdates::fingerprint(kind, row, update),
                    PresentedUpdates::fingerprint(kind, row, update + 1),
                    "{kind:?} row {row} update {update}"
                );
            }
        }
    }
}

/// The first body row is grid index 1 (screen row 2), and the body spans 68 rows.
#[test]
fn the_body_rows_map_to_grid_indices() {
    assert_eq!(PresentedUpdates::first_body_index(), 1);
    assert_eq!(PresentedUpdates::body_row_count(), 68);
}

/// The scan's real input: each workload's update bytes parsed into a 250x70 grid, wide characters'
/// continuation cells included. A complete update is seen whole; a split PTY delivery (the first
/// half of the next update's bytes) is not, until the rest of its bytes arrive.
#[test]
fn a_parsed_grid_is_complete_only_once_every_update_byte_arrived() {
    use sonicterm_vt::vt::{CaptureStagingPool, Parser};
    for kind in [RowRunWorkload::Powerline, RowRunWorkload::CjkTui, RowRunWorkload::Unique] {
        let mut parser =
            Parser::new_with_staging_pool(Grid::new(250, 70), None, CaptureStagingPool::new());
        let mut handshake = PresentedUpdates::new(kind, 2);
        parser.advance(&kind.update_bytes(0));
        handshake.observe_grid(&PresentedUpdates::body_rows(parser.grid()), 1);
        assert_eq!(handshake.observe_frames(2), Some(0), "{kind:?}: update 0 parsed whole");
        let next = kind.update_bytes(1);
        let (head, tail) = next.split_at(next.len() / 2);
        parser.advance(head);
        handshake.observe_grid(&PresentedUpdates::body_rows(parser.grid()), 2);
        assert_eq!(handshake.observe_frames(3), None, "{kind:?}: half of update 1 arrived");
        parser.advance(tail);
        handshake.observe_grid(&PresentedUpdates::body_rows(parser.grid()), 3);
        assert_eq!(handshake.observe_frames(4), Some(1), "{kind:?}: the rest of update 1 arrived");
    }
}
