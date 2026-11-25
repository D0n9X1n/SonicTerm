//! The event loop's side of OSC 52 reads: an `ask` ticket waits for its consent page
//! (`osc52_read_consent.rs`) and an `allow` or approved ticket queues; each is revalidated before
//! its native read and before every delivery attempt; one read at a time runs on a disposable
//! reader thread with a deadline; and a reply reaches only the source PTY's
//! sensitive-reply slot, with the ticket and its budget lease as custody, never the reply spool, a
//! broadcast receiver or a log.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use sonicterm_cfg::config::Config;
use sonicterm_io::pty::{SensitiveReply, SensitiveReplyError};
use winit::event_loop::{ActiveEventLoop, ControlFlow};

use super::osc52_read::{READ_REPLY_BUDGET_BYTES, TICKET_LIFETIME};
use sonicterm_vt::vt::KittyClipboardStatus;

use super::kitty_clipboard_read::build_read_reply;
use super::osc52_read_grants::{
    GrantBoundSource, GrantClock, GrantDecision, GrantKey, PasteApproval, PasteBinding, PasteGrant,
    SessionGrants, PASTE_GRANT_LIFETIME,
};
use super::osc52_read_source::{read_replies, ClipboardTextSource, NativeSource, ReadLocation};
use super::osc52_read_ticket::{CancelReason, ClipboardTicketHandle, ConsentTicket, ReadBinding};
use super::osc52_stream::{ClipLease, ClipboardStaging};
use super::UserEvent;

/// Retries after a contended writer admission; each runs on a later event-loop turn.
pub(in crate::app) const MAX_DELIVERY_RETRIES: u32 = 3;

/// How long one destination's native read may run before its reply is answered empty: on Linux the
/// selection wait plus a whole `INCR` transfer, elsewhere the native call's own 2 s bound.
#[cfg(target_os = "linux")]
const READER_DEADLINE_PER_DESTINATION: Duration = Duration::from_secs(7);
#[cfg(not(target_os = "linux"))]
const READER_DEADLINE_PER_DESTINATION: Duration = Duration::from_secs(2);

/// The one native read outstanding.
pub(in crate::app) struct InFlight {
    binding: ReadBinding,
    /// The reply a read past its deadline gets: empty frames for OSC 52, `EIO` for OSC 5522.
    failure: Vec<u8>,
    deadline: Instant,
    /// Answered empty at its deadline; its late result is dropped when the native call returns.
    abandoned: bool,
    /// The `EBUSY` packet an OSC 5522 read hears when its reply cannot be admitted.
    busy: Option<Vec<u8>>,
}

/// A reply whose writer admission was contended, retried on a later turn.
struct Retry {
    binding: ReadBinding,
    reply: SensitiveReply,
    attempts: u32,
    check_age: bool,
    busy: Option<Vec<u8>>,
}

/// What a read reply keeps until its bytes are written: its ticket, and the reply-budget lease of
/// a served read. Typed, so a reply refused because its paste approval ended returns its ticket to
/// `waiting`. Fields drop in order: the ticket's slots, then the lease.
struct ReadCustody {
    ticket: ConsentTicket,
    lease: Option<ClipLease>,
}

/// What a reader thread hands back once its native calls return.
struct ReaderDone {
    /// The replies, or `None` when a paste-approved read found its offer changed before or after
    /// reading, so whatever it read was dropped on the reader thread.
    replies: Option<Vec<u8>>,
    ticket: ConsentTicket,
    lease: ClipLease,
    /// When the native calls returned; a result completed at or past its deadline is never
    /// delivered, however soon the event loop drains it.
    completed_at: Instant,
}

/// A test's stand-in for the source PTY's sensitive-reply slot.
#[cfg(test)]
pub(in crate::app) type ReplyPort =
    Box<dyn FnMut(u64, SensitiveReply) -> Result<(), SensitiveReplyError>>;

/// The App's OSC 52 read state.
pub(in crate::app) struct ClipboardReads {
    /// Where native reads come from; tests inject a fake.
    pub(in crate::app) source: Arc<dyn ClipboardTextSource>,
    /// The reply budget every read reserves its raw text and frames from before it starts.
    pub(in crate::app) budget: Arc<ClipboardStaging>,
    /// The deadline per destination a native read gets.
    pub(in crate::app) reader_deadline: Duration,
    /// `ask` tickets with no page yet, oldest first.
    pub(in crate::app) waiting: Vec<ConsentTicket>,
    /// The ticket on the palette's consent page; at most one, as the palette is modal.
    pub(in crate::app) presented: Option<super::osc52_read_consent::PresentedTicket>,
    /// Readable tickets waiting for the one native reader.
    pub(in crate::app) queued: VecDeque<ConsentTicket>,
    pub(in crate::app) in_flight: Option<InFlight>,
    retries: Vec<Retry>,
    done_tx: Sender<ReaderDone>,
    done_rx: Receiver<ReaderDone>,
    /// The clock consumed paste grants' ends are checked against; tests move it.
    pub(in crate::app) grant_clock: GrantClock,
    /// Test-only: refuse to start the reader, running the callback at that moment.
    #[cfg(test)]
    pub(in crate::app) fail_spawn: Option<Box<dyn Fn()>>,
    /// Why the last ticket or reply was revoked, for diagnostics.
    pub(in crate::app) last_cancel: Option<CancelReason>,
    /// The Always and Ban answers OSC 5522 programs named themselves for.
    pub(in crate::app) grants: SessionGrants,
    /// The one-time grants DECSET 5522 paste events issued, one per pane at most.
    pub(in crate::app) paste_grants: super::osc52_read_grants::PasteGrants,
    /// Paste events waiting for, or being built by, the off-loop offer capture.
    pub(in crate::app) paste_capture: super::osc52_read_paste::PasteCapture,
    /// Replaces the PTY's sensitive-reply slot in tests.
    #[cfg(test)]
    pub(in crate::app) port: Option<ReplyPort>,
}

