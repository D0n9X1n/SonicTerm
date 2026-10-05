//! The row-run shaping diagnostic: how often a counted row-run shape call repeats one made shortly
//! before, so a later cache can be judged on measured repetition.
//!
//! A *pass* is one row-emitter assembly; it ends committed (its frame presented) or uncommitted
//! (superseded, failed, retried or unwound). A key is the run's text hash, length and style. Each
//! counted call is classified when it is made, against the slots of the key's probe range:
//! - *same-pass repeat*: the key was already seen in this pass;
//! - *window repeat*: a committed sighting among the last `WINDOW` committed passes;
//! - *retry repeat*: a sighting from an uncommitted pass within that window;
//! - *first*: anything else, including a key whose sightings have all expired.
//!
//! Classes count only when the pass commits; an uncommitted pass's calls count as unpresented
//! work. A failed or unstable call (its face changed during the call) never touches the table.
//! Storage is two fixed vectors allocated on the first counted call and never grown.

use std::hash::{Hash, Hasher};

/// Slots in the table; a power of two, so a hash masks to its first probe.
pub(crate) const SLOT_COUNT: usize = 4096;
/// Pending records one pass may hold before further calls go unrecorded.
pub(crate) const PENDING_CAPACITY: usize = 8192;
/// Slots a key may occupy, from its hash's slot onward.
const PROBES: usize = 8;
/// Committed passes a sighting stays in the window for.
pub(crate) const WINDOW: u64 = 8;
/// Slot flag: the slot holds a key.
const OCCUPIED: u8 = 1;
/// Slot flag: the key's latest sighting is from an uncommitted pass, at `retry_at`.
const RETRY: u8 = 2;
/// A pass number or committed count that is not set.
const ABSENT: u64 = u64::MAX;
/// Pending-record class bit: this pass assigned the slot to its key.
const NEW_ASSIGNMENT: u8 = 0x80;

/// One key's table entry.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Slot {
    hash: u64,
    /// The committed count of the pass that last committed a sighting, or [`ABSENT`].
    committed_at: u64,
    /// The committed count when an uncommitted pass last saw the key, or [`ABSENT`].
    retry_at: u64,
    /// The pass that has the key pending, or [`ABSENT`].
    pending_pass: u64,
    /// The style epoch the key was recorded under; a later epoch makes the slot dead.
    style_epoch: u32,
    /// Incremented on every assignment, so a pending record of an earlier key is ignored.
    generation: u32,
    /// The key's text length in bytes.
    len: u32,
    /// The key's style index, `bold | italic << 1`.
    style: u8,
    flags: u8,
}

/// An unassigned slot.
const EMPTY_SLOT: Slot = Slot {
    hash: 0,
    committed_at: ABSENT,
    retry_at: ABSENT,
    pending_pass: ABSENT,
    style_epoch: 0,
    generation: 0,
    len: 0,
    style: 0,
    flags: 0,
};

/// One recorded call of the open pass, settled when the pass ends.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingRecord {
    /// The call's shaping time in nanoseconds.
    ns: u64,
    slot: u32,
    /// The slot's generation when recorded; a mismatch at settlement means the slot moved on.
    generation: u32,
    /// The slot's style epoch when recorded.
    style_epoch: u32,
    /// The call's [`RunClass`] code, with [`NEW_ASSIGNMENT`] when it assigned the slot.
    class: u8,
}

/// The face a style currently shapes with, and that style's epoch; epoch 0 is never adopted.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StyleIdentity {
    face: u64,
    handles: u32,
    epoch: u32,
}

/// A face identity as the table compares it: the face id and its handle count. An unresolvable
/// face reads as [`UNRESOLVED`].
pub(crate) type FaceKey = (u64, u32);
/// The identity of a face that could not be resolved.
pub(crate) const UNRESOLVED: FaceKey = (u64::MAX, u32::MAX);

