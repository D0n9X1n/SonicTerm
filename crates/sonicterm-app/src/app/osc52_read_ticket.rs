//! OSC 52 consent tickets: what a read, or an `osc52 = "ask"` write, binds when the VT worker
//! admits it, and the bounds it holds.
//!
//! The VT worker admits a read before posting any event: under `deny` it is silent; otherwise it
//! takes this pane's one read slot and one of `MAX_TICKETS` process slots, or is answered empty at
//! once. An asked write takes the pane's one write slot and a process slot the same way, carrying
//! its job, or is refused busy. A [`ConsentTicket`] records the pane, PTY incarnation, owner and
//! command epochs and policy generation it was admitted under, so the event loop can cancel it when
//! any of them moves. Dropping a ticket frees both slots, and a write's job with them.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sonicterm_cfg::config::{Osc52Policy, Osc52ReadPolicy};

use sonicterm_vt::vt::{KittyClipboardLocation, KittyClipboardStatus};

use super::kitty_clipboard_read::{KittyReadRequest, KITTY_READ_RESERVATION_BYTES};
use super::osc52_read::{empty_frames, read_reservation_bytes, reply_labels};
use super::osc52_read_grants::GrantCredential;
use super::osc52_stream::{ClipboardJob, ClipboardStaging};

/// One pane's read identity, shared by the pane, its VT worker and its intake.
#[derive(Debug, Default)]
pub(in crate::app) struct ReadIdentity {
    /// Bumped whenever the pane leaves a window: a tab move, transfer or tear-out.
    owner_epoch: AtomicU64,
    /// The command boundaries (prompt start, command start and end, RIS) the VT worker published.
    command_epoch: AtomicU64,
    /// The incarnation of the PTY the VT worker reads; 0 for a pane without one.
    incarnation: AtomicU64,
    /// The RIS count the VT worker published; a reset revokes every session grant bound before it.
    reset_epoch: AtomicU64,
    /// Whether this pane holds a read ticket.
    read_pending: AtomicBool,
    /// Whether this pane holds an asked write's ticket.
    write_pending: AtomicBool,
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

