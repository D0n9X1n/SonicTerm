use super::*;

/// The identifying row text a grid would hold for `update`: the segments' text, without styles.
fn row(kind: RowRunWorkload, update: u32) -> String {
    kind.body_segments(IDENTIFYING_ROW + 1, update).into_iter().map(|(_, _, text)| text).collect()
}

/// An update is marked presented only after it was seen in the grid and a later frame presented;
/// seeing it in the same frame count again marks nothing, and the next update is awaited after.
#[test]
fn an_update_is_marked_only_after_a_later_frame_presents_it() {
    let kind = RowRunWorkload::Powerline;
    let mut handshake = PresentedUpdates::new(kind, 2);
    assert_eq!(handshake.observe("", 10), None, "the update is not on screen yet");
    assert_eq!(handshake.observe(&row(kind, 0), 10), None, "first seen at frame 10");
    assert_eq!(handshake.observe(&row(kind, 0), 10), None, "no frame presented since");
    assert_eq!(handshake.observe(&row(kind, 0), 11), Some(0));
    assert_eq!(handshake.observe(&row(kind, 0), 12), None, "update 1 is awaited now");
    assert_eq!(handshake.observe(&row(kind, 1), 12), None);
    assert_eq!(handshake.observe(&row(kind, 1), 13), Some(1));
    assert!(handshake.done());
    assert_eq!(handshake.observe(&row(kind, 1), 20), None, "nothing after the last update");
}

/// A sighting that disappears before a later frame starts over: the update must be seen again.
#[test]
fn a_sighting_that_disappears_starts_over() {
    let kind = RowRunWorkload::CjkTui;
    let mut handshake = PresentedUpdates::new(kind, 1);
    assert_eq!(handshake.observe(&row(kind, 0), 5), None);
    assert_eq!(handshake.observe("", 6), None, "the row was cleared");
    assert_eq!(handshake.observe(&row(kind, 0), 7), None, "seen again, at frame 7");
    assert_eq!(handshake.observe(&row(kind, 0), 8), Some(0));
}

/// Every workload's identifying digits differ between successive warm updates, so the probe can
/// tell the awaited update from the one before it.
#[test]
fn successive_warm_updates_have_distinct_fingerprints() {
    for kind in [RowRunWorkload::Powerline, RowRunWorkload::CjkTui, RowRunWorkload::Unique] {
        for update in 0..crate::scenarios::ROW_RUN_WARM_UPDATES {
            assert_ne!(
                PresentedUpdates::fingerprint(kind, update),
                PresentedUpdates::fingerprint(kind, update + 1),
                "{kind:?} update {update}"
            );
        }
    }
}
