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

/// Many writes under one held snapshot copy the list once, not once per write: after the first
/// write makes the list the store's own, later writes edit in place until the next snapshot. The
/// copy moves entries only, `size_of::<InlineImage>()` bytes each, never pixel data.
#[test]
fn writes_under_one_held_snapshot_copy_the_list_once() {
    let store = InlineImageStore::default();
    for id in 0..64 {
        store.lock().push(image(id));
    }
    let held = store.try_snapshot().unwrap();
    store.lock().push(image(64));
    let after_first = Arc::as_ptr(&store.try_snapshot().unwrap());
    for id in 65..128 {
        store.lock().push(image(id));
    }
    let after_many = store.try_snapshot().unwrap();
    assert_eq!(Arc::as_ptr(&after_many), after_first, "only the first write copied the list");
    assert_eq!(held.len(), 64, "the held frame kept its own list");
    assert_eq!(after_many.len(), 128);
    // What that one copy moved: 64 entries of fixed size, sharing every pixel buffer.
    let copied_bytes = 64 * std::mem::size_of::<InlineImage>();
    assert!(copied_bytes < 64 * 1024, "the copy is entries only: {copied_bytes} bytes");
    assert!(Arc::ptr_eq(&held[0].bgra, &after_many[0].bgra));
}

/// Counted, not inferred from pointers: with one snapshot held, N writes copy the list once (one
/// allocation for the copy, the rest growing it in place), and with no snapshot held they copy
/// nothing. The image pixels are shared either way, so a copy is the entry list alone.
#[test]
fn writes_copy_the_list_at_most_once_per_held_snapshot() {
    let store = InlineImageStore::default();
    for id in 0..8 {
        store.lock().push(image(id));
    }
    let images: Vec<InlineImage> = (100..116).map(image).collect();
    // A held snapshot: the first write copies the 8 entries, later writes edit that copy.
    let held = store.try_snapshot().unwrap();
    let counter = crate::lib_tests::CountAllocations::start();
    for entry in images.iter().take(8).cloned() {
        store.lock().push(entry);
    }
    let with_snapshot = counter.count();
    drop(counter);
    // No snapshot: every write edits the list in place.
    drop(held);
    let counter = crate::lib_tests::CountAllocations::start();
    for entry in images.iter().skip(8).cloned() {
        store.lock().push(entry);
    }
    let without_snapshot = counter.count();
    drop(counter);
    // One allocation for the copy, at most a few for growth; never one per write.
    assert!((1..=4).contains(&with_snapshot), "{with_snapshot} allocations for 8 writes");
    assert!(without_snapshot <= 2, "{without_snapshot} allocations with no snapshot held");
    assert_eq!(store.try_snapshot().unwrap().len(), 24);
}
