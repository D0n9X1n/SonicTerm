//! Bounded probe queue, candidate tiers and native direct-open worker dispatch.

use super::*;

impl PathProbeQueue {
    pub(super) fn submit(&mut self, request: PathProbeRequest) {
        let window_id = request.key.window_id;
        if self.pending.insert(window_id, request).is_none() {
            // A replacement keeps the window's turn; only a newly waiting window joins the back.
            self.order.push_back(window_id);
        }
        self.publish_waiting();
    }

    /// Publish how many requests wait, for `path_check`.
    // Ordering: waiting is stored Relaxed; the reader needs a recent count, not a happens-before.
    pub(super) fn publish_waiting(&self) {
        self.waiting.store(self.pending.len(), std::sync::atomic::Ordering::Relaxed);
    }

    pub(super) fn take_next(&mut self) -> Option<PathProbeRequest> {
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

    pub(super) fn cancel_window(&mut self, window_id: WindowId) -> bool {
        self.order.retain(|queued| *queued != window_id);
        let removed = self.pending.remove(&window_id).is_some();
        self.publish_waiting();
        removed
    }
}

impl PathProbeMailbox {
    pub(in crate::app) fn new() -> (Self, Receiver<()>) {
        let (wake, receiver) = crossbeam_channel::bounded(1);
        let queue = PathProbeQueue::default();
        let waiting = Arc::clone(&queue.waiting);
        (Self { queue: Arc::new(Mutex::new(queue)), wake, waiting }, receiver)
    }

    /// Requests waiting for the worker, without taking the queue lock.
    // Ordering: waiting is loaded Relaxed, pairing with publish_waiting's Relaxed store.
    pub(in crate::app) fn waiting(&self) -> usize {
        self.waiting.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Queue `request` behind other windows' waiting probes, replacing only its own window's.
    pub(in crate::app) fn submit(&self, request: PathProbeRequest) -> Result<(), TrySendError<()>> {
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
    pub(in crate::app) fn cancel_window(&self, window_id: WindowId) -> bool {
        lock_probe_queue(&self.queue).cancel_window(window_id)
    }

    #[cfg(test)]
    pub(in crate::app) fn take_next(&self) -> Option<PathProbeRequest> {
        take_next_probe(&self.queue)
    }

    #[cfg(test)]
    pub(in crate::app) fn waiting_len(&self) -> usize {
        lock_probe_queue(&self.queue).pending.len()
    }
}

/// Pop the next fair request, releasing the queue lock before any filesystem probe runs.
pub(super) fn take_next_probe(queue: &Mutex<PathProbeQueue>) -> Option<PathProbeRequest> {
    lock_probe_queue(queue).take_next()
}

/// Serve queued probes until the mailbox disconnects or delivery reports a stopped event loop.
///
/// One wake may stand for several queued windows, so each wake drains the fair
/// queue. `classify` sees only immutable request identities, never native windows.
pub(super) fn run_probe_worker(
    wake: &Receiver<()>,
    queue: &Mutex<PathProbeQueue>,
    mut classify: impl FnMut(&Path) -> PathOpenDecision,
    mut deliver: impl FnMut(PathProbeResult) -> bool,
    busy: Option<&super::super::jobs::BusySlot>,
) {
    while wake.recv().is_ok() {
        while let Some(request) = take_next_probe(queue) {
            let outcome = {
                // Execution only, never queue wait, counts toward `path_check`'s age.
                let _busy = busy.map(super::super::jobs::BusySlot::enter);
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

pub(super) fn classify_source_reference(path: &Path) -> PathOpenDecision {
    match classify_local_target(path) {
        PathOpenDecision::Openable(PathKind::File) => PathOpenDecision::SourceReveal,
        PathOpenDecision::Missing => PathOpenDecision::Missing,
        _ => PathOpenDecision::Blocked,
    }
}

pub(super) fn local_target_action(decision: PathOpenDecision) -> Option<LocalTargetAction> {
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

pub(super) fn validate_reveal_target(path: &Path, expected: PathOpenDecision) -> io::Result<()> {
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

pub(super) fn open_path(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
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
pub(super) fn select_openable_candidate(
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
pub(super) fn probe_tiers(
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
pub(super) fn probe_candidates(
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
pub(super) fn contends_with(candidate: &PathProbeCandidate, selected: &PathProbeCandidate) -> bool {
    match (candidate.joined, selected.joined) {
        (true, false) => true,
        (false, true) => false,
        _ => candidate.char_len() <= selected.char_len(),
    }
}

/// Report whether repeating the hover-time selection over `request.contenders` still picks
/// `request.path` with the same decision, so a file that appeared or became blocked since the
/// hover is never skipped in favor of a longer path.
pub(super) fn open_request_still_selected(
    request: &PathOpenRequest,
    classify: impl FnMut(&Path) -> PathOpenDecision,
) -> bool {
    probe_candidates(&request.contenders, classify).is_ok_and(|selection| {
        selection.candidate.resolved_path == request.path
            && selection.decision == request.expected_decision
    })
}

impl PathWorkers {
    /// Start one fair per-window openability worker and one serialized target-open worker.
    pub(in crate::app) fn start(
        proxy: winit::event_loop::EventLoopProxy<super::super::UserEvent>,
    ) -> io::Result<Self> {
        let (probe, wake) = PathProbeMailbox::new();
        let busy = Arc::new(super::super::jobs::PathBusy::new());
        let (probe_busy, open_busy) = (Arc::clone(&busy), Arc::clone(&busy));
        // The worker owns no wake sender, so dropping the App's mailbox ends it.
        let queue = Arc::clone(&probe.queue);
        let probe_proxy = proxy.clone();
        std::thread::Builder::new()
            .name("sonicterm-path-probe".into())
            .spawn(move || {
                let deliver = |result: PathProbeResult| {
                    probe_proxy
                        .send_event(super::super::UserEvent::PathProbeFinished(Box::new(result)))
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
                        let _ = proxy.send_event(super::super::UserEvent::PathOpenFailed {
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
    pub(in crate::app) fn waiting(&self) -> usize {
        self.probe.waiting() + self.open.len()
    }

    /// The workers' busy slots, which the status line's `path_check` reads.
    pub(in crate::app) fn busy(&self) -> &super::super::jobs::PathBusy {
        &self.busy
    }

    pub(in crate::app) fn probe(&self, request: PathProbeRequest) -> io::Result<()> {
        self.probe
            .submit(request)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "path probe worker stopped"))
    }

    /// Drop a closed window's waiting probe without disturbing any other window's request.
    pub(in crate::app) fn cancel_window(&self, window_id: WindowId) {
        self.probe.cancel_window(window_id);
    }

    /// Queue a target open without blocking the event loop.
    ///
    /// `Ok(false)` means the bounded worker already has one running and one
    /// waiting request; the click is still consumed and the extra open drops.
    pub(in crate::app) fn open(
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
#[path = "path_target_probe_worker_tests.rs"]
mod path_target_probe_worker_tests;