impl Default for ClipboardReads {
    fn default() -> Self {
        // One reader runs at a time, so at most one result waits.
        let (done_tx, done_rx) = crossbeam_channel::unbounded();
        Self {
            source: Arc::new(NativeSource),
            budget: ClipboardStaging::with_budget(READ_REPLY_BUDGET_BYTES, 0),
            reader_deadline: READER_DEADLINE_PER_DESTINATION,
            waiting: Vec::new(),
            presented: None,
            queued: VecDeque::new(),
            in_flight: None,
            retries: Vec::new(),
            done_tx,
            done_rx,
            grant_clock: GrantClock::default(),
            #[cfg(test)]
            fail_spawn: None,
            last_cancel: None,
            grants: SessionGrants::default(),
            paste_grants: super::osc52_read_grants::PasteGrants::default(),
            paste_capture: super::osc52_read_paste::PasteCapture::default(),
            #[cfg(test)]
            port: None,
        }
    }
}

impl std::fmt::Debug for ClipboardReads {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClipboardReads")
            .field("waiting", &self.waiting.len())
            .field("presented", &self.presented.is_some())
            .field("queued", &self.queued.len())
            .field("in_flight", &self.in_flight.is_some())
            .field("retries", &self.retries.len())
            .field("last_cancel", &self.last_cancel)
            .field("grants", &self.grants.len())
            .finish()
    }
}

impl ClipboardReads {
    /// Test-only: whether the reader's result waits to be settled, as (a copy of its replies,
    /// completed before its deadline); the result is put back untouched.
    #[cfg(test)]
    pub(in crate::app) fn peek_finished_read(&self) -> Option<(Option<Vec<u8>>, bool)> {
        let done = self.done_rx.try_recv().ok()?;
        let deadline = self.in_flight.as_ref().map(|in_flight| in_flight.deadline);
        let seen = (done.replies.clone(), deadline.is_some_and(|at| done.completed_at < at));
        let _ = self.done_tx.send(done);
        Some(seen)
    }

    /// Whether nothing waits, is presented, queues, reads, retries or holds a grant to purge.
    fn idle(&self) -> bool {
        self.waiting.is_empty()
            && self.presented.is_none()
            && self.queued.is_empty()
            && self.in_flight.is_none()
            && self.retries.is_empty()
            && self.grants.is_empty()
            && self.paste_grants.is_empty()
            && self.paste_capture.is_idle()
    }
}

impl super::App {
    /// Take a ticket the VT worker posted: cancel it when it no longer matches, else wait (`ask`)
    /// or queue (`allow`) it and run the pipeline.
    pub(in crate::app) fn admit_clipboard_ticket(&mut self, read: ClipboardTicketHandle) {
        let Some(ticket) = read.take() else {
            // When: take finds the ticket gone, a clone of the event already admitted it.
            return;
        };
        if let Err(reason) = self.revalidate_ticket(ticket.binding(), Instant::now(), false) {
            // When: revalidate_ticket refuses, the ticket ends silently.
            self.cancel_ticket(ticket, reason);
            return;
        }
        let grant =
            self.grant_key(&ticket).and_then(|key| self.clip_state.reads.grants.lookup(&key));
        // A paste event's one-time grant is consumed by the first named read that carries its
        // password, whatever answers the read, so a replay never finds it.
        let pasted = self.consume_paste_grant(&ticket);
        match (grant, pasted) {
            (Some(GrantDecision::Ban), _) => {
                // A Ban grant refuses the request `EPERM` without a page, before a listing-only
                // read readable under `ask` is queued: Ban covers every read under its password.
                self.refuse_ticket(ticket, Instant::now());
            }
            _ if ticket.readable() => self.clip_state.reads.queued.push_back(ticket),
            (Some(GrantDecision::Always), _) => {
                // An Always grant approves a waiting ticket without a page.
                let mut ticket = ticket;
                ticket.approve();
                if ticket.is_write() {
                    self.submit_approved_write(ticket);
                } else {
                    // When: the ticket is a read, it queues for the one native reader.
                    self.clip_state.reads.queued.push_back(ticket);
                }
            }
            (None, Some(approval)) => {
                // A live paste grant approves this one read without a page, bound to its recipient
                // and offer until delivery.
                let mut ticket = ticket;
                ticket.approve_by_paste(approval);
                self.clip_state.reads.queued.push_back(ticket);
            }
            (None, None) => {
                // With no grant, the ticket waits for an answer or its expiry.
                self.clip_state.reads.waiting.push(ticket);
            }
        }
        self.pump_clipboard_reads();
    }