    /// Note a RIS at its place in the output, revoking every session grant bound before it.
    pub(in crate::app) fn bump_reset(&self) {
        self.reset_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// The RIS count a request binds now.
    pub(in crate::app) fn reset_epoch(&self) -> u64 {
        self.reset_epoch.load(Ordering::SeqCst)
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
    /// The pane's RIS count at admission, which session grants bind.
    pub(in crate::app) reset_epoch: u64,
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
    /// Whether the pane slot is the write slot rather than the read slot.
    write: bool,
}

/// The pane flag a slot of kind `write` holds on `identity`.
fn pending_flag(identity: &ReadIdentity, write: bool) -> &AtomicBool {
    if write {
        &identity.write_pending
    } else {
        // When: write is false, the slot is the pane's read slot.
        &identity.read_pending
    }
}

// Lifecycle: dropping a ReadSlot clears its pending_flag and calls release_ticket_slot.
impl Drop for ReadSlot {
    fn drop(&mut self) {
        pending_flag(&self.identity, self.write).store(false, Ordering::SeqCst);
        self.staging.release_ticket_slot();
    }
}

/// Why no slot was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotRefusal {
    /// The pane already holds its one ticket of this kind.
    PaneBusy,
    /// Every process slot is held.
    ProcessFull,
}

/// Take the pane's flag of kind `write`, then a process ticket slot; refused, holding nothing,
/// when either is taken already. The guard is built only once both are held.
fn take_slot(
    staging: &Arc<ClipboardStaging>,
    identity: &Arc<ReadIdentity>,
    write: bool,
) -> Result<ReadSlot, SlotRefusal> {
    let flag = pending_flag(identity, write);
    if flag.swap(true, Ordering::SeqCst) {
        // When: the pane flag was already set, the pane holds its one ticket of this kind.
        return Err(SlotRefusal::PaneBusy);
    }
    if !staging.try_take_ticket_slot() {
        // When: try_take_ticket_slot finds all MAX_TICKETS process slots held, the pane flag returns.
        flag.store(false, Ordering::SeqCst);
        return Err(SlotRefusal::ProcessFull);
    }
    Ok(ReadSlot { staging: Arc::clone(staging), identity: Arc::clone(identity), write })
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

/// One admitted read or asked write: its binding, reply labels (none for a write), the write's
/// job, its state and its slots.
pub(in crate::app) struct ConsentTicket {
    binding: ReadBinding,
    labels: Vec<u8>,
    /// An asked write's job, holding its lease and program slot, boxed so every ticket stays
    /// small; `None` for a read.
    write: Option<Box<ClipboardJob>>,
    /// An OSC 5522 read's request, boxed so every ticket stays small; `None` for OSC 52 and writes.
    kitty_read: Option<Box<KittyReadRequest>>,
    state: TicketState,
    _slot: ReadSlot,
}

impl ConsentTicket {
    /// What the ticket was admitted under.
    pub(in crate::app) fn binding(&self) -> &ReadBinding {
        &self.binding
    }

    /// The reply labels, one per destination, in reply order.
    pub(in crate::app) fn labels(&self) -> &[u8] {
        &self.labels
    }

    /// Whether the ticket holds an asked write rather than a read.
    pub(in crate::app) fn is_write(&self) -> bool {
        self.write.is_some()
    }

    /// An OSC 5522 read's request, if the ticket holds one.
    pub(in crate::app) fn kitty_read(&self) -> Option<&KittyReadRequest> {
        self.kitty_read.as_deref()
    }

    /// What a session grant for this request is keyed and shown by: whether it writes, its
    /// location and the program's credential; `None` for OSC 52 and for a request without one.
    pub(in crate::app) fn grant_scope(
        &self,
    ) -> Option<(bool, KittyClipboardLocation, &GrantCredential)> {
        if let Some(request) = &self.kitty_read {
            // When: the ticket holds an OSC 5522 read, its request carries the credential.
            return request
                .credential
                .as_ref()
                .map(|credential| (false, request.location, credential));
        }
        let reply = self.write.as_ref()?.kitty.as_ref()?;
        reply.credential().map(|credential| (true, reply.location(), credential))
    }

    /// The reply a read gets when it is not served: `status` for an OSC 5522 read, one empty frame
    /// per label for OSC 52.
    pub(in crate::app) fn failure_reply(&self, status: KittyClipboardStatus) -> Vec<u8> {
        match &self.kitty_read {
            Some(request) => request.status(status),
            None => empty_frames(&self.labels),
        }
    }

    /// The `EBUSY` packet an OSC 5522 read hears when its reply cannot be admitted; `None` for OSC
    /// 52 reads and writes, which have no status.
    pub(in crate::app) fn busy_reply(&self) -> Option<Vec<u8>> {
        let request = self.kitty_read.as_ref()?;
        Some(request.status(KittyClipboardStatus::Ebusy))
    }

    /// What the read reserves from the reply budget before it starts.
    pub(in crate::app) fn reservation_bytes(&self) -> usize {
        match &self.kitty_read {
            Some(_) => KITTY_READ_RESERVATION_BYTES,
            None => read_reservation_bytes(self.labels.len()),
        }
    }

    /// Take an approved write's job, to hand it to the clipboard ordering domain.
    pub(in crate::app) fn take_write(&mut self) -> Option<ClipboardJob> {
        self.write.take().map(|job| *job)
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
pub struct ClipboardTicketHandle(Arc<std::sync::Mutex<Option<ConsentTicket>>>);

impl ClipboardTicketHandle {
    /// Share `ticket` for one event.
    pub(in crate::app) fn new(ticket: ConsentTicket) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Some(ticket))))
    }

    /// Take the ticket; `None` when a clone already took it.
    pub(in crate::app) fn take(&self) -> Option<ConsentTicket> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }
}

impl PartialEq for ClipboardTicketHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ClipboardTicketHandle {}

impl std::fmt::Debug for ClipboardTicketHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pane_id = guard.as_ref().map(|ticket| ticket.binding.pane_id);
        formatter.debug_struct("ClipboardTicketHandle").field("pane_id", &pane_id).finish()
    }
}

/// How the VT worker routes one complete write.
pub(in crate::app) enum WriteAdmission {
    /// `allow` or `deny`: post the job; the event loop applies `deny` before the clipboard.
    Post(ClipboardJob),
    /// `ask`, while the pane or the process holds every slot: refused busy, the job dropped.
    Busy,
    /// `ask`: a ticket holding the job, posted to the event loop for its consent page.
    Ticket(ConsentTicket),
}

