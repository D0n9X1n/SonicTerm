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
use sonicterm_grid::grid::{Cell, CellFlags, Grid, Row};
use sonicterm_vt::vt::Osc7Cwd;
use winit::window::WindowId;

use super::App;

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
    missing_before: Vec<DetectedTarget>,
    spans: SmallVec<[AbsoluteCellSpan; 2]>,
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

const MAX_WRAPPED_PATH_ROWS: usize = 8;

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
    pub(crate) missing_before: Vec<PathBuf>,
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

    fn span_len(&self) -> usize {
        self.spans.iter().copied().map(AbsoluteCellSpan::len).sum()
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

impl PathProbeState {
    pub(super) fn invalidate(&mut self) -> bool {
        let changed = self.current.is_some() || self.selection.is_some();
        if changed {
            self.epoch = self.epoch.next();
            self.current = None;
            self.selection = None;
            self.failure = None;
            self.pending_result = None;
        }
        changed
    }

    pub(super) fn request(&mut self, key: PathProbeKey) -> Option<PathProbeRequest> {
        if self.current.as_ref() == Some(&key) {
            // When: `key` is already current, retain its epoch and avoid duplicating an in-flight openability probe.
            return None;
        }
        if self.current.as_ref().is_some_and(|current| {
            self.selection
                .as_ref()
                .is_some_and(|selection| key_preserves_selection(current, &key, selection))
        }) {
            // When: `key` adds no unprobed equal-or-longer contender, retain the selected span under the same row identity.
            self.current = Some(key);
            return None;
        }
        self.epoch = self.epoch.next();
        self.current = Some(key.clone());
        self.selection = None;
        self.failure = None;
        self.pending_result = None;
        Some(PathProbeRequest { epoch: self.epoch, key })
    }

    pub(super) fn accept(
        &mut self,
        result: &PathProbeResult,
        fresh: Option<&PathProbeKey>,
    ) -> bool {
        if result.request.epoch != self.epoch || self.current.as_ref() != Some(&result.request.key)
        {
            // When: `result` differs from the current epoch or key, reject it without disturbing a newer probe.
            return false;
        }
        if !fresh.is_some_and(|fresh| match result.selection.as_ref() {
            Some(selection) => key_preserves_selection(&result.request.key, fresh, selection),
            None => fresh == &result.request.key,
        }) {
            // When: `fresh` no longer reproduces the request context and selected span, revoke the stale authorization.
            self.epoch = self.epoch.next();
            self.current = None;
            self.selection = None;
            self.failure = None;
            self.pending_result = None;
            return false;
        }
        self.current = fresh.cloned();
        self.selection = result.selection.clone();
        self.failure = result.failure;
        true
    }

    pub(super) fn authorized_selection(
        &self,
        key: &PathProbeKey,
        modifier_held: bool,
    ) -> Option<&PathProbeSelection> {
        if !modifier_held || self.current.as_ref() != Some(key) {
            // When: `modifier_held` is false or `current` differs from `key`, no probe result authorizes this click.
            return None;
        }
        self.selection.as_ref().filter(|selection| selection.decision.is_actionable())
    }

    #[cfg(test)]
    pub(super) fn authorized(&self, key: &PathProbeKey, modifier_held: bool) -> bool {
        self.authorized_selection(key, modifier_held).is_some()
    }

    fn failure_for(&self, key: &PathProbeKey) -> Option<&'static str> {
        (self.current.as_ref() == Some(key)).then_some(self.failure).flatten()
    }

    #[cfg(test)]
    pub(super) fn decision_for(&self, key: &PathProbeKey) -> Option<PathOpenDecision> {
        (self.current.as_ref() == Some(key))
            .then(|| self.selection.as_ref().map(|selection| selection.decision))
            .flatten()
    }
}

fn same_probe_context(left: &PathProbeKey, right: &PathProbeKey) -> bool {
    left.window_id == right.window_id
        && left.pane_id == right.pane_id
        && left.view_top == right.view_top
        && left.rows == right.rows
        && left.cwd == right.cwd
        && left.cwd_revision == right.cwd_revision
        && left.link_destination == right.link_destination
        && left.scrollback_evicted == right.scrollback_evicted
        && left.screen_epoch == right.screen_epoch
        && left.alt_screen == right.alt_screen
}

fn key_preserves_selection(
    probed: &PathProbeKey,
    destination: &PathProbeKey,
    selection: &PathProbeSelection,
) -> bool {
    let selected = &selection.candidate;
    // When: destination identity or selected-span ownership changes, discard authorization before any candidate-set work.
    if !same_probe_context(probed, destination)
        || !selected.contains(destination.pointed)
        || !destination.candidates.contains(selected)
    {
        return false;
    }
    let probed_candidates = probed.candidates.iter().collect::<HashSet<_>>();
    probed_candidates.contains(selected)
        // Shorter candidates cannot change a winner selected before their tier.
        && destination
            .candidates
            .iter()
            .filter(|candidate| candidate.span_len() >= selected.span_len())
            .all(|candidate| probed_candidates.contains(candidate))
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
}

impl PathProbeQueue {
    fn submit(&mut self, request: PathProbeRequest) {
        let window_id = request.key.window_id;
        if self.pending.insert(window_id, request).is_none() {
            // A replacement keeps the window's turn; only a newly waiting window joins the back.
            self.order.push_back(window_id);
        }
    }

    fn take_next(&mut self) -> Option<PathProbeRequest> {
        while let Some(window_id) = self.order.pop_front() {
            if let Some(request) = self.pending.remove(&window_id) {
                // When: `pending` holds `window_id`'s newest request, serve it before
                // any later-queued window.
                return Some(request);
            }
        }
        None
    }

    fn cancel_window(&mut self, window_id: WindowId) -> bool {
        self.order.retain(|queued| *queued != window_id);
        self.pending.remove(&window_id).is_some()
    }
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
}

impl PathProbeMailbox {
    pub(super) fn new() -> (Self, Receiver<()>) {
        let (wake, receiver) = crossbeam_channel::bounded(1);
        (Self { queue: Arc::default(), wake }, receiver)
    }

    /// Queue `request` behind other windows' waiting probes, replacing only its own window's.
    pub(super) fn submit(&self, request: PathProbeRequest) -> Result<(), TrySendError<()>> {
        lock_probe_queue(&self.queue).submit(request);
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(error @ TrySendError::Disconnected(())) => Err(error),
        }
    }

    /// Drop `window_id`'s waiting probe; an executing probe completes and is discarded on arrival.
    pub(super) fn cancel_window(&self, window_id: WindowId) -> bool {
        lock_probe_queue(&self.queue).cancel_window(window_id)
    }

    #[cfg(test)]
    pub(super) fn take_next(&self) -> Option<PathProbeRequest> {
        take_next_probe(&self.queue)
    }

    #[cfg(test)]
    pub(super) fn waiting_len(&self) -> usize {
        lock_probe_queue(&self.queue).pending.len()
    }
}

fn lock_probe_queue(queue: &Mutex<PathProbeQueue>) -> MutexGuard<'_, PathProbeQueue> {
    queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Pop the next fair request, releasing the queue lock before any filesystem probe runs.
fn take_next_probe(queue: &Mutex<PathProbeQueue>) -> Option<PathProbeRequest> {
    lock_probe_queue(queue).take_next()
}

/// Serve queued probes until the mailbox disconnects or delivery reports a stopped event loop.
///
/// One wake may stand for several queued windows, so each wake drains the fair
/// queue. `classify` sees only immutable request identities, never native windows.
fn run_probe_worker(
    wake: &Receiver<()>,
    queue: &Mutex<PathProbeQueue>,
    mut classify: impl FnMut(&Path) -> PathOpenDecision,
    mut deliver: impl FnMut(PathProbeResult) -> bool,
) {
    while wake.recv().is_ok() {
        while let Some(request) = take_next_probe(queue) {
            let outcome = probe_candidates(&request.key.candidates, &mut classify);
            let result = PathProbeResult {
                failure: outcome.as_ref().err().copied(),
                selection: outcome.ok(),
                request,
            };
            if !deliver(result) {
                // When: `deliver` reports the event loop is gone, no receiver remains
                // for any later probe result.
                return;
            }
        }
    }
}

fn classify_source_reference(path: &Path) -> PathOpenDecision {
    match classify_local_target(path) {
        PathOpenDecision::Openable(PathKind::File) => PathOpenDecision::SourceReveal,
        PathOpenDecision::Missing => PathOpenDecision::Missing,
        _ => PathOpenDecision::Blocked,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LocalTargetAction {
    Navigate,
    Reveal,
}

fn local_target_action(decision: PathOpenDecision) -> Option<LocalTargetAction> {
    match decision {
        PathOpenDecision::Openable(PathKind::Directory) => Some(LocalTargetAction::Navigate),
        PathOpenDecision::Openable(PathKind::File) | PathOpenDecision::SourceReveal => {
            Some(LocalTargetAction::Reveal)
        }
        #[cfg(any(target_os = "macos", test))]
        PathOpenDecision::Revealable(_) => {
            // When: decision is Revealable, retain selection-only behavior across the macOS policy boundary.
            Some(LocalTargetAction::Reveal)
        }
        PathOpenDecision::Blocked | PathOpenDecision::Missing => None,
    }
}

fn validate_reveal_target(path: &Path, expected: PathOpenDecision) -> io::Result<()> {
    // Each OS's classification applies that platform's link policy.
    let actual = if expected == PathOpenDecision::SourceReveal {
        classify_source_reference(path)
    } else {
        // When: expected is not SourceReveal, retain ordinary file policy rather than source-specific eligibility.
        classify_local_target(path)
    };
    if actual != expected || local_target_action(actual) != Some(LocalTargetAction::Reveal) {
        // When: actual differs from expected or local_target_action is not Reveal, reject stale or changed authorization.
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "changed or blocked reveal target",
        ));
    }
    Ok(())
}

fn open_path(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
    match local_target_action(expected_decision) {
        Some(LocalTargetAction::Navigate) => open_native_path(path, expected_decision),
        Some(LocalTargetAction::Reveal) => {
            validate_reveal_target(path, expected_decision)?;
            reveal_native_file(path)
        }
        None => {
            // No authorized action may reach a native handler.
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "blocked local target"))
        }
    }
}

#[cfg(test)]
fn select_openable_candidate(
    candidates: &[PathProbeCandidate],
    classify: impl FnMut(&Path) -> PathOpenDecision,
) -> Option<PathProbeSelection> {
    probe_candidates(candidates, classify).ok()
}

