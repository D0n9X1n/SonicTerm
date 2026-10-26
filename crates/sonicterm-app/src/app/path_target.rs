//! Contextual filesystem-target detection, validation, and direct-open contracts.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use smallvec::SmallVec;
#[cfg(test)]
use sonicterm_cfg::url_scan::{bare_name_at_char_col_for_style, find_targets_for_style};
use sonicterm_cfg::url_scan::{
    target_candidates_at_char_col_for_style, DetectedTarget, PathStyle, TargetMatch,
};
use sonicterm_gpu::core::GpuRenderer;
#[cfg(test)]
use sonicterm_grid::grid::Grid;
use sonicterm_grid::grid::{Cell, CellFlags, Row};
use sonicterm_vt::vt::Osc7Cwd;
use winit::window::WindowId;

use super::App;
use frame_rows::FrameRows;
use hover_cache::DetectionContext;
pub(super) use hover_cache::HoverDetectionCache;

mod frame_rows;
mod hover_cache;
#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
mod unix;
#[cfg(any(target_os = "windows", test))]
#[path = "path_target/windows.rs"]
mod windows_os;

#[cfg(target_os = "linux")]
use linux::{classify_local_target, open_native_path, reveal_native_file};
#[cfg(target_os = "macos")]
use macos::{classify_local_target, open_native_path, reveal_native_file};
#[cfg(target_os = "windows")]
use windows_os::{classify_local_target, open_native_path, reveal_native_file};

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn classify_local_target(path: &Path) -> PathOpenDecision {
    let _ = path;
    PathOpenDecision::Blocked
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn reveal_native_file(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "file selection unavailable"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn open_native_path(_path: &Path, _expected_decision: PathOpenDecision) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "path open is unsupported"))
}

/// A typed target extracted from one terminal row at one cell column.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RowTarget {
    pub(super) matched: TargetMatch,
    pub(super) start_col: u16,
    pub(super) end_col: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogicalTargetCandidate {
    target: DetectedTarget,
    spans: SmallVec<[AbsoluteCellSpan; 2]>,
    /// Whether the candidate joins rows a program broke itself (the hard-wrap rule).
    joined: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogicalPathScan {
    candidates: Vec<LogicalTargetCandidate>,
    rows: SmallVec<[PathRowIdentity; 2]>,
}

/// Filesystem kind that may be handed to a platform default application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathKind {
    File,
    Directory,
}

/// Result of asynchronous local-target validation, including the permitted native action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathOpenDecision {
    /// Eligible local kind: directories navigate and regular files reveal in the file manager.
    Openable(PathKind),
    /// Reveal in the native file manager without opening or executing the target.
    #[cfg(any(target_os = "macos", test))]
    Revealable(PathKind),
    /// Reveal a validated source reference without invoking its file association.
    SourceReveal,
    /// Existing target whose identity or content is not safe to dispatch.
    Blocked,
    /// Target did not exist when probed.
    Missing,
}

impl PathOpenDecision {
    fn is_actionable(self) -> bool {
        match self {
            Self::Openable(_) | Self::SourceReveal => true,
            #[cfg(any(target_os = "macos", test))]
            Self::Revealable(_) => {
                // When: `self` is `Revealable`, permit the same epoch-keyed click path without opening the target.
                true
            }
            Self::Blocked | Self::Missing => false,
        }
    }

    #[cfg(any(target_os = "windows", test))]
    fn is_blocked(self) -> bool {
        self == Self::Blocked
    }
}

/// Monotonic identity that prevents stale probe results from surviving ABA transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProbeEpoch(u64);

impl ProbeEpoch {
    pub(super) const INITIAL: Self = Self(0);

    pub(super) fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Rows one reconstructed target may span: the renderer underlines at most this many fragments.
const MAX_WRAPPED_PATH_ROWS: usize = sonicterm_render_model::inputs::MAX_HOVERED_URL_SPANS;

/// One absolute grid cell under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AbsoluteCell {
    pub(crate) row: u64,
    pub(crate) col: u16,
}

/// One non-empty candidate fragment in scrollback-absolute coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AbsoluteCellSpan {
    pub(crate) row: u64,
    pub(crate) start_col: u16,
    pub(crate) end_col: u16,
}