    /// Take the paste grant `ticket`'s read names by its pane, location and password, and return
    /// the approval it gives while it still holds: its recipient's identity and DECSET 5522 word
    /// unchanged since the event, the ticket admitted under that same reset count, and within its
    /// lifetime. Taken either way. The offer is not read here: the reader compares the approval's
    /// offer version before and after its native read, off the event loop.
    fn consume_paste_grant(&mut self, ticket: &ConsentTicket) -> Option<PasteApproval> {
        let Some(request) = ticket.kitty_read() else {
            // When: the ticket is no OSC 5522 read, no paste grant can answer it.
            return None;
        };
        let Some(credential) = request.credential.as_ref() else {
            // When: request.credential is None (no nonempty name and pw), nothing is consumed.
            return None;
        };
        let pane_id = ticket.binding().pane_id;
        let grants = &mut self.clip_state.reads.paste_grants;
        let Some(grant) = grants.take_for(pane_id, request.location, credential) else {
            // When: take_for finds no grant for this pane, location and password, none applies.
            return None;
        };
        let now = self.clip_state.reads.grant_clock.now();
        if !self.paste_grant_live(&grant, now)
            || grant.binding.reset_epoch != ticket.binding().reset_epoch
        {
            // When: the recipient changed or the grant expired, the read takes normal consent.
            return None;
        }
        let location = match request.location {
            sonicterm_vt::vt::KittyClipboardLocation::Standard => ReadLocation::Clipboard,
            sonicterm_vt::vt::KittyClipboardLocation::Primary => ReadLocation::Primary,
        };
        let expires_at = grant.issued_at + PASTE_GRANT_LIFETIME;
        let (binding, offer_version) = (grant.binding, grant.offer_version);
        Some(PasteApproval { binding, location, offer_version, expires_at })
    }

    /// Whether `grant`'s recipient still has the identity it was issued to and it is young enough:
    /// a grant ends at exactly its lifetime, the instant its expiry wake is scheduled for.
    fn paste_grant_live(&self, grant: &PasteGrant, now: Instant) -> bool {
        let fresh = now.saturating_duration_since(grant.issued_at) < PASTE_GRANT_LIFETIME;
        fresh && self.paste_binding(grant.binding.pane_id).as_ref() == Some(&grant.binding)
    }

    /// The identity a paste grant for `pane_id` binds now: its PTY incarnation (which its VT
    /// worker must agree with), owner and RIS epochs, published DECSET 5522 word and the policy
    /// generation; `None` for a closed pane or one whose worker reads another PTY.
    pub(in crate::app) fn paste_binding(&self, pane_id: u64) -> Option<PasteBinding> {
        let pane = self.pane_by_id(pane_id)?;
        let identity = &pane.read_identity;
        let incarnation = pane.pty.as_ref().map_or(0, |pty| pty.incarnation());
        if identity.incarnation() != incarnation {
            // When: the worker's incarnation differs from the pane's PTY, the recipient moved on.
            return None;
        }
        Some(PasteBinding {
            pane_id,
            incarnation,
            owner_epoch: identity.owner_epoch(),
            reset_epoch: identity.reset_epoch(),
            paste_mode: pane.paste_events_mode(),
            policy_generation: self.clip_state.staging.policy_generation(),
        })
    }

    /// The session-grant key `ticket` is looked up and remembered by; `None` without a credential.
    pub(in crate::app) fn grant_key(&self, ticket: &ConsentTicket) -> Option<GrantKey> {
        let (write, location, credential) = ticket.grant_scope()?;
        Some(GrantKey::new(ticket.binding(), write, location, credential))
    }

    /// Drop every grant whose pane closed, or whose PTY, owner or RIS count moved since it was made,
    /// and every paste grant no longer live at `now`.
    fn purge_grants(&mut self, now: Instant) {
        if !self.clip_state.reads.paste_grants.is_empty() {
            // Each held paste grant is dropped once its recipient or its age ends it.
            let mut paste_grants = std::mem::take(&mut self.clip_state.reads.paste_grants);
            paste_grants.retain(|grant| self.paste_grant_live(grant, now));
            self.clip_state.reads.paste_grants = paste_grants;
        }
        if self.clip_state.reads.grants.is_empty() {
            // When: grants is_empty, there is nothing to purge.
            return;
        }
        let mut grants = std::mem::take(&mut self.clip_state.reads.grants);
        grants.retain(|key| {
            self.pane_by_id(key.pane_id).is_some_and(|pane| {
                let identity = &pane.read_identity;
                pane.pty.as_ref().map_or(0, |pty| pty.incarnation()) == key.incarnation
                    && identity.incarnation() == key.incarnation
                    && identity.owner_epoch() == key.owner_epoch
                    && identity.reset_epoch() == key.reset_epoch
            })
        });
        self.clip_state.reads.grants = grants;
    }

