//! The clipboard ordering domain: one bounded worker thread owns native program clipboard writes
//! on Linux and Windows, where the native clipboard is not bound to the event-loop thread.
//!
//! The event loop posts move-only jobs into one FIFO queue: program writes (OSC 52, OSC 1337
//! `Copy=:`), each carrying its program slot and lease, and user copies, at most
//! [`MAX_USER_CLIPBOARD_JOBS`] of them in flight. The worker runs them strictly in posting order,
//! so a user copy is never reordered behind, or overwritten by, an older program write. The event
//! loop writes a user copy itself only when nothing is in flight and the text is at most
//! [`INLINE_WRITE_LIMIT_BYTES`]. macOS has no worker, because off-thread `NSPasteboard` use is not
//! proven: its program writes over that limit are refused.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};

use super::event_loop::ClipWriteOutcome;
#[cfg(not(target_os = "linux"))]
use super::kitty_clipboard_offer::Published;
use super::kitty_clipboard_offer::{CommitGate, NativeOffer};
use super::kitty_clipboard_tx::KittyReply;
use super::osc52_read_ticket::WriteBinding;
use super::osc52_stream::{
    native_clipboard_bytes, reassert_eligible, ClipLease, ClipTargets, ClipboardJob,
    RetainedShares, MACOS_PROGRAM_WRITE_LIMIT_BYTES, MAX_PROGRAM_CLIPBOARD_JOBS,
    REASSERT_MAX_UTF16_UNITS,
};
use super::spawn_pane::ClipAttempts;
use super::UserEvent;

/// User copies that may be in flight on the worker at once.
pub(super) const MAX_USER_CLIPBOARD_JOBS: usize = 4;

/// The largest native write, in UTF-8 bytes, the event-loop thread makes itself.
pub(super) const INLINE_WRITE_LIMIT_BYTES: usize = MACOS_PROGRAM_WRITE_LIMIT_BYTES;

/// Delay allowing a failing Windows clipboard helper to release its open handle.
const OSC52_CLIPBOARD_REASSERT_DELAY: Duration = Duration::from_millis(150);

/// The largest `CF_UNICODETEXT` block a pre-write snapshot reads: 512 KiB, terminator included.
pub(super) const SNAPSHOT_MAX_BLOCK_BYTES: usize = 2 * REASSERT_MAX_UTF16_UNITS;

/// What one selection did with a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum TargetOutcome {
    /// The write did not name this selection.
    Untouched,
    /// The selection took the text.
    Taken,
    /// The selection surely kept its previous value.
    Refused,
    /// The native call failed after it may have stored the text, so either value may be held.
    Unknown,
}

impl TargetOutcome {
    /// Whether the selection holds the write, or was not asked to.
    pub(in crate::app) fn took(self) -> bool {
        matches!(self, Self::Taken | Self::Untouched)
    }
}

/// One text write a sink makes.
pub(in crate::app) struct WriteRequest<'request> {
    /// The text; an empty text clears the selections.
    pub(in crate::app) text: &'request str,
    /// The selections it goes to.
    pub(in crate::app) targets: ClipTargets,
    /// Checked at the commit point, immediately before the native call changes anything.
    pub(in crate::app) gate: CommitGate<'request>,
    /// Read the value the write replaces, when its native block is at most this many bytes, in
    /// the same native acquisition and before `gate` (Windows).
    // Only the Windows sink reads it; other sinks have no reassertion to read for.
    #[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
    pub(in crate::app) snapshot_max_bytes: Option<usize>,
}

/// What one native write did, including independent selection confirmations.
pub(in crate::app) struct WriteResult {
    /// False when the gate refused at the commit point, so nothing changed.
    pub(in crate::app) committed: bool,
    /// The standard clipboard's outcome.
    pub(in crate::app) clipboard: TargetOutcome,
    /// PRIMARY's outcome.
    pub(in crate::app) primary: TargetOutcome,
    /// The value the write replaced, when it was asked for and readable.
    pub(in crate::app) replaced: Option<Snapshot>,
}

impl WriteResult {
    /// The gate refused at the commit point: nothing changed.
    pub(in crate::app) fn stale() -> Self {
        let untouched = TargetOutcome::Untouched;
        Self { committed: false, clipboard: untouched, primary: untouched, replaced: None }
    }

    /// Every selection `targets` names had `outcome`.
    pub(in crate::app) fn each(targets: ClipTargets, outcome: TargetOutcome) -> Self {
        let pick = |named: bool| if named { outcome } else { TargetOutcome::Untouched };
        let (clipboard, primary) = (pick(targets.clipboard), pick(targets.primary));
        Self { committed: true, clipboard, primary, replaced: None }
    }

    /// Whether the write committed and every selection it named took the text.
    pub(in crate::app) fn written(&self) -> bool {
        self.committed && self.clipboard.took() && self.primary.took()
    }
}

