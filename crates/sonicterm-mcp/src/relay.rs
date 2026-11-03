//! The relay: an agent's stdin to the endpoint, the endpoint to the agent's stdout.
//!
//! Three threads: stdin to the endpoint, the endpoint into a queue bounded at
//! [`QUEUE_BYTES`], and the queue to stdout. `main` returns on the first of: stdin closed, stdout
//! closed, a stall (the queue stayed full for [`STALL`] because stdout stopped draining), or the
//! endpoint closed. After the endpoint closes, the relay waits for stdout to write every queued
//! line, and ends as a stall once stdout goes [`DRAIN`] without taking a byte.
//!
//! Timing follows one rule: the relay gives up on stdout only when stdout took no byte for a
//! whole interval, [`STALL`] while connected with the queue full and [`DRAIN`] after the endpoint
//! closed. Every byte stdout accepts is progress: on macOS and Linux stdout is a nonblocking pipe
//! that takes whatever room the reader freed, and on Windows each write is capped at 512 bytes so
//! it blocks for little of a slow reader's time. A client
//! that stopped reading is let go within [`DRAIN`] of the close, or within [`STALL`] behind a
//! full queue. Returning ends the process even with a thread blocked on a standard handle, so no
//! handle is cancelled and nothing exits raw.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The longest line relayed either way; a longer one ends the relay.
pub const MAX_LINE_BYTES: usize = 1 << 20;
/// The most bytes queued for stdout.
pub const QUEUE_BYTES: usize = 4 << 20;
/// How long the queue may stay full before the relay gives up on stdout.
pub const STALL: Duration = Duration::from_secs(5);
/// After the endpoint closes, how long stdout may go without taking a line before the relay ends.
pub const DRAIN: Duration = Duration::from_secs(2);
/// The longest a stdout-progress check waits, in the drain and in a full queue.
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// Why the relay ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The agent closed stdin.
    InputClosed,
    /// SonicTerm closed the connection.
    EndpointClosed,
    /// stdout took no byte for [`STALL`] with the queue full, or for [`DRAIN`] after the
    /// endpoint closed.
    Stalled,
    /// stdout closed, so nothing more can reach the agent.
    OutputClosed,
    /// A line passed [`MAX_LINE_BYTES`].
    LineTooLong,
}

/// What a relay thread reports to `relay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    /// A thread saw the relay end.
    Ended(Ended),
    /// The writer wrote every queued line after the queue closed.
    Flushed,
}

/// Lines waiting for stdout, bounded in bytes.
struct Queue {
    /// The lines and the bytes they hold.
    state: Mutex<(VecDeque<Vec<u8>>, usize, bool)>,
    /// Signals a push, a pop or the end.
    changed: Condvar,
}

impl Queue {
    /// An empty queue.
    fn new() -> Arc<Self> {
        Arc::new(Self { state: Mutex::new((VecDeque::new(), 0, false)), changed: Condvar::new() })
    }

    /// Queue `line`, waiting for room while stdout keeps taking bytes (`written`); `false` once
    /// stdout took none for `stall` with the queue full.
    // Ordering: written Relaxed; a progress counter that orders no other memory.
    fn push(&self, line: Vec<u8>, written: &AtomicU64, stall: Duration) -> bool {
        let mut seen = written.load(Ordering::Relaxed);
        let mut deadline = Instant::now() + stall;
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.1 > 0 && state.1 + line.len() > QUEUE_BYTES {
            let now_written = written.load(Ordering::Relaxed);
            if now_written != seen {
                // stdout took bytes since the last look, so the stall restarts.
                seen = now_written;
                deadline = Instant::now() + stall;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // When: `left.is_zero()`, the deadline passed while the queue stayed full.
                return false;
            }
            state = self
                .changed
                .wait_timeout(state, left.min(DRAIN_POLL))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        state.1 += line.len();
        state.0.push_back(line);
        self.changed.notify_all();
        true
    }

    /// Mark the end: no more lines will be pushed.
    fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.2 = true;
        self.changed.notify_all();
    }

