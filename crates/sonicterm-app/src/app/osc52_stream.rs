//! OSC 52 and OSC 1337 `Copy=:` writes above the parser: streaming Base64 decode, the staging
//! budget every buffer is reserved from before it is allocated, and the bounded program slots a
//! write takes before it is posted to the event loop.
//!
//! The parser streams encoded chunks into its inbox and raises payload-free events; the pane's
//! VT worker feeds them to its [`Osc52Intake`] outside the parser lock. A complete write becomes
//! one [`ClipboardJobHandle`], whose lease is held until the native clipboard call returns.

use std::sync::{
    atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering},
    Arc, OnceLock,
};
use std::time::{Duration, Instant};

use sonicterm_cfg::config::{Osc52Policy, Osc52ReadPolicy};
use sonicterm_vt::vt::{ClipboardInbox, ClipboardPhase};

use super::osc52_read_ticket::{
    admit_read, admit_write, Admission, ConsentTicket, ReadIdentity, WriteAdmission, WriteBinding,
};
use super::spawn_pane::ClipAttempts;
pub(super) use super::spawn_pane::Osc52Refusal;
use super::UserEvent;

/// The smallest process-wide clipboard staging budget.
pub(super) const MIN_CLIPBOARD_BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// Program writes that may be posted to the event loop and not yet handled.
pub(super) const MAX_PROGRAM_CLIPBOARD_JOBS: usize = 8;

/// How long a clipboard stream may stay open without BEL or ST before it is aborted.
pub(super) const OSC52_STREAM_EXPIRY: Duration = Duration::from_secs(60);

/// The largest program write macOS accepts: macOS has no clipboard worker, because off-thread
/// `NSPasteboard` use is not proven, so larger writes fail closed rather than stall the loop.
pub(super) const MACOS_PROGRAM_WRITE_LIMIT_BYTES: usize = 512 * 1024;

/// The largest write Windows reasserts, in UTF-16 units (512 KiB of `CF_UNICODETEXT`).
pub(super) const REASSERT_MAX_UTF16_UNITS: usize = 262_144;

/// The process-wide staging budget and program slots every pane's clipboard writes share.
#[derive(Debug)]
pub(in crate::app) struct ClipboardStaging {
    /// Bytes every live lease may hold together.
    budget_bytes: usize,
    /// Bytes reserved by live leases.
    reserved_bytes: AtomicUsize,
    /// The decoded-byte limit a newly opened stream reads.
    stream_limit_bytes: AtomicUsize,
    /// Program writes posted and not yet handled.
    program_slots: AtomicUsize,
    /// `[terminal] osc52_read`, as [`read_policy_code`] encodes it; VT workers read it per read.
    read_policy: AtomicU8,
    /// `[terminal] osc52`, as [`write_policy_code`] encodes it; VT workers read it per write.
    write_policy: AtomicU8,
    /// Read tickets held, at most `MAX_TICKETS`.
    ticket_slots: AtomicUsize,
    /// Bumped by every change to `osc52` or `osc52_read`; a ticket bound to another is cancelled.
    policy_generation: AtomicU64,
    /// One capacity-1 wakeup per VT worker, so a policy change reaches an open OSC 5522 write
    /// even while its child prints nothing.
    policy_wakers: std::sync::Mutex<Vec<crossbeam_channel::Sender<()>>>,
}

/// The stored form of the write `policy`.
fn write_policy_code(policy: Osc52Policy) -> u8 {
    match policy {
        Osc52Policy::Allow => 0,
        Osc52Policy::Ask => 1,
        Osc52Policy::Deny => 2,
    }
}

/// The stored form of `policy`.
fn read_policy_code(policy: Osc52ReadPolicy) -> u8 {
    match policy {
        Osc52ReadPolicy::Ask => 0,
        Osc52ReadPolicy::Allow => 1,
        Osc52ReadPolicy::Deny => 2,
    }
}

impl ClipboardStaging {
    /// A staging sized for `startup_limit_bytes`: `max(128 MiB, 2 × limit)`.
    pub(in crate::app) fn for_startup_limit(startup_limit_bytes: usize) -> Arc<Self> {
        let budget = MIN_CLIPBOARD_BUDGET_BYTES.max(startup_limit_bytes.saturating_mul(2));
        let staging = Self::with_budget(budget, 0);
        staging.set_stream_limit(startup_limit_bytes);
        staging
    }

    /// A staging with an explicit budget and stream limit, which tests inject.
    pub(in crate::app) fn with_budget(budget_bytes: usize, stream_limit_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            budget_bytes,
            reserved_bytes: AtomicUsize::new(0),
            stream_limit_bytes: AtomicUsize::new(stream_limit_bytes),
            program_slots: AtomicUsize::new(0),
            read_policy: AtomicU8::new(read_policy_code(Osc52ReadPolicy::default())),
            write_policy: AtomicU8::new(write_policy_code(Osc52Policy::default())),
            ticket_slots: AtomicUsize::new(0),
            policy_generation: AtomicU64::new(0),
            policy_wakers: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// Register one VT worker's policy wakeup; a wakeup already pending coalesces with the next.
    /// Workers that ended are pruned here and at every wake, so the list tracks live panes.
    pub(in crate::app) fn register_policy_waker(&self) -> crossbeam_channel::Receiver<()> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        let mut wakers =
            self.policy_wakers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // A live worker woken here only rechecks its open write, which the policy still admits.
        wakers.retain(|waker| {
            !matches!(waker.try_send(()), Err(crossbeam_channel::TrySendError::Disconnected(_)))
        });
        wakers.push(sender);
        receiver
    }