/// The native clipboard calls a program write or user copy makes; tests substitute a fake. Every
/// call that changes the clipboard checks its gate at the commit point, inside the native
/// acquisition, after any read or comparison and immediately before the change.
pub(in crate::app) trait ClipboardSink {
    /// Write `request.text` to its selections behind `request.gate`.
    fn write(&mut self, request: WriteRequest<'_>) -> WriteResult;

    /// Write `text` to the standard clipboard again only while it holds exactly `previous`,
    /// compared and replaced in one native acquisition with `gate` checked after the comparison;
    /// true when it wrote. Only Windows reasserts.
    fn reassert(&mut self, _text: &str, _previous: &Snapshot, _gate: CommitGate<'_>) -> bool {
        false
    }

    /// Publish the whole offer to its targets only while the binding permits it; the owner takes the lease.
    fn publish_offer(
        &mut self,
        _offer: NativeOffer,
        _lease: ClipLease,
        targets: ClipTargets,
        _binding: WriteBinding,
    ) -> WriteResult {
        WriteResult::each(targets, TargetOutcome::Refused)
    }

    /// Whether the native clipboard keeps its own copy of each written selection until that
    /// selection is replaced, as arboard does on Linux.
    fn retains_copies(&self) -> bool {
        cfg!(target_os = "linux")
    }
}

/// Clipboard text read before a write: its native block size and its UTF-16 units.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(in crate::app) struct Snapshot {
    /// The native block's size in bytes, which a later comparison must match exactly.
    pub(in crate::app) block_bytes: usize,
    /// The text up to its terminator.
    pub(in crate::app) units: Vec<u16>,
}

/// One delayed reassertion the worker holds until it is due or a newer job cancels it.
struct PendingReassert {
    /// The program text to write again.
    text: String,
    /// The clipboard value the write replaced.
    previous: Snapshot,
    /// Covers `text`, its native copy and `previous` until the reassertion settles.
    _lease: ClipLease,
    /// What the write was admitted under; the reassertion is refused once it no longer holds.
    binding: WriteBinding,
    /// When the reassertion runs.
    due: Instant,
}

/// What one program write left behind.
pub(super) struct Committed {
    /// Whether the native clipboard took it.
    pub(super) outcome: ClipWriteOutcome,
    /// The text's length, for the status line.
    pub(super) bytes: usize,
    /// Each selection's share of the lease covering arboard's retained copy (Linux), held until
    /// that selection is known to be replaced.
    pub(super) retained: RetainedShares,
    /// Where an OSC 5522 write's status goes; `None` for OSC 52 and for a write dropped because
    /// its binding no longer held, which answers nothing.
    pub(super) kitty: Option<KittyReply>,
    /// What the write was admitted under, which decides whether its status may still be sent.
    pub(super) binding: WriteBinding,
}

impl Committed {
    /// A write that settled as `outcome` without anything retained; dropping `job` releases its
    /// lease and program slot.
    fn settled(job: ClipboardJob, outcome: ClipWriteOutcome) -> Self {
        let bytes = job.offer.as_ref().map_or(job.text.len(), NativeOffer::byte_len);
        let ClipboardJob { kitty, binding, .. } = job;
        let retained = RetainedShares::default();
        Self { outcome, bytes, retained, kitty, binding }
    }

    /// A write its binding no longer authorizes: dropped with no clipboard change and no status.
    fn stale(job: ClipboardJob) -> Self {
        Self::settled(job, ClipWriteOutcome::Denied).into_stale()
    }

    /// This write, refused at its commit point: denied, and its program is not answered.
    fn into_stale(mut self) -> Self {
        self.outcome = ClipWriteOutcome::Denied;
        self.kitty = None;
        self
    }
}

