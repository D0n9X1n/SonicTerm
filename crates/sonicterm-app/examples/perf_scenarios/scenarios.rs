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
    // S1/atlas-retry runs only in the counters set (perf-compare enforces it), capped at 2 short runs
    // per side; perf.yml places it in a shard with measured slack and records the projection.
    // S1/status-line is the idle shell with the status line on, one short run per side.
    capped(
        spec(
            "S1",
            "idle shell",
            &["default", "gdi", "wgpu", "role-exit", "atlas-retry", "status-line"],
            300,
            80,
        ),
        &[("atlas-retry", 2), ("status-line", 1)],
    ),
    // S2/flood is capped at 2 short runs to keep the PR comparison within 30 minutes; a release
    // comparison is never capped, so its full-length runs remain the variant's complete evidence.
    capped(spec("S2", "typing latency", &["default", "flood"], 300, 240), &[("flood", 2)]),
    spec("S3", "output flood throughput", &["default"], 480, 80),
    spec("S4", "visible streaming output", &["default"], 300, 240),
    spec("S5", "background tab streaming", &["default", "gdi", "wgpu"], 300, 240),
    // S6/rest is capped at 2 short runs a side to keep the shards that list it inside the PR budget; the
    // exact-head run, read with scripts/perf-critical-path.py, measures its time. A release comparison
    // runs it uncapped.
    capped(
        spec("S6", "pointer motion", &["default", "flood", "selection-drag", "rest"], 300, 240),
        &[("rest", 2)],
    ),
    spec("S7", "scrollback wheel", &["default"], 420, 360),
    spec("S8", "search", &["default"], 300, 240),
    spec("S9", "emoji and CJK text", &["default"], 300, 240),
    spec("S10", "full-screen redraw", &["default", "sync"], 300, 240),
    capped(
        spec("S11", "inline image tab switch", &["default", "gdi", "wgpu", "release"], 420, 300),
        &[("release", 1), ("gdi", 2), ("wgpu", 2)],
    ),
    // S12/status-line is the covered window with the status line on, one short run per side.
    capped(
        spec("S12", "covered window", &["default", "status-line"], 480, 360),
        &[("status-line", 1)],
    ),
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
    // The capabilities are unconditional: every build of this harness writes the split fields, a build
    // without perf-echo-trace with every credited sample `unsupported`; records every phase's kind; and
    // records S10's attribution, a build without the API cfg as `unavailable`; and gives every credited
    // S2 typing sample an echo timeline, a build without the API cfg or feature as `unavailable`.
    let capabilities = serde_json::json!({
        "latency_split_schema": SPLIT_SCHEMA,
        "phase_kinds": PHASE_KINDS_SCHEMA,
        "s10_attribution": ATTRIBUTION_SCHEMA,
        "echo_timeline_schema": ECHO_TIMELINE_SCHEMA,
        "guard_correlation_schema": GUARD_CORRELATION_SCHEMA,
    });
    serde_json::json!({ "schema_version": 1, "capabilities": capabilities, "scenarios": scenarios })
        .to_string()
}

/// The schema of the split fields in `latency`; a harness that writes them lists the same number
/// in `--list` as `capabilities.latency_split_schema`.
pub(crate) const SPLIT_SCHEMA: u32 = 1;

/// The phase-kinds schema: every phase records its kind and presentation trace and a transition its endpoint;
/// listed in `--list` as `capabilities.phase_kinds`.
pub(crate) const PHASE_KINDS_SCHEMA: u32 = 1;

/// The schema of S10's `s10_attribution` phase record; a harness that writes it lists the same number
/// in `--list` as `capabilities.s10_attribution`. It says what the harness can record, never whether
/// a side's App could arm a watch.
pub(crate) const ATTRIBUTION_SCHEMA: u32 = 1;

/// The schema of each credited S2/default and S2/flood sample's `echo_timeline`; listed in `--list` as
/// `capabilities.echo_timeline_schema`. It says what the harness writes, never whether a side's App
/// could record a timeline: a side that could not writes an `unavailable` object.
pub(crate) const ECHO_TIMELINE_SCHEMA: u32 = 1;