    /// Whether `binding` still names a live read: the pane, its PTY, owner and command epochs and
    /// the policy generation unchanged, its window not READONLY and, with `check_age`, young enough.
    pub(in crate::app) fn revalidate_ticket(
        &self,
        binding: &ReadBinding,
        now: Instant,
        check_age: bool,
    ) -> Result<(), CancelReason> {
        let Some(pane) = self.pane_by_id(binding.pane_id) else {
            // When: pane_by_id finds no pane, it closed.
            return Err(CancelReason::PaneClosed);
        };
        if pane.pty.as_ref().map_or(0, |pty| pty.incarnation()) != binding.incarnation {
            // When: the pane's PTY incarnation differs, another PTY would get the reply.
            return Err(CancelReason::PtyChanged);
        }
        if pane.read_identity.owner_epoch() != binding.owner_epoch {
            // When: owner_epoch moved, the pane left its window since admission.
            return Err(CancelReason::OwnerChanged);
        }
        if pane.read_identity.command_epoch() != binding.command_epoch {
            // When: command_epoch moved, the program that asked may be gone.
            return Err(CancelReason::CommandChanged);
        }
        if self.clip_state.staging.policy_generation() != binding.policy_generation {
            // When: policy_generation moved, osc52 or osc52_read changed since admission.
            return Err(CancelReason::PolicyChanged);
        }
        if !self.admits_new_user_input(binding.pane_id) {
            // When: admits_new_user_input refuses, the window is READONLY.
            return Err(CancelReason::ReadOnly);
        }
        // A grant's end is read on the grant clock, never earlier than `now`.
        let grant_now = now.max(self.clip_state.reads.grant_clock.now());
        let live_paste = |paste: &PasteApproval| {
            grant_now < paste.expires_at
                && self.paste_binding(binding.pane_id).as_ref() == Some(&paste.binding)
        };
        if binding.paste.as_deref().is_some_and(|paste| !live_paste(paste)) {
            // When: a paste-approved read's grant reached its expires_at, or its recipient reset
            // DECSET 5522, RIS or moved, the paste approval ended; the read itself may still ask.
            return Err(CancelReason::PasteRevoked);
        }
        if check_age && now.saturating_duration_since(binding.created_at) > TICKET_LIFETIME {
            // When: check_age is set and the binding outlived TICKET_LIFETIME, it expired.
            return Err(CancelReason::Expired);
        }
        Ok(())
    }

    /// Revoke `ticket`; dropping it frees its slots. Only an OSC 5522 write is answered, `EPERM`,
    /// and only while the program that asked still reads the pane: the same PTY and command.
    pub(in crate::app) fn cancel_ticket(
        &mut self,
        mut ticket: ConsentTicket,
        reason: CancelReason,
    ) {
        ticket.cancel(reason);
        let pane_id = ticket.binding().pane_id;
        tracing::debug!(pane_id, reason = ?ticket.cancel_reason(), "OSC 52 read cancelled");
        self.clip_state.reads.last_cancel = Some(reason);
        let write = ticket.take_write();
        drop(ticket);
        if let Some(job) = write {
            // The waiting program hears the refusal; another PTY or command would read the
            // status as input, so the binding withholds it there.
            self.deny_write_status(pane_id, job);
        }
    }

    /// Answer a refused asked write `EPERM` while its program still reads the pane; dropping
    /// `job` frees its lease and slot.
    fn deny_write_status(&mut self, pane_id: u64, job: super::osc52_stream::ClipboardJob) {
        let (reply, binding) = (job.kitty.clone(), job.binding.clone());
        drop(job);
        let denied = super::event_loop::ClipWriteOutcome::Denied;
        self.send_kitty_status(pane_id, reply, &binding, denied);
    }

    /// Run the read pipeline now.
    pub(in crate::app) fn pump_clipboard_reads(&mut self) {
        self.pump_clipboard_reads_at(Instant::now());
    }

    /// Run the read pipeline at `now`: purge stale grants, retry contended replies first, so a reply contended this
    /// turn waits for the next, then settle the reader, sweep tickets and the consent page,
    /// start the next read, then show the next page.
    pub(in crate::app) fn pump_clipboard_reads_at(&mut self, now: Instant) {
        self.purge_grants(now);
        self.pump_paste_capture(now);
        self.retry_read_replies(now);
        self.settle_reader(now);
        self.sweep_read_tickets(now);
        self.sweep_consent_page(now);
        self.start_reader(now);
        // Presented last, so a ticket any step above returned to `waiting`, starting the reader
        // included, gets its page this turn rather than at some later event.
        self.present_consent_page();
    }

