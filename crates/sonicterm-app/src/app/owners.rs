//! Governor owners: pane seam caps and owner limits, the closing `OwnerGuard`, and pane
//! owner reconciliation and reattribution.

use super::*;

/// One charged pane class and its exact production seam-cap contribution.
///
/// The three grid classes share one storage allocation: `GridVisible` carries
/// that cap, while `GridHistory` and `GridAlternate` carry zero rather than
/// pretending each region may allocate another full grid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneSeamCapTerm {
    /// Class charged by the pane-retention pass.
    pub class: ResourceClass,
    /// Bytes this class contributes to the pane backstop.
    pub bytes: usize,
}

/// Return the exact production pane seam inventory by charged class.
///
/// Every class charged by the pane-retention pass appears exactly once. Tests
/// compare this inventory to that production path, so a new charged class must
/// state its owning cap contribution before the build can pass.
#[must_use]
pub const fn pane_seam_cap_terms() -> [PaneSeamCapTerm; 8] {
    [
        PaneSeamCapTerm {
            class: ResourceClass::GridVisible,
            bytes: sonicterm_grid::grid::MAX_GRID_CELLS as usize
                * std::mem::size_of::<sonicterm_types::Cell>(),
        },
        PaneSeamCapTerm { class: ResourceClass::GridHistory, bytes: 0 },
        PaneSeamCapTerm { class: ResourceClass::GridAlternate, bytes: 0 },
        PaneSeamCapTerm {
            class: ResourceClass::ParserCapture,
            bytes: sonicterm_vt::vt::MAX_MEDIA_PAYLOAD_BYTES
                + sonicterm_vt::vt::MAX_ESCAPE_SEQUENCE_BYTES,
        },
        PaneSeamCapTerm {
            class: ResourceClass::ProtocolMetadata,
            bytes: sonicterm_grid::hyperlink::MAX_HYPERLINK_METADATA_BYTES,
        },
        PaneSeamCapTerm {
            class: ResourceClass::InlineMediaRetained,
            bytes: media::MAX_RETAINED_INLINE_IMAGE_BYTES,
        },
        PaneSeamCapTerm {
            class: ResourceClass::PtyOutput,
            bytes: sonicterm_io::pty::max_queued_output_ring_bytes(),
        },
        PaneSeamCapTerm {
            class: ResourceClass::PtyInput,
            bytes: sonicterm_io::pty::max_pty_queued_input_bytes(),
        },
    ]
}

pub(super) const fn pane_seam_cap_sum_bytes() -> usize {
    let terms = pane_seam_cap_terms();
    let mut total = 0usize;
    let mut index = 0usize;
    while index < terms.len() {
        total += terms[index].bytes;
        index += 1;
    }
    total
}

/// Owner limits: seam caps enforce, the governor backstops.
///
/// Enforcement stays with the per-seam caps that are already tested and
/// falsified. The governor's limit is [`PANE_COMMITTED_BUDGET_BYTES`], derived
/// from those caps and set above them, so it is a tripwire for a seam that has
/// stopped bounding rather than a second bound that must agree with the first.
///
/// Window and process owners stay untracked: their content is the sum of their
/// panes, each already held to its own budget, and a second aggregate limit
/// would be the drift surface this design avoids.
pub(super) fn pane_owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: PANE_COMMITTED_BUDGET_BYTES,
        class_bytes: enum_map::enum_map! { _ => usize::MAX },
        class_items: enum_map::enum_map! { _ => None },
    }
}

/// Owner limits that track without constraining.
///
/// Used for window and process owners, whose retention is the sum of the panes
/// beneath them. Each pane is already held to
/// [`PANE_COMMITTED_BUDGET_BYTES`], so an aggregate limit here would add a
/// second figure to keep in agreement without catching anything the per-pane
/// backstop misses.
pub(super) fn tracking_only_owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map::enum_map! { _ => usize::MAX },
        class_items: enum_map::enum_map! { _ => None },
    }
}

