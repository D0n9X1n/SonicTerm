//! OSC 8 hyperlink registry.
//!
//! Cells reference hyperlinks by a compact [`HyperlinkId`] so that we don't
//! duplicate URI strings across thousands of cells. The [`HyperlinkRegistry`]
//! interns `(id, uri)` pairs and hands out stable ids.

use std::collections::HashMap;

// `HyperlinkId` lives in `sonicterm-types` so value types like `Cell` can carry
// it without depending on this crate. Re-exported for source compatibility.
pub use sonicterm_types::HyperlinkId;

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
        if uri.len() > MAX_HYPERLINK_URI_BYTES
            || id.is_some_and(|value| value.len() > MAX_HYPERLINK_CLIENT_ID_BYTES)
        {
            return None;
        }
        let key = (id.map(String::from), uri.to_string());
        if let Some(hid) = self.by_key.get(&key) {
            return Some(*hid);
        }
        if self.by_id.len() >= MAX_HYPERLINKS {
            return None;
        }
        let entry_bytes =
            key.0.as_ref().map_or(0, String::len).saturating_add(key.1.len()).saturating_mul(2);
        if self.retained_bytes.saturating_add(entry_bytes) > MAX_HYPERLINK_METADATA_BYTES {
            return None;
        }
        let hid = HyperlinkId::next();
        let link = Hyperlink { id: key.0.clone(), uri: key.1.clone() };
        self.by_key.insert(key, hid);
        self.by_id.insert(hid, link);
        self.retained_bytes += entry_bytes;
        Some(hid)
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
}

#[cfg(test)]
#[path = "hyperlink_tests.rs"]
mod hyperlink_tests;
