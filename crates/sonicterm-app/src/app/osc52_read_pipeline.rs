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
use super::osc52_read_source::{read_replies, ClipboardTextSource, NativeSource};
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
}

/// A reply whose writer admission was contended, retried on a later turn.
struct Retry {
    binding: ReadBinding,
    reply: SensitiveReply,
    attempts: u32,
    check_age: bool,
}

/// What a reader thread hands back once its native calls return.
struct ReaderDone {
    replies: Vec<u8>,
    ticket: ConsentTicket,
    lease: ClipLease,
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
    /// Why the last ticket or reply was revoked, for diagnostics.
    pub(in crate::app) last_cancel: Option<CancelReason>,
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
            last_cancel: None,
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
            .finish()
    }
}

impl ClipboardReads {
    /// Whether nothing waits, is presented, queues, reads or retries.
    fn idle(&self) -> bool {
        self.waiting.is_empty()
            && self.presented.is_none()
            && self.queued.is_empty()
            && self.in_flight.is_none()
            && self.retries.is_empty()
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
        if ticket.readable() {
            self.clip_state.reads.queued.push_back(ticket);
        } else {
            // When: the ticket is not readable, it waits for an answer or its expiry.
            self.clip_state.reads.waiting.push(ticket);
        }
        self.pump_clipboard_reads();
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
        let asker_present = self.pane_by_id(pane_id).is_some_and(|pane| {
            pane.pty.as_ref().map_or(0, |pty| pty.incarnation()) == ticket.binding().incarnation
                && pane.read_identity.command_epoch() == ticket.binding().command_epoch
        });
        let kitty = ticket.take_write().and_then(|job| job.kitty.clone());
        drop(ticket);
        if asker_present {
            // The waiting program hears the refusal; another PTY or command would read the
            // status as input, so it is never sent there.
            self.send_kitty_status(pane_id, kitty, super::event_loop::ClipWriteOutcome::Denied);
        }
    }

    /// Run the read pipeline now.
    pub(in crate::app) fn pump_clipboard_reads(&mut self) {
        self.pump_clipboard_reads_at(Instant::now());
    }

    /// Run the read pipeline at `now`: retry contended replies first, so a reply contended this
    /// turn waits for the next, then settle the reader, sweep tickets and the consent page,
    /// show the next page and start the next read.
    pub(in crate::app) fn pump_clipboard_reads_at(&mut self, now: Instant) {
        self.retry_read_replies(now);
        self.settle_reader(now);
        self.sweep_read_tickets(now);
        self.sweep_consent_page(now);
        self.present_consent_page();
        self.start_reader(now);
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
            let ReaderDone { replies, ticket, lease } = done;
            let reply = SensitiveReply::new(replies, Box::new((ticket, lease)));
            self.deliver_read_reply(in_flight.binding, reply, now, true, 0);
        }
        let mut overdue = None;
        if let Some(in_flight) = self.clip_state.reads.in_flight.as_mut() {
            if !in_flight.abandoned && now >= in_flight.deadline {
                // Past its deadline the read is answered empty; the thread keeps its slot.
                in_flight.abandoned = true;
                overdue = Some((in_flight.binding.clone(), in_flight.failure.clone()));
            }
        }
        if let Some((binding, replies)) = overdue {
            tracing::warn!(pane_id = binding.pane_id, "OSC 52 read passed its deadline");
            self.deliver_read_reply(
                binding,
                SensitiveReply::new(replies, Box::new(())),
                now,
                false,
                0,
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
        for ticket in std::mem::take(&mut self.clip_state.reads.queued) {
            if let Err(reason) = self.revalidate_ticket(ticket.binding(), now, true) {
                // When: revalidate_ticket refuses a queued ticket before its read starts, it ends.
                self.cancel_ticket(ticket, reason);
                continue;
            }
            self.clip_state.reads.queued.push_back(ticket);
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
            let kitty = ticket.take_write().and_then(|job| job.kitty.clone());
            drop(ticket);
            self.send_kitty_status(pane_id, kitty, super::event_loop::ClipWriteOutcome::Denied);
            tracing::debug!(pane_id, "OSC 52 asked write refused");
            self.show_clip_outcome(pane_id, super::event_loop::ClipWriteOutcome::Denied, 0, now);
            return;
        }
        let binding = ticket.binding().clone();
        let replies = ticket.failure_reply(status);
        self.deliver_read_reply(
            binding,
            SensitiveReply::new(replies, Box::new(ticket)),
            now,
            false,
            0,
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
        let spawned =
            std::thread::Builder::new().name("sonicterm-clipboard-read".into()).spawn(move || {
                let mut lease = lease;
                let read = || match ticket.kitty_read() {
                    Some(request) => build_read_reply(source.as_ref(), request),
                    None => read_replies(source.as_ref(), ticket.labels()),
                };
                // A panicking native read still returns its ticket, so the slot is not lost.
                let replies = std::panic::catch_unwind(std::panic::AssertUnwindSafe(read))
                    .unwrap_or(thread_failure);
                // The raw text is gone, so only the frames stay reserved.
                lease.shrink_to(replies.capacity());
                let _ = done_tx.send(ReaderDone { replies, ticket, lease });
                if let Some(proxy) = proxy {
                    // When: proxy is Some, the loop is woken; a closed loop has nothing to wake.
                    let _ = proxy.send_event(UserEvent::ClipboardDone);
                }
            });
        match spawned {
            Ok(_) => {
                // The thread is detached; its result arrives through done_rx.
                self.clip_state.reads.in_flight =
                    Some(InFlight { binding, failure, deadline, abandoned: false });
            }
            Err(error) => {
                // The thread did not start; the dropped closure already freed the ticket and lease.
                tracing::warn!("OSC 52 clipboard reader could not start: {error}");
                let reply = SensitiveReply::new(failure, Box::new(()));
                self.deliver_read_reply(binding, reply, now, true, 0);
            }
        }
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
            );
        }
    }

