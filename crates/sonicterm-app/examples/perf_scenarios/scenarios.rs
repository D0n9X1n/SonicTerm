//! The scenario catalog, S1 to S12, and each scenario's plan as data.
//!
//! The catalog is compiled everywhere because `--list` works on every platform.
//! Plans exist where they run (macOS and Windows) and in tests, which pin both hosts' plans on
//! every host.

/// One catalog entry, as `--list` prints it.
pub(crate) struct ScenarioSpec {
    /// Stable id the comparison script selects runs by.
    pub(crate) id: &'static str,
    /// Short description for the comparison table.
    pub(crate) title: &'static str,
    /// Variant names; the first is always `default`.
    pub(crate) variants: &'static [&'static str],
    /// Generous upper bound of a whole run in seconds, startup and checkpoints included.
    pub(crate) timeout_s: u64,
    /// The same bound for a `--short` run; the smoke gate allows S1 and S3 at most 80 s.
    pub(crate) short_timeout_s: u64,
    /// Per-variant caps on a `--short` comparison's valid runs, as `(variant, cap)`; empty for none.
    pub(crate) run_caps: &'static [(&'static str, u32)],
}

/// Every scenario the harness can run, in id order.
pub(crate) const SCENARIOS: &[ScenarioSpec] = &[
    spec("S1", "idle shell", &["default", "gdi", "wgpu", "role-exit"], 300, 80),
    // S2/flood is capped at 2 short runs to keep the PR comparison within 30 minutes; a release
    // comparison is never capped, so its full-length runs remain the variant's complete evidence.
    capped(spec("S2", "typing latency", &["default", "flood"], 300, 240), &[("flood", 2)]),
    spec("S3", "output flood throughput", &["default"], 480, 80),
    spec("S4", "visible streaming output", &["default"], 300, 240),
    spec("S5", "background tab streaming", &["default", "gdi", "wgpu"], 300, 240),
    spec("S6", "pointer motion", &["default", "flood", "selection-drag"], 300, 240),
    spec("S7", "scrollback wheel", &["default"], 420, 360),
    spec("S8", "search", &["default"], 300, 240),
    spec("S9", "emoji and CJK text", &["default"], 300, 240),
    spec("S10", "full-screen redraw", &["default", "sync"], 300, 240),
    capped(
        spec("S11", "inline image tab switch", &["default", "gdi", "wgpu", "release"], 420, 300),
        &[("release", 1), ("gdi", 2), ("wgpu", 2)],
    ),
    spec("S12", "covered window", &["default"], 480, 360),
];

const fn spec(
    id: &'static str,
    title: &'static str,
    variants: &'static [&'static str],
    timeout_s: u64,
    short_timeout_s: u64,
) -> ScenarioSpec {
    ScenarioSpec { id, title, variants, timeout_s, short_timeout_s, run_caps: &[] }
}

/// `spec` with per-variant short-mode run caps, which keep a long variant inside the PR budget.
const fn capped(spec: ScenarioSpec, run_caps: &'static [(&'static str, u32)]) -> ScenarioSpec {
    ScenarioSpec { run_caps, ..spec }
}

/// The `--list` document: schema version 1, the harness's capabilities and every scenario with
/// its bounds.
pub(crate) fn list_json() -> String {
    let scenarios: Vec<_> = SCENARIOS
        .iter()
        .map(|scenario| {
            let mut entry = serde_json::json!({
                "id": scenario.id,
                "variants": scenario.variants,
                "title": scenario.title,
                "timeout_s": scenario.timeout_s,
                "short_timeout_s": scenario.short_timeout_s,
            });
            if !scenario.run_caps.is_empty() {
                // When: run_caps is non-empty, the comparison caps those variants' short runs.
                let caps = scenario
                    .run_caps
                    .iter()
                    .map(|(variant, cap)| ((*variant).to_owned(), serde_json::json!(cap)))
                    .collect();
                entry["run_caps"] = serde_json::Value::Object(caps);
            }
            entry
        })
        .collect();
    // The capability is unconditional: every build of this harness writes the split fields, a build
    // without perf-echo-trace with every credited sample `unsupported`.
    let capabilities = serde_json::json!({ "latency_split_schema": SPLIT_SCHEMA });
    serde_json::json!({ "schema_version": 1, "capabilities": capabilities, "scenarios": scenarios })
        .to_string()
}

/// The schema of the split fields in `latency`; a harness that writes them lists the same number
/// in `--list` as `capabilities.latency_split_schema`.
pub(crate) const SPLIT_SCHEMA: u32 = 1;

/// The catalog entry for `id`, if the harness knows it.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn find(id: &str) -> Option<&'static ScenarioSpec> {
    SCENARIOS.iter().find(|scenario| scenario.id == id)
}

