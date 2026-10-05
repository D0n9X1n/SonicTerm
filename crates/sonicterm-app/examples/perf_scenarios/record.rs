//! Per-phase samples, S2 latency attribution, grid scanning and the `result.json` and
//! `progress.json` documents.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::{json, Map, Value};
use sonicterm_grid::grid::Grid;
use sonicterm_ui::selection::plain_text_from_grid_range;

use crate::scenarios::{Host, Presentation};

use crate::counters::{CounterTotals, CountersMode};

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

/// The grid state a scrollback-absolute row index is valid under: rows evicted, screen
/// incarnation and resize generation. Any change means the index may name another row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowIdentity {
    /// Rows the grid has evicted from history.
    pub(crate) scrollback_evicted: u64,
    /// The primary or alternate screen's incarnation.
    pub(crate) screen_epoch: u64,
    /// Resizes that changed the grid's size.
    pub(crate) size_generation: u64,
}

/// The identity `grid` holds now; the harness reads it with the prompt origin, under one guard.
pub(crate) fn prompt_identity(grid: &Grid) -> RowIdentity {
    RowIdentity {
        scrollback_evicted: grid.scrollback_evicted(),
        screen_epoch: grid.screen_epoch(),
        size_generation: grid.size_generation(),
    }
}

/// What one nonblocking look at the echo cell found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EchoSnapshot {
    /// The parser lock was busy, so nothing was read.
    Busy,
    /// The grid was read: whether the echo is there, the grid's revision and its row identity.
    Read { present: bool, revision: u64, identity: RowIdentity },
}

/// Whether `target`'s character is in `grid`, and the revision and identity it was read under.
pub(crate) fn snapshot_echo(grid: &Grid, target: &EchoTarget) -> EchoSnapshot {
    let present = grid
        .row_at_abs(target.abs_row)
        .and_then(|row| row.get(usize::from(target.col)))
        .is_some_and(|cell| cell.ch == target.character);
    EchoSnapshot::Read { present, revision: grid.revision(), identity: prompt_identity(grid) }
}

/// Whether either snapshot of a dispatch read a row identity other than `armed`; the harness's own
/// check, which catches a change the worker never saw (an event-loop resize, for one).
pub(crate) fn snapshot_identity_changed(
    observation: &DispatchObservation,
    armed: RowIdentity,
) -> bool {
    [observation.before, observation.after].iter().any(
        |snapshot| matches!(snapshot, EchoSnapshot::Read { identity, .. } if *identity != armed),
    )
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
            EchoSnapshot::Read { present: true, revision: before, .. },
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

/// Whether a sample of `scenario`/`variant` is split at the flush: only S2's single idle pane.
/// S2/flood types into a pane beside a flood, so its samples are never armed.
pub(crate) fn split_in_scope(scenario: &str, variant: &str) -> bool {
    (scenario, variant) == ("S2", "default")
}

/// The schema of the split fields in `latency`; a harness that writes them lists the same number
/// in `--list` as `capabilities.latency_split_schema`.
pub(crate) const SPLIT_SCHEMA: u32 = 1;
/// The split reason of a sample with no credited frame.
pub(crate) const NOT_CREDITED: &str = "not-credited";
/// The split reason of a credited sample that was split.
pub(crate) const SPLIT: &str = "split";
/// Every split reason of a credited sample, in precedence order: the first that applies wins.
pub(crate) const SPLIT_REASONS: [&str; 22] = [
    "unsupported",
    "arm-gate-off",
    "arm-no-pane",
    "arm-exhausted",
    "take-gate-off",
    "take-no-pane",
    "take-mismatch",
    "take-already-taken",
    "pane-not-shown",
    "row-identity-changed",
    "first-appearance-unobserved",
    "no-appearance-observed",
    "echo-overwritten",
    "no-publication-observed-by-take",
    "untargeted",
    "other-window",
    "send-refused",
    "delivery-not-observed-by-take",
    "presented-before-publication",
    "delivered-after-present",
    "clock-order",
    SPLIT,
];

/// How an open sample's echo watch was armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArmState {
    /// The sample is outside the split's scope (not S2/default); nothing was armed.
    OutOfScope,
    /// This build has no `perf-echo-trace`; nothing was armed.
    // Built only by the fallback adapter, which exists only without perf-echo-trace.
    #[cfg_attr(feature = "perf-echo-trace", allow(dead_code))]
    Unsupported,
    // Built only by the echo_api adapter, which exists only with perf-echo-trace.
    #[cfg_attr(not(feature = "perf-echo-trace"), allow(dead_code))]
    /// The App refused the arm; the reason's name.
    Failed(&'static str),
    // Built only by the echo_api adapter, which exists only with perf-echo-trace.
    #[cfg_attr(not(feature = "perf-echo-trace"), allow(dead_code))]
    /// The watch is armed with this token.
    Armed(EchoToken),
}

/// What the echo watch said about a credited sample, as `split_for` reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EchoOutcome {
    /// No watch: no feature, or out of scope.
    Unsupported,
    /// The arm or the take failed; the reason's name.
    Failed(&'static str),
    // Built only by the echo_api adapter, which exists only with perf-echo-trace.
    #[cfg_attr(not(feature = "perf-echo-trace"), allow(dead_code))]
    /// The taken record.
    Taken(TraceFacts),
}

/// The outcome for a credited sample armed as `arm`: `take` runs only for an armed token.
pub(crate) fn echo_outcome(
    arm: ArmState,
    take: impl FnOnce(EchoToken) -> EchoOutcome,
) -> EchoOutcome {
    match arm {
        ArmState::OutOfScope | ArmState::Unsupported => EchoOutcome::Unsupported,
        ArmState::Failed(reason) => EchoOutcome::Failed(reason),
        ArmState::Armed(token) => take(token),
    }
}

/// A taken echo-watch record, in the harness's own types so the split builds without the feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TraceFacts {
    /// The pane was in the main window's active tab at the take.
    pub(crate) shown: bool,
    /// The worker read an identity other than the armed one.
    pub(crate) identity_changed: bool,
    /// The worker found the echo already present before any appearance.
    pub(crate) pre_present: bool,
    /// The worker found the echo absent again after its appearance.
    pub(crate) lost: bool,
    /// The first absent-to-present section.
    pub(crate) appearance: Option<AppearanceFacts>,
    /// The first flush published after it.
    pub(crate) publication: Option<PublicationFacts>,
    /// That flush's token decision.
    pub(crate) delivery: Option<DeliveryFacts>,
}