fn probe_candidates(
    candidates: &[PathProbeCandidate],
    mut classify: impl FnMut(&Path) -> PathOpenDecision,
) -> Result<PathProbeSelection, &'static str> {
    let mut index = 0;
    while index < candidates.len() {
        let span_len = candidates[index].span_len();
        let mut actionable = Vec::new();
        let mut blocked = false;
        while index < candidates.len() && candidates[index].span_len() == span_len {
            let candidate = &candidates[index];
            if candidate
                .missing_before
                .iter()
                .any(|literal| classify(literal) != PathOpenDecision::Missing)
            {
                // When: a literal is present or blocked, its shorter interpretation has no authority even if filtering removed its tier.
                return Err("path-error-ambiguous");
            }
            let decision = if matches!(candidate.target, DetectedTarget::SourceReference(_)) {
                classify_source_reference(&candidate.resolved_path)
            } else {
                // When: matches! rejects SourceReference provenance, keep generic opener classification unchanged.
                classify(&candidate.resolved_path)
            };
            match decision {
                decision @ (PathOpenDecision::Openable(_) | PathOpenDecision::SourceReveal) => {
                    actionable.push(PathProbeSelection { candidate: candidate.clone(), decision });
                }
                #[cfg(any(target_os = "macos", test))]
                decision @ PathOpenDecision::Revealable(_) => {
                    // When: `decision` is `Revealable`, retain its exact Finder-only action in this candidate tier.
                    actionable.push(PathProbeSelection { candidate: candidate.clone(), decision });
                }
                PathOpenDecision::Blocked => blocked = true,
                PathOpenDecision::Missing => {
                    // When: `classify` returns `PathOpenDecision::Missing`, keep searching shorter candidate tiers.
                }
            }
            index += 1;
        }
        if blocked || actionable.len() > 1 {
            // When: blocked or multiple actionable candidates exist, fail closed rather than choosing a shorter path.
            return Err(if blocked { "path-error-blocked" } else { "path-error-ambiguous" });
        }
        if let Some(selection) = actionable.pop() {
            // When: actionable.pop returns the sole longest candidate, authorize its exact action and span.
            return Ok(selection);
        }
    }
    Err("path-error-missing")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathOpenRequest {
    window_id: WindowId,
    pane_id: u64,
    path: PathBuf,
    expected_decision: PathOpenDecision,
    missing_before: Vec<PathBuf>,
}

/// App-owned handles for the bounded path probe and open workers.
pub(crate) struct PathWorkers {
    probe: PathProbeMailbox,
    open: Sender<PathOpenRequest>,
}

impl PathWorkers {
    /// Start one fair per-window openability worker and one serialized target-open worker.
    pub(super) fn start(
        proxy: winit::event_loop::EventLoopProxy<super::UserEvent>,
    ) -> io::Result<Self> {
        let (probe, wake) = PathProbeMailbox::new();
        // The worker owns no wake sender, so dropping the App's mailbox ends it.
        let queue = Arc::clone(&probe.queue);
        let probe_proxy = proxy.clone();
        std::thread::Builder::new()
            .name("sonicterm-path-probe".into())
            .spawn(move || {
                run_probe_worker(&wake, &queue, classify_local_target, |result| {
                    probe_proxy
                        .send_event(super::UserEvent::PathProbeFinished(Box::new(result)))
                        .is_ok()
                });
            })
            .map_err(|error| io::Error::other(format!("spawn path probe worker: {error}")))?;

        let (open, open_rx) = crossbeam_channel::bounded::<PathOpenRequest>(1);
        std::thread::Builder::new()
            .name("sonicterm-path-open".into())
            .spawn(move || {
                while let Ok(request) = open_rx.recv() {
                    let result =
                        if request.missing_before.iter().all(|literal| {
                            classify_local_target(literal) == PathOpenDecision::Missing
                        }) {
                            open_path(&request.path, request.expected_decision)
                        } else {
                            // When: a literal appeared after hover, do not reveal its shorter interpretation at activation time.
                            Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "literal path is no longer missing",
                            ))
                        };
                    if let Err(error) = result {
                        // When: result is an error, return its reason to the requesting window instead of leaving only a log.
                        tracing::warn!(path = ?request.path, %error, "path open failed");
                        let _ = proxy.send_event(super::UserEvent::PathOpenFailed {
                            window_id: request.window_id,
                            pane_id: request.pane_id,
                            reason: error.to_string(),
                            target: request.path.to_string_lossy().into_owned(),
                        });
                    }
                }
            })
            .map_err(|error| io::Error::other(format!("spawn path open worker: {error}")))?;

        Ok(Self { probe, open })
    }

    pub(super) fn probe(&self, request: PathProbeRequest) -> io::Result<()> {
        self.probe
            .submit(request)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "path probe worker stopped"))
    }

    /// Drop a closed window's waiting probe without disturbing any other window's request.
    pub(super) fn cancel_window(&self, window_id: WindowId) {
        self.probe.cancel_window(window_id);
    }

    /// Queue a target open without blocking the event loop.
    ///
    /// `Ok(false)` means the bounded worker already has one running and one
    /// waiting request; the click is still consumed and the extra open drops.
    pub(super) fn open(
        &self,
        window_id: WindowId,
        pane_id: u64,
        path: PathBuf,
        expected_decision: PathOpenDecision,
        missing_before: Vec<PathBuf>,
    ) -> io::Result<bool> {
        match self.open.try_send(PathOpenRequest {
            window_id,
            pane_id,
            path,
            expected_decision,
            missing_before,
        }) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "path open worker stopped"))
            }
        }
    }
}

#[cfg(test)]
fn row_target_at_cell(
    row: &Row,
    col: u16,
    style: PathStyle,
    lookup: impl FnOnce(&str, usize, PathStyle) -> Option<TargetMatch>,
) -> Option<RowTarget> {
    let cells = row.iter().collect::<Vec<_>>();
    let col = usize::from(col);
    cells.get(col)?;
    let (token_start, token_end) = token_bounds(&cells, col);
    if cells[token_start..token_end].iter().any(unsafe_path_cell) {
        // When: any cell in the token is wide or combining, reject the whole token rather than exposing an ASCII suffix.
        return None;
    }

    let mut text = String::with_capacity(cells.len());
    let mut byte_ranges = Vec::with_capacity(cells.len());
    for cell in cells {
        let start = text.len();
        let character =
            // When: `cell.flags` contains `WIDE_CONT`, preserve its column with a non-path sentinel; otherwise keep the cell character.
            if cell.flags.contains(CellFlags::WIDE_CONT) { '\u{fdd0}' } else { cell.ch };
        text.push(character);
        byte_ranges.push((start, text.len()));
    }
    let matched = lookup(&text, col, style)?;
    let start_col = byte_ranges.iter().position(|(start, _)| *start == matched.start)?;
    let end_col =
        byte_ranges.iter().position(|(start, _)| *start >= matched.end).unwrap_or(row.len());
    if start_col < token_start || end_col > token_end {
        // When: the detected span escapes `token_start..token_end`, reject cross-token reconstruction.
        return None;
    }
    let start_col = u16::try_from(start_col).ok()?;
    let end_col = u16::try_from(end_col).ok()?;
    Some(RowTarget { matched, start_col, end_col })
}

#[cfg(test)]
pub(super) fn target_at_row_cell(row: &Row, col: u16, style: PathStyle) -> Option<RowTarget> {
    row_target_at_cell(row, col, style, |text, col, style| {
        let clicked_byte = text.char_indices().nth(col)?.0;
        find_targets_for_style(text, style)
            .into_iter()
            .find(|matched| clicked_byte >= matched.start && clicked_byte < matched.end)
    })
}

#[cfg(test)]
pub(super) fn bare_target_at_row_cell(row: &Row, col: u16, style: PathStyle) -> Option<RowTarget> {
    row_target_at_cell(row, col, style, bare_name_at_char_col_for_style)
}

const MAX_LOGICAL_PATH_BYTES: usize = 4096;
const MAX_HARDWRAP_INDENT: usize = 8;

enum HardwrapUri {
    NotApplicable,
    Incomplete,
    Complete(LogicalTargetCandidate),
}

fn hardwrap_uri_at_cell(grid: &Grid, view_top: u64, pointed: AbsoluteCell) -> HardwrapUri {
    let view_end = view_top.saturating_add(u64::from(grid.rows));
    let first = pointed.row.saturating_sub((MAX_WRAPPED_PATH_ROWS - 1) as u64).max(view_top);
    for row_number in (first..=pointed.row).rev() {
        let Some(row) = grid.row_at_abs(row_number) else {
            // When: row_at_abs has no row_number, that evicted row cannot open a visible candidate.
            continue;
        };
        if row.soft_wrapped_from_previous()
            || grid.row_at_abs(row_number + 1).is_some_and(|next| next.soft_wrapped_from_previous())
        {
            // When: soft_wrapped_from_previous holds at either boundary, the logical scanner owns it.
            continue;
        }
        let cells = row.iter().collect::<Vec<_>>();
        let mut result = HardwrapUri::NotApplicable;
        for (index, cell) in cells.iter().enumerate() {
            let closer = match cell.ch {
                '(' => ')',
                '[' => ']',
                _ => {
                    // When: cell.ch opens no supported wrapper, this column cannot delimit a URI.
                    continue;
                }
            };
            let start = index + 1;
            let scheme = cells[start..].iter().take(8).map(|cell| cell.ch).collect::<String>();
            if !scheme.starts_with("https://") && !scheme.starts_with("http://") {
                // When: scheme is neither https:// nor http://, this wrapper opens ordinary prose.
                continue;
            }
            let found = hardwrap_uri_candidate(grid, view_end, pointed, row_number, start, closer);
            // When: matches! excludes NotApplicable for found, retain this wrapper's claim on pointed.
            if !matches!(found, HardwrapUri::NotApplicable) {
                if !matches!(result, HardwrapUri::NotApplicable) {
                    // When: matches! excludes NotApplicable for result, another wrapper already owns pointed.
                    return HardwrapUri::Incomplete;
                }
                result = found;
            }
        }
        if !matches!(result, HardwrapUri::NotApplicable) {
            // When: matches! excludes NotApplicable for result, the nearest owning wrapper decides pointed.
            return result;
        }
    }
    HardwrapUri::NotApplicable
}