/// Commit one program write through `sink`. Its binding is the gate the sink checks at the commit
/// point, inside the native acquisition and after any read of the value it replaces, so a write
/// that lost its authority (a policy change; for an approved write, its pane, PTY, command or
/// READONLY state) is dropped with no clipboard change and no status. With `reassert_delay`, a
/// write within the reassertion bound reads the value it replaces in that same acquisition and,
/// once written, arms one reassertion that keeps the lease; otherwise the lease is released when
/// the call returns.
fn commit_program(
    sink: &mut dyn ClipboardSink,
    job: ClipboardJob,
    reassert_delay: Option<Duration>,
) -> (Committed, Option<PendingReassert>) {
    // The program slot stays in `job` until this returns, after the native call.
    let ClipboardJob { text, targets, mut lease, kitty, offer, binding, .. } = job;
    let bytes = offer.as_ref().map_or(text.len(), NativeOffer::byte_len);
    let (outcome, retained) = (ClipWriteOutcome::Denied, RetainedShares::default());
    let mut committed = Committed { outcome, bytes, retained, kitty, binding: binding.clone() };
    let gate = || binding.may_commit();
    if let Some(offer) = offer {
        // When: the job carries an offer, every representation is published in one native call
        // behind the gate. On X11 the owner takes the lease for the lifetime of the offer.
        let mut marker = lease.split_off(0);
        let result = sink.publish_offer(offer, lease, targets, binding);
        if !result.committed {
            // When: committed is false, the gate refused before either selection was changed.
            return (committed.into_stale(), None);
        }
        if sink.retains_copies() {
            // The owner holds its own lease. Retire only confirmed arboard shares, even on EIO.
            committed.retained =
                RetainedShares::split(&mut marker, 0, result.clipboard, result.primary);
        }
        committed.outcome =
            if result.written() { ClipWriteOutcome::Written } else { ClipWriteOutcome::Failed };
        return (committed, None);
    }
    let reassertable = reassert_delay.is_some()
        && targets.clipboard
        && !text.is_empty()
        && reassert_eligible(&text);
    let base = lease.bytes();
    // The largest replaced value is reserved before it is read; no reassertion when it cannot be.
    let snapshot_max_bytes = (reassertable && lease.grow_to(base + SNAPSHOT_MAX_BLOCK_BYTES))
        .then_some(SNAPSHOT_MAX_BLOCK_BYTES);
    let result = sink.write(WriteRequest { text: &text, targets, gate: &gate, snapshot_max_bytes });
    let kept = result.replaced.as_ref().map_or(0, |snapshot| snapshot.units.capacity() * 2);
    lease.shrink_to(base + kept);
    if !result.committed {
        // When: result.committed is false, the gate refused at the commit point; nothing changed
        // and nothing is answered.
        return (committed.into_stale(), None);
    }
    if sink.retains_copies() {
        // Each selection that took the text, or may have, keeps one native copy's share until it
        // is replaced, even when the other selection refused the write.
        let (clipboard, primary) = (result.clipboard, result.primary);
        let copy_bytes = native_clipboard_bytes(&text);
        committed.retained = RetainedShares::split(&mut lease, copy_bytes, clipboard, primary);
    }
    if !result.written() {
        // When: result.written() is false, a selection did not take the write; nothing is
        // reasserted.
        committed.outcome = ClipWriteOutcome::Failed;
        return (committed, None);
    }
    committed.outcome = ClipWriteOutcome::Written;
    if let (Some(delay), Some(previous)) = (reassert_delay, result.replaced) {
        // When: delay is set and previous was read, one reassertion is armed and holds the lease.
        let due = Instant::now() + delay;
        return (committed, Some(PendingReassert { text, previous, _lease: lease, binding, due }));
    }
    (committed, None)
}

/// Run one due reassertion: refused once its binding no longer holds; otherwise the sink writes
/// the program's text again only while the clipboard holds exactly the value the write replaced,
/// compared and replaced in one native acquisition with the binding checked again after the
/// comparison, so a newer value or a revocation always wins. True when it wrote.
fn reassert(sink: &mut dyn ClipboardSink, pending: PendingReassert) -> bool {
    if !pending.binding.may_commit() {
        // When: the binding no longer holds, the clipboard is not even opened.
        return false;
    }
    let gate = || pending.binding.may_commit();
    sink.reassert(&pending.text, &pending.previous, &gate)
}

/// One job on the worker's queue.
enum Job {
    /// A program write from `pane_id`; its binding rides in the job to the commit point.
    Program { pane_id: u64, job: ClipboardJob },
    /// A user copy to the standard clipboard.
    User { text: String },
}

/// One result the worker reports, in posting order.
enum Done {
    /// A program write committed; with `reassert_armed` it stays in flight until
    /// [`Done::ReassertSettled`].
    Program { pane_id: u64, committed: Committed, reassert_armed: bool },
    /// An armed reassertion ran, or a newer job cancelled it.
    ReassertSettled,
    /// A user copy, and whether the native clipboard took it.
    User { written: bool },
}