/// Where the echo first appeared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AppearanceFacts {
    /// The output generation of the appearance's batch.
    pub(crate) generation: u64,
    /// When that section finished parsing.
    pub(crate) parsed_at: Instant,
    /// Whether a synchronized update was set after it.
    pub(crate) sync_open: bool,
}

/// The flush that published the echo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PublicationFacts {
    /// When the watch recorded the publication, before the token decision.
    pub(crate) published_at: Instant,
    /// Which window the flush targeted.
    pub(crate) window: PublicationWindow,
    /// Whether an earlier flush was still pending.
    pub(crate) coalesced: bool,
}

/// The publication's redraw target, relative to the measurement window.
// Built only by the echo_api adapter, which exists only with perf-echo-trace.
#[cfg_attr(not(feature = "perf-echo-trace"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicationWindow {
    /// The pane had no redraw target.
    Untargeted,
    /// The measurement window.
    Main,
    /// Some other window.
    Other,
}

/// The token decision for the published flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeliveryFacts {
    /// What the worker decided.
    pub(crate) outcome: Delivery,
    /// When it recorded the decision.
    pub(crate) decided_at: Instant,
}

/// The worker's decision for one output event.
// Built only by the echo_api adapter, which exists only with perf-echo-trace.
#[cfg_attr(not(feature = "perf-echo-trace"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Sent to the event loop.
    Sent,
    /// An earlier event was outstanding; its service reads this flush.
    Suppressed,
    /// The event loop refused it.
    Refused,
}

/// A credited sample's latency split at the flush publication, each part in whole nanoseconds.
/// The three parts telescope exactly to `ended - injected`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Split {
    /// Injection to the end of the section where the echo appeared.
    pub(crate) input_to_parse_ns: u64,
    /// That section's end to the flush's publication.
    pub(crate) parse_to_publication_ns: u64,
    /// The publication to the end of the credited dispatch.
    pub(crate) publication_to_present_ns: u64,
    /// The publication to the worker recording its token decision; not event-loop latency.
    pub(crate) delivery_lag_ns: u64,
    /// `Sent` or `Suppressed`; a refused send is never split.
    pub(crate) delivery: Delivery,
    /// Whether the publication coalesced with an earlier pending flush.
    pub(crate) coalesced: bool,
    /// Whether a synchronized update was set when the echo appeared.
    pub(crate) sync_open: bool,
    /// The output generation of the appearance's batch.
    pub(crate) echo_generation: u64,
}

