//! Per-phase samples, S2 latency attribution, grid scanning and the `result.json` and
//! `progress.json` documents.

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::{json, Map, Value};
use sonicterm_grid::grid::Grid;

use crate::scenarios::{Host, Presentation};

/// The characters S2 types, cycled; each self-inserts at a `zsh -f` prompt.
const TYPED_SYMBOLS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// The character typed at `index`.
pub(crate) fn typed_character(index: u32) -> char {
    char::from(TYPED_SYMBOLS[index as usize % TYPED_SYMBOLS.len()])
}

/// Where one typed character echoes: a scrollback-absolute row, a column and the character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EchoTarget {
    /// Row counted from the oldest retained history row.
    pub(crate) abs_row: u64,
    /// Column of the echoed cell.
    pub(crate) col: u16,
    /// The character that must be in that cell.
    pub(crate) character: char,
}

/// The scrollback-absolute row and the column just past `prompt`, when the cursor sits right
/// after it at the start of the cursor's row; `None` otherwise.
pub(crate) fn prompt_origin(grid: &Grid, prompt: &str) -> Option<(u64, u16)> {
    let prompt_cols = u16::try_from(prompt.chars().count()).ok()?;
    if grid.cursor.col != prompt_cols {
        // When: the cursor is not just past a prompt-sized prefix, typing has not begun here.
        return None;
    }
    let row = grid.row(grid.cursor.row);
    let shows_prompt = prompt
        .chars()
        .enumerate()
        .all(|(index, expected)| row.get(index).is_some_and(|cell| cell.ch == expected));
    if !shows_prompt {
        // When: the row does not start with the prompt, the cursor is after other text.
        return None;
    }
    Some((grid.scrollback_len() as u64 + u64::from(grid.cursor.row), prompt_cols))
}

/// Where typed character `index` echoes when typing starts at `origin` in a grid `cols` wide.
pub(crate) fn echo_target(origin: (u64, u16), cols: u16, index: u32) -> EchoTarget {
    let linear = u64::from(origin.1) + u64::from(index);
    let width = u64::from(cols.max(1));
    EchoTarget {
        abs_row: origin.0 + linear / width,
        col: (linear % width) as u16,
        character: typed_character(index),
    }
}

/// What one nonblocking look at the echo cell found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EchoSnapshot {
    /// The parser lock was busy, so nothing was read.
    Busy,
    /// The grid was read: whether the echo is there, and the grid's revision.
    Read { present: bool, revision: u64 },
}

/// Whether `target`'s character is in `grid`, and the revision it was read under.
pub(crate) fn snapshot_echo(grid: &Grid, target: &EchoTarget) -> EchoSnapshot {
    let present = grid
        .row_at_abs(target.abs_row)
        .and_then(|row| row.get(usize::from(target.col)))
        .is_some_and(|cell| cell.ch == target.character);
    EchoSnapshot::Read { present, revision: grid.revision() }
}

/// One forwarded dispatch while a sample is open: a snapshot on each side, and whether the
/// dispatch advanced `successful_frame_count`, which is known only afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DispatchObservation {
    /// Snapshot taken just before the dispatch.
    pub(crate) before: EchoSnapshot,
    /// Snapshot taken just after the dispatch.
    pub(crate) after: EchoSnapshot,
    /// Whether the dispatch presented a frame.
    pub(crate) advanced: bool,
}

/// How one dispatch settles an open latency sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attribution {
    /// The dispatch decides nothing; the sample stays open.
    Pending,
    /// The dispatch presented the echo; latency runs to its end.
    Credited,
    /// No frame can be credited with the sample.
    Unattributed(UnattributedReason),
}

/// Why a sample was not credited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnattributedReason {
    /// A snapshot of a presenting dispatch found the parser lock busy.
    LockBusy,
    /// The echo first appeared during a presenting dispatch.
    EchoDuringFrame,
    /// The grid changed during the first presenting dispatch that held the echo.
    RevisionChanged,
    /// No presenting dispatch held the echo before the next injection or the phase's end.
    NoCandidate,
}

impl UnattributedReason {
    /// The reason's name in `result.json`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::LockBusy => "lock-busy",
            Self::EchoDuringFrame => "echo-during-frame",
            Self::RevisionChanged => "revision-changed",
            Self::NoCandidate => "no-candidate",
        }
    }
}