    /// Deliver a finished read, drop a late one, and answer a read past its deadline empty.
    fn settle_reader(&mut self, now: Instant) {
        while let Ok(done) = self.clip_state.reads.done_rx.try_recv() {
            let Some(in_flight) = self.clip_state.reads.in_flight.take() else {
                // When: in_flight.take() is None, no read is recorded, so nothing is delivered.
                continue;
            };
            if in_flight.abandoned {
                // When: abandoned, the read was answered at its deadline; only now, after the
                // native call returned, are its ticket and lease released.
                tracing::debug!(pane_id = in_flight.binding.pane_id, "late OSC 52 read dropped");
                continue;
            }
            if done.completed_at >= in_flight.deadline {
                // When: completed_at reached the deadline, the read is late: its data and lease
                // are dropped, and it is answered as an overdue read is, the ticket as custody.
                let ReaderDone { replies, ticket, lease, .. } = done;
                drop((replies, lease));
                tracing::warn!(
                    pane_id = in_flight.binding.pane_id,
                    "OSC 52 read passed its deadline"
                );
                let custody = ReadCustody { ticket, lease: None };
                let reply = SensitiveReply::new(in_flight.failure, Box::new(custody));
                self.deliver_read_reply(in_flight.binding, reply, now, false, 0, in_flight.busy);
                continue;
            }
            let ReaderDone { replies, mut ticket, lease, .. } = done;
            let revoked = self.revalidate_ticket(ticket.binding(), now, true)
                == Err(CancelReason::PasteRevoked);
            let Some(replies) = replies.filter(|_| !revoked) else {
                // When: replies is None (the offer changed across the read) or revoked (its paste
                // approval ended while it ran), the data is dropped; the read waits to be answered.
                drop(lease);
                tracing::debug!(pane_id = in_flight.binding.pane_id, "paste approval withdrawn");
                ticket.withdraw_paste();
                self.clip_state.reads.waiting.push(ticket);
                continue;
            };
            let custody = ReadCustody { ticket, lease: Some(lease) };
            let reply = SensitiveReply::new(replies, Box::new(custody));
            self.deliver_read_reply(in_flight.binding, reply, now, true, 0, in_flight.busy);
        }
        let mut overdue = None;
        if let Some(in_flight) = self.clip_state.reads.in_flight.as_mut() {
            if !in_flight.abandoned && now >= in_flight.deadline {
                // Past its deadline the read is answered empty; the thread keeps its slot.
                in_flight.abandoned = true;
                let busy = in_flight.busy.clone();
                overdue = Some((in_flight.binding.clone(), in_flight.failure.clone(), busy));
            }
        }
        if let Some((binding, replies, busy)) = overdue {
            tracing::warn!(pane_id = binding.pane_id, "OSC 52 read passed its deadline");
            self.deliver_read_reply(
                binding,
                SensitiveReply::new(replies, Box::new(())),
                now,
                false,
                0,
                busy,
            );
        }
    }

    /// Cancel tickets that no longer match and answer `ask` tickets past their lifetime.
    fn sweep_read_tickets(&mut self, now: Instant) {
        for ticket in std::mem::take(&mut self.clip_state.reads.waiting) {
            if let Err(reason) = self.revalidate_ticket(ticket.binding(), now, false) {
                // When: revalidate_ticket refuses, the waiting ticket ends silently.
                self.cancel_ticket(ticket, reason);
                continue;
            }
            // A ticket whose page never opened is answered at expiry as a declined prompt is.
            if now.saturating_duration_since(ticket.binding().created_at) >= TICKET_LIFETIME {
                // When: the ticket's age reaches TICKET_LIFETIME, it is answered empty.
                self.refuse_ticket(ticket, now);
                continue;
            }
            self.clip_state.reads.waiting.push(ticket);
        }
        for mut ticket in std::mem::take(&mut self.clip_state.reads.queued) {
            match self.revalidate_ticket(ticket.binding(), now, true) {
                Ok(()) => self.clip_state.reads.queued.push_back(ticket),
                Err(CancelReason::PasteRevoked) => {
                    // Only its paste approval ended, so the read waits for an ordinary answer.
                    ticket.withdraw_paste();
                    self.clip_state.reads.waiting.push(ticket);
                }
                Err(reason) => {
                    // Any other refusal before the read starts ends the ticket.
                    self.cancel_ticket(ticket, reason);
                }
            }
        }
    }

    /// Answer a refused read with one empty reply per label, the ticket riding along as custody;
    /// a refused asked write is dropped, writing nothing, and the clip segment says it was denied.
    pub(in crate::app) fn refuse_ticket(&mut self, ticket: ConsentTicket, now: Instant) {
        self.refuse_ticket_with(ticket, now, KittyClipboardStatus::Eperm);
    }

    /// [`Self::refuse_ticket`], answering an OSC 5522 read with `status`: `EPERM` for a refusal,
    /// `EBUSY` for queue pressure or a full reply budget.
    fn refuse_ticket_with(
        &mut self,
        ticket: ConsentTicket,
        now: Instant,
        status: KittyClipboardStatus,
    ) {
        if ticket.is_write() {
            // When: the ticket holds a write, only an OSC 5522 write is answered (`EPERM`);
            // dropping the ticket frees the job.
            let pane_id = ticket.binding().pane_id;
            let mut ticket = ticket;
            let write = ticket.take_write();
            drop(ticket);
            if let Some(job) = write {
                self.deny_write_status(pane_id, job);
            }
            tracing::debug!(pane_id, "OSC 52 asked write refused");
            self.show_clip_outcome(pane_id, super::event_loop::ClipWriteOutcome::Denied, 0, now);
            return;
        }
        let binding = ticket.binding().clone();
        let replies = ticket.failure_reply(status);
        let busy = ticket.busy_reply();
        self.deliver_read_reply(
            binding,
            SensitiveReply::new(replies, Box::new(ReadCustody { ticket, lease: None })),
            now,
            false,
            0,
            busy,
        );
    }

