//! Does the hyperlink registry's reported figure track memory actually held?
//!
//! Every accounting defect found in this milestone shared one shape: a figure
//! measured against the number it was derived from rather than against real
//! heap. `Grid::retained_amount` under-reported 1.67x, `queued_output_bytes`
//! restated a constant, and this registry under-reported 4.8x — each with a
//! test that passed.
//!
//! A counting allocator is the check that catches all three, and it has to
//! live in an integration test because `#[global_allocator]` is crate-wide.

#![warn(clippy::min_ident_chars)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sonicterm_grid::hyperlink::{HyperlinkRegistry, MAX_HYPERLINKS, MAX_HYPERLINK_METADATA_BYTES};

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Serialises every test in this file.
///
/// The counting allocator is process-global, so two tests measuring
/// concurrently attribute each other's allocations to whichever one is
/// reading. Measured: all three pass serially and all three fail in parallel,
/// reporting a 5.80x "undercount" that was entirely sibling noise.
///
/// A lock rather than `--test-threads=1`, because the gate cannot be told to
/// serialise one file and a suite that only works under a flag is a suite that
/// will eventually run without it.
static MEASURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Counting;

// SAFETY: Operations forward exact pointers, layouts, and sizes to `System`; atomic bookkeeping allocates nothing and cannot re-enter.
unsafe impl GlobalAlloc for Counting {
    // SAFETY: `layout` must be valid; the atomic byte update is allocation-free before forwarding it unchanged.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: `layout` is the exact valid layout received under `GlobalAlloc::alloc`.
        unsafe { System.alloc(layout) }
    }
    // SAFETY: `ptr` and its original `layout` must match; allocation-free bookkeeping cannot re-enter deallocation.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr` and original `layout` are forwarded unchanged from the valid deallocation call.
        unsafe { System.dealloc(ptr, layout) }
    }
    // SAFETY: `ptr`, original `layout`, and `new_size` must be valid; atomic bookkeeping allocates nothing and cannot re-enter.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        LIVE_BYTES.fetch_sub(layout.size().saturating_sub(new_size), Ordering::Relaxed);
        // SAFETY: `ptr`, original `layout`, and `new_size` are forwarded unchanged under `GlobalAlloc::realloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn held() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// The reported figure must track real heap across sizes.
///
/// Measured before the table term was counted: reported 983,040 against
/// 4,718,624 held at 16 Ki entries — **4.80x**. The tables cost roughly 3.7 MB
/// against 1.0 MB of strings, so the uncounted term was the dominant one.
#[test]
fn reported_bytes_track_real_heap() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    for (count, uri_len) in [(1_000usize, 24usize), (1_000, 60), (8_000, 30)] {
        // Build every URI *before* the measurement window. `format!` allocates
        // a temporary per call, and measuring inside the loop would attribute
        // the harness's own garbage to the registry — the first draft of this
        // test did exactly that and reported a 4.21x undercount that was
        // mostly its own `String`s.
        let uris: Vec<String> =
            (0..count).map(|index| format!("{:width$}", index, width = uri_len)).collect();

        let before = held();
        let mut registry = HyperlinkRegistry::default();
        for uri in &uris {
            registry.intern(None, uri);
        }
        let truth = held().saturating_sub(before);
        let reported = registry.retained_bytes();

        assert!(truth > 0, "precondition: interning must allocate");

        // Understating is the direction that matters: admission judges the
        // reported figure, so an undercount admits past the cap. Overstating
        // only refuses early.
        assert!(
            reported + truth / 100 >= truth,
            "count={count} uri={uri_len}: reported {reported} understates real heap \
             {truth} by {} ({:.2}x)",
            truth.saturating_sub(reported),
            truth as f64 / reported.max(1) as f64
        );

        // And it must not wildly overstate, or the cap refuses work the user
        // asked for while memory is available.
        assert!(
            reported <= truth + truth / 10 + 4096,
            "count={count} uri={uri_len}: reported {reported} overstates real heap {truth}"
        );

        drop(registry);
    }
}