/// The reason a credited sample records.
pub(crate) const CREDITED: &str = "credited";

/// Settle an open sample from one dispatch.
///
/// The candidate is the first dispatch that presents with the echo already in the grid before
/// it; it is credited only when the grid's revision is unchanged after it. A busy lock at a
/// presenting dispatch, an echo that first appears during one, or a revision change during the
/// candidate leaves the sample unattributed rather than credited to another frame.
pub(crate) fn attribute_dispatch(observation: &DispatchObservation) -> Attribution {
    if !observation.advanced {
        // When: the dispatch presented nothing, it can neither show nor hide the echo.
        return Attribution::Pending;
    }
    match (observation.before, observation.after) {
        (EchoSnapshot::Busy, _) | (_, EchoSnapshot::Busy) => {
            Attribution::Unattributed(UnattributedReason::LockBusy)
        }
        (
            EchoSnapshot::Read { present: true, revision: before },
            EchoSnapshot::Read { revision: after, .. },
        ) => {
            if before == after {
                Attribution::Credited
            } else {
                // When: output was parsed during the frame, the presented grid is unknown.
                Attribution::Unattributed(UnattributedReason::RevisionChanged)
            }
        }
        (EchoSnapshot::Read { present: false, .. }, EchoSnapshot::Read { present: true, .. }) => {
            Attribution::Unattributed(UnattributedReason::EchoDuringFrame)
        }
        (EchoSnapshot::Read { present: false, .. }, EchoSnapshot::Read { present: false, .. }) => {
            Attribution::Pending
        }
    }
}

/// One S2 sample: when its character was injected, and its latency or why it has none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LatencySample {
    /// Injection time in Unix seconds.
    pub(crate) inject_unix_s: f64,
    /// Injection to the end of the presenting dispatch, in ms; `None` when unattributed.
    pub(crate) latency_ms: Option<f64>,
    /// [`CREDITED`] or an [`UnattributedReason`] name.
    pub(crate) reason: &'static str,
}

impl Serialize for LatencySample {
    fn serialize<Format: Serializer>(
        &self,
        serializer: Format,
    ) -> Result<Format::Ok, Format::Error> {
        // `attributed` is derived from `latency_ms`, so a reader need not infer it.
        let mut fields = serializer.serialize_struct("LatencySample", 4)?;
        fields.serialize_field("inject_unix_s", &self.inject_unix_s)?;
        fields.serialize_field("latency_ms", &self.latency_ms)?;
        fields.serialize_field("attributed", &self.latency_ms.is_some())?;
        fields.serialize_field("reason", self.reason)?;
        fields.end()
    }
}

/// The `latency` object: every sample, the attributed count, the total and the coverage.
///
/// Coverage of no samples is 0.0 rather than null, so a reader that needs a number gets one.
struct LatencyReport<'run>(&'run [LatencySample]);

impl Serialize for LatencyReport<'_> {
    fn serialize<Format: Serializer>(
        &self,
        serializer: Format,
    ) -> Result<Format::Ok, Format::Error> {
        let samples = self.0;
        let attributed = samples.iter().filter(|sample| sample.latency_ms.is_some()).count();
        let coverage =
            if samples.is_empty() { 0.0 } else { attributed as f64 / samples.len() as f64 };
        let mut fields = serializer.serialize_struct("LatencyReport", 4)?;
        fields.serialize_field("samples", samples)?;
        fields.serialize_field("attributed", &attributed)?;
        fields.serialize_field("total", &samples.len())?;
        fields.serialize_field("coverage", &coverage)?;
        fields.end()
    }
}

/// Whether a visible row from the cursor's row up to `rows_above` rows above it starts with `text`.
pub(crate) fn line_near_cursor(grid: &Grid, text: &str, rows_above: u16) -> bool {
    let cursor_row = grid.cursor.row;
    (cursor_row.saturating_sub(rows_above)..=cursor_row).any(|row| {
        let line = grid.row(row);
        text.chars()
            .enumerate()
            .all(|(index, expected)| line.get(index).is_some_and(|cell| cell.ch == expected))
    })
}