fn hardwrap_uri_candidate(
    grid: &Grid,
    view_end: u64,
    pointed: AbsoluteCell,
    first_row: u64,
    first_col: usize,
    closer: char,
) -> HardwrapUri {
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    let mut joined = String::new();
    let mut indent = None;
    let mut authority_end = None;
    let refusal = |spans: &[AbsoluteCellSpan]| {
        // When: a span contains pointed, suppress that fragment's syntactically valid truncated prefix.
        if spans.iter().any(|span| span.contains(pointed)) {
            HardwrapUri::Incomplete
        } else {
            HardwrapUri::NotApplicable
        }
    };
    for offset in 0..MAX_WRAPPED_PATH_ROWS {
        let row_number = first_row + offset as u64;
        // When: row_number reaches view_end, the chain leaves the viewport and cannot be proven complete.
        if row_number >= view_end {
            return refusal(&spans);
        }
        let Some(row) = grid.row_at_abs(row_number) else {
            // When: row_at_abs has no row_number, the chain cannot be continued or proven.
            return refusal(&spans);
        };
        if row.soft_wrapped_from_previous() {
            // When: soft_wrapped_from_previous holds, the wrap-aware scanner owns this boundary.
            return refusal(&spans);
        }
        let cells = row.iter().collect::<Vec<_>>();
        let start = if offset == 0 {
            first_col
        } else {
            // When: offset is past zero, start skips the continuation indent rather than first_col.
            cells.iter().position(|cell| cell.ch != ' ').unwrap_or(cells.len())
        };
        let close = cells
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, cell)| cell.ch == closer)
            .map(|(col, _)| col);
        if offset == 0 && close.is_some() {
            // When: close exists at offset zero, single-row detection stays authoritative.
            return HardwrapUri::NotApplicable;
        }
        let end = close.unwrap_or(usize::from(grid.cols));
        let Some(body) = cells.get(start..end).filter(|body| !body.is_empty()) else {
            // When: cells hold no non-empty body between start and end, this row adds no fragment.
            return refusal(&spans);
        };
        if body.iter().any(|cell| {
            unsafe_path_cell(cell)
                || cell.hyperlink().is_some()
                || !cell.ch.is_ascii()
                || cell.ch.is_whitespace()
                || cell.ch.is_control()
                || matches!(cell.ch, '(' | ')' | '[' | ']')
        }) {
            // When: any body cell is unsafe, hyperlinked, non-ascii, blank, control, or a wrapper,
            // the row carries prose rather than a continuation of this candidate.
            return refusal(&spans);
        }
        if offset > 0 {
            // When: offset is past zero, a continuation row must open no scheme and respect the indent.
            let prefix = body.iter().take(8).map(|cell| cell.ch).collect::<String>();
            if prefix.starts_with("https://") || prefix.starts_with("http://") {
                // When: prefix opens its own scheme, that independent URI keeps its destination.
                return refusal(&spans);
            }
            if start > MAX_HARDWRAP_INDENT {
                // When: start passes MAX_HARDWRAP_INDENT, the row is too deep to be one continued line.
                return refusal(&spans);
            }
        }
        if offset > 0 && *indent.get_or_insert(start) != start {
            // When: start differs from the recorded indent, the preceding fragment does not own this row.
            return refusal(&spans);
        }
        spans.push(AbsoluteCellSpan {
            row: row_number,
            start_col: start as u16,
            end_col: end as u16,
        });
        if joined.len() + body.len() > MAX_LOGICAL_PATH_BYTES {
            // When: joined plus body passes MAX_LOGICAL_PATH_BYTES, stop before admitting more text.
            return refusal(&spans);
        }
        joined.extend(body.iter().map(|cell| cell.ch));
        if offset == 0 {
            // When: offset is zero, joined must prove a complete authority before the margin cut.
            let scheme_len = if joined.starts_with("https://") { 8 } else { 7 };
            authority_end = joined[scheme_len..]
                .find(['/', '?', '#'])
                .filter(|index| *index > 0 && joined.as_bytes()[scheme_len + index] == b'/')
                .map(|index| scheme_len + index);
            if authority_end.is_none() {
                // When: authority_end is absent, no path slash followed the host, so invent no suffix.
                return refusal(&spans);
            }
        }
        if joined.matches("://").count() != 1 {
            // When: joined holds other than one :// separator, two destinations were concatenated.
            return refusal(&spans);
        }
        if close.is_some() {
            // When: close exists, the wrapper terminates and joined must validate as one whole URI.
            if unsafe_path_cell(&cells[end])
                || cells[end].hyperlink().is_some()
                || cells[end + 1..].iter().take_while(|cell| !cell.ch.is_whitespace()).any(|cell| {
                    unsafe_path_cell(cell) || !matches!(cell.ch, '.' | ',' | ';' | ':' | '!' | '?')
                })
            {
                // When: cells after end continue the token, the apparent closer may be internal URI text.
                return refusal(&spans);
            }
            let matches = sonicterm_cfg::url_scan::find_urls(&joined);
            let Some(found) = matches.first().filter(|found| {
                matches.len() == 1
                    && found.start == 0
                    && found.end == joined.len()
                    && found.url == joined
                    && authority_end.is_some()
            }) else {
                // When: find_urls does not return exactly joined as one whole match, reject the prefix.
                return refusal(&spans);
            };
            if !spans.iter().any(|span| span.contains(pointed)) {
                // When: no span contains pointed, this proven chain does not own the pointer.
                return HardwrapUri::NotApplicable;
            }
            return HardwrapUri::Complete(LogicalTargetCandidate {
                target: DetectedTarget::Uri(found.url.clone()),
                missing_before: Vec::new(),
                spans,
            });
        }
    }
    refusal(&spans)
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

impl PathCellText {
    fn from_cells(cells: &[&Cell], positions: &[AbsoluteCell]) -> Option<Self> {
        let mut text = String::new();
        let mut scalars = Vec::new();
        let mut index = 0;
        while index < cells.len() {
            let cell = cells[index];
            let wide = cell.flags.contains(CellFlags::WIDE);
            let continuation = cell.flags.contains(CellFlags::WIDE_CONT);
            let paired = wide
                && !continuation
                && cells.get(index + 1).is_some_and(|next| {
                    next.flags.contains(CellFlags::WIDE_CONT)
                        && !next.flags.contains(CellFlags::WIDE)
                        && positions[index].row == positions[index + 1].row
                        && positions[index].col.checked_add(1) == Some(positions[index + 1].col)
                });
            let end = index + if paired { 2 } else { 1 };
            let byte_start = text.len();
            // When: continuation is orphaned, keep an invalid non-delimiter scalar so its neighbors cannot join across it.
            let character = if continuation { '\u{fdd0}' } else { cell.ch };
            text.push(character);
            if text.len() > MAX_LOGICAL_PATH_BYTES {
                // When: text exceeds the logical byte cap, refuse before any candidate enumeration.
                return None;
            }
            scalars.push(PathScalar {
                byte_start,
                byte_end: text.len(),
                cell_start: index,
                cell_end: end,
                valid: !continuation && (!wide || paired),
            });
            index = end;
        }
        Some(Self { text, scalars })
    }

    fn candidates(
        &self,
        cells: &[&Cell],
        pointed: usize,
        style: PathStyle,
        include_bare_names: bool,
    ) -> Vec<(TargetMatch, std::ops::Range<usize>)> {
        let Some(col) = self
            .scalars
            .iter()
            .position(|scalar| (scalar.cell_start..scalar.cell_end).contains(&pointed))
        else {
            // When: no scalar contains pointed, it cannot own a scanner target.
            return Vec::new();
        };
        target_candidates_at_char_col_for_style(&self.text, col, style, include_bare_names)
            .into_iter()
            .filter_map(|matched| {
                let start =
                    self.scalars.iter().position(|scalar| scalar.byte_start == matched.start)?;
                let end =
                    self.scalars.iter().position(|scalar| scalar.byte_end == matched.end)? + 1;
                let source_start = self
                    .scalars
                    .iter()
                    .position(|scalar| scalar.byte_start == matched.source_start)?;
                let source_end =
                    self.scalars.iter().position(|scalar| scalar.byte_end == matched.source_end)?
                        + 1;
                if source_start > start || source_end < end || start >= end {
                    // When: source_start/source_end fail to enclose start..end, no identity can authorize the match.
                    return None;
                }
                for scalar in &self.scalars[source_start..source_end] {
                    if !scalar.valid
                        || cells[scalar.cell_start..scalar.cell_end].iter().any(|cell| {
                            cell.hyperlink().is_some()
                                || cell.extras().is_some_and(|extras| !extras.is_empty())
                                || cell.ch.is_control()
                        })
                    {
                        // When: scalar identity is unsafe, neither removed boundaries nor display text may be discarded to recover a target.
                        return None;
                    }
                }
                let range = self.scalars[start].cell_start..self.scalars[end - 1].cell_end;
                range.contains(&pointed).then_some((matched, range))
            })
            .collect()
    }
}

