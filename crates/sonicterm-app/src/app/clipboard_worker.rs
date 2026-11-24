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
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};

use super::event_loop::ClipWriteOutcome;
use super::osc52_stream::{
    native_clipboard_bytes, reassert_eligible, ClipLease, ClipTargets, ClipboardJob,
    MACOS_PROGRAM_WRITE_LIMIT_BYTES, MAX_PROGRAM_CLIPBOARD_JOBS, REASSERT_MAX_UTF16_UNITS,
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

/// The native clipboard calls a program write or user copy makes; tests substitute a fake.
pub(super) trait ClipboardSink {
    /// Write `text` to `targets`, an empty text clearing them; true when every target took it.
    fn write(&mut self, text: &str, targets: ClipTargets) -> bool;

    /// The clipboard's change counter, where the platform has one (Windows).
    fn sequence(&mut self) -> Option<u32> {
        None
    }

    /// The current text, when its native block is at most `max_block_bytes`.
    fn snapshot(&mut self, _max_block_bytes: usize) -> Option<Snapshot> {
        None
    }

    /// Whether the clipboard holds exactly `snapshot`: the same block size, then the same units.
    fn holds(&mut self, _snapshot: &Snapshot) -> bool {
        false
    }
}

/// Clipboard text read before a write: its native block size and its UTF-16 units.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(super) struct Snapshot {
    /// The native block's size in bytes, which a later comparison must match exactly.
    pub(super) block_bytes: usize,
    /// The text up to its terminator.
    pub(super) units: Vec<u16>,
}

/// One delayed reassertion the worker holds until it is due or a newer job cancels it.
struct PendingReassert {
    /// The program text to write again.
    text: String,
    /// The clipboard value the write replaced.
    previous: Snapshot,
    /// Covers `text`, its native copy and `previous` until the reassertion settles.
    _lease: ClipLease,
    /// The policy epoch the write was posted under.
    epoch: u64,
    /// When the reassertion runs.
    due: Instant,
}

/// What one program write left behind.
pub(super) struct Committed {
    /// Whether the native clipboard took it.
    pub(super) outcome: ClipWriteOutcome,
    /// The text's length, for the status line.
    pub(super) bytes: usize,
    /// The selections it went to.
    pub(super) targets: ClipTargets,
    /// The part of the lease covering arboard's retained copy (Linux), held until SonicTerm's next
    /// write to that selection replaces it.
    pub(super) retained: Option<ClipLease>,
    /// Where an OSC 5522 write's status goes.
    pub(super) kitty: Option<super::kitty_clipboard_tx::KittyReply>,
}

/// Commit one program write through `sink`: refused when the policy epoch moved since it was
/// posted, else the native call, which is the commit point. With `reassert_delay`, a write
/// within the reassertion bound first snapshots the value it replaces and, once written, arms one
/// reassertion that keeps the lease; otherwise the lease is released when the call returns.
fn commit_program(
    sink: &mut dyn ClipboardSink,
    job: ClipboardJob,
    epoch: u64,
    live_epoch: &AtomicU64,
    reassert_delay: Option<Duration>,
) -> (Committed, Option<PendingReassert>) {
    // The program slot stays in `job` until this returns, after the native call.
    let ClipboardJob { text, targets, mut lease, kitty, .. } = job;
    let mut committed = Committed {
        outcome: ClipWriteOutcome::Denied,
        bytes: text.len(),
        targets,
        retained: None,
        kitty,
    };
    if epoch != live_epoch.load(Ordering::SeqCst) {
        // When: the policy epoch moved after the write was posted, a reload denied OSC 52 since.
        return (committed, None);
    }
    let reassertable = reassert_delay.is_some()
        && targets.clipboard
        && !text.is_empty()
        && reassert_eligible(&text);
    let previous = reassertable.then(|| snapshot_previous(sink, &mut lease)).flatten();
    if !sink.write(&text, targets) {
        // When: sink.write was refused by the native clipboard, nothing is kept or reasserted.
        committed.outcome = ClipWriteOutcome::Failed;
        return (committed, None);
    }
    committed.outcome = ClipWriteOutcome::Written;
    if let (Some(delay), Some(previous)) = (reassert_delay, previous) {
        // When: delay is set and previous was read, one reassertion is armed and holds the lease.
        let due = Instant::now() + delay;
        return (committed, Some(PendingReassert { text, previous, _lease: lease, epoch, due }));
    }
    if cfg!(target_os = "linux") {
        // When: target_os is Linux, arboard keeps a copy until the selection is replaced.
        lease.shrink_to(native_clipboard_bytes(&text));
        committed.retained = Some(lease);
    }
    (committed, None)
}

