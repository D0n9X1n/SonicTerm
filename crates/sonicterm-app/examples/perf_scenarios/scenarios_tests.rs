//! Pins the scenario catalog, the `--list` contract and every scenario plan.

use super::*;

fn phase_names(plan: &Plan) -> Vec<&'static str> {
    plan.steps
        .iter()
        .filter_map(|step| match step {
            Step::Phase(phase) => Some(phase.name),
            Step::Act(_) | Step::Checkpoint(_) => None,
        })
        .collect()
}

fn phase<'plan>(plan: &'plan Plan, name: &str) -> &'plan PhaseSpec {
    plan.steps
        .iter()
        .find_map(|step| match step {
            Step::Phase(phase) if phase.name == name => Some(phase),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{} has no {name} phase", plan.scenario))
}

fn checkpoint_labels(plan: &Plan) -> Vec<&'static str> {
    plan.steps
        .iter()
        .filter_map(|step| match step {
            Step::Checkpoint(label) => Some(*label),
            Step::Phase(_) | Step::Act(_) => None,
        })
        .collect()
}

/// Nominal time a driver needs at its rate, in ms; the wheel assumes every configured row is kept.
fn driver_ms(plan: &Plan, driver: Driver) -> u64 {
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
fn seconds_after_go(plan: &Plan, drivers: bool) -> u64 {
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
fn catalog_lists_twelve_scenarios_with_their_variants() {
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
fn list_json_matches_the_interface_contract() {
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
fn every_plan_ends_with_a_safe_end_checkpoint() {
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
fn trailing_idle_phase_appears_only_where_the_plan_ends_early() {
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
fn timeouts_cover_the_schedule_and_the_smoke_gate() {
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
fn idle_and_streaming_plans_hold_for_their_named_durations() {
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
fn typing_plans_type_two_hundred_characters_at_ten_per_second() {
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
fn flood_plan_times_throughput_to_the_sentinel_then_idles() {
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
fn pointer_plans_sweep_or_drag_for_ten_seconds() {
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

/// S6/rest prints its fixture, then rests the pointer on the path row while rows below it stream,
/// then on a streaming row; each hold shortens to 5 s under `--short` except the 1 s warm-up.
#[test]
fn rest_plan_parks_on_the_path_row_then_on_the_streaming_tail() {
    for (short, hold_ms) in [(false, 10_000), (true, 5_000)] {
        let rest = plan("S6", "rest", short).unwrap();
        assert_eq!(rest.roles, [Workload::RestStream]);
        assert!(rest.setup.is_empty());
        assert_eq!(phase_names(&rest), ["print", "warmup", "rest", "tail", "idle"]);
        assert_eq!(phase(&rest, "print").end, PhaseEnd::Sentinels(vec![0]));
        let on_path = Driver::Park { row: REST_PATH_ROW, col: 3, hertz: 30 };
        assert_eq!(phase(&rest, "warmup").driver, on_path);
        assert_eq!(phase(&rest, "warmup").end, PhaseEnd::Hold(1_000));
        assert_eq!(phase(&rest, "rest").driver, on_path);
        assert_eq!(phase(&rest, "rest").end, PhaseEnd::Hold(hold_ms));
        assert_eq!(
            phase(&rest, "tail").driver,
            Driver::Park { row: REST_TAIL_ROW, col: 3, hertz: 30 }
        );
        assert_eq!(phase(&rest, "tail").end, PhaseEnd::Hold(hold_ms));
    }
    // The fixture rows sit above the 1-based region top, and the tail row is inside the region.
    const { assert!(REST_PATH_ROW + 4 < REST_REGION_TOP) };
    const { assert!(REST_TAIL_ROW >= REST_REGION_TOP) };
}

/// Evidence of a parked phase that measured what it claims, with `hover` as its counter deltas.
fn valid_park(hover: Option<HoverDeltas>) -> ParkEvidence {
    ParkEvidence {
        presented_frames: 30,
        streamed_rows_advanced: true,
        screen_unchanged: true,
        fixture_unchanged: true,
        pointed_row_changed: true,
        hover,
    }
}

/// A parked phase is valid only when frames presented, streamed rows advanced, and its screen and
/// fixture rows stayed the ones it started on; absent counters (counters off) skip the counter rules.
#[test]
fn park_progress_requires_frames_streamed_rows_and_an_unchanged_fixture() {
    for phase in ["warmup", "rest", "tail"] {
        assert_eq!(park_progress(phase, &valid_park(None)), Ok(()), "{phase}");
        let no_frames = ParkEvidence { presented_frames: 0, ..valid_park(None) };
        assert!(park_progress(phase, &no_frames).unwrap_err().contains("no frame"));
        let idle = ParkEvidence { streamed_rows_advanced: false, ..valid_park(None) };
        assert!(park_progress(phase, &idle).unwrap_err().contains("no streamed row"));
        let moved = ParkEvidence { screen_unchanged: false, ..valid_park(None) };
        assert!(park_progress(phase, &moved).unwrap_err().contains("screen or viewport"));
        // A repaint of the fixture, even with the same text, makes the run invalid, not passing.
        let repainted = ParkEvidence { fixture_unchanged: false, ..valid_park(None) };
        assert!(park_progress(phase, &repainted).unwrap_err().contains("repainted"));
    }
}

/// `rest` with counters on must hit the warm cache and do no detection or probe work after warm-up.
#[test]
fn park_progress_requires_rest_to_reuse_the_warm_cache() {
    let warm = HoverDeltas { detections: 0, cache_hits: 300, probes_submitted: 0 };
    assert_eq!(park_progress("rest", &valid_park(Some(warm))), Ok(()));
    let detected = HoverDeltas { detections: 1, ..warm };
    assert!(park_progress("rest", &valid_park(Some(detected)))
        .unwrap_err()
        .contains("1 detections"));
    let probed = HoverDeltas { probes_submitted: 1, ..warm };
    assert!(park_progress("rest", &valid_park(Some(probed))).unwrap_err().contains("1 probe"));
    let missed = HoverDeltas { cache_hits: 0, ..warm };
    assert!(park_progress("rest", &valid_park(Some(missed))).unwrap_err().contains("never hit"));
}

/// `tail` must rest on a row that changes, and with counters on must run detections there; a
/// stationary tail row rejects the run whether or not counters are on.
#[test]
fn park_progress_rejects_a_stationary_tail() {
    let streaming = HoverDeltas { detections: 12, cache_hits: 3, probes_submitted: 0 };
    assert_eq!(park_progress("tail", &valid_park(Some(streaming))), Ok(()));
    for hover in [None, Some(streaming)] {
        let stationary = ParkEvidence { pointed_row_changed: false, ..valid_park(hover) };
        assert!(park_progress("tail", &stationary).unwrap_err().contains("never changed"));
    }
    let undetected = HoverDeltas { detections: 0, ..streaming };
    assert!(park_progress("tail", &valid_park(Some(undetected)))
        .unwrap_err()
        .contains("no detection"));
}

#[test]
fn scrollback_search_and_text_plans_print_before_their_measured_phase() {
    // S7 wheels through its retained history; S8 opens search with `e`; S9 holds emoji and CJK.
    let scroll = plan("S7", "default", false).unwrap();
    assert_eq!(scroll.roles, [Workload::PrintThenShell(Fixture::ScrollbackLines)]);
    assert_eq!(phase_names(&scroll), ["print", "scroll", "settle", "idle"]);
    assert_eq!(phase(&scroll, "scroll").driver, Driver::Wheel { hertz: 60, role: 0 });
    assert_eq!(phase(&scroll, "scroll").end, PhaseEnd::DriverDone);
    let search = plan("S8", "default", false).unwrap();
    assert_eq!(search.roles, [Workload::PrintThenShell(Fixture::SearchText)]);
    assert_eq!(phase(&search, "search").enter, [Act::OpenSearch, Act::Commit("e")]);
    assert_eq!(phase(&search, "search").end, PhaseEnd::Hold(10_000));
    let text = plan("S9", "default", true).unwrap();
    assert_eq!(text.roles, [Workload::PrintThenShell(Fixture::EmojiCjk)]);
    assert_eq!(phase_names(&text), ["print", "hold", "idle"]);
    assert_eq!(phase(&text, "hold").end, PhaseEnd::Hold(5_000));
}

#[test]
fn redraw_plans_stream_sixty_frames_per_second() {
    // S10 plays 20 s of frames (5 s short); sync wraps each frame in DEC 2026 brackets.
    for (variant, synchronized) in [("default", false), ("sync", true)] {
        for (short, count) in [(false, 1_200), (true, 300)] {
            let redraw = plan("S10", variant, short).unwrap();
            assert_eq!(redraw.roles, [Workload::Frames { count, synchronized }]);
            assert_eq!(phase_names(&redraw), ["stream", "idle"]);
            assert_eq!(phase(&redraw, "stream").end, PhaseEnd::Sentinels(vec![0]));
        }
    }
}

#[test]
fn image_plan_switches_tabs_after_registration_and_holds_two_minutes() {
    // S11 measures a hidden tab that still holds its decoded image.
    let image = plan_for("S11", "default", false, Host::Posix).unwrap();
    assert_eq!(image.roles, [Workload::PrintThenSleep(Fixture::Sixel), Workload::IdleShell]);
    assert_eq!(image.setup, [SetupAction::NewTab, SetupAction::ActivateTab(0)]);
    assert_eq!(phase(&image, "image").end, PhaseEnd::ImageRegistered(0));
    let switch = image.steps.iter().position(|step| *step == Step::Act(Act::ActivateTab(1)));
    assert_eq!(image.steps[switch.unwrap() + 1], Step::Checkpoint("switched"));
    assert_eq!(phase(&image, "idle").end, PhaseEnd::Hold(120_000));
    assert_eq!(checkpoint_labels(&image), ["switched", "end"]);
}

#[test]
fn cover_plan_blurs_covers_and_uncovers_with_checkpoints() {
    // S12 settles three panes of history, covers the window for 90 s, then uncovers for 10 s.
    let cover = plan("S12", "default", false).unwrap();
    assert_eq!(cover.roles, [Workload::PrintThenShell(Fixture::HistoryScreen); 3]);
    assert_eq!(cover.setup, [SetupAction::SplitRight, SetupAction::SplitDown]);
    assert_eq!(phase_names(&cover), ["print", "covered", "uncovered"]);
    assert_eq!(phase(&cover, "print").end, PhaseEnd::Sentinels(vec![0, 1, 2]));
    assert_eq!(phase(&cover, "covered").enter, [Act::Unfocus, Act::Cover]);
    assert_eq!(phase(&cover, "covered").end, PhaseEnd::Hold(90_000));
    assert_eq!(phase(&cover, "uncovered").enter, [Act::Uncover]);
    assert_eq!(phase(&cover, "uncovered").end, PhaseEnd::Hold(10_000));
    assert_eq!(checkpoint_labels(&cover), ["settled", "covered", "end"]);
    let short = plan("S12", "default", true).unwrap();
    assert_eq!(phase(&short, "covered").end, PhaseEnd::Hold(5_000));
}

/// Only the short S12 plan asks for the covered-window trim: its trim act follows the cover inside
/// the covered phase, so it runs before the `covered` checkpoint, and only that plan names the
/// trim experiment. The full plan reaches the 30 s scheduler instead and asks for nothing.
#[test]
fn only_the_short_cover_plan_asks_for_the_trim() {
    let short = plan("S12", "default", true).unwrap();
    assert_eq!(phase(&short, "covered").enter, [Act::Unfocus, Act::Cover, Act::TrimCovered]);
    assert_eq!(checkpoint_labels(&short), ["settled", "covered", "end"]);
    assert_eq!(short.trim_experiment, Some("s12-short-trim"));
    for host in [Host::Posix, Host::Windows] {
        for other in all_plans_on(host) {
            let asks = other.steps.iter().any(|step| match step {
                Step::Phase(phase) => phase.enter.contains(&Act::TrimCovered),
                Step::Act(act) => *act == Act::TrimCovered,
                Step::Checkpoint(_) => false,
            });
            let is_short_cover = other.scenario == "S12" && other.short;
            assert_eq!(
                asks, is_short_cover,
                "{} {} short={}",
                other.scenario, other.variant, other.short
            );
            assert_eq!(other.trim_experiment.is_some(), is_short_cover);
        }
    }
}

#[test]
fn windows_image_plan_sends_an_osc_1337_png() {
    // Sixel never arrives through ConPTY, so Windows S11 prints an inline PNG; Posix keeps the Sixel.
    for variant in ["default", "gdi", "wgpu"] {
        let windows = plan_for("S11", variant, false, Host::Windows).unwrap();
        let png = Workload::PrintThenSleep(Fixture::InlinePng);
        assert_eq!(windows.roles, [png, Workload::IdleShell]);
        let posix = plan_for("S11", variant, false, Host::Posix).unwrap();
        assert_eq!(posix.roles, [Workload::PrintThenSleep(Fixture::Sixel), Workload::IdleShell]);
        assert_eq!(windows.steps, posix.steps);
    }
    // Every other scenario plans alike on both hosts.
    for spec in SCENARIOS.iter().filter(|spec| spec.id != "S11") {
        let windows = plan_for(spec.id, "default", true, Host::Windows).unwrap();
        assert_eq!(windows.roles, plan_for(spec.id, "default", true, Host::Posix).unwrap().roles);
    }
    assert_eq!(BUILD_HOST, if cfg!(windows) { Host::Windows } else { Host::Posix });
}

#[test]
fn scrollback_wheel_settles_without_input_after_scrolling() {
    // S7 reports a `settle` phase right after `scroll`: 1.5 s with no input
    // driver and no entry action, ending on a fixed duration that `--short`
    // keeps. It covers the 600 ms scrollbar idle window, the 300 ms fade and a
    // margin, so the frames a settled scrollbar requests are measured apart.
    for short in [false, true] {
        let scroll = plan("S7", "default", short).unwrap();
        let names = phase_names(&scroll);
        let scroll_at = names.iter().position(|name| *name == "scroll").unwrap();
        assert_eq!(names[scroll_at + 1], "settle", "short={short}");
        let settle = phase(&scroll, "settle");
        assert_eq!(settle.driver, Driver::None, "short={short}");
        assert!(settle.enter.is_empty(), "short={short}");
        assert_eq!(settle.end, PhaseEnd::Hold(SETTLE_MS), "short={short}");
        assert_eq!(settle.throughput_bytes, None);
    }
    assert_eq!(SETTLE_MS, 1_500);
}

#[test]
fn image_release_variant_waits_for_media_free_holds_and_reshows() {
    // S11/release: register the image, switch away until a media-free frame presents, hold 65 s from
    // that frame (unshortened), read memory fresh 30 s after it, then switch back until the image shows.
    for (short, host) in [(false, Host::Posix), (true, Host::Posix), (true, Host::Windows)] {
        let release = plan_for("S11", "release", short, host).unwrap();
        assert_eq!(phase_names(&release), ["image", "media-free", "released-hold", "reshow"]);
        assert_eq!(phase(&release, "image").end, PhaseEnd::ImageRegistered(0));
        assert_eq!(phase(&release, "media-free").enter, [Act::ActivateTab(1)]);
        assert_eq!(phase(&release, "media-free").end, PhaseEnd::MediaFree);
        assert_eq!(
            phase(&release, "released-hold").end,
            PhaseEnd::HoldFrom { anchor: "media-free", hold_ms: 65_000 }
        );
        assert_eq!(phase(&release, "reshow").enter, [Act::ActivateTab(0)]);
        assert_eq!(phase(&release, "reshow").end, PhaseEnd::Reshow);
        assert_eq!(checkpoint_labels(&release), ["switched", "released", "end"]);
        assert_eq!(
            release.fresh_after,
            Some(FreshAfter { checkpoint: "released", anchor: "media-free", delay_ms: 30_000 })
        );
        assert_eq!(release.timeout_s, if short { 300 } else { 420 });
    }
    assert!(plan("S11", "default", false).unwrap().fresh_after.is_none());
}

#[test]
fn run_caps_list_only_where_a_scenario_declares_them() {
    // `--list` carries `run_caps` only for S1, S2, S6, S11 and S12: S1/atlas-retry at 2, S2/flood at 2
    // (it rebalances the PR budget; a release comparison runs it in full), S6/rest at 2, S11/release at
    // 1, the Windows presenter variants at 2, and both status-line variants at 1.
    assert_eq!(find("S1").unwrap().run_caps, [("atlas-retry", 2), ("status-line", 1)]);
    assert_eq!(find("S12").unwrap().run_caps, [("status-line", 1)]);
    assert_eq!(find("S6").unwrap().run_caps, [("rest", 2)]);
    assert_eq!(find("S2").unwrap().variants, ["default", "flood"]);
    assert_eq!(find("S2").unwrap().run_caps, [("flood", 2)]);
    assert_eq!(find("S11").unwrap().variants, ["default", "gdi", "wgpu", "release"]);
    assert_eq!(find("S11").unwrap().run_caps, [("release", 1), ("gdi", 2), ("wgpu", 2)]);
    let value: serde_json::Value = serde_json::from_str(&list_json()).unwrap();
    for entry in value["scenarios"].as_array().unwrap() {
        if entry["id"] == "S1" {
            assert_eq!(entry["run_caps"], serde_json::json!({"atlas-retry": 2, "status-line": 1}));
        } else if entry["id"] == "S2" {
            assert_eq!(entry["run_caps"], serde_json::json!({"flood": 2}));
        } else if entry["id"] == "S6" {
            assert_eq!(entry["run_caps"], serde_json::json!({"rest": 2}));
        } else if entry["id"] == "S12" {
            assert_eq!(entry["run_caps"], serde_json::json!({"status-line": 1}));
        } else if entry["id"] == "S11" {
            assert_eq!(entry["run_caps"], serde_json::json!({"release": 1, "gdi": 2, "wgpu": 2}));
        } else {
            assert!(entry.get("run_caps").is_none(), "{}", entry["id"]);
        }
    }
    for spec in SCENARIOS {
        for (variant, cap) in spec.run_caps {
            assert!(spec.variants.contains(variant) && *cap >= 1, "{} {variant}", spec.id);
        }
    }
}

/// S10's `stream` phase records the update count its selected workload plays, 1,200 at full
/// length and 300 with `--short`, read from the workload rather than a constant; every other phase
/// of every scenario, variant and host records none.
#[test]
fn only_s10_stream_carries_its_workload_update_count() {
    for spec in SCENARIOS {
        for variant in spec.variants {
            for (short, host) in [(false, Host::Posix), (true, Host::Posix), (true, Host::Windows)]
            {
                let Some(plan) = plan_for(spec.id, variant, short, host) else { continue };
                let frames = plan.roles.iter().find_map(|role| match role {
                    Workload::Frames { count, .. } => Some(*count),
                    _ => None,
                });
                for step in &plan.steps {
                    let Step::Phase(phase) = step else { continue };
                    let expected = (spec.id == "S10" && phase.name == "stream")
                        .then(|| frames.expect("S10 plays frames"));
                    assert_eq!(phase.updates, expected, "{}/{variant} {}", spec.id, phase.name);
                }
                if spec.id == "S10" {
                    assert_eq!(frames, Some(if short { 300 } else { 1_200 }), "{variant}");
                }
            }
        }
    }
}

/// `--list` declares the latency split, phase-kinds, S10 attribution, echo-timeline and guard-correlation
/// schemas this
/// harness writes, whatever its features or cfg, so the comparison validates every result of this
/// harness against each contract. The listed echo-timeline schema is the one each timeline carries.
#[test]
fn list_json_declares_the_latency_split_capability() {
    let listed: serde_json::Value = serde_json::from_str(&list_json()).expect("valid JSON");
    assert_eq!(
        listed["capabilities"],
        serde_json::json!({
            "latency_split_schema": 1,
            "phase_kinds": 1,
            "s10_attribution": 1,
            "echo_timeline_schema": 1,
            "guard_correlation_schema": 1,
        })
    );
    assert_eq!(super::ECHO_TIMELINE_SCHEMA, crate::timeline::TIMELINE_SCHEMA);
    assert_eq!(listed["schema_version"], 1, "the list's own schema is unchanged");
}

/// S1/atlas-retry prints its 70 rows, drives the recovery episodes until they end, idles to the
/// memory threshold and ends; it is the only plan that drives them.
#[test]
fn the_atlas_retry_plan_prints_its_rows_then_drives_the_episodes() {
    let plan = plan("S1", "atlas-retry", true).unwrap();
    assert_eq!(plan.roles, [Workload::PrintThenSleep(Fixture::AtlasRetryRows)]);
    assert_eq!(phase_names(&plan), ["print", "recovery", "idle"]);
    assert_eq!(phase(&plan, "recovery").driver, Driver::AtlasRetry);
    assert_eq!(phase(&plan, "recovery").end, PhaseEnd::DriverDone);
    assert_eq!(checkpoint_labels(&plan), ["end"]);
    for other in all_plans() {
        let drives = other
            .steps
            .iter()
            .any(|step| matches!(step, Step::Phase(phase) if phase.driver == Driver::AtlasRetry));
        assert_eq!(drives, other.scenario == "S1" && other.variant == "atlas-retry");
    }
}

/// Every (scenario/variant, phase) the catalog plans, with its contract kind and a transition's endpoint:
/// the spec's frozen matrix plus the addendum's S1/atlas-retry print, recovery and idle. Stated here,
/// not derived from the plans.
const PHASE_KINDS: &[(&str, &str, &str, Option<&str>)] = &[
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

/// A status line variant requests the same grid as its default variant, and that grid plus the
/// strip fits below the macOS runner's 43-row screen clamp, so the ref without the strip and the
/// ref with it measure the same terminal grid and the pair-grid check compares like for like.
#[test]
fn status_line_variants_request_their_default_grid_below_the_screen_clamp() {
    const CLAMPED_ROWS: f32 = 43.0;
    for id in ["S1", "S12"] {
        for short in [false, true] {
            let strip = plan(id, "status-line", short).unwrap();
            let base = plan(id, "default", short).unwrap();
            assert_eq!(strip.window_rows, base.window_rows, "{id}");
            assert_eq!(strip.window_rows, Some(STRIP_SAFE_ROWS), "{id}");
        }
    }
    let font_size = 13.0;
    let row_px = font_size * 1.3;
    let strip_px = sonicterm_gpu::status_strip::status_line_height(font_size, 1.0);
    let strip_rows = strip_px / row_px;
    assert!(f32::from(STRIP_SAFE_ROWS) + strip_rows < CLAMPED_ROWS - 1.0, "{strip_rows}");
    assert_eq!(
        plan("S1", "atlas-retry", true).unwrap().window_rows,
        None,
        "its fixture fills 70 rows"
    );
}

/// Every catalog plan, on each host and at each length on its own, holds exactly its variant's contract phases,
/// each once, with the contract's kind and, for a transition, its endpoint. A phase missing from one host or
/// length fails even when another configuration has it, a duplicate fails, and so does a contract variant no
/// plan has. Startup, created apart from the plans, is a transition ending at first-present-then-about-to-wait.
#[test]
fn every_planned_phase_has_its_contract_kind_and_endpoint() {
    type PhaseRow<'row> = (&'row str, &'row str, Option<&'row str>);
    let mut expected: std::collections::BTreeMap<String, std::collections::BTreeSet<PhaseRow>> =
        std::collections::BTreeMap::new();
    for (label, phase, kind, endpoint) in PHASE_KINDS {
        expected.entry(label.to_string()).or_default().insert((*phase, *kind, *endpoint));
    }
    let mut planned_labels = std::collections::BTreeSet::new();
    for host in [Host::Posix, Host::Windows] {
        for scenario in SCENARIOS {
            for variant in scenario.variants {
                for short in [false, true] {
                    let plan = plan_for(scenario.id, variant, short, host).expect("listed plan");
                    let label = format!("{}/{}", plan.scenario, plan.variant);
                    let configuration = format!("{label} on {host:?}, short {short}");
                    let mut actual = std::collections::BTreeSet::new();
                    for step in &plan.steps {
                        if let Step::Phase(phase) = step {
                            let endpoint = phase.endpoint();
                            assert_eq!(
                                endpoint.is_some(),
                                phase.kind == PhaseKind::Transition,
                                "{configuration} {}: an endpoint exactly for a transition",
                                phase.name
                            );
                            let row = (phase.name, phase.kind.name(), endpoint.map(Endpoint::name));
                            assert!(
                                actual.insert(row),
                                "{configuration}: phase {} twice",
                                phase.name
                            );
                        }
                    }
                    let contract = expected.get(&label).cloned().unwrap_or_default();
                    assert_eq!(
                        actual, contract,
                        "{configuration}: its phases against the contract"
                    );
                    planned_labels.insert(label);
                }
            }
        }
    }
    let unplanned: Vec<_> =
        expected.keys().filter(|label| !planned_labels.contains(*label)).collect();
    assert!(unplanned.is_empty(), "contract variants no plan has: {unplanned:?}");
    assert_eq!(
        (STARTUP_KIND, STARTUP_ENDPOINT.name()),
        (PhaseKind::Transition, "first-present-then-about-to-wait")
    );
}

/// Only the `status-line` variants turn the status line on. S12's plan is otherwise the default
/// plan; S1's keeps the default's idle hold and then adds the selection phases.
#[test]
fn status_line_variants_differ_from_default_only_by_the_strip() {
    let names = |plan: &Plan| -> Vec<&'static str> {
        plan.steps
            .iter()
            .map(|step| match step {
                Step::Phase(phase) => phase.name,
                Step::Checkpoint(label) => label,
                Step::Act(_) => "act",
            })
            .collect()
    };
    for short in [false, true] {
        let strip = plan("S12", "status-line", short).unwrap();
        let base = plan("S12", "default", short).unwrap();
        assert!(strip.status_line && !base.status_line);
        // S12/status-line only adds `program` before `end`; everything else is the default plan.
        let without_program: Vec<Step> = strip
            .steps
            .iter()
            .filter(|step| !matches!(step, Step::Phase(phase) if phase.name == "program"))
            .cloned()
            .collect();
        assert_eq!(without_program.len() + 1, strip.steps.len(), "exactly one program phase");
        assert_eq!(
            (strip.roles, strip.setup, without_program),
            (base.roles, base.setup, base.steps)
        );
        let strip = plan("S1", "status-line", short).unwrap();
        let base = plan("S1", "default", short).unwrap();
        assert!(strip.status_line && !base.status_line);
        assert_eq!(strip.roles, [Workload::PrintThenShell(Fixture::LinkedHistory)]);
        assert_eq!(strip.setup, base.setup);
        assert_eq!(strip.steps[1], base.steps[0], "the idle hold the strip's guard checks");
        assert_eq!(
            names(&strip),
            ["print", "idle", "select", "selected", "input", "glyphs", "held", "end"]
        );
        assert_eq!(strip.scrollback_rows, 10_000);
    }
    assert!(all_plans()
        .iter()
        .filter(|plan| plan.status_line)
        .all(|plan| plan.variant == "status-line"));
}

/// The strip's idle contract per phase: cadence-bound samples and wakes, redraws only on a changed
/// sample, one resume sample after uncover, and no sampling while occlusion was delivered.
#[test]
fn status_line_progress_checks_each_phase() {
    let deltas = |samples, wakes, redraws| StatusLineDeltas {
        samples,
        wakes,
        redraws,
        sel_counts: 0,
        sel_cells: 0,
        history_shows: 0,
        held_shows: 0,
        glyph_shows: 0,
    };
    assert!(status_line_progress("idle", 5_000, false, deltas(3, 3, 3)).is_ok());
    assert!(
        status_line_progress("idle", 5_000, false, deltas(4, 3, 0)).is_err(),
        "more samples than the cadence"
    );
    assert!(
        status_line_progress("idle", 5_000, false, deltas(3, 9, 0)).is_err(),
        "a wake without a sample"
    );
    assert!(
        status_line_progress("idle", 5_000, false, deltas(1, 1, 3)).is_err(),
        "redraws without samples"
    );
    assert!(
        status_line_progress("covered", 90_000, true, deltas(1, 1, 0)).is_ok(),
        "one sample before delivery"
    );
    assert!(status_line_progress("covered", 90_000, true, deltas(2, 2, 0)).is_err());
    assert!(
        status_line_progress("covered", 90_000, false, deltas(45, 45, 0)).is_ok(),
        "no occlusion to pause on"
    );
    assert!(
        status_line_progress("uncovered", 5_000, false, deltas(0, 0, 0)).is_err(),
        "the resume sample"
    );
    assert!(status_line_progress("uncovered", 5_000, false, deltas(2, 2, 1)).is_ok());
    assert!(
        status_line_progress("drag", 0, false, deltas(9, 9, 9)).is_ok(),
        "other phases are not checked"
    );
}

/// The selection phases: a drag must count within 200,000 cells per count, and a held selection
/// must never be recounted.
#[test]
fn status_line_progress_checks_the_selection_phases() {
    let sel = |sel_counts, sel_cells| StatusLineDeltas {
        samples: 0,
        wakes: 0,
        redraws: 0,
        sel_counts,
        sel_cells,
        history_shows: 0,
        held_shows: 0,
        glyph_shows: 0,
    };
    assert!(status_line_progress("select", 2_000, false, sel(3, 52_500)).is_ok());
    assert!(status_line_progress("select", 2_000, false, sel(0, 0)).is_err(), "nothing counted");
    assert!(
        status_line_progress("select", 2_000, false, sel(1, SEL_CELLS_PER_COUNT + 1)).is_err(),
        "past the cell bound"
    );
    assert!(status_line_progress("selected", 2_000, false, sel(0, 0)).is_ok());
    assert!(status_line_progress("selected", 2_000, false, sel(1, 10)).is_err(), "a recount");
    assert_eq!(SEL_CELLS_PER_COUNT, sonicterm_ui::selection::SELECTION_EXTENT_MAX_CELLS as u64);
}

/// `print` must show `History: trimmed` exactly once and redraw the strip only for a sample or
/// that appearance, never once per output frame.
#[test]
fn status_line_progress_checks_the_trimmed_history_print() {
    let print = |samples, redraws, history_shows| StatusLineDeltas {
        samples,
        wakes: samples,
        redraws,
        sel_counts: 0,
        sel_cells: 0,
        history_shows,
        held_shows: 0,
        glyph_shows: 0,
    };
    assert!(status_line_progress("print", 0, false, print(2, 4, 1)).is_ok());
    assert!(status_line_progress("print", 0, false, print(2, 1, 0)).is_err(), "never trimmed");
    assert!(status_line_progress("print", 0, false, print(2, 2, 2)).is_err(), "shown twice");
    assert!(status_line_progress("print", 0, false, print(2, 30, 1)).is_err(), "per-frame redraws");
}

/// Only S1/status-line prints the linked history that passes the byte budget, so only its `print`
/// carries the `History: trimmed` check; S12/status-line prints its default history first and is
/// checked only on its covered and uncovered holds.
#[test]
fn only_s1_status_line_prints_the_trimmed_history() {
    for short in [false, true] {
        assert!(plan("S1", "status-line", short).unwrap().prints_linked_history());
        let covered = plan("S12", "status-line", short).unwrap();
        assert!(covered.status_line && !covered.prints_linked_history());
        assert!(!covered.roles.contains(&Workload::PrintThenShell(Fixture::LinkedHistory)));
        assert!(!plan("S1", "default", short).unwrap().prints_linked_history());
    }
}

/// `input` types into the idle shell: `held` must never appear, and the strip may redraw only for
/// a sample, so the outstanding-work deadlines typing arms present nothing.
#[test]
fn status_line_progress_checks_the_typing_input_phase() {
    let input = |samples, redraws, held_shows| StatusLineDeltas {
        samples,
        wakes: samples,
        redraws,
        sel_counts: 0,
        sel_cells: 0,
        history_shows: 0,
        held_shows,
        glyph_shows: 0,
    };
    assert!(status_line_progress("input", 0, false, input(2, 2, 0)).is_ok());
    assert!(status_line_progress("input", 0, false, input(2, 2, 1)).is_err(), "held while typing");
    assert!(status_line_progress("input", 0, false, input(1, 9, 0)).is_err(), "deadline redraws");
}

/// `held` pastes 256 KiB behind a reader a POSIX shell stalls in raw mode: `Input: held` may appear
/// at most once anywhere, and on a POSIX role exactly once; a Windows role only presses Enter.
#[test]
fn status_line_progress_checks_the_held_paste_phase() {
    let held = |held_shows| StatusLineDeltas {
        samples: 0,
        wakes: 0,
        redraws: 0,
        sel_counts: 0,
        sel_cells: 0,
        history_shows: 0,
        held_shows,
        glyph_shows: 0,
    };
    assert!(status_line_progress("held", 0, false, held(1)).is_ok());
    assert!(status_line_progress("held", 0, false, held(0)).is_ok(), "lenient off POSIX");
    assert!(status_line_progress("held", 0, false, held(2)).is_err(), "flapped");
    assert!(status_line_posix_progress("held", held(1)).is_ok());
    assert!(status_line_posix_progress("held", held(0)).is_err(), "never held");
    assert!(status_line_posix_progress("input", held(0)).is_ok(), "only `held` is stricter");
    assert!(matches!(
        HELD_POSIX_STEPS[0].text,
        CommitText::Literal(text) if text.starts_with("\u{15}stty raw -echo; sleep 1;")
    ));
    assert_eq!(HELD_POSIX_STEPS[1].text, CommitText::Repeat('x', 256 << 10));
    let held_driver = |host| {
        plan_for("S1", "status-line", true, host)
            .unwrap()
            .steps
            .iter()
            .find_map(|step| match step {
                Step::Phase(phase) if phase.name == "held" => Some(phase.driver),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(
        held_driver(Host::Posix),
        Driver::Commit { steps: HELD_POSIX_STEPS, settle_ms: 1_800 }
    );
    assert_eq!(
        held_driver(Host::Windows),
        Driver::Commit { steps: HELD_WINDOWS_STEPS, settle_ms: 500 }
    );
}

/// `glyphs` prints U+0378: `Glyphs:` appears at most once anywhere and exactly once on a POSIX
/// role, and the strip redraws only for a sample or that appearance.
#[test]
fn status_line_progress_checks_the_glyphs_phase() {
    let glyphs = |samples, redraws, glyph_shows| StatusLineDeltas {
        samples,
        wakes: samples,
        redraws,
        sel_counts: 0,
        sel_cells: 0,
        history_shows: 0,
        held_shows: 0,
        glyph_shows,
    };
    assert!(status_line_progress("glyphs", 0, false, glyphs(1, 2, 1)).is_ok());
    assert!(status_line_progress("glyphs", 0, false, glyphs(1, 2, 2)).is_err(), "flapped");
    assert!(status_line_progress("glyphs", 0, false, glyphs(1, 9, 1)).is_err(), "extra frames");
    assert!(status_line_posix_progress("glyphs", glyphs(1, 2, 1)).is_ok());
    assert!(status_line_posix_progress("glyphs", glyphs(1, 1, 0)).is_err(), "never shown");
    assert!(matches!(
        GLYPHS_POSIX_STEPS[0].text,
        CommitText::Literal(text) if text == "\u{15}printf '\\315\\270\\n'\r"
    ));
    // `\315\270` is U+0378's UTF-8 encoding.
    assert_eq!("\u{378}".as_bytes(), [0o315, 0o270]);
}

/// S1/status-line's short run keeps at least 5 s inside S1's 80 s smoke gate on both hosts, so a
/// slow runner does not time it out; the phases are shortened, the limit is not raised.
#[test]
fn s1_status_line_short_keeps_headroom_inside_the_smoke_gate() {
    let bound = find("S1").unwrap().short_timeout_s;
    for host in [Host::Posix, Host::Windows] {
        let plan = plan_for("S1", "status-line", true, host).unwrap();
        let needed = seconds_after_go(&plan, true) + 60;
        assert!(needed + 5 <= bound, "{host:?} needs {needed} s of {bound} s");
    }
}

/// S12/status-line ends with `program`, after `uncovered`: a POSIX role prints an `OSC 9;4` set, a
/// burst of OSC 9 and OSC 777 notifications in one write, then the clear, each change 300 ms or more
/// apart so the strip and the taskbar see each one. S12/default has no such phase, and S12's short
/// run stays far inside its bound.
#[test]
fn s12_status_line_ends_with_the_program_phase() {
    for host in [Host::Posix, Host::Windows] {
        for short in [false, true] {
            let plan = plan_for("S12", "status-line", short, host).unwrap();
            assert_eq!(
                phase_names(&plan),
                ["print", "covered", "uncovered", "program"],
                "{host:?}"
            );
            let default = plan_for("S12", "default", short, host).unwrap();
            assert!(!phase_names(&default).contains(&"program"));
            let needed = seconds_after_go(&plan, true) + 60;
            assert!(needed <= find("S12").unwrap().short_timeout_s, "{host:?} short={short}");
        }
    }
    let posix = plan_for("S12", "status-line", true, Host::Posix).unwrap();
    let Driver::Commit { steps, .. } = phase(&posix, "program").driver else {
        panic!("program is a commit driver");
    };
    let text: String = steps
        .iter()
        .map(|step| match step.text {
            CommitText::Literal(text) => text.to_owned(),
            CommitText::Repeat(character, count) => character.to_string().repeat(count),
        })
        .collect();
    for sequence in [
        r"\033]9;4;1;40\007",
        r"\033]9;4;1;50\007'; sleep 0.1; printf '\033]9;4;1;70\007",
        r"\033]9;build done\007",
        r"\033]777;notify;CI;green\007",
        r"\033]9;4;0\007",
    ] {
        assert!(text.contains(sequence), "{sequence} in {text}");
    }
    // The burst is one step, 100 ms apart inside it; the steps themselves stay 300 ms apart.
    assert!(steps.windows(2).all(|pair| pair[1].after_ms >= pair[0].after_ms + 300));
}

/// `program` syncs once per change and redraws nothing else: a toast redraw per notification batch
/// that a present follows, a taskbar delivery per progress change, and no strip redraw beyond the
/// sample cadence. A POSIX role must also show the set, the burst and the clear and every batch.
#[test]
fn program_phase_progress_syncs_once_per_change() {
    let strip = |samples, redraws| StatusLineDeltas {
        samples,
        wakes: samples,
        redraws,
        sel_counts: 0,
        sel_cells: 0,
        history_shows: 0,
        held_shows: 0,
        glyph_shows: 0,
    };
    let program = |progress, notifies, toast_redraws, deliveries, presented| {
        Some(ProgramDeltas { progress, notifies, toast_redraws, deliveries, presented })
    };
    let ok = program(4, 1, 1, 4, 3);
    assert!(program_phase_progress(strip(1, 1), ok, None, true, 4).is_ok());
    assert!(
        program_phase_progress(strip(1, 3), ok, None, true, 4).is_err(),
        "strip redraws per change"
    );
    let extra_toasts = program(4, 1, 3, 4, 3);
    assert!(program_phase_progress(strip(1, 1), extra_toasts, None, true, 4).is_err());
    let extra_deliveries = program(3, 1, 1, 4, 3);
    assert!(program_phase_progress(strip(1, 1), extra_deliveries, None, true, 4).is_err());
    let unpresented = program(4, 1, 1, 4, 0);
    assert!(
        program_phase_progress(strip(1, 1), unpresented, None, true, 4).is_err(),
        "a toast redraw request that never presented"
    );
    let nothing = program(0, 0, 0, 0, 0);
    assert!(program_phase_progress(strip(1, 1), nothing, None, true, 4).is_err(), "nothing seen");
    assert!(
        program_phase_progress(strip(1, 1), nothing, None, false, 1).is_ok(),
        "cmd prints none"
    );
    assert!(
        program_phase_progress(strip(1, 1), None, None, true, 4).is_ok(),
        "a base whose App predates the program counters is not checked"
    );
}

/// Presents in `program` are bounded by the work the phase did: strip redraws and samples, toast
/// redraws, and per committed step its echo, output and prompt. Windows, whose `cmd` prints no
/// program sequence, is bounded the same way by its single Enter step.
#[test]
fn program_phase_progress_bounds_presents_by_the_work_it_did() {
    let strip = |samples, redraws| StatusLineDeltas {
        samples,
        wakes: samples,
        redraws,
        sel_counts: 0,
        sel_cells: 0,
        history_shows: 0,
        held_shows: 0,
        glyph_shows: 0,
    };
    let posix = |presented| ProgramDeltas {
        progress: 4,
        notifies: 1,
        toast_redraws: 1,
        deliveries: 4,
        presented,
    };
    // Four updates (one strip redraw, one sample, one toast, the progress) and four steps.
    assert!(program_phase_progress(strip(1, 1), Some(posix(100_000)), None, true, 4).is_err());
    let allowed = 1 + 1 + 1 + 4 * PRESENTS_PER_STEP;
    assert!(program_phase_progress(strip(1, 1), Some(posix(allowed)), None, true, 4).is_ok());
    assert!(program_phase_progress(strip(1, 1), Some(posix(allowed + 1)), None, true, 4).is_err());
    let windows = |presented| ProgramDeltas {
        progress: 0,
        notifies: 0,
        toast_redraws: 0,
        deliveries: 0,
        presented,
    };
    // Zero updates and no step: any present is unexplained.
    assert!(program_phase_progress(strip(0, 0), Some(windows(1)), None, false, 0).is_err());
    // Zero updates and the one Enter step: its echo and prompt only.
    assert!(program_phase_progress(strip(0, 0), Some(windows(2)), None, false, 1).is_ok());
    let past = PRESENTS_PER_STEP + 1;
    assert!(program_phase_progress(strip(0, 0), Some(windows(past)), None, false, 1).is_err());
    assert!(program_phase_progress(strip(0, 0), Some(windows(100_000)), None, false, 1).is_err());
}

/// The counting surface's record: 40, the burst's 70 on the trailing edge at least one interval
/// after the value before it, then the clear last; 50 may be absent when the PTY merged the burst.
#[test]
fn program_phase_progress_checks_the_delivered_sequence_and_its_trailing_edge() {
    let strip = StatusLineDeltas {
        samples: 1,
        wakes: 1,
        redraws: 1,
        sel_counts: 0,
        sel_cells: 0,
        history_shows: 0,
        held_shows: 0,
        glyph_shows: 0,
    };
    let delivered = |values: &[(u64, Option<u8>)]| -> Vec<Delivered> {
        values.iter().map(|&(at_ms, percent)| Delivered { at_ms, percent }).collect()
    };
    let check = |values: &[(u64, Option<u8>)]| {
        let record = delivered(values);
        let program = ProgramDeltas {
            progress: 4,
            notifies: 1,
            toast_redraws: 1,
            deliveries: record.len() as u64,
            presented: 2,
        };
        program_phase_progress(strip, Some(program), Some(&record), true, 4)
    };
    assert!(check(&[(0, Some(40)), (400, Some(50)), (650, Some(70)), (1100, None)]).is_ok());
    assert!(check(&[(0, Some(40)), (500, Some(70)), (1100, None)]).is_ok(), "merged burst");
    assert!(
        check(&[(0, Some(40)), (400, Some(50)), (500, Some(70)), (1100, None)]).is_err(),
        "70 delivered 100 ms after 50 skipped the interval"
    );
    assert!(
        check(&[(0, Some(40)), (400, Some(50)), (1100, None)]).is_err(),
        "the burst's last value never arrived"
    );
    assert!(
        check(&[(0, Some(40)), (400, Some(50)), (650, Some(70))]).is_err(),
        "the clear is not the final delivery"
    );
    assert!(
        check(&[(0, Some(40)), (400, Some(70)), (650, Some(50)), (1100, None)]).is_err(),
        "a stale value after the newest"
    );
    let mismatched =
        ProgramDeltas { progress: 4, notifies: 1, toast_redraws: 1, deliveries: 2, presented: 2 };
    let record = delivered(&[(0, Some(40)), (500, Some(70)), (1100, None)]);
    assert!(
        program_phase_progress(strip, Some(mismatched), Some(&record), true, 4).is_err(),
        "the record and the delivery counter disagree"
    );
}