/// The guard-correlation contract: every finalized phase records `guard_correlation` with its window, and a
/// written one names its sidecar; listed in `--list` as `capabilities.guard_correlation_schema`. It says what
/// the harness writes, never whether a side's App could take records: such a side writes `unavailable` or
/// `gate_off`.
pub(crate) const GUARD_CORRELATION_SCHEMA: u32 = 1;

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
    /// S6/rest: clear the screen, create the rest target file, print a URL row and that file's
    /// absolute path, the sentinel on row 4, then stream `date` lines forever inside a scroll region
    /// that starts below the fixture rows, so the hovered rows never change.
    RestStream,
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
    /// 70 distinct numbered lines, one per visible row, for S1/atlas-retry.
    AtlasRetryRows,
    /// 5,000 full-width rows, every cell inside an OSC 8 hyperlink, for S1/status-line: linked
    /// cells cost far more than plain ones, so this history passes the grid's byte budget and
    /// is trimmed below its row limit.
    LinkedHistory,
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
    /// Ask the App to trim the covered window once its occlusion has been delivered; on a host
    /// without native occlusion the probe delivers a synthetic `Occluded(true)` first.
    TrimCovered,
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
    /// Settle the scene, then inject eight atlas recovery episodes into the main renderer.
    AtlasRetry,
    /// Rest the pointer on one grid cell, `(row, col)`, repeating the same `CursorMoved` at `hertz`
    /// as a resting hand's sensor does, never pressing.
    Park { row: u16, col: u16, hertz: u32 },
    /// Each step's text as one `Ime::Commit` into the active pane at its offset, then `settle_ms`.
    Commit { steps: &'static [CommitStep], settle_ms: u64 },
}

/// The text one commit step sends.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitText {
    /// This text as it is.
    Literal(&'static str),
    /// One character repeated, for a paste too large to spell out.
    Repeat(char, usize),
}

/// One commit, `after_ms` after its phase started.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CommitStep {
    /// Milliseconds after the phase starts.
    pub(crate) after_ms: u64,
    /// What it sends.
    pub(crate) text: CommitText,
}

/// S12/status-line's `program` on a POSIX role: set `OSC 9;4` progress to 40, then a burst of 50
/// and 70 printed 100 ms apart, inside the 250 ms taskbar interval, so 70 reaches the surface on the
/// interval's trailing edge; then an OSC 9 and an OSC 777 notification in one write, then the clear.
/// The set, the burst, the notifications and the clear are 300 ms or more apart.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const PROGRAM_POSIX_STEPS: &[CommitStep] = &[
    CommitStep { after_ms: 0, text: CommitText::Literal("\u{15}printf '\\033]9;4;1;40\\007'\r") },
    CommitStep {
        after_ms: 400,
        text: CommitText::Literal(
            "\u{15}printf '\\033]9;4;1;50\\007'; sleep 0.1; printf '\\033]9;4;1;70\\007'\r",
        ),
    },
    CommitStep {
        after_ms: 800,
        text: CommitText::Literal(
            "\u{15}printf '\\033]9;build done\\007\\033]777;notify;CI;green\\007'\r",
        ),
    },
    CommitStep { after_ms: 1100, text: CommitText::Literal("\u{15}printf '\\033]9;4;0\\007'\r") },
];

/// How long `program` settles after its last step: past the held-input watch that step's keystroke
/// arms (`held_after_ms`, 1500 ms by default) with a quarter interval to spare, so the watch wakes
/// inside `program` and never adds a sampleless wake to the `accessibility` hold that follows.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const PROGRAM_SETTLE_MS: u64 = 2_000;

/// S12/status-line's `program` on a Windows role, whose `cmd` cannot print ESC: Enter only, so the
/// phase still runs and holds the strip to its contract with no program output.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const PROGRAM_WINDOWS_STEPS: &[CommitStep] =
    &[CommitStep { after_ms: 0, text: CommitText::Literal("\r") }];

