//! The Windows delivery replay: one untimed run of a scenario's role program under ConPTY, with no
//! window, that checks what ConPTY delivered against what the program printed.
//!
//! `--capture-delivery <scratch>` prepares the scratch as a run does, starts this binary in program
//! mode under a 250 x 70 pseudoconsole, reads its output until the scenario's checks are complete
//! or its timeout passes, and writes `delivery.json`. The classifiers here are pure, so tests pin
//! them on every host; only the replay itself is Windows-only.

use std::collections::{BTreeSet, HashMap};
#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use crossbeam_channel::RecvTimeoutError;
use serde::Serialize;
#[cfg(windows)]
use sonicterm_io::pty::PtyHandle;

#[cfg(windows)]
use crate::cli::{RunArgs, REFUSED};
#[cfg(windows)]
use crate::record::wide_tokens;
use crate::record::{missing_wide_tokens, Status};
use crate::scenarios::Workload;
#[cfg(windows)]
use crate::scenarios::{Fixture, Plan};
#[cfg(windows)]
use crate::workload::{self, FixtureBody, FixtureFile};

/// The record's file name in the scratch, as the comparison reads it.
#[cfg(windows)]
const DELIVERY_FILE: &str = "delivery.json";
/// The record's schema version; the comparison refuses any other.
const DELIVERY_SCHEMA_VERSION: u32 = 1;
/// The most replay output kept for the classifiers; output beyond it fails the replay.
pub(crate) const DELIVERY_LIMIT_BYTES: usize = 64 << 20;
/// The longest line, in bytes, the READY and sentinel comparisons hold; a longer line matches nothing.
const LINE_LIMIT_BYTES: usize = 4_096;
/// The most CSI parameter bytes kept; ConPTY's longest is a cursor position.
const PARAM_LIMIT_BYTES: usize = 32;
/// The most spaces one `CSI n C` stands for, so a hostile count cannot grow the text without bound.
const FORWARD_LIMIT: usize = 1_000;
/// The query ConPTY sends at startup with cursor inheritance; it paints nothing until answered.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";
/// The start of an OSC 1337 inline file.
const OSC1337_PREFIX: &[u8] = b"\x1b]1337;";

/// What one piece of ConPTY output means for the text a reader sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visible {
    /// One byte of text, a UTF-8 lead or continuation byte included.
    Text(u8),
    /// A carriage return.
    CarriageReturn,
    /// A line feed.
    LineFeed,
    /// A cursor move other than `CSI n C`, which can start text anywhere on the screen.
    Move,
    /// DEC 2026: `true` opens a synchronized update, `false` closes it.
    Sync(bool),
}

/// Where the parser stands inside an escape sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseState {
    /// Plain text.
    Ground,
    /// After ESC.
    Escape,
    /// After ESC and an intermediate byte, as in `ESC ( B`.
    Intermediate,
    /// Inside a CSI sequence.
    Csi,
    /// Inside an OSC, DCS, SOS, PM or APC string.
    String,
    /// After ESC inside a string, which `\` turns into the string terminator.
    StringEscape,
}

/// A streaming VT parser that turns ConPTY output into [`Visible`] events whatever byte a chunk
/// ends on: conhost re-renders the program's output, so text is compared after parsing, not raw.
struct VisibleStream {
    state: ParseState,
    /// The current CSI sequence's parameter and intermediate bytes, at most `PARAM_LIMIT_BYTES`.
    params: Vec<u8>,
}

impl VisibleStream {
    fn new() -> Self {
        Self { state: ParseState::Ground, params: Vec::new() }
    }

