//! A pane's inline image list, shared with the frames that draw it.
//!
//! The list sits behind one `Arc`. A frame takes a snapshot by cloning that `Arc`, so collecting
//! an unchanged pane copies no image entries. A writer gets the list through `Arc::make_mut`,
//! which copies it only while a frame still holds the previous snapshot; otherwise it edits in
//! place. Pixels are already shared (`InlineImage::bgra` is an `Arc`), so even that copy moves
//! only the entries, never pixel data.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use parking_lot::{Mutex, MutexGuard};
use sonicterm_render_model::InlineImage;

/// One pane's inline images: written by its VT worker and the retention pass, snapshotted by frame
/// collection.
#[derive(Debug, Default)]
pub struct InlineImageStore {
    /// The current list; a frame holds a clone of this `Arc` until it presents.
    images: Mutex<Arc<Vec<InlineImage>>>,
}

/// Exclusive access to a pane's images: reads see the list, and a write first makes the list this
/// store's own (copying it only if a frame still shares it).
pub struct InlineImagesGuard<'store>(MutexGuard<'store, Arc<Vec<InlineImage>>>);

impl InlineImageStore {
    /// Lock the list, waiting for the current holder.
    pub fn lock(&self) -> InlineImagesGuard<'_> {
        InlineImagesGuard(self.images.lock())
    }

    /// Lock the list only if no one holds it now.
    pub fn try_lock(&self) -> Option<InlineImagesGuard<'_>> {
        self.images.try_lock().map(InlineImagesGuard)
    }

    /// The current list for a frame, without copying it; `None` while another thread holds the
    /// lock, so frame collection never waits on a writer.
    pub fn try_snapshot(&self) -> Option<Arc<Vec<InlineImage>>> {
        self.images.try_lock().map(|images| Arc::clone(&images))
    }
}

impl Deref for InlineImagesGuard<'_> {
    type Target = Vec<InlineImage>;

    fn deref(&self) -> &Vec<InlineImage> {
        &self.0
    }
}

impl DerefMut for InlineImagesGuard<'_> {
    fn deref_mut(&mut self) -> &mut Vec<InlineImage> {
        Arc::make_mut(&mut self.0)
    }
}

#[cfg(test)]
#[path = "media_store_tests.rs"]
mod media_store_tests;