/// One measured phase's samples.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct PhaseRecord {
    /// Phase name.
    pub(crate) name: &'static str,
    /// Start, in Unix seconds.
    pub(crate) start_unix_s: f64,
    /// End, in Unix seconds.
    pub(crate) end_unix_s: f64,
    /// Process user CPU during the phase, in seconds.
    pub(crate) cpu_user_s: f64,
    /// Process system CPU during the phase, in seconds.
    pub(crate) cpu_system_s: f64,
    /// Frames the main renderer presented: the advances of its `successful_frame_count`.
    pub(crate) presented_frames: u64,
    /// `RedrawRequested` dispatches to the main window.
    pub(crate) redraw_requested: u64,
    /// Each `RedrawRequested` dispatch's duration, in ms.
    pub(crate) dispatch_ms: Vec<f64>,
    /// Intervals between the ends of consecutive presenting dispatches, in ms.
    pub(crate) present_interval_ms: Vec<f64>,
    /// Allocation calls during each `RedrawRequested` dispatch; `None` without the counting allocator.
    pub(crate) allocations_per_frame: Option<Vec<u64>>,
}

/// One memory checkpoint at the end of a timed phase.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct CheckpointRecord {
    /// Position among the run's checkpoints, from 0; it names `checkpoints/<index>-<label>.*`.
    pub(crate) index: usize,
    /// Label from the plan, `[a-z0-9-]` only.
    pub(crate) label: &'static str,
    /// When the request was written, in Unix seconds.
    pub(crate) unix_s: f64,
    /// `checkpoints/<index>-<label>.json` when it exists once `.done` appears; otherwise `None`.
    pub(crate) footprint_file: Option<String>,
}

/// Bytes a workload wrote from GO to its sentinel, and how long that took.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct Throughput {
    /// Workload output in bytes, before the terminal turns LF into CR LF.
    pub(crate) bytes: u64,
    /// GO to the sentinel seen in the parsed grid, in seconds.
    pub(crate) seconds: f64,
}

/// The measurement window's display, so a comparison can require the same refresh rate.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MonitorInfo {
    /// `MonitorHandle::name`, when winit reports one.
    pub(crate) name: Option<String>,
    /// `MonitorHandle::refresh_rate_millihertz`, when winit reports one.
    pub(crate) refresh_rate_millihertz: Option<u32>,
    /// `Window::scale_factor`.
    pub(crate) scale_factor: f64,
}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Every step completed.
    Valid,
    /// Unexpected input, an unrequested occlusion change or a failed setup voided the run.
    Invalid,
    /// The harness's own deadline expired.
    Timeout,
    /// This build cannot run the scenario.
    Blocked,
}

impl Status {
    /// The process exit code for this status.
    pub(crate) fn exit_code(self) -> u8 {
        match self {
            Self::Valid => 0,
            Self::Invalid => 3,
            Self::Timeout => 4,
            Self::Blocked => 5,
        }
    }

    /// The status's name in `result.json`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Timeout => "timeout",
            Self::Blocked => "blocked",
        }
    }
}