    /// Parse `bytes`, passing each event to `emit`.
    fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(Visible)) {
        for &byte in bytes {
            self.step(byte, &mut emit);
        }
    }

    fn step(&mut self, byte: u8, emit: &mut impl FnMut(Visible)) {
        match self.state {
            ParseState::Ground => match byte {
                0x1b => self.state = ParseState::Escape,
                b'\r' => emit(Visible::CarriageReturn),
                b'\n' => emit(Visible::LineFeed),
                // When: another C0 control, BEL or a backspace, prints no text a check compares.
                0x00..=0x1f | 0x7f => {}
                _ => emit(Visible::Text(byte)),
            },
            ParseState::Escape => match byte {
                b'[' => {
                    self.params.clear();
                    self.state = ParseState::Csi;
                }
                // OSC, DCS, SOS, PM and APC each run to their terminator.
                b']' | b'P' | b'X' | b'^' | b'_' => self.state = ParseState::String,
                0x20..=0x2f => self.state = ParseState::Intermediate,
                _ => self.state = ParseState::Ground,
            },
            ParseState::Intermediate => {
                if !(0x20..=0x2f).contains(&byte) {
                    // When: the final byte arrives, the sequence ends; it moves no text a check reads.
                    self.state = ParseState::Ground;
                }
            }
            ParseState::Csi => match byte {
                0x40..=0x7e => {
                    self.state = ParseState::Ground;
                    self.finish_csi(byte, emit);
                }
                // When: ESC interrupts the sequence, a new one starts and this one is dropped.
                0x1b => self.state = ParseState::Escape,
                _ if self.params.len() < PARAM_LIMIT_BYTES => self.params.push(byte),
                _ => {}
            },
            ParseState::String => match byte {
                0x07 => self.state = ParseState::Ground,
                0x1b => self.state = ParseState::StringEscape,
                _ => {}
            },
            ParseState::StringEscape => {
                if byte == b'\\' {
                    self.state = ParseState::Ground;
                } else {
                    // When: ESC was not the terminator's start, it begins a new sequence.
                    self.state = ParseState::Escape;
                    self.step(byte, emit);
                }
            }
        }
    }

    /// The event a complete CSI sequence ending in `final_byte` stands for.
    fn finish_csi(&mut self, final_byte: u8, emit: &mut impl FnMut(Visible)) {
        match (final_byte, self.params.as_slice()) {
            // conhost writes some runs of spaces as a cursor forward, so it reads as those spaces.
            (b'C', params) => {
                for _ in 0..forward_count(params) {
                    emit(Visible::Text(b' '));
                }
            }
            (b'h', b"?2026") => emit(Visible::Sync(true)),
            (b'l', b"?2026") => emit(Visible::Sync(false)),
            (
                b'A' | b'B' | b'D' | b'E' | b'F' | b'G' | b'H' | b'J' | b'S' | b'T' | b'd' | b'f'
                | b'r',
                _,
            ) => emit(Visible::Move),
            _ => {}
        }
    }
}

/// The spaces `CSI <params> C` moves over: one when the count is missing or zero.
fn forward_count(params: &[u8]) -> usize {
    let count = std::str::from_utf8(params).ok().and_then(|text| text.parse::<usize>().ok());
    count.unwrap_or(1).clamp(1, FORWARD_LIMIT)
}

/// The text a reader of `bytes` sees, with each line feed, carriage return or cursor move as `\n`,
/// and the offset in that text of every DEC 2026 bracket, `true` for an open.
fn visible_text(bytes: &[u8]) -> (Vec<u8>, Vec<(usize, bool)>) {
    let mut text = Vec::with_capacity(bytes.len());
    let mut brackets = Vec::new();
    VisibleStream::new().feed(bytes, |event| match event {
        Visible::Text(byte) => text.push(byte),
        Visible::CarriageReturn | Visible::LineFeed | Visible::Move => text.push(b'\n'),
        Visible::Sync(open) => brackets.push((text.len(), open)),
    });
    (text, brackets)
}

/// The current line's text as a reader sees it, for comparing whole lines.
#[derive(Default)]
struct LineBuffer {
    text: Vec<u8>,
    /// A carriage return came last, so the next text rewrites the line from its start.
    returned: bool,
    /// The line outgrew `LINE_LIMIT_BYTES`, so it matches nothing.
    overflowed: bool,
}

impl LineBuffer {
    fn push(&mut self, byte: u8) {
        if self.returned {
            // When: text follows a bare carriage return, it overwrites the line.
            self.clear();
        }
        if self.text.len() < LINE_LIMIT_BYTES {
            self.text.push(byte);
        } else {
            self.overflowed = true;
        }
    }

    fn clear(&mut self) {
        self.text.clear();
        self.returned = false;
        self.overflowed = false;
    }

    /// Whether the line reads `expected`, trailing spaces aside.
    fn reads(&self, expected: &str) -> bool {
        let end = self.text.iter().rposition(|byte| *byte != b' ').map_or(0, |index| index + 1);
        !self.overflowed && &self.text[..end] == expected.as_bytes()
    }
}

/// Watches a pane's output for its completion sentinel on a line of its own, whatever byte a
/// chunk ends on.
pub(crate) struct SentinelWatch {
    stream: VisibleStream,
    line: LineBuffer,
    sentinel: String,
    found: bool,
}

impl SentinelWatch {
    /// A watch for `sentinel`.
    pub(crate) fn new(sentinel: &str) -> Self {
        Self {
            stream: VisibleStream::new(),
            line: LineBuffer::default(),
            sentinel: sentinel.to_owned(),
            found: false,
        }
    }

    /// Feed one chunk; returns whether the sentinel has arrived.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> bool {
        let Self { stream, line, sentinel, found } = self;
        stream.feed(bytes, |event| match event {
            Visible::Text(byte) => line.push(byte),
            Visible::CarriageReturn => {
                *found |= line.reads(sentinel.as_str());
                line.returned = true;
            }
            Visible::LineFeed | Visible::Move => {
                *found |= line.reads(sentinel.as_str());
                line.clear();
            }
            Visible::Sync(_) => {}
        });
        // A sentinel the chunk ends on counts at once; its line break may come in the next chunk.
        self.found |= self.line.reads(&self.sentinel);
        self.found
    }
}

