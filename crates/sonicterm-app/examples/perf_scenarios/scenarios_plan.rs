//! Deterministic host-specific scenario plans and their phase endpoints.

#[cfg(test)]
use super::SCENARIOS;
#[cfg(any(target_os = "macos", windows, test))]
use super::{
    driven, entered, find, timed, Act, Driver, Fixture, FreshAfter, Host, PhaseEnd, PhaseKind,
    PhaseSpec, Plan, Presentation, SetupAction, Step, Workload, GLYPHS_POSIX_STEPS,
    GLYPHS_WINDOWS_STEPS, HELD_POSIX_STEPS, HELD_WINDOWS_STEPS, PROGRAM_POSIX_STEPS,
    PROGRAM_SETTLE_MS, PROGRAM_WINDOWS_STEPS, REST_PATH_ROW, REST_TAIL_ROW, SETTLE_MS,
    STRIP_SAFE_ROWS, TRIM_EXPERIMENT,
};

/// The plan for `id` and `variant` on `host`, or `None` when the catalog does not list them.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn plan_for(id: &str, variant: &str, short: bool, host: Host) -> Option<Plan> {
    let spec = find(id)?;
    let variant = *spec.variants.iter().find(|listed| **listed == variant)?;
    // `--short` shortens every hold to 5 s; drivers keep their sample counts.
    let hold = |full_ms: u64| if short { 5_000 } else { full_ms };
    let (flood_lines, bulk_bytes) = if short { (200_000, 5 << 20) } else { (2_000_000, 50 << 20) };
    let flood = Workload::Flood { lines: flood_lines, bulk_bytes };
    let typing = |role| Driver::Typing { role, chars: 200, per_second: 10, settle_ms: 2_000 };
    let print = |roles: Vec<usize>| {
        Step::Phase(timed("print", PhaseKind::Transition, PhaseEnd::Sentinels(roles)))
    };
    // Plans that end before GO + 60 s (5 s short) idle up to it, so memory is read after 60 s.
    let idle = Step::Phase(timed("idle", PhaseKind::Hold, PhaseEnd::AfterGo(hold(60_000))));
    let end = Step::Checkpoint("end");
    let sweep = Step::Phase(driven(
        "sweep",
        PhaseKind::Sustained,
        Driver::Sweep { hertz: 120 },
        PhaseEnd::Hold(hold(10_000)),
    ));
    let (roles, setup, steps) = match (spec.id, variant) {
        // atlas-retry prints its rows, then drives the recovery episodes until they end.
        ("S1", "atlas-retry") => (
            vec![Workload::PrintThenSleep(Fixture::AtlasRetryRows)],
            vec![],
            vec![
                print(vec![0]),
                Step::Phase(driven(
                    "recovery",
                    PhaseKind::Hold,
                    Driver::AtlasRetry,
                    PhaseEnd::DriverDone,
                )),
                idle,
                end,
            ],
        ),
        // status-line first prints a linked history that the byte budget trims, which shows
        // `History: trimmed` once; then it keeps the idle default's hold, drags a selection and
        // holds it: the drag counts each changed selection once, within the cell bound, and the
        // held one never again.
        ("S1", "status-line") => (
            vec![Workload::PrintThenShell(Fixture::LinkedHistory)],
            vec![],
            vec![
                print(vec![0]),
                Step::Phase(timed("idle", PhaseKind::Hold, PhaseEnd::Hold(hold(60_000)))),
                Step::Phase(driven(
                    "select",
                    PhaseKind::Sustained,
                    Driver::Drag { hertz: 120 },
                    PhaseEnd::Hold(if short { 2_000 } else { 10_000 }),
                )),
                Step::Phase(timed(
                    "selected",
                    PhaseKind::Hold,
                    PhaseEnd::Hold(if short { 2_000 } else { 10_000 }),
                )),
                Step::Phase(driven(
                    "input",
                    PhaseKind::Sustained,
                    Driver::Typing { role: 0, chars: 10, per_second: 10, settle_ms: 1_000 },
                    PhaseEnd::DriverDone,
                )),
                Step::Phase(driven(
                    "glyphs",
                    PhaseKind::Sustained,
                    if host == Host::Windows {
                        Driver::Commit { steps: GLYPHS_WINDOWS_STEPS, settle_ms: 1_500 }
                    } else {
                        // When: host is POSIX, printf draws U+0378 whatever the shell's locale.
                        Driver::Commit { steps: GLYPHS_POSIX_STEPS, settle_ms: 1_500 }
                    },
                    PhaseEnd::DriverDone,
                )),
                Step::Phase(driven(
                    "held",
                    PhaseKind::Sustained,
                    if host == Host::Windows {
                        Driver::Commit { steps: HELD_WINDOWS_STEPS, settle_ms: 500 }
                    } else {
                        // When: host is POSIX, its shell can stall the reader behind a paste.
                        Driver::Commit { steps: HELD_POSIX_STEPS, settle_ms: 1_800 }
                    },
                    PhaseEnd::DriverDone,
                )),
                end,
            ],
        ),
        // role-exit plans exactly like the idle default; only its role's program exits after GO.
        ("S1", _) => (
            vec![if variant == "role-exit" { Workload::ExitAfterGo } else { Workload::IdleShell }],
            vec![],
            vec![Step::Phase(timed("idle", PhaseKind::Hold, PhaseEnd::Hold(hold(60_000)))), end],
        ),
        ("S2", "default") => (
            vec![Workload::IdleShell],
            vec![],
            vec![
                Step::Phase(driven(
                    "typing",
                    PhaseKind::Sustained,
                    typing(0),
                    PhaseEnd::DriverDone,
                )),
                idle,
                end,
            ],
        ),
        ("S2", _) => (
            vec![flood, Workload::IdleShell],
            vec![SetupAction::SplitRight],
            vec![
                Step::Phase(driven(
                    "typing",
                    PhaseKind::Sustained,
                    typing(1),
                    PhaseEnd::DriverDone,
                )),
                idle,
                end,
            ],
        ),
        ("S3", _) => {
            let mut flood_phase =
                timed("flood", PhaseKind::Sustained, PhaseEnd::Sentinels(vec![0]));
            // `yes` writes two bytes a line; `cat` writes the fixture as it is.
            flood_phase.throughput_bytes = Some(2 * u64::from(flood_lines) + bulk_bytes);
            let rest = Step::Phase(timed("idle", PhaseKind::Hold, PhaseEnd::Hold(hold(60_000))));
            (vec![flood], vec![], vec![Step::Phase(flood_phase), rest, end])
        }
        ("S4", _) => (
            vec![Workload::DateLoop],
            vec![],
            vec![
                Step::Phase(timed("stream", PhaseKind::Sustained, PhaseEnd::Hold(hold(60_000)))),
                end,
            ],
        ),
        ("S5", _) => (
            vec![Workload::IdleShell, Workload::DateLoop],
            vec![SetupAction::NewTab, SetupAction::ActivateTab(0)],
            vec![Step::Phase(timed("stream", PhaseKind::Hold, PhaseEnd::Hold(hold(60_000)))), end],
        ),
        ("S6", "default") => (
            vec![Workload::IdleShell; 3],
            vec![SetupAction::NewTab, SetupAction::NewTab],
            vec![sweep, idle, end],
        ),
        // The flooding first tab is shown, so the sweep runs over a grid that keeps changing.
        ("S6", "flood") => (
            vec![flood, Workload::IdleShell, Workload::IdleShell],
            vec![SetupAction::NewTab, SetupAction::NewTab, SetupAction::ActivateTab(0)],
            vec![sweep, idle, end],
        ),
        // The pointer rests on the path row while rows below stream, then on the streaming rows.
        ("S6", "rest") => {
            let park = |row| Driver::Park { row, col: 3, hertz: 30 };
            (
                vec![Workload::RestStream],
                vec![],
                vec![
                    print(vec![0]),
                    Step::Phase(driven(
                        "warmup",
                        PhaseKind::Hold,
                        park(REST_PATH_ROW),
                        PhaseEnd::Hold(1_000),
                    )),
                    Step::Phase(driven(
                        "rest",
                        PhaseKind::Sustained,
                        park(REST_PATH_ROW),
                        PhaseEnd::Hold(hold(10_000)),
                    )),
                    Step::Phase(driven(
                        "tail",
                        PhaseKind::Sustained,
                        park(REST_TAIL_ROW),
                        PhaseEnd::Hold(hold(10_000)),
                    )),
                    idle,
                    end,
                ],
            )
        }
        ("S6", _) => (
            vec![Workload::PrintThenShell(Fixture::DenseScreen)],
            vec![],
            vec![
                print(vec![0]),
                Step::Phase(driven(
                    "drag",
                    PhaseKind::Sustained,
                    Driver::Drag { hertz: 120 },
                    PhaseEnd::Hold(hold(10_000)),
                )),
                idle,
                end,
            ],
        ),
        ("S7", _) => (
            vec![Workload::PrintThenShell(Fixture::ScrollbackLines)],
            vec![],
            vec![
                print(vec![0]),
                Step::Phase(driven(
                    "scroll",
                    PhaseKind::Sustained,
                    Driver::Wheel { hertz: 60, role: 0 },
                    PhaseEnd::DriverDone,
                )),
                // No input: the frames a scrollbar requests once scrolling stops.
                Step::Phase(timed("settle", PhaseKind::Hold, PhaseEnd::Hold(SETTLE_MS))),
                idle,
                end,
            ],
        ),
        ("S8", _) => {
            let enter = vec![Act::OpenSearch, Act::Commit("e")];
            let search = entered("search", PhaseKind::Hold, enter, PhaseEnd::Hold(hold(10_000)));
            (
                vec![Workload::PrintThenShell(Fixture::SearchText)],
                vec![],
                vec![print(vec![0]), Step::Phase(search), idle, end],
            )
        }
        ("S9", _) => (
            vec![Workload::PrintThenShell(Fixture::EmojiCjk)],
            vec![],
            vec![
                print(vec![0]),
                Step::Phase(timed("hold", PhaseKind::Hold, PhaseEnd::Hold(hold(10_000)))),
                idle,
                end,
            ],
        ),
        ("S10", _) => {
            // 60 frames a second for 20 s, or 5 s short.
            let count = if short { 300 } else { 1_200 };
            let frames = Workload::Frames { count, synchronized: variant == "sync" };
            // The stream phase records the count this run's workload plays, not a constant.
            let stream = PhaseSpec {
                updates: Some(count),
                ..timed("stream", PhaseKind::Sustained, PhaseEnd::Sentinels(vec![0]))
            };
            (vec![frames], vec![], vec![Step::Phase(stream), idle, end])
        }
        ("S11", "release") => (
            // The image tab is left until a media-free frame presents, held 65 s from that frame
            // (unshortened, since the idle release needs 30 s), then shown again.
            vec![
                Workload::PrintThenSleep(if host == Host::Windows {
                    Fixture::InlinePng
                } else {
                    Fixture::Sixel
                }),
                Workload::IdleShell,
            ],
            vec![SetupAction::NewTab, SetupAction::ActivateTab(0)],
            vec![
                Step::Phase(timed("image", PhaseKind::Transition, PhaseEnd::ImageRegistered(0))),
                Step::Phase(entered(
                    "media-free",
                    PhaseKind::Transition,
                    vec![Act::ActivateTab(1)],
                    PhaseEnd::MediaFree,
                )),
                Step::Checkpoint("switched"),
                Step::Phase(timed(
                    "released-hold",
                    PhaseKind::Hold,
                    PhaseEnd::HoldFrom { anchor: "media-free", hold_ms: 65_000 },
                )),
                Step::Checkpoint("released"),
                Step::Phase(entered(
                    "reshow",
                    PhaseKind::Transition,
                    vec![Act::ActivateTab(0)],
                    PhaseEnd::Reshow,
                )),
                end,
            ],
        ),
        ("S11", _) => (
            // Sixel never arrives through ConPTY, so Windows prints the same bands as an inline PNG.
            vec![
                Workload::PrintThenSleep(if host == Host::Windows {
                    Fixture::InlinePng
                } else {
                    Fixture::Sixel
                }),
                Workload::IdleShell,
            ],
            vec![SetupAction::NewTab, SetupAction::ActivateTab(0)],
            vec![
                Step::Phase(timed("image", PhaseKind::Transition, PhaseEnd::ImageRegistered(0))),
                Step::Act(Act::ActivateTab(1)),
                Step::Checkpoint("switched"),
                Step::Phase(timed("idle", PhaseKind::Hold, PhaseEnd::Hold(hold(120_000)))),
                end,
            ],
        ),
        ("S12", _) => {
            let mut steps = vec![
                print(vec![0, 1, 2]),
                Step::Checkpoint("settled"),
                // `--short` cannot reach the 30 s scheduler, so it asks for the trim once covered.
                Step::Phase(entered(
                    "covered",
                    PhaseKind::Hold,
                    if short {
                        vec![Act::Unfocus, Act::Cover, Act::TrimCovered]
                    } else {
                        vec![Act::Unfocus, Act::Cover]
                    },
                    PhaseEnd::Hold(hold(90_000)),
                )),
                Step::Checkpoint("covered"),
                Step::Phase(entered(
                    "uncovered",
                    PhaseKind::Hold,
                    vec![Act::Uncover],
                    PhaseEnd::Hold(hold(10_000)),
                )),
            ];
            if variant == "status-line" {
                // The window is visible again, so program notifications and progress reach the
                // toast and the taskbar; the default plan stays as it was.
                let program_steps =
                    if host == Host::Windows { PROGRAM_WINDOWS_STEPS } else { PROGRAM_POSIX_STEPS };
                steps.push(Step::Phase(driven(
                    "program",
                    PhaseKind::Sustained,
                    Driver::Commit { steps: program_steps, settle_ms: PROGRAM_SETTLE_MS },
                    PhaseEnd::DriverDone,
                )));
                // Every terminal window holds a dormant accessibility adapter: with no assistive
                // technology the strip must build no tree and present no extra frame. It follows
                // `program`, so the toast that phase raised has already been drawn.
                steps.push(Step::Phase(timed(
                    "accessibility",
                    PhaseKind::Hold,
                    PhaseEnd::Hold(hold(10_000)),
                )));
            }
            steps.push(end);
            (
                vec![Workload::PrintThenShell(Fixture::HistoryScreen); 3],
                vec![SetupAction::SplitRight, SetupAction::SplitDown],
                steps,
            )
        }
        _ => return None,
    };
    Some(Plan {
        scenario: spec.id,
        variant,
        short,
        timeout_s: if short { spec.short_timeout_s } else { spec.timeout_s },
        // Only S7 asks for more history than the default; the per-pane cell budget clamps it.
        // S1/status-line's linked history needs room to pass the byte budget below the row limit.
        scrollback_rows: if spec.id == "S7" || (spec.id == "S1" && variant == "status-line") {
            10_000
        } else {
            // When: the plan is neither S7 nor S1/status-line, the default history is enough.
            1_000
        },
        // S1 and S12 carry the status line comparisons; every variant of theirs but atlas-retry,
        // whose fixture fills 70 rows, requests a grid below the macOS runner's screen clamp with
        // the strip on, so both refs measure the same grid and the presenter variants stay alike.
        window_rows: (matches!(spec.id, "S1" | "S12") && variant != "atlas-retry")
            .then_some(STRIP_SAFE_ROWS),
        focused: true,
        roles,
        setup,
        steps,
        presentation: match variant {
            "gdi" => Presentation::ForceGdi,
            "wgpu" => Presentation::ForceWgpu,
            _ => Presentation::Configured,
        },
        // S11/release reads `released` only once the idle release can have happened.
        fresh_after: (spec.id == "S11" && variant == "release").then_some(FreshAfter {
            checkpoint: "released",
            anchor: "media-free",
            delay_ms: 30_000,
        }),
        trim_experiment: (spec.id == "S12" && short).then_some(TRIM_EXPERIMENT),
        status_line: variant == "status-line",
    })
}

/// Every listed scenario, variant and length, for the tests that pin all plans.
#[cfg(test)]
pub(crate) fn all_plans() -> Vec<Plan> {
    all_plans_on(Host::Posix)
}

/// Every listed scenario, variant and length as planned for `host`.
#[cfg(test)]
pub(crate) fn all_plans_on(host: Host) -> Vec<Plan> {
    SCENARIOS
        .iter()
        .flat_map(|scenario| {
            scenario.variants.iter().flat_map(move |variant| {
                [false, true]
                    .map(|short| plan_for(scenario.id, variant, short, host).expect("listed plan"))
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "scenarios_plan_tests.rs"]
mod scenarios_plan_tests;