    /// Wake every VT worker to recheck its open OSC 5522 write against the current policy.
    pub(in crate::app) fn wake_policy_workers(&self) {
        let mut wakers =
            self.policy_wakers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        wakers.retain(|waker| {
            !matches!(waker.try_send(()), Err(crossbeam_channel::TrySendError::Disconnected(_)))
        });
    }

    /// Publish `[terminal] osc52_read`; reads that end from now on follow it.
    // Ordering: read_policy is Relaxed; each read samples one whole value and orders no data.
    pub(in crate::app) fn set_read_policy(&self, policy: Osc52ReadPolicy) {
        self.read_policy.store(read_policy_code(policy), Ordering::Relaxed);
    }

    /// The `[terminal] osc52_read` policy a read ending now follows.
    // Ordering: read_policy is Relaxed; each read samples one whole value and orders no data.
    pub(in crate::app) fn read_policy(&self) -> Osc52ReadPolicy {
        match self.read_policy.load(Ordering::Relaxed) {
            0 => Osc52ReadPolicy::Ask,
            1 => Osc52ReadPolicy::Allow,
            _ => Osc52ReadPolicy::Deny,
        }
    }

    /// Publish `[terminal] osc52`; writes that end from now on follow it.
    // Ordering: write_policy is Relaxed; each write samples one whole value and orders no data.
    pub(in crate::app) fn set_write_policy(&self, policy: Osc52Policy) {
        self.write_policy.store(write_policy_code(policy), Ordering::Relaxed);
    }

    /// The `[terminal] osc52` policy a write ending now follows.
    // Ordering: write_policy is Relaxed; each write samples one whole value and orders no data.
    pub(in crate::app) fn write_policy(&self) -> Osc52Policy {
        match self.write_policy.load(Ordering::Relaxed) {
            0 => Osc52Policy::Allow,
            1 => Osc52Policy::Ask,
            _ => Osc52Policy::Deny,
        }
    }

    /// Read tickets held now.
    // Ordering: ticket_slots is Relaxed; it is a counter that orders no data.
    #[cfg(test)]
    pub(in crate::app) fn ticket_slots_in_use(&self) -> usize {
        self.ticket_slots.load(Ordering::Relaxed)
    }

    /// Take one of `MAX_TICKETS` read slots; false, unchanged, when all are held.
    pub(in crate::app) fn try_take_ticket_slot(&self) -> bool {
        bounded_add(&self.ticket_slots, 1, super::osc52_read::MAX_TICKETS)
    }

    /// Return a read slot a dropped ticket held.
    // Ordering: ticket_slots is Relaxed; a release only lowers the counter.
    pub(in crate::app) fn release_ticket_slot(&self) {
        self.ticket_slots.fetch_sub(1, Ordering::Relaxed);
    }

    /// The policy generation a read binds now; sampled before [`Self::read_policy`].
    pub(in crate::app) fn policy_generation(&self) -> u64 {
        self.policy_generation.load(Ordering::SeqCst)
    }

    /// Start a new policy generation, after the new policy is stored: a VT worker that samples it
    /// also reads the new policy, and every ticket bound to an older one is cancelled.
    pub(in crate::app) fn bump_policy_generation(&self) {
        self.policy_generation.fetch_add(1, Ordering::SeqCst);
    }

    /// The staging every production pane shares, sized by the startup config through
    /// [`Self::init_process_default`]; the default limit applies when nothing initialized it.
    pub(in crate::app) fn process_default() -> Arc<Self> {
        Self::init_process_default(sonicterm_cfg::config::DEFAULT_CLIPBOARD_WRITE_LIMIT_BYTES)
    }

    /// Size the process staging from the startup limit; later calls return the first staging.
    pub(in crate::app) fn init_process_default(startup_limit_bytes: usize) -> Arc<Self> {
        static PROCESS: OnceLock<Arc<ClipboardStaging>> = OnceLock::new();
        PROCESS.get_or_init(|| Self::for_startup_limit(startup_limit_bytes)).clone()
    }

    /// Set the limit new streams read, capped at half the budget and, on macOS, at
    /// [`MACOS_PROGRAM_WRITE_LIMIT_BYTES`]; open streams and leases keep theirs.
    // Ordering: stream_limit_bytes is Relaxed; a stream reads one value when it opens.
    pub(in crate::app) fn set_stream_limit(&self, limit_bytes: usize) {
        let mut limit = limit_bytes.min(self.budget_bytes / 2);
        if cfg!(target_os = "macos") {
            // When: target_os is macOS, program writes stay at or below the event-loop limit.
            limit = limit.min(MACOS_PROGRAM_WRITE_LIMIT_BYTES);
        }
        self.stream_limit_bytes.store(limit, Ordering::Relaxed);
    }

