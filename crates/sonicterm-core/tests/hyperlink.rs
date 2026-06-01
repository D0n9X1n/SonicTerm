//! Integration tests for the sonicterm-core hyperlink re-exports.

use sonicterm_core::hyperlink::*;

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