/// A counted call's key: the 64-bit hash of its text and style, the text length and the style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowRunKey {
    pub(crate) hash: u64,
    pub(crate) len: u32,
    pub(crate) style: u8,
}

/// The style index of `(bold, italic)`.
pub(crate) fn style_index(bold: bool, italic: bool) -> u8 {
    u8::from(bold) | (u8::from(italic) << 1)
}

/// The key of a call shaping `text` in `(bold, italic)`: a fixed-key SipHash-1-3 of the text bytes,
/// bold and italic, so equal calls hash equally in every process.
pub(crate) fn row_run_key(text: &str, bold: bool, italic: bool) -> RowRunKey {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.as_bytes().hash(&mut hasher);
    bold.hash(&mut hasher);
    italic.hash(&mut hasher);
    RowRunKey {
        hash: hasher.finish(),
        len: u32::try_from(text.len()).unwrap_or(u32::MAX),
        style: style_index(bold, italic),
    }
}

/// How a call was classified when it was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunClass {
    First,
    SamePass,
    Window,
    Retry,
}

impl RunClass {
    const ALL: [RunClass; 4] = [Self::First, Self::SamePass, Self::Window, Self::Retry];

    fn code(self) -> u8 {
        match self {
            Self::First => 0,
            Self::SamePass => 1,
            Self::Window => 2,
            Self::Retry => 3,
        }
    }

    fn from_code(code: u8) -> Self {
        Self::ALL[usize::from(code & !NEW_ASSIGNMENT) % 4]
    }
}

/// The diagnostic's sixteen counters, recorded into the renderer's frame statistics. Calls, outcomes,
/// resets, overflows and diagnostic time count when they happen; classes and shaping time count when
/// their pass commits, and an uncommitted pass's classified calls count as unpresented instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowRunCounts {
    /// Every counted row-run shape call.
    pub calls: u64,
    /// Calls whose shaping succeeded, unstable ones included.
    pub ok: u64,
    /// Calls whose shaping failed.
    pub failed: u64,
    /// Shaping nanoseconds of committed classified calls.
    pub shape_ns: u64,
    /// Committed calls classified first.
    pub first: u64,
    /// Committed window and same-pass repeats.
    pub repeats: u64,
    /// Committed same-pass repeats, a subset of `repeats`.
    pub same_pass_repeats: u64,
    /// Shaping nanoseconds of `repeats`.
    pub repeat_ns: u64,
    /// Successful calls whose face changed during the call.
    pub unstable: u64,
    /// Committed repeats of a call last seen in an uncommitted pass.
    pub retry_repeats: u64,
    /// Classified calls of uncommitted passes.
    pub unpresented_calls: u64,
    /// Their shaping nanoseconds.
    pub unpresented_ns: u64,
    /// Times a style's face changed and its entries were cleared.
    pub identity_resets: u64,
    /// Calls that found no slot to record in.
    pub overflows: u64,
    /// Calls that found the pass's pending list full.
    pub pass_overflows: u64,
    /// Nanoseconds spent hashing, probing and settling passes.
    pub diag_ns: u64,
}

impl RowRunCounts {
    /// All zero.
    pub const ZERO: Self = Self {
        calls: 0,
        ok: 0,
        failed: 0,
        shape_ns: 0,
        first: 0,
        repeats: 0,
        same_pass_repeats: 0,
        repeat_ns: 0,
        unstable: 0,
        retry_repeats: 0,
        unpresented_calls: 0,
        unpresented_ns: 0,
        identity_resets: 0,
        overflows: 0,
        pass_overflows: 0,
        diag_ns: 0,
    };

