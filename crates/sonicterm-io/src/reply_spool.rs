//! Bounded reply FIFO: a fixed resident tier, then private temporary spill storage with a disk budget.
//!
//! Complete submissions remain indivisible across UI writer turns. Live spilled bytes are capped at
//! [`SPILL_BUDGET_BYTES`]; a submission past the cap is refused alone and counted, and the spool
//! stays usable. Consumed disk prefixes are compacted away once they dominate the file, so the file
//! never grows past twice the budget plus one record.

use std::{
    collections::VecDeque,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

const RAM_BYTES: usize = 64 * 1024;
const CHUNK_BYTES: usize = 32 * 1024;
const HEADER_BYTES: usize = 4;
/// Most unconsumed spilled bytes (records and their headers) one pane's replies may hold on disk.
/// A program that issues queries forever and never reads its input reaches it; finite bursts,
/// such as 512 full-size replies written before the program reads, stay below it.
pub const SPILL_BUDGET_BYTES: u64 = 16 * 1024 * 1024;
/// Smallest consumed prefix worth compacting; below it the file keeps its prefix until it drains.
const COMPACT_MIN_BYTES: u64 = 1024 * 1024;
/// Bytes copied per step while compacting, so no allocation grows with the backlog.
const COMPACT_CHUNK_BYTES: usize = 64 * 1024;

/// Cloneable ordered reply sender with 64 KiB resident storage and private disk spill.
///
/// Submit complete protocol replies outside parser/grid locks. Each submission
/// is at most 32 KiB and remains indivisible when interleaved with UI input.
/// Admission may perform synchronous file I/O, but never waits for native PTY
/// capacity: one record write plus, amortised, one copy of each live byte per compaction.
/// Live spilled bytes are capped at [`SPILL_BUDGET_BYTES`]; a submission past the cap fails with
/// a [`ReplyBudgetExceeded`] error ([`is_budget_refusal`]), is dropped alone, and leaves the spool
/// usable.
/// Writer reads use at most 32 KiB plus a header of scratch and a 32 KiB output.
#[derive(Clone, Debug)]
pub struct PtyReplySender {
    state: Arc<Mutex<SpoolState>>,
    closed: Arc<AtomicBool>,
    wake: Sender<()>,
    stats: Arc<SpoolCounters>,
}

/// Backlog counters shared by every clone of one pane's sender.
#[derive(Debug, Default)]
struct SpoolCounters {
    /// Submissions refused because the spill budget was full.
    dropped_replies: AtomicU64,
    /// Largest number of live spilled bytes seen.
    peak_live_bytes: AtomicU64,
    /// Live spilled bytes after the last operation; published under the spool lock and read without it.
    live_bytes: AtomicU64,
    /// Bytes the resident tier holds allocated after the last operation (its capacity, which
    /// outlives a drain), published like `live_bytes`.
    resident_bytes: AtomicU64,
}

impl SpoolCounters {
    /// Publish `state`'s live spill and resident allocation, so `stats` never takes the spool lock.
    // Ordering: live_bytes and resident_bytes Relaxed; sampled gauges that publish no other data.
    fn publish(&self, state: &SpoolState) {
        self.live_bytes.store(state.live_spill_bytes(), Ordering::Relaxed);
        self.resident_bytes.store(state.ram.capacity() as u64, Ordering::Relaxed);
    }
}

/// The error a reply carries when the spill budget refused it. A native filesystem quota failure
/// also has `ErrorKind::QuotaExceeded` but latches the spool; only this type means "dropped alone".
#[derive(Debug)]
pub struct ReplyBudgetExceeded;

impl std::fmt::Display for ReplyBudgetExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PTY reply backlog exceeds its disk budget")
    }
}

impl std::error::Error for ReplyBudgetExceeded {}

/// Whether `error` is the spill budget's refusal of one reply, not a storage failure.
pub fn is_budget_refusal(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|inner| inner.is::<ReplyBudgetExceeded>())
}

