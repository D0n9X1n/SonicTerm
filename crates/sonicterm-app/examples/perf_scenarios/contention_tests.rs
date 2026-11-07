//! Pins the flood watch: a steadily scrolling pane has no stall, a pane that bursts then stops
//! reports the gap to its last observation, and busy reads are skipped, not counted as stalls.

use super::*;

/// Lines advancing at every observation leave the longest stall at one observation interval.
#[test]
fn a_steady_flood_never_stalls_longer_than_one_interval() {
    let mut watch = FloodWatch::default();
    for step in 0..50_u64 {
        watch.observe(step * OBSERVE_EVERY_MS, Some(step * 1_000));
    }
    assert_eq!(watch.longest_stall_ms(), Some(OBSERVE_EVERY_MS));
}

/// An early burst that then stops scrolling reports the whole quiet tail as one stall, even though
/// its total lines would satisfy any byte floor.
#[test]
fn a_burst_then_a_stop_reports_the_stall() {
    let mut watch = FloodWatch::default();
    watch.observe(0, Some(0));
    watch.observe(100, Some(5_000_000));
    for at_ms in (200..=3_000).step_by(100) {
        watch.observe(at_ms, Some(5_000_000));
    }
    assert_eq!(watch.longest_stall_ms(), Some(2_900));
    assert!(watch.longest_stall_ms().unwrap() > MAX_FLOOD_GAP_MS);
    assert_eq!(watch.to_json()["lines"], 5_000_000);
}

/// A busy parser yields no reading and no stall; only readings decide progress.
#[test]
fn busy_reads_are_skipped() {
    let mut watch = FloodWatch::default();
    watch.observe(0, Some(10));
    watch.observe(100, None);
    watch.observe(200, Some(20));
    assert_eq!(watch.longest_stall_ms(), Some(200));
    assert_eq!(watch.to_json()["busy"], 1);
    assert_eq!(FloodWatch::default().longest_stall_ms(), None, "no reading, no figure");
}

/// The record carries each role's grid, null for a pane it could not read, and the flood figures.
#[test]
fn the_record_lists_role_grids_and_the_flood() {
    let mut watch = FloodWatch::default();
    watch.observe(0, Some(1));
    let value = record(&[Some((125, 20)), None], &watch);
    assert_eq!(value["role_grids"], json!([[125, 20], null]));
    assert_eq!(value["flood"]["observations"], 1);
    assert_eq!(value["max_flood_gap_ms"], MAX_FLOOD_GAP_MS);
}
