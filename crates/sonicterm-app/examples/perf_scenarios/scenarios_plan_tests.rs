//! Host and variant plan contracts including run caps.

use super::super::scenarios_tests::*;
use super::*;

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
        // S12/status-line only adds `program` and `accessibility` before `end`; everything else is
        // the default plan.
        let added = ["program", "accessibility"];
        let without_added: Vec<Step> = strip
            .steps
            .iter()
            .filter(|step| !matches!(step, Step::Phase(phase) if added.contains(&phase.name)))
            .cloned()
            .collect();
        assert_eq!(
            without_added.len() + 2,
            strip.steps.len(),
            "one program and one accessibility phase"
        );
        assert_eq!((strip.roles, strip.setup, without_added), (base.roles, base.setup, base.steps));
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
        tree_builds: 0,
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
        tree_builds: 0,
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
        tree_builds: 0,
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
        tree_builds: 0,
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
        tree_builds: 0,
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
        tree_builds: 0,
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

/// S12/status-line runs `program` after `uncovered`, then `accessibility`: a POSIX role prints an `OSC 9;4` set, a
/// burst of OSC 9 and OSC 777 notifications in one write, then the clear, each change 300 ms or more
/// apart so the strip and the taskbar see each one. S12/default has no such phase, and S12's short
/// run stays far inside its bound.
#[test]
fn s12_status_line_runs_the_program_phase() {
    for host in [Host::Posix, Host::Windows] {
        for short in [false, true] {
            let plan = plan_for("S12", "status-line", short, host).unwrap();
            assert_eq!(
                phase_names(&plan),
                ["print", "covered", "uncovered", "program", "accessibility"],
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
        tree_builds: 0,
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
        tree_builds: 0,
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
        tree_builds: 0,
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

/// S12/status-line ends with `accessibility`, a hold after `program`; S12/default has none. The
/// hold adds 5 s to the short nominal schedule and 10 s to the full one, inside S12's bounds.
#[test]
fn s12_status_line_ends_with_the_accessibility_hold() {
    for host in [Host::Posix, Host::Windows] {
        for (short, hold_s) in [(true, 5), (false, 10)] {
            let plan = plan_for("S12", "status-line", short, host).unwrap();
            assert_eq!(phase_names(&plan).last(), Some(&"accessibility"), "{host:?}");
            assert_eq!(phase(&plan, "accessibility").end, PhaseEnd::Hold(hold_s * 1_000));
            // The same plan without the hold: the hold alone adds its seconds to the schedule.
            let mut without = plan.clone();
            without.steps.retain(
                |step| !matches!(step, Step::Phase(phase) if phase.name == "accessibility"),
            );
            let nominal_s = seconds_after_go(&plan, true);
            assert_eq!(
                nominal_s,
                seconds_after_go(&without, true) + hold_s,
                "{host:?} short={short}"
            );
            let default = plan_for("S12", "default", short, host).unwrap();
            assert!(!phase_names(&default).contains(&"accessibility"));
            let spec = find("S12").unwrap();
            let bound = if short { spec.short_timeout_s } else { spec.timeout_s };
            assert!(nominal_s + 60 <= bound, "{host:?} short={short}");
        }
    }
}

/// `accessibility` keeps the idle contract and fails on any tree built without an assistive
/// technology.
#[test]
fn accessibility_phase_builds_no_tree_and_keeps_the_idle_bounds() {
    let deltas = |samples, redraws, tree_builds| StatusLineDeltas {
        samples,
        wakes: samples,
        redraws,
        sel_counts: 0,
        sel_cells: 0,
        history_shows: 0,
        held_shows: 0,
        glyph_shows: 0,
        tree_builds,
    };
    assert!(status_line_progress("accessibility", 5_000, false, deltas(3, 3, 0)).is_ok());
    let built = status_line_progress("accessibility", 5_000, false, deltas(3, 3, 1)).unwrap_err();
    assert!(built.contains("accessibility trees"), "{built}");
    assert!(
        status_line_progress("accessibility", 5_000, false, deltas(3, 5, 0)).is_err(),
        "extra frames"
    );
    assert!(
        status_line_progress("accessibility", 5_000, false, deltas(9, 9, 0)).is_err(),
        "extra wakes"
    );
}

/// `accessibility` bounds actual presents, from any path, by the strip's own redraws plus one
/// slack frame for the `program` toast expiring inside the hold and one in flight at its start; a
/// base whose App predates the counter is not checked.
#[test]
fn accessibility_phase_bounds_presents_by_strip_redraws() {
    assert!(accessibility_phase_presents(3, Some(3 + ACCESSIBILITY_PRESENT_SLACK)).is_ok());
    let extra = accessibility_phase_presents(3, Some(4 + ACCESSIBILITY_PRESENT_SLACK)).unwrap_err();
    assert!(extra.contains("presented"), "{extra}");
    assert!(accessibility_phase_presents(0, Some(100)).is_err(), "presents with no cause");
    assert!(accessibility_phase_presents(3, None).is_ok());
}

/// `program` settles past the held-input watch its last keystroke arms (`held_after_ms` after
/// admission), so the watch fires inside `program` and never adds a wake to the `accessibility`
/// hold that follows, whose cadence bound stays as it is.
#[test]
fn program_settles_past_the_held_input_watch_it_arms() {
    let held_after_ms = sonicterm_cfg::status_line::InputSettings::default().held_after_ms;
    for host in [Host::Posix, Host::Windows] {
        for short in [false, true] {
            let plan = plan_for("S12", "status-line", short, host).unwrap();
            let Driver::Commit { settle_ms, .. } = phase(&plan, "program").driver else {
                panic!("program is a commit driver");
            };
            assert!(
                settle_ms >= held_after_ms + STATUS_LINE_INTERVAL_MS / 4,
                "{host:?} short={short}: settle {settle_ms} ms, watch at {held_after_ms} ms"
            );
        }
    }
}