impl Serialize for Split {
    fn serialize<Format: Serializer>(
        &self,
        serializer: Format,
    ) -> Result<Format::Ok, Format::Error> {
        let delivery = match self.delivery {
            Delivery::Suppressed => "suppressed",
            Delivery::Sent | Delivery::Refused => "sent",
        };
        let mut fields = serializer.serialize_struct("Split", 8)?;
        fields.serialize_field("input_to_parse_ms", &ns_to_ms(self.input_to_parse_ns))?;
        fields
            .serialize_field("parse_to_publication_ms", &ns_to_ms(self.parse_to_publication_ns))?;
        fields.serialize_field(
            "publication_to_present_ms",
            &ns_to_ms(self.publication_to_present_ns),
        )?;
        fields.serialize_field("delivery_lag_us", &(self.delivery_lag_ns as f64 / 1_000.0))?;
        fields.serialize_field("delivery", delivery)?;
        fields.serialize_field("coalesced", &self.coalesced)?;
        fields.serialize_field("sync_open", &self.sync_open)?;
        fields.serialize_field("echo_generation", &self.echo_generation)?;
        fields.end()
    }
}

/// Nanoseconds as milliseconds.
fn ns_to_ms(nanoseconds: u64) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

/// A duration in whole nanoseconds, or `None` when it does not fit in a `u64`.
pub(crate) fn duration_ns(duration: Duration) -> Option<u64> {
    u64::try_from(duration.as_nanos()).ok()
}

/// Split a sample injected at `injected` and credited to a dispatch that ended at `ended`, or name
/// the first reason in `SPLIT_REASONS` order that it cannot be. `harness_identity_changed` is the
/// harness's own snapshot check of the armed row identity.
pub(crate) fn split_for(
    injected: Instant,
    ended: Instant,
    outcome: &EchoOutcome,
    harness_identity_changed: bool,
) -> Result<Split, &'static str> {
    let facts = match outcome {
        EchoOutcome::Unsupported => return Err("unsupported"),
        EchoOutcome::Failed(reason) => return Err(reason),
        EchoOutcome::Taken(facts) => facts,
    };
    if !facts.shown {
        return Err("pane-not-shown");
    }
    if facts.identity_changed || harness_identity_changed {
        return Err("row-identity-changed");
    }
    if facts.pre_present {
        return Err("first-appearance-unobserved");
    }
    let Some(appearance) = facts.appearance else {
        return Err("no-appearance-observed");
    };
    if facts.lost {
        return Err("echo-overwritten");
    }
    let Some(publication) = facts.publication else {
        return Err("no-publication-observed-by-take");
    };
    match publication.window {
        PublicationWindow::Untargeted => return Err("untargeted"),
        PublicationWindow::Other => return Err("other-window"),
        PublicationWindow::Main => {}
    }
    let delivery = match facts.delivery {
        None => return Err("delivery-not-observed-by-take"),
        Some(DeliveryFacts { outcome: Delivery::Refused, .. }) => return Err("send-refused"),
        Some(delivery) => delivery,
    };
    if ended < publication.published_at {
        return Err("presented-before-publication");
    }
    if delivery.decided_at > ended {
        return Err("delivered-after-present");
    }
    // Checked intervals only: an out-of-order or unrepresentable endpoint is never saturated.
    let part = |from: Instant, to: Instant| to.checked_duration_since(from).and_then(duration_ns);
    let parts = (
        part(injected, appearance.parsed_at),
        part(appearance.parsed_at, publication.published_at),
        part(publication.published_at, ended),
        part(publication.published_at, delivery.decided_at),
    );
    let (
        Some(input_to_parse_ns),
        Some(parse_to_publication_ns),
        Some(publication_to_present_ns),
        Some(delivery_lag_ns),
    ) = parts
    else {
        return Err("clock-order");
    };
    Ok(Split {
        input_to_parse_ns,
        parse_to_publication_ns,
        publication_to_present_ns,
        delivery_lag_ns,
        delivery: delivery.outcome,
        coalesced: publication.coalesced,
        sync_open: appearance.sync_open,
        echo_generation: appearance.generation,
    })
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
    /// The split at the flush; only for split reason [`SPLIT`].
    pub(crate) split: Option<Split>,
    /// [`NOT_CREDITED`] for an uncredited sample, else one of [`SPLIT_REASONS`].
    pub(crate) split_reason: &'static str,
}

impl LatencySample {
    /// A sample no frame was credited with, for `reason`.
    pub(crate) fn uncredited(inject_unix_s: f64, reason: &'static str) -> Self {
        Self { inject_unix_s, latency_ms: None, reason, split: None, split_reason: NOT_CREDITED }
    }

