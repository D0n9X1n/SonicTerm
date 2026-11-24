//! Shared real-ConPTY protocol fixture (F-ConPTY): the case model, the streaming sentinel
//! scanner, the outcome classifier and the control-line format.
//!
//! This module is platform-free so its scanner and digest are tested on every host; the
//! Windows drivers that put a real pseudoconsole between a host and a child live in
//! `conpty_fixture_windows.rs`. Only tests reach it, through the `test-support` feature.

#[cfg(windows)]
#[path = "conpty_fixture_windows.rs"]
mod driver;
#[cfg(windows)]
pub use driver::{run, Engine, TransportEngine};

/// The fixture PTY's width: wide enough that no echo report or sentinel line wraps.
pub const PTY_COLS: u16 = 400;
/// The fixture PTY's height before any resize case.
pub const PTY_ROWS: u16 = 40;
/// The prefix of every control line a host writes on its own stdout, never through the PTY.
pub const LINE_PREFIX: &str = "@@fixture ";
/// The first and last bytes of a section kept as evidence; everything else is hashed only.
pub const KEEP_BYTES: usize = 256;
/// The longest section name the scanner accepts between `@@BEGIN ` and `@@`.
const MAX_NAME_BYTES: usize = 96;
const BEGIN: &[u8] = b"@@BEGIN ";
const DONE: &[u8] = b"@@DONE@@";
const ROW_LABEL: &[u8] = b"ROW-";

/// What the host does once the child opens a case's input section.
#[derive(Clone, Debug)]
pub enum InputPhase {
    /// The host writes these bytes into the PTY input; the child reports, as lowercase hex, every
    /// byte its stdin received in the collection window (an engine's own replies included).
    Echo(Vec<u8>),
    /// The child prints rows `ROW-01` to `ROW-<total_rows>` inside the section; 700 ms after it
    /// opens, the host shrinks the PTY to `rows`, so the section holds the console's repaint.
    Resize {
        /// Rows the child prints before the shrink.
        total_rows: u16,
        /// The PTY height after the shrink.
        rows: u16,
    },
}

/// One protocol form: what the child prints, and optionally what the host injects after it.
#[derive(Clone, Debug)]
pub struct Case {
    /// The case name; it labels both of its sections and every control line.
    pub name: &'static str,
    /// Bytes printed before the output section, outside the evidence (a mode the case needs).
    pub preamble: Vec<u8>,
    /// The protocol form the child prints between the `/out` sentinels.
    pub output: Vec<u8>,
    /// Bytes printed after the case, outside the evidence (resetting a mode it set).
    pub trailer: Vec<u8>,
    /// The case's input phase, if it has one.
    pub input: Option<InputPhase>,
    /// Whether the `/out` row must be `exact` on the packaged runtime.
    pub output_required: bool,
    /// Whether the `/in` row must be `exact` on the packaged runtime.
    pub input_required: bool,
}

impl Case {
    /// A recorded case that prints `output`; rows are recorded until marked required.
    pub fn new(name: &'static str, output: impl Into<Vec<u8>>) -> Self {
        Self {
            name,
            preamble: Vec::new(),
            output: output.into(),
            trailer: Vec::new(),
            input: None,
            output_required: false,
            input_required: false,
        }
    }

    /// Require the `/out` row to be `exact` on the packaged runtime.
    pub fn output_required(mut self) -> Self {
        self.output_required = true;
        self
    }

    /// Require the `/in` row to be `exact` on the packaged runtime.
    pub fn input_required(mut self) -> Self {
        self.input_required = true;
        self
    }

    /// Give the case an input phase.
    pub fn input(mut self, phase: InputPhase) -> Self {
        self.input = Some(phase);
        self
    }

    /// Print `bytes` before the output section.
    pub fn preamble(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.preamble = bytes.into();
        self
    }

    /// Print `bytes` after the case.
    pub fn trailer(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.trailer = bytes.into();
        self
    }
}

/// One scanner observation, in stream order.
#[derive(Clone, Debug, PartialEq)]
pub enum ScanEvent {
    /// A section named by its `BEGIN` sentinel opened.
    Opened(String),
    /// The section reached its own `END` sentinel; its evidence is complete.
    Closed(String, Evidence),
    /// Another `BEGIN` or `DONE` arrived before the section's `END`: its evidence is void.
    Abandoned(String),
    /// The child's final sentinel.
    Done,
}