/// The worker: run jobs strictly in posting order and hold at most one armed reassertion, which
/// any newer job cancels; report each result through `done`, then `wake` the event loop.
fn run_worker(
    mut sink: Box<dyn ClipboardSink + Send>,
    jobs: Receiver<Job>,
    done: Sender<Done>,
    wake: Box<dyn Fn() + Send>,
    reassert_delay: Option<Duration>,
) {
    let report = |result: Done| {
        if done.send(result).is_ok() {
            // The App still receives results, so it is woken to apply this one.
            wake();
        }
    };
    let mut armed: Option<PendingReassert> = None;
    loop {
        let next = match armed.as_ref() {
            Some(pending) => jobs.recv_deadline(pending.due),
            None => jobs.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let job = match next {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) => {
                // When: recv_deadline hit Timeout, no newer job came before the reassertion was due.
                if let Some(pending) = armed.take() {
                    // When: armed held the due reassertion, it runs and then settles.
                    let _ = reassert(sink.as_mut(), pending);
                    report(Done::ReassertSettled);
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // When: Disconnected, the App dropped its queue: the worker and its reassertion end.
                return;
            }
        };
        if armed.take().is_some() {
            // A newer job arrived first, so the armed reassertion settles without running.
            report(Done::ReassertSettled);
        }
        match job {
            Job::Program { pane_id, job } => {
                let (committed, pending) = commit_program(sink.as_mut(), job, reassert_delay);
                let reassert_armed = pending.is_some();
                armed = pending;
                report(Done::Program { pane_id, committed, reassert_armed });
            }
            Job::User { text } => {
                let targets = ClipTargets { clipboard: true, primary: false };
                // A user copy answers to no program, so nothing can revoke it.
                let gate = || true;
                let request =
                    WriteRequest { text: &text, targets, gate: &gate, snapshot_max_bytes: None };
                let written = sink.write(request).written();
                report(Done::User { written });
            }
        }
    }
}

/// What the event loop runs once a user copy's native result is known.
pub(in crate::app) type FollowUp = Box<dyn FnOnce(&mut super::App, bool)>;

/// Where a posted write's OSC 5522 status goes if the worker stops before reporting it.
struct StatusRoute {
    /// The pane that asked.
    pane_id: u64,
    /// The status address; `None` for OSC 52.
    reply: Option<KittyReply>,
    /// What the write was admitted under, which decides whether the status may still be sent.
    binding: WriteBinding,
}

/// The worker's queue and result channel, as the event loop holds them.
struct WorkerHandle {
    jobs: Sender<Job>,
    done: Receiver<Done>,
}

/// The event loop's side of the ordering domain.
#[derive(Default)]
pub(in crate::app) struct ClipboardDomain {
    /// The worker, started at the first write that needs it.
    worker: Option<WorkerHandle>,
    /// Posted jobs whose results the event loop has not applied, and an armed reassertion.
    in_flight: usize,
    /// The follow-ups of the user copies in flight, oldest first.
    follow_ups: VecDeque<FollowUp>,
    /// Per pane, its program writes in flight.
    program_panes: HashMap<u64, usize>,
    /// Each posted program write's status route, in posting order; the worker reports writes in
    /// that order, so a lost worker leaves exactly the unreported ones here.
    program_replies: VecDeque<StatusRoute>,
}

impl std::fmt::Debug for ClipboardDomain {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClipboardDomain")
            .field("worker", &self.worker.is_some())
            .field("in_flight", &self.in_flight)
            .field("user_copies", &self.follow_ups.len())
            .finish()
    }
}

/// One applied result.
enum Settled {
    /// A program write from `pane_id` and what it left behind.
    Program { pane_id: u64, committed: Committed },
    /// A user copy's follow-up and whether the clipboard took it.
    User { follow_up: FollowUp, written: bool },
    /// The worker stopped: the panes whose writes it held, the OSC 5522 statuses those writes
    /// still owe, and the copies it never reported.
    Lost { panes: Vec<u64>, replies: Vec<StatusRoute>, follow_ups: Vec<FollowUp> },
}

impl ClipboardDomain {
    /// Whether a worker runs.
    pub(super) fn has_worker(&self) -> bool {
        self.worker.is_some()
    }

    /// Posted jobs whose results are not applied yet, plus an armed reassertion.
    pub(super) fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// Whether a program write from `pane_id` is still in flight.
    pub(super) fn program_pending(&self, pane_id: u64) -> bool {
        self.program_panes.contains_key(&pane_id)
    }

    /// Start the worker over `sink`; false, with nothing started, when no thread can be spawned.
    pub(super) fn start_worker(
        &mut self,
        sink: Box<dyn ClipboardSink + Send>,
        reassert_delay: Option<Duration>,
        wake: Box<dyn Fn() + Send>,
    ) -> bool {
        // Every queued job holds a program slot or a user-copy place, so the queue never fills.
        let (jobs_tx, jobs_rx) =
            crossbeam_channel::bounded(MAX_PROGRAM_CLIPBOARD_JOBS + MAX_USER_CLIPBOARD_JOBS);
        // Results wait only for posted jobs, which the same bounds limit.
        let (done_tx, done_rx) = crossbeam_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("sonicterm-clipboard".into())
            .spawn(move || run_worker(sink, jobs_rx, done_tx, wake, reassert_delay));
        match spawned {
            Ok(_) => {
                // The thread is detached; dropping the queue ends it.
                self.worker = Some(WorkerHandle { jobs: jobs_tx, done: done_rx });
                true
            }
            Err(error) => {
                tracing::warn!("clipboard worker could not start: {error}");
                false
            }
        }
    }

    /// Post `job`; it comes back, boxed since this path is rare, when no worker runs or the worker
    /// stopped.
    fn post(&mut self, job: Job) -> Result<(), Box<Job>> {
        let Some(worker) = self.worker.as_ref() else {
            // When: no worker runs, there is nothing to post to.
            return Err(Box::new(job));
        };
        if let Err(refused) = worker.jobs.try_send(job) {
            // When: the worker stopped, the job is refused; its result loss is reported on drain.
            return Err(Box::new(refused.into_inner()));
        }
        self.in_flight += 1;
        Ok(())
    }

    /// Post a program write from `pane_id`; it comes back when it could not be posted.
    fn post_program(&mut self, pane_id: u64, job: ClipboardJob) -> Result<(), Box<ClipboardJob>> {
        let route = StatusRoute { pane_id, reply: job.kitty.clone(), binding: job.binding.clone() };
        let Err(refused) = self.post(Job::Program { pane_id, job }) else {
            // When: post returned Ok, the write is in flight and owes its status on drain.
            *self.program_panes.entry(pane_id).or_default() += 1;
            self.program_replies.push_back(route);
            return Ok(());
        };
        match *refused {
            Job::Program { job, .. } => Err(Box::new(job)),
            Job::User { .. } => unreachable!("post returns the job it was given"),
        }
    }

