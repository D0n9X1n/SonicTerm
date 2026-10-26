//! Per-phase samples, S2 latency attribution, grid scanning and the `result.json` and
//! `progress.json` documents.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use serde_json::{json, Map, Value};
use sonicterm_grid::grid::Grid;
use sonicterm_ui::selection::plain_text_from_grid_range;

use crate::scenarios::{
    Host, HoverDeltas, ParkEvidence, Presentation, Workload, REST_PATH_ROW, REST_REGION_TOP,
};
use crate::workload::{REST_TARGET_NAME, REST_URL};

use crate::counters::{CounterTotals, CountersMode};

/// The characters S2 types, cycled; each self-inserts at a `zsh -f` prompt.
const TYPED_SYMBOLS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

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

/// What one nonblocking look at the echo cell found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EchoSnapshot {
    /// The parser lock was busy, so nothing was read.
    Busy,
    /// The grid was read: whether the echo is there, the grid's revision and its row identity.
    Read { present: bool, revision: u64, identity: RowIdentity },
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

// The split schema lives in `scenarios`, which every build compiles, because `--list` declares it
// on hosts where this module is not built.
pub(crate) use crate::scenarios::SPLIT_SCHEMA;
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
    /// The sample is outside `split_in_scope` (neither S2/default nor S2/flood); nothing was armed.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
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
    /// What the sample's dispatch timeline came to at its close; [`TIMELINE_NOT_TAKEN`] until a close
    /// records it.
    pub(crate) dispatch_timeline: &'static str,
    /// The credited sample's echo timeline; `None` when uncredited or out of the timeline's scope.
    pub(crate) echo_timeline: Option<crate::timeline::TimelineSample>,
}

/// A sample whose close has not recorded its dispatch timeline: every probe close path records one, so
/// this value in `result.json` means a close path was missed.
pub(crate) const TIMELINE_NOT_TAKEN: &str = "not-taken";

impl Serialize for LatencySample {
    fn serialize<Format: Serializer>(
        &self,
        serializer: Format,
    ) -> Result<Format::Ok, Format::Error> {
        // `attributed` is derived from `latency_ms`, so a reader need not infer it.
        let mut fields = serializer.serialize_struct("LatencySample", 8)?;
        fields.serialize_field("inject_unix_s", &self.inject_unix_s)?;
        fields.serialize_field("latency_ms", &self.latency_ms)?;
        fields.serialize_field("attributed", &self.latency_ms.is_some())?;
        fields.serialize_field("reason", self.reason)?;
        fields.serialize_field("split", &self.split)?;
        fields.serialize_field("split_reason", self.split_reason)?;
        fields.serialize_field("dispatch_timeline", self.dispatch_timeline)?;
        let timeline = self.echo_timeline.map(crate::timeline::TimelineSample::to_json);
        fields.serialize_field("echo_timeline", &timeline)?;
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
        // Only credited samples carry a timeline; its coverage is recomputed from their fields.
        let timelines = samples
            .iter()
            .filter(|sample| sample.latency_ms.is_some())
            .filter_map(|sample| sample.echo_timeline.as_ref());
        let timeline_coverage = crate::timeline::coverage(timelines);
        let mut fields = serializer.serialize_struct("LatencyReport", 9)?;
        fields.serialize_field("samples", samples)?;
        fields.serialize_field("attributed", &attributed)?;
        fields.serialize_field("total", &samples.len())?;
        fields.serialize_field("coverage", &coverage)?;
        fields.serialize_field("split_schema", &SPLIT_SCHEMA)?;
        fields.serialize_field("split_count", &split_count)?;
        fields.serialize_field("split_reasons", &reasons)?;
        fields.serialize_field("split_coverage", &split_coverage)?;
        fields.serialize_field("echo_timeline_coverage", &timeline_coverage)?;
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
    use crate::timeline::TimelineTake;

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

    /// Take `pane`'s record for `token`, reading publications against the measurement window `main`,
    /// then drain its timeline, so no transfer is left behind on any take.
    pub(crate) fn take(
        app: &mut App,
        pane: u64,
        token: EchoToken,
        main: Option<WindowId>,
    ) -> (EchoOutcome, TimelineTake) {
        let taken = app.take_echo_watch(pane, token);
        let armed_at = match &taken {
            TakeOutcome::Trace(trace) => Some(trace.armed_at),
            _ => None,
        };
        let outcome = match taken {
            TakeOutcome::Trace(trace) => EchoOutcome::Taken(facts_of(&trace, main)),
            TakeOutcome::GateOff => EchoOutcome::Failed("take-gate-off"),
            TakeOutcome::NoPane => EchoOutcome::Failed("take-no-pane"),
            TakeOutcome::Mismatch => EchoOutcome::Failed("take-mismatch"),
            TakeOutcome::AlreadyTaken => EchoOutcome::Failed("take-already-taken"),
        };
        (outcome, crate::timeline::adapter::drain(app, pane, token, armed_at))
    }

    /// Disarm `pane`'s watch for `token` and drain its timeline, discarding both records.
    pub(crate) fn discard(app: &mut App, pane: u64, token: EchoToken) {
        // The records of an uncredited sample are not used; the takes only disarm and drain.
        let _ = app.take_echo_watch(pane, token);
        let _ = crate::timeline::adapter::drain(app, pane, token, None);
    }

    /// The token's number, an identity a timeline sample records.
    pub(crate) fn token_number(token: EchoToken) -> u64 {
        token.get()
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
    ) -> (EchoOutcome, crate::timeline::TimelineTake) {
        match token {}
    }

    /// Unreachable: no token exists.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    pub(crate) fn token_number(token: EchoToken) -> u64 {
        match token {}
    }

