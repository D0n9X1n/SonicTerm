//! Opaque hyperlink id.
//!
//! The actual `HyperlinkRegistry` (and the URI strings it interns) lives in
//! `sonic-core::hyperlink`. We expose only the id here so that types like
//! [`crate::Cell`] can carry a reference without dragging the registry into
//! every consumer.

use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_HYPERLINK_ID: AtomicU64 = AtomicU64::new(1);

/// Opaque id referencing a `Hyperlink` in a `HyperlinkRegistry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HyperlinkId(pub u64);

impl HyperlinkId {
    pub fn next() -> Self {
        Self(NEXT_HYPERLINK_ID.fetch_add(1, Ordering::Relaxed))
    }
}