impl AbsoluteCellSpan {
    fn contains(self, cell: AbsoluteCell) -> bool {
        self.row == cell.row && cell.col >= self.start_col && cell.col < self.end_col
    }

    #[cfg(test)]
    fn len(self) -> usize {
        usize::from(self.end_col.saturating_sub(self.start_col))
    }
}

/// Hash identity of one row participating in a reconstructed logical line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PathRowIdentity {
    pub(crate) row: u64,
    pub(crate) fingerprint: u64,
}

/// One typed filesystem candidate carried to the background probe worker.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathProbeCandidate {
    pub(crate) spans: SmallVec<[AbsoluteCellSpan; 2]>,
    pub(crate) target: DetectedTarget,
    pub(crate) resolved_path: PathBuf,
    /// Whether the candidate joins rows a program broke itself; such a candidate is probed
    /// before every row fragment of its key.
    pub(crate) joined: bool,
}

impl PathProbeCandidate {
    fn display(&self) -> &str {
        match &self.target {
            DetectedTarget::PathCandidate(candidate) | DetectedTarget::BareName(candidate) => {
                candidate
            }
            DetectedTarget::SourceReference(reference) => &reference.display,
            DetectedTarget::Uri(_) => unreachable!(),
        }
    }

    /// Unicode scalars of the displayed candidate; the unit of probe tiers and the length cap.
    fn char_len(&self) -> usize {
        self.display().chars().count()
    }

    /// Probe order: shortest displayed candidate first, then the earlier start.
    fn probe_order(&self) -> (usize, &[AbsoluteCellSpan]) {
        (self.char_len(), self.spans.as_slice())
    }

    fn contains(&self, cell: AbsoluteCell) -> bool {
        self.spans.iter().copied().any(|span| span.contains(cell))
    }

    fn visible_cells(
        &self,
        pane_id: u64,
        view_top: u64,
        active: bool,
    ) -> Option<sonicterm_render_model::inputs::HoveredUrlCells> {
        let spans = self.spans.iter().map(|span| {
            Some(sonicterm_render_model::inputs::HoveredUrlSpan {
                row: u16::try_from(span.row.checked_sub(view_top)?).ok()?,
                start_col: span.start_col,
                end_col: span.end_col,
            })
        });
        sonicterm_render_model::inputs::HoveredUrlCells::new(
            pane_id,
            spans.collect::<Option<Vec<_>>>()?,
            active,
        )
    }
}

/// Immutable identity of one bounded candidate set at one rendered pane cell.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathProbeKey {
    pub(crate) window_id: WindowId,
    pub(crate) pane_id: u64,
    pub(crate) pointed: AbsoluteCell,
    pub(crate) view_top: u64,
    pub(crate) candidates: Vec<PathProbeCandidate>,
    pub(crate) rows: SmallVec<[PathRowIdentity; 2]>,
    pub(crate) cwd: Option<Osc7Cwd>,
    pub(crate) cwd_revision: u64,
    pub(crate) link_destination: Option<String>,
    pub(crate) scrollback_evicted: u64,
    pub(crate) screen_epoch: u64,
    pub(crate) alt_screen: bool,
}

/// One openability request sent to the bounded probe worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProbeRequest {
    pub(crate) epoch: ProbeEpoch,
    pub(crate) key: PathProbeKey,
}

/// Selected candidate and openability returned by the background probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProbeSelection {
    pub(crate) candidate: PathProbeCandidate,
    pub(crate) decision: PathOpenDecision,
}

/// Openability result returned to the event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathProbeResult {
    pub(crate) request: PathProbeRequest,
    pub(crate) selection: Option<PathProbeSelection>,
    pub(crate) failure: Option<&'static str>,
}

/// Per-window authorization state for the raw path currently under the pointer.
#[derive(Debug, Clone, Default)]
pub(crate) struct PathProbeState {
    epoch: ProbeEpoch,
    current: Option<PathProbeKey>,
    selection: Option<PathProbeSelection>,
    failure: Option<&'static str>,
    // Only the current epoch/key may complete, so one unvalidated result per window is sufficient.
    pending_result: Option<PathProbeResult>,
    failed_click: Option<FailedTargetClick>,
}