/// What a section held, without a whole-payload buffer: running lengths and digests of the raw
/// bytes and of their text view, the first and last [`KEEP_BYTES`], and the `ROW-` label count.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    /// Raw byte count between the sentinels.
    pub raw_len: usize,
    /// SHA-256 of the raw bytes.
    pub raw_digest: [u8; 32],
    /// Byte count of the text view: escape sequences, C0 controls and DEL removed.
    pub text_len: usize,
    /// SHA-256 of the text view.
    pub text_digest: [u8; 32],
    /// The first raw bytes, at most [`KEEP_BYTES`].
    pub head: Vec<u8>,
    /// The last raw bytes, at most [`KEEP_BYTES`].
    pub tail: Vec<u8>,
    /// Whether every raw byte is CR or LF.
    pub only_line_breaks: bool,
    /// How many `ROW-` labels the text view holds.
    pub row_labels: usize,
}

/// The incremental state of one open section.
struct Section {
    name: String,
    end: Vec<u8>,
    raw: Sha256,
    raw_len: usize,
    text: Sha256,
    text_len: usize,
    filter: TextFilter,
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    only_line_breaks: bool,
    label_progress: usize,
    row_labels: usize,
}

impl Section {
    fn open(name: String) -> Self {
        let end = format!("@@END {name}@@").into_bytes();
        Self {
            name,
            end,
            raw: Sha256::default(),
            raw_len: 0,
            text: Sha256::default(),
            text_len: 0,
            filter: TextFilter::Ground,
            head: Vec::new(),
            tail: std::collections::VecDeque::new(),
            only_line_breaks: true,
            label_progress: 0,
            row_labels: 0,
        }
    }

    /// Account `bytes` to the section: digests, lengths, kept edges and the label count.
    fn push(&mut self, bytes: &[u8]) {
        self.raw.update(bytes);
        self.raw_len += bytes.len();
        let head_room = KEEP_BYTES.saturating_sub(self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..head_room]);
        for &byte in bytes {
            // The tail window stays bounded: at KEEP_BYTES the oldest byte leaves first.
            if self.tail.len() == KEEP_BYTES {
                self.tail.pop_front();
            }
            self.tail.push_back(byte);
            self.only_line_breaks &= byte == b'\r' || byte == b'\n';
            // Only printable text outside any escape sequence enters the text view.
            if self.filter.keeps(byte) {
                self.text.update(&[byte]);
                self.text_len += 1;
                self.count_label(byte);
            }
        }
    }

    /// Advance the `ROW-` matcher by one text byte, carrying a partial label across reads.
    fn count_label(&mut self, byte: u8) {
        if byte == ROW_LABEL[self.label_progress] {
            self.label_progress += 1;
        } else {
            // When: `byte` breaks a partial label, label_progress restarts at this byte.
            self.label_progress = usize::from(byte == ROW_LABEL[0]);
        }
        if self.label_progress == ROW_LABEL.len() {
            self.row_labels += 1;
            self.label_progress = 0;
        }
    }

    fn finish(self) -> (String, Evidence) {
        let evidence = Evidence {
            raw_len: self.raw_len,
            raw_digest: self.raw.finish(),
            text_len: self.text_len,
            text_digest: self.text.finish(),
            head: self.head,
            tail: self.tail.into_iter().collect(),
            only_line_breaks: self.only_line_breaks,
            row_labels: self.row_labels,
        };
        (self.name, evidence)
    }
}

/// The text view's escape-sequence state, carried across reads.
#[derive(Clone, Copy)]
enum TextFilter {
    Ground,
    Escape,
    Csi,
    /// An OSC, DCS, APC, PM or SOS string, ended by BEL or ST.
    String,
    StringEscape,
}

