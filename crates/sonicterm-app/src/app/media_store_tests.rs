//! Pins the image store's sharing contract: a snapshot of an unchanged list is the same
//! allocation every time, a write after a snapshot copies the entries so the held frame is
//! unaffected, and a write with no snapshot alive edits in place without copying.

use std::sync::Arc;

use sonicterm_render_model::InlineImage;

use super::InlineImageStore;

/// A one-pixel image with `id`.
fn image(id: u64) -> InlineImage {
    InlineImage {
        id,
        row: 0,
        col: 0,
        scroll_mark: 0,
        width: 1,
        height: 1,
        bgra: Arc::from(vec![0u8; 4]),
        kitty: None,
    }
}

#[test]
fn snapshots_of_an_unchanged_list_share_one_allocation() {
    // Two frames of an idle pane hold the very same list, so collection copies no entries.
    let store = InlineImageStore::default();
    store.lock().push(image(1));
    let first = store.try_snapshot().unwrap();
    let second = store.try_snapshot().unwrap();
    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn a_write_after_a_snapshot_leaves_the_held_frame_unchanged() {
    // The decoder adds an image while a frame still holds the old list: the frame keeps what it
    // collected, and the next snapshot sees the new image in a new allocation.
    let store = InlineImageStore::default();
    store.lock().push(image(1));
    let held = store.try_snapshot().unwrap();
    store.lock().push(image(2));
    assert_eq!(held.iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![1]);
    let next = store.try_snapshot().unwrap();
    assert_eq!(next.iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![1, 2]);
    assert!(!Arc::ptr_eq(&held, &next));
    assert!(Arc::ptr_eq(&held.first().unwrap().bgra, &next.first().unwrap().bgra), "pixels shared");
}

#[test]
fn a_write_with_no_frame_alive_edits_in_place() {
    // With every snapshot dropped, a write reuses the list's own allocation instead of copying.
    let store = InlineImageStore::default();
    store.lock().push(image(1));
    let before = Arc::as_ptr(&store.try_snapshot().unwrap());
    store.lock().push(image(2));
    let after = store.try_snapshot().unwrap();
    assert_eq!(Arc::as_ptr(&after), before, "no snapshot was alive, so nothing was copied");
    assert_eq!(after.len(), 2);
}

#[test]
fn a_snapshot_never_waits_for_a_writer() {
    // Frame collection must not block on the decoder: while the list is locked, a snapshot is
    // refused at once and the frame retries.
    let store = InlineImageStore::default();
    let held = store.lock();
    assert!(store.try_snapshot().is_none());
    drop(held);
    assert!(store.try_snapshot().is_some());
}
