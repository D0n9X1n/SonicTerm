//! Epoch-bound filesystem probe authorization and selected-target preservation.

use super::*;

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

    pub(in crate::app) fn invalidate(&mut self) -> bool {
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
    pub(in crate::app) fn is_current(&self, key: &PathProbeKey) -> bool {
        self.current.as_ref() == Some(key)
    }

    pub(in crate::app) fn request(&mut self, key: PathProbeKey) -> Option<PathProbeRequest> {
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

    pub(in crate::app) fn accept(
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
    pub(in crate::app) fn accept_waiting_for(&mut self, fresh: &PathProbeKey) {
        if let Some(result) = self.pending_result.take() {
            self.accept(&result, Some(fresh));
        }
    }

    pub(in crate::app) fn authorized_selection(
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
    pub(in crate::app) fn authorized(&self, key: &PathProbeKey, modifier_held: bool) -> bool {
        self.authorized_selection(key, modifier_held).is_some()
    }

    pub(super) fn failure_for(&self, key: &PathProbeKey) -> Option<&'static str> {
        (self.current.as_ref() == Some(key)).then_some(self.failure).flatten()
    }

    #[cfg(test)]
    pub(in crate::app) fn decision_for(&self, key: &PathProbeKey) -> Option<PathOpenDecision> {
        (self.current.as_ref() == Some(key))
            .then(|| self.selection.as_ref().map(|selection| selection.decision))
            .flatten()
    }
}

pub(super) fn same_probe_context(left: &PathProbeKey, right: &PathProbeKey) -> bool {
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

pub(super) fn key_preserves_selection(
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

#[cfg(test)]
#[path = "path_target_probe_state_tests.rs"]
mod path_target_probe_state_tests;
