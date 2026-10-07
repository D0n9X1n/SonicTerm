use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::*;

/// Totals as plain numbers: custody sum and count, rendered dispatch sum and count, failed sum and count.
fn read(totals: &CustodyTotals) -> [u64; 6] {
    [
        totals.custody_ns.load(Ordering::Relaxed),
        totals.custodies.load(Ordering::Relaxed),
        totals.dispatch_ns.load(Ordering::Relaxed),
        totals.dispatches.load(Ordering::Relaxed),
        totals.dispatch_failed_ns.load(Ordering::Relaxed),
        totals.dispatches_failed.load(Ordering::Relaxed),
    ]
}

/// A rendered frame records one custody interval (first guard to the token's drop, after the last guard)
/// and one dispatch interval in the rendered population; nothing reaches the failed population.
#[test]
fn a_rendered_frame_records_custody_and_one_dispatch() {
    let totals = std::sync::Arc::new(CustodyTotals::default());
    let first_guard = Instant::now() - Duration::from_millis(5);
    let (custody, dispatch) = start_at(&totals, first_guard);
    drop(custody);
    dispatch.rendered();
    let [custody_ns, custodies, dispatch_ns, dispatches, failed_ns, failed] = read(&totals);
    assert_eq!((custodies, dispatches, failed, failed_ns), (1, 1, 0, 0));
    assert!(custody_ns >= 5_000_000, "custody counts from the first guard: {custody_ns}");
    assert!(dispatch_ns >= custody_ns, "dispatch runs to the render's return, after the release");
}

/// A collection that fails before render (contention after a first guard, a sync hold, a reconcile
/// failure or no renderer) records its custody and joins the failed population, never the rendered one.
#[test]
fn a_frame_that_never_renders_is_its_own_population() {
    let totals = std::sync::Arc::new(CustodyTotals::default());
    let (custody, dispatch) = start_at(&totals, Instant::now() - Duration::from_millis(2));
    drop(custody);
    drop(dispatch);
    let [_, custodies, dispatch_ns, dispatches, failed_ns, failed] = read(&totals);
    assert_eq!((custodies, dispatches, dispatch_ns, failed), (1, 0, 0, 1));
    assert!(failed_ns >= 2_000_000, "{failed_ns}");
}

/// Totals accumulate across frames as sums and counts, so a phase reads them as a difference.
#[test]
fn totals_accumulate_as_sums_and_counts() {
    let totals = std::sync::Arc::new(CustodyTotals::default());
    for _frame in 0..3 {
        let (custody, dispatch) = start_at(&totals, Instant::now());
        drop(custody);
        dispatch.rendered();
    }
    let [_, custodies, _, dispatches, _, failed] = read(&totals);
    assert_eq!((custodies, dispatches, failed), (3, 3, 0));
}