impl TextFilter {
    /// Advance by `byte` and report whether it is printable text in the ground state.
    fn keeps(&mut self, byte: u8) -> bool {
        let (next, kept) = match (*self, byte) {
            (Self::Ground, 0x1b) => (Self::Escape, false),
            (Self::Ground, byte) => (Self::Ground, (0x20..0x7f).contains(&byte) || byte >= 0x80),
            (Self::Escape, b'[') => (Self::Csi, false),
            (Self::Escape, b']' | b'P' | b'_' | b'^' | b'X') => (Self::String, false),
            (Self::Escape, _) => (Self::Ground, false),
            (Self::Csi, 0x40..=0x7e) => (Self::Ground, false),
            (Self::Csi, _) => (Self::Csi, false),
            (Self::String, 0x07) => (Self::Ground, false),
            (Self::String, 0x1b) => (Self::StringEscape, false),
            (Self::String, _) => (Self::String, false),
            (Self::StringEscape, b'\\') => (Self::Ground, false),
            (Self::StringEscape, _) => (Self::String, false),
        };
        *self = next;
        kept
    }
}

/// Finds `@@BEGIN name@@` … `@@END name@@` sections and `@@DONE@@` in a byte stream read in
/// arbitrary pieces. It holds back only the bytes a split sentinel could need.
#[derive(Default)]
pub struct Scanner {
    pending: Vec<u8>,
    open: Option<Section>,
}

impl Scanner {
    /// Consume `bytes` and return the events they complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<ScanEvent> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        loop {
            let progressed = match self.open.take() {
                Some(section) => self.scan_open(section, &mut events),
                None => self.scan_closed(&mut events),
            };
            if !progressed {
                // When: nothing `progressed`, no sentinel completes yet; wait for the next read.
                return events;
            }
        }
    }

    /// Look for the next `BEGIN` or `DONE`; return whether one was consumed.
    fn scan_closed(&mut self, events: &mut Vec<ScanEvent>) -> bool {
        let begin = find(&self.pending, BEGIN);
        let done = find(&self.pending, DONE);
        if let Some(done_at) =
            done.filter(|&done_at| begin.is_none_or(|begin_at| done_at < begin_at))
        {
            // When: `done_at` precedes any BEGIN, the child finished; what follows is not evidence.
            self.pending.drain(..done_at + DONE.len());
            events.push(ScanEvent::Done);
            return true;
        }
        let Some(begin_at) = begin else {
            // When: no `begin` is present, keep only a tail long enough to complete a split one.
            let keep = self.pending.len().min(BEGIN.len() + MAX_NAME_BYTES);
            self.pending.drain(..self.pending.len() - keep);
            return false;
        };
        let name_start = begin_at + BEGIN.len();
        let Some(name_len) = find(&self.pending[name_start..], b"@@") else {
            // When: `find` sees no `@@` after the name yet, it is cut off or was no sentinel.
            if self.pending.len() - name_start > MAX_NAME_BYTES {
                // When: past MAX_NAME_BYTES with no closing `@@`, this was not a sentinel.
                self.pending.drain(..name_start);
                return true;
            }
            self.pending.drain(..begin_at);
            return false;
        };
        if name_len > MAX_NAME_BYTES {
            // When: `name_len` exceeds any case name, this was not a sentinel.
            self.pending.drain(..name_start);
            return true;
        }
        let name = String::from_utf8_lossy(&self.pending[name_start..name_start + name_len]);
        let section = Section::open(name.into_owned());
        self.pending.drain(..name_start + name_len + 2);
        events.push(ScanEvent::Opened(section.name.clone()));
        self.open = Some(section);
        true
    }

    /// Feed the open section up to its `END`, or abandon it at an earlier `BEGIN` or `DONE`;
    /// return whether the section ended.
    fn scan_open(&mut self, mut section: Section, events: &mut Vec<ScanEvent>) -> bool {
        let end = find(&self.pending, &section.end);
        let interrupt =
            [find(&self.pending, BEGIN), find(&self.pending, DONE)].into_iter().flatten().min();
        let closes = match (end, interrupt) {
            (Some(end_at), Some(interrupt_at)) => end_at <= interrupt_at,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if closes {
            // When: the section `closes` at its own END, its evidence is complete.
            let end_at = end.unwrap_or_default();
            section.push(&self.pending[..end_at]);
            self.pending.drain(..end_at + section.end.len());
            let (name, evidence) = section.finish();
            events.push(ScanEvent::Closed(name, evidence));
            return true;
        }
        if let Some(interrupt_at) = interrupt {
            // When: an `interrupt` sentinel precedes this section's END, the END was lost.
            self.pending.drain(..interrupt_at);
            events.push(ScanEvent::Abandoned(section.name));
            return true;
        }
        let hold = section.end.len().max(BEGIN.len()).max(DONE.len()) - 1;
        let settled = self.pending.len().saturating_sub(hold);
        section.push(&self.pending[..settled]);
        self.pending.drain(..settled);
        self.open = Some(section);
        false
    }
}