    /// A sample injected at `injected` and credited to the dispatch that ended at `ended`, split
    /// from the echo watch's `outcome` when it can be.
    pub(crate) fn credited(
        inject_unix_s: f64,
        injected: Instant,
        ended: Instant,
        outcome: &EchoOutcome,
        harness_identity_changed: bool,
    ) -> Self {
        let latency_ms = ended.saturating_duration_since(injected).as_secs_f64() * 1_000.0;
        let (split, split_reason) =
            match split_for(injected, ended, outcome, harness_identity_changed) {
                Ok(split) => (Some(split), SPLIT),
                Err(reason) => (None, reason),
            };
        debug_assert!(
            SPLIT_REASONS.contains(&split_reason),
            "{split_reason} is not a split reason"
        );
        Self { inject_unix_s, latency_ms: Some(latency_ms), reason: CREDITED, split, split_reason }
    }
}

impl Serialize for LatencySample {
    fn serialize<Format: Serializer>(
        &self,
        serializer: Format,
    ) -> Result<Format::Ok, Format::Error> {
        // `attributed` is derived from `latency_ms`, so a reader need not infer it.
        let mut fields = serializer.serialize_struct("LatencySample", 6)?;
        fields.serialize_field("inject_unix_s", &self.inject_unix_s)?;
        fields.serialize_field("latency_ms", &self.latency_ms)?;
        fields.serialize_field("attributed", &self.latency_ms.is_some())?;
        fields.serialize_field("reason", self.reason)?;
        fields.serialize_field("split", &self.split)?;
        fields.serialize_field("split_reason", self.split_reason)?;
        fields.end()
    }
}

/// The `latency` object: every sample, the attributed count, the total and the coverage, and the
/// split schema with its count, its reasons over credited samples and its coverage.
///
/// Coverage of no samples is 0.0 rather than null, so a reader that needs a number gets one. Split
/// coverage is null when nothing was credited, which reads as unavailable.
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
        let mut reasons = BTreeMap::new();
        for sample in samples.iter().filter(|sample| sample.latency_ms.is_some()) {
            *reasons.entry(sample.split_reason).or_insert(0_usize) += 1;
        }
        let split_count = reasons.get(SPLIT).copied().unwrap_or(0);
        let split_coverage = (attributed > 0).then(|| split_count as f64 / attributed as f64);
        let mut fields = serializer.serialize_struct("LatencyReport", 8)?;
        fields.serialize_field("samples", samples)?;
        fields.serialize_field("attributed", &attributed)?;
        fields.serialize_field("total", &samples.len())?;
        fields.serialize_field("coverage", &coverage)?;
        fields.serialize_field("split_schema", &SPLIT_SCHEMA)?;
        fields.serialize_field("split_count", &split_count)?;
        fields.serialize_field("split_reasons", &reasons)?;
        fields.serialize_field("split_coverage", &split_coverage)?;
        fields.end()
    }
}

/// The App's echo-watch API, in a tree that declares `perf-echo-trace`.
#[cfg(feature = "perf-echo-trace")]
pub(crate) mod echo_api {
    use sonicterm_app::app::{
        App, ArmOutcome, ArmToken, EchoDeliveryOutcome, EchoRowIdentity, EchoTrace,
        EchoWatchTarget, TakeOutcome,
    };
    use winit::window::WindowId;

    use super::{
        AppearanceFacts, ArmState, Delivery, DeliveryFacts, EchoOutcome, EchoTarget,
        PublicationFacts, PublicationWindow, RowIdentity, TraceFacts,
    };

    /// The App's arm token.
    pub(crate) type EchoToken = ArmToken;

    /// Arm `pane`'s watch for `target` under `identity`; no parser lock is taken.
    pub(crate) fn arm(
        app: &mut App,
        pane: u64,
        target: &EchoTarget,
        identity: RowIdentity,
    ) -> ArmState {
        let identity = EchoRowIdentity {
            scrollback_evicted: identity.scrollback_evicted,
            screen_epoch: identity.screen_epoch,
            size_generation: identity.size_generation,
        };
        let watch = EchoWatchTarget {
            abs_row: target.abs_row,
            col: target.col,
            character: target.character,
            identity,
        };
        match app.arm_echo_watch(pane, watch) {
            ArmOutcome::Armed(token) => ArmState::Armed(token),
            ArmOutcome::GateOff => ArmState::Failed("arm-gate-off"),
            ArmOutcome::NoPane => ArmState::Failed("arm-no-pane"),
            ArmOutcome::Exhausted => ArmState::Failed("arm-exhausted"),
        }
    }