    /// Add `other`'s counts to these.
    pub fn add(&mut self, other: &Self) {
        self.calls += other.calls;
        self.ok += other.ok;
        self.failed += other.failed;
        self.shape_ns += other.shape_ns;
        self.first += other.first;
        self.repeats += other.repeats;
        self.same_pass_repeats += other.same_pass_repeats;
        self.repeat_ns += other.repeat_ns;
        self.unstable += other.unstable;
        self.retry_repeats += other.retry_repeats;
        self.unpresented_calls += other.unpresented_calls;
        self.unpresented_ns += other.unpresented_ns;
        self.identity_resets += other.identity_resets;
        self.overflows += other.overflows;
        self.pass_overflows += other.pass_overflows;
        self.diag_ns += other.diag_ns;
    }
}

/// The diagnostic's table and pass state.
#[derive(Debug)]
pub(crate) struct RowRunTable {
    slots: Vec<Slot>,
    pending: Vec<PendingRecord>,
    identities: [StyleIdentity; 4],
    /// Committed passes so far.
    committed: u64,
    /// The latest pass's number.
    pass: u64,
    open: bool,
    /// Classified calls of the open pass that hold no pending record, by class: calls and ns.
    unrecorded: [(u64, u64); 4],
    counts: RowRunCounts,
}

impl RowRunTable {
    /// An empty table; nothing is allocated until the first counted call.
    pub(crate) fn new() -> Self {
        Self {
            slots: Vec::new(),
            pending: Vec::new(),
            identities: [StyleIdentity { face: UNRESOLVED.0, handles: UNRESOLVED.1, epoch: 0 }; 4],
            committed: 0,
            pass: 0,
            open: false,
            unrecorded: [(0, 0); 4],
            counts: RowRunCounts::default(),
        }
    }

    /// Open a pass. A pass still open is first settled as uncommitted: it was superseded.
    pub(crate) fn begin_pass(&mut self) {
        if self.open {
            // The previous pass never reached its settlement, so it did not present.
            self.end_pass(false);
        }
        self.pass += 1;
        self.open = true;
    }

    /// Whether a pass is open.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Settle the open pass, committed when `presented`; nothing happens without an open pass.
    pub(crate) fn end_pass(&mut self, presented: bool) {
        if !self.open {
            // When: no pass is open, there is nothing to settle.
            return;
        }
        self.open = false;
        if presented {
            // A committed pass computes its committed count once and writes it to every slot.
            self.committed += 1;
        }
        let (pass, committed) = (self.pass, self.committed);
        for index in 0..self.pending.len() {
            let record = self.pending[index];
            self.count_settled(RunClass::from_code(record.class), 1, record.ns, presented);
            let current_epoch =
                self.identities[usize::from(self.slots[record.slot as usize].style)].epoch;
            let slot = &mut self.slots[record.slot as usize];
            // Only the slot's first record this pass settles it; the marker is cleared either way.
            let first_record = slot.pending_pass == pass;
            if first_record {
                slot.pending_pass = ABSENT;
            }
            let live = slot.flags & OCCUPIED != 0
                && slot.generation == record.generation
                && slot.style_epoch == record.style_epoch
                && slot.style_epoch == current_epoch;
            if !(first_record && live) {
                // When: not `first_record`, or the slot is no longer `live` (reassigned or cleared),
                // the record has no transition to apply.
                continue;
            }
            if presented {
                slot.committed_at = committed;
                slot.flags &= !RETRY;
            } else {
                // When: not `presented`, the pass did not reach the screen: a retry sighting.
                settle_uncommitted(slot, record.class & NEW_ASSIGNMENT != 0, committed);
            }
        }
        self.pending.clear();
        for class in RunClass::ALL {
            let (calls, ns) = std::mem::take(&mut self.unrecorded[usize::from(class.code())]);
            self.count_settled(class, calls, ns, presented);
        }
    }