/// Read the value a reassertable write replaces, reserving the largest snapshot on `lease` first;
/// `None` when it is unreadable, too large, or the budget cannot cover the read.
fn snapshot_previous(sink: &mut dyn ClipboardSink, lease: &mut ClipLease) -> Option<Snapshot> {
    let base = lease.bytes();
    if !lease.grow_to(base + SNAPSHOT_MAX_BLOCK_BYTES) {
        // When: grow_to fails, the staging budget cannot cover the read; no reassertion.
        return None;
    }
    let snapshot = sink.snapshot(SNAPSHOT_MAX_BLOCK_BYTES);
    let kept = snapshot.as_ref().map_or(0, |snapshot| snapshot.units.capacity() * 2);
    lease.shrink_to(base + kept);
    snapshot
}

/// Run one due reassertion: write the program's text again only while the policy epoch is
/// unchanged, the clipboard holds exactly the value the write replaced, and its change counter
/// did not move during that comparison, so a newer value always wins. True when it wrote.
fn reassert(
    sink: &mut dyn ClipboardSink,
    pending: PendingReassert,
    live_epoch: &AtomicU64,
) -> bool {
    if pending.epoch != live_epoch.load(Ordering::SeqCst) {
        // When: the pending epoch differs from live_epoch, a reload denied OSC 52 since.
        return false;
    }
    let before = sink.sequence();
    if !sink.holds(&pending.previous) {
        // When: the sink no longer holds the previous value, its newer owner keeps it.
        return false;
    }
    if sink.sequence() != before {
        // When: sequence differs from before, a newer value arrived during the comparison.
        return false;
    }
    sink.write(&pending.text, ClipTargets { clipboard: true, primary: false })
}

/// One job on the worker's queue.
enum Job {
    /// A program write and the policy epoch it was posted under.
    Program { pane_id: u64, job: ClipboardJob, epoch: u64 },
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
    live_epoch: Arc<AtomicU64>,
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
                    let _ = reassert(sink.as_mut(), pending, &live_epoch);
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
            Job::Program { pane_id, job, epoch } => {
                let (committed, pending) =
                    commit_program(sink.as_mut(), job, epoch, &live_epoch, reassert_delay);
                let reassert_armed = pending.is_some();
                armed = pending;
                report(Done::Program { pane_id, committed, reassert_armed });
            }
            Job::User { text } => {
                let written = sink.write(&text, ClipTargets { clipboard: true, primary: false });
                report(Done::User { written });
            }
        }
    }
}