/// What one role's shell runs after GO.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Workload {
    /// `exec /bin/zsh -f` at the fixed prompt; no sentinel.
    IdleShell,
    /// `yes | head -n <lines>`, then `cat` of a seeded `bulk_bytes` fixture, then the sentinel.
    Flood { lines: u32, bulk_bytes: u64 },
    /// `while :; do date; sleep 0.01; done`, which never ends.
    DateLoop,
    /// `cat` of one fixture, the sentinel, then the idle shell.
    PrintThenShell(Fixture),
    /// `cat` of one fixture, the sentinel, then `exec sleep`, so no prompt redraws it.
    PrintThenSleep(Fixture),
    /// `count` full-screen frames paced at 60 per second; `synchronized` wraps each in DEC 2026.
    Frames { count: u32, synchronized: bool },
    /// Exit 1 right after GO, printing nothing: `role-exit`, which runs only on Windows.
    ExitAfterGo,
}

/// How a plan's config sets `[appearance].software_render_mode`.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Presentation {
    /// The config leaves the mode at its default.
    Configured,
    /// `force`: the `gdi` variant, which presents through Windows GDI.
    ForceGdi,
    /// `off`: the `wgpu` variant, which never degrades to the software presenter.
    ForceWgpu,
}

/// The host a plan is built for; tests pin both hosts' plans on every OS.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    /// macOS, and every other host that runs the POSIX role script.
    Posix,
    /// Windows, where the harness binary is every role's program.
    Windows,
}

/// The host this binary was built for.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const BUILD_HOST: Host = if cfg!(windows) { Host::Windows } else { Host::Posix };

/// A deterministic fixture generated into `workload/fixtures/`.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fixture {
    /// One screen of dense words, sized to the configured 250 × 70 grid.
    DenseScreen,
    /// One screen of words heavy in `e`, for search highlighting.
    SearchText,
    /// 12,000 numbered lines for scrollback.
    ScrollbackLines,
    /// 1,000 numbered lines, then one dense screen.
    HistoryScreen,
    /// Lines of emoji and CJK mixed with ASCII.
    EmojiCjk,
    /// One Sixel image of colored bands.
    Sixel,
    /// One OSC 1337 inline PNG of the same bands, for Windows, where Sixel never arrives through ConPTY.
    InlinePng,
}

/// A production action that opens the next role's pane or arranges tabs before GO.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupAction {
    /// `Action::NewTab`; opens the next role.
    NewTab,
    /// `Action::SplitRight`; opens the next role.
    SplitRight,
    /// `Action::SplitDown`; opens the next role.
    SplitDown,
    /// `Action::ActivateTab(index)`; opens nothing.
    ActivateTab(usize),
}

/// A synthetic action the probe performs at a phase's start or between phases.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    /// Deliver `WindowEvent::Focused(false)`.
    Unfocus,
    /// `Action::OpenSearch`.
    OpenSearch,
    /// One `Ime::Commit` of this text.
    Commit(&'static str),
    /// `Action::ActivateTab(index)`.
    ActivateTab(usize),
    /// Drop the window to the normal level and open the harness's cover over it.
    Cover,
    /// Raise the window again and close the cover.
    Uncover,
}

/// One step of a plan after GO.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// A measured phase.
    Phase(PhaseSpec),
    /// An action between phases, outside any measurement.
    Act(Act),
    /// A memory checkpoint at the end of the preceding timed phase; labels use `[a-z0-9-]`.
    Checkpoint(&'static str),
}

/// What injects synthetic input during a phase.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Driver {
    /// No input.
    None,
    /// `chars` single-character `Ime::Commit`s into `role`'s pane at `per_second`, then a settle.
    Typing { role: usize, chars: u32, per_second: u32, settle_ms: u64 },
    /// Hover-only `CursorMoved` across the tab bar and the grid.
    Sweep { hertz: u32 },
    /// Press, `CursorMoved` and release inside the grid only.
    Drag { hertz: u32 },
    /// `MouseWheel` line ticks over `role`'s pane, up through the retained history and back down.
    Wheel { hertz: u32, role: usize },
}

/// When a phase ends.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PhaseEnd {
    /// This many ms after the phase starts.
    Hold(u64),
    /// This many ms after GO, or at once when that time has passed.
    AfterGo(u64),
    /// When each listed role's completion sentinel is in its parsed grid.
    Sentinels(Vec<usize>),
    /// When the role's pane registers an inline image and a later frame presents.
    ImageRegistered(usize),
    /// When the driver finishes.
    DriverDone,
    /// The media-free barrier: the first frame presented after the phase's entry act, within 5 s.
    MediaFree,
    /// The reshow barrier: the first frame presented after the entry act whose image atlas holds an
    /// item, within 10 s.
    Reshow,
    /// `hold_ms` after the end of the earlier phase `anchor`, however late this phase starts.
    HoldFrom { anchor: &'static str, hold_ms: u64 },
}