    /// The limit a stream opened now reads.
    // Ordering: stream_limit_bytes is Relaxed; a stream reads one value when it opens.
    pub(in crate::app) fn stream_limit(&self) -> usize {
        self.stream_limit_bytes.load(Ordering::Relaxed)
    }

    /// Bytes live leases hold.
    // Ordering: reserved_bytes is Relaxed; it is a sampled accounting total.
    pub(in crate::app) fn reserved_bytes(&self) -> usize {
        self.reserved_bytes.load(Ordering::Relaxed)
    }

    /// Program writes posted and not yet handled.
    // Ordering: program_slots is Relaxed; it is a counter that orders no data.
    pub(in crate::app) fn program_slots_in_use(&self) -> usize {
        self.program_slots.load(Ordering::Relaxed)
    }

    /// An empty lease against this staging.
    pub(in crate::app) fn lease(self: &Arc<Self>) -> ClipLease {
        ClipLease { staging: self.clone(), bytes: 0 }
    }

    /// Reserve `bytes` more if the budget covers them.
    fn reserve(&self, bytes: usize) -> bool {
        bounded_add(&self.reserved_bytes, bytes, self.budget_bytes)
    }

    // Ordering: reserved_bytes is Relaxed; a release only lowers the accounting total.
    fn release(&self, bytes: usize) {
        self.reserved_bytes.fetch_sub(bytes, Ordering::Relaxed);
    }

    /// Take one of [`MAX_PROGRAM_CLIPBOARD_JOBS`] program slots.
    fn take_program_slot(self: &Arc<Self>) -> Option<ProgramSlot> {
        bounded_add(&self.program_slots, 1, MAX_PROGRAM_CLIPBOARD_JOBS)
            .then(|| ProgramSlot { staging: self.clone() })
    }
}

/// Add `amount` to `counter` only while the total stays at most `bound`; false, unchanged, else.
// Ordering: counter uses Relaxed compare-exchange; only the bound matters, no data is published.
fn bounded_add(counter: &AtomicUsize, amount: usize, bound: usize) -> bool {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let Some(total) = current.checked_add(amount).filter(|total| *total <= bound) else {
            // When: the total would pass `bound`, nothing is added.
            return false;
        };
        match counter.compare_exchange_weak(current, total, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => {
                // When: compare_exchange_weak stored total, the amount is added.
                return true;
            }
            Err(observed) => current = observed,
        }
    }
}

/// A move-only reservation against [`ClipboardStaging`], released when dropped.
#[derive(Debug)]
pub(in crate::app) struct ClipLease {
    staging: Arc<ClipboardStaging>,
    bytes: usize,
}

impl ClipLease {
    /// Bytes this lease holds.
    pub(in crate::app) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Grow the reservation to `total` bytes; false, unchanged, when the budget cannot cover it.
    pub(in crate::app) fn grow_to(&mut self, total: usize) -> bool {
        if total <= self.bytes {
            // When: `total` is already covered, nothing more is reserved.
            return true;
        }
        let reserved = self.staging.reserve(total - self.bytes);
        if reserved {
            self.bytes = total;
        }
        reserved
    }

    /// Move `bytes` of this reservation, at most what it holds, into a new lease on the same
    /// staging; the total reserved is unchanged.
    pub(in crate::app) fn split_off(&mut self, bytes: usize) -> ClipLease {
        let moved = bytes.min(self.bytes);
        self.bytes -= moved;
        ClipLease { staging: self.staging.clone(), bytes: moved }
    }

    /// Shrink the reservation to `total` bytes.
    pub(in crate::app) fn shrink_to(&mut self, total: usize) {
        if total < self.bytes {
            self.staging.release(self.bytes - total);
            self.bytes = total;
        }
    }
}

// Lifecycle: dropping a `ClipLease` returns its `bytes` to the staging budget.
impl Drop for ClipLease {
    fn drop(&mut self) {
        self.staging.release(self.bytes);
    }
}

/// One of the program slots a write takes before it is posted; released when dropped.
#[derive(Debug)]
struct ProgramSlot {
    staging: Arc<ClipboardStaging>,
}