    /// Whether [`MAX_USER_CLIPBOARD_JOBS`] user copies are already in flight.
    fn user_copies_full(&self) -> bool {
        self.follow_ups.len() >= MAX_USER_CLIPBOARD_JOBS
    }

    /// Post a user copy; its follow-up comes back when it could not be posted.
    fn post_user(&mut self, text: String, follow_up: FollowUp) -> Result<(), FollowUp> {
        if self.post(Job::User { text }).is_err() {
            // When: post refused the copy, the caller reports the refusal.
            return Err(follow_up);
        }
        self.follow_ups.push_back(follow_up);
        Ok(())
    }

    /// The next result to apply, in posting order; `None` when none is waiting.
    fn next_settled(&mut self) -> Option<Settled> {
        loop {
            let received = self.worker.as_ref()?.done.try_recv();
            let done = match received {
                Ok(done) => done,
                Err(TryRecvError::Empty) => {
                    // When: try_recv found the queue Empty, every reported result is applied.
                    return None;
                }
                Err(TryRecvError::Disconnected) => {
                    // When: Disconnected, the worker stopped and what it held is lost.
                    return Some(self.lose_worker());
                }
            };
            if let Some(settled) = self.settle(done) {
                // When: settle returned a result, it is applied now; a settled reassertion only
                // frees its place in the domain.
                return Some(settled);
            }
        }
    }

    /// Account for one reported result and return what the event loop applies for it.
    fn settle(&mut self, done: Done) -> Option<Settled> {
        match done {
            Done::Program { pane_id, committed, reassert_armed } => {
                // An armed reassertion keeps the write in flight until it settles.
                self.in_flight = self.in_flight.saturating_sub(usize::from(!reassert_armed));
                if let Some(count) = self.program_panes.get_mut(&pane_id) {
                    *count = count.saturating_sub(1);
                }
                self.program_panes.retain(|_, count| *count > 0);
                // The reported write is the oldest posted one; its status rides `committed`.
                self.program_replies.pop_front();
                Some(Settled::Program { pane_id, committed })
            }
            Done::ReassertSettled => {
                self.in_flight = self.in_flight.saturating_sub(1);
                None
            }
            Done::User { written } => {
                self.in_flight = self.in_flight.saturating_sub(1);
                self.follow_ups.pop_front().map(|follow_up| Settled::User { follow_up, written })
            }
        }
    }

    /// The worker stopped (it panicked): drop it so the next write starts a fresh one, and hand
    /// back everything it held as lost.
    fn lose_worker(&mut self) -> Settled {
        tracing::warn!(in_flight = self.in_flight, "clipboard worker stopped; its writes failed");
        self.worker = None;
        self.in_flight = 0;
        Settled::Lost {
            panes: self.program_panes.drain().map(|(pane_id, _)| pane_id).collect(),
            replies: self.program_replies.drain(..).filter(|route| route.reply.is_some()).collect(),
            follow_ups: self.follow_ups.drain(..).collect(),
        }
    }
}

impl super::App {
    /// Copy the user's `text` to the clipboard, ordered with program writes, then run
    /// `follow_up` with whether the native clipboard took it: at once when it is written here or
    /// refused, else when the worker reports it. Returns false only when it was refused.
    pub(in crate::app) fn copy_text_then(
        &mut self,
        text: String,
        follow_up: impl FnOnce(&mut Self, bool) + 'static,
    ) -> bool {
        if text.is_empty() {
            // When: text is empty, writing it would clear the user's clipboard, so it is refused.
            follow_up(self, false);
            return false;
        }
        self.drain_clipboard_completions();
        self.ensure_clipboard_worker();
        let domain = &self.clip_state.domain;
        if domain.has_worker() && (domain.in_flight() > 0 || text.len() > INLINE_WRITE_LIMIT_BYTES)
        {
            // When: a job is in flight or the text is too large for this thread, it queues.
            if self.clip_state.domain.user_copies_full() {
                // When: user_copies_full, the copy is refused before anything is posted.
                follow_up(self, false);
                return false;
            }
            return match self.clip_state.domain.post_user(text, Box::new(follow_up)) {
                Ok(()) => true,
                Err(follow_up) => {
                    follow_up(self, false);
                    false
                }
            };
        }
        if !self.may_write_inline(text.len(), true) {
            // When: no worker runs and the text is too large for this thread, it is refused.
            follow_up(self, false);
            return false;
        }
        let written =
            self.write_clipboard_inline(&text, ClipTargets { clipboard: true, primary: false });
        if written {
            // A user copy replaced the clipboard, so arboard's retained copy of an earlier
            // program write is gone with it.
            self.clip_state.retained.release_clipboard();
        }
        follow_up(self, written);
        written
    }

