//! Doc-hidden test hooks for governor owners, charges, and retention.

use super::*;

impl App {
    /// Test-only: snapshot the governor's process root.
    #[doc(hidden)]
    pub fn __test_governor_snapshot_root(&self) -> sonicterm_types::ResourceSnapshot {
        self.governor
            .snapshot(self.governor.root_owner())
            .expect("the process root always snapshots")
    }

    /// Test-only: move a pane from one window to another.
    ///
    /// Mirrors what tab tear-out does — remove from the source map, insert
    /// into the destination — so the test exercises the real ownership
    /// consequence rather than a simulation of it.
    #[doc(hidden)]
    pub fn __test_move_pane_between_windows(
        &mut self,
        source: WindowId,
        destination: WindowId,
        pane_id: u64,
    ) -> bool {
        let Some(pane) = self.windows.get_mut(&source).and_then(|w| w.remove_pane(pane_id)) else {
            // When: `source` yields no `pane_id`, so nothing was detached and both
            // windows keep the panes they had.
            return false;
        };
        let Some(window) = self.windows.get_mut(&destination) else {
            // When: `destination` no longer resolves, so the already-removed pane
            // has nowhere to land and drops with its PTY.
            return false;
        };
        window.panes.insert(pane_id, pane);
        self.reattribute_pane_owners();
        true
    }

    /// Test-only: snapshot any owner.
    #[doc(hidden)]
    pub fn __test_owner_snapshot(
        &self,
        owner: ResourceOwnerId,
    ) -> Option<sonicterm_types::ResourceSnapshot> {
        self.governor.snapshot(owner).ok()
    }

    /// Test-only: measure one pane's retention through the reporting seam.
    #[doc(hidden)]
    pub fn __test_pane_retention(
        &self,
        window: WindowId,
        pane_id: u64,
    ) -> Option<retention::PaneRetention> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        retention::measure_pane(pane)
    }

    /// Test-only: the governor amounts a pane currently holds, by class.
    #[doc(hidden)]
    pub fn __test_pane_charges(
        &self,
        window: WindowId,
        pane_id: u64,
    ) -> Option<HashMap<ResourceClass, sonicterm_types::ResourceAmount>> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        Some(pane.charges.iter().map(|(class, held)| (*class, held.committed_amount())).collect())
    }

    /// Test-only: a pane's total charged bytes across every class.
    #[doc(hidden)]
    pub fn __test_pane_charge_total(&self, window: WindowId, pane_id: u64) -> Option<usize> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        Some(pane.charges.values().map(|held| held.committed_amount().bytes).sum())
    }

    /// Test-only: media captures currently in flight on a pane.
    ///
    /// Distinct from the retained-bytes figure: a cancelled capture and a
    /// completed one both report zero bytes, and the slow-transfer test turns
    /// on which of those happened.
    #[doc(hidden)]
    pub fn __test_pane_capture_count(&self, window: WindowId, pane_id: u64) -> Option<usize> {
        let pane = self.windows.get(&window)?.panes.get(&pane_id)?;
        pane.parser.try_lock().map(|parser| parser.live_capture_count())
    }

    /// Test-only: the byte ceiling the governor holds a pane owner to.
    ///
    /// Read back from the ledger rather than from the constant, so a limit that
    /// is computed correctly and never installed fails the assertion that uses
    /// this.
    #[doc(hidden)]
    pub fn __test_pane_owner_limit(&self, window: WindowId) -> Option<usize> {
        let pane = self.windows.get(&window)?.panes.values().next()?;
        let owner = pane.owner.as_ref()?.id();
        self.governor.snapshot(owner).ok().map(|snapshot| snapshot.owner_bytes_limit)
    }

    /// Test-only: set a child pane's scrollback limit.
    #[doc(hidden)]
    pub fn __test_set_child_pane_scrollback(
        &mut self,
        window: WindowId,
        pane_id: u64,
        limit: usize,
    ) -> bool {
        let Some(pane) = self.windows.get_mut(&window).and_then(|w| w.panes.get_mut(&pane_id))
        else {
            // When: neither `window` nor its `panes` resolve the request, so no
            // grid exists whose scrollback `limit` could be set.
            return false;
        };
        pane.parser.lock().grid_mut().set_scrollback_limit(limit);
        true
    }

    /// Test-only: run a retention sample regardless of the interval.
    ///
    /// The production sampler is interval-gated and level-gated, neither of
    /// which a test should wait on or install a subscriber for. This drives
    /// the same charging pass the sampler runs.
    #[doc(hidden)]
    pub fn __test_force_retention_sample(&mut self) {
        self.reconcile_pane_owners();
        self.__test_charge_pane_owners();
    }

    /// Test-only: whether `owner` is still open in the governor.
    ///
    /// A closed owner's record is dropped, so this reports `false` for both a
    /// closed owner and an owner that never existed. That is the answer the
    /// callers want — "is this still holding resources" — and it stays correct
    /// whichever way the ledger represents a finished owner.
    #[doc(hidden)]
    pub fn __test_owner_is_open(&self, owner: ResourceOwnerId) -> bool {
        self.governor
            .snapshot(owner)
            .map(|snapshot| snapshot.owner_state != sonicterm_types::OwnerState::Closed)
            .unwrap_or(false)
    }

    /// Test-only: a window's owner id, if it registered one.
    #[doc(hidden)]
    pub fn __test_window_owner(&self, id: WindowId) -> Option<ResourceOwnerId> {
        self.windows.get(&id).and_then(|window| window.owner.as_ref()).map(OwnerGuard::id)
    }

    /// Test-only: one pane's owner id, if it has one.
    #[doc(hidden)]
    pub fn __test_pane_owner(&self, window: WindowId, pane_id: u64) -> Option<ResourceOwnerId> {
        self.windows
            .get(&window)?
            .panes
            .get(&pane_id)
            .and_then(|pane| pane.owner.as_ref())
            .map(OwnerGuard::id)
    }

    /// Test-only: how many panes in a window have owners.
    #[doc(hidden)]
    pub fn __test_child_pane_owner_count(&self, id: WindowId) -> Option<usize> {
        self.windows
            .get(&id)
            .map(|window| window.panes.values().filter(|pane| pane.owner.is_some()).count())
    }

    /// Test-only: the pane owner ids in a window, sorted for comparison.
    #[doc(hidden)]
    pub fn __test_child_pane_owners(&self, id: WindowId) -> Vec<u64> {
        let mut owners: Vec<u64> = self
            .windows
            .get(&id)
            .map(|window| {
                window
                    .panes
                    .values()
                    .filter_map(|pane| pane.owner.as_ref())
                    .map(|owner| owner.id().get())
                    .collect()
            })
            .unwrap_or_default();
        owners.sort_unstable();
        owners
    }

    /// Test-only invoker for [`Self::reconcile_pane_owners`].
    #[doc(hidden)]
    pub fn __test_reconcile_pane_owners(&mut self) {
        self.reconcile_pane_owners();
    }
}