// Lifecycle: dropping a `ProgramSlot` releases its program_slots count for the next write.
impl Drop for ProgramSlot {
    // Ordering: program_slots is Relaxed; it is a counter that orders no data.
    fn drop(&mut self) {
        self.staging.program_slots.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Where one write goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct ClipTargets {
    /// The standard clipboard.
    pub(in crate::app) clipboard: bool,
    /// The X11/Wayland PRIMARY selection.
    pub(in crate::app) primary: bool,
}

/// The destinations an OSC 52 kinds field names: an empty field, `c`, `s` and `0`–`9` mean the
/// clipboard, `p` means PRIMARY where `has_primary`, and other letters are ignored; a field with
/// no known letter is refused, naming its first letter.
pub(in crate::app) fn clip_targets(
    kinds: &[u8],
    has_primary: bool,
) -> Result<ClipTargets, Osc52Refusal> {
    if kinds.is_empty() {
        // When: `kinds` is empty, the write goes to the clipboard.
        return Ok(ClipTargets { clipboard: true, primary: false });
    }
    let mut targets = ClipTargets { clipboard: false, primary: false };
    for kind in kinds {
        match kind {
            b'c' | b's' | b'0'..=b'9' => targets.clipboard = true,
            b'p' => targets.primary = has_primary,
            _ => {
                // When: `kind` names no known destination, it is ignored.
            }
        }
    }
    if !targets.clipboard && !targets.primary {
        // When: targets names neither clipboard nor primary, the write is dropped.
        return Err(Osc52Refusal::Selection(char::from(kinds[0])));
    }
    Ok(targets)
}

/// Incremental Base64 decoding with a strict alphabet, optional final padding and at most 3
/// sextets carried between chunks; every output byte is reserved before it is allocated.
pub(in crate::app) struct Base64Stream {
    sextets: [u8; 4],
    pending: usize,
    /// Padding characters seen; any non-padding byte after one is invalid.
    padding: usize,
    output: Vec<u8>,
    lease: ClipLease,
    limit_bytes: usize,
}

impl Base64Stream {
    /// A decoder whose output is reserved through `lease` and capped at `limit_bytes`.
    pub(in crate::app) fn new(lease: ClipLease, limit_bytes: usize) -> Self {
        Self { sextets: [0; 4], pending: 0, padding: 0, output: Vec::new(), lease, limit_bytes }
    }

    /// The output buffer's capacity.
    #[cfg(test)]
    pub(in crate::app) fn output_capacity(&self) -> usize {
        self.output.capacity()
    }

    /// Decode one chunk; on a refusal the caller drops the decoder, releasing its lease.
    pub(in crate::app) fn feed(&mut self, chunk: &[u8]) -> Result<(), Osc52Refusal> {
        // A chunk adds at most 3 bytes per complete group; an unpadded tail adds at most 2 more.
        let growth = (self.pending + chunk.len()) / 4 * 3;
        if self.output.len() + growth > self.limit_bytes + 2 {
            // When: the chunk decodes past the limit even allowing for padding, refuse it before
            // the output buffer grows.
            return Err(Osc52Refusal::TooLarge);
        }
        self.ensure_capacity(self.output.len() + growth)?;
        for byte in chunk {
            if *byte == b'=' {
                // When: byte is padding, it is checked and counted, never decoded.
                if self.pending < 2 || self.padding + self.pending >= 4 {
                    // When: padding follows fewer than two sextets or overfills the group, the
                    // Base64 is malformed.
                    return Err(Osc52Refusal::Invalid);
                }
                self.padding += 1;
                continue;
            }
            let Some(sextet) = sextet(*byte) else {
                // When: `byte` is outside the standard alphabet, the whole write is discarded.
                return Err(Osc52Refusal::Invalid);
            };
            if self.padding > 0 {
                // When: data follows padding, the Base64 is malformed.
                return Err(Osc52Refusal::Invalid);
            }
            self.sextets[self.pending] = sextet;
            self.pending += 1;
            if self.pending == 4 {
                self.flush_group(3)?;
            }
        }
        Ok(())
    }

    /// End the stream: decode the final group and return the bytes with their lease.
    pub(in crate::app) fn finish(mut self) -> Result<(Vec<u8>, ClipLease), Osc52Refusal> {
        match self.pending {
            0 => {
                // When: pending is 0, every group was already decoded.
            }
            1 => {
                // When: pending is 1, one sextet encodes no whole byte.
                return Err(Osc52Refusal::Invalid);
            }
            pending => {
                self.ensure_capacity(self.output.len() + pending - 1)?;
                self.flush_group(pending - 1)?;
            }
        }
        Ok((self.output, self.lease))
    }

    /// Append the first `byte_count` bytes of the pending group.
    fn flush_group(&mut self, byte_count: usize) -> Result<(), Osc52Refusal> {
        if self.output.len() + byte_count > self.limit_bytes {
            // When: output plus byte_count passes limit_bytes, the write is too large.
            return Err(Osc52Refusal::TooLarge);
        }
        let [first, second, third, fourth] = self.sextets;
        let decoded = [first << 2 | second >> 4, second << 4 | third >> 2, third << 6 | fourth];
        self.output.extend_from_slice(&decoded[..byte_count]);
        self.sextets = [0; 4];
        self.pending = 0;
        Ok(())
    }

    /// Reserve, then allocate, room for `needed` output bytes: doubling within the limit when the
    /// budget allows it, else exactly `needed`.
    fn ensure_capacity(&mut self, needed: usize) -> Result<(), Osc52Refusal> {
        if needed <= self.output.capacity() {
            // When: the buffer already holds `needed`, nothing is reserved or allocated.
            return Ok(());
        }
        let doubled = needed.max(self.output.capacity() * 2).min(self.limit_bytes + 2);
        let target = if self.lease.grow_to(doubled.max(needed)) {
            doubled.max(needed)
        } else if self.lease.grow_to(needed) {
            // When: the budget cannot cover doubling, it may still cover exactly `needed`.
            needed
        } else {
            // When: the budget cannot cover `needed`, the write is refused busy.
            return Err(Osc52Refusal::Busy);
        };
        self.output.reserve_exact(target - self.output.len());
        Ok(())
    }
}

/// The value of one standard-alphabet Base64 character.
pub(in crate::app) fn sextet(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// One complete program write: its text, destinations, lease and program slot. Its `Debug`
/// prints lengths only, so a job can be logged without the clipboard text.
pub(in crate::app) struct ClipboardJob {
    pub(in crate::app) text: String,
    pub(in crate::app) targets: ClipTargets,
    /// Covers `text` and one native copy per selection until the native call returns.
    pub(in crate::app) lease: ClipLease,
    _slot: ProgramSlot,
    /// Where an OSC 5522 write's status goes once it settles; `None` for OSC 52.
    pub(in crate::app) kitty: Option<super::kitty_clipboard_tx::KittyReply>,
    /// An OSC 5522 write's representations beside text, published together; `text` is then empty.
    pub(in crate::app) offer: Option<super::kitty_clipboard_offer::NativeOffer>,
    /// What the write was admitted under, checked at the commit point and before its status.
    pub(in crate::app) binding: WriteBinding,
}

impl ClipboardJob {
    /// A clipboard-target job for `text` on `staging`, its lease and program slot taken as the
    /// reader thread takes them.
    #[cfg(test)]
    pub(in crate::app) fn for_test(staging: &Arc<ClipboardStaging>, text: &str) -> Self {
        let mut lease = staging.lease();
        assert!(lease.grow_to(text.len() + native_clipboard_bytes(text)), "test staging budget");
        let slot = staging.take_program_slot().expect("a free program slot");
        let targets = ClipTargets { clipboard: true, primary: false };
        let binding = WriteBinding::unbound(staging);
        Self {
            text: text.to_owned(),
            targets,
            lease,
            _slot: slot,
            kitty: None,
            offer: None,
            binding,
        }
    }

    /// The write an OSC 5522 commit resolved: `bytes` become text with `lease` grown first, and a
    /// program slot is taken, as an OSC 52 write's are; its status goes back through `reply`.
    pub(in crate::app) fn kitty(
        staging: &Arc<ClipboardStaging>,
        bytes: Vec<u8>,
        lease: ClipLease,
        targets: ClipTargets,
        reply: super::kitty_clipboard_tx::KittyReply,
    ) -> Result<Self, Osc52Refusal> {
        let (text, lease) = into_text(bytes, lease, targets)?;
        let slot = staging.take_program_slot().ok_or(Osc52Refusal::Busy)?;
        let binding = WriteBinding::unbound(staging);
        Ok(Self { text, targets, lease, _slot: slot, kitty: Some(reply), offer: None, binding })
    }

    /// The multi-representation write an OSC 5522 commit planned, `lease` already covering it.
    pub(in crate::app) fn kitty_offer(
        staging: &Arc<ClipboardStaging>,
        offer: super::kitty_clipboard_offer::NativeOffer,
        lease: ClipLease,
        targets: ClipTargets,
        reply: super::kitty_clipboard_tx::KittyReply,
    ) -> Result<Self, Osc52Refusal> {
        let slot = staging.take_program_slot().ok_or(Osc52Refusal::Busy)?;
        let (text, kitty, offer) = (String::new(), Some(reply), Some(offer));
        let binding = WriteBinding::unbound(staging);
        Ok(Self { text, targets, lease, _slot: slot, kitty, offer, binding })
    }
}

/// A program write posted to the event loop. Clones share one job, which is taken once; `Debug`
/// prints only its length, so an event can be cloned or logged without copying the payload.
#[derive(Clone)]
pub struct ClipboardJobHandle(Arc<std::sync::Mutex<Option<ClipboardJob>>>);

impl ClipboardJobHandle {
    /// Share `job` for one event.
    pub(in crate::app) fn new(job: ClipboardJob) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Some(job))))
    }

    /// Take the job; `None` when a clone already took it.
    pub(in crate::app) fn take(&self) -> Option<ClipboardJob> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }
}