    /// Unreachable: no token exists.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    pub(crate) fn discard(_app: &mut App, _pane: u64, token: EchoToken) {
        match token {}
    }
}

pub(crate) use echo_api::EchoToken;

/// What a parked S6/rest phase sees of role 0's pane: the screen's identity, the content stamps of
/// the fixed fixture rows above the streaming region, and the stamp of the row the pointer rests on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParkSnapshot {
    /// Eviction, screen incarnation and size generation; any change renumbers or replaces rows.
    pub(crate) identity: RowIdentity,
    /// History rows; a scroll region below the fixture never adds one.
    pub(crate) scrollback_len: usize,
    pub(crate) alt_screen: bool,
    /// The pane's pinned viewport top; `None` follows the live screen.
    pub(crate) viewport: Option<u64>,
    /// The grid's whole-content stamp, which the streaming region advances.
    pub(crate) content_seq: u64,
    /// Content stamps of the fixed rows above `REST_REGION_TOP`, top first.
    pub(crate) fixture_stamps: Vec<Option<u64>>,
    /// The displayed row the pointer rests on, and its content stamp.
    pub(crate) pointed_row: u16,
    pub(crate) pointed_stamp: Option<u64>,
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
/// The SLOW_DISPATCH_LIMIT longest dispatches of a phase, kept in a bounded min-heap, and how many were offered.
#[derive(Debug, Default)]
pub(crate) struct SlowDispatches {
    shortest_first: std::collections::BinaryHeap<std::cmp::Reverse<ByDuration>>,
    count: u64,
}

/// One measured phase's samples.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct PhaseRecord {
    /// Phase name.
    pub(crate) name: &'static str,
    /// The phase's kind: `sustained`, `transition` or `hold`, from its scenario contract.
    pub(crate) kind: &'static str,
    /// A transition's endpoint, the event its completion is measured to; absent for other kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) endpoint: Option<&'static str>,
    /// Milliseconds from the phase's start to its endpoint, reached before the run's deadline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) completion_ms: Option<f64>,
    /// Why a transition has no completion: `expired` or `incomplete`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) completion_missing: Option<&'static str>,
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
    /// Ms from the phase's start to the end of its first main-window redraw that presented.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) first_present_ms: Option<f64>,
    /// Ms from the phase's start to the end of its last main-window redraw that presented.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_present_ms: Option<f64>,
    /// The main renderer's presented count after the first presenting redraw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) first_present_seq: Option<u64>,
    /// The main renderer's presented count after the last presenting redraw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) last_present_seq: Option<u64>,
    /// `no-presentation` when no main-window redraw of the phase presented; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) present_missing: Option<&'static str>,
    /// Main-window redraws that did not advance the presented count.
    pub(crate) nonpresenting_redraws: u64,
    /// Diagnostic only: ms from GO to the roles' first output, when the phase released them and saw it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) go_to_first_output_ms: Option<f64>,
    /// Diagnostic only: ms from that output to the first frame presented after it, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) first_output_to_present_ms: Option<f64>,
    /// Each `RedrawRequested` dispatch's duration, in ms.
    pub(crate) dispatch_ms: Vec<f64>,
    /// The `dispatch_ms` values whose redraw presented a frame.
    pub(crate) dispatch_presenting_ms: Vec<f64>,
    /// The `dispatch_ms` values whose redraw presented nothing.
    pub(crate) dispatch_nonpresenting_ms: Vec<f64>,
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
    /// S10's per-present attribution for this phase; present only for a phase that plays counted updates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) s10_attribution: Option<crate::attribution::Attribution>,
    /// The phase's guard-correlation transport, written once its endpoints are latched; absent when no
    /// transport ran (a killed run), which perf-compare reads as incomplete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) guard_correlation: Option<crate::guard_transport::GuardCorrelationField>,
    /// Nanoseconds the transport took: the take, serialization, digest and write. Never added to any
    /// completion, timing, CPU, wall or counter field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) guard_correlation_transport_ns: Option<u64>,
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
    /// The main renderer's glyph completeness at S9's and S12's `end`; absent elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) completeness: Option<CompletenessRecord>,
}