/// Real heap must stop below the cap, not merely the reported figure.
///
/// The case a `reported <= cap` assertion cannot catch: with tables uncounted
/// the registry stopped at 8,388,244 against a cap of 8,388,608 — compliant on
/// its own number — while holding roughly 12.1 MB. **44.5% over the cap it
/// reported itself as meeting.**
#[test]
fn real_heap_stops_below_the_cap() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let uri = "u".repeat(256);

    let uris: Vec<String> = (0..MAX_HYPERLINKS).map(|index| format!("{uri}{index}")).collect();

    let before = held();
    let mut registry = HyperlinkRegistry::default();
    for uri in &uris {
        if registry.try_intern(None, uri).is_none() {
            break;
        }
    }
    let truth = held().saturating_sub(before);

    assert!(!registry.is_empty(), "precondition: the registry admitted something");
    assert!(
        truth > MAX_HYPERLINK_METADATA_BYTES / 2,
        "precondition: the run must approach the cap, or this is vacuous (held {truth})"
    );
    assert!(
        truth <= MAX_HYPERLINK_METADATA_BYTES + MAX_HYPERLINK_METADATA_BYTES / 20,
        "real heap {truth} passed the cap {MAX_HYPERLINK_METADATA_BYTES} by {}",
        truth.saturating_sub(MAX_HYPERLINK_METADATA_BYTES)
    );
}

/// Clearing must return the heap it charged, not only the strings.
///
/// `HashMap` does not shrink on removal, so a figure decremented only by the
/// string bytes reports an empty registry while ~934 KB of table is still
/// held — a reported zero over real memory.
#[test]
fn clearing_returns_the_heap_it_charged() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let uris: Vec<String> =
        (0..4_000u32).map(|index| format!("https://example.com/a-path/{index}")).collect();

    let before = held();
    let mut registry = HyperlinkRegistry::default();
    for uri in &uris {
        registry.intern(None, uri);
    }
    let peak = held().saturating_sub(before);
    assert!(peak > 0, "precondition: interning allocated");

    registry.clear();

    let after = held().saturating_sub(before);
    assert_eq!(registry.retained_bytes(), 0, "a cleared registry must report zero");
    assert!(
        after < peak / 4,
        "clear reported zero while still holding {after} of {peak} — a reported zero \
         over real memory is the case a diagnostic exists to prevent"
    );
}

/// Bytes other test threads can allocate inside a measurement window. `MEASURE` serialises the
/// measurements, but the harness still records and prints sibling results while one runs: measured
/// as 515 to 584 B on a 9 KB figure in 6 of 40 parallel runs, and 0 of 40 with one test thread.
const SIBLING_NOISE_BYTES: usize = 4096;

/// Assert `reported` is within this file's tolerance of the real heap `truth`: it may not
/// understate by more than 1% plus `SIBLING_NOISE_BYTES`, or overstate by more than 10% plus 4 KiB.
fn assert_tracks(label: &str, reported: usize, truth: usize) {
    assert!(
        reported + truth / 100 + SIBLING_NOISE_BYTES >= truth,
        "{label}: reported {reported} understates real heap {truth}"
    );
    assert!(
        reported <= truth + truth / 10 + 4096,
        "{label}: reported {reported} overstates real heap {truth}"
    );
}

/// A URI of `MAX_HYPERLINK_URI_BYTES` less 32 bytes, about 8 KiB.
fn long_uri() -> String {
    format!("https://example.com/{}", "x".repeat(8 * 1024 - 32))
}

/// A thousand client ids on one 8 KiB URI are all admitted and hold, and report, about one URI
/// plus the ids and tables — not a thousand copies of the URI.
#[test]
fn many_client_ids_on_one_long_uri_hold_one_copy_of_it() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let uri = long_uri();
    let client_ids: Vec<String> = (0..1_000).map(|index| format!("client-{index}")).collect();

    let before = held();
    let mut registry = HyperlinkRegistry::default();
    let admitted = client_ids
        .iter()
        .filter(|client_id| registry.try_intern(Some(client_id), &uri).is_some())
        .count();
    let truth = held().saturating_sub(before);
    let reported = registry.retained_bytes();

    assert_eq!(admitted, client_ids.len(), "every client id on the shared URI must be admitted");
    assert_tracks("1,000 ids on one URI", reported, truth);
    assert!(truth < 1024 * 1024, "real heap {truth} must hold one URI copy, not one per id");
    drop(registry);
}

