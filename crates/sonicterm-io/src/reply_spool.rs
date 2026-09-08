//! Bounded-resident reply FIFO with private temporary spill storage.
//!
//! Complete submissions remain indivisible across UI writer turns. Consumed
//! disk prefixes are reclaimed when the spill drains, not during a backlog.

use std::{
    collections::VecDeque,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

const RAM_BYTES: usize = 64 * 1024;
const CHUNK_BYTES: usize = 32 * 1024;
const HEADER_BYTES: usize = 4;

/// Cloneable ordered reply sender with 64 KiB resident storage and private disk spill.
///
/// Submit complete protocol replies outside parser/grid locks. Each submission
/// is at most 32 KiB and remains indivisible when interleaved with UI input.
/// Admission may perform synchronous file I/O, but never waits for native PTY
/// capacity. Disk usage is uncapped; consumed prefixes remain until drain.
/// Writer reads use at most 32 KiB plus a header of scratch and a 32 KiB output.
#[derive(Clone, Debug)]
pub struct PtyReplySender {
    state: Arc<Mutex<SpoolState>>,
    closed: Arc<AtomicBool>,
    wake: Sender<()>,
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
    pub fn send(&self, bytes: Vec<u8>) -> io::Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            // When: closed is published, reject without waiting on an in-flight storage operation.
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let mut state = self.state.lock();
        let result = state.append(&bytes);
        let result = state.finish_operation(result);
        // One pending hint is sufficient; notifications must not backpressure admission.
        let _ = self.wake.try_send(());
        result
    }

    /// Cancel admission without locks or file I/O; the native writer owns final storage cleanup.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.wake.try_send(());
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
        let result = (|| {
            if self.spill.is_none() {
                // Allocate a private anonymous file only after resident capacity is exhausted.
                self.spill = Some(Spill { file: tempfile::tempfile()?, read: 0, written: 0 });
            }
            let spill = self.spill.as_mut().expect("spill was initialized");
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
                let header = std::array::from_fn(|i| self.ram[i]);
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
    };
    (sender.clone(), ReplyReader { sender, wake: rx })
}

impl ReplyReader {
    /// Remove whole submissions fitting a 32 KiB turn, unlocking before native PTY I/O.
    pub(crate) fn pop(&self) -> io::Result<Option<Vec<u8>>> {
        let mut state = self.sender.state.lock();
        let result = state.pop();
        let result = state.finish_operation(result);
        if !state.ram.is_empty() || state.spill.is_some() {
            // When: ram or spill still holds records, retain a wake hint for the next writer turn.
            let _ = self.sender.wake.try_send(());
        }
        result
    }

    /// Latch native writer failure for future reply admissions.
    pub(crate) fn fail(&self, error: io::Error) {
        self.sender.state.lock().fail(error);
    }
}

// Lifecycle: ReplyReader closes sender admission and clears state on the native writer thread after in-flight operations.
impl Drop for ReplyReader {
    fn drop(&mut self) {
        self.sender.close();
        self.sender.state.lock().clear();
    }
}

#[cfg(test)]
#[path = "reply_spool_tests.rs"]
mod reply_spool_tests;
