//! Pins the scenario catalog, the `--list` contract and every scenario plan.

use super::*;

pub(super) fn phase_names(plan: &Plan) -> Vec<&'static str> {
    plan.steps
        .iter()
        .filter_map(|step| match step {
            Step::Phase(phase) => Some(phase.name),
            Step::Act(_) | Step::Checkpoint(_) => None,
        })
        .collect()
}

pub(super) fn phase<'plan>(plan: &'plan Plan, name: &str) -> &'plan PhaseSpec {
    plan.steps
        .iter()
        .find_map(|step| match step {
            Step::Phase(phase) if phase.name == name => Some(phase),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{} has no {name} phase", plan.scenario))
}

pub(super) fn checkpoint_labels(plan: &Plan) -> Vec<&'static str> {
    plan.steps
        .iter()
        .filter_map(|step| match step {
            Step::Checkpoint(label) => Some(*label),
            Step::Phase(_) | Step::Act(_) => None,
        })
        .collect()
}

/// Nominal time a driver needs at its rate, in ms; the wheel assumes every configured row is kept.
pub(super) fn driver_ms(plan: &Plan, driver: Driver) -> u64 {
    match driver {
        Driver::Typing { chars, per_second, settle_ms, .. } => {
            u64::from(chars) * 1_000 / u64::from(per_second) + settle_ms
        }
        Driver::Wheel { hertz, .. } => {
            let ticks = (plan.scrollback_rows as u64).div_ceil(3);
            2 * ticks * 1_000 / u64::from(hertz)
        }
        // The settle and 32 single-frame steps take a few frame periods each; 2 s is generous.
        Driver::AtlasRetry => 2_000,
        Driver::Commit { steps, settle_ms } => {
            steps.last().map_or(0, |step| step.after_ms) + settle_ms
        }
        Driver::None | Driver::Sweep { .. } | Driver::Drag { .. } | Driver::Park { .. } => 0,
    }
}

/// Seconds from GO to the end of the last phase. Without `drivers`, driver phases and open-ended
/// waits count as zero, which gives a lower bound; with it, the nominal schedule.
pub(super) fn seconds_after_go(plan: &Plan, drivers: bool) -> u64 {
    let mut elapsed_ms = 0_u64;
    for step in &plan.steps {
        let Step::Phase(phase) = step else { continue };
        match phase.end {
            PhaseEnd::Hold(hold_ms) => elapsed_ms += hold_ms,
            PhaseEnd::AfterGo(after_go_ms) => elapsed_ms = elapsed_ms.max(after_go_ms),
            PhaseEnd::DriverDone if drivers => elapsed_ms += driver_ms(plan, phase.driver),
            PhaseEnd::HoldFrom { hold_ms, .. } => elapsed_ms += hold_ms,
            PhaseEnd::DriverDone
            | PhaseEnd::Sentinels(_)
            | PhaseEnd::ImageRegistered(_)
            | PhaseEnd::MediaFree
            | PhaseEnd::Reshow => {}
        }
    }
    elapsed_ms.div_ceil(1_000)
}

