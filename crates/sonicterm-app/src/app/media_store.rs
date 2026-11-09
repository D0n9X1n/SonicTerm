//! A pane's inline image list, shared with the frames that draw it.
//!
//! The list sits behind one `Arc`. A frame takes a snapshot by cloning that `Arc`, so collecting
//! an unchanged pane copies no image entries. A writer gets the list through `Arc::make_mut`,
//! which copies it only while a frame still holds the previous snapshot; otherwise it edits in
//! place. Pixels are already shared (`InlineImage::bgra` is an `Arc`), so even that copy moves
//! only the entries, never pixel data.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};
use sonicterm_render_model::InlineImage;

/// One pane's inline images: written by its VT worker and the retention pass, snapshotted by frame
/// collection.
#[derive(Debug, Default)]
pub struct InlineImageStore {
    /// The current list and its revision; a frame holds a clone of this `Arc` until it presents.
    images: Mutex<Revisioned>,
}

/// The list and the revision of its last change, kept under one lock so a snapshot reads both at
/// once.
#[derive(Debug)]
struct Revisioned {
    /// The current list.
    images: Arc<Vec<InlineImage>>,
    /// Drawn from [`NEXT_REVISION`] at creation and at every write, so no two lists, in this store
    /// or any other, ever share one; 0 is never drawn.
    revision: u64,
}

impl Default for Revisioned {
    fn default() -> Self {
        Self { images: Arc::default(), revision: next_revision() }
    }
}

/// The next media revision; starts at 1, so 0 can mean "not tracked".
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

/// A revision no earlier list or change had.
// Ordering: NEXT_REVISION Relaxed; the counter only hands out distinct values and publishes nothing.
fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}

/// A frame's view of a pane's images: the shared list and the revision it was read at. A renderer
/// that saw this revision before saw exactly this list.
#[derive(Clone, Debug, Default)]
pub struct MediaSnapshot {
    /// The list, shared with the store.
    pub images: Arc<Vec<InlineImage>>,
    /// The list's revision; 0 means untracked, which a renderer always recomputes.
    pub revision: u64,
}

impl Deref for MediaSnapshot {
    type Target = Vec<InlineImage>;

    fn deref(&self) -> &Vec<InlineImage> {
        &self.images
    }
}

/// Exclusive access to a pane's images: reads see the list, and a write first makes the list this
/// store's own (copying it only if a frame still shares it) and gives it a new revision.
pub struct InlineImagesGuard<'store>(MutexGuard<'store, Revisioned>);

impl InlineImageStore {
    /// Lock the list, waiting for the current holder.
    pub fn lock(&self) -> InlineImagesGuard<'_> {
        InlineImagesGuard(self.images.lock())
    }

    /// Lock the list only if no one holds it now.
    pub fn try_lock(&self) -> Option<InlineImagesGuard<'_>> {
        self.images.try_lock().map(InlineImagesGuard)
    }

    /// The current list and its revision for a frame, without copying the list; `None` while
    /// another thread holds the lock, so frame collection never waits on a writer. Both are read
    /// under the same lock, so the revision always names this list.
    pub fn try_snapshot(&self) -> Option<MediaSnapshot> {
        self.images.try_lock().map(|state| MediaSnapshot {
            images: Arc::clone(&state.images),
            revision: state.revision,
        })
    }
}

impl Deref for InlineImagesGuard<'_> {
    type Target = Vec<InlineImage>;

    fn deref(&self) -> &Vec<InlineImage> {
        &self.0.images
    }
}

impl DerefMut for InlineImagesGuard<'_> {
    /// Every mutable access draws a new revision, before the list can change, so a frame never
    /// pairs a changed list with an old revision.
    fn deref_mut(&mut self) -> &mut Vec<InlineImage> {
        self.0.revision = next_revision();
        Arc::make_mut(&mut self.0.images)
    }
}

/// A pane's image list as the kitty store and the trims see it: read through `view`, and changed
/// only through `edit`, which they call only when they will change the list. On the store's guard
/// `edit` draws a new revision and may copy a list a frame still holds, so an operation that
/// changes nothing leaves both alone.
pub(in crate::app) trait ImageList {
    /// The list, read-only.
    fn view(&self) -> &[InlineImage];
    /// The list, to change it.
    fn edit(&mut self) -> &mut Vec<InlineImage>;
}

impl ImageList for Vec<InlineImage> {
    fn view(&self) -> &[InlineImage] {
        self
    }

    fn edit(&mut self) -> &mut Vec<InlineImage> {
        self
    }
}

impl ImageList for InlineImagesGuard<'_> {
    fn view(&self) -> &[InlineImage] {
        &self.0.images
    }

    fn edit(&mut self) -> &mut Vec<InlineImage> {
        self
    }
}

#[cfg(test)]
#[path = "media_store_tests.rs"]
mod media_store_tests;