    /// Revalidate, then hand `reply` to the source PTY's writer; a contended admission is retried
    /// up to [`MAX_DELIVERY_RETRIES`] times, and any other refusal drops it. Only lengths are logged.
    fn deliver_read_reply(
        &mut self,
        binding: ReadBinding,
        reply: SensitiveReply,
        now: Instant,
        check_age: bool,
        attempts_before: u32,
    ) {
        let pane_id = binding.pane_id;
        let bytes = reply.len();
        if let Err(reason) = self.revalidate_ticket(&binding, now, check_age) {
            // When: revalidate_ticket refuses, the reply is dropped unsent with its custody.
            tracing::debug!(pane_id, bytes, ?reason, "OSC 52 read reply revoked");
            self.clip_state.reads.last_cancel = Some(reason);
            return;
        }
        let attempts = attempts_before + 1;
        match self.send_read_reply(pane_id, reply) {
            Ok(()) => tracing::debug!(pane_id, bytes, "OSC 52 read answered"),
            Err(SensitiveReplyError::Contended(reply)) if attempts <= MAX_DELIVERY_RETRIES => {
                // The admission lock was busy and retries remain, so it waits a turn.
                self.clip_state.reads.retries.push(Retry { binding, reply, attempts, check_age });
            }
            Err(error) => tracing::debug!(pane_id, bytes, ?error, "OSC 52 read reply dropped"),
        }
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
    /// running read's deadline and every ticket's lifetime end.
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
        reader.into_iter().chain(expiry).min()
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
    /// and its slots end here. The write's native result shows on the clip segment once it settles.
    pub(in crate::app) fn submit_approved_write(&mut self, mut ticket: ConsentTicket) {
        let pane_id = ticket.binding().pane_id;
        let Some(job) = ticket.take_write() else {
            // When: take_write is None, the ticket holds no write.
            return;
        };
        drop(ticket);
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
    /// new policy generation, after the policies are stored, and revokes every ticket now.
    pub(in crate::app) fn publish_clipboard_policies(&mut self, new_cfg: &Config) {
        let staging = &self.clip_state.staging;
        staging.set_read_policy(new_cfg.terminal.osc52_read);
        staging.set_write_policy(new_cfg.terminal.osc52);
        // Every VT worker rechecks an open OSC 5522 write now, even while its child is silent.
        staging.wake_policy_workers();
        let old = &self.config.terminal;
        if new_cfg.terminal.osc52 != old.osc52 || new_cfg.terminal.osc52_read != old.osc52_read {
            staging.bump_policy_generation();
            self.pump_clipboard_reads();
        }
    }
}

#[cfg(test)]
#[path = "osc52_read_pipeline_tests.rs"]
mod osc52_read_pipeline_tests;