#[derive(Debug, Clone)]
struct FailedTargetClick {
    pane_id: u64,
    target: String,
    message: String,
    reason: String,
}

impl Default for ProbeEpoch {
    fn default() -> Self {
        Self::INITIAL
    }
}

/// What the status line's link segment reads from the probe under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeStatus<'probe> {
    /// Validation has not finished for this path.
    Pending(&'probe Path),
    /// The winning candidate's path and the action a click takes.
    Selected(&'probe Path, PathOpenDecision),
    /// Validation finished without an actionable candidate; the i18n key says why.
    Failed(&'probe Path, &'static str),
    /// A bare name with no shell-reported directory to resolve it against.
    NoDirectory(&'probe Path),
    /// A bare name on the alternate screen, where bare names are not resolved.
    AltScreen(&'probe Path),
}

/// Pending openability probes in fair first-queued window order.
///
/// Each live window holds at most one waiting request. A newer request from the
/// same window replaces it in place, so one window's churn never displaces or
/// reorders another window's waiting probe.
#[derive(Default)]
struct PathProbeQueue {
    order: VecDeque<WindowId>,
    pending: HashMap<WindowId, PathProbeRequest>,
    /// `pending.len()`, published after every change so the status line reads it without the lock.
    waiting: Arc<std::sync::atomic::AtomicUsize>,
}

/// Fair per-window probe mailbox that wakes one worker.
///
/// One request executes while each live window keeps only its newest waiting
/// request, so output streaming in one window cannot displace another
/// window's stationary hover. Waiting work is bounded by one key per live
/// window, plus the one executing request.
#[derive(Clone)]
pub(super) struct PathProbeMailbox {
    queue: Arc<Mutex<PathProbeQueue>>,
    wake: Sender<()>,
    /// The queue's published waiting count.
    waiting: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Debug, PartialEq, Eq)]
enum LocalTargetAction {
    Navigate,
    Reveal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathOpenRequest {
    window_id: WindowId,
    pane_id: u64,
    path: PathBuf,
    expected_decision: PathOpenDecision,
    /// Every candidate at or before the selected tier; re-probing them must reproduce the selection.
    contenders: Vec<PathProbeCandidate>,
}

/// App-owned handles for the bounded path probe and open workers.
pub(crate) struct PathWorkers {
    probe: PathProbeMailbox,
    open: Sender<PathOpenRequest>,
    /// Each worker's busy slot, stamped around its blocking call.
    busy: Arc<super::jobs::PathBusy>,
}

const MAX_LOGICAL_PATH_BYTES: usize = 4096;
/// Byte cap of one reconstructed logical line: 32 rows of 128 four-byte cells.
const MAX_LOGICAL_LINE_BYTES: usize = 16 * 1024;
/// Blank cells an OSC 8 fragment may leave before its pane's right edge and still continue below.
const LINK_CONTINUATION_MARGIN: u16 = 2;
/// Blank cells of hanging indent that may precede an OSC 8 continuation fragment.
const LINK_CONTINUATION_INDENT: u16 = 8;
const MAX_HARDWRAP_INDENT: usize = 8;

#[cfg_attr(test, derive(Debug, PartialEq))]
enum HardwrapUri {
    NotApplicable,
    Incomplete,
    Complete(LogicalTargetCandidate),
}

/// Test-only count of the row cells the hard-wrap pre-pass walks, so tests can pin its work as
/// linear in the row width.
#[cfg(test)]
pub(super) mod hardwrap_work {
    use std::cell::Cell;

    thread_local! {
        static CELLS_WALKED: Cell<u64> = const { Cell::new(0) };
    }

    /// Record `cell_count` more cells walked on this thread.
    pub(in crate::app) fn add(cell_count: usize) {
        CELLS_WALKED.with(|walked| walked.set(walked.get().saturating_add(cell_count as u64)));
    }

    /// Run `body` and return its value with the cells it walked on this thread.
    pub(in crate::app) fn measure<Value>(body: impl FnOnce() -> Value) -> (Value, u64) {
        let before = CELLS_WALKED.with(Cell::get);
        let value = body();
        (value, CELLS_WALKED.with(Cell::get).saturating_sub(before))
    }
}

/// One displayed row as the hard-wrap pre-pass reads it: its cells, collected once, and the last
/// column before the grid's edge that no URI body may contain.
struct HardwrapRowView<'grid> {
    row: &'grid Row,
    cells: Vec<&'grid Cell>,
    /// The last column below `min(cells.len(), cols)` holding a [`hardwrap_body_break`] cell.
    last_break: Option<usize>,
}

/// The displayed rows one hard-wrap pre-pass has read, each collected into a view once, so a row
/// with many wrappers, or one that several chains reach, costs one walk over its cells.
struct HardwrapRows<'grid> {
    views: HashMap<u64, Option<HardwrapRowView<'grid>>>,
}

impl<'grid> HardwrapRows<'grid> {}

/// Blank cells a program may leave before a pane's right edge when it breaks a long token across
/// rows itself. Claude Code 2.1.306, measured in tmux at 64, 80, 103 and 120 columns, ends every
/// non-last row of a wrapped URL exactly at the edge, so no margin is allowed.
const HARDWRAP_EDGE_MARGIN: u16 = 0;

/// Bullet markers that may sit between a row's indent and a token that starts the row's content.
/// A numbered marker (`1. ` through `999. `) is recognized separately.
const HARDWRAP_BULLETS: [&str; 4] = ["⏺ ", "- ", "* ", "• "];

/// The token a program-broken chain carries. The row geometry is shared by every kind; only the
/// opening-token test, the new-token test and the joined-text validation depend on it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HardwrapKind {
    /// An `http://`, `https://` or `file://` URL, validated by `find_urls`.
    Uri,
    /// An absolute POSIX (`/…`), home (`~/…`) or drive (`C:\…`, `C:/…`) path, parsed by the typed
    /// path scanner.
    Path,
}

