//! Pins the relay: the preamble's format, line order both ways, the line cap, stdout carrying only
//! relayed bytes, and bounded exits when the endpoint closes or stdout stops draining.

use std::io::{self, Read, Write};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use super::*;

/// The reading end of an in-memory pipe; a dropped sender reads as end of stream.
struct PipeReader {
    /// Chunks from the writer.
    chunks: Receiver<Vec<u8>>,
    /// The unread rest of the current chunk.
    pending: Vec<u8>,
}

impl Read for PipeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_empty() {
            match self.chunks.recv() {
                Ok(chunk) => self.pending = chunk,
                Err(_) => return Ok(0),
            }
        }
        let count = buffer.len().min(self.pending.len());
        buffer[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        Ok(count)
    }
}

/// The writing end of an in-memory pipe.
struct PipeWriter(Sender<Vec<u8>>);

impl Write for PipeWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.send(buffer.to_vec()).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// An in-memory pipe.
fn pipe() -> (PipeWriter, PipeReader) {
    let (sender, receiver) = channel();
    (PipeWriter(sender), PipeReader { chunks: receiver, pending: Vec::new() })
}

/// A stdout that never drains: every write blocks forever.
struct Stuck;

impl Write for Stuck {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        loop {
            std::thread::park();
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Everything readable from `reader` until it closes, gathered on another thread.
fn collect(mut reader: PipeReader) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    })
}

/// The preamble names the pane as a number, or null when the variable is missing or not a pane id.
#[test]
fn the_preamble_names_the_pane() {
    assert_eq!(preamble(Some("42")), "{\"sonicterm_bridge\":1,\"pane_id\":42}\n");
    assert_eq!(preamble(None), "{\"sonicterm_bridge\":1,\"pane_id\":null}\n");
    assert_eq!(preamble(Some("nope")), "{\"sonicterm_bridge\":1,\"pane_id\":null}\n");
}

/// Lines go both ways in order; the server sees the preamble first, and stdout carries exactly
/// what the server sent and nothing else.
#[test]
fn lines_relay_in_order_and_stdout_holds_only_relayed_bytes() {
    let (mut agent_in, input) = pipe();
    let (output, agent_out) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, server_in) = pipe();
    let server_seen = collect(server_in);
    let stdout_seen = collect(agent_out);
    agent_in.write_all(b"one\ntwo\n").unwrap();
    server_out.write_all(b"reply 1\nreply 2\n").unwrap();
    drop(server_out);
    let ended = relay(&preamble(Some("7")), input, output, from_server, to_server);
    assert_eq!(ended, Ended::EndpointClosed);
    drop(agent_in);
    assert_eq!(String::from_utf8(stdout_seen.join().unwrap()).unwrap(), "reply 1\nreply 2\n");
    let seen = String::from_utf8(server_seen.join().unwrap()).unwrap();
    assert!(seen.starts_with("{\"sonicterm_bridge\":1,\"pane_id\":7}\n"), "{seen}");
    assert!(seen.ends_with("one\ntwo\n"), "{seen}");
}