/// A snapshot of one pane's reply backlog counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplySpoolStats {
    /// Submissions refused because the spill budget was full.
    pub dropped_replies: u64,
    /// Largest number of live spilled bytes (records and headers) seen.
    pub peak_live_bytes: u64,
    /// Live spilled bytes now, on disk.
    pub live_bytes: u64,
    /// Bytes the resident tier holds allocated now, in memory: its capacity, which a drain does not
    /// return, so it can exceed the records it holds.
    pub resident_bytes: u64,
}

#[derive(Debug)]
struct SpoolState {
    ram: VecDeque<u8>,
    spill: Option<Spill>,
    closed: Arc<AtomicBool>,
    failure: Option<Arc<io::Error>>,
}

#[derive(Debug)]
struct Spill {
    file: File,
    read: u64,
    written: u64,
}

pub(crate) struct ReplyReader {
    sender: PtyReplySender,
    pub(crate) wake: Receiver<()>,
}

impl PtyReplySender {
    /// Admit one complete submission; oversize, storage failure, and closed-writer errors remain explicit.
    // Ordering: closed SeqCst pairs with close; dropped_replies and peak_live_bytes are Relaxed
    // diagnostic counters that publish no other data.
    pub fn send(&self, bytes: Vec<u8>) -> io::Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            // When: closed is published, reject without waiting on an in-flight storage operation.
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let mut state = self.state.lock();
        let result = state.append(&bytes);
        let result = state.finish_operation(result);
        if result.as_ref().is_err_and(is_budget_refusal) {
            self.stats.dropped_replies.fetch_add(1, Ordering::Relaxed);
        }
        self.stats.peak_live_bytes.fetch_max(state.live_spill_bytes(), Ordering::Relaxed);
        self.stats.publish(&state);
        // One pending hint is sufficient; notifications must not backpressure admission.
        let _ = self.wake.try_send(());
        result
    }

    /// The backlog counters as of the last send or pop. Read without the spool lock, so a sampler
    /// on the event loop never waits on a send or pop blocked in file I/O.
    // Ordering: dropped_replies, peak_live_bytes, live_bytes and resident_bytes load Relaxed; gauges
    // only, each published Relaxed under the spool lock by SpoolCounters::publish.
    pub fn stats(&self) -> ReplySpoolStats {
        ReplySpoolStats {
            dropped_replies: self.stats.dropped_replies.load(Ordering::Relaxed),
            peak_live_bytes: self.stats.peak_live_bytes.load(Ordering::Relaxed),
            live_bytes: self.stats.live_bytes.load(Ordering::Relaxed),
            resident_bytes: self.stats.resident_bytes.load(Ordering::Relaxed),
        }
    }

    /// Cancel admission without locks or file I/O; the native writer owns final storage cleanup.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.wake.try_send(());
    }
}

impl Spill {
    /// Move the live bytes to the start of the file once the consumed prefix is at least
    /// [`COMPACT_MIN_BYTES`] and at least half the file. Each live byte is copied at most once per
    /// halving, so the cost is amortised constant per admitted byte, and the file stays below
    /// twice [`SPILL_BUDGET_BYTES`] plus one record. One compaction copies at most the live bytes,
    /// under the budget, while the spool lock is held, so the writer's next `pop` can wait for it.
    fn compact_if_due(&mut self) -> io::Result<()> {
        if self.read < COMPACT_MIN_BYTES || self.read * 2 < self.written {
            // When: `self.read` is under COMPACT_MIN_BYTES or under half of `self.written`, compaction is not due.
            return Ok(());
        }
        let live = self.written - self.read;
        let mut buffer = vec![0; COMPACT_CHUNK_BYTES];
        let mut moved = 0;
        while moved < live {
            let step = (live - moved).min(COMPACT_CHUNK_BYTES as u64) as usize;
            // Source and destination never overlap ahead of the copy: the destination trails.
            self.file.seek(SeekFrom::Start(self.read + moved))?;
            self.file.read_exact(&mut buffer[..step])?;
            self.file.seek(SeekFrom::Start(moved))?;
            self.file.write_all(&buffer[..step])?;
            moved += step as u64;
        }
        self.file.set_len(live)?;
        self.read = 0;
        self.written = live;
        Ok(())
    }
}