    /// Take `pane`'s record for `token`, reading publications against the measurement window `main`.
    pub(crate) fn take(
        app: &mut App,
        pane: u64,
        token: EchoToken,
        main: Option<WindowId>,
    ) -> EchoOutcome {
        match app.take_echo_watch(pane, token) {
            TakeOutcome::Trace(trace) => EchoOutcome::Taken(facts_of(&trace, main)),
            TakeOutcome::GateOff => EchoOutcome::Failed("take-gate-off"),
            TakeOutcome::NoPane => EchoOutcome::Failed("take-no-pane"),
            TakeOutcome::Mismatch => EchoOutcome::Failed("take-mismatch"),
            TakeOutcome::AlreadyTaken => EchoOutcome::Failed("take-already-taken"),
        }
    }

    /// Disarm `pane`'s watch for `token`, discarding its record.
    pub(crate) fn discard(app: &mut App, pane: u64, token: EchoToken) {
        // The record of an uncredited sample is not used; the take only disarms the watch.
        let _ = app.take_echo_watch(pane, token);
    }

    /// The App's record in the harness's types.
    fn facts_of(trace: &EchoTrace, main: Option<WindowId>) -> TraceFacts {
        TraceFacts {
            shown: trace.shown,
            identity_changed: trace.identity_changed,
            pre_present: trace.pre_present,
            lost: trace.lost,
            appearance: trace.appearance.map(|appearance| AppearanceFacts {
                generation: appearance.generation,
                parsed_at: appearance.parsed_at,
                sync_open: appearance.sync_open,
            }),
            publication: trace.publication.map(|publication| PublicationFacts {
                published_at: publication.published_at,
                window: match publication.window {
                    None => PublicationWindow::Untargeted,
                    Some(window) if Some(window) == main => PublicationWindow::Main,
                    Some(_) => PublicationWindow::Other,
                },
                coalesced: publication.coalesced,
            }),
            delivery: trace.delivery.map(|delivery| DeliveryFacts {
                outcome: match delivery.outcome {
                    EchoDeliveryOutcome::Sent => Delivery::Sent,
                    EchoDeliveryOutcome::Suppressed => Delivery::Suppressed,
                    EchoDeliveryOutcome::Refused => Delivery::Refused,
                },
                decided_at: delivery.decided_at,
            }),
        }
    }
}

/// Without `perf-echo-trace` there is no watch: no token exists, and no sample is ever armed.
#[cfg(not(feature = "perf-echo-trace"))]
pub(crate) mod echo_api {
    use sonicterm_app::app::App;
    use winit::window::WindowId;

    use super::{ArmState, EchoOutcome, EchoTarget, RowIdentity};

    /// No token can exist in this build.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum EchoToken {}

    /// Nothing to arm: every sample reads `unsupported`.
    pub(crate) fn arm(
        _app: &mut App,
        _pane: u64,
        _target: &EchoTarget,
        _identity: RowIdentity,
    ) -> ArmState {
        ArmState::Unsupported
    }

    /// Unreachable: no token exists.
    pub(crate) fn take(
        _app: &mut App,
        _pane: u64,
        token: EchoToken,
        _main: Option<WindowId>,
    ) -> EchoOutcome {
        match token {}
    }

    /// Unreachable: no token exists.
    pub(crate) fn discard(_app: &mut App, _pane: u64, token: EchoToken) {
        match token {}
    }
}

pub(crate) use echo_api::EchoToken;

/// Whether a visible row from the cursor's row up to `rows_above` rows above it starts with `text`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn line_near_cursor(grid: &Grid, text: &str, rows_above: u16) -> bool {
    line_row_near_cursor(grid, text, rows_above).is_some()
}

/// The rows lines of these display `widths` fill in a grid `cols` wide: a line wraps onto
/// ceil(width / cols) rows, and an empty line or one exactly `cols` wide takes one, because the
/// terminal defers the wrap until another character arrives.
pub(crate) fn planned_rows(widths: impl IntoIterator<Item = usize>, cols: u16) -> u64 {
    let cols = usize::from(cols.max(1));
    widths.into_iter().map(|width| width.div_ceil(cols).max(1) as u64).sum()
}

/// The `result.json` note for a failed LockSetForegroundWindow, naming its `error`. The run goes on,
/// and the comparison's foreground rule still judges whether the window took the foreground.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn foreground_lock_note(error: &str) -> String {
    format!(
        "LockSetForegroundWindow failed: {error}; the window may take the foreground as it opens, \
         and the comparison's foreground rule still judges the run"
    )
}