/// One row's text inside a pane, as the program-broken chain rule reads it.
#[derive(Clone, Copy)]
struct HardwrapRowToken {
    /// First column after the pane-relative indent that holds text.
    first_text: usize,
    /// Column where the token starts: `first_text`, or just after one bullet marker.
    token_start: usize,
    /// First column after the token: a break cell, or the pane's right edge.
    token_end: usize,
    /// Whether text follows the token before the pane's right edge.
    trailing_text: bool,
}

/// One row of a prospective chain read inside its pane: the token, its text, and whether the
/// terminal recorded a wrap into the row.
struct HardwrapPaneRow {
    token: HardwrapRowToken,
    text: String,
    soft_wrapped: bool,
}

/// A prospective program-broken chain, decided for one pointed cell.
enum HardwrapChain {
    /// The rule does not own the pointed cell; the single-row result stands.
    NotApplicable,
    /// An owned chain covering the pointed cell cannot be proven, so no prefix may resolve.
    Incomplete,
    /// A proven chain covering the pointed cell: its fragments and their joined text.
    Proven { spans: SmallVec<[AbsoluteCellSpan; 2]>, joined: String },
}

/// A path a program broke across rows, decided for one pointed cell.
enum HardwrapPathScan {
    /// The rule does not own the pointed cell.
    NotApplicable,
    /// An owned chain covering the pointed cell cannot be proven.
    Incomplete,
    /// The joined path candidates, every one covering the pointed cell, with every chain row's identity.
    Joined(LogicalPathScan),
}

struct PathScalar {
    byte_start: usize,
    byte_end: usize,
    cell_start: usize,
    cell_end: usize,
    valid: bool,
}

struct PathCellText {
    text: String,
    scalars: Vec<PathScalar>,
}

/// Vertical box-drawing glyphs that tmux, rmux and Zellij draw down the column between two panes.
///
/// A column counts as a pane edge only when two adjacent rows both draw one of these there.
const PANE_BORDER_GLYPHS: [char; 12] = [
    '\u{2502}', '\u{251C}', '\u{2524}', '\u{253C}', '\u{2503}', '\u{2523}', '\u{252B}', '\u{254B}',
    '\u{2551}', '\u{2560}', '\u{2563}', '\u{256C}',
];

/// One way a logical line can grow by one row.
#[derive(Debug, Clone, Copy)]
enum ChainStep {
    /// A soft wrap the terminal recorded continues the line into this segment.
    Recorded(PaneRowSegment),
    /// An alternate-screen pane-edge continuation with no recorded wrap.
    Inferred(PaneRowSegment),
    /// The line ends in this direction.
    End,
    /// A recorded wrap's partner row is out of view, so the line cannot be proven.
    Incomplete,
}

