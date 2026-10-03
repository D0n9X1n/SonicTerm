//! OSC 8 hyperlink registry.
//!
//! Cells reference hyperlinks by a compact [`HyperlinkId`] so that we don't
//! duplicate URI strings across thousands of cells. The [`HyperlinkRegistry`]
//! interns `(id, uri)` pairs and hands out stable ids. Each distinct URI is
//! one shared allocation however many client ids use it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

// `HyperlinkId` lives in `sonicterm-types` so value types like `Cell` can carry
// it without depending on this crate. Re-exported for source compatibility.
pub use sonicterm_types::HyperlinkId;
// Rejection vocabulary is shared, not redefined here: a caller that matches on
// it must be able to use the same variants across every admission point.
use sonicterm_types::retained_hash_table_bytes;
pub use sonicterm_types::AdmissionRejection;

/// Maximum distinct OSC 8 links retained by one parser/grid.
pub const MAX_HYPERLINKS: usize = 16 * 1024;
/// Maximum URI bytes accepted for one OSC 8 link.
pub const MAX_HYPERLINK_URI_BYTES: usize = 8 * 1024;
/// Maximum client-supplied OSC 8 id bytes accepted for one link.
pub const MAX_HYPERLINK_CLIENT_ID_BYTES: usize = 1024;
/// Maximum bytes one hyperlink registry retains: its shared strings and its tables.
pub const MAX_HYPERLINK_METADATA_BYTES: usize = 8 * 1024 * 1024;

/// A parsed OSC 8 hyperlink: optional client-supplied id + uri.
///
/// Both strings are shared with the registry's lookup table, so a link costs
/// no copy of its URI.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hyperlink {
    /// Optional client-supplied id, used to group multi-cell hyperlinks.
    pub id: Option<Arc<str>>,
    /// Target URI string (validated by the application before opening).
    pub uri: Arc<str>,
}

/// The ids interned under one URI: the anonymous link and one per client id.
#[derive(Debug, Default)]
struct UriEntry {
    /// The link with no client id, if interned.
    anonymous: Option<HyperlinkId>,
    /// Links by client id. Empty, with no allocation, until the URI's first client id.
    by_client: HashMap<Arc<str>, HyperlinkId>,
}

impl UriEntry {
    /// True when no link references this URI any more.
    fn is_empty(&self) -> bool {
        self.anonymous.is_none() && self.by_client.is_empty()
    }
}

/// Heap bytes of one `Arc<str>` allocation holding `text`: two reference counts and the bytes,
/// rounded up to the 8-byte alignment of the counts.
fn arc_bytes(text: &str) -> usize {
    (2 * std::mem::size_of::<usize>()).saturating_add(text.len()).next_multiple_of(8)
}

/// Bytes of one inner client-id table at `capacity`.
fn client_table_bytes(capacity: usize) -> usize {
    retained_hash_table_bytes::<Arc<str>, HyperlinkId>(capacity)
}

/// The capacity a table of `len` entries and `capacity` has after one more new key.
///
/// It grows only when full. A full table grows to the smallest bucket count whose load limit holds
/// `len + 1`: 4 or 8 buckets (capacity 3 or 7) while small, else the next power of two of
/// `(len + 1) * 8 / 7`, holding 7/8 of its buckets. That is the standard `HashMap`'s policy for a
/// table without tombstones, which this registry's tables never hold (see [`HyperlinkRegistry`]).
fn capacity_after_insert(len: usize, capacity: usize) -> usize {
    if len < capacity {
        // When: `len < capacity`, the table has a free slot and the insert does not grow it.
        return capacity;
    }
    let wanted = len.max(capacity).saturating_add(1);
    let buckets = match wanted {
        0..=3 => 4,
        4..=7 => 8,
        _ => (wanted.saturating_mul(8) / 7).next_power_of_two(),
    };
    if buckets <= 8 {
        buckets - 1
    } else {
        // When: `buckets > 8`, the table holds at most 7/8 of its buckets.
        buckets / 8 * 7
    }
}

/// The extra bytes a table of `len` entries and `capacity` retains after one more new key.
fn growth_bytes<Key, Value>(len: usize, capacity: usize) -> usize {
    retained_hash_table_bytes::<Key, Value>(capacity_after_insert(len, capacity))
        .saturating_sub(retained_hash_table_bytes::<Key, Value>(capacity))
}