/// S1/status-line's `glyphs` on a POSIX role: clear the line and print U+0378, which no font
/// covers, with `printf`, so the line editor's locale never decides what is drawn.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const GLYPHS_POSIX_STEPS: &[CommitStep] =
    &[CommitStep { after_ms: 0, text: CommitText::Literal("\u{15}printf '\\315\\270\\n'\r") }];

/// S1/status-line's `glyphs` on a Windows role: the character itself, which `cmd` echoes.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const GLYPHS_WINDOWS_STEPS: &[CommitStep] =
    &[CommitStep { after_ms: 0, text: CommitText::Literal("\u{378}\r") }];

/// S1/status-line's `held` on a POSIX role: clear the line, stall the reader in raw mode for 1 s,
/// then paste 256 KiB, far past any PTY input buffer, so the writer blocks behind it until `head`
/// drains it. The scratch config sets `held_after_ms = 250`, so the block crosses it.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const HELD_POSIX_STEPS: &[CommitStep] = &[
    CommitStep {
        after_ms: 0,
        text: CommitText::Literal(
            "\u{15}stty raw -echo; sleep 1; head -c 262144 >/dev/null; stty sane\r",
        ),
    },
    CommitStep { after_ms: 300, text: CommitText::Repeat('x', 256 << 10) },
];

/// S1/status-line's `held` on a Windows role, whose shell has no raw mode to stall: Enter only.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const HELD_WINDOWS_STEPS: &[CommitStep] =
    &[CommitStep { after_ms: 0, text: CommitText::Literal("\r") }];

/// The grid row S6/rest prints the rest target's absolute path on; its pointer rests here.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const REST_PATH_ROW: u16 = 1;
/// The first grid row of S6/rest's streaming scroll region, 1-based as DECSTBM takes it: rows
/// 0 to 4 (the fixture, a blank row and the sentinel) stay fixed above it.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const REST_REGION_TOP: u16 = 6;
/// The grid row S6/rest's `tail` phase rests on, inside the streaming region.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const REST_TAIL_ROW: u16 = 40;

/// The hover counter deltas over one parked phase, read only while counters are on.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HoverDeltas {
    pub(crate) detections: u64,
    pub(crate) cache_hits: u64,
    pub(crate) probes_submitted: u64,
}

/// The status line's sampling interval the scratch config leaves at its default, in ms.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const STATUS_LINE_INTERVAL_MS: u64 = 2_000;

/// Cells one selection count may read; `sonicterm_ui::selection::SELECTION_EXTENT_MAX_CELLS`.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const SEL_CELLS_PER_COUNT: u64 = 200_000;

/// Status line counter deltas over one phase.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatusLineDeltas {
    /// Memory samples taken.
    pub(crate) samples: u64,
    /// Due status line wakes serviced.
    pub(crate) wakes: u64,
    /// Strip-only redraw requests (one per window whose strip pixels changed).
    pub(crate) redraws: u64,
    /// Selection counts the frame passes ran.
    pub(crate) sel_counts: u64,
    /// Cells those counts read.
    pub(crate) sel_cells: u64,
    /// Times `History: trimmed` appeared.
    pub(crate) history_shows: u64,
    /// Times `Input: held` appeared.
    pub(crate) held_shows: u64,
    /// Times `Glyphs:` appeared.
    pub(crate) glyph_shows: u64,
    /// Accessibility trees sent; 0 on a side whose App predates the counter.
    pub(crate) tree_builds: u64,
}

/// Program-reported change counts over one phase, from the App's counters.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramDeltas {
    /// `OSC 9;4` progress changes the App received.
    pub(crate) progress: u64,
    /// Notification batches the App received.
    pub(crate) notifies: u64,
    /// Redraws program toasts requested.
    pub(crate) toast_redraws: u64,
    /// Values the taskbar, Dock or launcher surface received.
    pub(crate) deliveries: u64,
    /// Frames the windows actually presented.
    pub(crate) presented: u64,
}