    /// Route one program write: dropped unanswered once its binding no longer holds; refused here
    /// under `osc52 = "deny"`, and under `"ask"` unless it was `consented` on its page; posted to
    /// the worker in order when one runs, else written here when it is small enough, else refused
    /// too large. Returns the outcome when it settled here; a posted write reports through
    /// [`Self::drain_clipboard_completions`].
    pub(in crate::app) fn submit_program_clipboard(
        &mut self,
        pane_id: u64,
        job: ClipboardJob,
        consented: bool,
    ) -> Option<ClipWriteOutcome> {
        // An OSC 5522 write settled here is answered now; one on the worker answers on drain.
        let mut committed = self.route_program_clipboard(pane_id, job, consented)?;
        let reply = committed.kitty.take();
        self.send_kitty_status(pane_id, reply, &committed.binding, committed.outcome);
        Some(self.settle_program(committed))
    }

    /// The routing [`Self::submit_program_clipboard`] describes, without the OSC 5522 status.
    fn route_program_clipboard(
        &mut self,
        pane_id: u64,
        job: ClipboardJob,
        consented: bool,
    ) -> Option<Committed> {
        use sonicterm_cfg::config::Osc52Policy;
        if !job.binding.may_commit() {
            // When: the binding no longer holds (a policy change; for an approved write its pane,
            // PTY, command or READONLY state), the write is dropped with no status.
            tracing::debug!(pane_id, "OSC 52 clipboard write dropped: its binding changed");
            return Some(Committed::stale(job));
        }
        let policy = self.config.terminal.osc52;
        if policy == Osc52Policy::Deny || (policy == Osc52Policy::Ask && !consented) {
            // When: osc52 is Deny, or Ask and the write was not consented, the clipboard is never
            // touched; dropping the job releases its lease and slot.
            tracing::debug!(
                bytes = job.text.len(),
                ?policy,
                "OSC 52 clipboard write denied by [terminal] osc52"
            );
            return Some(Committed::settled(job, ClipWriteOutcome::Denied));
        }
        self.drain_clipboard_completions();
        self.ensure_clipboard_worker();
        if self.clip_state.domain.has_worker() {
            // When: has_worker, every program write goes to it, in order with user copies; a
            // stopped worker refuses the post and the write fails.
            return match self.clip_state.domain.post_program(pane_id, job) {
                Ok(()) => None,
                Err(job) => Some(Committed::settled(*job, ClipWriteOutcome::Failed)),
            };
        }
        if job.offer.is_some() {
            // When: no worker runs and the job carries an offer, it is published here in one
            // native operation (macOS, a test buffer); no posted write can be overtaken.
            return Some(self.publish_offer_inline(job));
        }
        if !self.may_write_inline(job.text.len(), false) {
            // When: may_write_inline refuses its size with no worker, the write is refused.
            return Some(Committed::settled(job, ClipWriteOutcome::TooLarge));
        }
        let (committed, _) = commit_program(&mut InlineSink(self), job, None);
        Some(committed)
    }

    /// Publish an offer job on this thread: refused too large past the inline limit, else every
    /// representation in one native operation behind its binding; its lease and slot end when the
    /// call returns.
    fn publish_offer_inline(&mut self, job: ClipboardJob) -> Committed {
        let offer = job.offer.as_ref().expect("an offer job");
        if !self.may_write_inline(offer.byte_len(), false) {
            // When: may_write_inline refuses the offer's size, the write is refused unpublished.
            return Committed::settled(job, ClipWriteOutcome::TooLarge);
        }
        #[cfg(any(test, feature = "test-support"))]
        if self.testing.test_clipboard_write_failure || self.test_clipboard_installed() {
            // When: test_clipboard_write_failure or test_clipboard_installed is set, the test
            // buffer takes the offer's text, never the clipboard.
            let text = String::from_utf8_lossy(offer.text().unwrap_or_default()).into_owned();
            let written = self.write_clipboard_inline(&text, job.targets);
            let outcome =
                if written { ClipWriteOutcome::Written } else { ClipWriteOutcome::Failed };
            return Committed::settled(job, outcome);
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Off macOS, an offer publishes only on the clipboard worker, so
            // without one it fails rather than block the event loop on the clipboard.
            Committed::settled(job, ClipWriteOutcome::Failed)
        }
        #[cfg(target_os = "macos")]
        {
            // The event loop is the commit point here: nothing runs between the gate and the publish.
            let gate = || job.binding.may_commit();
            let outcome = match super::kitty_clipboard_offer::publish_native(offer, &gate) {
                Published::Written => ClipWriteOutcome::Written,
                Published::Failed => ClipWriteOutcome::Failed,
                Published::Unchanged => {
                    // When: Unchanged, the gate refused at the commit point; nothing is answered.
                    return Committed::stale(job);
                }
            };
            Committed::settled(job, outcome)
        }
    }

    /// Keep each selection's retained share of a committed write and return its outcome.
    fn settle_program(&mut self, committed: Committed) -> ClipWriteOutcome {
        self.clip_state.retained.retain(committed.retained);
        committed.outcome
    }