    /// Count `calls` settled calls of `class` taking `ns`.
    fn count_settled(&mut self, class: RunClass, calls: u64, ns: u64, presented: bool) {
        let counts = &mut self.counts;
        if !presented {
            // When: not `presented`, the calls count as unpresented and no class is credited.
            counts.unpresented_calls += calls;
            counts.unpresented_ns += ns;
            return;
        }
        counts.shape_ns += ns;
        match class {
            RunClass::First => counts.first += calls,
            RunClass::SamePass => {
                counts.repeats += calls;
                counts.same_pass_repeats += calls;
                counts.repeat_ns += ns;
            }
            RunClass::Window => {
                counts.repeats += calls;
                counts.repeat_ns += ns;
            }
            RunClass::Retry => counts.retry_repeats += calls,
        }
    }

    /// Count one call of the open pass with key `key`: the face identity read `before` and
    /// `after` it, whether it succeeded and its shaping time. A failed or unstable call touches
    /// nothing but its counters; any identity change first clears the style. Returns the class
    /// of a classified call.
    pub(crate) fn observe_call(
        &mut self,
        key: RowRunKey,
        before: FaceKey,
        after: FaceKey,
        succeeded: bool,
        ns: u64,
    ) -> Option<RunClass> {
        self.counts.calls += 1;
        let stable = self.observe_identity(key.style, before, after);
        if !succeeded {
            // When: not `succeeded`, a failed shape is counted but never enters the table.
            self.counts.failed += 1;
            return None;
        }
        self.counts.ok += 1;
        if !stable {
            // When: not `stable`, the face moved during the call, so its key cannot be trusted.
            self.counts.unstable += 1;
            return None;
        }
        Some(self.record(key, ns))
    }

    /// Track style `style`'s face: any transition (before differs from the current identity, or
    /// from after) clears the style and adopts `after`. Returns whether the call was stable.
    fn observe_identity(&mut self, style: u8, before: FaceKey, after: FaceKey) -> bool {
        let index = usize::from(style);
        let identity = self.identities[index];
        if identity.epoch == 0 {
            // The style was never adopted: its first identity is adopted without a reset.
            self.identities[index] = StyleIdentity { face: after.0, handles: after.1, epoch: 1 };
        } else if before != (identity.face, identity.handles) || before != after {
            // When: `before` differs from the adopted `identity` or from `after`, the style's
            // entries describe another face and are cleared by a new epoch.
            let next = if identity.epoch == u32::MAX {
                // The epoch would wrap: the whole table is cleared and every epoch restarts.
                self.clear_table();
                1
            } else {
                // When: the `identity` `epoch` is below its maximum, the next epoch is free.
                identity.epoch + 1
            };
            self.identities[index] = StyleIdentity { face: after.0, handles: after.1, epoch: next };
            self.counts.identity_resets += 1;
        }
        before == after
    }

    /// Classify and record one stable successful call of the open pass.
    fn record(&mut self, key: RowRunKey, ns: u64) -> RunClass {
        self.allocate();
        let start = (key.hash as usize) & (SLOT_COUNT - 1);
        let (mut found, mut dead, mut expired) = (None, None, None);
        // The whole probe range is scanned: a dead slot before the key never hides it.
        for probe in 0..PROBES {
            let index = (start + probe) & (SLOT_COUNT - 1);
            let slot = self.slots[index];
            if !self.live(&slot) {
                // When: the slot is not `live`, it is a candidate; the scan goes on past the hole.
                dead.get_or_insert(index);
                continue;
            }
            if found.is_none()
                && slot.hash == key.hash
                && slot.len == key.len
                && slot.style == key.style
            {
                found = Some(index);
            } else if slot.pending_pass != self.pass && !self.sighted(&slot) {
                // When: another key, not pending in this `pass` and not `sighted`, may be replaced.
                expired.get_or_insert(index);
            }
        }
        let class = found.map_or(RunClass::First, |index| self.classify(&self.slots[index]));
        if self.pending.len() == PENDING_CAPACITY {
            // When: the pending list is full, the call is classified and timed but marks nothing.
            self.counts.pass_overflows += 1;
            self.add_unrecorded(class, ns);
            return class;
        }
        let (index, assigned) =
            match found.map(|index| (index, false)).or(dead.or(expired).map(|index| (index, true)))
            {
                Some(target) => target,
                None => {
                    // When: no key was `found` and no `dead` or `expired` slot exists, every
                    // candidate is live and recently sighted, so nothing is displaced.
                    self.counts.overflows += 1;
                    self.add_unrecorded(RunClass::First, ns);
                    return RunClass::First;
                }
            };
        if assigned {
            self.assign(index, key);
        }
        let slot = &mut self.slots[index];
        slot.pending_pass = self.pass;
        let new_flag = if assigned { NEW_ASSIGNMENT } else { 0 };
        self.pending.push(PendingRecord {
            ns,
            slot: index as u32,
            generation: slot.generation,
            style_epoch: slot.style_epoch,
            class: class.code() | new_flag,
        });
        class
    }

