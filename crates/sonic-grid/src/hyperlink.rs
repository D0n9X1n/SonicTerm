//! OSC 8 hyperlink registry.
//!
//! Cells reference hyperlinks by a compact [`HyperlinkId`] so that we don't
//! duplicate URI strings across thousands of cells. The [`HyperlinkRegistry`]
//! interns `(id, uri)` pairs and hands out stable ids.

use std::collections::HashMap;

// `HyperlinkId` lives in `sonic-types` so value types like `Cell` can carry
// it without depending on this crate. Re-exported for source compatibility.
pub use sonic_types::HyperlinkId;

/// A parsed OSC 8 hyperlink: optional client-supplied id + uri.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hyperlink {
    pub id: Option<String>,
    pub uri: String,
}

/// Interns hyperlinks keyed by `(id, uri)`.
#[derive(Debug, Default)]
pub struct HyperlinkRegistry {
    by_key: HashMap<(Option<String>, String), HyperlinkId>,
    by_id: HashMap<HyperlinkId, Hyperlink>,
}

impl HyperlinkRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the id for `(id, uri)`, creating a new entry on first sight.
    pub fn intern(&mut self, id: Option<&str>, uri: &str) -> HyperlinkId {
        let key = (id.map(String::from), uri.to_string());
        if let Some(hid) = self.by_key.get(&key) {
            return *hid;
        }
        let hid = HyperlinkId::next();
        let link = Hyperlink { id: key.0.clone(), uri: key.1.clone() };
        self.by_key.insert(key, hid);
        self.by_id.insert(hid, link);
        hid
    }

    pub fn lookup(&self, hid: HyperlinkId) -> Option<&Hyperlink> {
        self.by_id.get(&hid)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}
