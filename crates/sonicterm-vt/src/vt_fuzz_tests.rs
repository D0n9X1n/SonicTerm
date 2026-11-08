//! A deterministic fuzz of the VT parser: seeded byte streams biased toward escape prefixes,
//! partial frames and huge parameters, fed in random chunk sizes. It pins what must hold for any
//! input: no panic, the grid keeps its size unless the stream asked for a resize, every reply is
//! captured and stays proportionate to its input, and media staging stays inside its pool.
//! `SONICTERM_FUZZ_ITERS` raises the iteration count for a longer local run.

use super::*;
use sonicterm_grid::grid::Grid;

/// Iterations the gate runs; each feeds one generated stream.
const GATE_ITERATIONS: usize = 20_000;

/// The most reply bytes one input byte may cause. The largest replies (XTVERSION, a palette or
/// colour query) are under 64 bytes and each needs at least a few input bytes to ask for.
const MAX_REPLY_BYTES_PER_INPUT_BYTE: usize = 64;

/// A xorshift64* generator: the same seed gives the same streams on every platform.
struct Xorshift(u64);

impl Xorshift {
    /// The next 64 random bits.
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..bound`.
    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// Escape openings the generator splices in, so most streams reach the parser's capture,
/// dispatch and discard paths rather than only printing.
const PREFIXES: &[&[u8]] = &[
    b"\x1b[",
    b"\x1b]",
    b"\x1bP",
    b"\x1b_G",
    b"\x1b^",
    b"\x1b]1337;File=",
    b"\x1b]52;c;",
    b"\x1b]133;",
    b"\x1b]8;;",
    b"\x1b[?",
    b"\x1bPq",
    b"\x1b[>",
    b"\x1b[6n",
    b"\x1b[c",
    b"\x1b[>q",
    b"\x1b]4;1;?\x07",
];

/// Ways a sequence ends, including the missing end of a partial frame.
const ENDS: &[&[u8]] = &[b"\x07", b"\x1b\\", b"m", b"h", b"l", b"H", b"J", b"K", b"", b"\x18"];

/// One generated stream of up to a few KiB.
fn stream(rng: &mut Xorshift) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..1 + rng.below(24) {
        match rng.below(6) {
            0 => {
                // Plain text, including UTF-8 and C0 controls.
                let text = ["hello", "日本語", "\r\n", "\t", "\x08", "e\u{301}", "🙂"];
                bytes.extend_from_slice(text[rng.below(text.len())].as_bytes());
            }
            1 => {
                // A huge numeric parameter list.
                bytes.extend_from_slice(b"\x1b[");
                for _ in 0..rng.below(64) {
                    bytes.extend_from_slice(rng.next_u64().to_string().as_bytes());
                    bytes.push(if rng.below(4) == 0 { b':' } else { b';' });
                }
                bytes.push(b"mHJKrtX@PLMr"[rng.below(12)]);
            }
            _ => {
                // An escape prefix, a random body and an end that may be missing.
                bytes.extend_from_slice(PREFIXES[rng.below(PREFIXES.len())]);
                for _ in 0..rng.below(256) {
                    bytes.push(rng.next_u64() as u8);
                }
                bytes.extend_from_slice(ENDS[rng.below(ENDS.len())]);
            }
        }
    }
    bytes
}

/// Whether `bytes` holds anything that may legitimately change the grid's size (`CSI 8 ; … t`
/// window resizes and DECCOLM), so the size check applies only to streams that hold none.
fn may_resize(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"\x1b[8;")
        || bytes.windows(3).any(|window| window == b"?3h" || window == b"?3l")
}

/// Feed `bytes` to `parser` in chunks of random size, consuming every reply as the PTY worker
/// does, and return the reply bytes it produced.
fn feed(parser: &mut Parser, bytes: &[u8], rng: &mut Xorshift) -> usize {
    let mut replies = 0;
    let mut offset = 0;
    while offset < bytes.len() {
        let chunk_end = (offset + 1 + rng.below(512)).min(bytes.len());
        let mut chunk = &bytes[offset..chunk_end];
        // A call may consume nothing when it yields for the host (an image or Kitty reply comes
        // first), as the PTY worker's loop allows; two idle calls in a row would be a stall.
        let mut idle_calls = 0;
        while !chunk.is_empty() {
            let (consumed, _events, reply) = parser.advance_with_replies(chunk);
            replies += reply.len();
            idle_calls = if consumed == 0 { idle_calls + 1 } else { 0 };
            assert!(idle_calls < 2, "the parser stalled on {chunk:?}");
            chunk = &chunk[consumed..];
        }
        offset = chunk_end;
    }
    replies
}

/// Any input leaves the parser whole: no panic, the grid's size unchanged unless the stream asked
/// to resize, replies proportionate to the input, and staging within its private pool's ceiling.
#[test]
fn generated_streams_keep_the_parser_within_its_bounds() {
    let iterations = std::env::var("SONICTERM_FUZZ_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(GATE_ITERATIONS);
    let mut rng = Xorshift(0x005E_ED0F_F022);
    let pool = CaptureStagingPool::new();
    let (cols, rows) = (80, 24);
    let mut total_replies = 0;
    let mut parser = Parser::new_with_staging_pool(Grid::new(cols, rows), None, Arc::clone(&pool));
    for iteration in 0..iterations {
        let bytes = stream(&mut rng);
        // A fresh parser now and then, so a stuck state cannot hide every later stream.
        if iteration % 64 == 0 {
            parser = Parser::new_with_staging_pool(Grid::new(cols, rows), None, Arc::clone(&pool));
        }
        let replies = feed(&mut parser, &bytes, &mut rng);
        total_replies += replies;
        assert!(
            replies <= bytes.len() * MAX_REPLY_BYTES_PER_INPUT_BYTE,
            "iteration {iteration}: {replies} reply bytes for {} input bytes",
            bytes.len()
        );
        let grid = parser.grid();
        if !may_resize(&bytes) {
            assert_eq!((grid.cols, grid.rows), (cols, rows), "iteration {iteration}");
        }
        assert!(grid.cols >= 1 && grid.rows >= 1, "iteration {iteration}");
        assert!(
            parser.retained_amount().bytes <= MAX_PROCESS_CAPTURE_STAGING_BYTES + 64 * 1024,
            "iteration {iteration}: {:?} retained",
            parser.retained_amount()
        );
    }
    assert!(total_replies > 1000, "replies were exercised: {total_replies}");
    drop(parser);
    assert_eq!(pool.live_captures(), 0, "every capture released its staging");
}