    /// Put `key` into slot `index`, a new generation of it; a wrapping generation clears the table.
    fn assign(&mut self, index: usize, key: RowRunKey) {
        if self.slots[index].generation == u32::MAX {
            // The generation would wrap: the table is cleared so no stale record can match.
            self.clear_table();
        }
        let epoch = self.identities[usize::from(key.style)].epoch;
        let slot = &mut self.slots[index];
        *slot = Slot {
            hash: key.hash,
            len: key.len,
            style: key.style,
            style_epoch: epoch,
            generation: slot.generation + 1,
            flags: OCCUPIED,
            ..EMPTY_SLOT
        };
    }

    /// Empty every slot and the pending list, restarting each adopted style's epoch at 1. The
    /// open pass's records keep their class and time as unrecorded calls.
    fn clear_table(&mut self) {
        for index in 0..self.pending.len() {
            let record = self.pending[index];
            self.add_unrecorded(RunClass::from_code(record.class), record.ns);
        }
        self.pending.clear();
        self.slots.fill(EMPTY_SLOT);
        for identity in &mut self.identities {
            identity.epoch = identity.epoch.min(1);
        }
    }

    fn add_unrecorded(&mut self, class: RunClass, ns: u64) {
        let entry = &mut self.unrecorded[usize::from(class.code())];
        entry.0 += 1;
        entry.1 += ns;
    }

    /// Whether `slot` holds a key of its style's current epoch.
    fn live(&self, slot: &Slot) -> bool {
        slot.flags & OCCUPIED != 0
            && slot.style_epoch == self.identities[usize::from(slot.style)].epoch
    }

    /// Whether `slot` has a sighting still in the window.
    fn sighted(&self, slot: &Slot) -> bool {
        self.in_window(slot.committed_at)
            || (slot.flags & RETRY != 0 && self.in_window(slot.retry_at))
    }

    /// Whether a sighting at committed count `at` is among the last [`WINDOW`] committed passes.
    fn in_window(&self, at: u64) -> bool {
        at != ABSENT && self.committed - at < WINDOW
    }

    /// The class of a call whose key is live in `slot`.
    fn classify(&self, slot: &Slot) -> RunClass {
        if slot.pending_pass == self.pass {
            RunClass::SamePass
        } else if self.in_window(slot.committed_at) {
            // When: `committed_at` is `in_window`, an earlier committed pass shaped the key.
            RunClass::Window
        } else if slot.flags & RETRY != 0 && self.in_window(slot.retry_at) {
            // When: `RETRY` is set and `retry_at` is `in_window`, only an uncommitted pass shaped it.
            RunClass::Retry
        } else {
            // When: neither sighting is `in_window`, the live key is as good as new.
            RunClass::First
        }
    }

    /// Allocate both vectors at their fixed capacities, once.
    fn allocate(&mut self) {
        if self.slots.capacity() == 0 {
            // The first counted call allocates the table; it is never grown after.
            self.slots = Vec::with_capacity(SLOT_COUNT);
            self.slots.resize(SLOT_COUNT, EMPTY_SLOT);
            self.pending = Vec::with_capacity(PENDING_CAPACITY);
        }
    }