impl PartialEq for ClipboardJobHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ClipboardJobHandle {}

impl std::fmt::Debug for ClipboardJobHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let text_bytes = guard.as_ref().map(|job| job.text.len());
        formatter.debug_struct("ClipboardJobHandle").field("text_bytes", &text_bytes).finish()
    }
}

/// A request the intake accepted: a write to post, or a consent ticket it holds for the section.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::app) enum Accepted {
    /// A complete write, posted to the event loop.
    Write(ClipboardJobHandle),
    /// A read or an asked write took a ticket, posted once the section's epoch is published.
    Ticket,
}

impl Clone for Osc52Intake {
    /// A fresh intake on the same staging, pane and read identity: an open stream's decoder and
    /// lease, and a held read ticket, belong to the one worker that tracks them.
    fn clone(&self) -> Self {
        let mut intake = Self::new(self.staging.clone()).with_identity(self.identity.clone());
        intake.pane_id = self.pane_id;
        intake
    }
}

/// The open stream of one pane: its id, when it opened, the policy generation it opened under, and
/// its decoder (`None` once refused).
struct OpenStream {
    stream: u64,
    opened_at: Instant,
    policy_generation: u64,
    decoder: Result<Base64Stream, Osc52Refusal>,
}

/// One pane's clipboard streams, driven by its VT worker outside the parser lock.
pub(in crate::app) struct Osc52Intake {
    staging: Arc<ClipboardStaging>,
    open: Option<OpenStream>,
    /// Empty read replies queued since the last [`Self::take_read_replies`]; never clipboard data.
    read_replies: Vec<u8>,
    /// The pane's read identity, which every read ticket binds.
    identity: Arc<ReadIdentity>,
    /// The pane the tickets name; 0 until bound.
    pane_id: u64,
    /// The command epoch at the request being handled, which [`Self::run_section`] sets.
    request_epoch: Option<u64>,
    /// Tickets admitted in this section, posted once its command epoch is published.
    tickets: Vec<ConsentTicket>,
}