fn logical_path_scan_at_cell(
    grid: &Grid,
    view_top: u64,
    pointed: AbsoluteCell,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<LogicalPathScan> {
    let view_end = view_top.checked_add(u64::from(grid.rows))?;
    if pointed.row < view_top || pointed.row >= view_end || pointed.col >= grid.cols {
        // When: `pointed` lies outside the visible grid, no complete visible logical line owns it.
        return None;
    }

    let mut first_row = pointed.row;
    let mut row_count = 1usize;
    while grid.row_at_abs(first_row)?.soft_wrapped_from_previous() {
        if first_row == 0 || first_row == view_top || row_count == MAX_WRAPPED_PATH_ROWS {
            // When: `first_row` has no visible predecessor or `row_count` reached the cap, reject the partial chain.
            return None;
        }
        first_row -= 1;
        row_count += 1;
    }

    let mut last_row = pointed.row;
    while let Some(next_row_number) = last_row.checked_add(1) {
        let Some(next_row) = grid.row_at_abs(next_row_number) else {
            // When: `grid.row_at_abs(next_row_number)` is absent, the retained logical line ends at `last_row`.
            break;
        };
        if !next_row.soft_wrapped_from_previous() {
            // When: `next_row` has no incoming soft-wrap bit, its predecessor ended at a hard boundary.
            break;
        }
        if next_row_number >= view_end || row_count == MAX_WRAPPED_PATH_ROWS {
            // When: `next_row_number` is offscreen or `row_count` reached the cap, reject the partial chain.
            return None;
        }
        last_row = next_row_number;
        row_count += 1;
    }

    // On the alternate screen a multiplexer can draw several panes on one row. Scan only the
    // pointed pane, so a pane border ends every name as the grid's edge does.
    let alt_screen = grid.is_alt();
    if alt_screen && pane_border_at(grid, view_top, pointed.row, pointed.col) {
        // When: alt_screen and the pointed cell is itself a pane border, it belongs to no pane's text.
        return None;
    }
    let (pane_left, pane_right) = if alt_screen {
        pane_columns_at(grid, view_top, pointed.row, pointed.col)
    } else {
        // When: not alt_screen, no multiplexer draws panes, so the scan covers every column.
        (0, grid.cols)
    };
    if (pane_left, pane_right) != (0, grid.cols) && first_row != last_row {
        // When: pane_left..pane_right is narrower than the grid across first_row..=last_row, the chain is not one pane's text.
        return None;
    }
    let mut cells = Vec::new();
    let mut positions = Vec::new();
    let mut rows = SmallVec::<[PathRowIdentity; 2]>::new();
    for absolute_row in first_row..=last_row {
        let row = grid.row_at_abs(absolute_row)?;
        rows.push(PathRowIdentity { row: absolute_row, fingerprint: row_fingerprint(row) });
        for (column, cell) in row.iter().enumerate() {
            let column = u16::try_from(column).ok()?;
            if !(pane_left..pane_right).contains(&column) {
                // When: `column` lies outside pane_left..pane_right, its cell is a border or another pane's text.
                continue;
            }
            cells.push(cell);
            positions.push(AbsoluteCell { row: absolute_row, col: column });
        }
    }
    let pointed_index = positions.iter().position(|position| *position == pointed)?;
    let mapped = PathCellText::from_cells(&cells, &positions)?;
    let candidates = mapped
        .candidates(&cells, pointed_index, style, include_bare_names)
        .into_iter()
        .filter_map(|(matched, range)| {
            let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
            for position in &positions[range] {
                match spans.last_mut() {
                    Some(span) if span.row == position.row && span.end_col == position.col => {
                        span.end_col = position.col.checked_add(1)?;
                    }
                    Some(span) if span.end_col == grid.cols && position.col == 0 => {
                        spans.push(AbsoluteCellSpan {
                            row: position.row,
                            start_col: 0,
                            end_col: 1,
                        });
                    }
                    None => spans.push(AbsoluteCellSpan {
                        row: position.row,
                        start_col: position.col,
                        end_col: position.col.checked_add(1)?,
                    }),
                    Some(_) => {
                        // When: `spans.last_mut()` is `Some(_)` without adjacency, reject the discontinuous span map.
                        return None;
                    }
                }
            }
            Some(LogicalTargetCandidate {
                target: matched.target,
                missing_before: matched.missing_before,
                spans,
            })
        })
        .collect::<Vec<_>>();
    // A multiplexer may have cut the text at a pane edge. Then a shorter candidate is unproven too,
    // because the longer name it must rule out may continue on another row.
    if alt_screen
        && (spans_reach_cut_pane_edge(
            grid,
            view_top,
            &pointed_text_spans(&cells, &positions, pointed_index),
        ) || candidates
            .iter()
            .any(|candidate| spans_reach_cut_pane_edge(grid, view_top, &candidate.spans)))
    {
        // When: alt_screen and the pointed text or any candidate reaches a cut pane edge, refuse rather than offer a prefix.
        return None;
    }
    (!candidates.is_empty()).then_some(LogicalPathScan { candidates, rows })
}

#[cfg(test)]
fn row_target_candidates_at_cell(
    row: &Row,
    col: u16,
    style: PathStyle,
    include_bare_names: bool,
) -> Vec<RowTarget> {
    let cells = row.iter().collect::<Vec<_>>();
    let col = usize::from(col);
    if cells.get(col).is_none() {
        // When: `col` lies beyond the materialized row, no scanner span can own it.
        return Vec::new();
    }
    let positions =
        (0..cells.len()).map(|col| AbsoluteCell { row: 0, col: col as u16 }).collect::<Vec<_>>();
    let Some(mapped) = PathCellText::from_cells(&cells, &positions) else {
        // When: PathCellText::from_cells exceeds the logical bound, the row cannot authorize an incomplete candidate.
        return Vec::new();
    };
    mapped
        .candidates(&cells, col, style, include_bare_names)
        .into_iter()
        .filter_map(|(matched, range)| {
            Some(RowTarget {
                matched,
                start_col: u16::try_from(range.start).ok()?,
                end_col: u16::try_from(range.end).ok()?,
            })
        })
        .collect()
}

/// Return the first configured home variable that is valid for the native path grammar.
pub(super) fn native_home_dir() -> Option<PathBuf> {
    let variables = if cfg!(target_os = "windows") {
        // When: `target_os` is Windows, prefer `USERPROFILE` before validating fallback `HOME`.
        ["USERPROFILE", "HOME"]
    } else {
        // When: `target_os` is not Windows, prefer `HOME` before validating fallback `USERPROFILE`.
        ["HOME", "USERPROFILE"]
    };
    variables.into_iter().filter_map(std::env::var_os).find_map(|value| {
        let value = value.to_str()?;
        match PathStyle::native() {
            PathStyle::Posix => normalize_posix_cwd(value).map(PathBuf::from),
            PathStyle::Windows => normalize_windows_home(value).map(PathBuf::from),
        }
    })
}

pub(super) fn resolve_path_candidate(
    candidate: &str,
    style: PathStyle,
    cwd: Option<&Osc7Cwd>,
    home: Option<&Path>,
    local_hostname: &str,
) -> Option<PathBuf> {
    let home_relative = is_home_relative(candidate, style);
    let relative =
        is_explicit_relative(candidate, style) || is_contextual_relative(candidate, style);
    let combined = if home_relative {
        let home = home?.to_str()?;
        let suffix = &candidate[2..];
        match style {
            PathStyle::Posix => {
                let home = normalize_posix_cwd(home)?;
                format!("{}/{}", home.trim_end_matches('/'), suffix)
            }
            PathStyle::Windows => {
                let home = normalize_windows_home(home)?;
                format!("{}\\{}", home.trim_end_matches(['/', '\\']), suffix)
            }
        }
    } else if relative {
        // When: `candidate` is dot-relative or separator-relative, resolve it only from this pane's trusted local CWD.
        let cwd = cwd.filter(|cwd| authority_is_local(&cwd.authority, local_hostname))?;
        match style {
            PathStyle::Posix => format!("{}/{}", cwd.path.trim_end_matches('/'), candidate),
            PathStyle::Windows => {
                let cwd = normalize_windows_cwd(&cwd.path)?;
                format!("{}\\{}", cwd.trim_end_matches(['/', '\\']), candidate)
            }
        }
    } else {
        // When: `candidate` is absolute rather than home-relative or dot-relative, resolve it without contextual state.
        candidate.to_string()
    };
    match style {
        PathStyle::Posix => normalize_posix_cwd(&combined).map(PathBuf::from),
        PathStyle::Windows => normalize_windows_absolute(&combined).map(PathBuf::from),
    }
}

pub(super) fn resolve_detected_path(
    target: &DetectedTarget,
    style: PathStyle,
    cwd: Option<&Osc7Cwd>,
    home: Option<&Path>,
    local_hostname: &str,
) -> Option<PathBuf> {
    match target {
        DetectedTarget::Uri(_) => None,
        DetectedTarget::SourceReference(reference) => {
            let path = if reference.explicit_path {
                DetectedTarget::PathCandidate(reference.path.clone())
            } else {
                // When: reference.explicit_path is false, retain the bare filename's CWD-only provenance.
                DetectedTarget::BareName(reference.path.clone())
            };
            resolve_detected_path(&path, style, cwd, home, local_hostname)
        }
        DetectedTarget::PathCandidate(candidate) => {
            resolve_path_candidate(candidate, style, cwd, home, local_hostname)
        }
        DetectedTarget::BareName(candidate) => {
            // When: `target` is `BareName`, require one safe component and the exact pane's trusted local CWD.
            if candidate.is_empty()
                || candidate.len() > 4096
                || matches!(candidate.as_str(), "." | "..")
                || candidate.contains(['/', '\\'])
                || candidate.chars().any(char::is_control)
            {
                // When: `candidate` is empty, overlong, special, separated, or controlled, reject contextual resolution.
                return None;
            }
            let cwd = cwd.filter(|cwd| authority_is_local(&cwd.authority, local_hostname))?;
            match style {
                PathStyle::Posix => {
                    let cwd = normalize_posix_cwd(&cwd.path)?;
                    normalize_posix_absolute(&format!("{}/{candidate}", cwd.trim_end_matches('/')))
                        .map(PathBuf::from)
                }
                PathStyle::Windows => {
                    // When: `style` is Windows, apply native component restrictions before joining the trusted CWD.
                    if candidate.contains(':')
                        || candidate.ends_with(['.', ' '])
                        || candidate
                            .chars()
                            .any(|character| matches!(character, '<' | '>' | '"' | '|' | '?' | '*'))
                    {
                        // When: Windows `candidate` contains reserved, ADS, or normalization-sensitive syntax, leave it inert.
                        return None;
                    }
                    let cwd = normalize_windows_cwd(&cwd.path)?;
                    normalize_windows_absolute(&format!(
                        "{}\\{candidate}",
                        cwd.trim_end_matches(['/', '\\'])
                    ))
                    .map(PathBuf::from)
                }
            }
        }
    }
}

#[cfg(test)]
fn token_bounds(cells: &[&Cell], col: usize) -> (usize, usize) {
    let mut start = col;
    while start > 0 && !cell_delimiter(cells[start - 1]) {
        start -= 1;
    }
    let mut end = col + 1;
    while end < cells.len() && !cell_delimiter(cells[end]) {
        end += 1;
    }
    (start, end)
}

#[cfg(test)]
fn cell_delimiter(cell: &Cell) -> bool {
    if cell.flags.contains(CellFlags::WIDE_CONT) {
        // When: `cell` is a wide continuation, keep it attached to its lead cell instead of treating its stored space as a delimiter.
        return false;
    }
    cell.ch.is_whitespace()
        || cell.ch.is_control()
        || matches!(cell.ch, '"' | '\'' | '`' | '<' | '>')
}

fn row_fingerprint(row: &Row) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    row.hash(&mut hasher);
    hasher.finish()
}

fn unsafe_path_cell(cell: &&Cell) -> bool {
    cell.flags.intersects(CellFlags::WIDE | CellFlags::WIDE_CONT)
        || cell.extras().is_some_and(|extras| !extras.is_empty())
}