    /// Add `ns` of diagnostic work: hashing, probing and settlement.
    fn add_diag_ns(&mut self, ns: u64) {
        self.counts.diag_ns += ns;
    }

    /// The counters recorded since the last take.
    pub(crate) fn take_counts(&mut self) -> RowRunCounts {
        std::mem::take(&mut self.counts)
    }

    /// Heap bytes the two vectors hold: 0 before the first counted call, 393,216 after.
    pub(crate) fn retained_heap_bytes(&self) -> usize {
        self.slots.capacity() * std::mem::size_of::<Slot>()
            + self.pending.capacity() * std::mem::size_of::<PendingRecord>()
    }
}

/// An uncommitted pass's transition for its first live record of `slot`, at committed count
/// `committed`: a newly assigned key becomes a retry sighting with no committed one; a retry
/// sighting is refreshed; an in-window committed sighting is kept; an expired one adds a retry.
fn settle_uncommitted(slot: &mut Slot, assigned: bool, committed: u64) {
    let committed_in_window = slot.committed_at != ABSENT && committed - slot.committed_at < WINDOW;
    if assigned {
        slot.flags |= RETRY;
        slot.retry_at = committed;
        slot.committed_at = ABSENT;
    } else if slot.flags & RETRY != 0 {
        // When: `RETRY` is already set, the retry sighting is refreshed.
        slot.retry_at = committed;
    } else if !committed_in_window {
        // When: not `committed_in_window`, the expired key gains a retry sighting.
        slot.flags |= RETRY;
        slot.retry_at = committed;
    }
}

/// The bytes one renderer's diagnostic can retain: both vectors at their fixed capacities plus
/// its inline state. `ResourceClass::RowRunDiagnostics` records this figure; a test pins the two equal.
#[cfg(test)]
pub(crate) const ROW_RUN_DIAG_ENVELOPE_BYTES: usize = SLOT_COUNT * std::mem::size_of::<Slot>()
    + PENDING_CAPACITY * std::mem::size_of::<PendingRecord>()
    + std::mem::size_of::<RowRunDiagnostics>();

/// Run one render attempt's `body`, catching an unwind so the caller can settle the attempt's
/// row-run pass with [`RowRunDiagnostics::settle_attempt`] before its scopes close.
pub(crate) fn catch_attempt<Output>(body: impl FnOnce() -> Output) -> std::thread::Result<Output> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body))
}

/// Nanoseconds since the first reading in this process.
fn monotonic_ns() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    u64::try_from(START.get_or_init(std::time::Instant::now).elapsed().as_nanos())
        .unwrap_or(u64::MAX)
}

/// A renderer's row-run diagnostic: the table, opened per pass only while the frame-counter gate
/// is on, and the clock its timings read. With the gate off nothing is hashed, read or allocated.
#[derive(Debug)]
pub(crate) struct RowRunDiagnostics {
    table: RowRunTable,
    clock: fn() -> u64,
}

impl RowRunDiagnostics {
    /// A diagnostic reading the monotonic clock.
    pub(crate) fn new() -> Self {
        Self::with_clock(monotonic_ns)
    }

    /// A diagnostic reading `clock`, nanoseconds from any fixed origin.
    pub(crate) fn with_clock(clock: fn() -> u64) -> Self {
        Self { table: RowRunTable::new(), clock }
    }

    /// Start a row-emitter pass. A pass left open (superseded, failed or unwound) is first settled
    /// as not presented; the new pass is counted only when `counting` (the gate is on).
    pub(crate) fn begin_pass(&mut self, counting: bool) {
        self.settle(false);
        if counting {
            // The frame-counter gate is on, so this pass's calls are counted.
            self.table.begin_pass();
        }
    }

    /// Start a row-emitter pass counted only while this thread's frame-counter gate is on, so
    /// production with counters off never hashes, reads a face or allocates the table.
    pub(crate) fn begin_gated_pass(&mut self) {
        self.begin_pass(crate::frame_stats::collecting());
    }