    /// Start the next queued read when no native read is outstanding; while one is past its
    /// deadline, queued reads fail fast with empty replies and no native call.
    fn start_reader(&mut self, now: Instant) {
        loop {
            let wedged = match &self.clip_state.reads.in_flight {
                None => false,
                Some(in_flight) if !in_flight.abandoned => {
                    // When: in_flight is not abandoned, a read runs within its deadline: wait.
                    return;
                }
                Some(_) => true,
            };
            let Some(ticket) = self.clip_state.reads.queued.pop_front() else {
                // When: pop_front finds no queued ticket, nothing starts.
                return;
            };
            if wedged {
                // When: wedged, a native read is still outstanding, so this one fails fast.
                self.refuse_ticket_with(ticket, now, KittyClipboardStatus::Ebusy);
                continue;
            }
            let mut lease = self.clip_state.reads.budget.lease();
            if !lease.grow_to(ticket.reservation_bytes()) {
                // When: grow_to fails, the reply budget is busy and the read is answered empty
                // (OSC 5522: `EBUSY`).
                self.refuse_ticket_with(ticket, now, KittyClipboardStatus::Ebusy);
                continue;
            }
            self.spawn_reader(ticket, lease, now);
        }
    }

    /// Run `ticket`'s native reads on a disposable thread holding the ticket and `lease` until the
    /// calls return; the event loop waits only until the deadline.
    fn spawn_reader(&mut self, ticket: ConsentTicket, lease: ClipLease, now: Instant) {
        let binding = ticket.binding().clone();
        let failure = ticket.failure_reply(KittyClipboardStatus::Eio);
        let busy = ticket.busy_reply();
        let reads = &self.clip_state.reads;
        let destination_count = match ticket.kitty_read() {
            Some(request) => request.types.len() + usize::from(request.listing),
            None => ticket.labels().len(),
        };
        let destinations = u32::try_from(destination_count.max(1)).unwrap_or(u32::MAX);
        let deadline = now + reads.reader_deadline.saturating_mul(destinations);
        let source = Arc::clone(&reads.source);
        let done_tx = reads.done_tx.clone();
        let proxy = self.event_loop_proxy.clone();
        let thread_failure = failure.clone();
        // A paste-approved read is bound to the offer its event announced.
        let paste = ticket.paste_approval().map(|paste| (paste.location, paste.offer_version));
        // The grant's end travels with the read, so the reader checks it against the clock.
        let expires_at = ticket.paste_approval().map(|paste| paste.expires_at);
        let clock = self.clip_state.reads.grant_clock.clone();
        // The ticket and lease reach the reader only once it started, so a reader that cannot
        // start leaves both here, to be answered through their custody.
        let (handoff_tx, handoff_rx) = crossbeam_channel::bounded::<(ConsentTicket, ClipLease)>(1);
        let spawned =
            std::thread::Builder::new().name("sonicterm-clipboard-read".into()).spawn(move || {
                let Ok((ticket, mut lease)) = handoff_rx.recv() else {
                    // When: recv finds the handoff dropped, the start was refused; nothing to read.
                    return;
                };
                let read = || {
                    let clock = clock.clone();
                    let bound = GrantBoundSource { source: source.as_ref(), expires_at, clock };
                    // The offer must still be the announced one, and the grant still live after the
                    // version read, which can wait on the owner.
                    let offer_holds = || {
                        paste.is_none_or(|(location, version)| {
                            source.offer_version(location) == Some(version)
                        }) && bound.live()
                    };
                    if !offer_holds() {
                        // When: offer_holds is false, the offer already changed: nothing is read.
                        return None;
                    }
                    let replies = match ticket.kitty_read() {
                        Some(request) => build_read_reply(&bound, request),
                        None => read_replies(&bound, ticket.labels()),
                    };
                    // Sampled again after the read: data read across an offer change is dropped
                    // here, so it never leaves this thread.
                    offer_holds().then_some(replies)
                };
                // A panicking native read still returns its ticket, so the slot is not lost.
                let replies = std::panic::catch_unwind(std::panic::AssertUnwindSafe(read))
                    .unwrap_or(Some(thread_failure));
                // The raw text is gone, so only the frames stay reserved.
                lease.shrink_to(replies.as_ref().map_or(0, Vec::capacity));
                let completed_at = Instant::now();
                let _ = done_tx.send(ReaderDone { replies, ticket, lease, completed_at });
                if let Some(proxy) = proxy {
                    // When: proxy is Some, the loop is woken; a closed loop has nothing to wake.
                    let _ = proxy.send_event(UserEvent::ClipboardDone);
                }
            });
        #[cfg(test)]
        let spawned = self.refused_spawn_for_test(spawned);
        match spawned {
            Ok(_) => {
                // When: spawned is Ok, the detached thread waits for its ticket; its result
                // arrives through done_rx.
                let _ = handoff_tx.send((ticket, lease));
                self.clip_state.reads.in_flight =
                    Some(InFlight { binding, failure, deadline, abandoned: false, busy });
            }
            Err(error) => {
                // The reader never started and the ticket never left, so the failure is answered
                // through its custody; a read whose paste approval ended asks again.
                tracing::warn!("OSC 52 clipboard reader could not start: {error}");
                drop(lease);
                let custody = ReadCustody { ticket, lease: None };
                let reply = SensitiveReply::new(failure, Box::new(custody));
                self.deliver_read_reply(binding, reply, now, true, 0, busy);
            }
        }
    }

