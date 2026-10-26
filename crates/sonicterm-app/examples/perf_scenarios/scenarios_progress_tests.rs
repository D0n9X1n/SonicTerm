//! Scenario readiness and phase-progress verdicts.

use super::super::scenarios_tests::*;
use super::*;

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
    assert_eq!(super::super::ECHO_TIMELINE_SCHEMA, crate::timeline::TIMELINE_SCHEMA);
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