/// Closes a governor owner when the thing that owned it drops.
///
/// The charge on a pane is released by `CommittedReservation::Drop`, and its
/// doc comment states why that is correct: *there is no teardown site to
/// forget*. The owner beside it had no such guarantee — it was a plain
/// `Option<ResourceOwnerId>` that vanished when the pane dropped, leaving the
/// governor holding a record that never closed.
///
/// Measured before this: 80 of 80 owners still `Open` after 40 create/destroy
/// cycles, and `OwnerRegistry` has `get` and `insert` and **no `remove`**, so
/// each one is retained for the life of the process along with its `RwLock`,
/// `Mutex`, and two `EnumMap`s over every resource class.
///
/// Six pane-removal sites across four files reach `panes.remove`. Patching
/// each is how the original defect happened; this makes the close a property
/// of ownership instead.
pub(crate) struct OwnerGuard {
    pub(super) governor: ResourceGovernor,
    owner: ResourceOwnerId,
}

impl OwnerGuard {
    /// Take responsibility for closing `owner` when this drops.
    pub(crate) fn new(governor: ResourceGovernor, owner: ResourceOwnerId) -> Self {
        Self { governor, owner }
    }

    /// The owner this guard will close.
    pub(crate) fn id(&self) -> ResourceOwnerId {
        self.owner
    }
}

impl std::fmt::Debug for OwnerGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("OwnerGuard").field("owner", &self.owner).finish()
    }
}

// Lifecycle: dropping an `OwnerGuard` closes `owner` in the governor, releasing
// its ledger record; a refusal leaves that record retained rather than retried.
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        // Charges must already be gone: `finish_close` refuses an owner still
        // holding them. `PaneState` declares `charges` before `owner`, and
        // Rust drops fields in declaration order, so the reservations release
        // before this runs.
        if let Err(error) = close_owner(&self.governor, self.owner) {
            tracing::warn!(
                target: "memory",
                ?error,
                owner = ?self.owner,
                "owner did not close on drop; its record is retained for the process lifetime"
            );
        }
    }
}

/// Install a provisional pane owner only after every committed charge moves.
pub(super) fn install_transferred_pane_owner(
    pane: &mut PaneState,
    provisional: OwnerGuard,
) -> Result<Option<OwnerGuard>, sonicterm_resource::CommittedBatchTransferError> {
    let owner = provisional.id();
    sonicterm_resource::CommittedReservation::transfer_batch(pane.charges.values_mut(), owner)?;
    Ok(pane.owner.replace(provisional))
}