impl Osc52Intake {
    /// An intake with no open stream, reserving from `staging`.
    pub(in crate::app) fn new(staging: Arc<ClipboardStaging>) -> Self {
        Self {
            staging,
            open: None,
            read_replies: Vec::new(),
            identity: Arc::default(),
            pane_id: 0,
            request_epoch: None,
            tickets: Vec::new(),
        }
    }

    /// This intake, binding reads to the pane's shared `identity`.
    pub(in crate::app) fn with_identity(mut self, identity: Arc<ReadIdentity>) -> Self {
        self.identity = identity;
        self
    }

    /// Name `pane_id` in the events and tickets this intake raises.
    pub(in crate::app) fn bind_pane(&mut self, pane_id: u64) {
        self.pane_id = pane_id;
    }

    /// Reserve from `staging` instead, dropping any open stream; the pane binding stays.
    #[cfg(any(test, feature = "test-support"))]
    pub(in crate::app) fn replace_staging(&mut self, staging: Arc<ClipboardStaging>) {
        self.open = None;
        self.staging = staging;
    }

    /// Take the read tickets admitted since the last call.
    pub(in crate::app) fn take_tickets(&mut self) -> Vec<ConsentTicket> {
        std::mem::take(&mut self.tickets)
    }

    /// Answer one read of `kinds`: silent and refused under `deny`, a ticket while this pane and
    /// the process have a slot, and otherwise empty replies at once, refused busy.
    fn answer_read(&mut self, kinds: &[u8], now: Instant) -> Result<Accepted, Osc52Refusal> {
        let epoch = self.request_epoch.unwrap_or_else(|| self.identity.command_epoch());
        match admit_read(&self.staging, &self.identity, self.pane_id, epoch, kinds, now) {
            Admission::Silent => Err(Osc52Refusal::Read),
            Admission::Busy => {
                self.read_replies.extend(super::osc52_read::empty_replies(kinds));
                Err(Osc52Refusal::Busy)
            }
            Admission::Ticket(ticket) => {
                self.tickets.push(ticket);
                Ok(Accepted::Ticket)
            }
        }
    }

    /// Route one complete `job`: posted under `allow` and `deny`, held on a ticket under `ask`, or
    /// refused busy when no ticket slot is free.
    fn route_write(&mut self, job: ClipboardJob, now: Instant) -> Result<Accepted, Osc52Refusal> {
        let epoch = self.request_epoch.unwrap_or_else(|| self.identity.command_epoch());
        match admit_write(&self.staging, &self.identity, self.pane_id, epoch, job, now) {
            WriteAdmission::Post(job) => {
                Ok(Accepted::Write(ClipboardJobHandle(Arc::new(std::sync::Mutex::new(Some(job))))))
            }
            WriteAdmission::Busy => Err(Osc52Refusal::Busy),
            WriteAdmission::Ticket(ticket) => {
                self.tickets.push(ticket);
                Ok(Accepted::Ticket)
            }
        }
    }

