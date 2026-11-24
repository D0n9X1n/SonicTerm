//! OSC 52 read tickets: what a read binds when the VT worker admits it, and the bounds it holds.
//!
//! The VT worker admits a read before posting any event: under `deny` it is silent; otherwise it
//! takes this pane's one read slot and one of `MAX_READS` process slots, or is answered empty at
//! once. A [`ReadTicket`] records the pane, PTY incarnation, owner and command epochs and policy
//! generation it was admitted under, so the event loop can cancel it when any of them moves.
//! Dropping a ticket frees both slots.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sonicterm_cfg::config::Osc52ReadPolicy;

use super::osc52_read::reply_labels;
use super::osc52_stream::ClipboardStaging;

/// One pane's read identity, shared by the pane, its VT worker and its intake.
#[derive(Debug, Default)]
pub(in crate::app) struct ReadIdentity {
    /// Bumped whenever the pane leaves a window: a tab move, transfer or tear-out.
    owner_epoch: AtomicU64,
    /// The command boundaries (prompt start, command start and end, RIS) the VT worker published.
    command_epoch: AtomicU64,
    /// The incarnation of the PTY the VT worker reads; 0 for a pane without one.
    incarnation: AtomicU64,
    /// Whether this pane holds a read ticket.
    read_pending: AtomicBool,
}

impl ReadIdentity {
    /// Note that the pane left its window, revoking every read bound to its old owner.
    pub(in crate::app) fn bump_owner(&self) {
        self.owner_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// The owner epoch a read binds now.
    pub(in crate::app) fn owner_epoch(&self) -> u64 {
        self.owner_epoch.load(Ordering::SeqCst)
    }

    /// The command epoch the VT worker last published.
    pub(in crate::app) fn command_epoch(&self) -> u64 {
        self.command_epoch.load(Ordering::SeqCst)
    }

    /// Publish the command epoch after a section's boundaries, before its tickets are posted.
    pub(in crate::app) fn set_command_epoch(&self, epoch: u64) {
        self.command_epoch.store(epoch, Ordering::SeqCst);
    }

    /// Record the incarnation of the PTY the pane's VT worker reads.
    pub(in crate::app) fn set_incarnation(&self, incarnation: u64) {
        self.incarnation.store(incarnation, Ordering::SeqCst);
    }

    /// The incarnation a read binds now.
    pub(in crate::app) fn incarnation(&self) -> u64 {
        self.incarnation.load(Ordering::SeqCst)
    }
}

/// What a ticket was admitted under; revalidation compares each field with the live value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct ReadBinding {
    /// The pane whose program asked.
    pub(in crate::app) pane_id: u64,
    /// Its PTY's incarnation.
    pub(in crate::app) incarnation: u64,
    /// Its owner epoch; a window move bumps it, so it also stands for the window.
    pub(in crate::app) owner_epoch: u64,
    /// The command epoch at the request's position in the output.
    pub(in crate::app) command_epoch: u64,
    /// The process policy generation, sampled before the policy itself.
    pub(in crate::app) policy_generation: u64,
    /// When the VT worker admitted it.
    pub(in crate::app) created_at: Instant,
}

/// A ticket's pane and process slots, freed when dropped.
struct ReadSlot {
    staging: Arc<ClipboardStaging>,
    identity: Arc<ReadIdentity>,
}

// Lifecycle: dropping a ReadSlot clears read_pending and returns its process read slot.
impl Drop for ReadSlot {
    fn drop(&mut self) {
        self.identity.read_pending.store(false, Ordering::SeqCst);
        self.staging.release_read_slot();
    }
}

/// Where a ticket stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum TicketState {
    /// Admitted under `allow`: read with no consent page.
    AutoAllowed,
    /// Admitted under `ask`: waits for an answer, or its 120 s expiry.
    Waiting,
    /// Approved by Apply on its own consent page.
    Approved,
    /// Revoked; it answers nothing.
    Cancelled(CancelReason),
}

/// Why a ticket was revoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum CancelReason {
    /// The pane closed.
    PaneClosed,
    /// The pane left its window since admission.
    OwnerChanged,
    /// A prompt or command boundary passed since the request.
    CommandChanged,
    /// The pane's PTY is not the one that asked.
    PtyChanged,
    /// The pane's window is in READONLY.
    ReadOnly,
    /// `osc52` or `osc52_read` changed.
    PolicyChanged,
    /// A readable ticket outlived its lifetime.
    Expired,
    /// Its consent page closed elsewhere, was replaced, or moved to another window.
    PageLost,
}

/// One admitted read: its binding, reply labels, state and slots.
pub(in crate::app) struct ReadTicket {
    binding: ReadBinding,
    labels: Vec<u8>,
    state: TicketState,
    _slot: ReadSlot,
}