    /// The next line, or `None` once closed and empty.
    fn pop(&self) -> Option<Vec<u8>> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(line) = state.0.pop_front() {
                // When: `pop_front` yields a `line`, its bytes leave the queue's total.
                state.1 -= line.len();
                self.changed.notify_all();
                return Some(line);
            }
            if state.2 {
                // When: `state.2` marks the queue closed and it is empty, no line will arrive.
                return None;
            }
            state = self.changed.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// The next line from `reader`, newline included, or `None` at EOF; `Err` past the cap.
fn read_line(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, Ended> {
    let mut line = Vec::new();
    let read = reader.by_ref().take(MAX_LINE_BYTES as u64 + 1).read_until(b'\n', &mut line);
    match read {
        Ok(0) | Err(_) => Ok(None),
        Ok(_) if line.len() > MAX_LINE_BYTES => Err(Ended::LineTooLong),
        Ok(_) => Ok(Some(line)),
    }
}

/// The bridge preamble naming the pane the agent runs in, from `SONICTERM_PANE_ID`.
pub fn preamble(pane_id: Option<&str>) -> String {
    let pane = pane_id.and_then(|id| id.parse::<u64>().ok());
    match pane {
        Some(pane) => format!("{{\"sonicterm_bridge\":1,\"pane_id\":{pane}}}\n"),
        None => "{\"sonicterm_bridge\":1,\"pane_id\":null}\n".to_owned(),
    }
}

/// How long stdout may take no byte before the relay gives up on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    /// While connected, with the queue full.
    stall: Duration,
    /// After the endpoint closed.
    drain: Duration,
}

/// The shipping limits.
const LIMITS: Limits = Limits { stall: STALL, drain: DRAIN };

/// Relay until the first end, after sending `preamble`. The endpoint's halves are `from_server`
/// and `to_server`; `input` and `output` are the agent's stdin and stdout.
pub fn relay(
    preamble: &str,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
    from_server: impl Read + Send + 'static,
    to_server: impl Write + Send + 'static,
) -> Ended {
    relay_with(LIMITS, preamble, input, output, from_server, to_server)
}

/// [`relay`] with explicit `limits`, so tests can exercise the stall rule in milliseconds.
fn relay_with(
    limits: Limits,
    preamble: &str,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
    from_server: impl Read + Send + 'static,
    mut to_server: impl Write + Send + 'static,
) -> Ended {
    if to_server.write_all(preamble.as_bytes()).and_then(|()| to_server.flush()).is_err() {
        // When: writing the `preamble` to `to_server` failed, the endpoint is already gone.
        return Ended::EndpointClosed;
    }
    let (ended, first_end) = channel::<Event>();
    let queue = Queue::new();
    let written = Arc::new(AtomicU64::new(0));
    spawn_input(input, to_server, ended.clone());
    spawn_endpoint_reader(
        from_server,
        Arc::clone(&queue),
        ended.clone(),
        Arc::clone(&written),
        limits.stall,
    );
    spawn_output(output, Arc::clone(&queue), ended, Arc::clone(&written));
    // `Flushed` and the reader's end travel from different threads, so `Flushed` can arrive
    // first; it is remembered, since the writer sends it only once.
    let mut flushed = false;
    let end = loop {
        match first_end.recv() {
            Ok(Event::Ended(end)) => {
                // When: a thread reported `Ended`, that `end` decides what happens next.
                break end;
            }
            Ok(Event::Flushed) => {
                // When: `Flushed` came first, the writer already wrote everything; the endpoint's
                // close follows and is awaited.
                flushed = true;
                continue;
            }
            Err(_) => {
                // When: `recv` failed, every sender is gone, which only follows the endpoint's close.
                break Ended::EndpointClosed;
            }
        }
    };
    if end != Ended::EndpointClosed || flushed {
        // When: the first `end` is not `EndpointClosed`, or the writer already `flushed`, nothing
        // more is owed to stdout.
        return end;
    }
    drain(&first_end, &written, limits.drain)
}

/// After the endpoint closed: wait for stdout to write every queued line, while every byte it
/// takes (`written`) restarts the `limit`. A closed stdout or stdin meanwhile ends the session
/// normally; `limit` with no byte taken is a stall.
// Ordering: written Relaxed; a progress counter that orders no other memory.
fn drain(events: &Receiver<Event>, written: &AtomicU64, limit: Duration) -> Ended {
    let mut seen = written.load(Ordering::Relaxed);
    let mut deadline = Instant::now() + limit;
    loop {
        let now_written = written.load(Ordering::Relaxed);
        if now_written != seen {
            // stdout took bytes since the last look, so the drain limit restarts.
            deadline = Instant::now() + limit;
            seen = now_written;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            // When: `left` is zero, stdout took no byte for the whole `limit`, which is a stall.
            return Ended::Stalled;
        }
        match events.recv_timeout(left.min(DRAIN_POLL)) {
            Ok(Event::Flushed) => {
                // When: `Flushed` arrived, the writer's last write returned, so no line is cut off.
                return Ended::EndpointClosed;
            }
            Ok(Event::Ended(Ended::OutputClosed | Ended::InputClosed)) => {
                // When: stdout or stdin closed (`OutputClosed`, `InputClosed`) while draining, the
                // agent went away, which is a normal end.
                return Ended::EndpointClosed;
            }
            Ok(Event::Ended(_)) | Err(RecvTimeoutError::Timeout) => {
                // When: a late `Ended` from another thread or a poll `Timeout` arrived, the
                // deadline above decides.
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // When: every sender is gone (`Disconnected`), no thread can report progress.
                return Ended::Stalled;
            }
        }
    }
}

/// stdin to the endpoint, line by line.
fn spawn_input(
    input: impl Read + Send + 'static,
    mut to_server: impl Write + Send + 'static,
    ended: Sender<Event>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(input);
        let end = loop {
            match read_line(&mut reader) {
                Ok(Some(line)) => {
                    // When: stdin gave a whole `line`, it is forwarded to the endpoint at once.
                    if to_server.write_all(&line).and_then(|()| to_server.flush()).is_err() {
                        // When: writing the `line` to `to_server` failed, the endpoint closed.
                        break Ended::EndpointClosed;
                    }
                }
                Ok(None) => {
                    // When: `read_line` hit EOF on stdin, the agent closed its input and the relay ends.
                    break Ended::InputClosed;
                }
                Err(end) => {
                    // When: stdin gave an over-long line, that `end` stops the relay.
                    break end;
                }
            }
        };
        let _ = ended.send(Event::Ended(end));
    });
}