/// One value the counting progress surface received during the phase.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Delivered {
    /// Milliseconds after the phase's first delivery.
    pub(crate) at_ms: u64,
    /// The percent shown, or `None` for a clear.
    pub(crate) percent: Option<u8>,
}

/// Frames one committed step may present beyond the counted work: the typed line's echo, the
/// command's output (the burst prints twice, 100 ms apart) and the next prompt, each of which can
/// land in its own frame. Cursor blink is off in a status-line run, so it adds none.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const PRESENTS_PER_STEP: u64 = 6;

/// The taskbar interval as the harness measures it: both delivery times are read after their
/// dispatch began, so a held value can land a few ms short of the App's 250 ms.
#[cfg(any(target_os = "macos", windows, test))]
const DELIVERY_INTERVAL_MIN_MS: u64 = 240;

/// Whether `program` synced once per change and redrew nothing else, or why not: no strip redraw
/// beyond a sample, at most one toast redraw per notification batch, at most one surface delivery
/// per progress change. `program` is `None` on a side whose App predates the program counters.
///
/// A POSIX role (`posix`) prints the steps, so it must also show every batch with one redraw that
/// presented. `delivered` is the counting surface's record, `None` when the comparison's harness
/// cannot read it (`perf_taskbar_progress_api` off): with it, the surface must receive 40, then
/// the burst's last value 70 at least one interval after the value before it, then the clear last,
/// with no value repeated, reordered or stale, and no two deliveries closer than the interval.
/// Presents `accessibility` allows beyond its strip redraws: the `program` toast expiring inside the
/// hold, and one frame already in flight when the hold began.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const ACCESSIBILITY_PRESENT_SLACK: u64 = 2;

/// What one parked S6/rest phase observed between its start and its end.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParkEvidence {
    pub(crate) presented_frames: u64,
    pub(crate) streamed_rows_advanced: bool,
    /// The screen identity, history length, screen and viewport are those the phase started on.
    pub(crate) screen_unchanged: bool,
    /// No fixed fixture row was rewritten, a same-value repaint included.
    pub(crate) fixture_unchanged: bool,
    pub(crate) pointed_row_changed: bool,
    /// `None` when counters are off.
    pub(crate) hover: Option<HoverDeltas>,
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

/// How a phase is reported: a sustained workload, a finite transition to an endpoint, or a hold.
/// Set explicitly by each phase's scenario contract, never derived from how the phase ends.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PhaseKind {
    /// A paced or driven workload, reported with rates and intervals.
    Sustained,
    /// A finite change that completes at an endpoint.
    Transition,
    /// Activity only: no rates, intervals or completion.
    Hold,
}

#[cfg(any(target_os = "macos", windows, test))]
impl PhaseKind {
    /// The kind's name in `result.json`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Sustained => "sustained",
            Self::Transition => "transition",
            Self::Hold => "hold",
        }
    }
}

/// The event a transition's completion is measured to.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Endpoint {
    /// The latest first-observed completion sentinel among the required roles.
    SentinelParsed,
    /// The end of the first presenting dispatch after the image registration was observed.
    ImageRegisteredThenPresented,
    /// The first frame presented after the phase's entry act.
    FirstPresentAfterEntry,
    /// The first frame presented after the entry act with an image atlas item.
    FirstPresentWithImage,
    /// The end of the first App `about_to_wait` after a presentation: the warm-pool barrier.
    FirstPresentThenAboutToWait,
}

#[cfg(any(target_os = "macos", windows, test))]
impl Endpoint {
    /// The endpoint's name in `result.json`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::SentinelParsed => "sentinel-parsed",
            Self::ImageRegisteredThenPresented => "image-registered-then-presented",
            Self::FirstPresentAfterEntry => "first-present-after-entry",
            Self::FirstPresentWithImage => "first-present-with-image",
            Self::FirstPresentThenAboutToWait => "first-present-then-about-to-wait",
        }
    }
}

/// The startup phase, created by the probe rather than a plan: a transition to the warm-pool barrier.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const STARTUP_KIND: PhaseKind = PhaseKind::Transition;
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const STARTUP_ENDPOINT: Endpoint = Endpoint::FirstPresentThenAboutToWait;