    /// Settle the open pass, committed when `presented`; nothing happens without one.
    pub(crate) fn end_pass(&mut self, presented: bool) {
        self.settle(presented);
    }

    /// Close one render attempt caught by [`catch_attempt`]: a pass it left open (an early return,
    /// an error or an unwind) settles as not presented and every count is recorded, inside the
    /// attempt's scopes; then the attempt's output returns or its unwind resumes unchanged. A pass
    /// the attempt already settled (a presented frame) is not settled again.
    pub(crate) fn settle_attempt<Output>(
        &mut self,
        attempt: std::thread::Result<Output>,
    ) -> Output {
        self.end_pass(false);
        crate::frame_stats::note_row_runs(&self.take_counts());
        match attempt {
            Ok(output) => output,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn settle(&mut self, presented: bool) {
        if !self.table.is_open() {
            // When: the table `is_open` is false, no counted pass exists to settle or time.
            return;
        }
        let started = (self.clock)();
        self.table.end_pass(presented);
        let settled = (self.clock)();
        self.table.add_diag_ns(settled.saturating_sub(started));
    }

    /// Run `shape`, shaping `text` in `(bold, italic)`, and count it when a counted pass is open:
    /// `identity` is read before and after the call, and its time is attributed to its class.
    pub(crate) fn shape<Output, Error>(
        &mut self,
        text: &str,
        bold: bool,
        italic: bool,
        identity: impl Fn() -> FaceKey,
        shape: impl FnOnce() -> Result<Output, Error>,
    ) -> Result<Output, Error> {
        if !self.table.is_open() {
            // When: the table `is_open` is false (the gate is off), the call is not observed.
            return shape();
        }
        let started = (self.clock)();
        let key = row_run_key(text, bold, italic);
        let before = identity();
        let shaping = (self.clock)();
        let shaped = shape();
        let shaped_at = (self.clock)();
        let after = identity();
        self.table.observe_call(
            key,
            before,
            after,
            shaped.is_ok(),
            shaped_at.saturating_sub(shaping),
        );
        let finished = (self.clock)();
        self.table
            .add_diag_ns(shaping.saturating_sub(started) + finished.saturating_sub(shaped_at));
        shaped
    }

    /// The counters recorded since the last take.
    pub(crate) fn take_counts(&mut self) -> RowRunCounts {
        self.table.take_counts()
    }

    /// Bytes this diagnostic retains: its inline state plus the table's heap, which is 0 until
    /// the first counted call and 393,216 after.
    pub(crate) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.table.retained_heap_bytes()
    }

    /// Slots allocated: 0 until the first counted call, then [`SLOT_COUNT`].
    pub(crate) fn retained_items(&self) -> usize {
        self.table.slots.capacity()
    }

    /// Test hook: the table.
    #[cfg(test)]
    pub(crate) fn table(&self) -> &RowRunTable {
        &self.table
    }
}

#[cfg(test)]
impl RowRunTable {
    /// Test hook: slots whose pending marker is set.
    pub(crate) fn pending_markers(&self) -> usize {
        self.slots.iter().filter(|slot| slot.pending_pass != ABSENT).count()
    }

    /// Test hook: the slot vector's capacity.
    pub(crate) fn slot_capacity(&self) -> usize {
        self.slots.capacity()
    }

    /// Test hook: set slot `index`'s generation.
    pub(crate) fn set_generation(&mut self, index: usize, generation: u32) {
        self.allocate();
        self.slots[index].generation = generation;
    }

    /// Test hook: set style `style`'s epoch.
    pub(crate) fn set_epoch(&mut self, style: u8, epoch: u32) {
        self.identities[usize::from(style)].epoch = epoch;
    }
}

#[cfg(test)]
#[path = "row_run_diag_tests.rs"]
mod row_run_diag_tests;