/// Admit one complete `job` from `pane_id` at `command_epoch`: under `ask` it takes the pane's one
/// write slot and a process slot and waits for consent; otherwise it is posted. The generation is
/// sampled before the policy, as a read's is.
pub(in crate::app) fn admit_write(
    staging: &Arc<ClipboardStaging>,
    identity: &Arc<ReadIdentity>,
    pane_id: u64,
    command_epoch: u64,
    job: ClipboardJob,
    now: Instant,
) -> WriteAdmission {
    let policy_generation = staging.policy_generation();
    if staging.write_policy() != Osc52Policy::Ask {
        // When: write_policy is allow or deny, no consent is asked for.
        return WriteAdmission::Post(job);
    }
    let Ok(slot) = take_slot(staging, identity, true) else {
        // When: take_slot finds the pane's write slot or every process slot held, it is busy.
        return WriteAdmission::Busy;
    };
    let binding = ReadBinding {
        pane_id,
        incarnation: identity.incarnation(),
        owner_epoch: identity.owner_epoch(),
        reset_epoch: identity.reset_epoch(),
        command_epoch,
        policy_generation,
        created_at: now,
    };
    WriteAdmission::Ticket(ConsentTicket {
        binding,
        labels: Vec::new(),
        write: Some(Box::new(job)),
        kitty_read: None,
        state: TicketState::Waiting,
        _slot: slot,
    })
}

/// How the VT worker answers one read.
pub(in crate::app) enum Admission {
    /// `osc52_read = "deny"`: no reply, nothing reserved.
    Silent,
    /// The pane or the process holds every slot: answer empty now, before any event is posted.
    Busy,
    /// A ticket to post to the event loop.
    Ticket(ConsentTicket),
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
    let Ok(slot) = take_slot(staging, identity, false) else {
        // When: take_slot finds the pane's read slot or every process slot held, it is busy.
        return Admission::Busy;
    };
    let binding = ReadBinding {
        pane_id,
        incarnation: identity.incarnation(),
        owner_epoch: identity.owner_epoch(),
        reset_epoch: identity.reset_epoch(),
        command_epoch,
        policy_generation,
        created_at: now,
    };
    let labels = reply_labels(kinds);
    let ticket =
        ConsentTicket { binding, labels, write: None, kitty_read: None, state, _slot: slot };
    Admission::Ticket(ticket)
}

/// Admit one OSC 5522 read from `pane_id` at `command_epoch`: under `deny`, or while this pane
/// reads, the reply is `EPERM`; with every process slot held it is `EBUSY`. A listing-only request
/// reads no content, so it skips the prompt under `ask`.
pub(in crate::app) fn admit_kitty_read(
    staging: &Arc<ClipboardStaging>,
    identity: &Arc<ReadIdentity>,
    pane_id: u64,
    command_epoch: u64,
    request: KittyReadRequest,
    now: Instant,
) -> Result<ConsentTicket, Vec<u8>> {
    let policy_generation = staging.policy_generation();
    let state = match staging.read_policy() {
        Osc52ReadPolicy::Deny => {
            // When: read_policy is Deny, the read is refused `EPERM` and reserves nothing.
            return Err(request.status(KittyClipboardStatus::Eperm));
        }
        Osc52ReadPolicy::Ask if !request.listing_only() => TicketState::Waiting,
        Osc52ReadPolicy::Ask | Osc52ReadPolicy::Allow => TicketState::AutoAllowed,
    };
    let slot = match take_slot(staging, identity, false) {
        Ok(slot) => slot,
        Err(SlotRefusal::PaneBusy) => {
            // When: take_slot finds this pane already reading, the read is refused `EPERM`.
            return Err(request.status(KittyClipboardStatus::Eperm));
        }
        Err(SlotRefusal::ProcessFull) => {
            // When: take_slot finds every process slot held, the read is busy, `EBUSY`.
            return Err(request.status(KittyClipboardStatus::Ebusy));
        }
    };
    let binding = ReadBinding {
        pane_id,
        incarnation: identity.incarnation(),
        owner_epoch: identity.owner_epoch(),
        reset_epoch: identity.reset_epoch(),
        command_epoch,
        policy_generation,
        created_at: now,
    };
    let kitty_read = Some(Box::new(request));
    Ok(ConsentTicket { binding, labels: Vec::new(), write: None, kitty_read, state, _slot: slot })
}

#[cfg(test)]
#[path = "osc52_read_ticket_tests.rs"]
mod osc52_read_ticket_tests;