#[test]
pub(super) fn catalog_lists_twelve_scenarios_with_their_variants() {
    // The comparison script selects runs by exactly these ids and variant names.
    let ids: Vec<_> = SCENARIOS.iter().map(|spec| spec.id).collect();
    assert_eq!(ids, ["S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9", "S10", "S11", "S12"]);
    assert_eq!(find("S2").unwrap().variants, ["default", "flood"]);
    assert_eq!(find("S6").unwrap().variants, ["default", "flood", "selection-drag", "rest"]);
    assert_eq!(find("S10").unwrap().variants, ["default", "sync"]);
    // The presenter variants and the role program's exit are Windows runs; the catalog lists them everywhere.
    // `atlas-retry` runs only in the counters set; perf-compare enforces it.
    assert_eq!(
        find("S1").unwrap().variants,
        ["default", "gdi", "wgpu", "role-exit", "atlas-retry", "status-line"]
    );
    // `status-line` runs the default plan with the status line on in the scratch config.
    assert_eq!(find("S12").unwrap().variants, ["default", "status-line"]);
    // S11 adds `release`, whose run cap and plan are pinned by their own tests.
    assert_eq!(find("S5").unwrap().variants, ["default", "gdi", "wgpu"]);
    for (variant, presentation) in [
        ("default", Presentation::Configured),
        ("gdi", Presentation::ForceGdi),
        ("wgpu", Presentation::ForceWgpu),
        ("role-exit", Presentation::Configured),
    ] {
        assert_eq!(plan("S1", variant, false).unwrap().presentation, presentation, "{variant}");
    }
    // role-exit plans exactly like the idle default; only its role's program exits after GO.
    let idle = plan("S1", "default", false).unwrap();
    let exiting = plan("S1", "role-exit", false).unwrap();
    assert_eq!(exiting.roles, [Workload::ExitAfterGo]);
    assert_eq!((exiting.steps, exiting.setup), (idle.steps, idle.setup));
    for spec in SCENARIOS {
        assert_eq!(spec.variants[0], "default", "{} lists default first", spec.id);
        assert!(spec.short_timeout_s <= spec.timeout_s, "{} short bound", spec.id);
    }
    assert!(find("S13").is_none());
    assert!(plan("S1", "flood", false).is_none(), "an unlisted variant has no plan");
}

#[test]
pub(super) fn list_json_matches_the_interface_contract() {
    // `--list` is the comparison script's only source of ids, variants and timeouts.
    let value: serde_json::Value = serde_json::from_str(&list_json()).unwrap();
    assert_eq!(value["schema_version"], 1);
    let scenarios = value["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), SCENARIOS.len());
    for (entry, spec) in scenarios.iter().zip(SCENARIOS) {
        let mut keys: Vec<_> = entry.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        // `run_caps` appears only for a scenario that declares caps.
        let mut expected = vec!["id", "short_timeout_s", "timeout_s", "title", "variants"];
        if !spec.run_caps.is_empty() {
            expected.insert(1, "run_caps");
        }
        assert_eq!(keys, expected);
        assert_eq!(entry["id"], spec.id);
        assert_eq!(entry["title"], spec.title);
        assert_eq!(entry["timeout_s"], spec.timeout_s);
        assert_eq!(entry["short_timeout_s"], spec.short_timeout_s);
        assert_eq!(entry["variants"], serde_json::json!(spec.variants));
    }
}

#[test]
pub(super) fn every_plan_ends_with_a_safe_end_checkpoint() {
    // Memory is read at `end`, at least 60 s after GO (5 s short); labels become file names.
    for plan in all_plans() {
        let labels = checkpoint_labels(&plan);
        assert_eq!(labels.last(), Some(&"end"), "{} {}", plan.scenario, plan.variant);
        assert!(matches!(plan.steps.last(), Some(Step::Checkpoint("end"))));
        let mut unique = labels.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), labels.len(), "{} repeats a label", plan.scenario);
        for label in labels {
            assert!(
                !label.is_empty()
                    && label.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'-'),
                "unsafe checkpoint label {label:?}"
            );
        }
        let threshold_s = if plan.short { 5 } else { 60 };
        assert!(
            seconds_after_go(&plan, false) >= threshold_s,
            "{} {} short={} ends before GO + {threshold_s} s",
            plan.scenario,
            plan.variant,
            plan.short
        );
        assert!(matches!(plan.steps.first(), Some(Step::Phase(_))), "GO opens a measured phase");
    }
}

