//! OSC 8 hyperlink registry.
//!
//! Cells reference hyperlinks by a compact [`HyperlinkId`] so that we don't
//! duplicate URI strings across thousands of cells. The [`HyperlinkRegistry`]
//! interns `(id, uri)` pairs and hands out stable ids.

use std::collections::{HashMap, HashSet};

// `HyperlinkId` lives in `sonicterm-types` so value types like `Cell` can carry
// it without depending on this crate. Re-exported for source compatibility.
pub use sonicterm_types::HyperlinkId;
// Rejection vocabulary is shared, not redefined here: a caller that matches on
// it must be able to use the same variants across every admission point.
pub use sonicterm_types::AdmissionRejection;

/// Buckets a hashbrown table allocates to hold `capacity` entries.
///
/// The table grows to a power of two and keeps one eighth of it free, so a map
/// reporting capacity for 16,384 entries has 28,672 buckets behind it.
fn buckets_for(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    let mut buckets = 1usize;
    while buckets - buckets / 8 < capacity {
        buckets = buckets.saturating_mul(2);
    }
    buckets
}

/// Heap one hashbrown table holds: the `(K, V)` array, one control byte per
/// bucket, and the trailing group replica.
///
/// `capacity * size_of::<(K, V)>()` alone understates it. That was the first
/// correction made to this figure, and it was itself an undercount — by
/// 524,320 bytes at 16,384 links, the same shape as the defect it was fixing.
fn table_bytes_for<K, V>(capacity: usize) -> usize {
    let buckets = buckets_for(capacity);
    if buckets == 0 {
        return 0;
    }
    buckets.saturating_mul(std::mem::size_of::<(K, V)>()).saturating_add(buckets).saturating_add(16)
}

/// Maximum distinct OSC 8 links retained by one parser/grid.
pub const MAX_HYPERLINKS: usize = 16 * 1024;
/// Maximum URI bytes accepted for one OSC 8 link.
pub const MAX_HYPERLINK_URI_BYTES: usize = 8 * 1024;
/// Maximum client-supplied OSC 8 id bytes accepted for one link.
pub const MAX_HYPERLINK_CLIENT_ID_BYTES: usize = 1024;
/// Maximum combined string bytes retained by the two hyperlink lookup maps.
pub const MAX_HYPERLINK_METADATA_BYTES: usize = 8 * 1024 * 1024;

/// A parsed OSC 8 hyperlink: optional client-supplied id + uri.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hyperlink {
    /// Optional client-supplied id, used to group multi-cell hyperlinks.
    pub id: Option<String>,
    /// Target URI string (validated by the application before opening).
    pub uri: String,
}

/// Interns hyperlinks keyed by `(id, uri)`.
#[derive(Debug, Default)]
pub struct HyperlinkRegistry {
    by_key: HashMap<(Option<String>, String), HyperlinkId>,
    by_id: HashMap<HyperlinkId, Hyperlink>,
    retained_bytes: usize,
}