/// Which checkpoint's memory reading becomes fresh only `delay_ms` after `anchor` phase ended.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FreshAfter {
    /// The checkpoint label it applies to.
    pub(crate) checkpoint: &'static str,
    /// The phase whose end starts the delay.
    pub(crate) anchor: &'static str,
    /// The delay in ms.
    pub(crate) delay_ms: u64,
}

/// One measured phase.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PhaseSpec {
    /// Phase name in `result.json`.
    pub(crate) name: &'static str,
    /// Actions run as the phase starts, inside its measurement.
    pub(crate) enter: Vec<Act>,
    /// Input injected during the phase.
    pub(crate) driver: Driver,
    /// When the phase ends.
    pub(crate) end: PhaseEnd,
    /// Bytes the workload writes from GO to its sentinel, for a throughput figure.
    pub(crate) throughput_bytes: Option<u64>,
    /// The logical updates the selected workload plays in this phase, the denominator of a
    /// comparison's presented frames per update; `None` for a phase that plays no counted updates.
    pub(crate) updates: Option<u32>,
}

/// One scenario variant's complete plan: each role's workload, setup before GO, and steps after.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    /// Scenario id.
    pub(crate) scenario: &'static str,
    /// Variant name.
    pub(crate) variant: &'static str,
    /// Whether `--short` shortened the holds and the flood.
    pub(crate) short: bool,
    /// The harness's own deadline for the whole run, in seconds.
    pub(crate) timeout_s: u64,
    /// `[terminal].scrollback` rows written to the scratch config.
    pub(crate) scrollback_rows: usize,
    /// Whether the window gets a synthetic `Focused(true)` before setup.
    pub(crate) focused: bool,
    /// Each role's workload, in role order; role 0 is the first tab's shell.
    pub(crate) roles: Vec<Workload>,
    /// Actions that open roles 1 and later, or arrange tabs, before GO.
    pub(crate) setup: Vec<SetupAction>,
    /// Phases, actions and checkpoints after GO.
    pub(crate) steps: Vec<Step>,
    /// How the scratch config sets the software render mode.
    pub(crate) presentation: Presentation,
    /// The checkpoint whose reading is fresh only some time after an earlier phase, if any.
    pub(crate) fresh_after: Option<FreshAfter>,
}

/// A phase with no driver, no entry actions and no throughput figure.
#[cfg(any(target_os = "macos", windows, test))]
fn timed(name: &'static str, end: PhaseEnd) -> PhaseSpec {
    PhaseSpec {
        name,
        enter: Vec::new(),
        driver: Driver::None,
        end,
        throughput_bytes: None,
        updates: None,
    }
}

/// A phase whose input comes from `driver`.
#[cfg(any(target_os = "macos", windows, test))]
fn driven(name: &'static str, driver: Driver, end: PhaseEnd) -> PhaseSpec {
    PhaseSpec { driver, ..timed(name, end) }
}

/// A phase that runs `enter` as it starts.
#[cfg(any(target_os = "macos", windows, test))]
fn entered(name: &'static str, enter: Vec<Act>, end: PhaseEnd) -> PhaseSpec {
    PhaseSpec { enter, ..timed(name, end) }
}

/// How long S7's `settle` phase runs with no input after `scroll`, in ms: the
/// 600 ms scrollbar idle window, its 300 ms fade-out and a margin. `--short`
/// keeps it, because a shorter window would not reach the fade.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const SETTLE_MS: u64 = 1_500;