/// The reason a build without `perf_completeness_api` records, since it cannot read the renderer.
#[cfg(any(test, not(perf_completeness_api)))]
pub(crate) const COMPLETENESS_API_DISABLED: &str = "api-disabled";

/// One perf-end glyph completeness reading: the distinct missing terminal and chrome characters the
/// renderer's last `Full` frame certified, or why it has none, with the renderer's scale.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct CompletenessRecord {
    /// `certified` or `unavailable`.
    pub(crate) state: &'static str,
    /// Why the reading is unavailable; absent when certified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<&'static str>,
    /// Distinct missing terminal characters; absent when unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) missing_terminal: Option<usize>,
    /// Distinct missing chrome characters; absent when unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) missing_chrome: Option<usize>,
    /// The glyph atlas width at the certified frame, in pixels; absent when unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) atlas_width: Option<u32>,
    /// The glyph atlas height at the certified frame, in pixels; absent when unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) atlas_height: Option<u32>,
    /// The main renderer's display scale factor.
    pub(crate) scale: f64,
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
    /// What the App's covered-window trim hook reported, as `result.json`'s `hooks.trim`.
    pub(crate) trim_hook: TrimHookOutcome,
    /// The plan's trim experiment, as `result.json`'s `trim_experiment`; `None` for every other plan.
    pub(crate) trim_experiment: Option<&'static str>,
    /// The App's trim number the hook's own trim reported; `None` unless the hook trimmed. A
    /// checkpoint sample whose `trim_seq` is at least this one was taken after that trim.
    pub(crate) trim_seq_after_hook: Option<u64>,
    /// S1/atlas-retry's recovery episodes, as `result.json`'s `atlas_recovery`; absent elsewhere.
    pub(crate) atlas_recovery: Option<Value>,
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

/// Whether this build samples memory at each checkpoint through the App's hook.
#[cfg(feature = "perf-hook-checkpoint-memory")]
pub(crate) const CHECKPOINT_MEMORY: bool = true;
/// Whether this build samples memory at each checkpoint through the App's hook.
#[cfg(not(feature = "perf-hook-checkpoint-memory"))]
pub(crate) const CHECKPOINT_MEMORY: bool = false;

/// What the App's covered-window trim hook reported for a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrimHookOutcome {
    /// The hook was never called: the plan asks for no trim, or the covered hold ended before the
    /// App was covered.
    NotReached,
    /// No trim: a build without `perf-hook-trim`, or an App whose hook cannot trim yet. The run
    /// measures an untrimmed baseline, so its memory readings stay valid figures.
    Unsupported,
    /// The App examined the window and skipped it.
    // Built only by the hook adapter, which exists only with perf-hook-trim.
    #[cfg_attr(not(feature = "perf-hook-trim"), allow(dead_code))]
    Skipped,
    /// The App trimmed the window's renderer.
    // Built only by the hook adapter, which exists only with perf-hook-trim.
    #[cfg_attr(not(feature = "perf-hook-trim"), allow(dead_code))]
    Trimmed,
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

#[cfg(test)]
#[path = "record_tests.rs"]
mod record_tests;

#[path = "record_grid.rs"]
mod grid;
pub(crate) use grid::{
    armed_token, bulk_tail_mismatch, echo_target, line_row_near_cursor, missing_wide_tokens,
    park_entry_problem, park_snapshot, planned_rows, prompt_identity, prompt_origin, protocol_rows,
    retained_text, row_count_mismatch, sentinel_row, snapshot_echo, snapshot_identity_changed,
    wide_tokens,
};

#[path = "record_latency.rs"]
mod latency;
use latency::ns_to_ms;
pub(crate) use latency::{attribute_dispatch, duration_ns, echo_outcome, split_in_scope};
#[path = "record_result.rs"]
mod result;
#[cfg(any(test, not(perf_completeness_api)))]
pub(crate) use result::completeness_api_disabled;
#[cfg(perf_completeness_api)]
pub(crate) use result::completeness_from_checkpoint;
pub(crate) use result::{
    presenter_blocked, presenter_record_for, takes_completeness, write_progress,
};

#[path = "record_slow_dispatch.rs"]
mod slow_dispatch;

#[cfg(test)]
pub(crate) use grid::foreground_lock_note;

#[cfg(test)]
pub(crate) use result::{checkpoint_memory_support, filetime_seconds};

use slow_dispatch::ByDuration;