/// Where S3's count stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CountPhase {
    /// READY has not arrived; nothing is counted.
    AwaitingReady,
    /// Each line feed after READY ends one delivered line.
    Counting,
    /// The sentinel arrived; the count is final.
    Finished,
}

/// Counts the lines delivered between READY and the sentinel, each once, keeping none of them:
/// a line ends at its line feed, so a CR LF split across chunks is one line.
pub(crate) struct LineCounter {
    stream: VisibleStream,
    line: LineBuffer,
    ready: String,
    sentinel: String,
    phase: CountPhase,
    delivered: u64,
}

impl LineCounter {
    /// A counter that starts after the line `ready` and stops at the line `sentinel`.
    pub(crate) fn new(ready: &str, sentinel: &str) -> Self {
        Self {
            stream: VisibleStream::new(),
            line: LineBuffer::default(),
            ready: ready.to_owned(),
            sentinel: sentinel.to_owned(),
            phase: CountPhase::AwaitingReady,
            delivered: 0,
        }
    }

    /// Feed one chunk; returns whether the sentinel has arrived.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> bool {
        let Self { stream, line, ready, sentinel, phase, delivered } = self;
        stream.feed(bytes, |event| match event {
            Visible::Text(byte) => line.push(byte),
            Visible::CarriageReturn => {
                if *phase == CountPhase::Counting && line.reads(sentinel.as_str()) {
                    // When: the sentinel's line is complete, the count ends before its line feed.
                    *phase = CountPhase::Finished;
                }
                line.returned = true;
            }
            Visible::LineFeed | Visible::Move => {
                match *phase {
                    CountPhase::AwaitingReady if line.reads(ready.as_str()) => {
                        *phase = CountPhase::Counting;
                    }
                    CountPhase::Counting if line.reads(sentinel.as_str()) => {
                        *phase = CountPhase::Finished;
                    }
                    // Only a line feed ends a delivered line; a cursor move only starts text elsewhere.
                    CountPhase::Counting if event == Visible::LineFeed => *delivered += 1,
                    _ => {}
                }
                line.clear();
            }
            Visible::Sync(_) => {}
        });
        if self.phase == CountPhase::Counting && self.line.reads(&self.sentinel) {
            // When: the chunk ends on the whole sentinel, its line break may come in the next one.
            self.phase = CountPhase::Finished;
        }
        self.finished()
    }

    /// Lines delivered after READY so far.
    pub(crate) fn delivered(&self) -> u64 {
        self.delivered
    }

    /// Whether the sentinel has arrived.
    pub(crate) fn finished(&self) -> bool {
        self.phase == CountPhase::Finished
    }

    /// The "delivered lines" check: the count matches `planned` and the sentinel arrived.
    pub(crate) fn check(&self, planned: u64) -> DeliveryCheck {
        let mut detail = format!("{} delivered, {planned} planned", self.delivered());
        match self.phase {
            CountPhase::AwaitingReady => detail.push_str("; READY never arrived"),
            CountPhase::Counting => detail.push_str("; the sentinel never arrived"),
            CountPhase::Finished => {}
        }
        let ok = self.finished() && self.delivered() == planned;
        DeliveryCheck::new("delivered lines", ok, detail)
    }
}

/// Finds ConPTY's startup cursor query once, whatever byte a chunk ends on.
pub(crate) struct CursorQuery {
    /// How many bytes of `CURSOR_QUERY` the latest bytes matched.
    matched: usize,
    /// The query was found; it is answered once.
    seen: bool,
}

impl CursorQuery {
    /// A finder that has seen nothing yet.
    pub(crate) fn new() -> Self {
        Self { matched: 0, seen: false }
    }

    /// Feed one chunk; returns true for the chunk that completes the first query, and never again.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> bool {
        if self.seen {
            return false;
        }
        for &byte in bytes {
            if byte == CURSOR_QUERY[self.matched] {
                self.matched += 1;
            } else {
                // When: the match breaks, this byte may still start a new query.
                self.matched = usize::from(byte == CURSOR_QUERY[0]);
            }
            if self.matched == CURSOR_QUERY.len() {
                self.seen = true;
                return true;
            }
        }
        false
    }
}

/// How S10's frames sit relative to the DEC 2026 brackets ConPTY delivered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SyncCounts {
    /// Frames painted inside an open bracket that a close follows.
    pub(crate) enclosed: usize,
    /// Frames painted after an empty open-close pair that came since the previous frame.
    pub(crate) empty_pair_ahead: usize,
    /// Frames painted with no bracket around or just ahead of them.
    pub(crate) absent: usize,
    /// Frames whose marker never arrived: ConPTY painted a later frame over them.
    pub(crate) unseen: usize,
    /// DEC 2026 brackets delivered in all.
    pub(crate) brackets: usize,
}

