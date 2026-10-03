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
        Driver::None | Driver::Sweep { .. } | Driver::Drag { .. } => 0,
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
    assert_eq!(find("S6").unwrap().variants, ["default", "flood", "selection-drag"]);
    assert_eq!(find("S10").unwrap().variants, ["default", "sync"]);
    // The presenter variants and the role program's exit are Windows runs; the catalog lists them everywhere.
    assert_eq!(find("S1").unwrap().variants, ["default", "gdi", "wgpu", "role-exit"]);
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
        let long_on_its_own = matches!(plan.scenario, "S1" | "S3" | "S4" | "S5" | "S11" | "S12");
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
        let expected_rows = if plan.scenario == "S7" { 10_000 } else { 1_000 };
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
    // `--list` carries `run_caps` only for S2 and S11: S2/flood at 2 (it rebalances the PR budget; a
    // release comparison runs it in full), S11/release at 1 and the Windows presenter variants at 2.
    assert_eq!(find("S2").unwrap().variants, ["default", "flood"]);
    assert_eq!(find("S2").unwrap().run_caps, [("flood", 2)]);
    assert_eq!(find("S11").unwrap().variants, ["default", "gdi", "wgpu", "release"]);
    assert_eq!(find("S11").unwrap().run_caps, [("release", 1), ("gdi", 2), ("wgpu", 2)]);
    let value: serde_json::Value = serde_json::from_str(&list_json()).unwrap();
    for entry in value["scenarios"].as_array().unwrap() {
        if entry["id"] == "S2" {
            assert_eq!(entry["run_caps"], serde_json::json!({"flood": 2}));
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