    /// Apply every result the worker reported, in posting order: record program outcomes and the
    /// batch summaries they held back, keep retained leases, and run user-copy follow-ups.
    pub(in crate::app) fn drain_clipboard_completions(&mut self) {
        while let Some(settled) = self.clip_state.domain.next_settled() {
            match settled {
                Settled::Program { pane_id, mut committed } => {
                    // A worker-settled OSC 5522 write is answered now, if its program still asks.
                    let reply = committed.kitty.take();
                    self.send_kitty_status(pane_id, reply, &committed.binding, committed.outcome);
                    let bytes = committed.bytes;
                    let outcome = self.settle_program(committed);
                    self.note_clip_write(pane_id, bytes, outcome);
                    self.release_deferred_attempts(pane_id);
                }
                Settled::User { follow_up, written } => {
                    if written {
                        self.clip_state.retained.release_clipboard();
                    }
                    follow_up(self, written);
                }
                Settled::Lost { panes, replies, follow_ups } => {
                    // An OSC 5522 write the worker never reported failed natively: `EIO`, sent only
                    // while its program still reads the pane.
                    for route in replies {
                        let failed = ClipWriteOutcome::Failed;
                        self.send_kitty_status(route.pane_id, route.reply, &route.binding, failed);
                    }
                    for pane_id in panes {
                        self.note_clip_write(pane_id, 0, ClipWriteOutcome::Failed);
                        self.release_deferred_attempts(pane_id);
                    }
                    for follow_up in follow_ups {
                        follow_up(self, false);
                    }
                }
            }
        }
    }

    /// Record one VT batch's summary now, or hold it while a program write from `pane_id` is on
    /// the worker, so the summary shows that write's native result; held summaries merge.
    pub(in crate::app) fn note_or_defer_clip_attempts(
        &mut self,
        pane_id: u64,
        attempts: ClipAttempts,
        now: Instant,
    ) {
        if !self.clip_state.domain.program_pending(pane_id) {
            // When: no write from pane_id is on the worker, the summary shows now.
            self.note_clip_attempts(pane_id, attempts, now);
            return;
        }
        let held = self.clip_state.deferred_attempts.entry(pane_id).or_default();
        held.total = held.total.saturating_add(attempts.total);
        held.accepted = held.accepted.saturating_add(attempts.accepted);
        held.refused = held.refused.saturating_add(attempts.refused);
        held.latest = attempts.latest;
    }

    /// Show the summary held for `pane_id` once none of its program writes is on the worker.
    fn release_deferred_attempts(&mut self, pane_id: u64) {
        if self.clip_state.domain.program_pending(pane_id) {
            // When: another write from pane_id is still on the worker, its summary keeps waiting.
            return;
        }
        if let Some(attempts) = self.clip_state.deferred_attempts.remove(&pane_id) {
            self.note_clip_attempts(pane_id, attempts, Instant::now());
        }
    }

    /// Start the native worker on Linux and Windows at the first write that needs it: never on
    /// macOS, under a test clipboard, or without an event-loop proxy to wake the App.
    fn ensure_clipboard_worker(&mut self) {
        if self.clip_state.domain.has_worker()
            || cfg!(target_os = "macos")
            || self.test_clipboard_installed()
        {
            // When: has_worker already, target_os is macOS, or test_clipboard_installed, no
            // native worker starts.
            return;
        }
        let Some(proxy) = self.event_loop_proxy.clone() else {
            // When: no proxy can wake the App for results, writes stay on this thread.
            return;
        };
        let wake = Box::new(move || {
            // A closed event loop has nothing left to wake.
            let _ = proxy.send_event(UserEvent::ClipboardDone);
        });
        let delay = cfg!(target_os = "windows").then_some(OSC52_CLIPBOARD_REASSERT_DELAY);
        let _ = self.clip_state.domain.start_worker(Box::new(NativeSink::default()), delay, wake);
    }

    /// Whether this thread may make a native write of `bytes`: up to
    /// [`INLINE_WRITE_LIMIT_BYTES`], any size into a test buffer, and a macOS user copy.
    fn may_write_inline(&self, bytes: usize, user_copy: bool) -> bool {
        // A macOS user copy of any size writes NSPasteboard on this thread: off-thread pasteboard
        // use is not proven, and a size cap would refuse the large selections users copy.
        bytes <= INLINE_WRITE_LIMIT_BYTES
            || self.test_clipboard_installed()
            || (user_copy && cfg!(target_os = "macos"))
    }
}

/// The event-loop thread's own clipboard handle, or the test buffer, as a sink.
struct InlineSink<'app>(&'app mut super::App);

impl ClipboardSink for InlineSink<'_> {
    fn write(&mut self, request: WriteRequest<'_>) -> WriteResult {
        if !(request.gate)() {
            // When: the gate refuses at the commit point, the event loop writes nothing.
            return WriteResult::stale();
        }
        self.0.write_clipboard_inline_targets(request.text, request.targets)
    }
}

/// The worker's own native clipboard: an arboard handle opened on the worker thread at its first
/// write, or, on Windows, one Win32 acquisition per call over the offer backend's seam.
#[derive(Default)]
struct NativeSink {
    #[cfg(not(target_os = "windows"))]
    clipboard: Option<arboard::Clipboard>,
    #[cfg(target_os = "linux")]
    owner: super::kitty_clipboard_offer::SelectionServer,
}