/// The plan for `id` and `variant` on this build's host, or `None` when the catalog does not list them.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn plan(id: &str, variant: &str, short: bool) -> Option<Plan> {
    plan_for(id, variant, short, BUILD_HOST)
}

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
    let print = |roles: Vec<usize>| Step::Phase(timed("print", PhaseEnd::Sentinels(roles)));
    // Plans that end before GO + 60 s (5 s short) idle up to it, so memory is read after 60 s.
    let idle = Step::Phase(timed("idle", PhaseEnd::AfterGo(hold(60_000))));
    let end = Step::Checkpoint("end");
    let sweep =
        Step::Phase(driven("sweep", Driver::Sweep { hertz: 120 }, PhaseEnd::Hold(hold(10_000))));
    let (roles, setup, steps) = match (spec.id, variant) {
        // role-exit plans exactly like the idle default; only its role's program exits after GO.
        ("S1", _) => (
            vec![if variant == "role-exit" { Workload::ExitAfterGo } else { Workload::IdleShell }],
            vec![],
            vec![Step::Phase(timed("idle", PhaseEnd::Hold(hold(60_000)))), end],
        ),
        ("S2", "default") => (
            vec![Workload::IdleShell],
            vec![],
            vec![Step::Phase(driven("typing", typing(0), PhaseEnd::DriverDone)), idle, end],
        ),
        ("S2", _) => (
            vec![flood, Workload::IdleShell],
            vec![SetupAction::SplitRight],
            vec![Step::Phase(driven("typing", typing(1), PhaseEnd::DriverDone)), idle, end],
        ),
        ("S3", _) => {
            let mut flood_phase = timed("flood", PhaseEnd::Sentinels(vec![0]));
            // `yes` writes two bytes a line; `cat` writes the fixture as it is.
            flood_phase.throughput_bytes = Some(2 * u64::from(flood_lines) + bulk_bytes);
            let rest = Step::Phase(timed("idle", PhaseEnd::Hold(hold(60_000))));
            (vec![flood], vec![], vec![Step::Phase(flood_phase), rest, end])
        }
        ("S4", _) => (
            vec![Workload::DateLoop],
            vec![],
            vec![Step::Phase(timed("stream", PhaseEnd::Hold(hold(60_000)))), end],
        ),
        ("S5", _) => (
            vec![Workload::IdleShell, Workload::DateLoop],
            vec![SetupAction::NewTab, SetupAction::ActivateTab(0)],
            vec![Step::Phase(timed("stream", PhaseEnd::Hold(hold(60_000)))), end],
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
        ("S6", _) => (
            vec![Workload::PrintThenShell(Fixture::DenseScreen)],
            vec![],
            vec![
                print(vec![0]),
                Step::Phase(driven(
                    "drag",
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
                    Driver::Wheel { hertz: 60, role: 0 },
                    PhaseEnd::DriverDone,
                )),
                // No input: the frames a scrollbar requests once scrolling stops.
                Step::Phase(timed("settle", PhaseEnd::Hold(SETTLE_MS))),
                idle,
                end,
            ],
        ),
        ("S8", _) => {
            let enter = vec![Act::OpenSearch, Act::Commit("e")];
            let search = entered("search", enter, PhaseEnd::Hold(hold(10_000)));
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
                Step::Phase(timed("hold", PhaseEnd::Hold(hold(10_000)))),
                idle,
                end,
            ],
        ),
        ("S10", _) => {
            // 60 frames a second for 20 s, or 5 s short.
            let count = if short { 300 } else { 1_200 };
            let frames = Workload::Frames { count, synchronized: variant == "sync" };
            // The stream phase records the count this run's workload plays, not a constant.
            let stream =
                PhaseSpec { updates: Some(count), ..timed("stream", PhaseEnd::Sentinels(vec![0])) };
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
                Step::Phase(timed("image", PhaseEnd::ImageRegistered(0))),
                Step::Phase(entered("media-free", vec![Act::ActivateTab(1)], PhaseEnd::MediaFree)),
                Step::Checkpoint("switched"),
                Step::Phase(timed(
                    "released-hold",
                    PhaseEnd::HoldFrom { anchor: "media-free", hold_ms: 65_000 },
                )),
                Step::Checkpoint("released"),
                Step::Phase(entered("reshow", vec![Act::ActivateTab(0)], PhaseEnd::Reshow)),
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
                Step::Phase(timed("image", PhaseEnd::ImageRegistered(0))),
                Step::Act(Act::ActivateTab(1)),
                Step::Checkpoint("switched"),
                Step::Phase(timed("idle", PhaseEnd::Hold(hold(120_000)))),
                end,
            ],
        ),
        ("S12", _) => (
            vec![Workload::PrintThenShell(Fixture::HistoryScreen); 3],
            vec![SetupAction::SplitRight, SetupAction::SplitDown],
            vec![
                print(vec![0, 1, 2]),
                Step::Checkpoint("settled"),
                Step::Phase(entered(
                    "covered",
                    vec![Act::Unfocus, Act::Cover],
                    PhaseEnd::Hold(hold(90_000)),
                )),
                Step::Checkpoint("covered"),
                Step::Phase(entered("uncovered", vec![Act::Uncover], PhaseEnd::Hold(hold(10_000)))),
                end,
            ],
        ),
        _ => return None,
    };
    Some(Plan {
        scenario: spec.id,
        variant,
        short,
        timeout_s: if short { spec.short_timeout_s } else { spec.timeout_s },
        // Only S7 asks for more history than the default; the per-pane cell budget clamps it.
        scrollback_rows: if spec.id == "S7" { 10_000 } else { 1_000 },
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
#[path = "scenarios_tests.rs"]
mod scenarios_tests;