/// The text each of S10's `count` frames paints and no other frame does: the vim-like half's
/// status line, then the htop-like half's uptime line.
pub(crate) fn frame_markers(count: u32) -> Vec<String> {
    let vim_frames = count / 2;
    (0..count)
        .map(|index| {
            if index < vim_frames {
                format!("line {} of 99999", index + 1)
            } else {
                format!("Uptime frame {index}")
            }
        })
        .collect()
}

/// `marker` split at its number: the text before it, its digits and the text after them.
fn split_marker(marker: &str) -> (&str, &str, &str) {
    let start = marker.find(|character: char| character.is_ascii_digit()).unwrap_or(marker.len());
    let length = marker[start..].bytes().take_while(u8::is_ascii_digit).count();
    (&marker[..start], &marker[start..start + length], &marker[start + length..])
}

/// Every offset in `text` where each marker appears with no digit around its number, in one pass
/// per marker shape, so a frame's number never matches inside a longer one.
fn marker_offsets(text: &[u8], markers: &[String]) -> Vec<Vec<usize>> {
    let parts: Vec<(&str, &str, &str)> =
        markers.iter().map(|marker| split_marker(marker)).collect();
    let index_of: HashMap<(&str, &str, &str), usize> =
        parts.iter().enumerate().map(|(index, part)| (*part, index)).collect();
    let shapes: BTreeSet<(&str, &str)> =
        parts.iter().map(|(prefix, _, suffix)| (*prefix, *suffix)).collect();
    let mut offsets = vec![Vec::new(); markers.len()];
    for (prefix, suffix) in shapes {
        let mut start = 0;
        while let Some(found) = find(&text[start..], prefix.as_bytes()) {
            let at = start + found;
            start = at + 1;
            let digits_start = at + prefix.len();
            let length =
                text[digits_start..].iter().take_while(|byte| byte.is_ascii_digit()).count();
            let after = &text[digits_start + length..];
            let bounded = after.starts_with(suffix.as_bytes())
                && !after.get(suffix.len()).is_some_and(u8::is_ascii_digit);
            let digits = std::str::from_utf8(&text[digits_start..digits_start + length]).ok();
            if let (true, Some(digits)) = (bounded && length > 0, digits) {
                if let Some(&marker) = index_of.get(&(prefix, digits, suffix)) {
                    offsets[marker].push(at);
                }
            }
        }
    }
    offsets
}

/// Where `needle` first appears in `haystack`; `needle` is never empty.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Classify each of `frames`, in play order, by the DEC 2026 brackets around its marker in the
/// text ConPTY delivered: enclosed, an empty pair ahead, absent, or never painted.
pub(crate) fn classify_sync_brackets(bytes: &[u8], frames: &[String]) -> SyncCounts {
    let (text, brackets) = visible_text(bytes);
    let offsets = marker_offsets(&text, frames);
    let mut counts = SyncCounts { brackets: brackets.len(), ..SyncCounts::default() };
    // Frames paint in order, so each one's marker is looked for after the previous frame's.
    let mut cursor = 0;
    let mut previous: Option<usize> = None;
    for frame_offsets in &offsets {
        let next = frame_offsets.partition_point(|offset| *offset < cursor);
        let Some(&position) = frame_offsets.get(next) else {
            counts.unseen += 1;
            continue;
        };
        let split = brackets.partition_point(|(offset, _)| *offset <= position);
        let before = split.checked_sub(1).map(|index| brackets[index]);
        let enclosed =
            matches!(before, Some((_, true))) && matches!(brackets.get(split), Some((_, false)));
        let empty_pair = split >= 2 && {
            let (open_at, open) = brackets[split - 2];
            let (close_at, close) = brackets[split - 1];
            open && !close
                && open_at == close_at
                && previous.is_none_or(|earlier| open_at > earlier)
        };
        if enclosed {
            counts.enclosed += 1;
        } else if empty_pair {
            counts.empty_pair_ahead += 1;
        } else {
            counts.absent += 1;
        }
        previous = Some(position);
        cursor = position + 1;
    }
    counts
}

/// The "sync brackets" check, whose detail states how ConPTY placed the DEC 2026 brackets around
/// S10's frames. Both variants pass only when no frame went unpainted (`unseen` is 0); `sync`
/// accepts any placement, and `default` also requires that no bracket arrived.
pub(crate) fn sync_check(counts: SyncCounts, synchronized: bool) -> DeliveryCheck {
    let mut detail = format!(
        "enclosed {}, empty pair ahead {}, absent {}",
        counts.enclosed, counts.empty_pair_ahead, counts.absent
    );
    if counts.unseen > 0 {
        // When: ConPTY painted over some frames, the detail says how many it never showed.
        detail.push_str(&format!(", never painted {}", counts.unseen));
    }
    // Enclosed, an empty pair ahead and absent are all measured ConPTY placements, so a placement
    // never fails the check; only a frame that was never painted does.
    let ok = counts.unseen == 0 && (synchronized || counts.brackets == 0);
    DeliveryCheck::new("sync brackets", ok, detail)
}