    /// Run one parser section's clipboard events, each tagged with the command boundaries the
    /// section passed before it and its position on the wire: post every write, with
    /// `other_writes` (the section's OSC 5522 writes, by position) merged in wire order, publish
    /// the section's command epoch, then post its read tickets and its refusal summary. Returns
    /// the section's empty read replies.
    pub(in crate::app) fn run_section(
        &mut self,
        inbox: Option<&ClipboardInbox>,
        parser_open: Option<u64>,
        requests: Vec<(u64, ClipboardPhase, u64, usize)>,
        boundaries: u64,
        other_writes: Vec<(usize, ClipboardJobHandle)>,
        mut emit: impl FnMut(UserEvent),
    ) -> Vec<u8> {
        let base_epoch = self.identity.command_epoch();
        let mut attempts = ClipAttempts::default();
        let mut other_writes = other_writes.into_iter().peekable();
        let pane_id = self.pane_id;
        // Posts every other write that came before `position` on the wire.
        let mut post_before = |position: usize, emit: &mut dyn FnMut(UserEvent)| {
            while let Some((_, job)) = other_writes.next_if(|(other, _)| *other < position) {
                emit(UserEvent::ClipboardWrite { pane_id, job });
            }
        };
        if inbox.is_some() || parser_open.is_some() {
            // When: inbox or parser_open is set, this section touched a clipboard stream.
            let at = Instant::now();
            for (stream, phase, passed, position) in requests {
                let Some(inbox) = inbox else {
                    // When: no inbox was taken, the section raised no clipboard event.
                    break;
                };
                self.request_epoch = Some(base_epoch + passed);
                let result = self.handle(inbox, stream, phase, at);
                self.request_epoch = None;
                let Some(result) = result else {
                    // When: result is None, the event was a chunk or an abort with no outcome yet.
                    continue;
                };
                attempts.total = attempts.total.saturating_add(1);
                match result {
                    Ok(accepted) => {
                        attempts.accepted = attempts.accepted.saturating_add(1);
                        attempts.latest = None;
                        if let Accepted::Write(job) = accepted {
                            // A write is posted now, after every write before it on the wire; a
                            // ticket is posted below, after the epoch.
                            post_before(position, &mut emit);
                            emit(UserEvent::ClipboardWrite { pane_id: self.pane_id, job });
                        }
                    }
                    Err(reason) => {
                        attempts.refused = attempts.refused.saturating_add(1);
                        attempts.latest = Some(reason);
                    }
                }
            }
            self.reconcile(parser_open, at);
        } else {
            // When: neither inbox nor parser_open is set, a stream the parser reset is released.
            self.release_open();
        }
        // The section's remaining writes follow every OSC 52 write, before its epoch.
        post_before(usize::MAX, &mut emit);
        if boundaries > 0 {
            // Published before the tickets: one from before a boundary already mismatches.
            self.identity.set_command_epoch(base_epoch + boundaries);
        }
        for ticket in self.take_tickets() {
            let ticket = super::osc52_read_ticket::ClipboardTicketHandle::new(ticket);
            emit(UserEvent::ClipboardTicket { pane_id: self.pane_id, ticket });
        }
        if attempts.total > 0 {
            emit(UserEvent::ClipboardAttempts { pane_id: self.pane_id, attempts });
        }
        self.take_read_replies()
    }