/// The figure tracks the heap while one URI's inner client-id table grows to the count cap.
#[test]
fn an_inner_table_growing_to_the_count_cap_is_charged_as_it_grows() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let uri = long_uri();
    let client_ids: Vec<String> = (0..MAX_HYPERLINKS).map(|index| format!("id-{index}")).collect();

    let before = held();
    let mut registry = HyperlinkRegistry::default();
    for (index, client_id) in client_ids.iter().enumerate() {
        assert!(registry.try_intern(Some(client_id), &uri).is_some(), "{client_id} admitted");
        if [999, 3_999, MAX_HYPERLINKS - 1].contains(&index) {
            let truth = held().saturating_sub(before);
            assert_tracks(&format!("{} ids", index + 1), registry.retained_bytes(), truth);
        }
    }
    drop(registry);
}

/// Reclaiming all but one of a URI's thousand links keeps the URI charged and the figure on the
/// heap after the tables shrink; reclaiming the last returns the registry to an empty figure.
#[test]
fn reclaiming_a_shared_uri_tracks_the_heap_down_to_empty() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let uri = long_uri();
    let client_ids: Vec<String> = (0..1_000).map(|index| format!("client-{index}")).collect();

    let before = held();
    let mut registry = HyperlinkRegistry::default();
    let ids: Vec<_> =
        client_ids.iter().map(|client_id| registry.intern(Some(client_id), &uri)).collect();
    let keep: std::collections::HashSet<_> = [ids[0]].into_iter().collect();
    assert_eq!(registry.retain_live(&keep), 999);
    drop(keep);
    drop(ids);
    let truth = held().saturating_sub(before);
    let reported = registry.retained_bytes();
    assert!(reported > uri.len(), "the surviving link keeps its URI charged ({reported})");
    assert_tracks("1 of 1,000 kept", reported, truth);

    assert_eq!(registry.retain_live(&std::collections::HashSet::new()), 1);
    let truth = held().saturating_sub(before);
    assert_eq!(registry.retained_bytes(), HyperlinkRegistry::default().retained_bytes());
    assert!(truth <= 4096, "an emptied registry still holds {truth} bytes");
    drop(registry);
}

/// The same client-id text under two URIs is two allocations, and the figure rises by both.
#[test]
fn the_same_client_id_under_two_uris_is_held_and_charged_twice() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let client_id = "i".repeat(1_000);
    let first_uri = "https://u.example/".to_string();
    let second_uri = "https://v.example/".to_string();

    let before = held();
    let mut registry = HyperlinkRegistry::default();
    registry.intern(Some(&client_id), &first_uri);
    let truth_first = held().saturating_sub(before);
    let reported_first = registry.retained_bytes();
    registry.intern(Some(&client_id), &second_uri);
    let truth_second = held().saturating_sub(before);
    let reported_second = registry.retained_bytes();

    assert!(truth_second - truth_first >= client_id.len(), "the id text is held twice");
    assert_tracks("one id", reported_first, truth_first);
    assert_tracks("the id under two URIs", reported_second, truth_second);
    drop(registry);
}

/// At the growth boundary the heap stays within the cap: a registry filled to the cap whose shared
/// URI's client table is full refuses one more client id, and both its reported figure and the real
/// heap stay within the cap (plus this file's tolerance), tracking each other.
#[test]
fn a_refused_table_growth_keeps_the_heap_within_the_cap() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let client_ids: Vec<String> = (0..=14_336).map(|index| format!("id-{index:05}")).collect();
    let long = "u".repeat(8 * 1024 - 16);
    let uris: Vec<String> = (0..1_200u32).map(|index| format!("{index:08}{long}")).collect();

    let before = held();
    let mut registry = HyperlinkRegistry::default();
    for client_id in &client_ids[..14_336] {
        assert!(registry.try_intern(Some(client_id), "shared").is_some());
    }
    let mut admitted = 0;
    for uri in &uris {
        if registry.try_intern(None, uri).is_none() {
            break;
        }
        admitted += 1;
    }
    assert!(admitted < uris.len(), "precondition: the anonymous URIs reach the cap");
    let refused = registry.try_intern(Some(&client_ids[14_336]), "shared");
    let truth = held().saturating_sub(before);
    let reported = registry.retained_bytes();

    assert!(refused.is_none(), "the client id whose table growth passes the cap is refused");
    assert!(reported <= MAX_HYPERLINK_METADATA_BYTES, "reported {reported}");
    assert!(
        truth <= MAX_HYPERLINK_METADATA_BYTES + MAX_HYPERLINK_METADATA_BYTES / 100,
        "real heap {truth} passed the cap"
    );
    assert_tracks("at the growth boundary", reported, truth);
    drop(registry);
}