/// The first OSC 1337 sequence's payload in `bytes`: everything after `ESC ] 1337 ;` up to its
/// BEL or ST terminator.
pub(crate) fn osc1337_payload(bytes: &[u8]) -> Result<&[u8], String> {
    let start = find(bytes, OSC1337_PREFIX).ok_or("no OSC 1337 sequence arrived")?;
    let body = &bytes[start + OSC1337_PREFIX.len()..];
    let end = body.iter().enumerate().find_map(|(index, byte)| match byte {
        0x07 => Some(index),
        0x1b if body.get(index + 1) == Some(&b'\\') => Some(index),
        _ => None,
    });
    end.map(|end| &body[..end]).ok_or_else(|| {
        format!("the OSC 1337 sequence never ended; {} bytes arrived after its start", body.len())
    })
}

/// Whether the OSC 1337 payload in `bytes` hashes to `payload_sha256`; the error says what differed.
pub(crate) fn osc1337_intact(bytes: &[u8], payload_sha256: &str) -> Result<(), String> {
    let payload = osc1337_payload(bytes)?;
    let actual = sha256_hex(payload);
    if actual == payload_sha256 {
        return Ok(());
    }
    Err(format!(
        "the OSC 1337 payload's SHA-256 is {actual}, not the fixture's {payload_sha256}; {} bytes arrived",
        payload.len()
    ))
}

/// The `tokens` missing from the text ConPTY delivered in `bytes`, in order.
pub(crate) fn wide_tokens_delivered<'token>(
    bytes: &[u8],
    tokens: &[&'token str],
) -> Vec<&'token str> {
    let (text, _) = visible_text(bytes);
    missing_wide_tokens(&String::from_utf8_lossy(&text), tokens)
}

/// FIPS 180-4 SHA-256 round constants.
const ROUND_CONSTANTS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// The lowercase hex FIPS 180-4 SHA-256 of `bytes`, by hand, so the payload check needs no crate.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let (blocks, remainder) = bytes.as_chunks::<64>();
    for block in blocks {
        compress(&mut state, block);
    }
    // Padding: one 0x80 byte, zeros to 56 bytes past a block boundary, then the bit length.
    let mut tail = remainder.to_vec();
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend_from_slice(&(bytes.len() as u64).wrapping_mul(8).to_be_bytes());
    for block in tail.as_chunks::<64>().0 {
        compress(&mut state, block);
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

/// One SHA-256 compression of `block` into `state`.
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut schedule = [0_u32; 64];
    for (word, word_bytes) in schedule.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*word_bytes);
    }
    for index in 16..64 {
        let early = schedule[index - 15];
        let late = schedule[index - 2];
        let small_zero = early.rotate_right(7) ^ early.rotate_right(18) ^ (early >> 3);
        let small_one = late.rotate_right(17) ^ late.rotate_right(19) ^ (late >> 10);
        schedule[index] = schedule[index - 16]
            .wrapping_add(small_zero)
            .wrapping_add(schedule[index - 7])
            .wrapping_add(small_one);
    }
    let mut work = *state;
    for (constant, word) in ROUND_CONSTANTS.iter().zip(schedule) {
        let [first, second, third, fourth, fifth, sixth, seventh, eighth] = work;
        let big_one = fifth.rotate_right(6) ^ fifth.rotate_right(11) ^ fifth.rotate_right(25);
        let choose = (fifth & sixth) ^ (!fifth & seventh);
        let temp_one = eighth
            .wrapping_add(big_one)
            .wrapping_add(choose)
            .wrapping_add(*constant)
            .wrapping_add(word);
        let big_zero = first.rotate_right(2) ^ first.rotate_right(13) ^ first.rotate_right(22);
        let majority = (first & second) ^ (first & third) ^ (second & third);
        let temp_two = big_zero.wrapping_add(majority);
        work = [
            temp_one.wrapping_add(temp_two),
            first,
            second,
            third,
            fourth.wrapping_add(temp_one),
            fifth,
            sixth,
            seventh,
        ];
    }
    for (state_word, work_word) in state.iter_mut().zip(work) {
        *state_word = state_word.wrapping_add(work_word);
    }
}

/// One check in `delivery.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DeliveryCheck {
    /// What was checked, as the comparison table names it.
    pub(crate) name: String,
    /// Whether the delivery passed it.
    pub(crate) ok: bool,
    /// What was found, shown in the table either way.
    pub(crate) detail: String,
}