impl SpoolState {
    fn check_open(&self) -> io::Result<()> {
        if let Some(error) = &self.failure {
            // When: failure is latched, preserve its cause for subsequent admissions and reads.
            return Err(io::Error::new(error.kind(), error.clone()));
        }
        if self.closed.load(Ordering::SeqCst) {
            // When: closed is published, retained senders cannot resurrect storage.
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        Ok(())
    }

    /// Unconsumed spilled bytes, records and headers.
    fn live_spill_bytes(&self) -> u64 {
        self.spill.as_ref().map_or(0, |spill| spill.written - spill.read)
    }

    fn clear(&mut self) {
        self.ram = VecDeque::new();
        self.spill = None;
    }

    fn finish_operation<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        if self.closed.load(Ordering::SeqCst) {
            // When: closed is set during file I/O, release storage without blocking the thread that requested closure.
            self.clear();
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        result
    }

    fn fail(&mut self, error: io::Error) -> io::Error {
        self.failure.get_or_insert_with(|| Arc::new(error));
        self.clear();
        self.check_open().expect_err("reply spool failure was latched")
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.append_with(bytes, |file, bytes| file.write_all(bytes))
    }

    fn append_with(
        &mut self,
        bytes: &[u8],
        write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.check_open()?;
        if bytes.len() > CHUNK_BYTES {
            // When: bytes exceeds one writer turn, reject rather than split a protocol submission around UI input.
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PTY reply submission exceeds 32 KiB",
            ));
        }
        if bytes.is_empty() {
            // When: bytes is empty, no record or storage is needed.
            return Ok(());
        }
        let header = (bytes.len() as u32).to_le_bytes();
        let record_len = HEADER_BYTES + bytes.len();
        if self.spill.is_none() && record_len <= RAM_BYTES - self.ram.len() {
            // When: record_len fits before any spill, keep payload and framing in the fixed resident byte FIFO.
            if self.ram.capacity() == 0 {
                self.ram = VecDeque::with_capacity(RAM_BYTES);
            }
            self.ram.extend(header);
            self.ram.extend(bytes);
            return Ok(());
        }
        if self.live_spill_bytes() + record_len as u64 > SPILL_BUDGET_BYTES {
            // When: the record would take the live spill past SPILL_BUDGET_BYTES, refuse it alone;
            // nothing is written, so the spool stays usable and older records keep their order.
            return Err(io::Error::new(io::ErrorKind::QuotaExceeded, ReplyBudgetExceeded));
        }
        let result = (|| {
            if self.spill.is_none() {
                // Allocate a private anonymous file only after resident capacity is exhausted.
                self.spill = Some(Spill { file: tempfile::tempfile()?, read: 0, written: 0 });
            }
            let spill = self.spill.as_mut().expect("spill was initialized");
            spill.compact_if_due()?;
            let end = spill
                .written
                .checked_add(record_len as u64)
                .ok_or_else(|| io::Error::other("PTY reply spill offset overflow"))?;
            spill.file.seek(SeekFrom::Start(spill.written))?;
            spill.file.write_all(&header)?;
            write(&mut spill.file, bytes)?;
            // Only complete records become visible; partial writes latch failure and remove the file.
            spill.written = end;
            Ok(())
        })();
        result.map_err(|error| self.fail(error))
    }

    fn pop(&mut self) -> io::Result<Option<Vec<u8>>> {
        self.check_open()?;
        if !self.ram.is_empty() {
            // When: ram contains older complete records, preserve that prefix ahead of spilled records.
            let mut output = Vec::new();
            while !self.ram.is_empty() {
                let header = std::array::from_fn(|index| self.ram[index]);
                let len = u32::from_le_bytes(header) as usize;
                if output.len() + len > CHUNK_BYTES {
                    // When: output plus len exceeds CHUNK_BYTES, preserve the complete record for the next turn.
                    break;
                }
                self.ram.drain(..HEADER_BYTES);
                output.extend(self.ram.drain(..len));
            }
            return Ok(Some(output));
        }
        let Some(spill) = self.spill.as_mut() else {
            // When: spill is absent, both storage tiers are empty.
            return Ok(None);
        };
        // Read a bounded encoded window, then emit only whole records fitting one writer turn.
        let count = (spill.written - spill.read).min((CHUNK_BYTES + HEADER_BYTES) as u64) as usize;
        let mut encoded = vec![0; count];
        let result = spill
            .file
            .seek(SeekFrom::Start(spill.read))
            .and_then(|_| spill.file.read_exact(&mut encoded));
        if let Err(error) = result {
            // When: result contains a read error, no partial record can escape into the terminal stream.
            return Err(self.fail(error));
        }
        let mut output = Vec::new();
        let mut consumed = 0;
        while encoded.len() - consumed >= HEADER_BYTES {
            let header = encoded[consumed..consumed + HEADER_BYTES].try_into().unwrap();
            let len = u32::from_le_bytes(header) as usize;
            if len == 0 || len > CHUNK_BYTES {
                // When: len is zero or exceeds CHUNK_BYTES, refuse corrupt framing before allocating payload storage.
                return Err(self
                    .fail(io::Error::new(io::ErrorKind::InvalidData, "invalid PTY reply record")));
            }
            if consumed + HEADER_BYTES + len > encoded.len() || output.len() + len > CHUNK_BYTES {
                // When: consumed plus len exceeds encoded or output capacity, reread the whole record on the next turn.
                break;
            }
            output.extend_from_slice(
                &encoded[consumed + HEADER_BYTES..consumed + HEADER_BYTES + len],
            );
            consumed += HEADER_BYTES + len;
        }
        if consumed == 0 {
            // When: consumed is zero, the committed file is truncated or corrupt rather than temporarily empty.
            return Err(self.fail(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete PTY reply record",
            )));
        }
        spill.read += consumed as u64;
        if spill.read == spill.written {
            self.spill = None;
        }
        Ok(Some(output))
    }
}

