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
}

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
}

/// Whether a `status-line` phase kept the strip's idle contract, or why not.
///
/// `idle` and `uncovered` hold `hold_ms`: samples follow the 2 s interval with no extra wake, so
/// at most `hold_ms / interval + 1` samples (the `+ 1` is a sample at the phase edge), and a strip
/// redraw needs a changed sample, so redraws never exceed samples plus one. `uncovered` must take
/// at least one sample, the resume sample. `covered`, when the App received `Occluded(true)`
/// (`occluded`), takes at most one sample and one wake: the one that can land between the cover
/// and its delivery. Without delivered occlusion (Windows outside the short trim) the covered
/// phase is not checked, because the platform reports no occlusion to pause on. `select` drags a
/// selection: it must count at least once and read at most 200,000 cells per count; `selected`
/// holds that selection unchanged, so it must not count again. `print` floods the linked history
/// with `history` and `size` listed: `History: trimmed` appears exactly once, and the strip redraws
/// only for a sample or that appearance, never per output frame.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn status_line_progress(
    phase: &str,
    hold_ms: u64,
    occluded: bool,
    deltas: StatusLineDeltas,
) -> Result<(), String> {
    let cadence_bound = hold_ms / STATUS_LINE_INTERVAL_MS + 1;
    match phase {
        "covered" if occluded && (deltas.samples > 1 || deltas.wakes > 1) => Err(format!(
            "status line sampled {} times with {} wakes while covered",
            deltas.samples, deltas.wakes
        )),
        "covered" => Ok(()),
        "uncovered" if deltas.samples == 0 => {
            Err("status line took no sample after the window was uncovered".to_owned())
        }
        "idle" | "uncovered" if deltas.samples > cadence_bound || deltas.wakes > cadence_bound => {
            Err(format!(
                "status line sampled {} times with {} wakes in {hold_ms} ms, more than {cadence_bound}",
                deltas.samples, deltas.wakes
            ))
        }
        "idle" | "uncovered" if deltas.redraws > deltas.samples + 1 => Err(format!(
            "status line requested {} redraws for {} samples",
            deltas.redraws, deltas.samples
        )),
        "print" if deltas.history_shows != 1 => Err(format!(
            "`History: trimmed` appeared {} times while the linked history printed, not once",
            deltas.history_shows
        )),
        "print" if deltas.redraws > deltas.samples + deltas.history_shows + 1 => Err(format!(
            "status line requested {} redraws for {} samples during output",
            deltas.redraws, deltas.samples
        )),
        "select" if deltas.sel_counts == 0 => {
            Err("the drag selected nothing the status line counted".to_owned())
        }
        "select" if deltas.sel_cells > deltas.sel_counts * SEL_CELLS_PER_COUNT => Err(format!(
            "status line read {} cells in {} selection counts, more than {SEL_CELLS_PER_COUNT} each",
            deltas.sel_cells, deltas.sel_counts
        )),
        "selected" if deltas.sel_counts > 0 => Err(format!(
            "status line recounted an unchanged selection {} times",
            deltas.sel_counts
        )),
        _ => Ok(()),
    }
}

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

/// Whether a parked phase measured what it claims, or why not.
///
/// Every parked phase must present frames and see the streamed rows advance, or it measured an
/// idle screen; its screen and viewport must stay the ones it started on, and a repaint of its
/// fixture rows makes the run invalid rather than passing. `rest` claims the pointer rests over
/// unchanged rows, so with counters on it must hit the hover cache and run no detection and
/// submit no probe after warm-up. `tail` rests on streaming rows, so its pointed row must change
/// and, with counters on, it must report detections: the cache misses there by design.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) fn park_progress(phase: &str, evidence: &ParkEvidence) -> Result<(), String> {
    if evidence.presented_frames == 0 {
        // When: no frame presented, the phase measured nothing the pointer could rest over.
        return Err(format!("S6/rest phase {phase} presented no frame"));
    }
    if !evidence.streamed_rows_advanced {
        // When: the streamed rows never changed, the scroll region did not stream.
        return Err(format!("S6/rest phase {phase} saw no streamed row advance"));
    }
    if !evidence.screen_unchanged {
        // When: the screen identity or viewport changed, the rows are not the ones the phase started on.
        return Err(format!("S6/rest phase {phase} changed its screen or viewport"));
    }
    if !evidence.fixture_unchanged {
        // When: a fixture row was rewritten, the pointer no longer rests over unchanged rows.
        return Err(format!("S6/rest phase {phase} saw its fixture rows repainted"));
    }
    if phase == "tail" {
        if !evidence.pointed_row_changed {
            // When: the tail's pointed row never changed, the phase rested on a stationary row.
            return Err("S6/rest phase tail rested on a row that never changed".to_owned());
        }
        if evidence.hover.is_some_and(|hover| hover.detections == 0) {
            // When: the tail rests on changing rows yet ran no detection, the hover counters are broken.
            return Err("S6/rest phase tail ran no detection over streaming rows".to_owned());
        }
    }
    if let (true, Some(hover)) = (phase == "rest", evidence.hover) {
        // When: the rest phase has counters, its warm cache must answer every lookup.
        if hover.detections > 0 || hover.probes_submitted > 0 {
            // When: detections or probes_submitted is nonzero, the rest phase did work after warm-up.
            return Err(format!(
                "S6/rest phase rest ran {} detections and {} probe submissions after warm-up",
                hover.detections, hover.probes_submitted
            ));
        }
        if hover.cache_hits == 0 {
            // When: cache_hits is zero, no lookup reused the warm result.
            return Err("S6/rest phase rest never hit the hover cache".to_owned());
        }
    }
    Ok(())
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
        ("S12", _) => (
            vec![Workload::PrintThenShell(Fixture::HistoryScreen); 3],
            vec![SetupAction::SplitRight, SetupAction::SplitDown],
            vec![
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

/// Grid rows the S1 and S12 status line comparisons request. The macOS runner clamps the window to
/// its screen at 43 rows, so a 70-row request lost the strip's row only on a ref that draws it. At
/// 13 pt the strip is under 1.4 rows, so 40 rows plus the strip fit unclamped on both refs.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const STRIP_SAFE_ROWS: u16 = 40;

/// The name `result.json` gives the short S12 plan's covered-window trim experiment.
#[cfg(any(target_os = "macos", windows, test))]
pub(crate) const TRIM_EXPERIMENT: &str = "s12-short-trim";

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