/// Interns hyperlinks keyed by `(id, uri)`.
///
/// Lookup is two hashed lookups on full strings — the URI, then the client id
/// within it — so no lookup scans entries.
///
/// Entries leave a table only when it is rebuilt from its survivors, never by
/// removal in place, so no table holds tombstones and each table's `capacity()`
/// says exactly when its next insert grows it. Admission charges that growth.
#[derive(Debug, Default)]
pub struct HyperlinkRegistry {
    by_uri: HashMap<Arc<str>, UriEntry>,
    by_id: HashMap<HyperlinkId, Hyperlink>,
    /// String allocations held: one `arc_bytes` per distinct URI and per `(uri, id)` client id.
    retained_bytes: usize,
    /// Sum of every inner `by_client` table's bytes, kept so [`Self::retained_bytes`] is O(1).
    inner_table_bytes: usize,
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

    /// The table bytes interning a new `(id, uri)` adds: `by_id` always gains a key, `by_uri` gains
    /// one for a new URI, and a URI's client table gains one for a client id. Each grows only when
    /// full; a URI's first client id creates its table.
    fn insert_growth(&self, id: Option<&str>, uri: &str) -> usize {
        let entry = self.by_uri.get(uri);
        let id_growth =
            growth_bytes::<HyperlinkId, Hyperlink>(self.by_id.len(), self.by_id.capacity());
        let uri_growth = match entry {
            Some(_) => 0,
            None => growth_bytes::<Arc<str>, UriEntry>(self.by_uri.len(), self.by_uri.capacity()),
        };
        let client_growth = match (id, entry) {
            (None, _) => 0,
            (Some(_), Some(entry)) => growth_bytes::<Arc<str>, HyperlinkId>(
                entry.by_client.len(),
                entry.by_client.capacity(),
            ),
            (Some(_), None) => growth_bytes::<Arc<str>, HyperlinkId>(0, 0),
        };
        id_growth.saturating_add(uri_growth).saturating_add(client_growth)
    }

    /// The interned id for `(id, uri)`, if any: two hashed lookups, no scan.
    fn interned_id(&self, id: Option<&str>, uri: &str) -> Option<HyperlinkId> {
        let entry = self.by_uri.get(uri)?;
        match id {
            None => entry.anonymous,
            Some(client_id) => entry.by_client.get(client_id).copied(),
        }
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
            // When: `uri` or client `id` exceeds its byte limit, reject the oversized link.
            return Err(AdmissionRejection::ItemTooLarge);
        }
        if let Some(hid) = self.interned_id(id, uri) {
            // When: `(id, uri)` is already interned, reuse its id.
            return Ok(hid);
        }
        if self.by_id.len() >= MAX_HYPERLINKS {
            // When: `self.by_id.len() >= MAX_HYPERLINKS`, reject a new distinct link.
            return Err(AdmissionRejection::ItemCountLimit);
        }
        let existing_uri = self.by_uri.get_key_value(uri).map(|(shared, _)| Arc::clone(shared));
        let entry_bytes = id.map_or(0, arc_bytes).saturating_add(match existing_uri {
            Some(_) => 0,
            None => arc_bytes(uri),
        });
        // Admit against the figure this registry *reports* after the insert:
        // the new strings and every table the insert grows. Checking only the
        // strings let a table's growth push retention past the ceiling while
        // the admission looked compliant. Nothing is changed before this check.
        let admitted = self
            .retained_bytes()
            .saturating_add(entry_bytes)
            .saturating_add(self.insert_growth(id, uri));
        if admitted > MAX_HYPERLINK_METADATA_BYTES {
            // When: `admitted > MAX_HYPERLINK_METADATA_BYTES`, the strings plus table growth do not fit; reject.
            return Err(AdmissionRejection::PerOwnerBudget);
        }
        let hid = HyperlinkId::next();
        let shared_uri = existing_uri.unwrap_or_else(|| Arc::from(uri));
        let shared_id: Option<Arc<str>> = id.map(Arc::from);
        let entry = self.by_uri.entry(Arc::clone(&shared_uri)).or_default();
        match &shared_id {
            None => entry.anonymous = Some(hid),
            Some(client_id) => {
                // The inner table may grow on insert; charge its capacity change.
                let capacity_before = entry.by_client.capacity();
                entry.by_client.insert(Arc::clone(client_id), hid);
                let capacity_after = entry.by_client.capacity();
                self.inner_table_bytes = self
                    .inner_table_bytes
                    .saturating_sub(client_table_bytes(capacity_before))
                    .saturating_add(client_table_bytes(capacity_after));
            }
        }
        self.by_id.insert(hid, Hyperlink { id: shared_id, uri: shared_uri });
        self.retained_bytes = self.retained_bytes.saturating_add(entry_bytes);
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