/// One measured phase.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PhaseSpec {
    /// Phase name in `result.json`.
    pub(crate) name: &'static str,
    /// How the phase is reported, set by its scenario contract.
    pub(crate) kind: PhaseKind,
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

#[cfg(any(target_os = "macos", windows, test))]
impl PhaseSpec {
    /// A transition's endpoint, from how it ends; None for a hold or sustained phase.
    pub(crate) fn endpoint(&self) -> Option<Endpoint> {
        match (self.kind, &self.end) {
            (PhaseKind::Transition, PhaseEnd::Sentinels(_)) => Some(Endpoint::SentinelParsed),
            (PhaseKind::Transition, PhaseEnd::ImageRegistered(_)) => {
                Some(Endpoint::ImageRegisteredThenPresented)
            }
            (PhaseKind::Transition, PhaseEnd::MediaFree) => Some(Endpoint::FirstPresentAfterEntry),
            (PhaseKind::Transition, PhaseEnd::Reshow) => Some(Endpoint::FirstPresentWithImage),
            _ => None,
        }
    }
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
    /// `[window].rows` the scratch config requests; `None` keeps the harness default.
    pub(crate) window_rows: Option<u16>,
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
    /// The covered-window trim experiment this plan runs, as `result.json` names it; `None` when
    /// the plan asks for no trim.
    pub(crate) trim_experiment: Option<&'static str>,
    /// Whether the scratch config turns the status line on; its phases are checked by
    /// [`status_line_progress`].
    pub(crate) status_line: bool,
}

#[cfg(any(target_os = "macos", windows, test))]
impl Plan {
    /// Whether a role prints the linked history, the only fixture that passes the byte budget and
    /// so shows `History: trimmed`; `print`'s trim check applies only to such a plan.
    pub(crate) fn prints_linked_history(&self) -> bool {
        self.roles.contains(&Workload::PrintThenShell(Fixture::LinkedHistory))
    }
}

/// A phase of `kind` with no driver, no entry actions and no throughput figure.
#[cfg(any(target_os = "macos", windows, test))]
fn timed(name: &'static str, kind: PhaseKind, end: PhaseEnd) -> PhaseSpec {
    PhaseSpec {
        name,
        kind,
        enter: Vec::new(),
        driver: Driver::None,
        end,
        throughput_bytes: None,
        updates: None,
    }
}

/// A phase whose input comes from `driver`.
#[cfg(any(target_os = "macos", windows, test))]
fn driven(name: &'static str, kind: PhaseKind, driver: Driver, end: PhaseEnd) -> PhaseSpec {
    PhaseSpec { driver, ..timed(name, kind, end) }
}

/// A phase that runs `enter` as it starts.
#[cfg(any(target_os = "macos", windows, test))]
fn entered(name: &'static str, kind: PhaseKind, enter: Vec<Act>, end: PhaseEnd) -> PhaseSpec {
    PhaseSpec { enter, ..timed(name, kind, end) }
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

/// Grid rows the S1 and S12 status line comparisons request. The macOS runner clamps the window to
/// its screen at 43 rows, so a 70-row request lost the strip's row only on a ref that draws it. At
/// 13 pt the strip is under 1.4 rows, so 40 rows plus the strip fit unclamped on both refs.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const STRIP_SAFE_ROWS: u16 = 40;

/// The name `result.json` gives the short S12 plan's covered-window trim experiment.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const TRIM_EXPERIMENT: &str = "s12-short-trim";

#[cfg(test)]
#[path = "scenarios_tests.rs"]
mod scenarios_tests;

#[path = "scenarios_plan.rs"]
mod plan;
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) use plan::plan_for;
#[cfg(test)]
pub(crate) use plan::{all_plans, all_plans_on};
#[path = "scenarios_progress.rs"]
mod progress;
#[cfg(any(target_os = "macos", windows, test))]
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) use progress::{
    accessibility_phase_presents, park_progress, program_phase_progress,
    status_line_posix_progress, status_line_progress,
};
