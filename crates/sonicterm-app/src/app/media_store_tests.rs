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
    let first = store.try_snapshot().unwrap().images;
    let second = store.try_snapshot().unwrap().images;
    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn a_write_after_a_snapshot_leaves_the_held_frame_unchanged() {
    // The decoder adds an image while a frame still holds the old list: the frame keeps what it
    // collected, and the next snapshot sees the new image in a new allocation.
    let store = InlineImageStore::default();
    store.lock().push(image(1));
    let held = store.try_snapshot().unwrap().images;
    store.lock().push(image(2));
    assert_eq!(held.iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![1]);
    let next = store.try_snapshot().unwrap().images;
    assert_eq!(next.iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![1, 2]);
    assert!(!Arc::ptr_eq(&held, &next));
    assert!(Arc::ptr_eq(&held.first().unwrap().bgra, &next.first().unwrap().bgra), "pixels shared");
}

#[test]
fn a_write_with_no_frame_alive_edits_in_place() {
    // With every snapshot dropped, a write reuses the list's own allocation instead of copying.
    let store = InlineImageStore::default();
    store.lock().push(image(1));
    let before = Arc::as_ptr(&store.try_snapshot().unwrap().images);
    store.lock().push(image(2));
    let after = store.try_snapshot().unwrap().images;
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
    let held = store.try_snapshot().unwrap().images;
    store.lock().push(image(64));
    let after_first = Arc::as_ptr(&store.try_snapshot().unwrap().images);
    for id in 65..128 {
        store.lock().push(image(id));
    }
    let after_many = store.try_snapshot().unwrap().images;
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
    let held = store.try_snapshot().unwrap().images;
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
    assert_eq!(store.try_snapshot().unwrap().images.len(), 24);
}

/// A snapshot's revision names exactly its list: an unchanged store keeps its revision, every
/// mutable access draws a new one (even when the list is edited in place under the same `Arc`
/// pointer, which pointer equality cannot see), and two stores never share a revision.
#[test]
fn every_write_draws_a_revision_no_list_shares() {
    let store = InlineImageStore::default();
    let first = store.try_snapshot().unwrap().revision;
    assert_ne!(first, 0, "0 is never drawn; it means untracked");
    assert_eq!(store.try_snapshot().unwrap().revision, first, "no write, same revision");
    // With no snapshot held, the write edits in place: same pointer, new revision.
    let before = Arc::as_ptr(&store.try_snapshot().unwrap().images);
    store.lock().push(image(1));
    let after = store.try_snapshot().unwrap();
    assert_eq!(Arc::as_ptr(&after.images), before, "edited in place");
    assert!(after.revision > first, "a new revision despite the same pointer");
    // A read through the guard is not a write.
    let read = store.lock().len();
    assert_eq!(read, 1);
    assert_eq!(store.try_snapshot().unwrap().revision, after.revision);
    let other = InlineImageStore::default();
    assert_ne!(other.try_snapshot().unwrap().revision, after.revision, "stores never share one");
}

/// The kitty command `metadata` parses to.
fn kitty(metadata: &str) -> sonicterm_vt::vt::KittyCommand {
    sonicterm_vt::vt::KittyCommand::parse(metadata.as_bytes()).expect("valid command")
}

/// Kitty commands and trims that change nothing keep the list's revision and allocation, so the
/// renderer reuses its media figures and a frame's held snapshot is never copied; an admitted
/// placement draws a new revision.
#[test]
fn commands_and_trims_that_change_nothing_keep_the_revision() {
    use super::super::{apply_kitty, trim_inline_images_charged, InlineMediaPool, KittyOp};
    let store = InlineImageStore::default();
    let pool = InlineMediaPool::new();
    let charge = pool.new_charge();
    let upload = KittyOp::Transmit(kitty("a=t,i=7,f=32,s=1,v=1"), image(1), Some((1, 1)));
    assert_eq!(apply_kitty(&mut store.lock(), upload).0, Some("OK"));
    let held = store.try_snapshot().unwrap();
    let unchanged = [
        // No image 9 is stored.
        ("place of a missing image", KittyOp::Place(kitty("a=p,i=9"), 0, 0, 0)),
        // No placement of 9, and image 7 has only its upload, which `d=i` keeps.
        ("delete of a missing image", KittyOp::Delete(kitty("a=d,d=i,i=9"))),
        ("delete of an unplaced upload", KittyOp::Delete(kitty("a=d,d=i,i=7"))),
        // A selector this subset does not handle.
        ("unsupported delete", KittyOp::Delete(kitty("a=d,d=a"))),
        // An anonymous upload can never be placed, so it is dropped unstored.
        ("anonymous upload", KittyOp::Transmit(kitty("a=t,f=32,s=1,v=1"), image(2), Some((1, 1)))),
    ];
    for (label, operation) in unchanged {
        let mut images = store.lock();
        drop(apply_kitty(&mut images, operation));
        drop(trim_inline_images_charged(&mut images, &charge));
        drop(images);
        let now = store.try_snapshot().unwrap();
        assert_eq!(now.revision, held.revision, "{label} drew a revision");
        assert!(Arc::ptr_eq(&now.images, &held.images), "{label} copied the list");
    }
    let place = KittyOp::Place(kitty("a=p,i=7,p=1"), 0, 0, 0);
    assert_eq!(apply_kitty(&mut store.lock(), place).0, Some("OK"));
    assert_ne!(store.try_snapshot().unwrap().revision, held.revision, "a placement is a change");
}

/// At the placement cap, a refused displayed transmit and a refused placement, each followed by
/// the charged trim, keep the order, the revision and the held snapshot's allocation.
#[test]
fn capacity_refusals_keep_the_revision() {
    use super::super::{
        apply_kitty, trim_inline_images_charged, InlineMediaPool, KittyOp, MAX_KITTY_PLACEMENTS,
    };
    let store = InlineImageStore::default();
    let pool = InlineMediaPool::new();
    let charge = pool.new_charge();
    {
        let mut images = store.lock();
        let upload = KittyOp::Transmit(kitty("a=t,i=7,f=32,s=1,v=1"), image(1), Some((1, 1)));
        assert_eq!(apply_kitty(&mut images, upload).0, Some("OK"));
        for placement in 1..=MAX_KITTY_PLACEMENTS {
            let place = KittyOp::Place(kitty(&format!("a=p,i=7,p={placement}")), 0, 0, 0);
            assert_eq!(apply_kitty(&mut images, place).0, Some("OK"));
        }
        drop(trim_inline_images_charged(&mut images, &charge));
    }
    let held = store.try_snapshot().unwrap();
    let refused = [
        (
            "displayed transmit",
            KittyOp::Transmit(kitty("a=T,i=8,f=32,s=1,v=1"), image(2), Some((1, 1))),
        ),
        ("placement", KittyOp::Place(kitty("a=p,i=7,p=5000"), 0, 0, 0)),
    ];
    for (label, operation) in refused {
        let mut images = store.lock();
        let (reply, _) = apply_kitty(&mut images, operation);
        assert_eq!(reply, Some("ENOSPC:too many placements"), "{label}");
        drop(trim_inline_images_charged(&mut images, &charge));
        drop(images);
        let now = store.try_snapshot().unwrap();
        assert_eq!(now.revision, held.revision, "{label} drew a revision");
        assert!(Arc::ptr_eq(&now.images, &held.images), "{label} copied the list");
    }
}