/// Rows above the cursor scanned for a READY line or a sentinel. zsh's prompt adds one row; on
/// Windows cmd.exe prints its two-line banner and a blank line before its first prompt, so the scan
/// reaches 8 rows. The sentinel carries the run's nonce and READY is all a role prints before GO,
/// so the wider scan matches no other line.
pub(crate) fn protocol_rows(host: Host) -> u16 {
    match host {
        Host::Posix => 3,
        Host::Windows => 8,
    }
}

/// The lifetime-absolute number of the first visible row, from `rows_above` rows above the
/// cursor's row down to it, that starts with `text`: rows the grid ever evicted, plus its
/// scrollback, plus the visible row. The number never changes once the row is printed.
pub(crate) fn line_row_near_cursor(grid: &Grid, text: &str, rows_above: u16) -> Option<u64> {
    let cursor_row = grid.cursor.row;
    let found = (cursor_row.saturating_sub(rows_above)..=cursor_row).find(|row| {
        let line = grid.row(*row);
        text.chars()
            .enumerate()
            .all(|(index, expected)| line.get(index).is_some_and(|cell| cell.ch == expected))
    })?;
    Some(grid.scrollback_evicted() + grid.scrollback_len() as u64 + u64::from(found))
}

/// Why the rows strictly between the READY row and the sentinel's row are not the `planned_rows`
/// the workload's lines fill at the pane's width, naming both counts; `None` when they match.
pub(crate) fn row_count_mismatch(
    ready_row: u64,
    sentinel_row: u64,
    planned_rows: u64,
) -> Option<String> {
    let delivered = sentinel_row.saturating_sub(ready_row).saturating_sub(1);
    (delivered != planned_rows).then(|| {
        format!(
            "{delivered} rows lie between READY (row {ready_row}) and the sentinel (row {sentinel_row}), \
             but the workload's lines fill {planned_rows} rows"
        )
    })
}

/// The lifetime-absolute row where the logical line ending on `last` starts, walking up across
/// soft wraps; `None` when any of its rows has left the retained history.
fn logical_line_start(grid: &Grid, last: u64) -> Option<u64> {
    let evicted = grid.scrollback_evicted();
    let mut first = last;
    loop {
        let row = grid.row_at_abs(first.checked_sub(evicted)?)?;
        if !row.soft_wrapped_from_previous() {
            // When: this row starts its line, the walk ends here.
            return Some(first);
        }
        first = first.checked_sub(1)?;
    }
}

/// The logical line on lifetime-absolute rows `first..=last`, joined across soft wraps as a copy
/// joins them, trailing blanks trimmed.
fn logical_line_text(grid: &Grid, first: u64, last: u64) -> String {
    let evicted = grid.scrollback_evicted();
    let end = usize::from(grid.cols).saturating_sub(1);
    let range = ((0, first.saturating_sub(evicted)), (end, last.saturating_sub(evicted)));
    plain_text_from_grid_range(grid, range.0, range.1).trim_end().to_owned()
}

/// Why the retained lines above the sentinel's row are not the end of `tail`, the last lines of the
/// workload's fixture, naming the first line that differs; `None` when every retained line matches.
/// A line that wrapped is read whole, so the check holds at any grid width.
pub(crate) fn bulk_tail_mismatch(grid: &Grid, sentinel_row: u64, tail: &[&str]) -> Option<String> {
    // The first row of the line checked last; the walk starts at the sentinel's own row.
    let mut below = sentinel_row;
    for (offset, expected) in tail.iter().rev().enumerate() {
        // A line whose rows have left the retained history ends the check: every line before matched.
        let last = below.checked_sub(1)?;
        let first = logical_line_start(grid, last)?;
        let actual = logical_line_text(grid, first, last);
        if actual != expected.trim_end() {
            return Some(format!(
                "the line on rows {first} to {last}, {} above the sentinel, reads {actual:?}, not the fixture's {expected:?}",
                offset + 1
            ));
        }
        below = first;
    }
    None
}

/// Every retained row, scrollback first, as the selection copy path reads them.
pub(crate) fn retained_text(grid: &Grid) -> String {
    let last_row = grid.scrollback_len() as u64 + u64::from(grid.rows).saturating_sub(1);
    let end = usize::from(grid.cols).saturating_sub(1);
    plain_text_from_grid_range(grid, (0, 0), (end, last_row))
}