impl DeliveryCheck {
    /// A check named `name` with its verdict and detail.
    pub(crate) fn new(name: &str, ok: bool, detail: impl Into<String>) -> Self {
        Self { name: name.to_owned(), ok, detail: detail.into() }
    }
}

/// `delivery.json`, field for field in the order the comparison documents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DeliveryRecord {
    /// Always `DELIVERY_SCHEMA_VERSION`.
    schema_version: u32,
    /// Scenario id.
    pub(crate) scenario: String,
    /// Variant name.
    pub(crate) variant: String,
    /// Output bytes kept for the classifiers; 0 for S3, which only counts.
    pub(crate) bytes_kept: u64,
    /// At least one check per replayed scenario.
    pub(crate) checks: Vec<DeliveryCheck>,
}

impl DeliveryRecord {
    /// A record of `scenario`/`variant`'s replay.
    pub(crate) fn new(
        scenario: &str,
        variant: &str,
        bytes_kept: u64,
        checks: Vec<DeliveryCheck>,
    ) -> Self {
        Self {
            schema_version: DELIVERY_SCHEMA_VERSION,
            scenario: scenario.to_owned(),
            variant: variant.to_owned(),
            bytes_kept,
            checks,
        }
    }

    /// The record as one line of JSON.
    pub(crate) fn to_json(&self) -> String {
        // Strings, booleans and integers always serialize, so this never fails.
        serde_json::to_string(self).expect("a delivery record is plain data")
    }

    /// 0 when there is a check and every check passed; otherwise Blocked's code, which the
    /// comparison requires to agree with the record.
    pub(crate) fn exit_code(&self) -> u8 {
        let passed = !self.checks.is_empty() && self.checks.iter().all(|check| check.ok);
        if passed {
            Status::Valid.exit_code()
        } else {
            Status::Blocked.exit_code()
        }
    }
}

/// Whether the replay keeps `workload`'s output for its classifiers; S3's flood is only counted.
pub(crate) fn keeps_output(workload: Workload) -> bool {
    !matches!(workload, Workload::Flood { .. })
}

/// Exit code when the replay fails before `delivery.json` is written.
#[cfg(windows)]
const REPLAY_ERROR: u8 = 1;
/// How long output is still read after the checks complete, so trailing bytes are classified too.
#[cfg(windows)]
const COMPLETION_GRACE: Duration = Duration::from_millis(500);
/// The longest wait for one output chunk, so the deadline is checked often.
#[cfg(windows)]
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// The answer to ConPTY's startup cursor query: the cursor at row 1, column 1.
#[cfg(windows)]
const CURSOR_REPLY: &[u8] = b"\x1b[1;1R";
/// The replay's pseudoconsole size, fixed so a replay's checks never depend on the grid a measured
/// run's window opens at; at 250 columns no fixture line wraps.
#[cfg(windows)]
const REPLAY_GRID: (u16, u16) = (250, 70);

/// What the replay watches the output for.
#[cfg(windows)]
enum Watch {
    /// S3: the lines between READY and the sentinel, counted and dropped.
    Lines { counter: LineCounter, planned: u64 },
    /// Every other scenario: the output kept up to `DELIVERY_LIMIT_BYTES`, and the sentinel.
    Kept { sentinel: SentinelWatch, kept: Vec<u8>, overflowed: bool },
}

#[cfg(windows)]
impl Watch {
    /// Take one chunk; returns whether the replay's checks have what they need.
    fn feed(&mut self, bytes: &[u8]) -> bool {
        match self {
            Self::Lines { counter, .. } => counter.feed(bytes),
            Self::Kept { sentinel, kept, overflowed } => {
                let room = DELIVERY_LIMIT_BYTES.saturating_sub(kept.len());
                // When: the output outgrows the limit, the rest is dropped and a check fails.
                *overflowed |= bytes.len() > room;
                kept.extend_from_slice(&bytes[..bytes.len().min(room)]);
                sentinel.feed(bytes)
            }
        }
    }

    /// The record of `plan`'s replay; `complete` says whether the sentinel arrived in time.
    fn record(self, plan: &Plan, files: &[FixtureFile], complete: bool) -> DeliveryRecord {
        let (bytes_kept, checks) = match self {
            Self::Lines { counter, planned } => (0, vec![counter.check(planned)]),
            Self::Kept { kept, overflowed, .. } => {
                let mut checks = kept_checks(plan.roles[0], files, &kept);
                if !complete {
                    let detail = "the sentinel did not arrive before the scenario's timeout";
                    checks.push(DeliveryCheck::new("completion", false, detail));
                }
                if overflowed {
                    let detail = format!("the output passed the {DELIVERY_LIMIT_BYTES}-byte limit");
                    checks.push(DeliveryCheck::new("output limit", false, detail));
                }
                (kept.len() as u64, checks)
            }
        };
        DeliveryRecord::new(plan.scenario, plan.variant, bytes_kept, checks)
    }
}