/// The columns `[left, right)` of absolute `row` that one pane's text occupies.
#[derive(Debug, Clone, Copy)]
struct PaneRowSegment {
    row: u64,
    left: u16,
    right: u16,
}

/// Byte cap of an OSC 8 link's captured visible text; a longer label is not compared.
const MAX_LINK_LABEL_BYTES: usize = 2048;

/// An OSC 8 link's visible text and whether it was read whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkLabel {
    /// The text the link's cells show, at most [`MAX_LINK_LABEL_BYTES`].
    pub text: String,
    /// False when the fragments reach the view's top or bottom row, hit the span cap, or the
    /// text passes the byte cap, so part of the label may be unseen.
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedCellTarget {
    Uri(String),
    Rejected(&'static str),
    Path(PathProbeKey),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CellTargetSnapshot {
    pub(super) pane_id: u64,
    pub(super) hover_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    pub(super) display: String,
    pub(super) explicit_hyperlink: bool,
    pub(super) target: ResolvedCellTarget,
    /// An OSC 8 URI's visible text; `None` for every other target.
    pub(super) label: Option<LinkLabel>,
}

#[cfg(test)]
#[path = "path_target_tests.rs"]
mod path_target_tests;

#[path = "path_target_probe_state.rs"]
mod probe_state;

#[path = "path_target_probe_worker.rs"]
mod probe_worker;
use probe_worker::contends_with;
#[cfg(test)]
use probe_worker::select_openable_candidate;
#[path = "path_target_hardwrap_uri.rs"]
mod hardwrap_uri;
#[cfg(test)]
use hardwrap_uri::hardwrap_uri_at_cell;
use hardwrap_uri::{hardwrap_body_break, hardwrap_uri_in, hardwrap_uri_target};
#[path = "path_target_hardwrap_paths.rs"]
mod hardwrap_paths;
use hardwrap_paths::{bare_hardwrap_path_in, bare_hardwrap_uri_in, merge_hardwrap_fragments};
#[path = "path_target_logical.rs"]
mod logical;
use logical::logical_path_scan_in;
#[cfg(test)]
pub(super) use logical::target_at_row_cell;
#[cfg(test)]
use logical::{logical_path_scan_at_cell, row_target_candidates_at_cell};
#[path = "path_target_resolve.rs"]
mod resolve;
#[cfg(test)]
use resolve::token_bounds;
use resolve::{
    detected_target_chars, detected_target_enabled, normalize_posix_absolute, row_fingerprint,
    unsafe_path_cell,
};
pub(super) use resolve::{local_launch_cwd, native_home_dir, resolve_detected_path};
#[path = "path_target_pane_spans.rs"]
mod pane_spans;
use pane_spans::{
    admit_chain_step, inferred_continuation_above, inferred_continuation_below, pane_border_at,
    pane_columns_at, pointed_text_spans, relative_text_cwd, shared_pane_columns,
    spans_join_across_pane_border, spans_reach_cut_pane_edge, wrap_joins_one_pane,
};
#[path = "path_target_osc8.rs"]
mod osc8;
use osc8::{hyperlink_hover_cells, hyperlink_label};
#[path = "path_target_activate.rs"]
mod activate;
pub(super) use activate::open_modifier_in;
#[path = "path_target_detect.rs"]
mod detect;
#[path = "path_target_hover.rs"]
mod hover;

fn lock_probe_queue(queue: &Mutex<PathProbeQueue>) -> MutexGuard<'_, PathProbeQueue> {
    queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
use probe_state::key_preserves_selection;

#[cfg(test)]
use probe_worker::classify_source_reference;

#[cfg(test)]
use probe_worker::validate_reveal_target;

#[cfg(test)]
use probe_worker::probe_candidates;

#[cfg(test)]
use probe_worker::open_request_still_selected;

#[cfg(test)]
use resolve::resolve_path_candidate;

#[cfg(test)]
use probe_state::same_probe_context;
#[cfg(test)]
use probe_worker::{local_target_action, run_probe_worker};