/// The endpoint into the bounded queue, which waits for room while stdout keeps taking bytes.
fn spawn_endpoint_reader(
    from_server: impl Read + Send + 'static,
    queue: Arc<Queue>,
    ended: Sender<Event>,
    written: Arc<AtomicU64>,
    stall: Duration,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(from_server);
        let end = loop {
            match read_line(&mut reader) {
                Ok(Some(line)) => {
                    // When: the endpoint gave a whole `line`, it waits in the bounded queue.
                    if !queue.push(line, &written, stall) {
                        // When: `queue.push` refused the `line`, stdout took no byte for the stall.
                        break Ended::Stalled;
                    }
                }
                Ok(None) => {
                    // When: `read_line` hit EOF on the endpoint, SonicTerm closed the connection.
                    break Ended::EndpointClosed;
                }
                Err(end) => {
                    // When: the endpoint sent an over-long line, that `end` stops the relay.
                    break end;
                }
            }
        };
        queue.close();
        let _ = ended.send(Event::Ended(end));
    });
}

/// The most bytes handed to a blocking stdout in one write, on Windows: a smaller write blocks
/// for less of a slow reader's time, so progress shows sooner.
#[cfg(windows)]
const WRITE_CHUNK: usize = 512;
/// The most bytes handed to stdout in one write on macOS and Linux. A nonblocking pipe takes a
/// partial write of any size, but a Linux socket frees its send buffer one written block at a
/// time, so a reader must consume a whole block before the writer sees room; 16 KiB blocks keep
/// that to one ordinary read.
#[cfg(not(windows))]
const WRITE_CHUNK: usize = 16 * 1024;

/// The part of `buffer` one write hands stdout: at most [`WRITE_CHUNK`] bytes.
fn one_write(buffer: &[u8]) -> &[u8] {
    &buffer[..buffer.len().min(WRITE_CHUNK)]
}

/// A writer that hands the inner writer [`one_write`]'s share per call and counts every byte it
/// accepts, so a long line being written still shows progress.
struct Counted<W> {
    /// The real stdout.
    inner: W,
    /// Bytes accepted so far.
    written: Arc<AtomicU64>,
}

impl<W: Write> Write for Counted<W> {
    // Ordering: written Relaxed; a progress counter that orders no other memory.
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let count = self.inner.write(one_write(buffer))?;
        self.written.fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// The queue to stdout, counting each byte in `written`: [`Event::Flushed`] once every line was
/// written after the queue closed, or `OutputClosed` when stdout refused a write.
fn spawn_output(
    output: impl Write + Send + 'static,
    queue: Arc<Queue>,
    ended: Sender<Event>,
    written: Arc<AtomicU64>,
) {
    std::thread::spawn(move || {
        let mut output = Counted { inner: output, written };
        while let Some(line) = queue.pop() {
            if output.write_all(&line).and_then(|()| output.flush()).is_err() {
                // When: writing the `line` to `output` failed, stdout closed and nothing can reach it.
                let _ = ended.send(Event::Ended(Ended::OutputClosed));
                return;
            }
        }
        let _ = ended.send(Event::Flushed);
    });
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod relay_tests;