/// The first index of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// A section's expected form: its length, digest and whether it is printable text.
#[derive(Clone, Debug)]
pub struct Expected {
    len: usize,
    digest: [u8; 32],
    printable: bool,
}

impl Expected {
    /// The expectation that a section holds exactly `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            len: bytes.len(),
            digest: Sha256::digest(bytes),
            printable: bytes.iter().all(|byte| (0x20..0x7f).contains(byte)),
        }
    }
}

/// A row's outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The section's digest equals the expected form's.
    Exact,
    /// Both sentinels arrived and other bytes than the expected form, or a changed prefix.
    Transformed,
    /// Both sentinels arrived with nothing but CR/LF between them.
    Dropped,
    /// The END sentinel was missing, the positive control failed, or the launch failed.
    Inconclusive,
}

impl Outcome {
    /// The outcome's control-line spelling.
    pub fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Transformed => "transformed",
            Self::Dropped => "dropped",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// Classify a section against `expected`. A printable expectation (the positive control, an
/// echo's hex report) compares the text view, so a console's cursor decoration around printable
/// text cannot change it; a protocol form compares the raw bytes, so any rewrite is visible.
pub fn classify(evidence: Option<&Evidence>, expected: &Expected) -> Outcome {
    let Some(evidence) = evidence else {
        // When: no `evidence` exists, the section's END never arrived.
        return Outcome::Inconclusive;
    };
    let (len, digest, empty) = if expected.printable {
        (evidence.text_len, evidence.text_digest, evidence.text_len == 0)
    } else {
        // When: the form is not `printable`, any rewrite of a raw byte must show.
        (evidence.raw_len, evidence.raw_digest, evidence.only_line_breaks)
    };
    if len == expected.len && digest == expected.digest {
        Outcome::Exact
    } else if empty {
        // When: the section is `empty` of everything but line breaks, the form was dropped.
        Outcome::Dropped
    } else {
        // When: other bytes than the form arrived, it was transformed, never `empty`-dropped.
        Outcome::Transformed
    }
}

/// Classify a repaint section, which has no exact form: no repainted text is `dropped`, and any
/// repainted text is `transformed` with its label count recorded beside it.
pub fn classify_repaint(evidence: Option<&Evidence>) -> Outcome {
    match evidence {
        None => Outcome::Inconclusive,
        Some(evidence) if evidence.text_len == 0 => Outcome::Dropped,
        Some(_) => Outcome::Transformed,
    }
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A JSON string literal for `text`.
fn json_string(text: &str) -> String {
    let mut quoted = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            control if u32::from(control) < 0x20 => {
                quoted.push_str(&format!("\\u{:04x}", u32::from(control)));
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// The control line for one row: its outcome, requirement and bounded evidence.
pub fn row_line(
    case: &str,
    phase: &str,
    required: bool,
    outcome: Outcome,
    evidence: Option<&Evidence>,
) -> String {
    let (len, head, tail, labels) = evidence.map_or((0, String::new(), String::new(), 0), |seen| {
        (seen.raw_len, hex(&seen.head), hex(&seen.tail), seen.row_labels)
    });
    format!(
        "{LINE_PREFIX}{{\"kind\":\"row\",\"case\":{},\"phase\":{},\"required\":{required},\"outcome\":\"{}\",\"len\":{len},\"row_labels\":{labels},\"head\":\"{head}\",\"tail\":\"{tail}\"}}",
        json_string(case),
        json_string(phase),
        outcome.label(),
    )
}

/// Read `key` from one flat JSON control line: a string's unescaped text, or a bare value.
pub fn json_field(line: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":");
    let rest = &line[line.find(&marker)? + marker.len()..];
    let Some(quoted) = rest.strip_prefix('"') else {
        // When: no `quoted` value follows, it is a number or boolean ending at `,` or `}`.
        let end = rest.find([',', '}'])?;
        return Some(rest[..end].to_owned());
    };
    let mut text = String::new();
    let mut characters = quoted.chars();
    while let Some(character) = characters.next() {
        match character {
            '"' => {
                // When: the `character` is the closing quote, the string value is complete.
                return Some(text);
            }
            '\\' => match characters.next()? {
                'u' => {
                    let code: String = characters.by_ref().take(4).collect();
                    text.push(char::from_u32(u32::from_str_radix(&code, 16).ok()?)?);
                }
                escaped => text.push(escaped),
            },
            other => text.push(other),
        }
    }
    None
}

/// Streaming SHA-256 (FIPS 180-4), so a section is hashed as it arrives on any host.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    total_bytes: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self {
            state: [
                0x6a09_e667,
                0xbb67_ae85,
                0x3c6e_f372,
                0xa54f_f53a,
                0x510e_527f,
                0x9b05_688c,
                0x1f83_d9ab,
                0x5be0_cd19,
            ],
            block: [0; 64],
            filled: 0,
            total_bytes: 0,
        }
    }
}

