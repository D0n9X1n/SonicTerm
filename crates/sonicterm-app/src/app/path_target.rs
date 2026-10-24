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

impl PathProbeState {
    /// The probe under the pointer for the status line, or `None` when no
    /// local path is under it. Reads retained state only; it never probes.
    pub(crate) fn status(&self) -> Option<ProbeStatus<'_>> {
        let key = self.current.as_ref()?;
        let candidate = key.candidates.iter().min_by_key(|candidate| candidate.char_len())?;
        let path = candidate.resolved_path.as_path();
        let bare = matches!(candidate.target, DetectedTarget::BareName(_));
        if bare && key.cwd.is_none() {
            // When: a bare name has no OSC 7 directory, it can never resolve.
            return Some(ProbeStatus::NoDirectory(path));
        }
        if bare && key.alt_screen {
            // When: bare names are not resolved on the alternate screen, say so.
            return Some(ProbeStatus::AltScreen(path));
        }
        Some(match (&self.selection, self.failure) {
            (Some(selection), _) => ProbeStatus::Selected(
                selection.candidate.resolved_path.as_path(),
                selection.decision,
            ),
            (None, Some(reason)) => ProbeStatus::Failed(path, reason),
            (None, None) => ProbeStatus::Pending(path),
        })
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

    /// Whether `key` is the current probe key, for which [`Self::request`] would do nothing.
    pub(super) fn is_current(&self, key: &PathProbeKey) -> bool {
        self.current.as_ref() == Some(key)
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
            // When: `key` adds no unprobed equal-or-shorter contender, retain the selected span under the same row identity.
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

    /// Accept a finished result that no hover has applied yet against `fresh`, the key a click
    /// computed for its own cell, so a click right after the check finishes is not reported as
    /// pending. A result for any other key revokes itself, as it does on the next hover.
    pub(super) fn accept_waiting_for(&mut self, fresh: &PathProbeKey) {
        if let Some(result) = self.pending_result.take() {
            self.accept(&result, Some(fresh));
        }
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
        // Only a candidate that could be chosen ahead of the winner can change it.
        && destination
            .candidates
            .iter()
            .filter(|candidate| contends_with(candidate, selected))
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
    /// `pending.len()`, published after every change so the status line reads it without the lock.
    waiting: Arc<std::sync::atomic::AtomicUsize>,
}

impl PathProbeQueue {
    fn submit(&mut self, request: PathProbeRequest) {
        let window_id = request.key.window_id;
        if self.pending.insert(window_id, request).is_none() {
            // A replacement keeps the window's turn; only a newly waiting window joins the back.
            self.order.push_back(window_id);
        }
        self.publish_waiting();
    }

    /// Publish how many requests wait, for `path_check`.
    // Ordering: waiting is stored Relaxed; the reader needs a recent count, not a happens-before.
    fn publish_waiting(&self) {
        self.waiting.store(self.pending.len(), std::sync::atomic::Ordering::Relaxed);
    }

    fn take_next(&mut self) -> Option<PathProbeRequest> {
        while let Some(window_id) = self.order.pop_front() {
            if let Some(request) = self.pending.remove(&window_id) {
                // When: `pending` holds `window_id`'s newest request, serve it before
                // any later-queued window.
                self.publish_waiting();
                return Some(request);
            }
        }
        None
    }

    fn cancel_window(&mut self, window_id: WindowId) -> bool {
        self.order.retain(|queued| *queued != window_id);
        let removed = self.pending.remove(&window_id).is_some();
        self.publish_waiting();
        removed
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
    /// The queue's published waiting count.
    waiting: Arc<std::sync::atomic::AtomicUsize>,
}

impl PathProbeMailbox {
    pub(super) fn new() -> (Self, Receiver<()>) {
        let (wake, receiver) = crossbeam_channel::bounded(1);
        let queue = PathProbeQueue::default();
        let waiting = Arc::clone(&queue.waiting);
        (Self { queue: Arc::new(Mutex::new(queue)), wake, waiting }, receiver)
    }

    /// Requests waiting for the worker, without taking the queue lock.
    // Ordering: waiting is loaded Relaxed, pairing with publish_waiting's Relaxed store.
    pub(super) fn waiting(&self) -> usize {
        self.waiting.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Queue `request` behind other windows' waiting probes, replacing only its own window's.
    pub(super) fn submit(&self, request: PathProbeRequest) -> Result<(), TrySendError<()>> {
        let window_id = request.key.window_id;
        lock_probe_queue(&self.queue).submit(request);
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(error @ TrySendError::Disconnected(())) => {
                // A stopped worker never takes the request, so it must not count as waiting.
                lock_probe_queue(&self.queue).cancel_window(window_id);
                Err(error)
            }
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
    busy: Option<&super::jobs::BusySlot>,
) {
    while wake.recv().is_ok() {
        while let Some(request) = take_next_probe(queue) {
            let outcome = {
                // Execution only, never queue wait, counts toward `path_check`'s age.
                let _busy = busy.map(super::jobs::BusySlot::enter);
                probe_candidates(&request.key.candidates, &mut classify)
            };
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

/// Choose the shortest existing candidate.
///
/// Candidates are probed in [`PathProbeCandidate::probe_order`]. A tier whose
/// candidates are all missing is skipped. In the first tier with any present
/// candidate, a blocked one ends the probe so no longer path wins past it;
/// otherwise the earliest actionable candidate is selected.
fn probe_tiers(
    candidates: &[PathProbeCandidate],
    mut classify: impl FnMut(&Path) -> PathOpenDecision,
) -> Result<PathProbeSelection, &'static str> {
    let mut ordered = candidates.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.probe_order().cmp(&right.probe_order()));
    for tier in ordered.chunk_by(|left, right| left.char_len() == right.char_len()) {
        let mut selected = None;
        let mut blocked = false;
        for candidate in tier {
            let decision = if matches!(candidate.target, DetectedTarget::SourceReference(_)) {
                classify_source_reference(&candidate.resolved_path)
            } else {
                // When: matches! rejects SourceReference provenance, keep generic opener classification unchanged.
                classify(&candidate.resolved_path)
            };
            match decision {
                PathOpenDecision::Blocked => blocked = true,
                PathOpenDecision::Missing => {
                    // When: `classify` returns `Missing`, this candidate names no file and cannot win.
                }
                decision => {
                    // The tier is in start order, so the first actionable candidate wins the tie.
                    if selected.is_none() && decision.is_actionable() {
                        selected =
                            Some(PathProbeSelection { candidate: (*candidate).clone(), decision });
                    }
                }
            }
        }
        if blocked {
            // When: `blocked`, a candidate no longer than every present one is refused, so no longer path wins past it.
            return Err("path-error-blocked");
        }
        if let Some(selection) = selected {
            // When: `selected` holds this tier's actionable candidate, it is the shortest existing path.
            return Ok(selection);
        }
    }
    Err("path-error-missing")
}

/// Probe a key's candidates. Joined candidates are probed first; the row's own fragments are
/// probed only when every joined candidate is definitely missing, so a blocked or actionable
/// joined path never falls back to a fragment.
fn probe_candidates(
    candidates: &[PathProbeCandidate],
    mut classify: impl FnMut(&Path) -> PathOpenDecision,
) -> Result<PathProbeSelection, &'static str> {
    let (joined, fragments): (Vec<_>, Vec<_>) =
        candidates.iter().cloned().partition(|candidate| candidate.joined);
    if !joined.is_empty() {
        // When: `joined` is non-empty, the joined candidates are probed before any fragment.
        match probe_tiers(&joined, &mut classify) {
            Err("path-error-missing") => {
                // When: every joined candidate is missing, the row's own fragments may decide.
            }
            decided => {
                // When: `decided` is a selection or a blocked refusal, it stands with no fallback.
                return decided;
            }
        }
    }
    probe_tiers(&fragments, classify)
}

/// Whether `candidate` could be chosen ahead of `selected`. A joined candidate is probed before
/// every fragment, so it contends with a fragment of any length; within one group the shorter wins.
fn contends_with(candidate: &PathProbeCandidate, selected: &PathProbeCandidate) -> bool {
    match (candidate.joined, selected.joined) {
        (true, false) => true,
        (false, true) => false,
        _ => candidate.char_len() <= selected.char_len(),
    }
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

/// Report whether repeating the hover-time selection over `request.contenders` still picks
/// `request.path` with the same decision, so a file that appeared or became blocked since the
/// hover is never skipped in favor of a longer path.
fn open_request_still_selected(
    request: &PathOpenRequest,
    classify: impl FnMut(&Path) -> PathOpenDecision,
) -> bool {
    probe_candidates(&request.contenders, classify).is_ok_and(|selection| {
        selection.candidate.resolved_path == request.path
            && selection.decision == request.expected_decision
    })
}

/// App-owned handles for the bounded path probe and open workers.
pub(crate) struct PathWorkers {
    probe: PathProbeMailbox,
    open: Sender<PathOpenRequest>,
    /// Each worker's busy slot, stamped around its blocking call.
    busy: Arc<super::jobs::PathBusy>,
}

impl PathWorkers {
    /// Start one fair per-window openability worker and one serialized target-open worker.
    pub(super) fn start(
        proxy: winit::event_loop::EventLoopProxy<super::UserEvent>,
    ) -> io::Result<Self> {
        let (probe, wake) = PathProbeMailbox::new();
        let busy = Arc::new(super::jobs::PathBusy::new());
        let (probe_busy, open_busy) = (Arc::clone(&busy), Arc::clone(&busy));
        // The worker owns no wake sender, so dropping the App's mailbox ends it.
        let queue = Arc::clone(&probe.queue);
        let probe_proxy = proxy.clone();
        std::thread::Builder::new()
            .name("sonicterm-path-probe".into())
            .spawn(move || {
                let deliver = |result: PathProbeResult| {
                    probe_proxy
                        .send_event(super::UserEvent::PathProbeFinished(Box::new(result)))
                        .is_ok()
                };
                run_probe_worker(
                    &wake,
                    &queue,
                    classify_local_target,
                    deliver,
                    Some(&probe_busy.probe),
                );
            })
            .map_err(|error| io::Error::other(format!("spawn path probe worker: {error}")))?;

        let (open, open_rx) = crossbeam_channel::bounded::<PathOpenRequest>(1);
        std::thread::Builder::new()
            .name("sonicterm-path-open".into())
            .spawn(move || {
                while let Ok(request) = open_rx.recv() {
                    // The re-probe and the native open or reveal are this request's execution.
                    let _busy = open_busy.open.enter();
                    let result = if open_request_still_selected(&request, classify_local_target) {
                        open_path(&request.path, request.expected_decision)
                    } else {
                        // When: `open_request_still_selected` fails, a contender changed since hover; never open the stale choice.
                        Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "a shorter or earlier path changed since hover",
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

        Ok(Self { probe, open, busy })
    }

    /// Path checks waiting for a worker: queued probes plus a queued open.
    pub(super) fn waiting(&self) -> usize {
        self.probe.waiting() + self.open.len()
    }

    /// The workers' busy slots, which the status line's `path_check` reads.
    pub(super) fn busy(&self) -> &super::jobs::PathBusy {
        &self.busy
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
        contenders: Vec<PathProbeCandidate>,
    ) -> io::Result<bool> {
        match self.open.try_send(PathOpenRequest {
            window_id,
            pane_id,
            path,
            expected_decision,
            contenders,
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

#[cfg(test)]
fn hardwrap_uri_at_cell(grid: &Grid, view_top: u64, pointed: AbsoluteCell) -> HardwrapUri {
    hardwrap_uri_in(&FrameRows::new(grid, view_top), pointed)
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

impl<'grid> HardwrapRows<'grid> {
    /// The view of absolute `row_number`; `None` when the frame does not display it.
    fn view(
        &mut self,
        frame: &FrameRows<'grid>,
        row_number: u64,
    ) -> Option<&HardwrapRowView<'grid>> {
        self.views
            .entry(row_number)
            .or_insert_with(|| {
                let row = frame.row(row_number)?;
                let cells = row.iter().collect::<Vec<_>>();
                #[cfg(test)]
                hardwrap_work::add(cells.len());
                let edge = cells.len().min(usize::from(frame.grid().cols));
                let last_break = cells[..edge].iter().rposition(|cell| hardwrap_body_break(cell));
                Some(HardwrapRowView { row, cells, last_break })
            })
            .as_ref()
    }
}

/// Whether `cell` cannot be part of a hard-wrapped URI body: unsafe, hyperlinked, non-ASCII,
/// blank, control, or a wrapper. Every wrapper is itself a break.
fn hardwrap_body_break(cell: &Cell) -> bool {
    unsafe_path_cell(&cell)
        || cell.hyperlink().is_some()
        || !cell.ch.is_ascii()
        || cell.ch.is_whitespace()
        || cell.ch.is_control()
        || matches!(cell.ch, '(' | ')' | '[' | ']')
}

fn hardwrap_uri_in(frame: &FrameRows<'_>, pointed: AbsoluteCell) -> HardwrapUri {
    let first =
        pointed.row.saturating_sub((MAX_WRAPPED_PATH_ROWS - 1) as u64).max(frame.view_top());
    let mut rows = HardwrapRows { views: HashMap::new() };
    for row_number in (first..=pointed.row).rev() {
        let Some(view) = rows.view(frame, row_number) else {
            // When: the frame has no row_number, that row cannot open a visible candidate.
            continue;
        };
        let soft_wrapped = view.row.soft_wrapped_from_previous();
        // A chain's first row must reach the grid's edge with no closer and no break in its body.
        // Every wrapper is a break, so only a wrapper at the row's last break can open such a body;
        // an earlier wrapper's body holds that break and its candidate is never applicable.
        let opener = view.last_break.and_then(|index| {
            let closer = match view.cells[index].ch {
                '(' => ')',
                '[' => ']',
                _ => {
                    // When: view.cells[index].ch at last_break opens no supported wrapper, no wrapper owns a body.
                    return None;
                }
            };
            opens_http_scheme(&view.cells, index + 1).then_some((index + 1, closer))
        });
        // The frame's last row may wrap into a row below it that is not displayed, so on a
        // scrolled-back view that boundary is unknown and the logical scanner, which fails closed
        // there, owns it.
        let next_row_number = row_number + 1;
        let wraps_below = if frame.contains(next_row_number) {
            rows.view(frame, next_row_number)
                .is_some_and(|next| next.row.soft_wrapped_from_previous())
        } else {
            // When: the next row is outside the frame, only a scrolled-back view has one at all.
            frame.rows_below()
        };
        if soft_wrapped || wraps_below {
            // When: soft_wrapped or wraps_below holds, or may hold, the logical scanner owns that boundary.
            continue;
        }
        let Some((start, closer)) = opener else {
            // When: opener is None, no wrapper opens an http(s) body at last_break, so no chain starts here.
            continue;
        };
        let found = hardwrap_uri_candidate(frame, &mut rows, pointed, row_number, start, closer);
        if !matches!(found, HardwrapUri::NotApplicable) {
            // When: matches! excludes NotApplicable for found, the nearest owning wrapper decides pointed.
            return found;
        }
    }
    HardwrapUri::NotApplicable
}

/// Whether `cells` from column `start` spell `https://` or `http://`, reading at most eight cells.
fn opens_http_scheme(cells: &[&Cell], start: usize) -> bool {
    let spells = |prefix: &str| {
        let mut found = cells.get(start..).unwrap_or_default().iter().map(|cell| cell.ch);
        prefix.chars().all(|expected| found.next() == Some(expected))
    };
    spells("https://") || spells("http://")
}

fn hardwrap_uri_candidate<'grid>(
    frame: &FrameRows<'grid>,
    rows: &mut HardwrapRows<'grid>,
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
        // When: a span contains pointed and spans_join_across_pane_border is false, suppress that fragment's truncated prefix.
        if spans.iter().any(|span| span.contains(pointed))
            && !spans_join_across_pane_border(frame, spans)
        {
            HardwrapUri::Incomplete
        } else {
            HardwrapUri::NotApplicable
        }
    };
    for offset in 0..MAX_WRAPPED_PATH_ROWS {
        let row_number = first_row + offset as u64;
        // When: row_number reaches view_end, the chain leaves the viewport and cannot be proven complete.
        if row_number >= frame.view_end() {
            return refusal(&spans);
        }
        let Some(view) = rows.view(frame, row_number) else {
            // When: the frame has no row_number, the chain cannot be continued or proven.
            return refusal(&spans);
        };
        if view.row.soft_wrapped_from_previous() {
            // When: soft_wrapped_from_previous holds, the wrap-aware scanner owns this boundary.
            return refusal(&spans);
        }
        let cells = &view.cells;
        // Each chain row's closer, indent and body scans walk at most its cells once.
        #[cfg(test)]
        hardwrap_work::add(cells.len());
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
        let end = close.unwrap_or(usize::from(frame.grid().cols));
        let Some(body) = cells.get(start..end).filter(|body| !body.is_empty()) else {
            // When: cells hold no non-empty body between start and end, this row adds no fragment.
            return refusal(&spans);
        };
        if body.iter().any(|cell| hardwrap_body_break(cell)) {
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
            if spans_join_across_pane_border(frame, &spans) {
                // When: spans_join_across_pane_border holds, the chain runs into the next pane's text.
                return HardwrapUri::NotApplicable;
            }
            return HardwrapUri::Complete(LogicalTargetCandidate {
                target: DetectedTarget::Uri(found.url.clone()),
                spans,
                joined: false,
            });
        }
    }
    refusal(&spans)
}

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

impl HardwrapRowToken {
    /// Whether the token fills its row to the pane's right edge, within the measured margin.
    fn reaches_edge(self, right: usize) -> bool {
        !self.trailing_text
            && right.saturating_sub(self.token_end) <= usize::from(HARDWRAP_EDGE_MARGIN)
    }
}

/// One row of a prospective chain read inside its pane: the token, its text, and whether the
/// terminal recorded a wrap into the row.
struct HardwrapPaneRow {
    token: HardwrapRowToken,
    text: String,
    soft_wrapped: bool,
}

/// Whether `cell` is empty: a space or a never-written cell, but not a wide character's
/// continuation, which is part of the text before it.
fn hardwrap_blank(cell: &Cell) -> bool {
    !cell.flags.contains(CellFlags::WIDE_CONT) && matches!(cell.ch, ' ' | '\0')
}

/// The column just after one bullet marker starting at `column`, or `column` when none does.
/// Wide-cell continuations inside a marker are skipped, so a two-cell `⏺` still matches.
fn after_bullet(cells: &[&Cell], column: usize, right: usize) -> usize {
    let after_marker = |marker: &str| {
        let mut at = column;
        for expected in marker.chars() {
            if at >= right || cells.get(at).is_none_or(|cell| cell.ch != expected) {
                // When: the cell at `at` is not `expected`, this marker does not start here.
                return None;
            }
            at += 1;
            while at < right
                && cells.get(at).is_some_and(|cell| cell.flags.contains(CellFlags::WIDE_CONT))
            {
                at += 1;
            }
        }
        Some(at)
    };
    if let Some(after) = HARDWRAP_BULLETS.iter().find_map(|marker| after_marker(marker)) {
        // When: a listed bullet starts the content, the token begins directly after it.
        return after;
    }
    let digits = (column..right)
        .take_while(|at| cells.get(*at).is_some_and(|cell| cell.ch.is_ascii_digit()))
        .count();
    let numbered = (1..=3).contains(&digits)
        && cells.get(column + digits).is_some_and(|cell| cell.ch == '.')
        && cells.get(column + digits + 1).is_some_and(|cell| cell.ch == ' ');
    if !numbered {
        // When: `numbered` is false, no `1. ` marker starts the content, so the token begins at `column`.
        return column;
    }
    column + digits + 2
}

/// The token of one row inside the pane `[left, right)`: its first text, where the token starts
/// (after one bullet when `bullet` allows it), where it ends, and whether more text follows it.
/// `None` when the row's pane segment holds no text.
fn hardwrap_row_token(
    cells: &[&Cell],
    left: usize,
    right: usize,
    bullet: bool,
) -> Option<HardwrapRowToken> {
    let first_text = (left..right)
        .find(|column| cells.get(*column).is_some_and(|cell| !hardwrap_blank(cell)))?;
    let token_start = if bullet {
        after_bullet(cells, first_text, right)
    } else {
        // When: `bullet` is false, a continuation row's token starts at its first text.
        first_text
    };
    let token_end = (token_start..right)
        .find(|column| cells.get(*column).is_none_or(|cell| hardwrap_body_break(cell)))
        .unwrap_or(right);
    let trailing_text = (token_end..right)
        .any(|column| cells.get(column).is_some_and(|cell| !hardwrap_blank(cell)));
    Some(HardwrapRowToken { first_text, token_start, token_end, trailing_text })
}

/// Read absolute `row_number` inside `pane`, probing the pane's bounds at column `probe`.
/// `None` when the row is not displayed, its pane at `probe` has other bounds, or it holds no text.
fn hardwrap_pane_row<'grid>(
    frame: &FrameRows<'grid>,
    rows: &mut HardwrapRows<'grid>,
    row_number: u64,
    pane: (u16, u16),
    probe: u16,
    bullet: bool,
) -> Option<HardwrapPaneRow> {
    if pane_columns_at(frame, row_number, probe) != pane {
        // When: this row's pane at `probe` has other bounds, it belongs to another pane layout.
        return None;
    }
    let view = rows.view(frame, row_number)?;
    let token = hardwrap_row_token(&view.cells, usize::from(pane.0), usize::from(pane.1), bullet)?;
    let text = view.cells[token.token_start..token.token_end].iter().map(|cell| cell.ch).collect();
    Some(HardwrapPaneRow { token, text, soft_wrapped: view.row.soft_wrapped_from_previous() })
}

/// Whether `text` starts with a drive prefix such as `C:\` or `C:/`.
fn starts_with_drive(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

/// Whether `text`, an opening row's token, can open a chain of `kind`. A URL needs one of the
/// `scheme://` prefixes `find_urls` accepts and a path after its authority, so a host alone never
/// opens a chain. A path needs an absolute, home or drive start; UNC and network paths
/// (`\\host`, `//host`) never open one.
fn hardwrap_opens(kind: HardwrapKind, text: &str) -> bool {
    match kind {
        HardwrapKind::Uri => {
            // When: `kind` is `Uri`, an allow-listed `scheme://` and a path must open the token.
            let Some(scheme) = ["https://", "http://", "file://"].into_iter().find(|scheme| {
                text.get(..scheme.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
            }) else {
                // When: no allow-listed `scheme://` prefix starts `text`, it opens no URL chain.
                return false;
            };
            let rest = &text[scheme.len()..];
            // A file URI's authority may be empty, so any path slash proves its path.
            if scheme == "file://" {
                rest.contains('/')
            } else {
                // When: `scheme` is http(s), a non-empty authority must precede the path slash.
                rest.find(['/', '?', '#'])
                    .is_some_and(|index| index > 0 && rest.as_bytes()[index] == b'/')
            }
        }
        HardwrapKind::Path => {
            // An absolute, home or drive start with a separator opens a path chain.
            let bytes = text.as_bytes();
            let posix =
                bytes.first() == Some(&b'/') && bytes.get(1).is_some_and(|byte| *byte != b'/');
            posix || text.starts_with("~/") || starts_with_drive(text)
        }
    }
}

/// Whether a continuation row's token starts a new token of `kind` instead of continuing one. A
/// path may continue with `/`, since programs often break before a separator.
fn hardwrap_starts_new(kind: HardwrapKind, text: &str) -> bool {
    match kind {
        HardwrapKind::Uri => text.contains("://"),
        HardwrapKind::Path => {
            // A scheme, home, drive or UNC start begins another target.
            text.contains("://")
                || text.starts_with("~/")
                || starts_with_drive(text)
                || text.starts_with("\\\\")
        }
    }
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

/// `Incomplete` when an owned chain covering `pointed` cannot be proven, so its prefix stays
/// inert; `NotApplicable` otherwise, so the single-row result stands.
fn hardwrap_unproven(
    spans: &[AbsoluteCellSpan],
    owned: bool,
    pointed: AbsoluteCell,
) -> HardwrapChain {
    if !(owned && spans.iter().any(|span| span.contains(pointed))) {
        // When: `owned` is false or no span covers `pointed`, the single-row result stands.
        return HardwrapChain::NotApplicable;
    }
    HardwrapChain::Incomplete
}

/// Follow the chain of `kind` opening at `opener_row` down the screen and decide it for
/// `pointed`. The chain is owned once a continuation row starts at the token's start column; an
/// owned chain that meets prose, the viewport edge or a cap is `Incomplete`, an unowned one is
/// `NotApplicable`.
fn bare_hardwrap_chain<'grid>(
    frame: &FrameRows<'grid>,
    rows: &mut HardwrapRows<'grid>,
    kind: HardwrapKind,
    pane: (u16, u16),
    opener_row: u64,
    opener: &HardwrapPaneRow,
    pointed: AbsoluteCell,
) -> HardwrapChain {
    let right = usize::from(pane.1);
    let column = opener.token.token_start;
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    spans.push(AbsoluteCellSpan {
        row: opener_row,
        start_col: column as u16,
        end_col: opener.token.token_end as u16,
    });
    let mut joined = opener.text.clone();
    let mut owned = false;
    for offset in 1..MAX_WRAPPED_PATH_ROWS as u64 {
        let row_number = opener_row + offset;
        if row_number >= frame.view_end() {
            // When: `row_number` reaches `view_end`, the chain's next row is out of view and unproven.
            return hardwrap_unproven(&spans, owned, pointed);
        }
        let Some(row) = hardwrap_pane_row(frame, rows, row_number, pane, column as u16, false)
            .filter(|row| {
                !row.soft_wrapped
                    && row.token.first_text == column
                    && !row.text.is_empty()
                    && !hardwrap_starts_new(kind, &row.text)
            })
        else {
            // When: the next row holds no continuation at the token's column, the edge end is unproven.
            return hardwrap_unproven(&spans, owned, pointed);
        };
        owned = true;
        spans.push(AbsoluteCellSpan {
            row: row_number,
            start_col: column as u16,
            end_col: row.token.token_end as u16,
        });
        if joined.len() + row.text.len() > MAX_LOGICAL_PATH_BYTES {
            // When: the joined text would pass the byte cap, the chain cannot be proven.
            return hardwrap_unproven(&spans, owned, pointed);
        }
        joined.push_str(&row.text);
        if row.token.trailing_text {
            // When: text follows the fragment on its row, the row is prose, not the token's end.
            return hardwrap_unproven(&spans, owned, pointed);
        }
        if !row.token.reaches_edge(right) {
            // When: the fragment ends before the edge, it is the chain's last row.
            if !spans.iter().any(|span| span.contains(pointed))
                || spans_join_across_pane_border(frame, &spans)
            {
                // When: no span covers `pointed` or the spans cross a pane border, the chain does not own it.
                return HardwrapChain::NotApplicable;
            }
            return HardwrapChain::Proven { spans, joined };
        }
    }
    // Every row up to the cap reached the edge, so the chain's end is out of reach.
    hardwrap_unproven(&spans, owned, pointed)
}

/// Decide a URL a program broke across rows itself, outside any wrapper. The token starts its
/// row's content (after the pane-relative indent or one bullet), every non-last row ends exactly
/// at the pane's right edge, and continuation rows start at the token's start column. Runs after
/// the bracketed rule and before the wrap-aware scanner, on both screens.
fn bare_hardwrap_uri_in(frame: &FrameRows<'_>, pointed: AbsoluteCell) -> HardwrapUri {
    match bare_hardwrap_in(frame, pointed, HardwrapKind::Uri) {
        HardwrapChain::NotApplicable => HardwrapUri::NotApplicable,
        HardwrapChain::Incomplete => HardwrapUri::Incomplete,
        HardwrapChain::Proven { spans, joined } => hardwrap_uri_target(spans, &joined, pointed),
    }
}

/// Validate a proven chain's joined text as one URL for `pointed`: `find_urls` must read it as one
/// URL from its start, and sentence punctuation it trims must lie inside the last fragment.
fn hardwrap_uri_target(
    mut spans: SmallVec<[AbsoluteCellSpan; 2]>,
    joined: &str,
    pointed: AbsoluteCell,
) -> HardwrapUri {
    let matches = sonicterm_cfg::url_scan::find_urls(joined);
    let Some(found) = matches.first().filter(|found| {
        matches.len() == 1
            && found.start == 0
            && joined.matches("://").count() == 1
            && joined.get(..found.end) == Some(found.url.as_str())
    }) else {
        // When: `find_urls` does not read `joined` as one URL from its start, the owned chain stays inert.
        return HardwrapUri::Incomplete;
    };
    let trimmed = joined.len() - found.end;
    let Some(last) =
        spans.last_mut().filter(|last| trimmed < usize::from(last.end_col - last.start_col))
    else {
        // When: the trimmed tail covers the whole last fragment, the URL ends before it.
        return HardwrapUri::Incomplete;
    };
    last.end_col -= trimmed as u16;
    if !spans.iter().any(|span| span.contains(pointed)) {
        // When: `pointed` is on punctuation `find_urls` trimmed, it is not part of the URL.
        return HardwrapUri::NotApplicable;
    }
    HardwrapUri::Complete(LogicalTargetCandidate {
        target: DetectedTarget::Uri(found.url.clone()),
        spans,
        joined: true,
    })
}

/// The chain of `kind` under `pointed`: the rows above it that a chain could span are walked
/// once, and openers among them are tried top first, so a continuation row that resembles an
/// opener never cuts off the rows above it.
fn bare_hardwrap_in(
    frame: &FrameRows<'_>,
    pointed: AbsoluteCell,
    kind: HardwrapKind,
) -> HardwrapChain {
    let pane = pane_columns_at(frame, pointed.row, pointed.col);
    if pointed.col < pane.0 || pointed.col >= pane.1 {
        // When: the pointer is on a pane border, no pane token lies under it.
        return HardwrapChain::NotApplicable;
    }
    let right = usize::from(pane.1);
    let top = pointed.row.saturating_sub((MAX_WRAPPED_PATH_ROWS - 1) as u64).max(frame.view_top());
    let mut rows = HardwrapRows { views: HashMap::new() };
    // Walk up while the row above fills to the edge and this row starts at its token column.
    let mut start = pointed.row;
    while start > top {
        let Some(lower) = hardwrap_pane_row(frame, &mut rows, start, pane, pointed.col, false)
        else {
            // When: this row holds no text in the pane, no chain continues through it.
            break;
        };
        let Some(upper) = hardwrap_pane_row(frame, &mut rows, start - 1, pane, pointed.col, true)
        else {
            // When: the row above holds no text in the pane, no chain reaches down from it.
            break;
        };
        if lower.soft_wrapped
            || !upper.token.reaches_edge(right)
            || lower.token.first_text != upper.token.token_start
        {
            // When: `lower` is soft-wrapped, `upper` misses the edge, or `first_text` misaligns, stop the walk.
            break;
        }
        start -= 1;
    }
    for opener_row in start..=pointed.row {
        let Some(opener) = hardwrap_pane_row(frame, &mut rows, opener_row, pane, pointed.col, true)
            .filter(|row| {
                !row.soft_wrapped
                    && row.token.reaches_edge(right)
                    && hardwrap_opens(kind, &row.text)
            })
        else {
            // When: this row's token does not open a chain of `kind` at the edge, try the next row.
            continue;
        };
        match bare_hardwrap_chain(frame, &mut rows, kind, pane, opener_row, &opener, pointed) {
            HardwrapChain::NotApplicable => {
                // When: this opener's chain does not decide `pointed`, a lower opener may.
            }
            decided => {
                // When: `decided` is Proven or Incomplete, the topmost owning opener settles `pointed`.
                return decided;
            }
        }
    }
    HardwrapChain::NotApplicable
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

/// Decide a path a program broke across rows itself, with the URL rule's row geometry. The joined
/// text goes through the typed path scanner for `style`, so source references, suffixes, limits
/// and locality behave as for a single-row path.
fn bare_hardwrap_path_in(
    frame: &FrameRows<'_>,
    pointed: AbsoluteCell,
    style: PathStyle,
) -> HardwrapPathScan {
    let spans = match bare_hardwrap_in(frame, pointed, HardwrapKind::Path) {
        HardwrapChain::NotApplicable => {
            // When: `bare_hardwrap_in` is NotApplicable, the path rule does not own `pointed`.
            return HardwrapPathScan::NotApplicable;
        }
        HardwrapChain::Incomplete => {
            // When: `bare_hardwrap_in` is Incomplete, the owned chain stays inert.
            return HardwrapPathScan::Incomplete;
        }
        HardwrapChain::Proven { spans, .. } => spans,
    };
    let mut cells = Vec::new();
    let mut positions = Vec::new();
    let mut rows = SmallVec::<[PathRowIdentity; 2]>::new();
    for span in &spans {
        let Some(row) = frame.row(span.row) else {
            // When: a chain row is no longer displayed, the proven chain cannot be read again.
            return HardwrapPathScan::Incomplete;
        };
        rows.push(PathRowIdentity { row: span.row, fingerprint: row_fingerprint(row) });
        let row_cells = row.iter().collect::<Vec<_>>();
        for column in span.start_col..span.end_col {
            let Some(cell) = row_cells.get(usize::from(column)) else {
                // When: `row_cells` has no cell at `column`, the row changed since the chain was read.
                return HardwrapPathScan::Incomplete;
            };
            cells.push(*cell);
            positions.push(AbsoluteCell { row: span.row, col: column });
        }
    }
    let Some(pointed_index) = positions.iter().position(|position| *position == pointed) else {
        // When: `pointed` is outside the chain's cells, the chain does not own it.
        return HardwrapPathScan::NotApplicable;
    };
    let Some(mapped) = PathCellText::from_cells(&cells, &positions) else {
        // When: the joined cells do not map to text, the scanner has nothing to read.
        return HardwrapPathScan::NotApplicable;
    };
    let candidates = mapped
        .candidates(&cells, pointed_index, style, false)
        .into_iter()
        .filter(|(matched, _)| !matches!(matched.target, DetectedTarget::Uri(_)))
        .filter_map(|(matched, range)| {
            let spans = spans_for_positions(&positions[range])?;
            (spans.len() >= 2 && spans.iter().any(|span| span.contains(pointed)))
                .then_some(LogicalTargetCandidate { target: matched.target, spans, joined: true })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        // When: `candidates` is empty, no path crosses a break and the single-row result stands.
        return HardwrapPathScan::NotApplicable;
    }
    HardwrapPathScan::Joined(LogicalPathScan { candidates, rows })
}

/// Group `positions`, in reading order, into one span per row; `None` when a row's cells are not
/// contiguous.
fn spans_for_positions(positions: &[AbsoluteCell]) -> Option<SmallVec<[AbsoluteCellSpan; 2]>> {
    let mut spans = SmallVec::<[AbsoluteCellSpan; 2]>::new();
    for position in positions {
        match spans.last_mut() {
            Some(span) if span.row == position.row && span.end_col == position.col => {
                span.end_col = position.col.checked_add(1)?;
            }
            Some(span) if span.row == position.row => {
                // When: `span` and `position` share a row without adjacency, the span map is discontinuous.
                return None;
            }
            _ => spans.push(AbsoluteCellSpan {
                row: position.row,
                start_col: position.col,
                end_col: position.col.checked_add(1)?,
            }),
        }
    }
    Some(spans)
}

/// The joined chain's candidates, then the pointed row's own fragments that cover `pointed`, so
/// every candidate contains the hovered cell; rows cover every chain row and fragment row.
fn merge_hardwrap_fragments(
    mut joined: LogicalPathScan,
    fragments: Option<LogicalPathScan>,
    pointed: AbsoluteCell,
) -> LogicalPathScan {
    let Some(fragments) = fragments else {
        // When: `fragments` is None, the joined candidates stand alone.
        return joined;
    };
    for candidate in fragments.candidates {
        let covers = candidate.spans.iter().any(|span| span.contains(pointed));
        let known = joined.candidates.iter().any(|existing| {
            existing.target == candidate.target && existing.spans == candidate.spans
        });
        if covers && !known {
            joined.candidates.push(candidate);
        }
    }
    for row in fragments.rows {
        if !joined.rows.contains(&row) {
            joined.rows.push(row);
        }
    }
    joined.rows.sort_by_key(|row| row.row);
    joined
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
            if text.len() > MAX_LOGICAL_LINE_BYTES {
                // When: text exceeds MAX_LOGICAL_LINE_BYTES, refuse before any candidate enumeration.
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

    /// The scalar starting at byte `byte`. Scalars are in strictly increasing byte order, so a
    /// binary search finds the one match a linear scan would.
    fn scalar_starting_at(&self, byte: usize) -> Option<usize> {
        let found = self.scalars.binary_search_by_key(&byte, |scalar| scalar.byte_start).ok();
        #[cfg(test)]
        assert_eq!(found, self.scalars.iter().position(|scalar| scalar.byte_start == byte));
        found
    }

    /// The scalar ending at byte `byte`, by binary search as [`Self::scalar_starting_at`].
    fn scalar_ending_at(&self, byte: usize) -> Option<usize> {
        let found = self.scalars.binary_search_by_key(&byte, |scalar| scalar.byte_end).ok();
        #[cfg(test)]
        assert_eq!(found, self.scalars.iter().position(|scalar| scalar.byte_end == byte));
        found
    }

    /// The scalar whose cells hold cell `cell`. Scalars cover contiguous, increasing cell ranges.
    fn scalar_covering_cell(&self, cell: usize) -> Option<usize> {
        let index = self.scalars.partition_point(|scalar| scalar.cell_end <= cell);
        let found =
            self.scalars.get(index).filter(|scalar| scalar.cell_start <= cell).map(|_| index);
        #[cfg(test)]
        assert_eq!(
            found,
            self.scalars
                .iter()
                .position(|scalar| (scalar.cell_start..scalar.cell_end).contains(&cell))
        );
        found
    }

    fn candidates(
        &self,
        cells: &[&Cell],
        pointed: usize,
        style: PathStyle,
        include_bare_names: bool,
    ) -> Vec<(TargetMatch, std::ops::Range<usize>)> {
        let Some(col) = self.scalar_covering_cell(pointed) else {
            // When: no scalar contains pointed, it cannot own a scanner target.
            return Vec::new();
        };
        target_candidates_at_char_col_for_style(&self.text, col, style, include_bare_names)
            .into_iter()
            .filter_map(|matched| {
                let start = self.scalar_starting_at(matched.start)?;
                let end = self.scalar_ending_at(matched.end)? + 1;
                let source_start = self.scalar_starting_at(matched.source_start)?;
                let source_end = self.scalar_ending_at(matched.source_end)? + 1;
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

#[cfg(test)]
fn logical_path_scan_at_cell(
    grid: &Grid,
    view_top: u64,
    pointed: AbsoluteCell,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<LogicalPathScan> {
    logical_path_scan_in(&FrameRows::new(grid, view_top), pointed, style, include_bare_names)
}

fn logical_path_scan_in(
    frame: &FrameRows<'_>,
    pointed: AbsoluteCell,
    style: PathStyle,
    include_bare_names: bool,
) -> Option<LogicalPathScan> {
    let grid = frame.grid();
    let (view_top, view_end) = (frame.view_top(), frame.view_end());
    if !frame.contains(pointed.row) || pointed.col >= grid.cols {
        // When: `pointed` lies outside the visible grid, no complete visible logical line owns it.
        return None;
    }

    // On the alternate screen a multiplexer can draw several panes on one row, and a terminal
    // records a wrap only at the grid's edge. So each row adds only its pane's columns, a wrap
    // continues text only within one pane (`wrap_joins_one_pane`), and a pane border ends every
    // name as the grid's edge does. The primary screen reads whole rows.
    let alt_screen = grid.is_alt();
    if alt_screen && pane_border_at(frame, pointed.row, pointed.col) {
        // When: alt_screen and the pointed cell is itself a pane border, it belongs to no pane's text.
        return None;
    }
    let segment_at = |row: u64, column: u16| {
        let (left, right) = if alt_screen {
            pane_columns_at(frame, row, column)
        } else {
            // When: not alt_screen, one program owns the whole row, so its segment spans every column.
            (0, grid.cols)
        };
        PaneRowSegment { row, left, right }
    };
    let pointed_segment = segment_at(pointed.row, pointed.col);
    // Each step adds one row by a recorded wrap or, on the alternate screen, by an inferred
    // pane-edge continuation. A recorded wrap whose partner row is out of view or past the cap
    // leaves the line unproven, so the scan fails closed; an inferred continuation only stops.
    // Every row a step reaches is checked for its own recorded wrap on the next step, so an
    // inferred join never hides an incomplete chain.
    let step_up = |first: PaneRowSegment| -> ChainStep {
        let Some(row) = frame.row(first.row) else {
            // When: `first.row` is no longer retained, nothing proves where its text began.
            return ChainStep::Incomplete;
        };
        let recorded = first.left == 0 && row.soft_wrapped_from_previous();
        if recorded && (first.row == 0 || first.row == view_top) {
            // When: `recorded` and the row that wrapped into `first.row` is out of view, the chain is unproven.
            return ChainStep::Incomplete;
        }
        if recorded && !wrap_joins_one_pane(frame, first.row - 1) {
            // When: `recorded` but `wrap_joins_one_pane` fails, the row above ends another pane's text.
            return ChainStep::End;
        }
        if recorded {
            // When: `recorded`, the terminal proved the row above continues into `first`.
            return ChainStep::Recorded(segment_at(first.row - 1, grid.cols.saturating_sub(1)));
        }
        if !alt_screen {
            // When: not `alt_screen`, only recorded wraps join rows.
            return ChainStep::End;
        }
        inferred_continuation_above(frame, first).map_or(ChainStep::End, ChainStep::Inferred)
    };
    let step_down = |last: PaneRowSegment| -> ChainStep {
        let Some(next_row_number) = last.row.checked_add(1) else {
            // When: `last.row` has no successor, the logical line ends at `last`.
            return ChainStep::End;
        };
        // The row below the frame is never read. On a scrolled-back view a segment reaching the
        // grid's edge may wrap into it, so that chain is unproven; the live view has no row below.
        if next_row_number >= view_end {
            // When: `next_row_number` reaches `view_end`, geometry alone decides Incomplete or End.
            let may_wrap = last.right == grid.cols && frame.rows_below();
            return if may_wrap { ChainStep::Incomplete } else { ChainStep::End };
        }
        let recorded = last.right == grid.cols
            && frame.row(next_row_number).is_some_and(|row| row.soft_wrapped_from_previous());
        if recorded && !wrap_joins_one_pane(frame, last.row) {
            // When: `recorded` but `wrap_joins_one_pane` fails, the wrap carries this pane's text into another pane.
            return ChainStep::End;
        }
        if recorded {
            // When: `recorded`, the terminal proved `last` continues on the next row.
            return ChainStep::Recorded(segment_at(next_row_number, 0));
        }
        if !alt_screen {
            // When: not `alt_screen`, only recorded wraps join rows.
            return ChainStep::End;
        }
        inferred_continuation_below(frame, last).map_or(ChainStep::End, ChainStep::Inferred)
    };
    let mut segments = VecDeque::from([pointed_segment]);
    let (mut first, mut last) = (pointed_segment, pointed_segment);
    let (mut grow_up, mut grow_down) = (true, true);
    while grow_up || grow_down {
        if grow_up {
            // `admit_chain_step` fails the whole scan when a recorded wrap leaves the line unproven.
            match admit_chain_step(step_up(first), segments.len()).ok()? {
                Some(segment) => {
                    first = segment;
                    segments.push_front(segment);
                }
                None => grow_up = false,
            }
        }
        if grow_down {
            match admit_chain_step(step_down(last), segments.len()).ok()? {
                Some(segment) => {
                    last = segment;
                    segments.push_back(segment);
                }
                None => grow_down = false,
            }
        }
    }

    let mut cells = Vec::new();
    let mut positions = Vec::new();
    let mut rows = SmallVec::<[PathRowIdentity; 2]>::new();
    for segment in &segments {
        let row = frame.row(segment.row)?;
        rows.push(PathRowIdentity { row: segment.row, fingerprint: row_fingerprint(row) });
        for (column, cell) in row.iter().enumerate() {
            let column = u16::try_from(column).ok()?;
            if !(segment.left..segment.right).contains(&column) {
                // When: `column` lies outside `segment`, its cell is a border or another pane's text.
                continue;
            }
            cells.push(cell);
            positions.push(AbsoluteCell { row: segment.row, col: column });
        }
        // Every scalar is at least one byte and covers at most two cells, so a chain past twice the
        // byte cap is a line `PathCellText::from_cells` refuses; stop gathering cells here.
        if cells.len() > 2 * MAX_LOGICAL_LINE_BYTES {
            // When: `cells` passes twice `MAX_LOGICAL_LINE_BYTES`, the line exceeds the cap.
            return None;
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
                    Some(span) if span.row != position.row => {
                        // Positions follow the joined segments, so a new row starts its next fragment.
                        spans.push(AbsoluteCellSpan {
                            row: position.row,
                            start_col: position.col,
                            end_col: position.col.checked_add(1)?,
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
            Some(LogicalTargetCandidate { target: matched.target, spans, joined: false })
        })
        .collect::<Vec<_>>();
    // A multiplexer may have cut the text at a pane edge. Shortest-first selection never needs to
    // rule out a longer name, so only the pointed run and each candidate that reaches a cut edge
    // are refused; a candidate wholly inside the visible chain stays valid.
    let chain_rows = segments.iter().map(|segment| segment.row).collect::<SmallVec<[u64; 8]>>();
    if alt_screen
        && spans_reach_cut_pane_edge(
            frame,
            &pointed_text_spans(&cells, &positions, pointed_index),
            &chain_rows,
        )
    {
        // When: alt_screen and the pointed run reaches a cut pane edge, its text may continue out of view.
        return None;
    }
    let candidates = candidates
        .into_iter()
        .filter(|candidate| {
            !alt_screen || !spans_reach_cut_pane_edge(frame, &candidate.spans, &chain_rows)
        })
        .collect::<Vec<_>>();
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

/// Unicode scalars of a detected filesystem target's displayed text, the unit of
/// `clickable_path_max_chars` and of probe tiers.
fn detected_target_chars(target: &DetectedTarget) -> usize {
    match target {
        DetectedTarget::PathCandidate(text) | DetectedTarget::BareName(text) => {
            text.chars().count()
        }
        DetectedTarget::SourceReference(reference) => reference.display.chars().count(),
        DetectedTarget::Uri(uri) => uri.chars().count(),
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
/// the displayed row above or below also draws in the same column. A neighbour outside the frame
/// is never read, so at a frame edge only the displayed neighbour counts.
fn pane_border_at(frame: &FrameRows<'_>, row: u64, column: u16) -> bool {
    let border_in = |row_number: u64| {
        is_pane_border(
            frame.row(row_number).and_then(|found_row| found_row.get(usize::from(column))),
        )
    };
    border_in(row)
        && (row.checked_sub(1).filter(|above| *above >= frame.view_top()).is_some_and(border_in)
            || row.checked_add(1).filter(|below| frame.contains(*below)).is_some_and(border_in))
}

/// Report whether consecutive hard-wrap `spans` continue across a vertical pane border: on the
/// alternate screen, a border that both rows draw between where the lower fragment starts and
/// where the upper one starts puts the next row's text in another pane.
fn spans_join_across_pane_border(frame: &FrameRows<'_>, spans: &[AbsoluteCellSpan]) -> bool {
    frame.grid().is_alt()
        && spans.iter().zip(spans.iter().skip(1)).any(|(upper, lower)| {
            upper.row.checked_add(1) == Some(lower.row)
                && (lower.start_col..upper.start_col).any(|column| {
                    pane_border_at(frame, upper.row, column)
                        && pane_border_at(frame, lower.row, column)
                })
        })
}

/// Return the columns `[left, right)` of the pane that holds `column` of absolute `row`: the cells
/// between the nearest pane borders on either side, or the grid's edges where there is none.
fn pane_columns_at(frame: &FrameRows<'_>, row: u64, column: u16) -> (u16, u16) {
    let cols = frame.grid().cols;
    let border = |edge_column: &u16| pane_border_at(frame, row, *edge_column);
    let left = (0..column).rev().find(border).map_or(0, |edge_column| edge_column + 1);
    let right = (column..cols).find(border).unwrap_or(cols);
    (left, right)
}

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

/// Apply the row cap to one chain step: `Err` fails the scan closed, `Ok(None)` stops growing in
/// that direction, and `Ok(Some(segment))` adds the row. A recorded wrap past the cap leaves the
/// line unproven, while an inferred continuation only stops there.
fn admit_chain_step(step: ChainStep, chain_rows: usize) -> Result<Option<PaneRowSegment>, ()> {
    match step {
        ChainStep::Incomplete => Err(()),
        ChainStep::End => Ok(None),
        ChainStep::Recorded(_) if chain_rows >= MAX_WRAPPED_PATH_ROWS => Err(()),
        ChainStep::Inferred(_) if chain_rows >= MAX_WRAPPED_PATH_ROWS => Ok(None),
        ChainStep::Recorded(segment) | ChainStep::Inferred(segment) => Ok(Some(segment)),
    }
}

/// The columns `[left, right)` of absolute `row` that one pane's text occupies.
#[derive(Debug, Clone, Copy)]
struct PaneRowSegment {
    row: u64,
    left: u16,
    right: u16,
}

/// Report whether the wrap a terminal recorded from absolute `upper_row` into the next row
/// continues one pane's text.
///
/// A terminal records a wrap only at the grid's edge, so the wrap joins the pane that reaches the
/// right edge on `upper_row` to the pane that starts at the left edge on the next row. A split
/// draws its border down both rows, so those are two panes unless either row spans its far edge.
/// The primary screen shows one program, so every wrap there continues its text.
fn wrap_joins_one_pane(frame: &FrameRows<'_>, upper_row: u64) -> bool {
    let grid = frame.grid();
    if !grid.is_alt() {
        // When: not grid.is_alt(), one program owns every row, so a recorded wrap continues its text.
        return true;
    }
    let Some(lower_row) = upper_row.checked_add(1) else {
        // When: `upper_row` has no successor, no wrap leaves it.
        return false;
    };
    let last_column = grid.cols.saturating_sub(1);
    if pane_border_at(frame, upper_row, last_column) || pane_border_at(frame, lower_row, 0) {
        // When: a pane border fills `upper_row`'s last column or `lower_row`'s first, no text crosses the wrap.
        return false;
    }
    let (upper_left, _) = pane_columns_at(frame, upper_row, last_column);
    let (_, lower_right) = pane_columns_at(frame, lower_row, 0);
    upper_left == 0 || lower_right == grid.cols
}

/// Return the next visible row's segment of `upper`'s pane when `upper`'s text fills the pane's
/// last column and that segment starts with text, so a multiplexer's unrecorded pane-edge wrap
/// continues one logical line. Mismatched pane edges or a blank start end the line.
fn inferred_continuation_below(
    frame: &FrameRows<'_>,
    upper: PaneRowSegment,
) -> Option<PaneRowSegment> {
    let grid = frame.grid();
    let lower_row = upper.row.checked_add(1).filter(|row| *row < frame.view_end())?;
    if (upper.left == 0 || upper.right == grid.cols)
        && frame.row(lower_row)?.soft_wrapped_from_previous()
        && !wrap_joins_one_pane(frame, upper.row)
    {
        // When: `upper` touches a grid edge and `wrap_joins_one_pane` fails, the recorded wrap took that text elsewhere.
        return None;
    }
    let cell_at = |row: u64, column: u16| frame.row(row)?.get(usize::from(column));
    let last_column = upper.right.checked_sub(1)?;
    if !is_text_cell(cell_at(upper.row, last_column))
        || !is_text_cell(cell_at(lower_row, upper.left))
    {
        // When: `is_text_cell` rejects `last_column` or `lower_row`'s first pane cell, no text crosses the edge.
        return None;
    }
    let (left, right) = pane_columns_at(frame, lower_row, upper.left);
    (left == upper.left && right == upper.right).then_some(PaneRowSegment {
        row: lower_row,
        left,
        right,
    })
}

/// Return the previous visible row's segment of `lower`'s pane when it fills the pane's last
/// column and `lower` starts with text: the mirror of [`inferred_continuation_below`].
fn inferred_continuation_above(
    frame: &FrameRows<'_>,
    lower: PaneRowSegment,
) -> Option<PaneRowSegment> {
    let grid = frame.grid();
    let upper_row = lower.row.checked_sub(1).filter(|row| *row >= frame.view_top())?;
    if (lower.left == 0 || lower.right == grid.cols)
        && frame.row(lower.row)?.soft_wrapped_from_previous()
        && !wrap_joins_one_pane(frame, upper_row)
    {
        // When: `lower` touches a grid edge and `wrap_joins_one_pane` fails, the recorded wrap joins other panes.
        return None;
    }
    let cell_at = |row: u64, column: u16| frame.row(row)?.get(usize::from(column));
    let last_column = lower.right.checked_sub(1)?;
    if !is_text_cell(cell_at(lower.row, lower.left))
        || !is_text_cell(cell_at(upper_row, last_column))
    {
        // When: `is_text_cell` rejects `lower`'s first cell or `upper_row`'s `last_column`, no text crosses the edge.
        return None;
    }
    let (left, right) = pane_columns_at(frame, upper_row, lower.left);
    (left == lower.left && right == lower.right).then_some(PaneRowSegment {
        row: upper_row,
        left,
        right,
    })
}

/// Return the OSC 7 directory that relative and contextual text resolves against.
///
/// On the alternate screen a full-screen program such as tmux or rmux can show several panes while
/// it relays only the active pane's directory, and the screen cannot show which pane holds the
/// text: pane borders may be box drawing, ASCII or blank, or look like a program's own rule. So
/// such text gets no directory there. The primary screen shows the reporting shell's own output.
fn relative_text_cwd(parser: &sonicterm_vt::vt::Parser) -> Option<Osc7Cwd> {
    if parser.grid().is_alt() {
        // When: parser.grid().is_alt(), the reported directory may belong to another pane, so none applies.
        return None;
    }
    parser.osc7_cwd().cloned()
}

/// Report whether plain-text `spans` touch a pane edge that a multiplexer may have cut.
///
/// On the alternate screen a multiplexer positions every pane row with a cursor move, so text
/// that reaches its pane's right edge may continue on the next row, and text that starts at the
/// left edge under a row that filled the pane may be the rest of a longer target. Neither end can
/// be proven from the grid, so the caller refuses such text instead of offering a cut-off prefix.
fn spans_reach_cut_pane_edge(
    frame: &FrameRows<'_>,
    spans: &[AbsoluteCellSpan],
    chain_rows: &[u64],
) -> bool {
    let (grid, view_top) = (frame.grid(), frame.view_top());
    let cell_at = move |row: u64, column: u16| frame.row(row)?.get(usize::from(column));
    let border = |row: u64, column: u16| pane_border_at(frame, row, column);
    let text = |row: u64, column: u16| is_text_cell(cell_at(row, column));
    let (Some(head), Some(tail)) = (spans.first(), spans.last()) else {
        // When: spans is empty, the target covers no cell and so touches no pane edge.
        return false;
    };
    // `chain_rows` holds every row the scan joined, by a recorded wrap or an inferred pane-edge
    // continuation, so text continues past an edge exactly when the next chain row exists.
    let tail_pane_right =
        (tail.end_col..grid.cols).find(|column| border(tail.row, *column)).unwrap_or(grid.cols);
    let wrapped_below = chain_rows.contains(&(tail.row + 1));
    let tail_cut =
        !wrapped_below && (tail.end_col..tail_pane_right).all(|column| text(tail.row, column));
    let head_pane_left = (0..head.start_col)
        .rev()
        .find(|column| border(head.row, *column))
        .map_or(0, |column| column + 1);
    let wrapped_above = head.row.checked_sub(1).is_some_and(|above| chain_rows.contains(&above));
    let starts_line = head.row > view_top && !wrapped_above;
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

/// The visible text of the hover fragments `cells`, read from `frame`.
fn hyperlink_label(
    frame: &FrameRows<'_>,
    cells: &sonicterm_render_model::inputs::HoveredUrlCells,
) -> LinkLabel {
    use sonicterm_render_model::inputs::MAX_HOVERED_URL_SPANS;
    let spans = cells.spans();
    let last_row = frame.grid().rows.saturating_sub(1);
    let mut complete = spans.len() < MAX_HOVERED_URL_SPANS
        && spans.first().is_some_and(|span| span.row > 0)
        && spans.last().is_some_and(|span| span.row < last_row);
    let mut text = String::new();
    'spans: for span in spans {
        let Some(row) = frame.row(frame.view_top() + u64::from(span.row)) else {
            // When: a fragment's row is not in the frame, the label cannot be read whole.
            complete = false;
            break;
        };
        let width = usize::from(span.end_col - span.start_col);
        for cell in row.iter().skip(usize::from(span.start_col)).take(width) {
            if cell.flags.contains(sonicterm_types::CellFlags::WIDE_CONT) {
                // When: `WIDE_CONT` marks the right half of a wide `cell`, its text is already read.
                continue;
            }
            if text.len() + cell.ch.len_utf8() + cell.extras().map_or(0, str::len)
                > MAX_LINK_LABEL_BYTES
            {
                // When: the label passes `MAX_LINK_LABEL_BYTES`, the rest is unseen.
                complete = false;
                break 'spans;
            }
            text.push(cell.ch);
            text.extend(cell.extras());
        }
    }
    LinkLabel { text, complete }
}

fn hyperlink_hover_cells(
    frame: &FrameRows<'_>,
    pane_id: u64,
    pointed_row: u16,
    col: u16,
    hyperlink_id: sonicterm_types::HyperlinkId,
) -> Option<sonicterm_render_model::inputs::HoveredUrlCells> {
    use sonicterm_render_model::inputs::{HoveredUrlCells, HoveredUrlSpan, MAX_HOVERED_URL_SPANS};

    let grid = frame.grid();
    let cells_at = move |row_number: u16| {
        let row = frame.row(frame.view_top().checked_add(u64::from(row_number))?)?;
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
    // A multiplexer positions each pane row with a cursor move, so its rows record no soft wrap, and
    // an app such as Claude Code wraps a long link itself with a hanging indent and a small right
    // margin. On the alternate screen a fragment continues into the next row's fragment of the same
    // link when, inside one pane, at most `LINK_CONTINUATION_MARGIN` blank cells follow the upper
    // fragment and only `LINK_CONTINUATION_INDENT` blank cells precede the lower one. Activation
    // opens the stored destination, not joined text, so this changes only which cells are underlined.
    let pane_continuation = grid.is_alt();
    // A margin or indent must be truly blank: a rule or border glyph between two fragments ends
    // the link's line even though it is not text.
    let is_blank = |cell: Option<&&Cell>| {
        cell.is_none_or(|cell| cell.ch == ' ' && !cell.flags.contains(CellFlags::WIDE_CONT))
    };
    let pane_predecessor = |lower: HoveredUrlSpan| {
        let upper_row = lower.row.checked_sub(1)?;
        let (_, upper) = cells_at(upper_row)?;
        let (_, below) = cells_at(lower.row)?;
        let (left, right) =
            shared_pane_columns(&upper, &below, grid.cols, lower.start_col, lower.end_col);
        if lower.start_col - left > LINK_CONTINUATION_INDENT
            || !(left..lower.start_col).all(|column| is_blank(below.get(usize::from(column))))
        {
            // When: `lower.start_col` follows non-blank cells or more than `LINK_CONTINUATION_INDENT` cells, it starts a line.
            return None;
        }
        let last_visible =
            (left..right).rev().find(|column| !is_blank(upper.get(usize::from(*column))))?;
        if right - last_visible - 1 > LINK_CONTINUATION_MARGIN {
            // When: more than `LINK_CONTINUATION_MARGIN` blank cells end the upper row, it did not wrap.
            return None;
        }
        fragment(upper_row, last_visible)
    };
    let pane_successor = |upper: HoveredUrlSpan| {
        let (_, above) = cells_at(upper.row)?;
        let (_, lower) = cells_at(upper.row + 1)?;
        let (left, right) =
            shared_pane_columns(&above, &lower, grid.cols, upper.start_col, upper.end_col);
        if right - upper.end_col > LINK_CONTINUATION_MARGIN
            || !(upper.end_col..right).all(|column| is_blank(above.get(usize::from(column))))
        {
            // When: non-blank cells or more than `LINK_CONTINUATION_MARGIN` cells follow `upper`, it did not wrap.
            return None;
        }
        let first_visible =
            (left..right).find(|column| !is_blank(lower.get(usize::from(*column))))?;
        if first_visible - left > LINK_CONTINUATION_INDENT {
            // When: `first_visible` lies past `LINK_CONTINUATION_INDENT`, the lower row is not a hanging indent.
            return None;
        }
        fragment(upper.row + 1, first_visible)
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
    /// An OSC 8 URI's visible text; `None` for every other target.
    pub(super) label: Option<LinkLabel>,
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
        Some(super::hovered_url::HoveredUrl {
            cells,
            url: self.display.clone(),
            label: self.label.clone(),
        })
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
        let frame = FrameRows::new(grid, view_top);
        self.detect_cell_target(window_id, pane_id, viewport_row, col, parser, &frame)
    }

    /// Detect the target at displayed cell (`viewport_row`, `col`), reading rows only through `frame`.
    fn detect_cell_target(
        &self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
        parser: &sonicterm_vt::vt::Parser,
        frame: &FrameRows<'_>,
    ) -> Option<CellTargetSnapshot> {
        let grid = frame.grid();
        let view_top = frame.view_top();
        if viewport_row >= grid.rows {
            // When: `viewport_row` is past the frame's rows, the pointer is on no displayed cell.
            return None;
        }
        let absolute_row = view_top.checked_add(u64::from(viewport_row))?;
        let row = frame.row(absolute_row)?;
        let cell = row.iter().nth(usize::from(col))?;
        let rejected = |destination: &str, reason| {
            Some(CellTargetSnapshot {
                label: None,
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
                let cwd = relative_text_cwd(parser);
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
                            fingerprint: row_fingerprint(frame.row(span.row)?),
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
                        joined: false,
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
                    label: None,
                    pane_id,
                    hover_cells,
                    display: destination.to_owned(),
                    explicit_hyperlink,
                    target: ResolvedCellTarget::Path(key),
                })
            };
        if let Some(hyperlink_id) = cell.hyperlink() {
            // When: `cell` carries `hyperlink_id`, preserve its OSC 8 URI provenance instead of scanning the displayed text as a path.
            let uri = parser.hyperlinks().lookup(hyperlink_id)?.uri.to_string();
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
                    hyperlink_hover_cells(frame, pane_id, viewport_row, col, hyperlink_id),
                );
            }
            let hover_cells =
                hyperlink_hover_cells(frame, pane_id, viewport_row, col, hyperlink_id);
            return Some(CellTargetSnapshot {
                pane_id,
                hover_cells,
                display: uri.clone(),
                explicit_hyperlink: true,
                target: ResolvedCellTarget::Uri(uri),
                label: hover_cells.as_ref().map(|cells| hyperlink_label(frame, cells)),
            });
        }

        let style = PathStyle::native();
        let clickable_local_targets = self.config.terminal.clickable_local_targets;
        let clickable_bare_names = self.config.terminal.clickable_bare_names;
        let pointed = AbsoluteCell { row: absolute_row, col };
        let logical = match hardwrap_uri_in(frame, pointed) {
            HardwrapUri::Complete(candidate) => {
                LogicalPathScan { candidates: vec![candidate], rows: SmallVec::new() }
            }
            HardwrapUri::Incomplete => {
                // When: hardwrap_uri_at_cell is Incomplete, never fall back to its valid-looking prefix.
                return None;
            }
            HardwrapUri::NotApplicable => {
                // When: `hardwrap_uri_in` is NotApplicable, the bare program-broken rule decides next.
                match bare_hardwrap_uri_in(frame, pointed) {
                    HardwrapUri::Complete(candidate) => {
                        LogicalPathScan { candidates: vec![candidate], rows: SmallVec::new() }
                    }
                    HardwrapUri::Incomplete => {
                        // When: `bare_hardwrap_uri_in` is Incomplete, never fall back to its prefix.
                        return None;
                    }
                    HardwrapUri::NotApplicable => {
                        // When: `bare_hardwrap_uri_in` is NotApplicable, the path rule decides next.
                        match bare_hardwrap_path_in(frame, pointed, style) {
                            HardwrapPathScan::Incomplete => {
                                // When: `bare_hardwrap_path_in` is Incomplete, never fall back to its prefix.
                                return None;
                            }
                            HardwrapPathScan::Joined(joined) => merge_hardwrap_fragments(
                                joined,
                                logical_path_scan_in(frame, pointed, style, clickable_bare_names),
                                pointed,
                            ),
                            HardwrapPathScan::NotApplicable => {
                                logical_path_scan_in(frame, pointed, style, clickable_bare_names)?
                            }
                        }
                    }
                }
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
                label: None,
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

        let cwd = relative_text_cwd(parser);
        let max_chars = self.config.terminal.effective_clickable_path_max_chars();
        let mut candidates = logical
            .candidates
            .into_iter()
            .filter(|candidate| {
                detected_target_enabled(
                    &candidate.target,
                    clickable_local_targets,
                    clickable_bare_names,
                ) && detected_target_chars(&candidate.target) <= max_chars
            })
            .filter_map(|candidate| {
                let resolved_path = resolve_detected_path(
                    &candidate.target,
                    style,
                    cwd.as_ref(),
                    self.home_dir.as_deref(),
                    &self.local_hostname,
                )?;
                Some(PathProbeCandidate {
                    spans: candidate.spans,
                    target: candidate.target,
                    resolved_path,
                    joined: candidate.joined,
                })
            })
            .collect::<Vec<_>>();
        // Joined candidates come first, so the hover shows the joined path.
        candidates.sort_by(|left, right| {
            right
                .joined
                .cmp(&left.joined)
                .then_with(|| left.probe_order().cmp(&right.probe_order()))
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
            label: None,
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
        let (rendered_pane, row, col) = match window.renderer.as_ref() {
            Some(renderer) => renderer.pixel_to_pane_cell(cursor_x_px, cursor_y_px)?,
            None => Self::headless_pointer_cell(window, cursor_x_px, cursor_y_px)?,
        };
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

    /// The pane cell under a point in a window with no renderer: a headless test with a pane
    /// viewport resolves it on that viewport's uniform grid; otherwise there is none.
    #[cfg(test)]
    fn headless_pointer_cell(
        window: &super::WindowState,
        cursor_x_px: f32,
        cursor_y_px: f32,
    ) -> Option<(u64, u16, u16)> {
        // A window without `test_pane_viewport` has no uniform grid to resolve the point on.
        let (_, cell_width_px, cell_height_px) = window.test_pane_viewport?;
        super::window_event::headless_pane_cell(
            &Self::compute_pane_rects_for(window),
            (cell_width_px, cell_height_px),
            cursor_x_px,
            cursor_y_px,
        )
    }

    /// The pane cell under a point in a window with no renderer: outside tests there is none.
    #[cfg(not(test))]
    fn headless_pointer_cell(
        _window: &super::WindowState,
        _cursor_x_px: f32,
        _cursor_y_px: f32,
    ) -> Option<(u64, u16, u16)> {
        None
    }

    /// Refreshes pointer feedback without treating a busy parser as a missing target.
    pub(super) fn refresh_target_hover(&mut self, window_id: WindowId) {
        let lookup = match self.pointer_target_cell(window_id) {
            Some((pane_id, row, col)) => self.hover_lookup(window_id, pane_id, row, col),
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
            self.hover_target_from_parser(window_id, pane_id, row, col, parser, viewport)
        });
        self.apply_target_hover(window_id, target);
    }

    /// An event-time hover lookup of one pane cell: `Err` when the pane's parser is busy, so the
    /// caller keeps its feedback instead of treating contention as a missing target.
    pub(super) fn hover_lookup(
        &mut self,
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
        // The pane's parser handle outlives the borrow of `windows`, so the lookup can update the
        // window's hover state while the guard is held.
        let parser_handle = Arc::clone(&pane.parser);
        let parser = parser_handle.try_lock().ok_or(())?;
        let viewport = pane.resolved_viewport(parser.grid());
        Ok(self.hover_target_from_parser(window_id, pane_id, viewport_row, col, &parser, viewport))
    }

    /// Detect the hover target at one displayed pane cell and count the work. Hover goes through
    /// here; a click computes its own fresh target through [`Self::cell_target_at`].
    fn hover_target_from_parser(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
        parser: &sonicterm_vt::vt::Parser,
        viewport_top_abs: Option<u64>,
    ) -> Option<CellTargetSnapshot> {
        let grid = parser.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        let frame = FrameRows::new(grid, view_top);
        let terminal = &self.config.terminal;
        let context = DetectionContext {
            pane_id,
            pointed: (viewport_row, col),
            view_top,
            rows_below: frame.rows_below(),
            scrollback_evicted: grid.scrollback_evicted(),
            screen_epoch: grid.screen_epoch(),
            size_generation: grid.size_generation(),
            alt_screen: grid.is_alt(),
            cwd_revision: parser.cwd_revision(),
            clickable_local_targets: terminal.clickable_local_targets,
            clickable_bare_names: terminal.clickable_bare_names,
            clickable_path_max_chars: terminal.effective_clickable_path_max_chars(),
        };
        let cached = self
            .windows
            .get(&window_id)
            .and_then(|window| window.hover_detection.lookup(&context, grid));
        if let Some(target) = cached {
            // When: the key and every read row's stamp match, the cached result stands; apply still runs.
            self.note_hover(|hover| {
                hover.lookups += 1;
                hover.cache_hits += 1;
            });
            return target;
        }
        let target = self.detect_cell_target(window_id, pane_id, viewport_row, col, parser, &frame);
        let rows_read = frame.rows_read();
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.hover_detection.store(context, grid, frame.read_span(), target.clone());
        }
        self.note_hover(|hover| {
            hover.lookups += 1;
            hover.detections += 1;
            hover.rows_read += rows_read;
        });
        target
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
                window.request_window_redraw();
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
            window.hover_unresolved = false;
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
                    // A current key needs no request, so it is compared rather than deep-cloned.
                    if !window.path_probe.is_current(key) {
                        probe_request = window.path_probe.request(key.clone());
                    }
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
                            label: None,
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
            self.note_hover(|hover| hover.probes_submitted += 1);
            let submitted = self.path_workers.as_ref().is_some_and(|workers| {
                workers
                    .probe(request)
                    .inspect_err(|error| {
                        tracing::warn!(%error, "path openability probe unavailable");
                    })
                    .is_ok()
            });
            if submitted {
                self.note_path_submit();
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
                window.request_window_redraw();
            }
        }
    }

    pub(super) fn clear_target_hover(&mut self, window_id: WindowId) {
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: `window_id` no longer identifies a live window, there is no hover or probe state left to clear.
            return;
        };
        window.hover_detection.clear();
        // The pointer left the targets, so the absence is resolved, not pending a re-detection.
        window.hover_unresolved = false;
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
            window.request_window_redraw();
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
        window.request_window_redraw();
    }

    pub(super) fn open_modifier_held(&self, window_id: WindowId) -> bool {
        let modifiers =
            self.windows.get(&window_id).map(|window| window.modifiers).unwrap_or_default();
        open_modifier_in(modifiers)
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
                if let Some(window) = self.windows.get_mut(&window_id) {
                    // A finished check can wait for a hover refresh; the click's own key validates it.
                    window.path_probe.accept_waiting_for(&key);
                }
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
                    if reason == "path-error-missing" && key.link_destination.is_none() {
                        // When: `reason` is path-error-missing and `link_destination` is none, the click stays ordinary.
                        return false;
                    }
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
                // Re-probe every contender at or before the selected tier so a file that appeared since hover wins.
                let contenders = key
                    .candidates
                    .iter()
                    .filter(|candidate| contends_with(candidate, &selection.candidate))
                    .cloned()
                    .collect::<Vec<_>>();
                match workers.open(
                    window_id,
                    pane_id,
                    selection.candidate.resolved_path,
                    selection.decision,
                    contenders,
                ) {
                    Ok(true) => {
                        self.note_path_submit();
                        true
                    }
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

/// Whether `modifiers` holds the platform's target open modifier: Cmd on macOS, Ctrl elsewhere.
/// The underline preview, the click and the status line's link segment all use this one rule.
pub(super) fn open_modifier_in(modifiers: winit::keyboard::ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        // When: `target_os` is macOS, Cmd is the platform-native target activation modifier.
        modifiers.super_key()
    } else {
        // When: `target_os` is not macOS, Ctrl is the platform-native target activation modifier.
        modifiers.control_key()
    }
}

#[cfg(test)]
#[path = "path_target_tests.rs"]
mod path_target_tests;