    /// Take the empty read replies queued since the last call, for the batch's reply path.
    pub(in crate::app) fn take_read_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.read_replies)
    }

    /// Track `stream`, replacing (and releasing) a different stream the parser abandoned.
    fn track(&mut self, stream: u64, now: Instant) -> &mut OpenStream {
        if self.open.as_ref().is_none_or(|open| open.stream != stream) {
            // `stream` is new, so it reads the stream limit now.
            let decoder = Base64Stream::new(self.staging.lease(), self.staging.stream_limit());
            let policy_generation = self.staging.policy_generation();
            let open =
                OpenStream { stream, opened_at: now, policy_generation, decoder: Ok(decoder) };
            self.open = Some(open);
        }
        self.open.as_mut().expect("tracked above")
    }

    /// Apply one stream event; returns the finished write or refusal when the stream ends.
    pub(in crate::app) fn handle(
        &mut self,
        inbox: &ClipboardInbox,
        stream: u64,
        phase: ClipboardPhase,
        now: Instant,
    ) -> Option<Result<Accepted, Osc52Refusal>> {
        match phase {
            ClipboardPhase::Data => {
                let chunk = inbox.pop(stream)?;
                let open = self.track(stream, now);
                if let Ok(decoder) = open.decoder.as_mut() {
                    if let Err(refusal) = decoder.feed(&chunk) {
                        // A refused chunk frees the decoder and its lease now.
                        open.decoder = Err(refusal);
                    }
                }
                None
            }
            ClipboardPhase::End { kinds, query } => {
                // When: ClipboardPhase::End arrives, the decoder becomes a job or a refusal.
                let decoder = self.track(stream, now);
                let decoder = std::mem::replace(&mut decoder.decoder, Err(Osc52Refusal::Invalid));
                self.open = None;
                if query {
                    // When: query is set, the payload was `?`, a read; it writes nothing.
                    return Some(self.answer_read(kinds.as_bytes(), now));
                }
                let job = self.finish(decoder, kinds.as_bytes());
                Some(job.and_then(|job| self.route_write(job, now)))
            }
            ClipboardPhase::Abort => {
                self.open = None;
                None
            }
        }
    }

    /// Turn a terminated stream into a job, or the reason it is refused.
    fn finish(
        &self,
        decoder: Result<Base64Stream, Osc52Refusal>,
        kinds: &[u8],
    ) -> Result<ClipboardJob, Osc52Refusal> {
        let targets = clip_targets(kinds, cfg!(target_os = "linux"))?;
        let (bytes, lease) = decoder?.finish()?;
        let (text, lease) = into_text(bytes, lease, targets)?;
        // The slot is taken here, in the reader thread, before the write is posted.
        let slot = self.staging.take_program_slot().ok_or(Osc52Refusal::Busy)?;
        let binding = WriteBinding::unbound(&self.staging);
        Ok(ClipboardJob { text, targets, lease, _slot: slot, kitty: None, offer: None, binding })
    }

    /// Match the parser's open stream after a section: start tracking a stream that opened with
    /// no chunk yet, and release one the parser ended without an event (a reset).
    pub(in crate::app) fn reconcile(&mut self, parser_open: Option<u64>, now: Instant) {
        match parser_open {
            Some(stream) => {
                self.track(stream, now);
            }
            None => self.open = None,
        }
    }

    /// Release the open stream, if any: the parser ended it without an event.
    pub(in crate::app) fn release_open(&mut self) {
        self.open = None;
    }

    /// When the open stream expires.
    pub(in crate::app) fn deadline(&self) -> Option<Instant> {
        self.open.as_ref().map(|open| open.opened_at + OSC52_STREAM_EXPIRY)
    }

    /// Release the open stream once it is [`OSC52_STREAM_EXPIRY`] old, or once `osc52` or
    /// `osc52_read` changed since it opened (even back to the same policy); returns its id so the
    /// caller aborts it in the parser too.
    pub(in crate::app) fn expire(&mut self, now: Instant) -> Option<u64> {
        let generation = self.staging.policy_generation();
        let stale = self.open.as_ref().is_some_and(|open| {
            now >= open.opened_at + OSC52_STREAM_EXPIRY || open.policy_generation != generation
        });
        if !stale {
            // When: stale is false, no stream is open, or it is young and under its policy.
            return None;
        }
        self.open.take().map(|open| open.stream)
    }
}

#[path = "osc52_stream_retained.rs"]
mod retained;

#[path = "osc52_stream_debug.rs"]
mod debug;

#[path = "osc52_stream_text.rs"]
mod text;
pub(in crate::app) use retained::{RetainedCopies, RetainedShares};
use text::into_text;
pub(in crate::app) use text::{native_clipboard_bytes, reassert_eligible};

/// The App's clipboard-write state: the shared staging, its governor charge, retained copies.
#[derive(Debug)]
pub(in crate::app) struct ClipState {
    /// The staging every pane's intake reserves from.
    pub(in crate::app) staging: Arc<ClipboardStaging>,
    /// The `ClipboardStaging` charge on the process owner.
    pub(in crate::app) charges: std::collections::HashMap<
        sonicterm_types::ResourceClass,
        sonicterm_resource::CommittedReservation,
    >,
    /// Leases for arboard's retained copies (Linux).
    pub(in crate::app) retained: RetainedCopies,
    /// The ordering domain program writes and user copies share, and its worker.
    pub(in crate::app) domain: super::clipboard_worker::ClipboardDomain,
    /// OSC 52 reads: waiting and queued tickets, the native reader and reply delivery.
    pub(in crate::app) reads: super::osc52_read_pipeline::ClipboardReads,
    /// Per pane, a batch summary held until its program writes on the worker settle.
    pub(in crate::app) deferred_attempts:
        std::collections::HashMap<u64, super::spawn_pane::ClipAttempts>,
}

impl ClipState {
    /// State over `staging`, with nothing charged, retained, posted or held.
    pub(in crate::app) fn new(staging: Arc<ClipboardStaging>) -> Self {
        Self {
            staging,
            charges: std::collections::HashMap::new(),
            retained: Default::default(),
            domain: Default::default(),
            reads: Default::default(),
            deferred_attempts: std::collections::HashMap::new(),
        }
    }
}

impl super::App {
    /// Charge the clipboard staging every live lease holds, and the posted program writes, to the
    /// process owner's `ClipboardStaging` class. Runs with the retention sample.
    pub(in crate::app) fn charge_clipboard_staging(&mut self) {
        // In-flight reads reserve their text and replies from the reads' own budget.
        let amount = sonicterm_types::ResourceAmount {
            bytes: self.clip_state.staging.reserved_bytes()
                + self.clip_state.reads.budget.reserved_bytes(),
            items: self.clip_state.staging.program_slots_in_use(),
        };
        let root = self.governor.root_owner();
        super::retention::charge_classes(
            &self.governor,
            root,
            &mut self.clip_state.charges,
            [(sonicterm_types::ResourceClass::ClipboardStaging, amount)],
        );
    }
}

#[cfg(test)]
#[path = "osc52_stream_tests.rs"]
mod osc52_stream_tests;