/// The checks for a kept replay of `workload`, from its fixture `files` and the `kept` output.
#[cfg(windows)]
fn kept_checks(workload: Workload, files: &[FixtureFile], kept: &[u8]) -> Vec<DeliveryCheck> {
    match workload {
        Workload::Frames { count, synchronized } => {
            vec![sync_check(classify_sync_brackets(kept, &frame_markers(count)), synchronized)]
        }
        Workload::PrintThenShell(Fixture::EmojiCjk) => {
            let text = String::from_utf8_lossy(fixture_body(files, "emoji-cjk.txt"));
            let tokens = wide_tokens(&text);
            let missing = wide_tokens_delivered(kept, &tokens);
            let detail = if missing.is_empty() {
                format!("all {} delivered", tokens.len())
            } else {
                format!("missing {} of {}: {}", missing.len(), tokens.len(), missing.join(" "))
            };
            vec![DeliveryCheck::new(
                "wide tokens",
                missing.is_empty() && !tokens.is_empty(),
                detail,
            )]
        }
        Workload::PrintThenSleep(Fixture::InlinePng) => {
            let expected = osc1337_payload(fixture_body(files, "inline.osc"))
                .map(sha256_hex)
                .map_err(|reason| format!("the fixture holds no payload to compare: {reason}"));
            let verdict = expected.and_then(|payload_sha256| osc1337_intact(kept, &payload_sha256));
            let ok = verdict.is_ok();
            let detail = verdict.err().unwrap_or_else(|| "intact".to_owned());
            vec![DeliveryCheck::new("OSC 1337 payload", ok, detail)]
        }
        other => {
            vec![DeliveryCheck::new(
                "replayed workload",
                false,
                format!("{other:?} has no delivery check"),
            )]
        }
    }
}

/// The bytes of the single-file fixture `name`; empty when the plan wrote none.
#[cfg(windows)]
fn fixture_body<'files>(files: &'files [FixtureFile], name: &str) -> &'files [u8] {
    let file = files.iter().find(|file| file.relative_path == name);
    match file.map(|file| &file.body) {
        Some(FixtureBody::Bytes(bytes)) => bytes,
        _ => &[],
    }
}

/// The lines S3's role prints between READY and its sentinel: its `y` lines and bulk.txt's lines.
#[cfg(windows)]
fn planned_lines(workload: Workload, files: &[FixtureFile]) -> u64 {
    let Workload::Flood { lines, .. } = workload else {
        return 0;
    };
    let newlines = |bytes: &[u8]| bytes.iter().filter(|byte| **byte == b'\n').count() as u64;
    let bulk = files.iter().find(|file| file.relative_path == "bulk.txt");
    let bulk_lines = bulk.map_or(0, |file| match &file.body {
        FixtureBody::Bytes(bytes) => newlines(bytes),
        FixtureBody::Repeated { block, count } => newlines(block) * *count as u64,
    });
    u64::from(lines) + bulk_lines
}

/// Replay `request`'s delivery into its scratch and write `delivery.json`; returns the exit code:
/// 0 when every check passed, Blocked's 5 when one failed, 2 refused, 1 for an earlier error.
#[cfg(windows)]
pub(crate) fn replay(request: &RunArgs) -> u8 {
    let Some(plan) = crate::scenarios::plan(request.scenario, request.variant, request.short)
    else {
        eprintln!(
            "perf_scenarios: refused: {} has no variant {}",
            request.scenario, request.variant
        );
        return REFUSED;
    };
    let scratch = PathBuf::from(&request.scratch);
    let recorded = capture(&plan, &scratch).and_then(|record| {
        write_record(&scratch, &record)?;
        Ok(record)
    });
    match recorded {
        Ok(record) => {
            print_summary(&record);
            record.exit_code()
        }
        Err(error) => {
            // When: no record was written, the comparison sees exit 1 and a missing delivery.json.
            eprintln!("perf_scenarios: delivery replay failed: {error}");
            REPLAY_ERROR
        }
    }
}

/// Prepare `scratch`, run role 0's program under ConPTY and classify what it delivered.
#[cfg(windows)]
fn capture(plan: &Plan, scratch: &Path) -> Result<DeliveryRecord, String> {
    std::fs::create_dir(scratch)
        .map_err(|error| format!("create {}: {error}", scratch.display()))?;
    // The role program finds the scratch through this variable. No other thread exists yet: the
    // PTY's reader and writer threads start in `read_replay`.
    std::env::set_var(workload::SCRATCH_ENV, scratch);
    let files = workload::fixtures(plan);
    let nonce = prepare(plan, scratch, &files)?;
    let sentinel = workload::sentinel_line(0, &nonce);
    let role = *plan.roles.first().ok_or("the plan has no role")?;
    let mut watch = if keeps_output(role) {
        Watch::Kept { sentinel: SentinelWatch::new(&sentinel), kept: Vec::new(), overflowed: false }
    } else {
        let counter = LineCounter::new(&workload::ready_line(0), &sentinel);
        Watch::Lines { counter, planned: planned_lines(role, &files) }
    };
    let deadline = Instant::now() + Duration::from_secs(plan.timeout_s);
    let complete = read_replay(&mut watch, deadline)?;
    Ok(watch.record(plan, &files, complete))
}