    /// Bytes held by the maps' own storage, independent of the strings.
    ///
    /// The maps reserve slots for their entries as well as the strings those
    /// entries point at. Measured at 16,384 links before tables were counted:
    /// 983,040 reported against 4,718,624 actually held, **4.8x**. Capacity
    /// rather than length, because capacity is what the allocator is holding.
    fn table_bytes(&self) -> usize {
        retained_hash_table_bytes::<Arc<str>, UriEntry>(self.by_uri.capacity())
            .saturating_add(retained_hash_table_bytes::<HyperlinkId, Hyperlink>(
                self.by_id.capacity(),
            ))
            .saturating_add(self.inner_table_bytes)
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
    /// A URI stays charged while any link references it and is refunded with
    /// its last one, so freeing genuinely reopens headroom.
    pub fn retain_live(&mut self, live: &HashSet<HyperlinkId>) -> usize {
        let before = self.by_id.len();
        let kept = self.by_id.keys().filter(|hid| live.contains(hid)).count();
        if kept == before {
            // When: `kept == before`, the live-set sweep would remove no entries.
            return 0;
        }

        // Rebuild each table from its survivors rather than removing in place: removal leaves
        // tombstones, after which `capacity()` no longer says when the next insert grows a table.
        self.by_id = std::mem::take(&mut self.by_id)
            .into_iter()
            .filter(|(hid, _)| live.contains(hid))
            .collect();
        self.by_uri = std::mem::take(&mut self.by_uri)
            .into_iter()
            .filter_map(|(uri, mut entry)| {
                entry.anonymous = entry.anonymous.filter(|hid| live.contains(hid));
                entry.by_client = std::mem::take(&mut entry.by_client)
                    .into_iter()
                    .filter(|(_, hid)| live.contains(hid))
                    .collect();
                (!entry.is_empty()).then_some((uri, entry))
            })
            .collect();

        // The maps keep their high-water capacity after `retain`, so a sweep
        // that freed nine tenths of the entries still held the table for all
        // of them. Shrinking is what turns a reclaim into returned memory
        // rather than a smaller number over the same allocation.
        self.by_uri.shrink_to_fit();
        self.by_id.shrink_to_fit();
        for entry in self.by_uri.values_mut() {
            entry.by_client.shrink_to_fit();
        }

        // Recompute after shrinking rather than subtract per entry: the figures are a pure
        // function of the survivors and their post-shrink capacities, so they cannot drift.
        self.retained_bytes = self
            .by_uri
            .iter()
            .map(|(uri, entry)| {
                entry.by_client.keys().map(|client_id| arc_bytes(client_id)).sum::<usize>()
                    + arc_bytes(uri)
            })
            .fold(0usize, usize::saturating_add);
        self.inner_table_bytes = self
            .by_uri
            .values()
            .map(|entry| client_table_bytes(entry.by_client.capacity()))
            .fold(0usize, usize::saturating_add);

        before - self.by_id.len()
    }

    /// Drop every entry unconditionally.
    ///
    /// For transitions that invalidate every referencing cell at once, where
    /// scanning to prove what is live would be wasted work.
    pub fn clear(&mut self) {
        self.by_uri.clear();
        self.by_id.clear();
        // `HashMap::clear` empties the map and keeps the allocation, so a
        // registry that had held 16,384 links still owned ~934 KiB of table
        // while reporting zero. Shrinking returns it, which is what makes the
        // reported figure true rather than merely small.
        self.by_uri.shrink_to_fit();
        self.by_id.shrink_to_fit();
        self.retained_bytes = 0;
        self.inner_table_bytes = 0;
    }
}

#[cfg(test)]
#[path = "hyperlink_tests.rs"]
mod hyperlink_tests;