/// The distinct whitespace-separated tokens of `text` that hold a non-ASCII character, in order.
pub(crate) fn wide_tokens(text: &str) -> Vec<&str> {
    let mut tokens: Vec<&str> = Vec::new();
    for token in text.split_whitespace() {
        if !token.is_ascii() && !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

/// The `tokens` that `text` does not contain, in order.
pub(crate) fn missing_wide_tokens<'token>(text: &str, tokens: &[&'token str]) -> Vec<&'token str> {
    tokens.iter().copied().filter(|token| !text.contains(token)).collect()
}

/// The longest dispatches a phase records with their times.
pub(crate) const SLOW_DISPATCH_LIMIT: usize = 64;

/// One `RedrawRequested` dispatch with its wall-clock span, so a log stamp can be matched to it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct SlowDispatch {
    /// Start, in Unix seconds.
    pub(crate) start_unix_s: f64,
    /// End, in Unix seconds.
    pub(crate) end_unix_s: f64,
    /// Duration, in ms; the JSON key is `ms`, which the comparison reads.
    #[serde(rename = "ms")]
    pub(crate) duration_ms: f64,
}

/// A dispatch ordered by its duration alone, so the heap's top is the shortest kept.
#[derive(Clone, Copy, Debug)]
struct ByDuration(SlowDispatch);

impl PartialEq for ByDuration {
    fn eq(&self, other: &Self) -> bool {
        self.0.duration_ms.total_cmp(&other.0.duration_ms).is_eq()
    }
}

impl Eq for ByDuration {}

impl PartialOrd for ByDuration {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ByDuration {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.duration_ms.total_cmp(&other.0.duration_ms)
    }
}

/// The SLOW_DISPATCH_LIMIT longest dispatches of a phase, kept in a bounded min-heap, and how many were offered.
#[derive(Debug, Default)]
pub(crate) struct SlowDispatches {
    shortest_first: std::collections::BinaryHeap<std::cmp::Reverse<ByDuration>>,
    count: u64,
}

impl SlowDispatches {
    /// Offer one dispatch: it is kept while the heap has room or when it outlasts the shortest one kept.
    pub(crate) fn push(&mut self, dispatch: SlowDispatch) {
        self.count += 1;
        if self.shortest_first.len() < SLOW_DISPATCH_LIMIT {
            self.shortest_first.push(std::cmp::Reverse(ByDuration(dispatch)));
        } else if self
            .shortest_first
            .peek()
            .is_some_and(|shortest| dispatch.duration_ms > shortest.0 .0.duration_ms)
        {
            // When: the heap is full and this dispatch outlasts its shortest, that one makes room.
            self.shortest_first.pop();
            self.shortest_first.push(std::cmp::Reverse(ByDuration(dispatch)));
        }
    }

    /// The kept dispatches, longest first, and the number offered.
    pub(crate) fn finish(self) -> (Vec<SlowDispatch>, u64) {
        let mut kept: Vec<SlowDispatch> =
            self.shortest_first.into_iter().map(|entry| entry.0 .0).collect();
        kept.sort_by(|left, right| right.duration_ms.total_cmp(&left.duration_ms));
        (kept, self.count)
    }
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
    /// The phase's SLOW_DISPATCH_LIMIT longest dispatches with their spans, longest first.
    pub(crate) slow_dispatches: Vec<SlowDispatch>,
    /// Every `RedrawRequested` dispatch of the phase, whether or not it was kept as slow.
    pub(crate) dispatch_count: u64,
    /// Intervals between the ends of consecutive presenting dispatches, in ms.
    pub(crate) present_interval_ms: Vec<f64>,
    /// Allocation calls during each `RedrawRequested` dispatch; `None` without the counting allocator.
    pub(crate) allocations_per_frame: Option<Vec<u64>>,
    /// The phase's frame and lock counter delta; present only when the run counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frame_counters: Option<CounterTotals>,
    /// The logical updates the selected workload played in this phase; present only for a phase
    /// that plays counted updates (S10's `stream`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) updates: Option<u32>,
}