    /// Test-only: `spawned`, or a refused start when a test set `fail_spawn`, whose callback runs
    /// at that moment.
    #[cfg(test)]
    fn refused_spawn_for_test(
        &self,
        spawned: std::io::Result<std::thread::JoinHandle<()>>,
    ) -> std::io::Result<std::thread::JoinHandle<()>> {
        let Some(fail) = self.clip_state.reads.fail_spawn.as_ref() else {
            // When: fail_spawn is None, the start stands.
            return spawned;
        };
        fail();
        spawned.and_then(|_| Err(std::io::Error::other("reader start refused by a test")))
    }

    /// Retry each reply whose writer admission was contended last turn.
    fn retry_read_replies(&mut self, now: Instant) {
        for retry in std::mem::take(&mut self.clip_state.reads.retries) {
            self.deliver_read_reply(
                retry.binding,
                retry.reply,
                now,
                retry.check_age,
                retry.attempts,
                retry.busy,
            );
        }
    }

    /// Revalidate, then hand `reply` to the source PTY's writer; a contended admission is retried
    /// up to [`MAX_DELIVERY_RETRIES`] times, and any other refusal drops it, after which an OSC 5522
    /// read hears its one `busy` packet if that can be admitted. Only lengths are logged.
    fn deliver_read_reply(
        &mut self,
        binding: ReadBinding,
        reply: SensitiveReply,
        now: Instant,
        check_age: bool,
        attempts_before: u32,
        busy: Option<Vec<u8>>,
    ) {
        let pane_id = binding.pane_id;
        let bytes = reply.len();
        // Admission is checked against the clock, not the turn's start, so a grant that ends
        // during the turn never admits its reply; a retry is checked the same way.
        let admitted_at = now.max(self.clip_state.reads.grant_clock.now());
        if let Err(reason) = self.revalidate_ticket(&binding, admitted_at, check_age) {
            // When: revalidate_ticket refuses, the reply is dropped unsent with its custody.
            tracing::debug!(pane_id, bytes, ?reason, "OSC 52 read reply revoked");
            self.clip_state.reads.last_cancel = Some(reason);
            if reason == CancelReason::PasteRevoked {
                // Only the paste approval ended.
                self.answer_revoked(binding, reply, now, check_age, attempts_before, busy);
            }
            return;
        }
        let attempts = attempts_before + 1;
        let mut reply = reply;
        if let Some(paste) = binding.paste.as_deref() {
            // A paste-approved reply carries its grant's end to the PTY writer, which checks it
            // under its admission lock, so no delay after the check above can send it late.
            reply = reply.with_valid_until(paste.expires_at);
        }
        match self.send_read_reply(pane_id, reply) {
            Ok(()) => tracing::debug!(pane_id, bytes, "OSC 52 read answered"),
            Err(SensitiveReplyError::Contended(reply)) if attempts <= MAX_DELIVERY_RETRIES => {
                // The admission lock was busy and retries remain, so it waits a turn.
                let retry = Retry { binding, reply, attempts, check_age, busy };
                self.clip_state.reads.retries.push(retry);
            }
            Err(SensitiveReplyError::Expired(reply)) => {
                // The writer found the grant's end passed at its admission instant.
                tracing::debug!(pane_id, bytes, "OSC 52 read reply expired at admission");
                self.clip_state.reads.last_cancel = Some(CancelReason::PasteRevoked);
                self.answer_revoked(binding, reply, now, check_age, attempts_before, busy);
            }
            Err(error) => {
                tracing::debug!(pane_id, bytes, ?error, "OSC 52 read reply dropped");
                // The refused reply and its custody end before the status is tried.
                drop(error);
                if let Some(busy) = busy {
                    // An OSC 5522 read hears one `EBUSY`, or nothing if that cannot be admitted
                    // either.
                    let sent =
                        self.send_read_reply(pane_id, SensitiveReply::new(busy, Box::new(())));
                    tracing::debug!(
                        pane_id,
                        admitted = sent.is_ok(),
                        "OSC 5522 read answered busy"
                    );
                }
            }
        }
    }

    /// Handle `reply` after its paste approval ended: a reply holding a ticket drops its stale
    /// bytes and the ticket asks again; one holding none is a deadline status with no clipboard
    /// data, which needs no grant, so it is answered under the rest of `binding`.
    fn answer_revoked(
        &mut self,
        binding: ReadBinding,
        reply: SensitiveReply,
        now: Instant,
        check_age: bool,
        attempts_before: u32,
        busy: Option<Vec<u8>>,
    ) {
        if let Some(status) = self.requeue_revoked(reply) {
            let mut binding = binding;
            binding.paste = None;
            self.deliver_read_reply(binding, status, now, check_age, attempts_before, busy);
        }
    }

    /// Return the ticket a refused `reply` carries to `waiting` after its paste approval ended: the
    /// stale bytes are dropped first, then the approval is withdrawn, so the read asks again. A
    /// reply carrying no ticket is a status with no clipboard data (a deadline answer) and comes
    /// back whole to be answered; every reply that carries clipboard data holds its ticket.
    fn requeue_revoked(&mut self, reply: SensitiveReply) -> Option<SensitiveReply> {
        let (bytes, custody) = reply.into_parts();
        let custody = match custody.downcast::<ReadCustody>() {
            Ok(custody) => custody,
            Err(custody) => {
                // When: downcast finds no ReadCustody, the reply is a status alone: it comes back.
                return Some(SensitiveReply::new(bytes, custody));
            }
        };
        drop(bytes);
        let ReadCustody { mut ticket, lease } = *custody;
        drop(lease);
        ticket.withdraw_paste();
        self.clip_state.reads.waiting.push(ticket);
        None
    }

