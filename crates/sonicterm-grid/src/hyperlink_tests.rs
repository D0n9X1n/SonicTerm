//! Behavior tests for the OSC 8 [`HyperlinkRegistry`].
//!
//! Covers tuple interning (`(id, uri)` as the dedup key), stable-id reuse for
//! repeat interns, distinct ids for differing id-or-uri, and id → `Hyperlink`
//! lookup round-trips including the unknown-id miss.

use super::*;

#[test]
fn intern_dedups_same_key() {
    let mut r = HyperlinkRegistry::new();
    let a = r.intern(Some("x"), "https://example.com");
    let b = r.intern(Some("x"), "https://example.com");
    assert_eq!(a, b, "same (id, uri) must return the same interned id");
    assert_eq!(r.len(), 1);
    assert!(!r.is_empty());
}

#[test]
fn intern_distinct_for_different_uri_or_id() {
    let mut r = HyperlinkRegistry::new();
    let a = r.intern(None, "https://a.example");
    let b = r.intern(None, "https://b.example");
    let c = r.intern(Some("id1"), "https://a.example");
    assert_ne!(a, b, "different uri => distinct id");
    assert_ne!(a, c, "same uri but different client id => distinct id");
    assert_ne!(b, c);
    assert_eq!(r.len(), 3);
}

#[test]
fn intern_id_none_vs_some_are_distinct_keys() {
    // The dedup key is the full `(Option<id>, uri)` tuple: an anonymous
    // link and an id-tagged link to the same URI are different entries.
    let mut r = HyperlinkRegistry::new();
    let anon = r.intern(None, "https://example.com");
    let tagged = r.intern(Some("1"), "https://example.com");
    assert_ne!(anon, tagged);
    assert_eq!(r.len(), 2);
    // Re-interning each key still dedups to its own id.
    assert_eq!(anon, r.intern(None, "https://example.com"));
    assert_eq!(tagged, r.intern(Some("1"), "https://example.com"));
    assert_eq!(r.len(), 2);
}

#[test]
fn lookup_roundtrip_preserves_id_and_uri() {
    let mut r = HyperlinkRegistry::new();
    let hid = r.intern(Some("k"), "https://example.com/path");
    let link = r.lookup(hid).expect("interned link resolves");
    assert_eq!(link.id.as_deref(), Some("k"));
    assert_eq!(link.uri, "https://example.com/path");
}

#[test]
fn lookup_roundtrip_for_anonymous_link() {
    let mut r = HyperlinkRegistry::new();
    let hid = r.intern(None, "https://anon.example");
    let link = r.lookup(hid).expect("interned link resolves");
    assert_eq!(link.id, None);
    assert_eq!(link.uri, "https://anon.example");
}

#[test]
fn lookup_unknown_returns_none() {
    let mut r = HyperlinkRegistry::new();
    // Intern one link so the registry is non-empty, then probe a never-issued
    // id. `HyperlinkId::next()` counts up from 1, so `u64::MAX` is never issued.
    let real = r.intern(Some("k"), "https://example.com");
    assert!(r.lookup(HyperlinkId(u64::MAX)).is_none());
    assert!(r.lookup(real).is_some());
}

#[test]
fn empty_registry_reports_empty() {
    let r = HyperlinkRegistry::new();
    assert!(r.is_empty());
    assert_eq!(r.len(), 0);
    assert!(r.lookup(HyperlinkId(1)).is_none());
}
