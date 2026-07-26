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
fn distinct_hyperlink_metadata_stays_bounded() {
    const LIMIT: usize = 16 * 1024;
    let mut registry = HyperlinkRegistry::new();
    for index in 0..(LIMIT + 100) {
        registry.intern(None, &format!("https://example.com/{index}"));
    }

    assert!(registry.len() <= LIMIT);
}

#[test]
fn oversized_hyperlink_fields_are_rejected() {
    let mut registry = HyperlinkRegistry::new();
    assert!(registry.try_intern(None, &"u".repeat(MAX_HYPERLINK_URI_BYTES + 1)).is_none());
    assert!(registry
        .try_intern(Some(&"i".repeat(MAX_HYPERLINK_CLIENT_ID_BYTES + 1)), "https://example.com")
        .is_none());
    assert!(registry.is_empty());
}

#[test]
fn hyperlink_string_bytes_stay_bounded() {
    let mut registry = HyperlinkRegistry::new();
    let uri_tail = "x".repeat(MAX_HYPERLINK_URI_BYTES - 32);
    for index in 0..MAX_HYPERLINKS {
        let _ = registry.intern(None, &format!("https://example.com/{index}/{uri_tail}"));
    }

    assert!(registry.retained_bytes <= MAX_HYPERLINK_METADATA_BYTES);
    assert!(registry.len() < MAX_HYPERLINKS, "byte budget should bind before count budget");
}

#[test]
fn empty_registry_reports_empty() {
    let r = HyperlinkRegistry::new();
    assert!(r.is_empty());
    assert_eq!(r.len(), 0);
    assert!(r.lookup(HyperlinkId(1)).is_none());
}

/// Reclaiming unreferenced entries reopens admission, not just memory.
///
/// `retained_bytes` is both the figure reported to a governor and the figure
/// [`HyperlinkRegistry::try_intern`] enforces against. A sweep that freed the
/// maps without decrementing it would release memory while leaving the
/// registry wedged — links would stay dead with the bytes already returned,
/// which is the most confusing possible outcome to diagnose.
#[test]
fn reclaiming_entries_restores_admission_headroom() {
    let mut registry = HyperlinkRegistry::new();
    let mut live = HashSet::new();

    // Fill to the count cap, keeping only every hundredth id live.
    for index in 0..MAX_HYPERLINKS {
        let hid = registry.intern(None, &format!("https://example.com/{index}"));
        if index % 100 == 0 {
            live.insert(hid);
        }
    }
    assert_eq!(registry.len(), MAX_HYPERLINKS, "precondition: the registry is full");
    assert!(
        registry.try_intern(None, "https://example.com/wedged").is_none(),
        "precondition: a full registry refuses new links"
    );
    let full_bytes = registry.retained_bytes();

    let freed = registry.retain_live(&live);

    assert_eq!(freed, MAX_HYPERLINKS - live.len(), "every unreferenced entry must be freed");
    assert_eq!(registry.len(), live.len());
    assert!(
        registry.retained_bytes() < full_bytes,
        "the enforced byte figure must fall: {} !< {full_bytes}",
        registry.retained_bytes()
    );
    assert!(
        registry.try_intern(None, "https://example.com/after-reclaim").is_some(),
        "reclaiming must reopen admission, not merely release memory"
    );
    for hid in &live {
        assert!(registry.lookup(*hid).is_some(), "a referenced link must survive the sweep");
    }
}

/// A swept registry re-interns a freed URI as a *new* id.
///
/// Both maps must be freed together. Dropping only `by_id` would leave the
/// `by_key` entry to hand back an id that no longer resolves, so `lookup`
/// would return `None` for a link the registry reported as interned.
#[test]
fn a_freed_uri_re_interns_to_a_working_id() {
    let mut registry = HyperlinkRegistry::new();
    let stale = registry.intern(None, "https://example.com/stale");
    let kept = registry.intern(None, "https://example.com/kept");

    let live: HashSet<HyperlinkId> = [kept].into_iter().collect();
    assert_eq!(registry.retain_live(&live), 1);
    assert!(registry.lookup(stale).is_none(), "the freed id must stop resolving");

    let reborn = registry.intern(None, "https://example.com/stale");
    assert_ne!(reborn, stale, "a re-interned URI must get a fresh id");
    assert_ne!(reborn, HyperlinkId(0), "re-interning must not return the invalid sentinel");
    assert_eq!(
        registry.lookup(reborn).map(|link| link.uri.as_str()),
        Some("https://example.com/stale"),
        "the re-interned id must resolve to its URI"
    );
}