/// What the event loop runs once a user copy's native result is known.
pub(in crate::app) type FollowUp = Box<dyn FnOnce(&mut super::App, bool)>;

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
    /// Bumped by every reload to `osc52 = "deny"`: a program write posted under an older value is
    /// refused at the worker's commit point, and an armed reassertion is cancelled.
    policy_epoch: Arc<AtomicU64>,
    /// Posted jobs whose results the event loop has not applied, and an armed reassertion.
    in_flight: usize,
    /// The follow-ups of the user copies in flight, oldest first.
    follow_ups: VecDeque<FollowUp>,
    /// Per pane, its program writes in flight.
    program_panes: HashMap<u64, usize>,
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
    /// The worker stopped: the panes whose writes it held and the copies it never reported.
    Lost { panes: Vec<u64>, follow_ups: Vec<FollowUp> },
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

    /// Refuse every program write the worker has not committed and cancel an armed reassertion.
    pub(in crate::app) fn revoke_program_writes(&self) {
        self.policy_epoch.fetch_add(1, Ordering::SeqCst);
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
        let live_epoch = self.policy_epoch.clone();
        let spawned = std::thread::Builder::new()
            .name("sonicterm-clipboard".into())
            .spawn(move || run_worker(sink, jobs_rx, done_tx, wake, live_epoch, reassert_delay));
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

    /// Post `job`; false when no worker runs or it stopped, and the job, its lease and slot drop.
    fn post(&mut self, job: Job) -> bool {
        let Some(worker) = self.worker.as_ref() else {
            // When: no worker runs, there is nothing to post to.
            return false;
        };
        if worker.jobs.try_send(job).is_err() {
            // When: the worker stopped, the job is refused; its result loss is reported on drain.
            return false;
        }
        self.in_flight += 1;
        true
    }

    /// Post a program write from `pane_id` under the current policy epoch.
    fn post_program(&mut self, pane_id: u64, job: ClipboardJob) -> bool {
        let epoch = self.policy_epoch.load(Ordering::SeqCst);
        let posted = self.post(Job::Program { pane_id, job, epoch });
        if posted {
            *self.program_panes.entry(pane_id).or_default() += 1;
        }
        posted
    }

    /// Whether [`MAX_USER_CLIPBOARD_JOBS`] user copies are already in flight.
    fn user_copies_full(&self) -> bool {
        self.follow_ups.len() >= MAX_USER_CLIPBOARD_JOBS
    }

    /// Post a user copy; its follow-up comes back when it could not be posted.
    fn post_user(&mut self, text: String, follow_up: FollowUp) -> Result<(), FollowUp> {
        if !self.post(Job::User { text }) {
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

    /// Route one program write: refused here under `osc52 = "deny"`, and under `"ask"` unless it
    /// was `consented` on its page; posted to the worker in order when one runs, else written here
    /// when it is small enough, else refused too large. Returns the outcome when it settled here; a
    /// posted write reports through [`Self::drain_clipboard_completions`].
    pub(in crate::app) fn submit_program_clipboard(
        &mut self,
        pane_id: u64,
        job: ClipboardJob,
        consented: bool,
    ) -> Option<ClipWriteOutcome> {
        // An OSC 5522 write settled here is answered now; one on the worker answers on drain.
        let kitty = job.kitty.clone();
        let outcome = self.route_program_clipboard(pane_id, job, consented);
        if let Some(outcome) = outcome {
            self.send_kitty_status(pane_id, kitty, outcome);
        }
        outcome
    }

    /// The routing [`Self::submit_program_clipboard`] describes, without the OSC 5522 status.
    fn route_program_clipboard(
        &mut self,
        pane_id: u64,
        job: ClipboardJob,
        consented: bool,
    ) -> Option<ClipWriteOutcome> {
        use sonicterm_cfg::config::Osc52Policy;
        let policy = self.config.terminal.osc52;
        if policy == Osc52Policy::Deny || (policy == Osc52Policy::Ask && !consented) {
            // When: osc52 is Deny, or Ask and the write was not consented, the clipboard is never
            // touched; dropping the job releases its lease and slot.
            tracing::debug!(
                bytes = job.text.len(),
                ?policy,
                "OSC 52 clipboard write denied by [terminal] osc52"
            );
            return Some(ClipWriteOutcome::Denied);
        }
        self.drain_clipboard_completions();
        self.ensure_clipboard_worker();
        if self.clip_state.domain.has_worker() {
            // When: has_worker, every program write goes to it, in order with user copies; a
            // stopped worker refuses the post and the write fails.
            let posted = self.clip_state.domain.post_program(pane_id, job);
            return (!posted).then_some(ClipWriteOutcome::Failed);
        }
        if !self.may_write_inline(job.text.len(), false) {
            // When: may_write_inline refuses its size with no worker, the write is refused.
            return Some(ClipWriteOutcome::TooLarge);
        }
        let live_epoch = self.clip_state.domain.policy_epoch.clone();
        let epoch = live_epoch.load(Ordering::SeqCst);
        let (committed, _) = commit_program(&mut InlineSink(self), job, epoch, &live_epoch, None);
        Some(self.settle_program(committed))
    }

    /// Keep a committed write's retained lease and return its outcome.
    fn settle_program(&mut self, committed: Committed) -> ClipWriteOutcome {
        if let Some(lease) = committed.retained {
            self.clip_state.retained.retain(committed.targets, lease);
        }
        committed.outcome
    }

    /// Apply every result the worker reported, in posting order: record program outcomes and the
    /// batch summaries they held back, keep retained leases, and run user-copy follow-ups.
    pub(in crate::app) fn drain_clipboard_completions(&mut self) {
        while let Some(settled) = self.clip_state.domain.next_settled() {
            match settled {
                Settled::Program { pane_id, mut committed } => {
                    // A worker-settled OSC 5522 write is answered on its source pane now.
                    let kitty = committed.kitty.take();
                    self.send_kitty_status(pane_id, kitty, committed.outcome);
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
                Settled::Lost { panes, follow_ups } => {
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
        // shortcut: a macOS user copy of any size still writes NSPasteboard on this thread, as
        // off-thread pasteboard use is not proven; only program writes over the limit fail closed.
        bytes <= INLINE_WRITE_LIMIT_BYTES
            || self.test_clipboard_installed()
            || (user_copy && cfg!(target_os = "macos"))
    }
}

/// The event-loop thread's own clipboard handle, or the test buffer, as a sink.
struct InlineSink<'app>(&'app mut super::App);

impl ClipboardSink for InlineSink<'_> {
    fn write(&mut self, text: &str, targets: ClipTargets) -> bool {
        self.0.write_clipboard_inline(text, targets)
    }
}

/// The worker's own native clipboard handle, opened on the worker thread at its first write.
#[derive(Default)]
struct NativeSink {
    clipboard: Option<arboard::Clipboard>,
}

impl ClipboardSink for NativeSink {
    fn write(&mut self, text: &str, targets: ClipTargets) -> bool {
        if self.clipboard.is_none() {
            // The worker's first write opens its own handle, on the worker thread.
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let Some(clipboard) = self.clipboard.as_mut() else {
            // When: the native clipboard could not be opened, the write fails.
            return false;
        };
        write_native_clipboard(clipboard, text, targets)
    }

    #[cfg(target_os = "windows")]
    fn sequence(&mut self) -> Option<u32> {
        Some(win32::sequence())
    }

    #[cfg(target_os = "windows")]
    fn snapshot(&mut self, max_block_bytes: usize) -> Option<Snapshot> {
        win32::snapshot(max_block_bytes)
    }

    #[cfg(target_os = "windows")]
    fn holds(&mut self, snapshot: &Snapshot) -> bool {
        win32::holds(snapshot)
    }
}

/// Write `text` to `targets` through `clipboard`; an empty text clears them. True when every
/// target took it.
pub(super) fn write_native_clipboard(
    clipboard: &mut arboard::Clipboard,
    text: &str,
    targets: ClipTargets,
) -> bool {
    let mut written = true;
    if targets.primary {
        written &= write_primary(clipboard, text);
    }
    if targets.clipboard {
        let result = if text.is_empty() {
            clipboard.clear()
        } else {
            // When: text is non-empty, it replaces the clipboard's contents.
            clipboard.set_text(text)
        };
        if let Err(error) = result {
            tracing::warn!("clipboard set failed: {error}");
            written = false;
        } else {
            // When: result is Ok, record the byte count so a silent clipboard failure stays
            // distinguishable from a real copy.
            tracing::info!("copied {} bytes", text.len());
        }
    }
    written
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

/// The Windows OSC 52 read, which shares this module's clipboard open and block sizing.
#[cfg(target_os = "windows")]
pub(in crate::app) use win32::read_text as read_windows_text;

#[cfg(test)]
#[path = "clipboard_worker_tests.rs"]
mod clipboard_worker_tests;