#[test]
pub(super) fn trailing_idle_phase_appears_only_where_the_plan_ends_early() {
    // Plans that already pass 60 s after GO end with their own last phase; the rest idle to it.
    for plan in all_plans() {
        // S1/atlas-retry ends when its episodes do, well before GO + 60 s, so it idles to it.
        let long_on_its_own = matches!(plan.scenario, "S1" | "S3" | "S4" | "S5" | "S11" | "S12")
            && !(plan.scenario == "S1" && plan.variant == "atlas-retry");
        let last_phase = plan
            .steps
            .iter()
            .rev()
            .find_map(|step| match step {
                Step::Phase(phase) => Some(phase),
                Step::Act(_) | Step::Checkpoint(_) => None,
            })
            .unwrap();
        let idles_to_threshold = matches!(last_phase.end, PhaseEnd::AfterGo(_));
        assert_eq!(idles_to_threshold, !long_on_its_own, "{} {}", plan.scenario, plan.variant);
        if let PhaseEnd::AfterGo(after_go_ms) = last_phase.end {
            assert_eq!(after_go_ms, if plan.short { 5_000 } else { 60_000 });
            assert_eq!(last_phase.name, "idle");
        }
    }
}

#[test]
pub(super) fn timeouts_cover_the_schedule_and_the_smoke_gate() {
    // A full run may wait 60 s at each checkpoint for `footprint` plus 120 s for startup and
    // acknowledgement. A short run budgets one 60 s margin; the smoke's S1 and S3 fit its 80 s gate.
    for plan in all_plans() {
        let spec = find(plan.scenario).unwrap();
        let scheduled = seconds_after_go(&plan, true);
        let (bound, needed) = if plan.short {
            (spec.short_timeout_s, scheduled + 60)
        } else {
            (spec.timeout_s, scheduled + 60 * checkpoint_labels(&plan).len() as u64 + 120)
        };
        let context = format!("{} {} short={}", plan.scenario, plan.variant, plan.short);
        assert!(needed <= bound, "{context} needs {needed} s of {bound} s");
        assert_eq!(plan.timeout_s, bound, "{context}");
    }
    for smoke in ["S1", "S3"] {
        assert!(find(smoke).unwrap().short_timeout_s <= 80, "{smoke} must fit the smoke gate");
    }
}

#[test]
pub(super) fn idle_and_streaming_plans_hold_for_their_named_durations() {
    // S1 idles, S4 streams in view and S5 streams in a background tab, each for 60 s (5 s short).
    let idle = plan("S1", "default", false).unwrap();
    assert_eq!(idle.roles, [Workload::IdleShell]);
    assert!(idle.setup.is_empty());
    assert_eq!(phase_names(&idle), ["idle"]);
    assert_eq!(phase(&idle, "idle").end, PhaseEnd::Hold(60_000));
    assert_eq!(phase(&plan("S1", "default", true).unwrap(), "idle").end, PhaseEnd::Hold(5_000));
    let stream = plan("S4", "default", false).unwrap();
    assert_eq!(stream.roles, [Workload::DateLoop]);
    assert_eq!(phase_names(&stream), ["stream"]);
    let background = plan("S5", "default", false).unwrap();
    assert_eq!(background.roles, [Workload::IdleShell, Workload::DateLoop]);
    assert_eq!(background.setup, [SetupAction::NewTab, SetupAction::ActivateTab(0)]);
    assert_eq!(phase(&background, "stream").end, PhaseEnd::Hold(60_000));
    for plan in all_plans() {
        assert!(plan.focused, "{} receives Focused(true) before GO", plan.scenario);
        // S1/status-line keeps 10,000 rows so its linked history passes the byte budget.
        let long =
            plan.scenario == "S7" || (plan.scenario == "S1" && plan.variant == "status-line");
        let expected_rows = if long { 10_000 } else { 1_000 };
        assert_eq!(plan.scrollback_rows, expected_rows, "{}", plan.scenario);
    }
}