impl ClipboardSink for NativeSink {
    #[cfg(not(target_os = "windows"))]
    fn write(&mut self, request: WriteRequest<'_>) -> WriteResult {
        if self.clipboard.is_none() {
            // The worker's first write opens its own handle, on the worker thread.
            self.clipboard = arboard::Clipboard::new().ok();
        }
        if !(request.gate)() {
            // When: the gate refuses at the commit point, immediately before the native calls.
            return WriteResult::stale();
        }
        let Some(clipboard) = self.clipboard.as_mut() else {
            // When: the native clipboard could not be opened, no selection changed.
            return WriteResult::each(request.targets, TargetOutcome::Refused);
        };
        write_native_targets(clipboard, request.text, request.targets)
    }

    #[cfg(target_os = "windows")]
    fn write(&mut self, request: WriteRequest<'_>) -> WriteResult {
        // Windows has no PRIMARY: the clipboard is read, gated and replaced in one acquisition.
        let (published, replaced) = super::kitty_clipboard_offer::write_windows_text(
            request.text,
            request.snapshot_max_bytes,
            request.gate,
        );
        let outcome = match published {
            Published::Written => TargetOutcome::Taken,
            Published::Failed => TargetOutcome::Refused,
            Published::Unchanged => {
                // When: Unchanged, the gate refused inside the acquisition; nothing changed.
                return WriteResult::stale();
            }
        };
        WriteResult { replaced, ..WriteResult::each(request.targets, outcome) }
    }

    #[cfg(target_os = "windows")]
    fn reassert(&mut self, text: &str, previous: &Snapshot, gate: CommitGate<'_>) -> bool {
        let published = super::kitty_clipboard_offer::reassert_windows_text(text, previous, gate);
        published == Published::Written
    }

    fn publish_offer(
        &mut self,
        offer: NativeOffer,
        lease: ClipLease,
        targets: ClipTargets,
        binding: WriteBinding,
    ) -> WriteResult {
        #[cfg(target_os = "linux")]
        {
            self.owner.publish(offer, lease, targets, binding)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _retained = lease;
            match super::kitty_clipboard_offer::publish_native(&offer, &|| binding.may_commit()) {
                Published::Written => WriteResult::each(targets, TargetOutcome::Taken),
                Published::Failed => WriteResult::each(targets, TargetOutcome::Refused),
                Published::Unchanged => WriteResult::stale(),
            }
        }
    }
}

/// Write `text` to `targets` through `clipboard`, an empty text clearing them, with each
/// selection's outcome on its own. A failed call is `Unknown`, never `Refused`: arboard's X11
/// backend stores the text before it claims the selection, so the selection may hold it.
pub(super) fn write_native_targets(
    clipboard: &mut arboard::Clipboard,
    text: &str,
    targets: ClipTargets,
) -> WriteResult {
    let outcome =
        |written: bool| if written { TargetOutcome::Taken } else { TargetOutcome::Unknown };
    let mut result = WriteResult::each(targets, TargetOutcome::Untouched);
    if targets.primary {
        result.primary = outcome(write_primary(clipboard, text));
    }
    if targets.clipboard {
        let set = if text.is_empty() {
            clipboard.clear()
        } else {
            // When: text is non-empty, it replaces the clipboard's contents.
            clipboard.set_text(text)
        };
        if let Err(error) = &set {
            tracing::warn!("clipboard set failed: {error}");
        } else {
            // When: set is Ok, record the byte count so a silent clipboard failure stays
            // distinguishable from a real copy.
            tracing::info!("copied {} bytes", text.len());
        }
        result.clipboard = outcome(set.is_ok());
    }
    result
}

/// Write PRIMARY, which exists only on Linux.
#[cfg(target_os = "linux")]
fn write_primary(clipboard: &mut arboard::Clipboard, text: &str) -> bool {
    use arboard::{LinuxClipboardKind, SetExtLinux};
    clipboard.set().clipboard(LinuxClipboardKind::Primary).text(text).is_ok()
}

/// PRIMARY does not exist here; `clip_targets` never selects it.
#[cfg(not(target_os = "linux"))]
fn write_primary(_clipboard: &mut arboard::Clipboard, _text: &str) -> bool {
    false
}

#[cfg(target_os = "windows")]
#[path = "clipboard_worker_win32.rs"]
mod win32;

/// The Windows OSC 52 read, which sizes the text block through the offer backend's
/// `Win32Clipboard` seam before it locks it.
#[cfg(target_os = "windows")]
pub(in crate::app) use super::kitty_clipboard_offer::read_windows_text;

/// The Windows OSC 5522 type listing, which shares this module's clipboard open.
#[cfg(target_os = "windows")]
pub(in crate::app) use win32::list_formats as list_windows_formats;

/// The Windows clipboard change counter, which versions a DECSET 5522 paste event's offer.
#[cfg(target_os = "windows")]
pub(in crate::app) use win32::sequence as clipboard_sequence;

#[cfg(test)]
#[path = "clipboard_worker_tests.rs"]
mod clipboard_worker_tests;