/// A line past the cap ends the relay rather than being cut.
#[test]
fn an_over_long_line_ends_the_relay() {
    let (mut agent_in, input) = pipe();
    let (output, _agent_out) = pipe();
    let (_server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    agent_in.write_all(&vec![b'x'; MAX_LINE_BYTES + 1]).unwrap();
    assert_eq!(relay("{}\n", input, output, from_server, to_server), Ended::LineTooLong);
}

/// With stdin idle, a closed endpoint ends the relay within 3 s, even when stdout never drains.
#[test]
fn a_closed_endpoint_ends_the_relay_quickly() {
    let (_agent_in, input) = pipe();
    let (server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    drop(server_out);
    let started = Instant::now();
    assert_eq!(relay("{}\n", input, Stuck, from_server, to_server), Ended::EndpointClosed);
    assert!(started.elapsed() < Duration::from_secs(3));
}

/// Output past the queue with stdout stuck and stdin open ends the relay as a stall within 8 s.
#[test]
fn a_stuck_stdout_ends_the_relay_as_a_stall() {
    let (_agent_in, input) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let line = [vec![b'y'; 64 * 1024 - 1], vec![b'\n']].concat();
    std::thread::spawn(move || {
        for _ in 0..(QUEUE_BYTES / line.len() + 4) {
            if server_out.write_all(&line).is_err() {
                return;
            }
        }
        std::thread::park();
    });
    let started = Instant::now();
    assert_eq!(relay("{}\n", input, Stuck, from_server, to_server), Ended::Stalled);
    assert!(started.elapsed() < Duration::from_secs(8));
}

/// A closed endpoint never cuts off a line stdout is still writing: the relay ends only after the
/// last byte of a 512 KiB reply was written, with stdin still open.
#[test]
fn a_closed_endpoint_waits_for_the_line_being_written() {
    let (_agent_in, input) = pipe();
    let (sink, agent_out) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let stdout_seen = collect(agent_out);
    let reply = [vec![b'z'; 512 * 1024 - 1], vec![b'\n']].concat();
    server_out.write_all(&reply).unwrap();
    drop(server_out);
    let output = Paced { sink, bytes_per_second: 3 << 20 };
    assert_eq!(relay("{}\n", input, output, from_server, to_server), Ended::EndpointClosed);
    assert_eq!(stdout_seen.join().unwrap(), reply, "every byte reached stdout before the end");
}

/// A stdout that refuses every write.
struct Closed;

impl Write for Closed {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A closed stdout ends the relay as soon as a reply cannot be written, with stdin and the
/// endpoint both still open.
#[test]
fn a_closed_stdout_ends_the_relay() {
    let (_agent_in, input) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    server_out.write_all(b"reply\n").unwrap();
    let started = Instant::now();
    assert_eq!(relay("{}\n", input, Closed, from_server, to_server), Ended::OutputClosed);
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(server_out);
}

/// With the queue full and stdout stuck, EOF waits behind the queue, so the stall governs: a
/// closed endpoint ends the relay as a stall within the stall plus the drain.
#[test]
fn a_closed_endpoint_behind_a_full_queue_ends_as_a_stall() {
    let (_agent_in, input) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let line = [vec![b'y'; 64 * 1024 - 1], vec![b'\n']].concat();
    for _ in 0..(QUEUE_BYTES / line.len() + 3) {
        server_out.write_all(&line).unwrap();
    }
    drop(server_out);
    let started = Instant::now();
    let ended = relay("{}\n", input, Stuck, from_server, to_server);
    assert!(started.elapsed() < STALL + DRAIN + Duration::from_secs(1), "{:?}", started.elapsed());
    assert_eq!(ended, Ended::Stalled, "queued output was never written");
}

/// A stdout that takes the first `allowed` bytes, then refuses every write, as a client that
/// closes its end partway through a long reply.
struct ClosesAfter {
    /// Bytes still accepted.
    allowed: usize,
}

impl Write for ClosesAfter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.allowed == 0 {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let count = buffer.len().min(self.allowed);
        // Taken at about 1 MiB/s, so 128 KiB lasts past the endpoint's close.
        std::thread::sleep(Duration::from_micros(count as u64));
        self.allowed -= count;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// stdout closing while the relay drains after the endpoint closed is a normal end, never a
/// stall: the relay ends at once as closed.
#[test]
fn stdout_closing_during_the_drain_is_a_normal_end() {
    let (_agent_in, input) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    server_out.write_all(&[vec![b'z'; 512 * 1024 - 1], vec![b'\n']].concat()).unwrap();
    drop(server_out);
    let started = Instant::now();
    let output = ClosesAfter { allowed: 128 * 1024 };
    assert_eq!(relay("{}\n", input, output, from_server, to_server), Ended::EndpointClosed);
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
}

/// A client that keeps reading, however slowly, is never cut off: stdout taking a capped write
/// every 12 ms, for longer than the drain in total, still receives every byte of a reply past
/// the queue.
#[test]
fn a_slow_but_steady_reader_is_never_cut_off() {
    let (_agent_in, input) = pipe();
    let (sink, agent_out) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let stdout_seen = collect(agent_out);
    let line = [vec![b'w'; 64 * 1024 - 1], vec![b'\n']].concat();
    let count = QUEUE_BYTES / line.len() + 8;
    let writer = std::thread::spawn(move || {
        for _ in 0..count {
            server_out.write_all(&line).unwrap();
        }
    });
    let output = Paced { sink, bytes_per_second: 5 << 20 };
    let ended = relay("{}\n", input, output, from_server, to_server);
    writer.join().unwrap();
    assert_eq!(ended, Ended::EndpointClosed);
    assert_eq!(stdout_seen.join().unwrap().len(), count * 64 * 1024);
}

/// A client reading one long reply steadily is never cut off, even when the reply takes longer
/// than the drain to write: progress is counted in bytes, not whole lines.
#[test]
fn a_long_line_read_steadily_is_never_cut_off() {
    let (_agent_in, input) = pipe();
    let (sink, agent_out) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let stdout_seen = collect(agent_out);
    let reply = [vec![b'q'; 512 * 1024 - 1], vec![b'\n']].concat();
    server_out.write_all(&reply).unwrap();
    drop(server_out);
    // 16 KiB every 100 ms: about 3.2 s for the whole line, past the 2 s drain.
    let output = Paced { sink, bytes_per_second: 160 * 1024 };
    assert_eq!(relay("{}\n", input, output, from_server, to_server), Ended::EndpointClosed);
    assert_eq!(stdout_seen.join().unwrap(), reply);
}

/// An endpoint that closes without sending anything, after the writer has settled, ends the
/// relay as closed every time: a `Flushed` that arrives before the close is not lost.
#[test]
fn an_early_flush_is_not_lost() {
    for _ in 0..200 {
        let (_agent_in, input) = pipe();
        let (output, _agent_out) = pipe();
        let (server_out, from_server) = pipe();
        let (to_server, _server_in) = pipe();
        drop(server_out);
        let started = Instant::now();
        assert_eq!(relay("{}\n", input, output, from_server, to_server), Ended::EndpointClosed);
        assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    }
}

/// A stdout like the bridge's nonblocking pipe: each write takes at most a page, and returns once
/// the reader has had time to take it, at `bytes_per_second`.
struct Paced {
    /// Where the written bytes go.
    sink: PipeWriter,
    /// The reader's pace.
    bytes_per_second: u64,
}

impl Write for Paced {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let taken = &buffer[..buffer.len().min(4096)];
        let micros = taken.len() as u64 * 1_000_000 / self.bytes_per_second;
        std::thread::sleep(Duration::from_micros(micros));
        self.sink.write(taken)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A stdout whose single write blocks for as long as the reader takes to drain it still shows
/// steady progress, because each write is capped: a 512 KiB line at 160 KiB/s arrives whole.
#[test]
fn a_write_that_blocks_on_a_slow_reader_still_shows_progress() {
    let (_agent_in, input) = pipe();
    let (sink, agent_out) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let stdout_seen = collect(agent_out);
    let reply = [vec![b'p'; 512 * 1024 - 1], vec![b'\n']].concat();
    server_out.write_all(&reply).unwrap();
    drop(server_out);
    let output = Paced { sink, bytes_per_second: 160 * 1024 };
    assert_eq!(relay("{}\n", input, output, from_server, to_server), Ended::EndpointClosed);
    assert_eq!(stdout_seen.join().unwrap(), reply);
}

/// Through a real kernel pipe read 1 KiB every 200 ms, as the slowest realistic client reads, at
/// the shipping limits: a reply that takes longer than the drain to read arrives whole.
#[cfg(unix)]
#[test]
fn a_real_pipe_read_slowly_receives_the_whole_reply() {
    use std::fs::File;
    use std::os::unix::io::FromRawFd;
    let mut ends = [0i32; 2];
    let made =
        // SAFETY: `ends` is a writable two-element array, as `pipe` requires.
        unsafe { libc::pipe(ends.as_mut_ptr()) };
    assert_eq!(made, 0);
    let mut read_end =
        // SAFETY: `pipe` returned `ends[0]`, and this `File` takes sole ownership of it.
        unsafe { File::from_raw_fd(ends[0]) };
    let write_end =
        // SAFETY: `pipe` returned `ends[1]`, and this `File` takes sole ownership of it.
        unsafe { File::from_raw_fd(ends[1]) };
    let reader = std::thread::spawn(move || {
        let mut received = Vec::new();
        let mut chunk = vec![0u8; 1024];
        loop {
            std::thread::sleep(Duration::from_millis(200));
            match read_end.read(&mut chunk) {
                Ok(0) | Err(_) => return received,
                Ok(count) => received.extend_from_slice(&chunk[..count]),
            }
        }
    });
    let (_agent_in, input) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    // 64 KiB fills the pipe's buffer; reading the next 16 KiB at this pace takes 3.2 s, past
    // the 2 s drain.
    let reply = [vec![b'r'; 80 * 1024 - 1], vec![b'\n']].concat();
    server_out.write_all(&reply).unwrap();
    drop(server_out);
    let output = crate::stdout_unix::Stdout::on(std::os::unix::io::AsRawFd::as_raw_fd(&write_end));
    assert_eq!(relay("{}\n", input, output, from_server, to_server), Ended::EndpointClosed);
    drop(write_end);
    assert_eq!(reader.join().unwrap().len(), reply.len());
}

/// Short limits, so the stall rule can be exercised in milliseconds.
const QUICK: Limits =
    Limits { stall: Duration::from_millis(300), drain: Duration::from_millis(300) };

/// A drain several read gaps long, for the slow-reader tests: each read shows up as progress within
/// the writer's 50 ms retry, so a late wake-up on a loaded CI runner cannot end the relay.
#[cfg(unix)]
const PACED: Limits = Limits { stall: Duration::from_secs(1), drain: Duration::from_secs(1) };
/// The gap between a slow reader's reads, well inside [`PACED`]'s drain.
#[cfg(unix)]
const READ_EVERY: Duration = Duration::from_millis(150);

/// One reply far longer to write than the drain, read at a pace whose every read frees more
/// than a write: the relay never gives up, because each 512-byte write is progress.
#[test]
fn a_reply_written_far_slower_than_the_drain_is_never_cut_off() {
    let (_agent_in, input) = pipe();
    let (sink, agent_out) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let stdout_seen = collect(agent_out);
    let reply = [vec![b'a'; 96 * 1024 - 1], vec![b'\n']].concat();
    server_out.write_all(&reply).unwrap();
    drop(server_out);
    // About 1.5 s for the reply, five times the 300 ms drain.
    let output = Paced { sink, bytes_per_second: 64 * 1024 };
    let started = Instant::now();
    let ended = relay_with(QUICK, "{}\n", input, output, from_server, to_server);
    assert_eq!(ended, Ended::EndpointClosed);
    assert!(started.elapsed() > QUICK.drain * 3, "the reply outlasted the drain");
    assert_eq!(stdout_seen.join().unwrap(), reply);
}

/// While connected, a push that waits behind the line being written and a full queue is not a
/// stall when stdout keeps taking bytes: six 1 MiB lines (one being written, four queued, one
/// waiting to be queued) all arrive, though each line takes longer to write than the stall.
#[test]
fn a_full_queue_behind_a_long_line_is_not_a_stall_while_stdout_reads() {
    let (_agent_in, input) = pipe();
    let (sink, mut agent_out) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let line = [vec![b'b'; (1 << 20) - 1], vec![b'\n']].concat();
    // One line in flight plus a full queue, plus one more so a push must wait.
    let count = QUEUE_BYTES / line.len() + 2;
    let total = count * line.len();
    let (all_read, server_may_close) = channel::<()>();
    let server = std::thread::spawn(move || {
        for _ in 0..count {
            server_out.write_all(&line).unwrap();
        }
        // The connection stays open until the agent has every byte, so only the queue's stall
        // rule could have ended the relay early.
        let _ = server_may_close.recv();
    });
    let reader = std::thread::spawn(move || {
        let mut seen = 0;
        let mut chunk = vec![0u8; 1 << 16];
        while seen < total {
            match agent_out.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => seen += read,
            }
        }
        let _ = all_read.send(());
        seen
    });
    // About 0.5 s per 1 MiB line, longer than the 300 ms stall.
    let output = Paced { sink, bytes_per_second: 2 << 20 };
    let ended = relay_with(QUICK, "{}\n", input, output, from_server, to_server);
    server.join().unwrap();
    assert_eq!(ended, Ended::EndpointClosed);
    assert_eq!(reader.join().unwrap(), total);
}

/// A full queue with stdout taking nothing is still a stall at the stall limit.
#[test]
fn a_full_queue_with_stdout_stuck_is_a_stall() {
    let (_agent_in, input) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let line = [vec![b'c'; 64 * 1024 - 1], vec![b'\n']].concat();
    std::thread::spawn(move || {
        for _ in 0..(QUEUE_BYTES / line.len() + 4) {
            if server_out.write_all(&line).is_err() {
                return;
            }
        }
        std::thread::park();
    });
    let started = Instant::now();
    assert_eq!(relay_with(QUICK, "{}\n", input, Stuck, from_server, to_server), Ended::Stalled);
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
}

/// The relay as the bridge composes it: the endpoint closed behind a reply far longer than the
/// pipe, written to stdout through the nonblocking pipe writer, and read in reads just big
/// enough to free room on this platform, one every 150 ms for three times the drain. The relay
/// stays up through the slow reads, then delivers every byte and ends as closed.
#[cfg(unix)]
#[test]
fn the_composed_writer_keeps_a_slow_small_reader() {
    use std::fs::File;
    use std::os::unix::io::{AsRawFd, FromRawFd};
    // The smallest read that frees room in a full pipe: a whole page on Linux, any read elsewhere.
    let freeing_read = if cfg!(target_os = "linux") { 4096 } else { 256 };
    let mut ends = [0i32; 2];
    let made =
        // SAFETY: `ends` is a writable two-element array, as `pipe` requires.
        unsafe { libc::pipe(ends.as_mut_ptr()) };
    assert_eq!(made, 0);
    let mut read_end =
        // SAFETY: `pipe` returned `ends[0]`, and this `File` takes sole ownership of it.
        unsafe { File::from_raw_fd(ends[0]) };
    let write_end =
        // SAFETY: `pipe` returned `ends[1]`, and this `File` takes sole ownership of it.
        unsafe { File::from_raw_fd(ends[1]) };
    let output = crate::stdout_unix::Stdout::on(write_end.as_raw_fd());
    let (_agent_in, input) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let reply = [vec![b's'; 256 * 1024 - 1], vec![b'\n']].concat();
    server_out.write_all(&reply).unwrap();
    drop(server_out);
    let (finished, ended) = channel();
    let relay_thread = std::thread::spawn(move || {
        let end = relay_with(PACED, "{}\n", input, output, from_server, to_server);
        // The writer's descriptor stays open until the relay is done with it.
        drop(write_end);
        let _ = finished.send(end);
    });
    let mut received = 0;
    let mut chunk = vec![0u8; freeing_read];
    // Slow phase: 3 s of small reads, three drains long. Each read shows up as progress within the
    // writer's 50 ms retry, so a gap stays far inside the 1 s drain even on a loaded CI runner.
    for _ in 0..20 {
        std::thread::sleep(READ_EVERY);
        received += read_end.read(&mut chunk).unwrap();
        assert!(ended.try_recv().is_err(), "the relay gave up on a reader that kept reading");
    }
    let mut rest = Vec::new();
    read_end.read_to_end(&mut rest).unwrap();
    received += rest.len();
    relay_thread.join().unwrap();
    assert_eq!(ended.recv().unwrap(), Ended::EndpointClosed);
    assert_eq!(received, reply.len());
}

/// The composed writer on a socket stdout, as Node's `child_process` gives an MCP server on
/// macOS: a 900 KiB reply read in 16 KiB reads 150 ms apart for three drains arrives whole.
#[cfg(unix)]
#[test]
fn the_composed_writer_keeps_a_slow_reader_on_a_socket() {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;
    let (mut client_end, bridge_end) = UnixStream::pair().unwrap();
    let output = crate::stdout_unix::Stdout::on(bridge_end.as_raw_fd());
    let (_agent_in, input) = pipe();
    let (mut server_out, from_server) = pipe();
    let (to_server, _server_in) = pipe();
    let reply = [vec![b'k'; 900 * 1024 - 1], vec![b'\n']].concat();
    server_out.write_all(&reply).unwrap();
    drop(server_out);
    let (finished, ended) = channel();
    let relay_thread = std::thread::spawn(move || {
        let end = relay_with(PACED, "{}\n", input, output, from_server, to_server);
        // The socket stays open until the relay is done with it.
        drop(bridge_end);
        let _ = finished.send(end);
    });
    let mut received = 0;
    // A Linux socket frees its send buffer one 16 KiB write at a time, so each read there takes
    // two writes' worth, a margin over the documented floor; macOS frees room for any read.
    let read_size = if cfg!(target_os = "linux") { 32 * 1024 } else { 16 * 1024 };
    let mut chunk = vec![0u8; read_size];
    // Slow phase: 3 s of reads, three drains long.
    for _ in 0..20 {
        std::thread::sleep(READ_EVERY);
        received += client_end.read(&mut chunk).unwrap();
        assert!(ended.try_recv().is_err(), "the relay gave up on a reader that kept reading");
    }
    let mut rest = Vec::new();
    client_end.read_to_end(&mut rest).unwrap();
    received += rest.len();
    relay_thread.join().unwrap();
    assert_eq!(ended.recv().unwrap(), Ended::EndpointClosed);
    assert_eq!(received, reply.len());
}
