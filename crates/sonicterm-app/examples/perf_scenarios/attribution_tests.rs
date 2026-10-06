use super::*;

/// A closed baseline of `resets` resets.
fn closed(resets: u64) -> SyncReading {
    SyncReading { set: false, epoch: resets, resets }
}

/// Attribution runs only in a counting run of a build that calls the API, from a closed baseline:
/// a disabled build, a non-counting run and a pane with no reading skip it, and an update already
/// open before GO refuses the run.
#[test]
fn the_step_before_go_arms_only_from_a_closed_baseline() {
    assert_eq!(start(true, false, Some(closed(0))), Start::Skip("api-disabled"));
    assert_eq!(start(false, true, Some(closed(0))), Start::Skip("counters-off"));
    assert_eq!(start(true, true, None), Start::Skip("no-baseline"));
    assert_eq!(start(true, true, Some(closed(3))), Start::Arm(closed(3)));
    let open = SyncReading { set: true, epoch: 2, resets: 1 };
    match start(true, true, Some(open)) {
        Start::Refuse(reason) => assert!(reason.contains("before GO"), "{reason}"),
        other => panic!("an open update before GO refuses the run: {other:?}"),
    }
}

/// Only an arming id makes a phase armed: a disabled adapter and the prerequisite's stub returning
/// `None` both record attribution unavailable, each with its own reason.
#[test]
fn only_an_arming_id_records_an_armed_phase() {
    assert_eq!(
        armed(ArmResult::Disabled, 7, 300, closed(0), 10),
        Attribution::Unavailable { reason: "api-disabled" }
    );
    assert_eq!(
        armed(ArmResult::NotArmed, 7, 300, closed(0), 10),
        Attribution::Unavailable { reason: "not-armed" }
    );
    let Attribution::Armed(record) = armed(ArmResult::Armed(4), 7, 300, closed(2), 10) else {
        panic!("an arming id arms the phase");
    };
    assert_eq!((record.arming, record.pane, record.updates, record.seq_start), (4, 7, 300, 10));
    assert_eq!((record.baseline, record.seq_end, record.end), (closed(2), None, None));
}

/// The record's JSON is the contract perf-compare reads: a `state` tag, the reason of an unavailable
/// phase, and the arming, baseline and presented counts of an armed one.
#[test]
fn the_record_serializes_as_the_comparison_reads_it() {
    let unavailable =
        serde_json::to_value(Attribution::Unavailable { reason: "not-armed" }).unwrap();
    assert_eq!(unavailable, serde_json::json!({ "state": "unavailable", "reason": "not-armed" }));
    let Attribution::Armed(mut record) = armed(ArmResult::Armed(4), 7, 300, closed(2), 10) else {
        panic!("armed");
    };
    record.seq_end = Some(312);
    record.end = Some(closed(302));
    let value = serde_json::to_value(Attribution::Armed(record)).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "state": "armed", "arming": 4, "pane": 7, "updates": 300,
            "baseline": { "set": false, "epoch": 2, "resets": 2 },
            "seq_start": 10, "seq_end": 312,
            "end": { "set": false, "epoch": 302, "resets": 302 },
        })
    );
}

/// The recorded cfg is this build's own: the adapter calls the API exactly when the cfg is set.
#[test]
fn the_recorded_cfg_is_the_builds_own() {
    assert_eq!(API_ENABLED, cfg!(perf_s10_attribution_api));
}