impl ReadTicket {
    /// What the ticket was admitted under.
    pub(in crate::app) fn binding(&self) -> &ReadBinding {
        &self.binding
    }

    /// The reply labels, one per destination, in reply order.
    pub(in crate::app) fn labels(&self) -> &[u8] {
        &self.labels
    }

    /// Where the ticket stands.
    #[cfg(test)]
    pub(in crate::app) fn state(&self) -> TicketState {
        self.state
    }

    /// Whether the ticket may start a native read: auto-allowed, or approved.
    pub(in crate::app) fn readable(&self) -> bool {
        matches!(self.state, TicketState::AutoAllowed | TicketState::Approved)
    }

    /// Approve a waiting ticket, as Apply on its consent page does; any other state is kept.
    pub(in crate::app) fn approve(&mut self) {
        // Only a waiting ticket can be approved; a cancelled one stays cancelled.
        if self.state == TicketState::Waiting {
            self.state = TicketState::Approved;
        }
    }

    /// Revoke the ticket for `reason`; it can no longer read.
    pub(in crate::app) fn cancel(&mut self, reason: CancelReason) {
        self.state = TicketState::Cancelled(reason);
    }

    /// Why the ticket was revoked, if it was.
    pub(in crate::app) fn cancel_reason(&self) -> Option<CancelReason> {
        match self.state {
            TicketState::Cancelled(reason) => Some(reason),
            TicketState::AutoAllowed | TicketState::Waiting | TicketState::Approved => None,
        }
    }
}

/// A ticket posted to the event loop. Clones share one ticket, taken once; `Debug` prints only the
/// pane, so the event can be cloned or logged. Dropping the last clone untaken frees the slots.
#[derive(Clone)]
pub struct ClipboardReadHandle(Arc<std::sync::Mutex<Option<ReadTicket>>>);

impl ClipboardReadHandle {
    /// Share `ticket` for one event.
    pub(in crate::app) fn new(ticket: ReadTicket) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Some(ticket))))
    }

    /// Take the ticket; `None` when a clone already took it.
    pub(in crate::app) fn take(&self) -> Option<ReadTicket> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }
}

impl PartialEq for ClipboardReadHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ClipboardReadHandle {}

impl std::fmt::Debug for ClipboardReadHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pane_id = guard.as_ref().map(|ticket| ticket.binding.pane_id);
        formatter.debug_struct("ClipboardReadHandle").field("pane_id", &pane_id).finish()
    }
}

/// How the VT worker answers one read.
pub(in crate::app) enum Admission {
    /// `osc52_read = "deny"`: no reply, nothing reserved.
    Silent,
    /// The pane or the process holds every slot: answer empty now, before any event is posted.
    Busy,
    /// A ticket to post to the event loop.
    Ticket(ReadTicket),
}

/// Admit one read of `kinds` from `pane_id` at `command_epoch`, the epoch at its position in the
/// output. The policy generation is sampled before the policy, so a reload racing the admission
/// leaves the ticket bound to a generation that no longer matches.
pub(in crate::app) fn admit_read(
    staging: &Arc<ClipboardStaging>,
    identity: &Arc<ReadIdentity>,
    pane_id: u64,
    command_epoch: u64,
    kinds: &[u8],
    now: Instant,
) -> Admission {
    let policy_generation = staging.policy_generation();
    let state = match staging.read_policy() {
        Osc52ReadPolicy::Deny => {
            // When: read_policy is Deny, the read is dropped silently and reserves nothing.
            return Admission::Silent;
        }
        Osc52ReadPolicy::Ask => TicketState::Waiting,
        Osc52ReadPolicy::Allow => TicketState::AutoAllowed,
    };
    if identity.read_pending.swap(true, Ordering::SeqCst) {
        // When: read_pending was already set, this pane holds its one ticket.
        return Admission::Busy;
    }
    if !staging.try_take_read_slot() {
        // When: try_take_read_slot finds all MAX_READS process slots held, the pane slot returns.
        identity.read_pending.store(false, Ordering::SeqCst);
        return Admission::Busy;
    }
    let slot = ReadSlot { staging: Arc::clone(staging), identity: Arc::clone(identity) };
    let binding = ReadBinding {
        pane_id,
        incarnation: identity.incarnation(),
        owner_epoch: identity.owner_epoch(),
        command_epoch,
        policy_generation,
        created_at: now,
    };
    Admission::Ticket(ReadTicket { binding, labels: reply_labels(kinds), state, _slot: slot })
}

#[cfg(test)]
#[path = "osc52_read_ticket_tests.rs"]
mod osc52_read_ticket_tests;
