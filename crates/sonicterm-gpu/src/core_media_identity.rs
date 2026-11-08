//! The frame's inline-media figures, reused while no pane's media changed: the retained byte total
//! and the identity hash. Each pane's list carries the revision its store drew at its last change,
//! so a frame whose `(pane id, revision)` list equals the previous one reuses both figures instead
//! of walking every image and allocating a set to deduplicate pixel buffers.

use sonicterm_render_model::PaneRender;

/// Most panes whose revisions are kept: a frame with more recomputes its figures and keeps no
/// list, so the list never reserves more than `MEDIA_REVISIONS_CAP_BYTES`.
pub(super) const MAX_TRACKED_PANES: usize =
    super::frame_scratch::MEDIA_REVISIONS_CAP_BYTES / std::mem::size_of::<(u64, u64)>();

/// The previous frame's figures and the revisions they were computed from.
#[derive(Debug, Default)]
pub(super) struct MediaIdentityCache {
    /// Each pane's `(id, revision)` the figures below came from, in pane order.
    revisions: Vec<(u64, u64)>,
    /// The retained byte total, each shared pixel buffer counted once.
    retained_bytes: usize,
    /// The media identity hash.
    hash: u64,
    /// Whether the figures are valid for `revisions`.
    valid: bool,
}

impl MediaIdentityCache {
    /// The retained byte total and identity hash of `panes`' media. Reused when every pane's
    /// revision matches the previous frame's and none is 0 (untracked); recomputed otherwise.
    pub(super) fn figures(&mut self, panes: &[PaneRender<'_>]) -> (usize, u64) {
        let unchanged = self.valid
            && self.revisions.len() == panes.len()
            && panes.iter().zip(&self.revisions).all(|(pane, (id, revision))| {
                pane.inline_media_revision != 0
                    && pane.id == *id
                    && pane.inline_media_revision == *revision
            });
        if unchanged {
            // When: unchanged, every pane holds exactly the list these figures were computed from.
            return (self.retained_bytes, self.hash);
        }
        note_recompute();
        // Kitty placements share a pixel buffer and `id`; each buffer is counted once.
        let mut seen = std::collections::HashSet::new();
        self.retained_bytes = panes
            .iter()
            .flat_map(|pane| pane.inline_images.iter())
            .filter(|image| seen.insert(image.id))
            .fold(0usize, |total, image| total.saturating_add(image.bgra.len()));
        self.hash = super::kitty_placeholders::inline_media_hash(
            panes.iter().map(|pane| (pane.id, pane.inline_images.as_slice())),
        );
        if panes.len() > MAX_TRACKED_PANES {
            // When: panes.len() is over MAX_TRACKED_PANES, no list is kept, so every frame recomputes.
            self.release();
            return (self.retained_bytes, self.hash);
        }
        // The revision list is reused in place, so a changed frame allocates only when it grows.
        self.revisions.clear();
        self.revisions.extend(panes.iter().map(|pane| (pane.id, pane.inline_media_revision)));
        self.valid = true;
        (self.retained_bytes, self.hash)
    }

    /// Bytes the revision list reserves; at most `MEDIA_REVISIONS_CAP_BYTES`, since `extend` from an
    /// exact-size iterator reserves exactly the panes it holds.
    pub(super) fn retained_bytes(&self) -> usize {
        self.revisions.capacity().saturating_mul(std::mem::size_of::<(u64, u64)>())
    }

    /// Drop the revision list, so the next frame recomputes; an occlusion trim calls it.
    pub(super) fn release(&mut self) {
        self.revisions = Vec::new();
        self.valid = false;
    }
}

/// Count a recomputation, so a test can tell a reused frame from a recomputed one.
#[cfg(test)]
fn note_recompute() {
    RECOMPUTES.with(|count| count.set(count.get() + 1));
}

/// Without tests, recomputations are not counted.
#[cfg(not(test))]
fn note_recompute() {}

#[cfg(test)]
thread_local! {
    /// Recomputations on this thread, for the reuse tests.
    pub(super) static RECOMPUTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[path = "core_media_identity_tests.rs"]
mod core_media_identity_tests;