/// Resolve a bounded local OSC 7 snapshot to a native absolute launch directory without filesystem I/O.
pub(super) fn local_launch_cwd(
    cwd: &Osc7Cwd,
    style: PathStyle,
    local_hostname: &str,
) -> Option<PathBuf> {
    if cwd.path.len() > 4096
        || cwd.path.chars().any(char::is_control)
        || !authority_is_local(&cwd.authority, local_hostname)
    {
        // When: cwd is oversized, controlled, or remote, it must not redirect a new local shell.
        return None;
    }
    match style {
        PathStyle::Posix => normalize_posix_cwd(&cwd.path).map(PathBuf::from),
        PathStyle::Windows => {
            // When: style is Windows, normalize OSC 7 drive paths without treating a relative drive designator as absolute.
            if cwd.path.len() == 2 && cwd.path.as_bytes()[1] == b':' {
                // When: cwd.path is a bare drive designator, do not promote it to that drive's root.
                return None;
            }
            normalize_windows_cwd(&cwd.path).map(PathBuf::from)
        }
    }
}

fn authority_is_local(authority: &str, local_hostname: &str) -> bool {
    authority.is_empty()
        || authority.eq_ignore_ascii_case("localhost")
        || (!local_hostname.is_empty() && authority.eq_ignore_ascii_case(local_hostname))
}

fn is_home_relative(candidate: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => candidate.starts_with("~/"),
        PathStyle::Windows => candidate.starts_with("~/") || candidate.starts_with("~\\"),
    }
}

fn is_contextual_relative(candidate: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => !candidate.starts_with('/') && candidate.contains('/'),
        PathStyle::Windows => {
            let bytes = candidate.as_bytes();
            let drive_absolute = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\');
            !drive_absolute && candidate.contains(['/', '\\'])
        }
    }
}

fn is_explicit_relative(candidate: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => candidate.starts_with("./") || candidate.starts_with("../"),
        PathStyle::Windows => {
            candidate.starts_with("./")
                || candidate.starts_with(".\\")
                || candidate.starts_with("../")
                || candidate.starts_with("..\\")
        }
    }
}

fn normalize_posix_cwd(path: &str) -> Option<String> {
    if path == "/" {
        Some(path.to_string())
    } else {
        // When: `path` is not the POSIX root, require an ordinary named absolute CWD.
        normalize_posix_absolute(path)
    }
}

fn normalize_posix_absolute(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
        // When: `path` is not a single-root POSIX absolute path, reject relative, network, and cross-platform forms.
        return None;
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {
                // When: `component` is empty or `.`, omit the non-naming POSIX segment.
            }
            ".." => {
                // Pop one lexical ancestor while clamping traversal at the root.
                components.pop();
            }
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| format!("/{}", components.join("/")))
}

fn normalize_windows_home(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'/' | b'\\')
    {
        // When: `bytes` lack a drive-root separator, reject drive-relative home input instead of promoting `C:` to `C:\`.
        return None;
    }
    normalize_windows_absolute(path)
}

fn normalize_windows_cwd(path: &str) -> Option<String> {
    let normalized = path.replace('/', "\\");
    let normalized = if normalized.len() >= 4
        && normalized.starts_with('\\')
        && normalized.as_bytes()[1].is_ascii_alphabetic()
        && normalized.as_bytes()[2] == b':'
        && normalized.as_bytes()[3] == b'\\'
    {
        normalized[1..].to_string()
    } else {
        // When: `normalized` is not the OSC 7 `/C:/...` drive form, retain it for ordinary absolute validation.
        normalized
    };
    normalize_windows_absolute(&normalized)
}