/// Everything `result.json` records about one run, including a partial one.
#[derive(Clone, Debug)]
pub(crate) struct RunResult {
    /// `--harness-hash`, when the comparison script passed one.
    pub(crate) harness_hash: Option<String>,
    /// Scenario id.
    pub(crate) scenario: &'static str,
    /// Variant name.
    pub(crate) variant: &'static str,
    /// Whether the comparison script acknowledges sessions and checkpoints.
    pub(crate) managed: bool,
    /// Whether `--short` shortened the holds.
    pub(crate) short: bool,
    /// Whether the run logged `render_timing` laps at `debug`.
    pub(crate) laps: bool,
    /// Whether the counting allocator ran.
    pub(crate) alloc_counting: bool,
    /// How the run ended.
    pub(crate) status: Status,
    /// Why the run is not valid.
    pub(crate) invalid_reason: Option<String>,
    /// This process's id.
    pub(crate) harness_pid: u32,
    /// The active pane's grid after startup, as `(cols, rows)`.
    pub(crate) grid: Option<(u16, u16)>,
    /// The measurement window's display after startup; `None` when winit reports no monitor.
    pub(crate) monitor: Option<MonitorInfo>,
    /// `production` for the window `do_resumed` builds.
    pub(crate) window_path: &'static str,
    /// Whether an occlusion change was delivered without a native event.
    pub(crate) synthetic_occlusion: bool,
    /// Native `Focused` events the probe recorded and dropped.
    pub(crate) native_focus_events_dropped: u64,
    /// What `App::finish_session` returned: true when every pane's PTY teardown drained within
    /// its bound. `result.json` is written only after that call, so false means it did not drain.
    pub(crate) finish_session_settled: bool,
    /// How the main window presented, recorded at the end of startup; `None` when not recorded.
    pub(crate) presenter: Option<PresenterRecord>,
    /// Every phase that started, the last one possibly cut short.
    pub(crate) phases: Vec<PhaseRecord>,
    /// S2 samples.
    pub(crate) latency: Option<Vec<LatencySample>>,
    /// S3 throughput.
    pub(crate) throughput: Option<Throughput>,
    /// S12: `Occluded(false)` to the end of the first presenting dispatch, in ms.
    pub(crate) uncover_ms: Option<f64>,
    /// S7: history rows the pane kept after its fixture.
    pub(crate) scrollback_rows_retained: Option<u64>,
    /// Memory checkpoints in order.
    pub(crate) checkpoints: Vec<CheckpointRecord>,
    /// Conditions a reader needs to interpret the numbers.
    pub(crate) notes: Vec<String>,
}

impl RunResult {
    /// The `result.json` document, schema version 1.
    pub(crate) fn to_json(&self) -> Value {
        let mut document = Map::new();
        let mut put = |key: &str, value: Value| {
            document.insert(key.to_owned(), value);
        };
        put("schema_version", json!(SCHEMA_VERSION));
        put("harness_hash", json!(self.harness_hash));
        put("scenario", json!(self.scenario));
        put("variant", json!(self.variant));
        put("managed", json!(self.managed));
        put("short", json!(self.short));
        put("laps", json!(self.laps));
        put("alloc_counting", json!(self.alloc_counting));
        put("status", json!(self.status.as_str()));
        put("invalid_reason", json!(self.invalid_reason));
        put("exit_code", json!(self.status.exit_code()));
        put("harness_pid", json!(self.harness_pid));
        put(
            "grid",
            self.grid.map_or(Value::Null, |(cols, rows)| json!({"cols": cols, "rows": rows})),
        );
        put(
            "monitor",
            self.monitor.as_ref().map_or(Value::Null, |monitor| {
                json!({
                    "name": monitor.name,
                    "refresh_rate_millihertz": monitor.refresh_rate_millihertz,
                    "scale_factor": monitor.scale_factor,
                })
            }),
        );
        put("presenter", json!(self.presenter));
        put("window_path", json!(self.window_path));
        put("synthetic_occlusion", json!(self.synthetic_occlusion));
        put("native_focus_events_dropped", json!(self.native_focus_events_dropped));
        put("finish_session_settled", json!(self.finish_session_settled));
        // The measurement fields come from the serializer progress.json streams, so both
        // documents record them identically. Every field converts; a non-finite float is null.
        let measured =
            serde_json::to_value(self.measurements()).expect("measurements always convert to JSON");
        if let Value::Object(fields) = measured {
            for (key, value) in fields {
                put(key.as_str(), value);
            }
        }
        put("notes", json!(self.notes));
        Value::Object(document)
    }
}

/// `result.json`'s `presenter`: how the run's main window presented.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct PresenterRecord {
    /// The configured `[appearance].software_render_mode`, as the config file spells it.
    pub(crate) software_render_mode: &'static str,
    /// Whether wgpu chose a CPU rasterizer.
    pub(crate) software_rendering: bool,
    /// Whether the software degrade path is active once the mode is applied.
    pub(crate) software_render_degraded: bool,
    /// Whether the window presents through Windows GDI: the degrade path on Windows.
    pub(crate) windows_gdi: bool,
}

/// Seconds in a FILETIME duration given as its two 32-bit words; it counts 100 ns ticks.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn filetime_seconds(low: u32, high: u32) -> f64 {
    ((u64::from(high) << 32) | u64::from(low)) as f64 / 1e7
}

