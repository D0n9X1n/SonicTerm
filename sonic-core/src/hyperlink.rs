//! OSC 8 hyperlink registry.
//!
//! Cells reference hyperlinks by a compact [`HyperlinkId`] so that we don't
//! duplicate URI strings across thousands of cells. The [`HyperlinkRegistry`]
//! interns `(id, uri)` pairs and hands out stable ids.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_HYPERLINK_ID: AtomicU64 = AtomicU64::new(1);

/// Opaque id referencing a [`Hyperlink`] in a [`HyperlinkRegistry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HyperlinkId(pub u64);

impl HyperlinkId {
    pub fn next() -> Self {
        Self(NEXT_HYPERLINK_ID.fetch_add(1, Ordering::Relaxed))
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_dedups_same_key() {
        let mut r = HyperlinkRegistry::new();
        let a = r.intern(Some("x"), "https://example.com");
        let b = r.intern(Some("x"), "https://example.com");
        assert_eq!(a, b);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn intern_distinct_for_different_uri_or_id() {
        let mut r = HyperlinkRegistry::new();
        let a = r.intern(None, "https://a.example");
        let b = r.intern(None, "https://b.example");
        let c = r.intern(Some("id1"), "https://a.example");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn lookup_unknown_returns_none() {
        let r = HyperlinkRegistry::new();
        assert!(r.lookup(HyperlinkId(99_999_999)).is_none());
    }

    #[test]
    fn lookup_roundtrip() {
        let mut r = HyperlinkRegistry::new();
        let hid = r.intern(Some("k"), "https://example.com/path");
        let link = r.lookup(hid).expect("present");
        assert_eq!(link.id.as_deref(), Some("k"));
        assert_eq!(link.uri, "https://example.com/path");
    }
}