/// A sweep that frees nothing leaves the registry byte-for-byte unchanged.
#[test]
fn a_sweep_with_everything_live_changes_nothing() {
    let mut registry = HyperlinkRegistry::new();
    let live: HashSet<HyperlinkId> =
        (0..64).map(|i| registry.intern(None, &format!("https://example.com/{i}"))).collect();
    let before_len = registry.len();
    let before_bytes = registry.retained_bytes();

    assert_eq!(registry.retain_live(&live), 0);
    assert_eq!(registry.len(), before_len);
    assert_eq!(registry.retained_bytes(), before_bytes);
}

/// `clear` returns the registry to exactly its constructed state.
#[test]
fn clear_returns_the_registry_to_empty() {
    let mut registry = HyperlinkRegistry::new();
    for i in 0..128 {
        registry.intern(None, &format!("https://example.com/{i}"));
    }

    registry.clear();

    assert!(registry.is_empty());
    assert_eq!(registry.len(), 0);
    assert_eq!(registry.retained_bytes(), 0, "clearing must zero the enforced byte figure");
    assert!(registry.try_intern(None, "https://example.com/after").is_some());
}

/// Rejections say which limit refused them, because the remedies differ.
///
/// All three paths returned a bare `None` before, so a caller could not tell
/// "no room right now" from "never". The parser sweeps the whole grid and
/// retries on a full registry; doing that for an oversized URI is an
/// `O(visible + scrollback)` walk that cannot change the answer, repeated per
/// link for as long as the shell keeps emitting them.
#[test]
fn rejections_report_which_limit_refused_them() {
    let mut registry = HyperlinkRegistry::new();

    // Oversized URI: no reclamation can ever admit it.
    let huge = "x".repeat(MAX_HYPERLINK_URI_BYTES + 1);
    assert_eq!(
        registry.intern_or_reject(None, &huge),
        Err(AdmissionRejection::ItemTooLarge),
        "an oversized URI must report a permanent rejection"
    );
    assert!(
        !AdmissionRejection::ItemTooLarge.is_retryable_after_reclaim(),
        "a permanently-rejected item must not invite a sweep"
    );

    // Oversized client id, same class.
    let huge_id = "i".repeat(MAX_HYPERLINK_CLIENT_ID_BYTES + 1);
    assert_eq!(
        registry.intern_or_reject(Some(&huge_id), "https://example.com/"),
        Err(AdmissionRejection::ItemTooLarge)
    );

    // Count limit: reclamation can relieve this one.
    for index in 0..MAX_HYPERLINKS {
        let _ = registry.intern(None, &format!("https://example.com/{index}"));
    }
    let full = registry.intern_or_reject(None, "https://example.com/one-more");
    assert_eq!(
        full,
        Err(AdmissionRejection::ItemCountLimit),
        "a full registry must report the count limit, not a size limit"
    );
    assert!(
        AdmissionRejection::ItemCountLimit.is_retryable_after_reclaim(),
        "a count limit is exactly what reclamation relieves"
    );
}

/// The byte budget is distinguishable from the count budget.
///
/// Both mean "full", but one is relieved by releasing a large entry and the
/// other by releasing any entry. Reporting them identically loses that.
#[test]
fn the_byte_budget_reports_itself_distinctly() {
    let mut registry = HyperlinkRegistry::new();
    let base = "https://example.com/".to_string() + &"x".repeat(MAX_HYPERLINK_URI_BYTES - 40);

    let mut rejection = None;
    for index in 0..30_000u32 {
        if let Err(reason) = registry.intern_or_reject(None, &format!("{base}{index:08}")) {
            rejection = Some(reason);
            break;
        }
    }

    assert_eq!(
        rejection,
        Some(AdmissionRejection::PerOwnerBudget),
        "maximum-length URIs must exhaust the byte budget before the count cap, \
         and must say so"
    );
    assert!(AdmissionRejection::PerOwnerBudget.is_retryable_after_reclaim());
}

/// Reason codes are stable strings, so an operator can grep across versions.
#[test]
fn reason_codes_are_stable_and_distinct() {
    use std::collections::HashSet;

    let all = [
        AdmissionRejection::ItemTooLarge,
        AdmissionRejection::PerOwnerBudget,
        AdmissionRejection::ProcessBudget,
        AdmissionRejection::ItemCountLimit,
        AdmissionRejection::Cancelled,
    ];
    let codes: HashSet<&str> = all.iter().map(|reason| reason.code()).collect();

    assert_eq!(codes.len(), all.len(), "every rejection must have a distinct code");
    assert_eq!(AdmissionRejection::ItemTooLarge.code(), "item_too_large");
    assert_eq!(AdmissionRejection::ProcessBudget.code(), "process_budget");
    for code in codes {
        assert!(
            code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "codes are grepped from logs, so they must stay snake_case: {code}"
        );
    }
}
