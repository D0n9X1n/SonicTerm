//! Pins the flood watch: progress is accounted over each phase's whole span, so a steady flood
//! has only one look interval of gap, while an unobserved prefix, an unobserved suffix, a burst
//! then a stop, and a monitor that stopped looking each show the whole stretch as a gap.

use super::*;

/// A phase of `duration_ms` with `looks` at those times and line counts.
fn phase(
    watch: &mut FloodWatch,
    name: &'static str,
    looks: &[(u64, Option<u64>)],
    duration_ms: u64,
) {
    watch.begin(name);
    for &(at_ms, lines) in looks {
        watch.observe(at_ms, lines);
    }
    watch.end(duration_ms);
}

/// Steady readings: every look advances, so the longest gap is one look interval.
fn steady(from_ms: u64, to_ms: u64) -> Vec<(u64, Option<u64>)> {
    (from_ms / OBSERVE_EVERY_MS..=to_ms / OBSERVE_EVERY_MS)
        .map(|step| (step * OBSERVE_EVERY_MS, Some(step * 1_000)))
        .collect()
}

#[test]
fn a_steady_flood_has_one_interval_of_gap() {
    let mut watch = FloodWatch::default();
    phase(&mut watch, "typing", &steady(0, 32_000), 32_000);
    phase(&mut watch, "tail", &steady(0, 5_000), 5_000);
    assert_eq!(watch.longest_gap_ms(), Some(OBSERVE_EVERY_MS));
}

#[test]
fn an_unobserved_suffix_is_a_gap() {
    // 320 advancing readings, then 40 busy looks to the end: the last 4 s showed no progress.
    let mut looks = steady(0, 32_000);
    looks.extend((321..=360).map(|step| (step * OBSERVE_EVERY_MS, None)));
    let mut watch = FloodWatch::default();
    phase(&mut watch, "typing", &looks, 36_000);
    assert_eq!(watch.longest_gap_ms(), Some(4_000));
    assert_eq!(watch.to_json()["phases"][0]["busy"], 40);
}

#[test]
fn an_unobserved_prefix_is_a_gap() {
    // Nothing advances until 3 s into the phase.
    let mut watch = FloodWatch::default();
    phase(&mut watch, "typing", &steady(3_000, 10_000), 10_000);
    assert_eq!(watch.longest_gap_ms(), Some(3_100));
}

#[test]
fn a_burst_then_a_stop_is_a_gap_whatever_its_line_total() {
    let mut looks = vec![(0, Some(0)), (100, Some(5_000_000))];
    looks.extend((2..=30).map(|step| (step * OBSERVE_EVERY_MS, Some(5_000_000))));
    let mut watch = FloodWatch::default();
    phase(&mut watch, "typing", &looks, 3_000);
    assert_eq!(watch.longest_gap_ms(), Some(2_900));
}

#[test]
fn a_monitor_that_stopped_looking_leaves_the_rest_as_a_gap() {
    // One reading, then no looks at all for the rest of a 30 s phase.
    let mut watch = FloodWatch::default();
    phase(&mut watch, "typing", &[(0, Some(10))], 30_000);
    assert_eq!(watch.longest_gap_ms(), Some(30_000));
    assert_eq!(FloodWatch::default().longest_gap_ms(), None, "no ended phase, no figure");
}

#[test]
fn the_record_lists_role_grids_and_each_phase() {
    let mut watch = FloodWatch::default();
    phase(&mut watch, "typing", &steady(0, 1_000), 1_000);
    let value = record(&[Some((120, 20)), None], &watch);
    assert_eq!(value["role_grids"], json!([[120, 20], null]));
    assert_eq!(value["flood"]["phases"][0]["name"], "typing");
    assert_eq!(value["flood"]["phases"][0]["duration_ms"], 1_000);
    assert_eq!(value["max_flood_gap_ms"], MAX_FLOOD_GAP_MS);
}

/// `advanced` counts readings that showed more lines than the reading before them: busy looks and
/// repeated unchanged readings add nothing, and the first reading is only the baseline.
#[test]
fn advanced_counts_readings_past_the_previous_one() {
    let mut watch = FloodWatch::default();
    phase(
        &mut watch,
        "typing",
        &[(0, Some(10)), (100, None), (200, Some(10)), (300, Some(25)), (400, Some(40))],
        500,
    );
    phase(&mut watch, "tail", &[(0, Some(40)), (100, Some(40)), (200, None)], 300);
    let record = watch.to_json();
    assert_eq!(record["phases"][0]["advanced"], 2);
    assert_eq!(record["phases"][1]["advanced"], 0, "unchanged and busy readings never advance");
}