#[test]
pub(super) fn typing_plans_type_two_hundred_characters_at_ten_per_second() {
    // S2 types into its only shell; the flood variant types into the split's new active pane.
    let typing = plan("S2", "default", false).unwrap();
    assert_eq!(typing.roles, [Workload::IdleShell]);
    assert_eq!(phase_names(&typing), ["typing", "idle"]);
    let driver = Driver::Typing { role: 0, chars: 200, per_second: 10, settle_ms: 2_000 };
    assert_eq!(phase(&typing, "typing").driver, driver);
    assert_eq!(phase(&typing, "typing").end, PhaseEnd::DriverDone);
    let flood = plan("S2", "flood", false).unwrap();
    let flooding = Workload::Flood { lines: 2_000_000, bulk_bytes: 50 << 20 };
    assert_eq!(flood.roles, [flooding, Workload::IdleShell]);
    assert_eq!(flood.setup, [SetupAction::SplitRight]);
    let typed_into_split = Driver::Typing { role: 1, chars: 200, per_second: 10, settle_ms: 2_000 };
    assert_eq!(phase(&flood, "typing").driver, typed_into_split);
    // --short shortens holds, not the number of latency samples.
    assert_eq!(phase(&plan("S2", "default", true).unwrap(), "typing").driver, driver);
}

#[test]
pub(super) fn flood_plan_times_throughput_to_the_sentinel_then_idles() {
    // S3 counts `yes | head` output plus the seeded fixture; --short shrinks both.
    for (short, lines, bulk_bytes) in
        [(false, 2_000_000_u32, 50_u64 << 20), (true, 200_000, 5 << 20)]
    {
        let flood = plan("S3", "default", short).unwrap();
        assert_eq!(flood.roles, [Workload::Flood { lines, bulk_bytes }]);
        assert_eq!(phase_names(&flood), ["flood", "idle"]);
        let flood_phase = phase(&flood, "flood");
        assert_eq!(flood_phase.end, PhaseEnd::Sentinels(vec![0]));
        assert_eq!(flood_phase.throughput_bytes, Some(2 * u64::from(lines) + bulk_bytes));
        assert_eq!(phase(&flood, "idle").end, PhaseEnd::Hold(if short { 5_000 } else { 60_000 }));
        assert_eq!(checkpoint_labels(&flood), ["end"]);
    }
}

#[test]
pub(super) fn pointer_plans_sweep_or_drag_for_ten_seconds() {
    // S6 hovers three tabs; flood shows the flooding first tab; selection-drag drags over text.
    let sweep = plan("S6", "default", false).unwrap();
    assert_eq!(sweep.roles, [Workload::IdleShell; 3]);
    assert_eq!(sweep.setup, [SetupAction::NewTab, SetupAction::NewTab]);
    assert_eq!(phase_names(&sweep), ["sweep", "idle"]);
    assert_eq!(phase(&sweep, "sweep").driver, Driver::Sweep { hertz: 120 });
    assert_eq!(phase(&sweep, "sweep").end, PhaseEnd::Hold(10_000));
    let flood = plan("S6", "flood", true).unwrap();
    assert_eq!(flood.roles[0], Workload::Flood { lines: 200_000, bulk_bytes: 5 << 20 });
    let flood_setup = [SetupAction::NewTab, SetupAction::NewTab, SetupAction::ActivateTab(0)];
    assert_eq!(flood.setup, flood_setup);
    assert_eq!(phase(&flood, "sweep").end, PhaseEnd::Hold(5_000));
    let drag = plan("S6", "selection-drag", false).unwrap();
    assert_eq!(drag.roles, [Workload::PrintThenShell(Fixture::DenseScreen)]);
    assert_eq!(phase_names(&drag), ["print", "drag", "idle"]);
    assert_eq!(phase(&drag, "print").end, PhaseEnd::Sentinels(vec![0]));
    assert_eq!(phase(&drag, "drag").driver, Driver::Drag { hertz: 120 });
    assert_eq!(phase(&drag, "drag").end, PhaseEnd::Hold(10_000));
}

/// Evidence of a parked phase that measured what it claims, with `hover` as its counter deltas.
pub(super) fn valid_park(hover: Option<HoverDeltas>) -> ParkEvidence {
    ParkEvidence {
        presented_frames: 30,
        streamed_rows_advanced: true,
        screen_unchanged: true,
        fixture_unchanged: true,
        pointed_row_changed: true,
        hover,
    }
}