/// Why a Windows run cannot measure its presenter variant, naming the variant and the field that
/// missed it: `gdi` without Windows GDI, or `wgpu` on the degrade path. None when it can.
pub(crate) fn presenter_blocked(
    presentation: Presentation,
    presenter: &PresenterRecord,
    host: Host,
) -> Option<String> {
    if host != Host::Windows {
        // When: off Windows the presenter variants are refused before the run, so nothing is judged.
        return None;
    }
    match presentation {
        Presentation::ForceGdi if !presenter.windows_gdi => Some(format!(
            "the gdi variant forced the software presenter, but presenter.windows_gdi is false \
             (software_render_degraded {}), so the run did not present through Windows GDI",
            presenter.software_render_degraded
        )),
        Presentation::ForceWgpu if presenter.software_render_degraded => Some(format!(
            "the wgpu variant turned the software presenter off, but presenter.software_render_degraded \
             is true (windows_gdi {}), so the run did not present through wgpu",
            presenter.windows_gdi
        )),
        _ => None,
    }
}

/// The schema version of `result.json` and `progress.json`.
const SCHEMA_VERSION: u32 = 1;

/// Every measurement a run has completed so far, borrowed from where the run keeps it.
/// `progress.json` and `result.json` both record exactly these fields.
#[derive(Clone, Copy)]
pub(crate) struct Measurements<'run> {
    /// Completed phases; in `result.json` the last one may be cut short.
    pub(crate) phases: &'run [PhaseRecord],
    /// S2 samples; `None` when the scenario types nothing.
    pub(crate) latency: Option<&'run [LatencySample]>,
    /// S3 throughput, once its phase has ended.
    pub(crate) throughput: Option<Throughput>,
    /// S12's uncover time, once measured.
    pub(crate) uncover_ms: Option<f64>,
    /// S7's retained history rows, once read.
    pub(crate) scrollback_rows_retained: Option<u64>,
    /// Memory checkpoints so far.
    pub(crate) checkpoints: &'run [CheckpointRecord],
}

impl RunResult {
    /// This result's measurements: the fields `progress.json` also records.
    fn measurements(&self) -> Measurements<'_> {
        Measurements {
            phases: &self.phases,
            latency: self.latency.as_deref(),
            throughput: self.throughput,
            uncover_ms: self.uncover_ms,
            scrollback_rows_retained: self.scrollback_rows_retained,
            checkpoints: &self.checkpoints,
        }
    }
}

// Field order follows result.json, so both documents lay the fields out alike.
impl Serialize for Measurements<'_> {
    fn serialize<Format: Serializer>(
        &self,
        serializer: Format,
    ) -> Result<Format::Ok, Format::Error> {
        let mut fields = serializer.serialize_struct("Measurements", 6)?;
        fields.serialize_field("phases", self.phases)?;
        fields.serialize_field("latency", &self.latency.map(LatencyReport))?;
        fields.serialize_field("throughput", &self.throughput)?;
        fields.serialize_field("uncover_ms", &self.uncover_ms)?;
        fields.serialize_field("scrollback_rows_retained", &self.scrollback_rows_retained)?;
        fields.serialize_field("checkpoints", self.checkpoints)?;
        fields.end()
    }
}

/// `progress.json`: what a run had measured when its latest phase ended.
#[derive(Serialize)]
struct Progress<'run> {
    schema_version: u32,
    harness_hash: Option<&'run str>,
    status: &'static str,
    /// Every measurement field, at the top level as in `result.json`.
    #[serde(flatten)]
    measured: Measurements<'run>,
}

/// Stream `progress.json`: the schema version, the harness hash, status `running` and every
/// measurement completed so far, each field in `result.json`'s shape. Serializing straight into
/// `writer` allocates no document-sized buffer between phases.
pub(crate) fn write_progress(
    writer: impl std::io::Write,
    harness_hash: Option<&str>,
    measured: Measurements<'_>,
) -> std::io::Result<()> {
    let progress =
        Progress { schema_version: SCHEMA_VERSION, harness_hash, status: "running", measured };
    serde_json::to_writer_pretty(writer, &progress)?;
    Ok(())
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod record_tests;