    /// Queue `reply` on pane `pane_id`'s sensitive-reply slot, or a test's port.
    pub(in crate::app) fn send_read_reply(
        &mut self,
        pane_id: u64,
        reply: SensitiveReply,
    ) -> Result<(), SensitiveReplyError> {
        #[cfg(test)]
        if let Some(port) = self.clip_state.reads.port.as_mut() {
            // When: a test installed a port, it stands in for the PTY's writer.
            return port(pane_id, reply);
        }
        match self.pane_by_id(pane_id).and_then(|pane| pane.pty.as_ref()) {
            Some(pty) => pty.try_send_reply(reply),
            None => Err(SensitiveReplyError::WriterStopped(reply)),
        }
    }

    /// When the pipeline next needs a turn: now while a retry waits, else the earliest of the
    /// running read's deadline, every ticket's lifetime end and every paste grant's expiry, so an
    /// idle grant is purged when it ends.
    pub(in crate::app) fn next_clipboard_read_wake(&self) -> Option<Instant> {
        let reads = &self.clip_state.reads;
        if !reads.retries.is_empty() {
            // When: retries is not empty, the next turn retries them.
            return Some(Instant::now());
        }
        let reader =
            reads.in_flight.as_ref().filter(|read| !read.abandoned).map(|read| read.deadline);
        let presented = reads.presented.as_ref().map(|presented| presented.ticket());
        let lifetimes = reads.waiting.iter().chain(presented).chain(&reads.queued);
        let expiry = lifetimes.map(|ticket| ticket.binding().created_at + TICKET_LIFETIME).min();
        let grant_expiry = reads.paste_grants.oldest_issue().map(|at| at + PASTE_GRANT_LIFETIME);
        let capture = reads.paste_capture.next_wake();
        reader.into_iter().chain(expiry).chain(grant_expiry).chain(capture).min()
    }

    /// Run the pipeline once per event-loop turn and wake the loop no later than it next needs.
    pub(in crate::app) fn service_clipboard_reads(&mut self, event_loop: &ActiveEventLoop) {
        if self.clip_state.reads.idle() {
            // When: idle, nothing waits on the pipeline.
            return;
        }
        self.pump_clipboard_reads();
        let Some(wake) = self.next_clipboard_read_wake() else {
            // When: next_clipboard_read_wake has no deadline, the loop's own schedule stands.
            return;
        };
        let sooner = match event_loop.control_flow() {
            ControlFlow::WaitUntil(at) => wake < at,
            ControlFlow::Wait => true,
            ControlFlow::Poll => false,
        };
        if sooner {
            event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
        }
    }

    /// Hand an approved write's job to the clipboard ordering domain, with its lease; the ticket
    /// and its slots end here. The job's binding records the approval, so the worker's commit
    /// point refuses it once its pane, PTY, command, READONLY state or either clipboard policy
    /// changed. The write's native result shows on the clip segment once it settles.
    pub(in crate::app) fn submit_approved_write(&mut self, mut ticket: ConsentTicket) {
        let pane_id = ticket.binding().pane_id;
        let Some(mut job) = ticket.take_write() else {
            // When: take_write is None, the ticket holds no write.
            return;
        };
        // The ticket's lifetime still bounds the write once the ticket itself is gone.
        let until = ticket.binding().created_at + TICKET_LIFETIME;
        drop(ticket);
        // Apply (or an Always grant) just revalidated the ticket, READONLY included.
        job.binding.approve(until);
        let bytes = job.text.len();
        match self.submit_program_clipboard(pane_id, job, true) {
            Some(outcome) => self.show_clip_outcome(pane_id, outcome, bytes, Instant::now()),
            None => {
                // The write went to the worker, so an empty summary waits for its result.
                let held = super::spawn_pane::ClipAttempts::default();
                self.note_or_defer_clip_attempts(pane_id, held, Instant::now());
            }
        }
    }

    /// Publish a reloaded `osc52_read` and `osc52` to the VT workers; any change to either starts a
    /// new policy generation, after the policies are stored and before the workers are woken, and
    /// revokes every ticket now.
    pub(in crate::app) fn publish_clipboard_policies(&mut self, new_cfg: &Config) {
        let old = &self.config.terminal;
        let changed =
            new_cfg.terminal.osc52 != old.osc52 || new_cfg.terminal.osc52_read != old.osc52_read;
        let staging = &self.clip_state.staging;
        staging.set_read_policy(new_cfg.terminal.osc52_read);
        staging.set_write_policy(new_cfg.terminal.osc52);
        if changed {
            staging.bump_policy_generation();
        }
        // Every VT worker rechecks an open write now, even while its child is silent; the new
        // generation is already published, so no worker spends this wake under the old one.
        staging.wake_policy_workers();
        if changed {
            // Any policy transition revokes every session grant and every paste grant.
            self.clip_state.reads.grants.clear();
            self.clip_state.reads.paste_grants.clear();
            self.pump_clipboard_reads();
        }
    }
}

#[cfg(test)]
#[path = "osc52_read_pipeline_tests.rs"]
mod osc52_read_pipeline_tests;