/// Lay the scratch out as a run does, with role 0 acknowledged and released at once, since the
/// replay has no window to wait for; returns the run's nonce.
#[cfg(windows)]
fn prepare(plan: &Plan, scratch: &Path, files: &[FixtureFile]) -> Result<String, String> {
    for dir in ["workload/fixtures", "roles", "sessions", "acks", "go", "done"] {
        std::fs::create_dir_all(scratch.join(dir))
            .map_err(|error| format!("create {dir}: {error}"))?;
    }
    let nonce = workload::choose_nonce(nonce_seed(), files);
    let fixture_root = scratch.join("workload/fixtures");
    for file in files {
        file.write_under(&fixture_root)
            .map_err(|error| format!("write fixture {}: {error}", file.relative_path))?;
    }
    let spec = workload::program_json(plan, &nonce);
    write_atomic(&scratch.join("workload/program.json"), spec.as_bytes())?;
    for marker in ["acks/0", "go/0"] {
        std::fs::write(scratch.join(marker), b"")
            .map_err(|error| format!("write {marker}: {error}"))?;
    }
    Ok(nonce)
}

/// Run this binary in program mode under a 250 x 70 pseudoconsole and feed its output to `watch`
/// until the checks complete, plus a short grace, or `deadline` passes; returns whether they did.
#[cfg(windows)]
fn read_replay(watch: &mut Watch, deadline: Instant) -> Result<bool, String> {
    let harness = std::env::current_exe().map_err(|error| format!("find the harness: {error}"))?;
    let harness = harness.to_str().ok_or("the harness path is not UTF-8")?.to_owned();
    let (cols, rows) = REPLAY_GRID;
    // No arguments and the scratch variable set: the child is a pane's role program.
    let pty = PtyHandle::spawn_with_args(&harness, &[], cols, rows)
        .map_err(|error| format!("start the role program under ConPTY: {error:#}"))?;
    let mut query = CursorQuery::new();
    let mut complete = false;
    let mut stop_at = deadline;
    loop {
        let now = Instant::now();
        if now >= stop_at {
            break;
        }
        match pty.out_rx.recv_timeout(POLL_INTERVAL.min(stop_at - now)) {
            Ok(chunk) => {
                let bytes: &[u8] = chunk.bytes();
                if query.feed(bytes) {
                    // When: ConPTY asks where the cursor is, it paints nothing until answered.
                    pty.send_input_nonblocking(CURSOR_REPLY.to_vec())
                        .map_err(|error| format!("answer the cursor query: {error:?}"))?;
                }
                if watch.feed(bytes) && !complete {
                    complete = true;
                    stop_at = (Instant::now() + COMPLETION_GRACE).min(deadline);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // When: the reader closed, the program and its console are gone; nothing more arrives.
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // An error means the program already exited; dropping the handle closes the pseudoconsole,
    // which ends every process still attached to it, either way.
    let _ = pty.kill();
    drop(pty);
    Ok(complete)
}

/// Write `delivery.json` through a temporary sibling and a rename, so no reader sees part of it.
#[cfg(windows)]
fn write_record(scratch: &Path, record: &DeliveryRecord) -> Result<(), String> {
    write_atomic(&scratch.join(DELIVERY_FILE), (record.to_json() + "\n").as_bytes())
}

/// Write `bytes` to `path` through a `.tmp` sibling and a rename.
#[cfg(windows)]
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    std::fs::write(&temporary, bytes)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|error| format!("write {}: {error}", path.display()))
}

/// A seed for the nonce that differs between replays.
#[cfg(windows)]
fn nonce_seed() -> u64 {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    (since_epoch.as_nanos() as u64) ^ u64::from(std::process::id())
}

/// One line naming the replay's verdict and every failed check.
#[cfg(windows)]
fn print_summary(record: &DeliveryRecord) {
    let failed: Vec<String> = record
        .checks
        .iter()
        .filter(|check| !check.ok)
        .map(|check| format!("{}: {}", check.name, check.detail))
        .collect();
    let verdict =
        if failed.is_empty() { "every check passed".to_owned() } else { failed.join("; ") };
    println!(
        "perf_scenarios {} {} delivery: {verdict} (exit {})",
        record.scenario,
        record.variant,
        record.exit_code()
    );
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod delivery_tests;