impl HyperlinkRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the id for `(id, uri)`, creating a new entry on first sight.
    ///
    /// Returns the reserved invalid id `0` when memory limits reject a new
    /// link; [`Self::lookup`] then returns `None`. New code that needs to
    /// distinguish rejection should call [`Self::try_intern`].
    pub fn intern(&mut self, id: Option<&str>, uri: &str) -> HyperlinkId {
        self.try_intern(id, uri).unwrap_or(HyperlinkId(0))
    }

    /// Fallible bounded variant of [`Self::intern`].
    pub fn try_intern(&mut self, id: Option<&str>, uri: &str) -> Option<HyperlinkId> {
        self.intern_or_reject(id, uri).ok()
    }

    /// Intern `(id, uri)`, reporting **why** on refusal.
    ///
    /// The three rejection paths are not interchangeable, and treating them as
    /// one costs real work. An oversized URI is refused by a size check that
    /// no amount of reclamation can change, so sweeping the grid and retrying
    /// — which the parser does when the registry is full — is a wasted
    /// `O(visible + scrollback)` scan on the VT hot path, repeated per link
    /// for as long as the shell keeps emitting them.
    ///
    /// [`AdmissionRejection::is_retryable_after_reclaim`] is the distinction
    /// callers need: it separates "no room right now" from "never".
    pub fn intern_or_reject(
        &mut self,
        id: Option<&str>,
        uri: &str,
    ) -> Result<HyperlinkId, AdmissionRejection> {
        if uri.len() > MAX_HYPERLINK_URI_BYTES
            || id.is_some_and(|value| value.len() > MAX_HYPERLINK_CLIENT_ID_BYTES)
        {
            return Err(AdmissionRejection::ItemTooLarge);
        }
        let key = (id.map(String::from), uri.to_string());
        if let Some(hid) = self.by_key.get(&key) {
            return Ok(*hid);
        }
        if self.by_id.len() >= MAX_HYPERLINKS {
            return Err(AdmissionRejection::ItemCountLimit);
        }
        let entry_bytes =
            key.0.as_ref().map_or(0, String::len).saturating_add(key.1.len()).saturating_mul(2);
        // Admit against the figure this registry *reports*, not the string
        // half of it. Checking only strings let the maps' own tables push
        // actual retention 3.7 MiB past the ceiling while every admission
        // looked compliant — a cap that admits by one number and is judged by
        // another is the drift shape this milestone exists to remove.
        if self.retained_bytes().saturating_add(entry_bytes) > MAX_HYPERLINK_METADATA_BYTES {
            return Err(AdmissionRejection::PerOwnerBudget);
        }
        let hid = HyperlinkId::next();
        let link = Hyperlink { id: key.0.clone(), uri: key.1.clone() };
        self.by_key.insert(key, hid);
        self.by_id.insert(hid, link);
        self.retained_bytes += entry_bytes;
        Ok(hid)
    }

    /// Resolve `hid` back to the interned `Hyperlink`.
    pub fn lookup(&self, hid: HyperlinkId) -> Option<&Hyperlink> {
        self.by_id.get(&hid)
    }

    /// Number of interned hyperlinks.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// True when the registry has no interned hyperlinks.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Bytes retained by interned hyperlink ids and URIs.
    ///
    /// This is the same figure the registry already enforces against
    /// [`MAX_HYPERLINK_METADATA_BYTES`], exposed so a governor charges what the
    /// registry actually admits rather than a second estimate of it.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes.saturating_add(self.table_bytes())
    }

    /// Bytes held by the two maps' own storage, independent of the strings.
    ///
    /// `retained_bytes` counts URI and id *contents*. The maps reserve slots
    /// for their entries as well — 56 bytes each per entry, in two tables —
    /// and neither was counted. Measured at 16,384 links: 983,040 reported
    /// against 4,718,624 actually held, **4.8x**.
    ///
    /// Same defect as `Cell`'s rare-attribute boxes: the figure counted what
    /// the pointer addressed and not the table the pointer lived in. Capacity
    /// rather than length, because capacity is what the allocator is holding.
    fn table_bytes(&self) -> usize {
        table_bytes_for::<(Option<String>, String), HyperlinkId>(self.by_key.capacity())
            .saturating_add(table_bytes_for::<HyperlinkId, Hyperlink>(self.by_id.capacity()))
    }

    /// Drop every entry whose id is not in `live`, returning the number freed.
    ///
    /// Admission is append-only in normal operation, so without this a pane
    /// that has seen [`MAX_HYPERLINKS`] distinct links stops interning
    /// permanently: [`Self::try_intern`] returns `None`, [`Self::intern`]
    /// hands back the reserved invalid id `0`, and every subsequent OSC 8
    /// renders as unlinked text for the rest of the session. The slots are
    /// held overwhelmingly by links whose cells scrolled out of scrollback
    /// long ago, so reclaiming them restores a feature the user is still
    /// asking for.
    ///
    /// `live` must be the *complete* set of referencing ids. Freeing an id a
    /// cell still holds silently breaks a link the user can see, which is a
    /// worse defect than the exhaustion this repairs.
    ///
    /// Both maps are freed together and `retained_bytes` is decremented by the
    /// same `(id + uri) * 2` charge admission applied, so freeing genuinely
    /// reopens headroom rather than only releasing memory.
    pub fn retain_live(&mut self, live: &HashSet<HyperlinkId>) -> usize {
        let before = self.by_id.len();
        self.by_id.retain(|hid, _| live.contains(hid));
        if self.by_id.len() == before {
            return 0;
        }

        self.by_key.retain(|_, hid| live.contains(hid));

        // Recompute rather than subtract per entry: the charge is a pure
        // function of the retained keys, so recomputing cannot drift from the
        // figure admission enforces the way accumulated subtractions can.
        self.retained_bytes = self
            .by_key
            .keys()
            .map(|(id, uri)| {
                id.as_ref().map_or(0, String::len).saturating_add(uri.len()).saturating_mul(2)
            })
            .fold(0usize, usize::saturating_add);

        // The maps keep their high-water capacity after `retain`, so a sweep
        // that freed nine tenths of the entries still held the table for all
        // of them. Shrinking is what turns a reclaim into returned memory
        // rather than a smaller number over the same allocation.
        self.by_key.shrink_to_fit();
        self.by_id.shrink_to_fit();

        before - self.by_id.len()
    }

    /// Drop every entry unconditionally.
    ///
    /// For transitions that invalidate every referencing cell at once, where
    /// scanning to prove what is live would be wasted work.
    pub fn clear(&mut self) {
        self.by_key.clear();
        self.by_id.clear();
        // `HashMap::clear` empties the map and keeps the allocation, so a
        // registry that had held 16,384 links still owned ~934 KiB of table
        // while reporting zero. Shrinking returns it, which is what makes the
        // reported figure true rather than merely small.
        self.by_key.shrink_to_fit();
        self.by_id.shrink_to_fit();
        self.retained_bytes = 0;
    }
}

#[cfg(test)]
#[path = "hyperlink_tests.rs"]
mod hyperlink_tests;