impl Sha256 {
    /// The digest of `bytes`.
    pub fn digest(bytes: &[u8]) -> [u8; 32] {
        let mut hasher = Self::default();
        hasher.update(bytes);
        hasher.finish()
    }

    /// Absorb `bytes`.
    pub fn update(&mut self, mut bytes: &[u8]) {
        self.total_bytes += bytes.len() as u64;
        while !bytes.is_empty() {
            let take = (64 - self.filled).min(bytes.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled == 64 {
                self.compress();
                self.filled = 0;
            }
        }
    }

    /// Pad, compress the final blocks and return the digest.
    pub fn finish(mut self) -> [u8; 32] {
        let bit_len = self.total_bytes.wrapping_mul(8);
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        self.update(&bit_len.to_be_bytes());
        let mut digest = [0; 32];
        for (out, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *out = word.to_be_bytes();
        }
        digest
    }

    fn compress(&mut self) {
        let mut schedule = [0u32; 64];
        for (word, bytes) in schedule.iter_mut().zip(self.block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for index in 16..64 {
            let early = schedule[index - 15];
            let late = schedule[index - 2];
            let sigma0 = early.rotate_right(7) ^ early.rotate_right(18) ^ (early >> 3);
            let sigma1 = late.rotate_right(17) ^ late.rotate_right(19) ^ (late >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(sigma0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(sigma1);
        }
        let mut work = self.state;
        for (round_constant, word) in ROUND_CONSTANTS.iter().zip(schedule) {
            let [word_a, word_b, word_c, word_d, word_e, word_f, word_g, word_h] = work;
            let big_sigma1 =
                word_e.rotate_right(6) ^ word_e.rotate_right(11) ^ word_e.rotate_right(25);
            let choose = (word_e & word_f) ^ (!word_e & word_g);
            let temp1 = word_h
                .wrapping_add(big_sigma1)
                .wrapping_add(choose)
                .wrapping_add(*round_constant)
                .wrapping_add(word);
            let big_sigma0 =
                word_a.rotate_right(2) ^ word_a.rotate_right(13) ^ word_a.rotate_right(22);
            let majority = (word_a & word_b) ^ (word_a & word_c) ^ (word_b & word_c);
            let temp2 = big_sigma0.wrapping_add(majority);
            work = [
                temp1.wrapping_add(temp2),
                word_a,
                word_b,
                word_c,
                word_d.wrapping_add(temp1),
                word_e,
                word_f,
                word_g,
            ];
        }
        for (state, worked) in self.state.iter_mut().zip(work) {
            *state = state.wrapping_add(worked);
        }
    }
}

/// The first 32 bits of the fractional parts of the cube roots of the first 64 primes.
const ROUND_CONSTANTS: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

#[cfg(test)]
#[path = "conpty_fixture_tests.rs"]
mod conpty_fixture_tests;