/// One memory checkpoint at the end of a timed phase.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct CheckpointRecord {
    /// Position among the run's checkpoints, from 0; it names `checkpoints/<index>-<label>.*`.
    pub(crate) index: usize,
    /// Label from the plan, `[a-z0-9-]` only.
    pub(crate) label: &'static str,
    /// When the request was written, in Unix seconds.
    pub(crate) unix_s: f64,
    /// `checkpoints/<index>-<label>.json` when it exists once `.done` appears; otherwise `None`.
    pub(crate) footprint_file: Option<String>,
    /// The Unix time from which a memory sample reflects this checkpoint; only S11/release's
    /// `released` has one, 30 s after its media-free frame. Absent elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fresh_after_unix_s: Option<f64>,
    /// The main renderer's frame texture, width x height x 4 bytes, at `end`; only a tree built with
    /// `perf-frame-texture` reads it. Absent elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) frame_texture_bytes: Option<u64>,
    /// How this checkpoint's memory sampling ended: `complete`, `exhausted`, or `active` for a run
    /// that stopped before it did. Absent from a build without the checkpoint-memory hook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sampling: Option<&'static str>,
    /// Memory samples attempted for this checkpoint; absent without the hook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) attempts: Option<u32>,
    /// Whether the last attempted sample measured every pane; absent without the hook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_attempt_complete: Option<bool>,
    /// One glyph atlas reading per memory sampling attempt, taken in the same turn as that attempt's
    /// memory line; absent when no attempt was taken.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) atlas_readings: Vec<AtlasReading>,
}

/// The window identities and counted glyph atlas growths at one memory sampling attempt, so a report
/// can compare each live window's counted growths with its renderer's snapshot growths at one instant.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct AtlasReading {
    /// The sampling attempt this reading was taken with, from 1.
    pub(crate) attempt: u32,
    /// The main window's native label, spelled as the memory line labels its renderer; `None` before
    /// the App has a main window.
    pub(crate) main_window: Option<String>,
    /// Each live window's `glyph_atlas_growths` since it was created, by native label; only windows
    /// with a renderer have one. `None` when the run does not count.
    pub(crate) counted_glyph_atlas_growths: Option<std::collections::BTreeMap<String, u64>>,
    /// Every closed window's `glyph_atlas_growths`, summed; `None` when the run does not count.
    pub(crate) closed_glyph_atlas_growths: Option<u64>,
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
    /// Windows: native `CursorMoved` events dropped because the pointer rested where the window
    /// opened under it; always 0 on macOS.
    pub(crate) native_cursor_rest_events_dropped: u64,
    /// What `App::finish_session` returned: true when every pane's PTY teardown drained within
    /// its bound. `result.json` is written only after that call, so false means it did not drain.
    pub(crate) finish_session_settled: bool,
    /// Whether this build and run record frame counters.
    pub(crate) frame_counters: CountersMode,
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
        put("native_cursor_rest_events_dropped", json!(self.native_cursor_rest_events_dropped));
        put("finish_session_settled", json!(self.finish_session_settled));
        put("checkpoint_memory", json!(checkpoint_memory_support()));
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

/// The presenter record for a run on `host`: Windows GDI only for a degraded Windows run, since
/// every other host's degrade path stays on wgpu.
pub(crate) fn presenter_record_for(
    host: Host,
    software_render_mode: &'static str,
    software_rendering: bool,
    degraded: bool,
) -> PresenterRecord {
    PresenterRecord {
        software_render_mode,
        software_rendering,
        software_render_degraded: degraded,
        windows_gdi: host == Host::Windows && degraded,
    }
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

/// Whether this build samples memory at each checkpoint through the App's hook.
#[cfg(feature = "perf-hook-checkpoint-memory")]
pub(crate) const CHECKPOINT_MEMORY: bool = true;
/// Whether this build samples memory at each checkpoint through the App's hook.
#[cfg(not(feature = "perf-hook-checkpoint-memory"))]
pub(crate) const CHECKPOINT_MEMORY: bool = false;

/// `result.json`'s `checkpoint_memory`: `supported` when this build has the hook, else `unsupported`.
/// The serializer reads it here, so no caller can record a value the build does not have.
pub(crate) fn checkpoint_memory_support() -> &'static str {
    if CHECKPOINT_MEMORY {
        "supported"
    } else {
        // When: `CHECKPOINT_MEMORY` is false, this build takes no checkpoint sample.
        "unsupported"
    }
}

/// The schema version of `result.json` and `progress.json`.
const SCHEMA_VERSION: u32 = 1;

/// Every measurement a run has completed so far, borrowed from where the run keeps it.
/// `progress.json` and `result.json` both record exactly these fields.
#[derive(Clone, Copy)]
pub(crate) struct Measurements<'run> {
    /// Whether the phases carry frame counters: unsupported, off or on.
    pub(crate) frame_counters: CountersMode,
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
            frame_counters: self.frame_counters,
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
        let mut fields = serializer.serialize_struct("Measurements", 7)?;
        fields.serialize_field("frame_counters", &self.frame_counters)?;
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