impl App {
    /// Reconcile pane owners against every window's actual pane set.
    ///
    /// Panes are inserted at a dozen sites, several inside borrows where the
    /// governor is not reachable, and threading registration through all of
    /// them is the "every call site must remember" pattern that produces the
    /// one forgotten site. Reconciling instead means there is no site to
    /// forget: a pane without an owner gets one, and an owner whose pane is
    /// gone is closed.
    ///
    /// Runs from the periodic retention sampler rather than per frame, so its
    /// cost is bounded by that interval regardless of how often panes move.
    pub(super) fn reconcile_pane_owners(&mut self) {
        for window in self.windows.values_mut() {
            window.reconcile_pane_owners();
            for pane in window.panes.values_mut() {
                if pane.reap_slot.is_none() {
                    if let Some(pty) = pane.pty.as_mut() {
                        match self.pty_reaper.reserve(pty) {
                            Ok(slot) => pane.reap_slot = Some(slot),
                            Err(reason) => {
                                tracing::debug!(
                                    target: "sonicterm_app::app",
                                    ?reason,
                                    "PTY teardown reservation refused; retirement will retry once"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Re-parent pane owners whose window has changed, and close their old ones.
    ///
    /// A `PaneState` carries its `owner` field when tab tear-out moves it
    /// between windows, but the owner itself was created *below the source
    /// window's* owner and the governor has no move operation. Left alone, the
    /// source window keeps reporting a pane it no longer has and the
    /// destination reports none for a pane it does — which makes "what does
    /// this window hold" wrong in both directions, and that question is the
    /// entire reason the hierarchy exists.
    ///
    /// Detected by comparing each pane owner's recorded parent against the
    /// window it now lives in, so this needs no hook at the move sites: a pane
    /// that never moved has a matching parent and costs one snapshot read.
    ///
    /// The old owner is closed rather than abandoned. Existing committed
    /// charges move as one class-preserving batch before the guard changes, so
    /// parser contention cannot leave the destination owner empty. The fresh
    /// owner has the same pane limits, so a rejection means an internal ledger
    /// invariant failed; the owned provisional guard closes before that failure
    /// stops the move, while every token remains on the old owner.
    pub(super) fn reattribute_pane_owners(&mut self) {
        let window_ids: Vec<WindowId> = self.windows.keys().copied().collect();
        for window_id in window_ids {
            let Some(window) = self.windows.get(&window_id) else {
                // When: `window_id` no longer resolves, so no pane set remains
                // whose owners could be reattributed.
                continue;
            };
            let Some(window_owner) = window.owner.as_ref().map(OwnerGuard::id) else {
                // When: this `window` holds no owner, so there is no destination
                // parent to move its pane owners onto.
                continue;
            };

            let misattributed: Vec<u64> = window
                .panes
                .iter()
                .filter_map(|(pane_id, pane)| {
                    let owner = pane.owner.as_ref()?.id();
                    let parent = self.governor.snapshot(owner).ok()?.parent?;
                    (parent != window_owner).then_some(*pane_id)
                })
                .collect();

            for pane_id in misattributed {
                let new_owner = match self.governor.create_child(
                    window_owner,
                    OwnerKind::AppPane,
                    pane_owner_limits(),
                ) {
                    Ok(owner) => owner,
                    Err(error) => {
                        // When: `create_child` returns `Err(error)`, no provisional
                        // owner exists and source attribution remains unchanged.
                        tracing::warn!(
                            target: "memory",
                            ?error,
                            pane = pane_id,
                            "pane owner reattribution could not create its destination owner"
                        );
                        continue;
                    }
                };
                let provisional = OwnerGuard::new(self.governor.clone(), new_owner);
                let transferred = {
                    let Some(pane) =
                        self.windows.get_mut(&window_id).and_then(|w| w.panes.get_mut(&pane_id))
                    else {
                        // When: `pane_id` vanished after the owner was created, the
                        // empty provisional guard below must close it immediately.
                        drop(provisional);
                        continue;
                    };
                    install_transferred_pane_owner(pane, provisional)
                };
                match transferred {
                    Ok(stale) => drop(stale),
                    Err(error) => {
                        panic!(
                            "pane {pane_id} owner reattribution violated governor invariants: {error}"
                        );
                    }
                }
            }
        }
        // Ownerless panes may coexist with moved panes when a populated window
        // is first registered; adopt them after reattribution finishes.
        self.reconcile_pane_owners();
    }

    /// Close a window's owner and every pane owner below it.
    ///
    /// Called from window teardown. Owners are closed leaf-first because the
    /// governor refuses to finish closing a parent with open children — which
    /// is the invariant that makes a leaked pane owner visible rather than
    /// silent.
    /// Close the governor owners held by a window already removed from the map.
    ///
    /// Takes the `WindowState` rather than looking it up, because the
    /// production close paths remove the window *before* releasing its
    /// registries — so a lookup-based release returns early and closes
    /// nothing. That is exactly what happened: the release ran, found no
    /// window, and returned, leaving every owner `Open` for the life of the
    /// process.
    pub(super) fn release_owners_of(&mut self, window: &mut WindowState) {
        // Charges first. `finish_close` refuses an owner that still holds
        // them, and the previous order took `pane.owner` while leaving
        // `pane.charges` populated — so every close returned
        // `OwnerHasLiveCharges` and stopped at `Closing`.
        // Charges first, then drop the guards: each closes its owner on drop,
        // and `finish_close` refuses an owner still holding charges.
        for pane in window.panes.values_mut() {
            pane.charges.clear();
            drop(pane.owner.take());
        }
        drop(window.owner.take());
    }

    pub(super) fn release_window_owner(&mut self, id: WindowId) {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` no longer resolves, so the map exposes no guards to
            // drain and the owner records are already unreachable.
            return;
        };
        // Charges must be released before the owner closes.
        //
        // `finish_close` refuses an owner that still holds charges, and this
        // took `pane.owner` while leaving `pane.charges` populated — so every
        // close returned `OwnerHasLiveCharges`, the `let _` discarded it, and
        // the owner stopped at `Closing` forever. Measured: 80 of 80 owners
        // still open after 40 create/destroy cycles.
        //
        // `reattribute_pane_owners` already does this in the right order,
        // twelve lines away.
        for pane in window.panes.values_mut() {
            pane.charges.clear();
            drop(pane.owner.take());
        }
        drop(window.owner.take());
    }
}