/// Every (scenario/variant, phase) the catalog plans, with its contract kind and a transition's endpoint:
/// the spec's frozen matrix plus the addendum's S1/atlas-retry print, recovery and idle. Stated here,
/// not derived from the plans.
pub(super) const PHASE_KINDS: &[(&str, &str, &str, Option<&str>)] = &[
    ("S1/atlas-retry", "idle", "hold", None),
    ("S1/atlas-retry", "print", "transition", Some("sentinel-parsed")),
    ("S1/atlas-retry", "recovery", "hold", None),
    ("S1/default", "idle", "hold", None),
    ("S1/gdi", "idle", "hold", None),
    ("S1/role-exit", "idle", "hold", None),
    ("S1/status-line", "glyphs", "sustained", None),
    ("S1/status-line", "held", "sustained", None),
    ("S1/status-line", "idle", "hold", None),
    ("S1/status-line", "input", "sustained", None),
    ("S1/status-line", "print", "transition", Some("sentinel-parsed")),
    ("S1/status-line", "select", "sustained", None),
    ("S1/status-line", "selected", "hold", None),
    ("S1/wgpu", "idle", "hold", None),
    ("S10/default", "idle", "hold", None),
    ("S10/default", "stream", "sustained", None),
    ("S10/sync", "idle", "hold", None),
    ("S10/sync", "stream", "sustained", None),
    ("S11/default", "idle", "hold", None),
    ("S11/default", "image", "transition", Some("image-registered-then-presented")),
    ("S11/gdi", "idle", "hold", None),
    ("S11/gdi", "image", "transition", Some("image-registered-then-presented")),
    ("S11/release", "image", "transition", Some("image-registered-then-presented")),
    ("S11/release", "media-free", "transition", Some("first-present-after-entry")),
    ("S11/release", "released-hold", "hold", None),
    ("S11/release", "reshow", "transition", Some("first-present-with-image")),
    ("S11/wgpu", "idle", "hold", None),
    ("S11/wgpu", "image", "transition", Some("image-registered-then-presented")),
    ("S12/default", "covered", "hold", None),
    ("S12/default", "print", "transition", Some("sentinel-parsed")),
    ("S12/default", "uncovered", "hold", None),
    ("S12/status-line", "accessibility", "hold", None),
    ("S12/status-line", "covered", "hold", None),
    ("S12/status-line", "print", "transition", Some("sentinel-parsed")),
    ("S12/status-line", "uncovered", "hold", None),
    ("S12/status-line", "program", "sustained", None),
    ("S2/default", "idle", "hold", None),
    ("S2/default", "typing", "sustained", None),
    ("S2/flood", "idle", "hold", None),
    ("S2/flood", "typing", "sustained", None),
    ("S3/default", "flood", "sustained", None),
    ("S3/default", "idle", "hold", None),
    ("S4/default", "stream", "sustained", None),
    ("S5/default", "stream", "hold", None),
    ("S5/gdi", "stream", "hold", None),
    ("S5/wgpu", "stream", "hold", None),
    ("S6/default", "idle", "hold", None),
    ("S6/default", "sweep", "sustained", None),
    ("S6/flood", "idle", "hold", None),
    ("S6/flood", "sweep", "sustained", None),
    ("S6/selection-drag", "drag", "sustained", None),
    ("S6/selection-drag", "idle", "hold", None),
    ("S6/selection-drag", "print", "transition", Some("sentinel-parsed")),
    ("S6/rest", "idle", "hold", None),
    ("S6/rest", "print", "transition", Some("sentinel-parsed")),
    ("S6/rest", "rest", "sustained", None),
    ("S6/rest", "tail", "sustained", None),
    ("S6/rest", "warmup", "hold", None),
    ("S7/default", "idle", "hold", None),
    ("S7/default", "print", "transition", Some("sentinel-parsed")),
    ("S7/default", "scroll", "sustained", None),
    ("S7/default", "settle", "hold", None),
    ("S8/default", "idle", "hold", None),
    ("S8/default", "print", "transition", Some("sentinel-parsed")),
    ("S8/default", "search", "hold", None),
    ("S9/default", "hold", "hold", None),
    ("S9/default", "idle", "hold", None),
    ("S9/default", "print", "transition", Some("sentinel-parsed")),
];