fn normalize_windows_absolute(path: &str) -> Option<String> {
    if path.starts_with("\\\\") || path.starts_with("//") {
        // When: `path` starts with a double separator, reject unsupported UNC and network targets.
        return None;
    }
    let path = path.replace('/', "\\");
    let bytes = path.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        // When: `bytes` are exactly a drive designator, normalize the Windows root with its trailing separator.
        return Some(format!("{}:\\", bytes[0] as char));
    }
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        // When: `bytes` do not form a drive-rooted absolute path, reject relative and malformed Windows input.
        return None;
    }
    let drive = (bytes[0] as char).to_ascii_uppercase();
    let mut components = Vec::new();
    for component in path[3..].split('\\') {
        match component {
            "" | "." => {
                // When: `component` is empty or `.`, omit the non-naming Windows segment.
            }
            ".." => {
                components.pop();
            }
            value
                if value.contains(':')
                    || value.chars().any(|character| {
                        matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
                    }) =>
            {
                // When: `value` contains a reserved Windows path character, reject the component.
                return None;
            }
            value if value.ends_with(['.', ' ']) => {
                // When: `value` ends in a dot or space, reject Windows normalization aliases before probing.
                return None;
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        Some(format!("{drive}:\\"))
    } else {
        // When: `components` contains named segments, append them beneath the normalized drive root.
        Some(format!("{drive}:\\{}", components.join("\\")))
    }
}

fn detected_target_enabled(
    target: &DetectedTarget,
    clickable_local_targets: bool,
    clickable_bare_names: bool,
) -> bool {
    match target {
        DetectedTarget::Uri(_) => true,
        DetectedTarget::SourceReference(reference) => {
            clickable_local_targets && (reference.explicit_path || clickable_bare_names)
        }
        DetectedTarget::PathCandidate(_) => clickable_local_targets,
        DetectedTarget::BareName(_) => clickable_local_targets && clickable_bare_names,
    }
}

/// Vertical box-drawing glyphs that tmux, rmux and Zellij draw down the column between two panes.
///
/// A column counts as a pane edge only when two adjacent rows both draw one of these there.
const PANE_BORDER_GLYPHS: [char; 12] = [
    '\u{2502}', '\u{251C}', '\u{2524}', '\u{253C}', '\u{2503}', '\u{2523}', '\u{252B}', '\u{254B}',
    '\u{2551}', '\u{2560}', '\u{2563}', '\u{256C}',
];

fn is_pane_border(cell: Option<&Cell>) -> bool {
    cell.is_some_and(|cell| cell.hyperlink().is_none() && PANE_BORDER_GLYPHS.contains(&cell.ch))
}

/// Return the columns `[left, right)` of the pane holding columns `start..end` of two adjacent rows.
///
/// Pane edges are the grid's edges and the borders both rows draw in the same column. A border in
/// only one row is text, so it cannot narrow the pane.
fn shared_pane_columns(
    upper: &[&Cell],
    lower: &[&Cell],
    cols: u16,
    start: u16,
    end: u16,
) -> (u16, u16) {
    let shared = |column: &u16| {
        let index = usize::from(*column);
        is_pane_border(upper.get(index).copied()) && is_pane_border(lower.get(index).copied())
    };
    let left = (0..start).rev().find(shared).map_or(0, |border| border + 1);
    let right = (end..cols).find(shared).unwrap_or(cols);
    (left, right)
}

/// Report whether `cell` shows text: any visible cell except a blank or box drawing, which
/// multiplexers use for pane borders and rules. The trailing half of a wide character is text.
fn is_text_cell(cell: Option<&Cell>) -> bool {
    cell.is_some_and(|cell| {
        cell.flags.contains(CellFlags::WIDE_CONT)
            || (cell.ch != ' ' && !('\u{2500}'..='\u{257F}').contains(&cell.ch))
    })
}

/// Return the visible spans of the whitespace-free text run that holds the `pointed` cell.
///
/// The run stops at blank cells, box drawing and the ends of the logical line, so a pane border
/// ends it. `cells` and `positions` hold the scanned cells of one logical line, one per cell.
fn pointed_text_spans(
    cells: &[&Cell],
    positions: &[AbsoluteCell],
    pointed: usize,
) -> SmallVec<[AbsoluteCellSpan; 2]> {
    let mut spans = SmallVec::new();
    if !is_text_cell(cells.get(pointed).copied()) {
        // When: is_text_cell rejects the pointed cell, it is blank or a border and starts no run.
        return spans;
    }
    let mut start = pointed;
    while start > 0 && is_text_cell(cells.get(start - 1).copied()) {
        start -= 1;
    }
    let mut end = pointed + 1;
    while is_text_cell(cells.get(end).copied()) {
        end += 1;
    }
    for position in positions.get(start..end).unwrap_or_default() {
        match spans.last_mut() {
            Some(span) if span.row == position.row => {
                span.end_col = position.col.saturating_add(1);
            }
            _ => spans.push(AbsoluteCellSpan {
                row: position.row,
                start_col: position.col,
                end_col: position.col.saturating_add(1),
            }),
        }
    }
    spans
}

/// Report whether `column` of absolute `row` is a pane border: a vertical box-drawing glyph that
/// the visible row above or the row below also draws in the same column.
fn pane_border_at(grid: &Grid, view_top: u64, row: u64, column: u16) -> bool {
    let border_in = |row_number: u64| {
        is_pane_border(
            grid.row_at_abs(row_number).and_then(|found_row| found_row.get(usize::from(column))),
        )
    };
    border_in(row)
        && (row.checked_sub(1).filter(|above| *above >= view_top).is_some_and(border_in)
            || row.checked_add(1).is_some_and(border_in))
}

/// Return the columns `[left, right)` of the pane that holds `column` of absolute `row`: the cells
/// between the nearest pane borders on either side, or the grid's edges where there is none.
fn pane_columns_at(grid: &Grid, view_top: u64, row: u64, column: u16) -> (u16, u16) {
    let border = |edge_column: &u16| pane_border_at(grid, view_top, row, *edge_column);
    let left = (0..column).rev().find(border).map_or(0, |edge_column| edge_column + 1);
    let right = (column..grid.cols).find(border).unwrap_or(grid.cols);
    (left, right)
}

/// Report whether plain-text `spans` touch a pane edge that a multiplexer may have cut.
///
/// On the alternate screen a multiplexer positions every pane row with a cursor move, so text
/// that reaches its pane's right edge may continue on the next row, and text that starts at the
/// left edge under a row that filled the pane may be the rest of a longer target. Neither end can
/// be proven from the grid, so the caller refuses such text instead of offering a cut-off prefix.
fn spans_reach_cut_pane_edge(grid: &Grid, view_top: u64, spans: &[AbsoluteCellSpan]) -> bool {
    let cell_at = move |row: u64, column: u16| grid.row_at_abs(row)?.get(usize::from(column));
    let border = |row: u64, column: u16| pane_border_at(grid, view_top, row, column);
    let text = |row: u64, column: u16| is_text_cell(cell_at(row, column));
    let (Some(head), Some(tail)) = (spans.first(), spans.last()) else {
        // When: spans is empty, the target covers no cell and so touches no pane edge.
        return false;
    };
    let tail_pane_right =
        (tail.end_col..grid.cols).find(|column| border(tail.row, *column)).unwrap_or(grid.cols);
    let wrapped_below =
        grid.row_at_abs(tail.row + 1).is_some_and(|row| row.soft_wrapped_from_previous());
    let tail_cut =
        !wrapped_below && (tail.end_col..tail_pane_right).all(|column| text(tail.row, column));
    let starts_line = head.row > view_top
        && !grid.row_at_abs(head.row).is_some_and(|row| row.soft_wrapped_from_previous());
    let head_pane_left = (0..head.start_col)
        .rev()
        .find(|column| border(head.row, *column))
        .map_or(0, |column| column + 1);
    let head_pane_right =
        (head.end_col..grid.cols).find(|column| border(head.row, *column)).unwrap_or(grid.cols);
    let head_cut = starts_line
        && (head_pane_left..head.start_col).all(|column| text(head.row, column))
        && head
            .row
            .checked_sub(1)
            .zip(head_pane_right.checked_sub(1))
            .is_some_and(|(above, column)| text(above, column));
    tail_cut || head_cut
}

fn hyperlink_hover_cells(
    grid: &Grid,
    pane_id: u64,
    view_top: u64,
    pointed_row: u16,
    col: u16,
    hyperlink_id: sonicterm_types::HyperlinkId,
) -> Option<sonicterm_render_model::inputs::HoveredUrlCells> {
    use sonicterm_render_model::inputs::{HoveredUrlCells, HoveredUrlSpan, MAX_HOVERED_URL_SPANS};

    let cells_at = move |row_number: u16| {
        let row = grid.row_at_abs(view_top.checked_add(u64::from(row_number))?)?;
        Some((row, row.iter().collect::<Vec<_>>()))
    };
    let fragment = |row_number: u16, column: u16| {
        let (row, cells) = cells_at(row_number)?;
        let column = usize::from(column);
        if cells.get(column)?.hyperlink() != Some(hyperlink_id) {
            // When: the pointed column has another hyperlink identity, it cannot continue this occurrence.
            return None;
        }
        let mut start = column;
        let mut end = column + 1;
        while start > 0 && cells[start - 1].hyperlink() == Some(hyperlink_id) {
            start -= 1;
        }
        while end < cells.len() && cells[end].hyperlink() == Some(hyperlink_id) {
            end += 1;
        }
        Some((
            HoveredUrlSpan {
                row: row_number,
                start_col: u16::try_from(start).ok()?,
                end_col: u16::try_from(end).ok()?,
            },
            row.soft_wrapped_from_previous(),
        ))
    };
    // A multiplexer positions each pane row with a cursor move, so its rows record no soft wrap. On
    // the alternate screen a fragment that ends at its pane's right edge continues into the next
    // row's fragment when that one starts at the same pane's left edge. Activation opens the stored
    // destination, not joined text, so this changes only which cells are underlined.
    let pane_continuation = grid.is_alt();
    let pane_predecessor = |lower: HoveredUrlSpan| {
        let upper_row = lower.row.checked_sub(1)?;
        let (_, upper) = cells_at(upper_row)?;
        let (_, below) = cells_at(lower.row)?;
        let (left, right) =
            shared_pane_columns(&upper, &below, grid.cols, lower.start_col, lower.end_col);
        if lower.start_col != left {
            // When: lower.start_col is past its pane's left edge, the row above cannot continue into it.
            return None;
        }
        fragment(upper_row, right.checked_sub(1)?)
    };
    let pane_successor = |upper: HoveredUrlSpan| {
        let (_, above) = cells_at(upper.row)?;
        let (_, lower) = cells_at(upper.row + 1)?;
        let (left, right) =
            shared_pane_columns(&above, &lower, grid.cols, upper.start_col, upper.end_col);
        if upper.end_col != right {
            // When: upper.end_col stops before its pane's right edge, the row below cannot continue it.
            return None;
        }
        fragment(upper.row + 1, left)
    };
    if pointed_row >= grid.rows {
        // When: pointed_row is outside the viewport, never project retained scrollback as visible geometry.
        return None;
    }
    let (pointed, mut incoming_wrap) = fragment(pointed_row, col)?;
    let mut spans = Vec::with_capacity(MAX_HOVERED_URL_SPANS);
    spans.push(pointed);
    // Start at the pointer so clipping an overlong occurrence cannot discard its pointed fragment.
    while spans.len() < MAX_HOVERED_URL_SPANS {
        let first = spans[0];
        if first.row == 0 {
            // When: first.row is the viewport's top row, no visible row above can continue this occurrence.
            break;
        }
        let soft_wrapped = (first.start_col == 0 && incoming_wrap)
            .then(|| fragment(first.row - 1, grid.cols.checked_sub(1)?))
            .flatten();
        let predecessor =
            soft_wrapped.or_else(|| pane_continuation.then_some(first).and_then(pane_predecessor));
        let Some((previous, wrap)) = predecessor else {
            // When: predecessor is None, no soft wrap or shared pane edge links the row above to this occurrence.
            break;
        };
        spans.insert(0, previous);
        incoming_wrap = wrap;
    }
    while spans.len() < MAX_HOVERED_URL_SPANS {
        let last = *spans.last()?;
        if last.row + 1 >= grid.rows {
            // When: last.row is the viewport's final row, no visible row below can continue this occurrence.
            break;
        }
        let soft_wrapped = (last.end_col == grid.cols)
            .then(|| fragment(last.row + 1, 0))
            .flatten()
            .filter(|(_, wrap)| *wrap);
        let successor =
            soft_wrapped.or_else(|| pane_continuation.then_some(last).and_then(pane_successor));
        let Some((next, _)) = successor else {
            // When: successor is None, no soft wrap or shared pane edge links the row below to this occurrence.
            break;
        };
        spans.push(next);
    }
    HoveredUrlCells::new(pane_id, spans, false)
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
}

impl CellTargetSnapshot {
    fn explicit_path_text(&self) -> Option<String> {
        match &self.target {
            ResolvedCellTarget::Rejected(_) => Some(self.display.clone()),
            ResolvedCellTarget::Path(key) => key.link_destination.clone().or_else(|| {
                sonicterm_cfg::url_scan::explicit_path_feedback(
                    key.candidates.iter().map(|candidate| &candidate.target),
                    PathStyle::native(),
                )
                .map(str::to_owned)
            }),
            ResolvedCellTarget::Uri(_) => None,
        }
    }

    fn preview(
        &self,
        modifier_held: bool,
        pointer: (f32, f32),
    ) -> Option<sonicterm_render_model::inputs::LinkPreview> {
        let ResolvedCellTarget::Uri(uri) = &self.target else {
            // When: target is a path probe rather than Uri, keep filesystem authorization separate from URI previews.
            return None;
        };
        if !modifier_held {
            // When: modifier_held is false, leave ordinary terminal hover free of destination overlays.
            return None;
        }
        Some(sonicterm_render_model::inputs::LinkPreview {
            uri: uri.clone(),
            pointer,
            available: sonicterm_cfg::url_open::validate(uri).is_ok(),
        })
    }

    fn hovered(&self, active: bool) -> Option<super::hovered_url::HoveredUrl> {
        let mut cells = self.hover_cells?;
        cells.active = active;
        Some(super::hovered_url::HoveredUrl { cells, url: self.display.clone() })
    }
}

impl App {
    fn report_or_copy_path_failure(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        reason: &str,
        path: &str,
    ) {
        if self.copy_confirmed_failure(window_id, pane_id, path) {
            // When: copy_confirmed_failure consumes the confirmation, retain its clipboard result notification.
            return;
        }
        let reason = if reason.starts_with("path-error-") {
            self.i18n.translate(reason)
        } else {
            // When: reason has no path-error key prefix, retain the native diagnostic text.
            reason.to_owned()
        };
        self.report_failed_target(window_id, pane_id, &reason, path);
    }

    fn copy_confirmed_failure(&mut self, window_id: WindowId, pane_id: u64, target: &str) -> bool {
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: window_id has closed, no confirmation remains actionable.
            return false;
        };
        let Some(failed) = window.path_probe.failed_click.take() else {
            // When: no failed click is armed, this click must attempt the normal action.
            return false;
        };
        if failed.pane_id != pane_id
            || failed.target != target
            || !window.notification.as_ref().is_some_and(|bubble| {
                bubble.message == failed.message
                    && bubble.expires_at.is_some_and(|expiry| expiry > std::time::Instant::now())
            })
        {
            // When: target identity or the visible error differs, do not reuse a previous copy confirmation.
            return false;
        }
        let copied = self.set_clipboard_text(target.to_owned());
        let status = self.i18n.translate(if copied {
            "path-error-copied"
        } else {
            "path-error-copy-failed"
        });
        let prefix = self.i18n.translate("path-error-title");
        let reason = sonicterm_ui::overlays::link_preview_text(&failed.reason);
        let path = sonicterm_ui::overlays::link_preview_text(target);
        self.show_notification_for_kind(
            super::FrontmostKind::Child(window_id),
            sonicterm_ui::overlays::NotificationLevel::Error,
            format!("{status}\n{path}\n{prefix}: {reason}"),
        );
        true
    }

    pub(super) fn report_failed_target(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        reason: &str,
        target: &str,
    ) {
        if !self.windows.get(&window_id).is_some_and(|window| window.panes.contains_key(&pane_id)) {
            // When: window_id no longer owns pane_id, a late failure must not overwrite the clipboard.
            return;
        }
        let prefix = self.i18n.translate("path-error-title");
        let escaped_reason = sonicterm_ui::overlays::link_preview_text(reason);
        let escaped_target = sonicterm_ui::overlays::link_preview_text(target);
        let instruction = self.i18n.translate("path-error-copy-again");
        let message = format!("{instruction}\n{escaped_target}\n{prefix}: {escaped_reason}");
        self.show_notification_for_kind(
            super::FrontmostKind::Child(window_id),
            sonicterm_ui::overlays::NotificationLevel::Error,
            message.clone(),
        );
        self.windows.get_mut(&window_id).unwrap().path_probe.failed_click =
            Some(FailedTargetClick {
                pane_id,
                target: target.to_owned(),
                message,
                reason: reason.to_owned(),
            });
    }

    pub(super) fn cell_target_at(
        &self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
    ) -> Option<CellTargetSnapshot> {
        self.cell_target_lookup(window_id, pane_id, viewport_row, col).ok().flatten()
    }

    fn cell_target_lookup(
        &self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
    ) -> Result<Option<CellTargetSnapshot>, ()> {
        let Some(pane) = self.windows.get(&window_id).and_then(|window| window.panes.get(&pane_id))
        else {
            // When: the owning pane is gone, this is genuine absence rather than transient parser contention.
            return Ok(None);
        };
        let parser = pane.parser.try_lock().ok_or(())?;
        Ok(self.cell_target_from_parser(
            window_id,
            pane_id,
            viewport_row,
            col,
            &parser,
            pane.resolved_viewport(parser.grid()),
        ))
    }

    fn cell_target_from_parser(
        &self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
        parser: &sonicterm_vt::vt::Parser,
        viewport_top_abs: Option<u64>,
    ) -> Option<CellTargetSnapshot> {
        let grid = parser.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        let absolute_row = view_top.checked_add(u64::from(viewport_row))?;
        let row = grid.row_at_abs(absolute_row)?;
        let cell = row.iter().nth(usize::from(col))?;
        let rejected = |destination: &str, reason| {
            Some(CellTargetSnapshot {
                pane_id,
                hover_cells: None,
                display: destination.to_owned(),
                explicit_hyperlink: cell.hyperlink().is_some(),
                target: ResolvedCellTarget::Rejected(reason),
            })
        };
        let local_snapshot =
            |destination: &str,
             local: DetectedTarget,
             explicit_hyperlink: bool,
             hover_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>| {
                if !self.config.terminal.clickable_local_targets {
                    // When: clickable_local_targets is false, an OSC 8 wrapper cannot bypass local-target policy.
                    return rejected(destination, "path-error-disabled");
                }
                let cwd = parser.osc7_cwd().cloned();
                let resolved_path = resolve_detected_path(
                    &local,
                    PathStyle::native(),
                    cwd.as_ref(),
                    self.home_dir.as_deref(),
                    &self.local_hostname,
                )?;
                // Highlight capacity cannot revoke an independently stored OSC 8 destination.
                let probe_cells = hover_cells.or_else(|| {
                    sonicterm_render_model::inputs::HoveredUrlCells::single(
                        pane_id,
                        viewport_row,
                        col,
                        col.checked_add(1)?,
                        false,
                    )
                })?;
                let spans = probe_cells
                    .spans()
                    .iter()
                    .map(|span| AbsoluteCellSpan {
                        row: view_top + u64::from(span.row),
                        start_col: span.start_col,
                        end_col: span.end_col,
                    })
                    .collect::<SmallVec<[AbsoluteCellSpan; 2]>>();
                let rows = spans
                    .iter()
                    .map(|span| {
                        Some(PathRowIdentity {
                            row: span.row,
                            fingerprint: row_fingerprint(grid.row_at_abs(span.row)?),
                        })
                    })
                    .collect::<Option<SmallVec<[PathRowIdentity; 2]>>>()?;
                let key = PathProbeKey {
                    window_id,
                    pane_id,
                    pointed: AbsoluteCell { row: absolute_row, col },
                    view_top,
                    candidates: vec![PathProbeCandidate {
                        spans,
                        target: local,
                        resolved_path,
                        missing_before: Vec::new(),
                    }],
                    rows,
                    cwd,
                    cwd_revision: parser.cwd_revision(),
                    link_destination: Some(destination.to_owned()),
                    scrollback_evicted: grid.scrollback_evicted(),
                    screen_epoch: grid.screen_epoch(),
                    alt_screen: grid.is_alt(),
                };
                Some(CellTargetSnapshot {
                    pane_id,
                    hover_cells,
                    display: destination.to_owned(),
                    explicit_hyperlink,
                    target: ResolvedCellTarget::Path(key),
                })
            };
        if let Some(hyperlink_id) = cell.hyperlink() {
            // When: `cell` carries `hyperlink_id`, preserve its OSC 8 URI provenance instead of scanning the displayed text as a path.
            let uri = parser.hyperlinks().lookup(hyperlink_id)?.uri.clone();
            let local = match sonicterm_cfg::url_scan::local_link_target(&uri, PathStyle::native())
            {
                Ok(local) => local,
                Err(reason) => {
                    // When: local_link_target returns Err, preserve the rejection instead of falling back to URI dispatch.
                    return rejected(&uri, reason);
                }
            };
            if let Some(local) = local {
                // When: local_link_target yields a local destination, require the bounded filesystem probe before activation.
                return local_snapshot(
                    &uri,
                    local,
                    true,
                    hyperlink_hover_cells(grid, pane_id, view_top, viewport_row, col, hyperlink_id),
                );
            }
            return Some(CellTargetSnapshot {
                pane_id,
                hover_cells: hyperlink_hover_cells(
                    grid,
                    pane_id,
                    view_top,
                    viewport_row,
                    col,
                    hyperlink_id,
                ),
                display: uri.clone(),
                explicit_hyperlink: true,
                target: ResolvedCellTarget::Uri(uri),
            });
        }

        let style = PathStyle::native();
        let clickable_local_targets = self.config.terminal.clickable_local_targets;
        let clickable_bare_names = self.config.terminal.clickable_bare_names;
        let pointed = AbsoluteCell { row: absolute_row, col };
        // Bracketed URLs rebuild across hard rows only in a pane that spans the grid: in a split
        // pane the next grid row begins with another pane's text.
        let pane_spans_grid = !grid.is_alt()
            || pane_columns_at(grid, view_top, pointed.row, pointed.col) == (0, grid.cols);
        let logical = match pane_spans_grid.then(|| hardwrap_uri_at_cell(grid, view_top, pointed)) {
            Some(HardwrapUri::Complete(candidate)) => {
                LogicalPathScan { candidates: vec![candidate], rows: SmallVec::new() }
            }
            Some(HardwrapUri::Incomplete) => {
                // When: hardwrap_uri_at_cell is Incomplete, never fall back to its valid-looking prefix.
                return None;
            }
            Some(HardwrapUri::NotApplicable) | None => {
                logical_path_scan_at_cell(grid, view_top, pointed, style, clickable_bare_names)?
            }
        };
        if let Some(LogicalTargetCandidate { target: DetectedTarget::Uri(uri), spans, .. }) =
            logical.candidates.first()
        {
            // When: logical URI provenance wins, preserve its complete destination and every visible fragment.
            let spans = spans
                .iter()
                .map(|span| {
                    Some(sonicterm_render_model::inputs::HoveredUrlSpan {
                        row: u16::try_from(span.row.checked_sub(view_top)?).ok()?,
                        start_col: span.start_col,
                        end_col: span.end_col,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            let hover_cells =
                sonicterm_render_model::inputs::HoveredUrlCells::new(pane_id, spans, false)?;
            let local = match sonicterm_cfg::url_scan::local_link_target(uri, style) {
                Ok(local) => local,
                Err(reason) => {
                    // When: local_link_target rejects the complete URI, no fragment may fall back to navigation.
                    return rejected(uri, reason);
                }
            };
            if let Some(local) = local {
                // When: local is Some, keep full-span file URI activation behind filesystem authorization.
                return local_snapshot(uri, local, false, Some(hover_cells));
            }
            return Some(CellTargetSnapshot {
                pane_id,
                hover_cells: Some(hover_cells),
                display: uri.clone(),
                explicit_hyperlink: false,
                target: ResolvedCellTarget::Uri(uri.clone()),
            });
        }
        if !clickable_local_targets {
            // When: `clickable_local_targets` is false, leave non-URI text inert before resolving any candidate set.
            return None;
        }

        let cwd = parser.osc7_cwd().cloned();
        let mut candidates = logical
            .candidates
            .into_iter()
            .filter(|candidate| {
                detected_target_enabled(
                    &candidate.target,
                    clickable_local_targets,
                    clickable_bare_names,
                )
            })
            .filter_map(|candidate| {
                let resolved_path = resolve_detected_path(
                    &candidate.target,
                    style,
                    cwd.as_ref(),
                    self.home_dir.as_deref(),
                    &self.local_hostname,
                )?;
                let missing_before = candidate
                    .missing_before
                    .iter()
                    .map(|literal| {
                        resolve_detected_path(
                            literal,
                            style,
                            cwd.as_ref(),
                            self.home_dir.as_deref(),
                            &self.local_hostname,
                        )
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(PathProbeCandidate {
                    spans: candidate.spans,
                    target: candidate.target,
                    resolved_path,
                    missing_before,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .span_len()
                .cmp(&left.span_len())
                .then_with(|| left.spans.as_slice().cmp(right.spans.as_slice()))
        });
        candidates.dedup();
        let display = candidates.first()?.display().to_string();
        let key = PathProbeKey {
            window_id,
            pane_id,
            pointed,
            view_top,
            candidates,
            rows: logical.rows,
            cwd,
            cwd_revision: parser.cwd_revision(),
            link_destination: None,
            scrollback_evicted: grid.scrollback_evicted(),
            screen_epoch: grid.screen_epoch(),
            alt_screen: grid.is_alt(),
        };
        Some(CellTargetSnapshot {
            pane_id,
            hover_cells: None,
            display,
            explicit_hyperlink: false,
            target: ResolvedCellTarget::Path(key),
        })
    }

    fn pointer_target_cell(&self, window_id: WindowId) -> Option<(u64, u16, u16)> {
        let window = self.windows.get(&window_id)?;
        let (cursor_x_px, cursor_y_px) = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
        let (rendered_pane, row, col) =
            window.renderer.as_ref()?.pixel_to_pane_cell(cursor_x_px, cursor_y_px)?;
        let pane_id = if rendered_pane == 0 {
            // When: `rendered_pane` is zero, use `main_window_id` geometry for the main `window_id` and child geometry otherwise.
            if Some(window_id) == self.main_window_id {
                self.pane_at_cursor(cursor_x_px, cursor_y_px)?
            } else {
                super::pane_id_at_point(
                    &Self::compute_pane_rects_for(window),
                    cursor_x_px,
                    cursor_y_px,
                )?
            }
        } else {
            // When: `rendered_pane` is nonzero, retain the renderer-owned pane identity paired with `row` and `col`.
            rendered_pane
        };
        Some((pane_id, row, col))
    }

    /// Refreshes pointer feedback without treating a busy parser as a missing target.
    pub(super) fn refresh_target_hover(&mut self, window_id: WindowId) {
        let lookup = match self.pointer_target_cell(window_id) {
            Some((pane_id, row, col)) => self.cell_target_lookup(window_id, pane_id, row, col),
            None => Ok(None),
        };
        self.apply_target_lookup(window_id, lookup);
    }

    /// Resolves frame hover from the same held parser snapshots that will be presented.
    pub(super) fn refresh_target_hover_from_parsers<'a>(
        &mut self,
        window_id: WindowId,
        parsers: impl IntoIterator<Item = (u64, &'a sonicterm_vt::vt::Parser)>,
    ) {
        let target = self.pointer_target_cell(window_id).and_then(|(pane_id, row, col)| {
            let (_, parser) = parsers.into_iter().find(|(id, _)| *id == pane_id)?;
            let pane = self.windows.get(&window_id)?.panes.get(&pane_id)?;
            let viewport = pane.resolved_viewport(parser.grid());
            self.cell_target_from_parser(window_id, pane_id, row, col, parser, viewport)
        });
        self.apply_target_hover(window_id, target);
    }

    fn apply_target_lookup(
        &mut self,
        window_id: WindowId,
        lookup: Result<Option<CellTargetSnapshot>, ()>,
    ) {
        let Ok(target) = lookup else {
            // When: parser contention prevents a fresh lookup, preserve the hint without extending modifier-only feedback.
            if !self.open_modifier_held(window_id) {
                if let Some(window) = self.windows.get_mut(&window_id) {
                    let was_active =
                        window.hovered_url.as_ref().is_some_and(|hover| hover.active());
                    let had_preview = window.link_preview.take().is_some();
                    if let Some(hover) = window.hovered_url.as_mut() {
                        hover.cells.active = false;
                    }
                    if was_active || had_preview {
                        window.hover_link = false;
                        if let Some(native) = window.window.as_ref() {
                            native.set_cursor(winit::window::CursorIcon::Default);
                        }
                    }
                }
            }
            // A busy lookup needs a coherent frame so stationary feedback cannot wait for unrelated input.
            if let Some(window) = self.windows.get(&window_id) {
                window.request_redraw();
            }
            return;
        };
        self.apply_target_hover(window_id, target);
    }

    fn apply_target_hover(&mut self, window_id: WindowId, target: Option<CellTargetSnapshot>) {
        let modifier_held = self.open_modifier_held(window_id);
        let preview_allowed = self.frontmost_window == Some(window_id)
            && !(self.command_palette.is_open()
                && self.palette_attached_window.or(self.main_window_id) == Some(window_id));
        let mut probe_request = None;
        let mut hovered = None;
        let explicit_hyperlink = target.as_ref().is_some_and(|target| target.explicit_hyperlink);
        let mut visual_changed = false;

        if let Some(window) = self.windows.get_mut(&window_id) {
            let previous_hover = window.hovered_url.clone();
            let previous_link = window.hover_link;
            let show_preview = modifier_held
                && preview_allowed
                && !window.hidden
                && !window.mouse_down
                && window.splitter_drag.is_none()
                && window.scrollbar_drag.is_none()
                && window.drag_session.is_none();
            let pointer = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
            let mut preview =
                target.as_ref().and_then(|target| target.preview(show_preview, pointer));
            match target.as_ref() {
                Some(target @ CellTargetSnapshot { target: ResolvedCellTarget::Uri(_), .. }) => {
                    window.path_probe.invalidate();
                    hovered = target.hovered(modifier_held);
                }
                Some(target @ CellTargetSnapshot { target: ResolvedCellTarget::Path(key), .. }) => {
                    // Accept before request can advance the epoch; pending results never authorize clicks without a fresh key.
                    if let Some(result) = window.path_probe.pending_result.take() {
                        window.path_probe.accept(&result, Some(key));
                    }
                    probe_request = window.path_probe.request(key.clone());
                    if let Some(selection) =
                        window.path_probe.authorized_selection(key, modifier_held)
                    {
                        if show_preview {
                            let destination =
                                selection.candidate.resolved_path.to_string_lossy().into_owned();
                            preview = Some(sonicterm_render_model::inputs::LinkPreview {
                                uri: destination,
                                pointer,
                                available: true,
                            });
                        }
                        let cells =
                            selection.candidate.visible_cells(target.pane_id, key.view_top, true);
                        hovered = cells.map(|cells| super::hovered_url::HoveredUrl {
                            cells,
                            url: selection.candidate.display().to_string(),
                        });
                    }
                }
                Some(CellTargetSnapshot { target: ResolvedCellTarget::Rejected(_), .. }) => {
                    // Rejected local targets have no resolved destination to preview.
                    window.path_probe.invalidate();
                }
                None => {
                    window.path_probe.invalidate();
                }
            }
            let preview_changed = window.link_preview != preview;
            window.link_preview = preview;
            window.hovered_url = hovered;
            window.hover_link = window.hovered_url.as_ref().is_some_and(|hover| hover.active())
                || explicit_hyperlink;
            visual_changed = previous_hover != window.hovered_url
                || previous_link != window.hover_link
                || preview_changed;
        }

        if let Some(request) = probe_request {
            if let Some(workers) = &self.path_workers {
                if let Err(error) = workers.probe(request) {
                    tracing::warn!(%error, "path openability probe unavailable");
                }
            }
        }
        if let Some(window) = self.windows.get(&window_id) {
            if visual_changed {
                if let Some(native) = window.window.as_ref() {
                    native.set_cursor(if window.hover_link {
                        winit::window::CursorIcon::Pointer
                    } else {
                        winit::window::CursorIcon::Default
                    });
                }
                window.request_redraw();
            }
        }
    }

    pub(super) fn clear_target_hover(&mut self, window_id: WindowId) {
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: `window_id` no longer identifies a live window, there is no hover or probe state left to clear.
            return;
        };
        let probe_changed = window.path_probe.invalidate();
        let hover_changed = window.hovered_url.take().is_some();
        let link_changed = window.hover_link;
        let changed =
            probe_changed | hover_changed | link_changed | window.link_preview.take().is_some();
        window.hover_link = false;
        if changed {
            if let Some(native) = window.window.as_ref() {
                native.set_cursor(winit::window::CursorIcon::Default);
            }
            window.request_redraw();
        }
    }

    /// Retains a current probe completion for validation against the next fresh hover snapshot.
    pub(super) fn handle_path_probe_finished(&mut self, result: PathProbeResult) {
        let window_id = result.request.key.window_id;
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: windows no longer contains window_id, discard the result with its original owner.
            return;
        };
        if result.request.epoch != window.path_probe.epoch
            || window.path_probe.current.as_ref() != Some(&result.request.key)
        {
            // When: result differs from the current epoch or key, it must not replace a newer pending completion.
            return;
        }
        window.path_probe.pending_result = Some(result);
        window.request_redraw();
    }

    pub(super) fn open_modifier_held(&self, window_id: WindowId) -> bool {
        let modifiers =
            self.windows.get(&window_id).map(|window| window.modifiers).unwrap_or_default();
        if cfg!(target_os = "macos") {
            // When: `target_os` is macOS, Cmd is the platform-native target activation modifier.
            modifiers.super_key()
        } else {
            // When: `target_os` is not macOS, Ctrl is the platform-native target activation modifier.
            modifiers.control_key()
        }
    }

    pub(super) fn activate_target_at(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
    ) -> bool {
        let modifier_held = self.open_modifier_held(window_id);
        if !modifier_held {
            // When: `modifier_held` is false at click time, never activate a target authorized by earlier hover state.
            return false;
        }
        let Some(target) = self.cell_target_at(window_id, pane_id, viewport_row, col) else {
            // When: the clicked pane cell has no current `target`, leave the click available to ordinary selection handling.
            return false;
        };
        let explicit_path = target.explicit_path_text();
        match target.target {
            ResolvedCellTarget::Rejected(reason) => {
                // Rejected targets retain explicit filepath provenance but never dispatch a native action.
                self.report_or_copy_path_failure(window_id, pane_id, reason, &target.display);
                true
            }
            ResolvedCellTarget::Uri(uri) => {
                // When: target is Uri, retain URI navigation independently of filesystem authorization.
                if self.copy_confirmed_failure(window_id, pane_id, &uri) {
                    // When: copy_confirmed_failure consumes the second click, skip another URI navigation attempt.
                    return true;
                }
                if let Err(error) = sonicterm_cfg::url_open::open(&uri) {
                    tracing::warn!(%error, "URL open failed");
                    self.report_failed_target(window_id, pane_id, &error.to_string(), &uri);
                }
                true
            }
            ResolvedCellTarget::Path(key) => {
                // When: `target.target` is `Path`, activation requires the current typed probe result and bounded opener.
                let Some(selection) = self.windows.get(&window_id).and_then(|window| {
                    window.path_probe.authorized_selection(&key, modifier_held).cloned()
                }) else {
                    // When: authorized_selection is absent, only explicit path syntax permits failure feedback.
                    let Some(path) = explicit_path else {
                        // When: explicit_path is absent, guessed bare names remain ordinary terminal text.
                        return false;
                    };
                    let reason = self
                        .windows
                        .get(&window_id)
                        .and_then(|window| window.path_probe.failure_for(&key))
                        .unwrap_or("path-error-pending");
                    tracing::debug!(
                        window_id = ?key.window_id,
                        pane_id = key.pane_id,
                        pointed = ?key.pointed,
                        view_top = key.view_top,
                        screen_epoch = key.screen_epoch,
                        scrollback_evicted = key.scrollback_evicted,
                        cwd_revision = key.cwd_revision,
                        cwd = ?key.cwd,
                        clicked_path = ?path,
                        candidates = ?key.candidates,
                        reason,
                        "local path activation unverified"
                    );
                    self.report_or_copy_path_failure(window_id, pane_id, reason, &path);
                    return true;
                };
                if selection.candidate.visible_cells(pane_id, key.view_top, true).is_none() {
                    // When: visible_cells returns None, do not activate a path without a drawable underline.
                    return false;
                }
                let copy = selection.candidate.resolved_path.to_string_lossy().into_owned();
                if self.copy_confirmed_failure(window_id, pane_id, &copy) {
                    // When: copy_confirmed_failure consumes this path's second click, do not enqueue another open.
                    return true;
                }
                let Some(workers) = &self.path_workers else {
                    // When: path_workers is absent, report that the requested operation cannot be queued.
                    self.report_failed_target(
                        window_id,
                        pane_id,
                        &self.i18n.translate("path-error-worker"),
                        &copy,
                    );
                    return true;
                };
                match workers.open(
                    window_id,
                    pane_id,
                    selection.candidate.resolved_path,
                    selection.decision,
                    selection.candidate.missing_before,
                ) {
                    Ok(true) => true,
                    Ok(false) => {
                        tracing::warn!("path open queue full; request dropped");
                        self.report_failed_target(
                            window_id,
                            pane_id,
                            &self.i18n.translate("path-error-busy"),
                            &copy,
                        );
                        true
                    }
                    Err(error) => {
                        tracing::warn!(%error, "path open unavailable");
                        self.report_failed_target(window_id, pane_id, &error.to_string(), &copy);
                        true
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "path_target_tests.rs"]
mod path_target_tests;