/// Create the producer and sole reader with a bounded wake hint.
pub(crate) fn reply_spool() -> (PtyReplySender, ReplyReader) {
    let (wake, rx) = crossbeam_channel::bounded(1);
    let closed = Arc::new(AtomicBool::new(false));
    let sender = PtyReplySender {
        state: Arc::new(Mutex::new(SpoolState {
            ram: VecDeque::new(),
            spill: None,
            closed: closed.clone(),
            failure: None,
        })),
        closed,
        wake,
        stats: Arc::default(),
    };
    (sender.clone(), ReplyReader { sender, wake: rx })
}

impl ReplyReader {
    /// Remove whole submissions fitting a 32 KiB turn, unlocking before native PTY I/O.
    pub(crate) fn pop(&self) -> io::Result<Option<Vec<u8>>> {
        let mut state = self.sender.state.lock();
        let result = state.pop();
        let result = state.finish_operation(result);
        self.sender.stats.publish(&state);
        if !state.ram.is_empty() || state.spill.is_some() {
            // When: ram or spill still holds records, retain a wake hint for the next writer turn.
            let _ = self.sender.wake.try_send(());
        }
        result
    }

    /// Latch native writer failure for future reply admissions.
    pub(crate) fn fail(&self, error: io::Error) {
        let mut state = self.sender.state.lock();
        state.fail(error);
        // The storage is gone, so a sampler reading the gauges sees zero rather than the last backlog.
        self.sender.stats.publish(&state);
    }
}

// Lifecycle: ReplyReader closes sender admission and clears state on the native writer thread after in-flight operations.
impl Drop for ReplyReader {
    fn drop(&mut self) {
        self.sender.close();
        let mut state = self.sender.state.lock();
        state.clear();
        self.sender.stats.publish(&state);
    }
}

#[cfg(test)]
#[path = "reply_spool_tests.rs"]
mod reply_spool_tests;
