//! VT/ANSI parser. We delegate the lexer to the `vte` crate (the same
//! implementation alacritty uses) and translate parsed events into mutations
//! on a [`sonicterm_grid::grid::Grid`].
//!
//! The supported subset (v0.1.0):
//! - Printable ASCII + UTF-8
//! - C0 controls: BEL, BS, HT, LF, CR
//! - CSI: `H`/`f` (CUP), `A`/`B`/`C`/`D` (cursor motion), `J` (ED), `K` (EL),
//!   `m` (SGR — bold/italic/underline/inverse/reset + 30..37, 40..47, 90..97,
//!   100..107, 38;5;n / 48;5;n, 38;2;r;g;b / 48;2;r;g;b)
//! - OSC: `0`/`2` (window title), `8` (hyperlink), `52` (clipboard — stub),
//!   `1337;File=...` (iTerm2 inline media metadata/payload event)
//! - DCS/APC media capture: Sixel (`DCS ... q`) and Kitty graphics (`APC G...`)
//!
//! Out of scope: media texture decoding/rendering and most mouse tracking.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_channel::Sender;
use vte::{Params, Perform};

use sonicterm_grid::grid::{Cell, CellFlags, Color, Grid, Pos, UnderlineStyle};
use sonicterm_grid::hyperlink::{HyperlinkId, HyperlinkRegistry, MAX_HYPERLINK_CLIENT_ID_BYTES};
// Governor accounting type, consumed rather than republished.
use sonicterm_types::ResourceAmount;

/// Version string reported in answer to CSI > q (XTVERSION).
pub const SONIC_VERSION: &str = "SonicTerm 0.7";

/// Largest staging buffer a single capture may hold when it is the only one
/// in flight.
///
/// This is what a lone pane receiving a large image gets, unchanged: the
/// common case is one capture at a time, and it is not the case that needs
/// constraining.
/// Public so the app can derive the governor's per-pane backstop from the caps
/// the seams actually enforce, rather than restating a number that would then
/// need to be kept in agreement with this one.
pub const MAX_MEDIA_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Ceiling on in-flight capture staging summed over every parser in this
/// process.
///
/// A capture is *staging*, not retained: it exists only between an
/// APC/DCS introducer and its terminator, and the bytes are handed to the host
/// as a `MediaEvent` the moment the sequence completes. What makes it worth
/// bounding is that the terminator is not guaranteed to arrive. A capture
/// whose stream stalls — `imgcat` over a dropped SSH link, a program killed
/// mid-transfer — pins its buffer until the pane dies, and no eviction pass
/// can reclaim it, because the parser cannot distinguish a stalled transfer
/// from a slow one.
///
/// Per-parser the buffer is bounded. Composed across panes it was not:
/// 20 panes each mid-capture measured 320 MiB, every parser individually
/// compliant. That composition is the shape this ceiling exists to close.
///
/// This is a real ceiling, not a target: staging is handed out from two fixed
/// pools that sum to exactly this figure, so the total cannot exceed it at any
/// number of panes. A per-capture share with a floor cannot make that promise
/// — past the point where the floor wins the clamp, the sum is the floor times
/// the number of panes, and nothing bounds the number of panes.
///
/// Public so the bound can be measured against real heap from outside the
/// crate. A ceiling checked only against the arithmetic it was derived from is
/// how the composition above passed review in the first place.
pub const MAX_PROCESS_CAPTURE_STAGING_BYTES: usize = 64 * 1024 * 1024;

/// Smallest staging budget an admitted capture is ever given.
///
/// A fair share alone shrinks toward zero as captures multiply, and a capture
/// truncated below the size of a typical encoded image renders a broken
/// picture — the outcome this floor exists to prevent. Sized to hold a
/// representative PNG/JPEG payload whole so that an admitted pane can always
/// complete an ordinary image.
///
/// Public so the guarantee can be asserted from outside the crate: the floor
/// is the promise made to an admitted pane, and a bound that held by quietly
/// withdrawing it would be a regression dressed as a fix.
pub const MIN_CAPTURE_STAGING_BYTES: usize = 4 * 1024 * 1024;

/// Staging reserved for growth beyond the floor.
///
/// Exactly what one capture needs to climb from the floor to the per-capture
/// maximum, so a lone pane receiving a large image still gets all 16 MiB of it
/// — the common case, and not the one that needs constraining. Held apart from
/// the floor pool so that a capture growing large cannot consume the floors
/// other panes are guaranteed.
const CAPTURE_GROWTH_POOL_BYTES: usize = MAX_MEDIA_PAYLOAD_BYTES - MIN_CAPTURE_STAGING_BYTES;

/// Staging reserved for the floors of concurrent captures.
const CAPTURE_FLOOR_POOL_BYTES: usize =
    MAX_PROCESS_CAPTURE_STAGING_BYTES - CAPTURE_GROWTH_POOL_BYTES;

/// How many panes can hold an ordinary image whole at the same time.
///
/// The honest form of the promise the floor makes. The old formulation —
/// every pane, however many are active, gets at least the floor — is not
/// something a fixed ceiling can promise, because panes are not bounded:
/// nothing in the workspace caps tab or split count, so "every pane" is
/// unbounded and `N × floor` has no maximum.
///
/// Derived rather than chosen, so it cannot drift from the pools it describes.
/// Raising it means raising the ceiling; the arithmetic is the trade, stated.
pub const GUARANTEED_CONCURRENT_CAPTURES: usize =
    CAPTURE_FLOOR_POOL_BYTES / MIN_CAPTURE_STAGING_BYTES;

/// Floor bytes currently reserved by live captures.
static CAPTURE_FLOOR_RESERVED: AtomicUsize = AtomicUsize::new(0);

/// Growth bytes currently reserved by live captures.
static CAPTURE_GROWTH_RESERVED: AtomicUsize = AtomicUsize::new(0);

/// Captures currently accumulating across every parser in this process.
static LIVE_MEDIA_CAPTURES: AtomicUsize = AtomicUsize::new(0);

/// Take `want` bytes from `pool` if `capacity` has them, all or nothing.
///
/// All-or-nothing because a partial grant would put a capture's buffer at a
/// size that is not a power of two, and `Vec` growth rounds up: a 6 MiB budget
/// is held in an 8 MiB allocation, so the pool would be handing out bytes the
/// allocator does not honour and the ceiling would fail by the rounding.
fn reserve_from(pool: &AtomicUsize, capacity: usize, want: usize) -> bool {
    let mut reserved = pool.load(Ordering::Relaxed);
    loop {
        if want > capacity.saturating_sub(reserved) {
            return false;
        }
        match pool.compare_exchange_weak(
            reserved,
            reserved + want,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(current) => reserved = current,
        }
    }
}

/// One capture's claim on the process staging pools.
///
/// A separate guard rather than `impl Drop for MediaCapture` because
/// `into_event`/`into_kitty_event` move fields out of the capture, which a
/// `Drop` impl on the capture itself would forbid. As a field, it is dropped
/// by those destructurings exactly as it is by an explicit `= None`, so every
/// release path returns its bytes without any of them naming the pools.
#[derive(Debug)]
struct StagingReservation {
    floor: usize,
    growth: usize,
}

impl StagingReservation {
    /// Admit a capture if the floor pool can still guarantee it an ordinary
    /// image, otherwise refuse it.
    ///
    /// Refusing rather than admitting at a reduced size is what makes the
    /// ceiling hold. It is also the better rendering outcome: a capture
    /// truncated below a whole image decodes to nothing for Kitty and iTerm2,
    /// and for Sixel to a silently cut-off picture, which is the broken
    /// picture the floor exists to prevent rather than an approximation of the
    /// image the user asked for.
    fn admit() -> Self {
        LIVE_MEDIA_CAPTURES.fetch_add(1, Ordering::Relaxed);
        let floor = if reserve_from(
            &CAPTURE_FLOOR_RESERVED,
            CAPTURE_FLOOR_POOL_BYTES,
            MIN_CAPTURE_STAGING_BYTES,
        ) {
            MIN_CAPTURE_STAGING_BYTES
        } else {
            tracing::warn!(
                guaranteed = GUARANTEED_CONCURRENT_CAPTURES,
                "media capture refused: staging pool is fully committed to captures \
                     already in flight"
            );
            0
        };
        Self { floor, growth: 0 }
    }

    /// Whether this capture was given staging at all.
    fn admitted(&self) -> bool {
        self.floor > 0
    }

    /// Bytes this capture may hold.
    fn budget(&self) -> usize {
        self.floor + self.growth
    }

    /// Double the budget out of the growth pool.
    ///
    /// Doubling rather than a fixed block so every budget stays a power of
    /// two. `Vec` grows by doubling, so a power-of-two budget is held in an
    /// allocation of exactly that size and the reservation matches the heap;
    /// any other budget would be rounded up by the allocator into bytes the
    /// pool never granted.
    fn try_double(&mut self) -> bool {
        let current = self.budget();
        if current == 0 || current >= MAX_MEDIA_PAYLOAD_BYTES {
            return false;
        }
        if !reserve_from(&CAPTURE_GROWTH_RESERVED, CAPTURE_GROWTH_POOL_BYTES, current) {
            return false;
        }
        self.growth += current;
        true
    }
}

impl Clone for StagingReservation {
    /// A cloned capture is a second live capture, so it makes its own claim
    /// rather than duplicating one the pools only granted once.
    fn clone(&self) -> Self {
        Self::admit()
    }
}

impl Drop for StagingReservation {
    fn drop(&mut self) {
        LIVE_MEDIA_CAPTURES.fetch_sub(1, Ordering::Relaxed);
        CAPTURE_FLOOR_RESERVED.fetch_sub(self.floor, Ordering::Relaxed);
        CAPTURE_GROWTH_RESERVED.fetch_sub(self.growth, Ordering::Relaxed);
    }
}
/// Rejected OSC 8 links to skip after a sweep that freed nothing.
///
/// A sweep frees nothing only when every interned link is still on screen, in
/// which case the next link is no more likely to find garbage. Waiting this
/// many rejections bounds the scan to roughly one per that many links instead
/// of one per link, while still recovering as soon as content scrolls away.
const HYPERLINK_RECLAIM_BACKOFF_LINKS: u32 = 256;
const MAX_RAW_OSC4_BYTES: usize = 4096;
/// Public for the same reason as [`MAX_MEDIA_PAYLOAD_BYTES`]: the backstop is
/// derived from the caps, not parallel to them.
pub const MAX_ESCAPE_SEQUENCE_BYTES: usize = 1024 * 1024;

/// Event surfaced to the host so it can update window chrome, clipboard, etc.
#[derive(Debug, Clone)]
pub enum VtEvent {
    /// OSC 133 — shell integration command lifecycle marker.
    Command(CommandEvent),
    /// OSC 0/2 — shell asked the terminal to update the window title.
    SetTitle(String),
    /// BEL (0x07) — audible/visual bell request from the shell.
    Bell,
    /// OSC 8 — enter (or exit, when `uri` is empty) a hyperlink span; cells
    /// emitted while active carry the interned id so the renderer can underline
    /// them and the input layer can resolve clicks back to a URI.
    Hyperlink {
        /// Optional `id=…` parameter so multiple discontiguous runs can share
        /// one logical link target.
        id: Option<String>,
        /// The target URI; empty string terminates the currently-active link.
        uri: String,
    },
    /// OSC 52 — shell requested clipboard read/write on the named selection.
    Clipboard {
        /// Selection target byte (`c` = clipboard, `p` = primary, etc.).
        selection: char,
        /// Base64-encoded payload as received from the shell.
        data: String,
    },
    /// Inline media protocol payload captured from the stream.
    ///
    /// SonicTerm surfaces this as typed data instead of silently discarding the
    /// escape sequence. Decoding/uploading it into the renderer is handled by
    /// higher layers.
    Media(MediaEvent),
    /// DEC private mode ?25 — host should show/hide the cursor.
    CursorVisibility(bool),
}

#[derive(Debug, Clone)]
struct MediaCapture {
    protocol: MediaProtocol,
    metadata: String,
    data: Vec<u8>,
    truncated: bool,
    pending_esc: bool,
    /// Every byte offered to this capture, including those refused after the
    /// budget was reached.
    ///
    /// Distinct from `data.len()`, which stops advancing once the capture is
    /// full. A host watching for a stalled transfer needs to see that bytes
    /// are still arriving even when none of them are being kept.
    seen: usize,
    /// Set once the growth pool has refused this capture, so a full capture
    /// stops asking on every subsequent byte.
    growth_exhausted: bool,
    /// This capture's claim on the process staging pools, released when the
    /// capture is dropped or destructured into an event.
    reservation: StagingReservation,
}

impl MediaCapture {
    fn new(protocol: MediaProtocol, metadata: String) -> Self {
        Self {
            protocol,
            metadata,
            data: Vec::new(),
            truncated: false,
            pending_esc: false,
            seen: 0,
            growth_exhausted: false,
            reservation: StagingReservation::admit(),
        }
    }

    /// Bytes this capture is holding, counting reserved capacity.
    ///
    /// Capacity rather than length because that is what the allocator is
    /// actually holding: a capture that grew and was truncated still owns the
    /// larger buffer.
    fn retained_bytes(&self) -> usize {
        self.data.capacity().saturating_add(self.metadata.capacity())
    }

    fn append_byte(&mut self, byte: u8) {
        self.seen = self.seen.saturating_add(1);

        if self.data.len() < self.reservation.budget() {
            self.data.push(byte);
            return;
        }

        // Full. Ask the growth pool for a larger budget, once: a capture that
        // has been refused keeps receiving bytes, and retrying per byte would
        // put a contended atomic on the hot path of a transfer that is already
        // known to be capped.
        if !self.growth_exhausted && self.reservation.try_double() {
            self.data.push(byte);
            return;
        }

        self.growth_exhausted = true;
        self.truncated = true;
    }

    /// Whether this capture was admitted to the staging pools.
    ///
    /// A refused capture holds no staging and has kept no bytes, so it has
    /// nothing to dispatch.
    fn admitted(&self) -> bool {
        self.reservation.admitted()
    }

    /// Whether this capture holds the whole payload it was offered.
    fn whole(&self) -> bool {
        self.admitted() && !self.truncated
    }

    /// Dispatch the payload, unless it is not the whole picture.
    ///
    /// A capture that was refused, or admitted and then cut, is dropped rather
    /// than surfaced, for the reason [`Parser::cancel_capture`] gives for
    /// discarding a partial one: a fragment of an image decodes to nothing
    /// useful, so surfacing it would trade memory for a broken picture instead
    /// of no picture.
    ///
    /// For Sixel the distinction is load-bearing rather than theoretical. Its
    /// decoder paints whatever bytes it is given and reports the bounding box
    /// of what it painted, so a cut payload decodes to a real image containing
    /// the top fraction of the picture — byte-identical to the whole one for
    /// as far as it goes, with nothing in it marking it incomplete. A user
    /// cannot tell that image from a complete one that happens to be shorter.
    fn into_event(self, row: u16, col: u16) -> Option<MediaEvent> {
        if !self.whole() {
            return None;
        }
        Some(MediaEvent {
            protocol: self.protocol,
            row,
            col,
            metadata: self.metadata,
            data: self.data,
        })
    }

    fn into_kitty_event(mut self, row: u16, col: u16) -> Option<MediaEvent> {
        if !self.admitted() {
            return None;
        }
        if self.pending_esc {
            self.append_byte(0x1b);
            self.pending_esc = false;
        }
        if !self.whole() {
            return None;
        }
        if self.data.first().copied() != Some(b'G') {
            return None;
        }
        let payload = &self.data[1..];
        let (metadata, data) = split_once_byte(payload, b';')
            .map(|(m, d)| (String::from_utf8_lossy(m).into_owned(), d.to_vec()))
            .unwrap_or_else(|| (String::new(), payload.to_vec()));
        Some(MediaEvent { protocol: MediaProtocol::Kitty, row, col, metadata, data })
    }
}

/// Inline-media escape protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaProtocol {
    /// Sixel graphics payload from `DCS ... q`.
    Sixel,
    /// iTerm2 `OSC 1337 ; File=... : <base64>` inline file/image payload.
    Iterm2File,
    /// Kitty graphics payload from `APC G...`.
    Kitty,
}

/// Captured media payload plus protocol metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaEvent {
    /// Protocol that produced this payload.
    pub protocol: MediaProtocol,
    /// Cursor row when the media sequence completed.
    pub row: u16,
    /// Cursor column when the media sequence completed.
    pub col: u16,
    /// Protocol-specific metadata before the binary/base64 payload. For Kitty
    /// this is the comma-separated control section; for iTerm2 this is the
    /// `File=...` attribute section; for Sixel this is currently empty.
    pub metadata: String,
    /// Raw protocol payload bytes, capped at 16 MiB to keep untrusted PTY output
    /// from growing memory without bound.
    ///
    /// Always the whole payload the sequence carried. A payload that could not
    /// be staged whole is not dispatched at all — see
    /// [`Parser::cancel_capture`] for why a fragment is worse than nothing —
    /// so a consumer never has to ask whether what it received is complete.
    pub data: Vec<u8>,
}

/// Command lifecycle events surfaced from OSC 133 shell-integration markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandEvent {
    /// Prompt started (`OSC 133 ; A`).
    PromptStart,
    /// Command started (`OSC 133 ; B` or `OSC 133 ; C`).
    CmdStart,
    /// Command ended, optionally with an exit code (`OSC 133 ; D ; <code>`).
    CmdEnd(Option<u8>),
}

/// Streaming parser wrapping `vte::Parser` and a [`Performer`] that owns the
/// grid + current SGR attributes.
pub struct Parser {
    inner: vte::Parser,
    performer: Performer,
    apc_capture: Option<MediaCapture>,
    pending_esc: bool,
    raw_osc: Option<RawOsc>,
    escape_bytes_in_flight: usize,
    discarding_oversized_escape: bool,
    discard_escape_pending_esc: bool,
    /// Let a newline end the discard, used only when the discard was started
    /// by [`Parser::cancel_capture`].
    ///
    /// A cancelled transfer's sender is never told, so its tail keeps
    /// arriving and has to be swallowed. But a sender that *died* rather than
    /// stalled sends no terminator at all, and an unbounded discard would eat
    /// the shell's next prompt forever. Media payloads carry no newline —
    /// base64 has none, and Sixel breaks lines with `-` — while shell output
    /// is full of them, so a newline is the signal that the bytes now arriving
    /// belong to the user rather than to the abandoned transfer.
    discard_exits_on_newline: bool,
    escape_family: EscapeFamily,
}

/// SonicTerm-side OSC capture for sequences where vte's public callback loses
/// information before dispatch. vte 0.15 stores the full OSC in a private
/// buffer, but `Perform::osc_dispatch` exposes only up to MAX_OSC_PARAMS split
/// params; OSC 4 needs the raw `index;?` stream for 16-colour batch queries.
enum RawOsc {
    /// We have consumed `ESC ]` and are checking whether the command is `4`.
    Probe { saw_four: bool },
    /// Capturing bytes after `OSC 4 ;` until BEL or ST.
    Palette { content: Vec<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeFamily {
    Ground,
    Esc,
    Csi,
    Osc,
    String,
}

impl Parser {
    /// Build a parser bound to `grid`, with no upstream reply channel — DSR /
    /// XTVERSION queries will be silently dropped.
    pub fn new(grid: Grid) -> Self {
        Self {
            inner: vte::Parser::new(),
            performer: Performer::new(grid, None),
            apc_capture: None,
            pending_esc: false,
            raw_osc: None,
            escape_bytes_in_flight: 0,
            discarding_oversized_escape: false,
            discard_escape_pending_esc: false,
            discard_exits_on_newline: false,
            escape_family: EscapeFamily::Ground,
        }
    }

    /// Construct a parser that can send replies (DSR, DA, XTVERSION, focus
    /// reporting) back to the pty via the given channel.
    pub fn new_with_reply(grid: Grid, reply_tx: Sender<Vec<u8>>) -> Self {
        Self {
            inner: vte::Parser::new(),
            performer: Performer::new(grid, Some(reply_tx)),
            apc_capture: None,
            pending_esc: false,
            raw_osc: None,
            escape_bytes_in_flight: 0,
            discarding_oversized_escape: false,
            discard_escape_pending_esc: false,
            discard_exits_on_newline: false,
            escape_family: EscapeFamily::Ground,
        }
    }

    /// Tell the parser the theme default foreground colour. Used to answer
    /// `OSC 10 ; ? ST` queries from the shell/TUI. nvim sends OSC 10/11
    /// at startup to learn the terminal's defaults so it can render cells
    /// declared with `fg=NONE`/`bg=NONE` consistently.
    pub fn set_theme_fg(&mut self, r: u8, g: u8, b: u8) {
        self.performer.theme_fg = Some((r, g, b));
    }

    /// Tell the parser the theme default background colour. Used to answer
    /// `OSC 11 ; ? ST` queries (see [`Parser::set_theme_fg`]).
    pub fn set_theme_bg(&mut self, r: u8, g: u8, b: u8) {
        self.performer.theme_bg = Some((r, g, b));
    }

    /// Tell the parser the theme cursor colour. Used to answer
    /// `OSC 12 ; ? ST` queries. When unset, OSC 12 falls back to the
    /// theme foreground.
    pub fn set_theme_cursor(&mut self, r: u8, g: u8, b: u8) {
        self.performer.theme_cursor = Some((r, g, b));
    }

    /// Seed one slot (0..=15) of the 16-colour ANSI palette used to answer
    /// `OSC 4 ; <index> ; ? ST` queries. Indices map to the standard xterm
    /// layout: 0..=7 normal, 8..=15 bright. Some CLIs (e.g. GitHub Copilot)
    /// require the full palette query reply to enable their richer prompt
    /// frame — without it they treat the terminal as colourless.
    pub fn set_theme_palette_color(&mut self, index: u8, r: u8, g: u8, b: u8) {
        if (index as usize) < self.performer.theme_palette.len() {
            self.performer.theme_palette[index as usize] = Some((r, g, b));
        }
    }

    /// Whether DECSET ?1004 (focus reporting) is currently enabled. App should
    /// send `\e[I` / `\e[O` on focus in/out when this is true.
    pub fn focus_reporting_enabled(&self) -> bool {
        self.performer.focus_reporting
    }

    /// Feed raw bytes from the pty. Drains any queued events for the caller.
    ///
    /// Implements an ASCII SWAR fast-path: while the underlying vte state
    /// machine is in the Ground state (no escape sequence in flight), we
    /// scan the input via `memchr` for the next byte that vte would actually
    /// need to dispatch (ESC `0x1B`, BEL `0x07`, or anything outside the
    /// `[0x20, 0x7E]` printable-ASCII range), bulk-print the safe ASCII run
    /// straight into the grid, and only hand the remainder to vte. Hot
    /// payloads like `cat largefile` are ~99 % printable ASCII, so this
    /// bypasses vte's byte-at-a-time state machine for the common case while
    /// keeping behaviour identical to feeding the whole slice through vte.
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<VtEvent> {
        let mut i = 0;
        let len = bytes.len();
        while i < len {
            if self.discarding_oversized_escape {
                self.consume_discarded_escape_byte(bytes[i]);
                i += 1;
                continue;
            }
            if self.apc_capture.is_some() {
                self.consume_apc_byte(bytes[i]);
                i += 1;
                continue;
            }
            if self.performer.dcs_capture.is_some() && matches!(bytes[i], 0x18 | 0x1a) {
                self.inner = vte::Parser::new();
                self.reset_cancelled_escape();
                i += 1;
                continue;
            }
            if self.performer.ground && bytes[i..].starts_with(b"\x1b_") {
                self.performer.ground = false;
                self.apc_capture = Some(MediaCapture::new(MediaProtocol::Kitty, String::new()));
                i += 2;
                continue;
            }
            if self.performer.ground {
                // memchr3 for ESC / BEL / LF — the three commonest break
                // bytes — gives us a cheap upper bound on the run length.
                // We then scalar-verify the prefix is entirely printable
                // [0x20, 0x7E]; the first non-printable byte ends the run.
                let upper = memchr::memchr3(0x1B, 0x07, 0x0A, &bytes[i..]).unwrap_or(len - i);
                let mut run_end = 0;
                while run_end < upper {
                    let b = bytes[i + run_end];
                    if !(0x20..=0x7E).contains(&b) {
                        break;
                    }
                    run_end += 1;
                }
                if run_end > 0 {
                    // SAFETY: every byte in [i..i+run_end] is in [0x20, 0x7E],
                    // i.e. valid 1-byte UTF-8 = the same code point as the byte.
                    for &b in &bytes[i..i + run_end] {
                        self.performer.print_graphic(b as char);
                    }
                    i += run_end;
                    continue;
                }
                // First byte is non-printable — feed exactly that byte to
                // vte. vte will either dispatch it (still Ground after) or
                // start consuming an escape (ground flips false). The
                // Performer callbacks below update `self.performer.ground`.
                self.performer.ground = false;
                self.escape_bytes_in_flight = 1;
                self.escape_family = match bytes[i] {
                    0x1b => EscapeFamily::Esc,
                    0x9b => EscapeFamily::Csi,
                    0x9d => EscapeFamily::Osc,
                    0x90 | 0x98 | 0x9e | 0x9f => EscapeFamily::String,
                    _ => EscapeFamily::Ground,
                };
                self.observe_osc4_byte(bytes[i]);
                self.performer.sequence_dispatched = false;
                let byte = bytes[i];
                self.inner.advance(&mut self.performer, &bytes[i..i + 1]);
                if matches!(byte, 0x18 | 0x1a) {
                    self.reset_cancelled_escape();
                }
                if self.performer.ground || self.performer.sequence_dispatched {
                    self.escape_bytes_in_flight = 0;
                    self.escape_family = EscapeFamily::Ground;
                }
                // If vte stayed in Ground (execute() or print()), the
                // callback has already set ground=true. If not, leave it
                // false so the next iteration feeds bytes through vte until
                // a dispatch callback flips it back to Ground.
                i += 1;
            } else {
                // Escape in flight — feed bytes through vte one at a time
                // and let the dispatch callbacks decide when we're back in
                // Ground. Feeding the remainder en bloc would work too, but
                // we want to return to fast-path as soon as possible, so
                // stop the moment ground flips back to true.
                let start = i;
                while i < len && !self.performer.ground {
                    if self.performer.dcs_capture.is_some() && matches!(bytes[i], 0x18 | 0x1a) {
                        self.inner = vte::Parser::new();
                        self.reset_cancelled_escape();
                        i += 1;
                        break;
                    }
                    let started_escape = if self.escape_family == EscapeFamily::Ground {
                        self.escape_family = match bytes[i] {
                            0x1b => EscapeFamily::Esc,
                            0x9b => EscapeFamily::Csi,
                            0x9d => EscapeFamily::Osc,
                            0x90 | 0x98 | 0x9e | 0x9f => EscapeFamily::String,
                            _ => EscapeFamily::Ground,
                        };
                        if self.escape_family != EscapeFamily::Ground {
                            self.escape_bytes_in_flight = 1;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if self.escape_family == EscapeFamily::Esc && !started_escape {
                        self.escape_family = match bytes[i] {
                            b'[' => EscapeFamily::Csi,
                            b']' => EscapeFamily::Osc,
                            b'P' | b'X' | b'^' | b'_' => EscapeFamily::String,
                            _ => EscapeFamily::Esc,
                        };
                    }
                    let media_capture_has_own_budget = self.performer.dcs_capture.is_some();
                    if self.escape_family != EscapeFamily::Ground
                        && !started_escape
                        && !media_capture_has_own_budget
                    {
                        self.escape_bytes_in_flight = self.escape_bytes_in_flight.saturating_add(1);
                        if self.escape_bytes_in_flight > MAX_ESCAPE_SEQUENCE_BYTES {
                            let pending_esc = self.pending_esc;
                            self.begin_discarding_oversized_escape(pending_esc);
                            self.consume_discarded_escape_byte(bytes[i]);
                            i += 1;
                            break;
                        }
                    }
                    self.observe_osc4_byte(bytes[i]);
                    self.performer.sequence_dispatched = false;
                    let byte = bytes[i];
                    self.inner.advance(&mut self.performer, &bytes[i..i + 1]);
                    i += 1;
                    if matches!(byte, 0x18 | 0x1a) {
                        self.reset_cancelled_escape();
                    }
                    if self.performer.ground || self.performer.sequence_dispatched {
                        self.escape_bytes_in_flight = 0;
                        self.escape_family = EscapeFamily::Ground;
                    }
                }
                debug_assert!(i > start, "vte must consume at least one byte per iteration");
            }
        }
        std::mem::take(&mut self.performer.events)
    }

    fn reset_cancelled_escape(&mut self) {
        self.raw_osc = None;
        self.pending_esc = false;
        self.apc_capture = None;
        self.performer.dcs_capture = None;
        self.escape_bytes_in_flight = 0;
        self.discarding_oversized_escape = false;
        self.discard_escape_pending_esc = false;
        self.discard_exits_on_newline = false;
        self.escape_family = EscapeFamily::Ground;
        self.performer.ground = true;
        self.performer.sequence_dispatched = true;
    }

    fn begin_discarding_oversized_escape(&mut self, pending_esc: bool) {
        tracing::warn!(
            limit = MAX_ESCAPE_SEQUENCE_BYTES,
            "escape sequence exceeded memory limit; discarding through terminator"
        );
        self.inner = vte::Parser::new();
        self.raw_osc = None;
        self.performer.dcs_capture = None;
        self.pending_esc = false;
        self.discarding_oversized_escape = true;
        self.discard_escape_pending_esc = pending_esc;
        self.performer.ground = false;
    }

    fn consume_discarded_escape_byte(&mut self, byte: u8) {
        let string_terminated = match self.escape_family {
            EscapeFamily::Osc => {
                byte == 0x07 || byte == 0x9c || (self.discard_escape_pending_esc && byte == b'\\')
            }
            EscapeFamily::String => {
                byte == 0x9c || (self.discard_escape_pending_esc && byte == b'\\')
            }
            EscapeFamily::Ground | EscapeFamily::Esc | EscapeFamily::Csi => false,
        };
        let final_byte = match self.escape_family {
            EscapeFamily::Csi => (0x40..=0x7e).contains(&byte),
            EscapeFamily::Esc => (0x30..=0x7e).contains(&byte),
            EscapeFamily::Ground | EscapeFamily::Osc | EscapeFamily::String => false,
        };
        let abandoned_by_newline = self.discard_exits_on_newline && byte == b'\n';
        let terminated =
            string_terminated || final_byte || abandoned_by_newline || matches!(byte, 0x18 | 0x1a);
        if terminated {
            self.discarding_oversized_escape = false;
            self.discard_escape_pending_esc = false;
            self.discard_exits_on_newline = false;
            self.escape_bytes_in_flight = 0;
            self.performer.ground = true;
            self.escape_family = EscapeFamily::Ground;
            // A newline is the user's output, not the transfer's terminator,
            // so it has to reach the grid rather than be eaten as one.
            if abandoned_by_newline {
                self.performer.execute(b'\n');
            }
            return;
        }
        self.discard_escape_pending_esc = byte == 0x1b;
    }

    fn consume_apc_byte(&mut self, byte: u8) {
        if matches!(byte, 0x18 | 0x1a) {
            self.reset_cancelled_escape();
            return;
        }
        let Some(capture) = self.apc_capture.as_mut() else { return };
        if capture.pending_esc {
            capture.pending_esc = false;
            if byte == b'\\' {
                let capture = self.apc_capture.take().expect("capture present");
                let row = self.performer.grid.cursor.row;
                let col = self.performer.grid.cursor.col;
                if let Some(event) = capture.into_kitty_event(row, col) {
                    self.performer.events.push(VtEvent::Media(event));
                }
                self.performer.ground = true;
                return;
            }
            capture.append_byte(0x1b);
        }
        if byte == 0x1b {
            capture.pending_esc = true;
        } else {
            capture.append_byte(byte);
        }
    }

    fn observe_osc4_byte(&mut self, byte: u8) {
        if let Some(mut raw_osc) = self.raw_osc.take() {
            match &mut raw_osc {
                RawOsc::Probe { saw_four } => match byte {
                    b'4' if !*saw_four => {
                        *saw_four = true;
                        self.raw_osc = Some(raw_osc);
                    }
                    b';' if *saw_four => {
                        self.raw_osc = Some(RawOsc::Palette { content: Vec::new() })
                    }
                    _ => self.pending_esc = byte == 0x1b,
                },
                RawOsc::Palette { content } => match byte {
                    0x07 | 0x1b => {
                        let content = std::mem::take(content);
                        self.performer.handle_osc4_raw(&content, byte == 0x07);
                        self.performer.suppress_next_osc4 = true;
                        self.performer.ground = true;
                        self.pending_esc = byte == 0x1b;
                    }
                    _ => {
                        if content.len() < MAX_RAW_OSC4_BYTES {
                            content.push(byte);
                            self.raw_osc = Some(raw_osc);
                        }
                    }
                },
            }
            return;
        }

        if self.pending_esc {
            self.pending_esc = false;
            if byte == b']' {
                self.raw_osc = Some(RawOsc::Probe { saw_four: false });
                return;
            }
        }
        self.pending_esc = byte == 0x1b;
    }

    /// Borrow the underlying [`Grid`] — used by the renderer to read cells.
    pub fn grid(&self) -> &Grid {
        &self.performer.grid
    }

    /// Bytes and buffers this parser is holding mid-sequence.
    ///
    /// Covers the in-flight media capture, the raw OSC palette accumulator, and
    /// reserved capacity in both. Excludes the grid and the hyperlink registry,
    /// which report their own retention: a pane composes these figures rather
    /// than any one of them restating another.
    ///
    /// Items count live capture buffers, which is at most one — see
    /// [`Self::live_capture_count`].
    #[must_use]
    pub fn retained_amount(&self) -> ResourceAmount {
        let capture_bytes = self
            .apc_capture
            .iter()
            .chain(self.performer.dcs_capture.iter())
            .map(MediaCapture::retained_bytes)
            .sum::<usize>();
        let osc_bytes = match &self.raw_osc {
            Some(RawOsc::Palette { content }) => content.capacity(),
            Some(RawOsc::Probe { .. }) | None => 0,
        };
        ResourceAmount {
            bytes: capture_bytes.saturating_add(osc_bytes),
            items: self.live_capture_count(),
        }
    }

    /// Bytes this parser has fed into media captures over its lifetime.
    ///
    /// Monotonic, and advances only while a capture is actually receiving. A
    /// host that samples this periodically can tell a stalled capture from a
    /// slow one — the distinction the parser cannot make, having no clock —
    /// by observing that the figure has not moved between two samples.
    ///
    /// Paired with [`Parser::cancel_capture`], this is what lets a stalled
    /// transfer's staging be reclaimed. Neither half is useful alone: the
    /// parser cannot decide *when*, and the host cannot reach *what*.
    #[must_use]
    pub fn capture_progress(&self) -> usize {
        self.apc_capture
            .iter()
            .chain(self.performer.dcs_capture.iter())
            .map(|capture| capture.seen)
            .sum()
    }

    /// Abandon any capture in flight, releasing its staging allocation.
    ///
    /// Returns the bytes released. The partially-received payload is
    /// discarded rather than dispatched: a fragment of an image decodes to
    /// nothing useful, so surfacing it would trade memory for a broken
    /// picture instead of no picture.
    ///
    /// The sender is not told, so the rest of the transfer keeps arriving.
    /// Those bytes are swallowed rather than returned to ground: a payload is
    /// printable ASCII end to end, so a parser back in ground would print
    /// megabytes of base64 into the grid — the user would lose the image
    /// *and* the screen. The discard ends at the transfer's terminator, or at
    /// the first newline if the sender died and no terminator is ever coming.
    ///
    /// Intended for a capture the host has determined is stalled — see
    /// [`Parser::capture_progress`]. Cancelling one that is merely slow costs
    /// the user their transfer, so the staleness threshold belongs to the host
    /// where the clock and the user's configuration both are.
    ///
    /// Returns 0 and does nothing when no capture is in flight.
    pub fn cancel_capture(&mut self) -> usize {
        let released = self.retained_amount().bytes;
        if released == 0 && self.live_capture_count() == 0 {
            return 0;
        }
        self.inner = vte::Parser::new();
        self.reset_cancelled_escape();
        // Both capture families terminate with ST, so the discard reads as a
        // string sequence. Set after the reset, which clears these.
        self.escape_family = EscapeFamily::String;
        self.discarding_oversized_escape = true;
        self.discard_exits_on_newline = true;
        self.performer.ground = false;
        released
    }

    /// Number of media captures currently accumulating.
    ///
    /// Beginning any escape family cancels a capture already in flight, so this
    /// is at most one. The two capture slots are therefore alternatives rather
    /// than addends, and a budget covering their sum would guard a state the
    /// parser cannot reach. That exclusivity is what needs holding, so it is
    /// exposed for assertion rather than left as an emergent property.
    #[must_use]
    pub fn live_capture_count(&self) -> usize {
        usize::from(self.apc_capture.is_some()) + usize::from(self.performer.dcs_capture.is_some())
    }

    /// Mutably borrow the [`Grid`] — used by the host on resize, scrollback
    /// scroll, and selection clears.
    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.performer.grid
    }

    /// Borrow the hyperlink registry (OSC 8 interned uris).
    pub fn hyperlinks(&self) -> &HyperlinkRegistry {
        &self.performer.hyperlinks
    }

    /// Currently-active hyperlink id, if any.
    pub fn current_hyperlink(&self) -> Option<HyperlinkId> {
        self.performer.current_hyperlink
    }

    /// Whether DECSET ?2004 (bracketed paste) is currently enabled.
    pub fn bracketed_paste_enabled(&self) -> bool {
        self.performer.bracketed_paste
    }

    /// Whether DECSET ?1006 (SGR mouse reporting) is currently enabled.
    pub fn mouse_sgr_enabled(&self) -> bool {
        self.performer.mouse_sgr
    }

    /// Whether DECCKM ?1 (application cursor keys) is currently enabled. When
    /// true, arrow-key sequences — including the synthetic ones SonicTerm
    /// emits for alt-screen wheel scroll — use the `ESC O A` form.
    pub fn application_cursor_keys(&self) -> bool {
        self.performer.app_cursor_keys
    }

    /// Whether any of DECSET ?1000/?1002/?1003 (mouse tracking) is currently
    /// enabled. When true, the host should forward wheel events to the PTY as
    /// mouse reports rather than synthesizing scroll/arrow-key motion.
    pub fn mouse_tracking_enabled(&self) -> bool {
        self.performer.mouse_tracking
    }

    /// Active kitty keyboard protocol flags (the top of the progressive
    /// enhancement push/pop stack). `0` means no flags / legacy encoding.
    /// The host reads this to decide whether to emit CSI-u key encodings —
    /// e.g. Shift+Enter as `CSI 13 ; 2 u` when a TUI like Copilot CLI has
    /// pushed the disambiguate flag.
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.performer.kitty_keyboard_flags()
    }

    /// Latest OSC 0/2 window title (sticky), or `None` if no title has been
    /// set. Used by the tab bar to label tabs with the shell's reported title.
    pub fn title(&self) -> Option<&str> {
        self.performer.title.as_deref()
    }

    /// Latest OSC 7 working directory (sticky), or `None` if the shell hasn't
    /// reported one. Stored as a filesystem path (the `file://host/` prefix
    /// is stripped at parse time); used by the tab-title renderer to show
    /// `parent/leaf` of the current cwd.
    pub fn cwd(&self) -> Option<&str> {
        self.performer.cwd.as_deref()
    }
}

struct Performer {
    grid: Grid,
    fg: Color,
    bg: Color,
    flags: CellFlags,
    underline_style: UnderlineStyle,
    underline_color: Option<Color>,
    events: Vec<VtEvent>,
    hyperlinks: HyperlinkRegistry,
    current_hyperlink: Option<HyperlinkId>,
    /// Scrollback-eviction count at the last reclaim sweep.
    ///
    /// The backoff below is only valid while the grid still looks the way it
    /// did when the sweep found nothing. Evicting a row turns live links into
    /// garbage, so a change here releases the backoff immediately rather than
    /// making the next 256 links wait out a counter set against a grid that no
    /// longer exists.
    hyperlink_reclaim_scanned_at: u64,
    /// Rejected OSC 8 links since the last reclaim sweep.
    ///
    /// Bounds how often a full-grid scan may run. When every interned link is
    /// genuinely still on screen a sweep frees nothing, and without this an
    /// output stream of distinct links would scan the whole grid once per
    /// link — turning a dead feature into a stall.
    hyperlink_reclaim_backoff: u32,
    /// Cursor saved by DECSET ?1049 when entering the alt screen.
    saved_cursor: Option<Pos>,
    bracketed_paste: bool,
    mouse_sgr: bool,
    /// DECCKM ?1 — application cursor keys. When set, the arrow keys (and the
    /// synthetic arrow sequences SonicTerm emits for alt-screen wheel scroll)
    /// must use the `ESC O A` form instead of `ESC [ A`.
    app_cursor_keys: bool,
    /// DECSET ?1000/?1002/?1003 — X10/button/any-motion mouse tracking. When
    /// any of these is on the application wants raw mouse reports, so the host
    /// must forward wheel events to the PTY rather than synthesizing scroll.
    mouse_tracking: bool,
    focus_reporting: bool,
    /// Latest OSC 0/2 title (sticky — survives consumed events).
    title: Option<String>,
    /// Latest OSC 7 working directory (sticky), filesystem path with the
    /// `file://host/` prefix already stripped. `None` until the shell sends
    /// one — modern zsh/bash/fish ship with cwd-reporting prompts.
    cwd: Option<String>,
    reply_tx: Option<Sender<Vec<u8>>>,
    reply_queue_full_warned: std::sync::atomic::AtomicBool,
    sequence_dispatched: bool,
    /// Theme default foreground (sRGB), used to answer OSC 10 `?` queries.
    /// `None` means the parser was never told a theme — query replies are
    /// suppressed in that case so we don't lie to the shell.
    theme_fg: Option<(u8, u8, u8)>,
    /// Theme default background (sRGB), used to answer OSC 11 `?` queries.
    /// nvim queries this to colour cells painted with `bg=NONE` (e.g.
    /// neo-tree icon cells); without a reply nvim guesses (27,29,30)
    /// instead of SonicTerm's actual theme bg.
    theme_bg: Option<(u8, u8, u8)>,
    /// Theme cursor colour (sRGB), used to answer OSC 12 `?` queries.
    /// Falls back to `theme_fg` if unset.
    theme_cursor: Option<(u8, u8, u8)>,
    /// 16-colour ANSI palette (sRGB) used to answer `OSC 4 ; <i> ; ? ST`
    /// queries (index 0..=15: 0-7 normal, 8-15 bright). Per-slot `None`
    /// suppresses that slot's reply so we never report a colour we were
    /// not told.
    theme_palette: [Option<(u8, u8, u8)>; 16],
    /// SonicTerm's raw OSC4 capture handles full batched queries before vte's
    /// capped `osc_dispatch`; suppress the immediately-following duplicate.
    suppress_next_osc4: bool,
    /// DECSTBM scrolling region top margin (visible-row, 0-based,
    /// inclusive). `None` means "no region set — full screen".
    scroll_top: Option<u16>,
    /// DECSTBM scrolling region bottom margin (visible-row, 0-based,
    /// inclusive).
    scroll_bottom: Option<u16>,
    /// Tracks whether the underlying vte state machine is in the Ground
    /// state (no escape sequence currently being consumed). Maintained
    /// externally: set to `true` after every dispatch callback fires
    /// (`print` / `execute` / `csi_dispatch` / `osc_dispatch` /
    /// `esc_dispatch` / `unhook`), set to `false` inside `Parser::advance`
    /// just before feeding the first byte of a potential escape, and held
    /// `false` while inside a DCS passthrough (`hook` … `unhook`).
    /// The ASCII fast-path in `Parser::advance` is only taken when this is
    /// `true`.
    ground: bool,
    /// Most-recently-printed graphic character, for CSI `b` (REP).
    /// ECMA-48: REP repeats the GRAPHIC CHARACTER immediately preceding
    /// REP in the data stream. Reset when a control function intervenes.
    last_printed_char: Option<char>,
    dcs_capture: Option<MediaCapture>,
    /// Kitty keyboard protocol progressive-enhancement flag stack. The active
    /// flags are the top of stack (`last()`); empty stack == flags 0 == legacy
    /// encoding. Apps push with `CSI > flags u`, pop with `CSI < number u`,
    /// set with `CSI = flags ; mode u`, and query with `CSI ? u`.
    kitty_kbd_flags: Vec<u8>,
}

/// Maximum depth of the kitty keyboard flag stack. The protocol allows nested
/// push/pop but a misbehaving app must not be able to grow it without bound.
const KITTY_KBD_STACK_MAX: usize = 32;

impl Performer {
    fn new(grid: Grid, reply_tx: Option<Sender<Vec<u8>>>) -> Self {
        Self {
            grid,
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            underline_style: UnderlineStyle::Single,
            underline_color: None,
            events: Vec::new(),
            hyperlinks: HyperlinkRegistry::new(),
            current_hyperlink: None,
            hyperlink_reclaim_scanned_at: 0,
            hyperlink_reclaim_backoff: 0,
            saved_cursor: None,
            bracketed_paste: false,
            mouse_sgr: false,
            app_cursor_keys: false,
            mouse_tracking: false,
            focus_reporting: false,
            title: None,
            cwd: None,
            reply_tx,
            reply_queue_full_warned: std::sync::atomic::AtomicBool::new(false),
            sequence_dispatched: false,
            theme_fg: None,
            theme_bg: None,
            theme_cursor: None,
            theme_palette: [None; 16],
            suppress_next_osc4: false,
            scroll_top: None,
            scroll_bottom: None,
            ground: true,
            last_printed_char: None,
            dcs_capture: None,
            kitty_kbd_flags: Vec::new(),
        }
    }

    /// Resolve the active scroll region, defaulting to the full
    /// visible grid when DECSTBM has not been set. Used by every
    /// scroll-emitting opcode (CSI S, CSI T, IND-at-bottom-margin,
    /// RI-at-top-margin).
    fn effective_scroll_region(&self) -> (u16, u16) {
        let rows = self.grid.rows;
        let top = self.scroll_top.unwrap_or(0);
        let bot = self.scroll_bottom.unwrap_or(rows.saturating_sub(1));
        (top, bot)
    }

    fn reply(&self, bytes: &[u8]) {
        if let Some(tx) = &self.reply_tx {
            match tx.try_send(bytes.to_vec()) {
                Ok(()) => {
                    self.reply_queue_full_warned.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    if !self
                        .reply_queue_full_warned
                        .swap(true, std::sync::atomic::Ordering::Relaxed)
                    {
                        tracing::warn!("terminal reply dropped because the reply queue is full");
                    }
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
            }
        }
    }

    fn handle_osc4_raw(&self, raw_pairs: &[u8], bell_terminated: bool) {
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        let mut parts = raw_pairs.split(|&byte| byte == b';');
        while let Some(idx) = parts.next() {
            let Some(spec) = parts.next() else { break };
            let idx = std::str::from_utf8(idx).ok().and_then(|s| s.trim().parse::<u8>().ok());
            let spec = std::str::from_utf8(spec).ok().map(str::trim);
            if let (Some(idx), Some("?")) = (idx, spec) {
                self.reply_osc4_query(idx, terminator);
            }
        }
    }

    fn handle_osc4_pairs(&self, params: &[&[u8]], bell_terminated: bool) {
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        let mut i = 1;
        while i + 1 < params.len() {
            let idx = std::str::from_utf8(params[i]).ok().and_then(|s| s.trim().parse::<u8>().ok());
            let spec = std::str::from_utf8(params[i + 1]).ok().map(str::trim);
            if let (Some(idx), Some("?")) = (idx, spec) {
                self.reply_osc4_query(idx, terminator);
            }
            i += 2;
        }
    }

    fn reply_osc4_query(&self, idx: u8, terminator: &[u8]) {
        if let Some(Some((r, g, b))) = self.theme_palette.get(idx as usize).copied() {
            let mut buf = Vec::with_capacity(28);
            buf.extend_from_slice(b"\x1b]4;");
            buf.extend_from_slice(idx.to_string().as_bytes());
            buf.extend_from_slice(
                format!(";rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}").as_bytes(),
            );
            buf.extend_from_slice(terminator);
            self.reply(&buf);
        }
    }

    /// Active kitty keyboard protocol flags (top of the push/pop stack).
    /// Empty stack reports 0, meaning legacy (non-kitty) key encoding.
    fn kitty_keyboard_flags(&self) -> u8 {
        *self.kitty_kbd_flags.last().unwrap_or(&0)
    }

    fn reset_last_printed_char(&mut self) {
        self.last_printed_char = None;
    }

    /// Blank cell with the current SGR rendition. This is the Sonic Grid
    /// equivalent of WezTerm/xterm background-color erase (BCE): ED/EL/ECH,
    /// inserted blanks, deleted-cell fill, and scroll-fill rows inherit the
    /// app's active colors instead of falling back to the terminal theme.
    fn erase_fill_cell(&self) -> Cell {
        let mut flags = self.flags;
        flags.remove(CellFlags::WIDE | CellFlags::WIDE_CONT);
        let mut cell = Cell::plain(' ', self.fg, self.bg, flags);
        cell.set_hyperlink(self.current_hyperlink);
        if flags.contains(CellFlags::UNDERLINE) {
            cell.set_underline_style(self.underline_style);
            cell.set_underline_color(self.underline_color);
        }
        cell
    }

    fn print_graphic(&mut self, ch: char) {
        let region = (self.scroll_top.is_some() || self.scroll_bottom.is_some())
            .then(|| self.effective_scroll_region());
        let fill = self.erase_fill_cell();
        self.grid.put_char_styled_in_region(
            ch,
            self.fg,
            self.bg,
            self.flags,
            self.current_hyperlink,
            self.underline_style,
            self.underline_color,
            region,
            fill,
        );
        self.last_printed_char = Some(ch);
    }

    fn reset_attrs(&mut self) {
        self.fg = Color::Default;
        self.bg = Color::Default;
        self.flags = CellFlags::empty();
        self.underline_style = UnderlineStyle::Single;
        self.underline_color = None;
    }

    fn reset_terminal(&mut self) {
        self.reset_attrs();
        self.saved_cursor = None;
        self.bracketed_paste = false;
        self.mouse_sgr = false;
        self.app_cursor_keys = false;
        self.mouse_tracking = false;
        self.focus_reporting = false;
        self.current_hyperlink = None;
        self.scroll_top = None;
        self.scroll_bottom = None;
        self.last_printed_char = None;
        self.dcs_capture = None;
        self.kitty_kbd_flags.clear();
        self.grid.set_autowrap(true);
        if self.grid.is_alt() {
            self.grid.leave_alt_screen();
        }
        self.grid.erase_screen_with(Cell::default());
        self.grid.goto(0, 0);
        // RIS erases every screen and drops the alt buffer above, so no cell
        // can still reference an interned link. Clearing outright is cheaper
        // and no less correct than scanning to prove the set is empty.
        self.hyperlinks.clear();
        self.hyperlink_reclaim_backoff = 0;
    }

    /// Free interned hyperlinks no cell references, returning the count.
    ///
    /// Runs only when admission has already failed, so a pane below the cap
    /// never pays for it. The scan is `O(visible + scrollback)` over every
    /// screen the grid owns.
    ///
    /// The root set is every cell plus [`Self::current_hyperlink`], which is
    /// live but not yet written to any cell: between OSC 8 open and the first
    /// printed character there is no cell holding it, and freeing it there
    /// would unlink the text about to be written.
    fn reclaim_hyperlinks(&mut self) -> usize {
        let mut live: HashSet<HyperlinkId> = HashSet::new();
        if let Some(open) = self.current_hyperlink {
            live.insert(open);
        }
        self.grid.collect_live_hyperlinks(&mut live);
        self.hyperlinks.retain_live(&live)
    }

    fn apply_sgr(&mut self, params: &Params) {
        let mut iter = params.iter();
        while let Some(slice) = iter.next() {
            let p = slice.first().copied().unwrap_or(0);
            match p {
                0 => self.reset_attrs(),
                1 => self.flags |= CellFlags::BOLD,
                2 => self.flags |= CellFlags::DIM,
                3 => self.flags |= CellFlags::ITALIC,
                4 => {
                    let style = slice.get(1).copied().unwrap_or(1);
                    match style {
                        0 => {
                            self.flags.remove(CellFlags::UNDERLINE);
                            self.underline_style = UnderlineStyle::Single;
                        }
                        1 => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Single;
                        }
                        2 => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Double;
                        }
                        3 => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Curly;
                        }
                        4 => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Dotted;
                        }
                        5 => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Dashed;
                        }
                        _ => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Single;
                        }
                    }
                }
                5 => self.flags |= CellFlags::BLINK,
                7 => self.flags |= CellFlags::INVERSE,
                8 => self.flags |= CellFlags::HIDDEN,
                9 => self.flags |= CellFlags::STRIKETHROUGH,
                21 => {
                    self.flags |= CellFlags::UNDERLINE;
                    self.underline_style = UnderlineStyle::Double;
                }
                22 => self.flags.remove(CellFlags::BOLD | CellFlags::DIM),
                23 => self.flags.remove(CellFlags::ITALIC),
                24 => {
                    self.flags.remove(CellFlags::UNDERLINE);
                    self.underline_style = UnderlineStyle::Single;
                }
                25 => self.flags.remove(CellFlags::BLINK),
                27 => self.flags.remove(CellFlags::INVERSE),
                28 => self.flags.remove(CellFlags::HIDDEN),
                29 => self.flags.remove(CellFlags::STRIKETHROUGH),
                30..=37 => self.fg = Color::Indexed((p - 30) as u8),
                39 => self.fg = Color::Default,
                40..=47 => self.bg = Color::Indexed((p - 40) as u8),
                49 => self.bg = Color::Default,
                90..=97 => self.fg = Color::Indexed((p - 90 + 8) as u8),
                100..=107 => self.bg = Color::Indexed((p - 100 + 8) as u8),
                38 => {
                    if let Some(c) = parse_ext_color(&mut iter) {
                        self.fg = c;
                    }
                }
                48 => {
                    if let Some(c) = parse_ext_color(&mut iter) {
                        self.bg = c;
                    }
                }
                58 => {
                    self.underline_color = parse_ext_color(&mut iter);
                }
                59 => self.underline_color = None,
                _ => {} // unknown — silently ignore for forward compat
            }
        }
    }

    /// Handle a CSI sequence with `?` intermediate (DEC private modes).
    fn handle_dec_private_mode(&mut self, params: &Params, set: bool) {
        self.reset_last_printed_char();
        for slice in params.iter() {
            let code = slice.first().copied().unwrap_or(0);
            match code {
                1 => self.app_cursor_keys = set,
                7 => self.grid.set_autowrap(set),
                25 => self.events.push(VtEvent::CursorVisibility(set)),
                47 => {
                    let before = self.grid.is_alt();
                    if set {
                        self.grid.enter_alt_screen();
                    } else {
                        self.grid.leave_alt_screen();
                    }
                    let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                    let after = self.grid.is_alt();
                    let sr = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonicterm_vt::alt",
                        "private mode CSI ?47{sr}: alt_screen_active={before}→{after}, cursor=({r},{c})"
                    );
                }
                1047 => {
                    // Same as ?47 — alt-screen switch WITHOUT cursor save/restore.
                    // Distinct from ?1049 (which also saves/restores the cursor)
                    // and from ?1048 (cursor save/restore only).
                    let before = self.grid.is_alt();
                    if set {
                        self.grid.enter_alt_screen();
                    } else {
                        self.grid.leave_alt_screen();
                    }
                    let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                    let after = self.grid.is_alt();
                    let sr = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonicterm_vt::alt",
                        "private mode CSI ?1047{sr}: alt_screen_active={before}→{after}, cursor=({r},{c})"
                    );
                }
                1048 => {
                    // Save / restore cursor only (DECSC / DECRC equivalent).
                    let before = self.grid.is_alt();
                    if set {
                        self.saved_cursor = Some(self.grid.cursor);
                    } else if let Some(c) = self.saved_cursor {
                        self.grid.goto(c.row, c.col);
                    }
                    let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                    let sr = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonicterm_vt::alt",
                        "private mode CSI ?1048{sr}: alt_screen_active={before}→{before}, cursor=({r},{c})"
                    );
                }
                1049 => {
                    let before = self.grid.is_alt();
                    if set {
                        // Guard against repeated ?1049h while already in alt
                        // screen — must not clobber the previously saved
                        // primary-screen cursor. xterm behaviour: second
                        // ?1049h is a no-op.
                        if !self.grid.is_alt() {
                            self.saved_cursor = Some(self.grid.cursor);
                            self.grid.enter_alt_screen();
                        }
                    } else {
                        self.grid.leave_alt_screen();
                        if let Some(c) = self.saved_cursor.take() {
                            self.grid.goto(c.row, c.col);
                        }
                    }
                    let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                    let after = self.grid.is_alt();
                    let sr = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonicterm_vt::alt",
                        "private mode CSI ?1049{sr}: alt_screen_active={before}→{after}, cursor=({r},{c})"
                    );
                }
                2004 => self.bracketed_paste = set,
                1006 => self.mouse_sgr = set,
                1000 | 1002 | 1003 => self.mouse_tracking = set,
                1004 => self.focus_reporting = set,
                2026 => { /* synchronized output (BSU/ESU) — accept silently for now;
                     defer-paint optimisation tracked separately. Prevents future
                     smear classes from apps that wrap updates in ?2026 h/l. */
                }
                _ => {}
            }
        }
    }
}

/// Parse an OSC 7 payload (typically `file://host/path`) into a filesystem
/// path. Strips the scheme + host, and percent-decodes `%XX` escapes so
/// names with spaces / unicode round-trip correctly. Empty / malformed
/// inputs return an empty string.
pub fn parse_osc7_cwd(raw: &str) -> String {
    let stripped = raw.strip_prefix("file://").unwrap_or(raw);
    // After `file://` the next `/` starts the absolute path; anything
    // before it is the (often empty) hostname which we discard.
    let path_part = match stripped.find('/') {
        Some(i) => &stripped[i..],
        None => stripped,
    };
    percent_decode(path_part)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + b - b'a'),
        b'A'..=b'F' => Some(10 + b - b'A'),
        _ => None,
    }
}

fn parse_ext_color(iter: &mut vte::ParamsIter<'_>) -> Option<Color> {
    let mode = iter.next()?.first().copied()?;
    match mode {
        5 => Some(Color::Indexed(iter.next()?.first().copied()? as u8)),
        2 => {
            let r = iter.next()?.first().copied()? as u8;
            let g = iter.next()?.first().copied()? as u8;
            let b = iter.next()?.first().copied()? as u8;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

fn join_osc_params(params: &[&[u8]]) -> String {
    let mut out = Vec::new();
    for (idx, param) in params.iter().enumerate() {
        if idx > 0 {
            out.push(b';');
        }
        out.extend_from_slice(param);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse an iTerm2 `File=...` inline media payload.
///
/// Returns `None` for a payload larger than a capture may hold, rather than a
/// capped one: the bytes past the cap are the rest of the image, and an image
/// missing its tail decodes to nothing for a base64 protocol. Dispatching the
/// prefix would cost a decode that is guaranteed to fail.
fn parse_iterm2_file_event(payload: &[u8], row: u16, col: u16) -> Option<MediaEvent> {
    if !payload.starts_with(b"File=") {
        return None;
    }
    let (metadata, data) = split_once_byte(payload, b':')?;
    if data.len() > MAX_MEDIA_PAYLOAD_BYTES {
        tracing::warn!(
            payload_bytes = data.len(),
            cap = MAX_MEDIA_PAYLOAD_BYTES,
            "inline media payload refused: larger than a capture may hold"
        );
        return None;
    }
    Some(MediaEvent {
        protocol: MediaProtocol::Iterm2File,
        row,
        col,
        metadata: String::from_utf8_lossy(metadata).into_owned(),
        data: data.to_vec(),
    })
}

fn split_once_byte(bytes: &[u8], needle: u8) -> Option<(&[u8], &[u8])> {
    let pos = bytes.iter().position(|b| *b == needle)?;
    Some((&bytes[..pos], &bytes[pos + 1..]))
}

impl Perform for Performer {
    fn print(&mut self, c: char) {
        self.print_graphic(c);
        self.ground = true;
    }

    fn execute(&mut self, byte: u8) {
        self.reset_last_printed_char();
        match byte {
            0x07 => self.events.push(VtEvent::Bell),
            0x08 => self.grid.backspace(),
            0x09 => self.grid.tab(),
            0x0A..=0x0C => {
                // LF/VT/FF — like IND, must scroll the active region
                // (not the whole grid) when at the bottom margin so
                // DECSTBM works for shells/apps that use LF rather
                // than IND.
                let (top, bot) = self.effective_scroll_region();
                if self.grid.cursor.row == bot
                    && (self.scroll_top.is_some() || self.scroll_bottom.is_some())
                {
                    self.grid.scroll_region_up_with(top, bot, 1, self.erase_fill_cell());
                } else {
                    self.grid.linefeed_with(self.erase_fill_cell());
                }
            }
            0x0D => self.grid.carriage_return(),
            _ => {}
        }
        // NB: do NOT set ground=true here. vte may call execute() while still
        // inside an ESC/CSI/OSC/DCS state machine (C0 bytes are dispatched
        // even mid-escape). Resuming the SWAR fast-path here would consume
        // the remainder of the escape sequence as printable text.
        self.ground = false;
    }

    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], _ignore: bool, action: char) {
        self.ground = false;
        self.sequence_dispatched = true;
        if action != 'b' {
            self.reset_last_printed_char();
        }
        if inter.first() == Some(&b'?') {
            match action {
                'h' => {
                    self.handle_dec_private_mode(params, true);
                    return;
                }
                'l' => {
                    self.handle_dec_private_mode(params, false);
                    return;
                }
                'u' => {
                    // Kitty keyboard protocol query: report the active flags.
                    let flags = self.kitty_keyboard_flags();
                    self.reply(format!("\x1b[?{flags}u").as_bytes());
                    return;
                }
                _ => return,
            }
        }
        let p0 = || params.iter().next().and_then(|s| s.first().copied()).unwrap_or(0);
        let p1 = || params.iter().nth(1).and_then(|s| s.first().copied()).unwrap_or(0);
        // CSI with `>` intermediate — secondary DA / XTVERSION.
        if inter.first() == Some(&b'>') {
            match action {
                'c' => {
                    // Secondary DA: VT220 (1), firmware version 0, ROM 0.
                    self.reply(b"\x1b[>1;0;0c");
                }
                'q' => {
                    // XTVERSION: DCS > | <name> ST
                    let mut buf = Vec::with_capacity(SONIC_VERSION.len() + 5);
                    buf.extend_from_slice(b"\x1bP>|");
                    buf.extend_from_slice(SONIC_VERSION.as_bytes());
                    buf.extend_from_slice(b"\x1b\\");
                    self.reply(&buf);
                }
                'u' if self.kitty_kbd_flags.len() < KITTY_KBD_STACK_MAX => {
                    // Kitty keyboard protocol push: `CSI > flags u`. Push the
                    // requested flag set onto the stack. Cap the depth so a
                    // misbehaving app can't grow it without bound.
                    self.kitty_kbd_flags.push(p0() as u8);
                }
                _ => {}
            }
            return;
        }
        // CSI with `<` intermediate — kitty keyboard protocol pop.
        // `CSI < number u` pops up to `number` (default 1) entries off the
        // flag stack. Other `<`-prefixed sequences are not used by SonicTerm.
        if inter.first() == Some(&b'<') {
            if action == 'u' {
                let n = (p0() as usize).max(1);
                let new_len = self.kitty_kbd_flags.len().saturating_sub(n);
                self.kitty_kbd_flags.truncate(new_len);
            }
            return;
        }
        // CSI with `=` intermediate — kitty keyboard protocol set.
        // `CSI = flags ; mode u` sets the current (top-of-stack) flags. `mode`
        // selects all (1)/set-or (2)/reset-and (3); we keep the common cases
        // and otherwise replace. With an empty stack there is nothing to set,
        // so push the requested flags as the active set.
        if inter.first() == Some(&b'=') {
            if action == 'u' {
                let flags = p0() as u8;
                let mode = p1();
                let current = self.kitty_keyboard_flags();
                let next = match mode {
                    2 => current | flags,
                    3 => current & !flags,
                    // mode 1 (default) and anything else: replace.
                    _ => flags,
                };
                if let Some(top) = self.kitty_kbd_flags.last_mut() {
                    *top = next;
                } else if self.kitty_kbd_flags.len() < KITTY_KBD_STACK_MAX {
                    self.kitty_kbd_flags.push(next);
                }
            }
            return;
        }
        match action {
            'A' => {
                let n = p0().max(1);
                let row = self.grid.cursor.row.saturating_sub(n);
                let col = self.grid.cursor.col;
                self.grid.goto(row, col);
            }
            'B' => {
                let n = p0().max(1);
                let row = (self.grid.cursor.row + n).min(self.grid.rows.saturating_sub(1));
                let col = self.grid.cursor.col;
                self.grid.goto(row, col);
            }
            'C' => {
                let n = p0().max(1);
                let row = self.grid.cursor.row;
                let col = (self.grid.cursor.col + n).min(self.grid.cols.saturating_sub(1));
                self.grid.goto(row, col);
            }
            'D' => {
                let n = p0().max(1);
                let row = self.grid.cursor.row;
                let col = self.grid.cursor.col.saturating_sub(n);
                self.grid.goto(row, col);
            }
            'E' => {
                let n = p0().max(1);
                let row = (self.grid.cursor.row + n).min(self.grid.rows.saturating_sub(1));
                self.grid.goto(row, 0);
            }
            'F' => {
                let n = p0().max(1);
                let row = self.grid.cursor.row.saturating_sub(n);
                self.grid.goto(row, 0);
            }
            'H' | 'f' => {
                let row = p0().saturating_sub(1);
                let col = p1().saturating_sub(1);
                self.grid.goto(row, col);
            }
            'J' => {
                let mode = p0();
                let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                let (rows, cols) = (self.grid.rows, self.grid.cols);
                let will_blank = match mode {
                    0 => format!(
                        "rows ({r},{c})..({r},{}) + ({},0)..({},{})",
                        cols.saturating_sub(1),
                        r + 1,
                        rows.saturating_sub(1),
                        cols.saturating_sub(1)
                    ),
                    1 => format!("(0,0)..({r},{c}) inclusive"),
                    2 | 3 => "entire screen".to_string(),
                    _ => "<unknown mode, no-op>".to_string(),
                };
                tracing::debug!(
                    target: "sonicterm_vt::erase",
                    "CSI {mode}J: cursor=({r},{c}), grid_size=({rows},{cols}), will_blank={will_blank}"
                );
                match mode {
                    0 => self.grid.erase_below_with(self.erase_fill_cell()),
                    1 => self.grid.erase_above_with(self.erase_fill_cell()),
                    2 | 3 => self.grid.erase_screen_with(self.erase_fill_cell()),
                    _ => {}
                }
            }
            'K' => {
                let mode = p0();
                let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                let (rows, cols) = (self.grid.rows, self.grid.cols);
                let will_blank = match mode {
                    0 => format!("cells ({r},{c})..({r},{})", cols.saturating_sub(1)),
                    1 => format!("cells ({r},0)..({r},{c}) inclusive"),
                    2 => format!("cells ({r},0)..({r},{})", cols.saturating_sub(1)),
                    _ => "<unknown mode, no-op>".to_string(),
                };
                tracing::debug!(
                    target: "sonicterm_vt::erase",
                    "CSI {mode}K: cursor=({r},{c}), grid_size=({rows},{cols}), will_blank={will_blank}"
                );
                match mode {
                    0 => self.grid.erase_line_to_end_with(self.erase_fill_cell()),
                    1 => self.grid.erase_line_to_start_with(self.erase_fill_cell()),
                    2 => self.grid.erase_line_with(self.erase_fill_cell()),
                    _ => {}
                }
            }
            'L' => {
                // CSI Ps L — IL (Insert Line). Insert n blank lines at the
                // cursor row, pushing the rest of the scroll region down.
                // ECMA-48: no-op when cursor is outside the active region.
                // xterm behaviour: cursor moves to column 0.
                let n = p0().max(1);
                let (top, bot) = self.effective_scroll_region();
                let cur = self.grid.cursor.row;
                if cur >= top && cur <= bot {
                    self.grid.scroll_region_down_with(cur, bot, n, self.erase_fill_cell());
                    self.grid.cursor.col = 0;
                }
            }
            'M' => {
                // CSI Ps M — DL (Delete Line). Delete n lines starting at
                // the cursor row, pulling the region below up. Cursor->col 0.
                let n = p0().max(1);
                let (top, bot) = self.effective_scroll_region();
                let cur = self.grid.cursor.row;
                if cur >= top && cur <= bot {
                    self.grid.scroll_region_up_with(cur, bot, n, self.erase_fill_cell());
                    self.grid.cursor.col = 0;
                }
            }
            'm' => self.apply_sgr(params),
            '@' => {
                // ICH — Insert n blank cells at the cursor on the current
                // row, shifting trailing cells right and dropping overflow.
                let n = p0().max(1) as usize;
                let cur = self.grid.cursor;
                self.grid.insert_cells_with(cur.row, cur.col, n, self.erase_fill_cell());
            }
            'P' => {
                // DCH — Delete n cells at the cursor, shifting trailing
                // cells left and filling the right edge with blanks.
                let n = p0().max(1) as usize;
                let cur = self.grid.cursor;
                self.grid.delete_cells_with(cur.row, cur.col, n, self.erase_fill_cell());
            }
            'X' => {
                // ECH — Erase n cells starting at the cursor with the
                // current SGR blank cell. Cursor is unchanged. neo-tree's
                // per-row tail-clear pattern depends on this.
                let n = p0().max(1) as usize;
                let cur = self.grid.cursor;
                self.grid.erase_cells_with(cur.row, cur.col, n, self.erase_fill_cell());
            }
            'G' | '`' => {
                // CHA (G) / HPA (`) — Cursor to column p0 (1-based) on the
                // current row.
                let col_1 = p0().max(1);
                let row = self.grid.cursor.row;
                self.grid.goto(row, col_1.saturating_sub(1));
            }
            'd' => {
                // VPA — Cursor to row p0 (1-based), column unchanged.
                let row_1 = p0().max(1);
                let col = self.grid.cursor.col;
                self.grid.goto(row_1.saturating_sub(1), col);
            }
            'b' => {
                // REP — Repeat last printable character n times at cursor.
                let n = p0().max(1) as usize;
                if let Some(ch) = self.last_printed_char {
                    for _ in 0..n {
                        self.print_graphic(ch);
                    }
                }
            }
            'n' => match p0() {
                5 => self.reply(b"\x1b[0n"),
                6 => {
                    let row = self.grid.cursor.row.saturating_add(1);
                    let col = self.grid.cursor.col.saturating_add(1);
                    self.reply(format!("\x1b[{row};{col}R").as_bytes());
                }
                _ => {}
            },
            'c' => {
                // Primary DA — VT220 with 132-columns (62) + printer port (c).
                let p = p0();
                if p == 0 {
                    self.reply(b"\x1b[?62;c");
                }
            }
            'S' => {
                // CSI Ps S — Scroll Up (SU). Scrolls the active region
                // up by `n` lines, fills bottom with blanks. Dest rows
                // are marked dirty by the grid, which is the fix for
                // (stale LineQuadCache entries after region scroll).
                let n = p0().max(1);
                let (top, bot) = self.effective_scroll_region();
                self.grid.scroll_region_up_with(top, bot, n, self.erase_fill_cell());
            }
            'T' => {
                // CSI Ps T — Scroll Down (SD).
                let n = p0().max(1);
                let (top, bot) = self.effective_scroll_region();
                self.grid.scroll_region_down_with(top, bot, n, self.erase_fill_cell());
            }
            'r' => {
                // CSI Ps ; Ps r — DECSTBM Set Top and Bottom Margins.
                // Both omitted / 0 / out-of-range -> reset to full
                // screen. Cursor moves to home as per spec.
                let rows = self.grid.rows;
                let top_p = p0();
                let bot_p = p1();
                let cur_before = (self.grid.cursor.row, self.grid.cursor.col);
                let new_top = if top_p == 0 { 0 } else { top_p.saturating_sub(1) };
                let new_bot =
                    if bot_p == 0 { rows.saturating_sub(1) } else { bot_p.saturating_sub(1) };
                let (applied_top, applied_bot) = if new_top < new_bot && new_bot < rows {
                    self.scroll_top = Some(new_top);
                    self.scroll_bottom = Some(new_bot);
                    (new_top, new_bot)
                } else {
                    self.scroll_top = None;
                    self.scroll_bottom = None;
                    (0, rows.saturating_sub(1))
                };
                self.grid.goto(0, 0);
                tracing::debug!(
                    target: "sonicterm_vt::stbm",
                    "CSI {top_p};{bot_p}r DECSTBM: parsed=({new_top},{new_bot}), applied=({applied_top},{applied_bot}), grid_rows={rows}, cursor {:?}→(0,0)",
                    cur_before
                );
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.ground = false;
        self.sequence_dispatched = true;
        let code = params
            .first()
            .and_then(|s| std::str::from_utf8(s).ok())
            .and_then(|s| s.parse::<u16>().ok());
        match code {
            Some(0) | Some(2) => {
                if let Some(text) = params.get(1).and_then(|s| std::str::from_utf8(s).ok()) {
                    self.title = Some(text.to_string());
                    self.events.push(VtEvent::SetTitle(text.to_string()));
                }
            }
            Some(7) => {
                // OSC 7 ; file://<host>/<path> ST — shell-reported cwd.
                // Used by the tab-title renderer to show `parent/leaf`.
                // We are permissive: accept the raw payload even when it
                // doesn't start with `file://` (some shells skip the
                // scheme), strip the host component when present, and
                // percent-decode the path so spaces/unicode survive.
                if let Some(raw) = params.get(1).and_then(|s| std::str::from_utf8(s).ok()) {
                    let path = parse_osc7_cwd(raw);
                    if !path.is_empty() {
                        self.cwd = Some(path);
                    }
                }
            }
            Some(8) => {
                // OSC 8;params;uri ST — hyperlink. Empty uri = end of link.
                let id = params.get(1).and_then(|s| std::str::from_utf8(s).ok());
                let uri = params.get(2).and_then(|s| std::str::from_utf8(s).ok());
                if let Some(uri) = uri {
                    let id_norm = id.filter(|s| !s.is_empty());
                    if uri.is_empty() {
                        self.current_hyperlink = None;
                        self.events.push(VtEvent::Hyperlink {
                            id: id_norm
                                .filter(|value| value.len() <= MAX_HYPERLINK_CLIENT_ID_BYTES)
                                .map(String::from),
                            uri: String::new(),
                        });
                    } else {
                        let mut admission = self.hyperlinks.intern_or_reject(id_norm, uri);
                        // Only a rejection that reclamation could plausibly
                        // relieve is worth a grid scan. An oversized URI is
                        // refused by a size check no sweep can change, so
                        // retrying it burns an O(visible + scrollback) walk to
                        // reach the same answer — once per link, for as long
                        // as the shell keeps emitting them.
                        if admission
                            .as_ref()
                            .err()
                            .is_some_and(|reason| reason.is_retryable_after_reclaim())
                        {
                            // The registry is full. Almost all of it is links
                            // whose cells scrolled away, so reclaim and retry
                            // rather than leaving this and every later link
                            // dead for the rest of the session.
                            //
                            // Swept before `current_hyperlink` is reassigned,
                            // so the span the parser still considers open is a
                            // live root even though no cell holds it yet.
                            //
                            // Backoff bounds the scan: when the links really
                            // are all still on screen the sweep frees nothing,
                            // and retrying per link would scan the whole grid
                            // per link.
                            //
                            // It is released as soon as scrollback evicts a
                            // row, because that is the event that turns live
                            // links into garbage. A blind countdown outlived
                            // the state it was set on: scrolling every link
                            // out of history made the whole registry
                            // reclaimable while the next links still took the
                            // skip branch and rendered as plain text.
                            let evicted = self.grid.scrollback_evicted();
                            if evicted != self.hyperlink_reclaim_scanned_at {
                                self.hyperlink_reclaim_backoff = 0;
                            }
                            if self.hyperlink_reclaim_backoff == 0 {
                                let freed = self.reclaim_hyperlinks();
                                self.hyperlink_reclaim_scanned_at = evicted;
                                self.hyperlink_reclaim_backoff =
                                    if freed > 0 { 0 } else { HYPERLINK_RECLAIM_BACKOFF_LINKS };
                                if freed > 0 {
                                    tracing::debug!(
                                        target: "memory",
                                        freed,
                                        retained = self.hyperlinks.len(),
                                        retained_bytes = self.hyperlinks.retained_bytes(),
                                        "reclaimed unreferenced hyperlinks"
                                    );
                                    admission = self.hyperlinks.intern_or_reject(id_norm, uri);
                                }
                            } else {
                                self.hyperlink_reclaim_backoff -= 1;
                            }
                        }
                        self.current_hyperlink = admission.as_ref().ok().copied();
                        if self.current_hyperlink.is_some() {
                            self.events.push(VtEvent::Hyperlink {
                                id: id_norm.map(String::from),
                                uri: uri.to_string(),
                            });
                        } else {
                            self.events.push(VtEvent::Hyperlink { id: None, uri: String::new() });
                            tracing::warn!(
                                target: "memory",
                                uri_bytes = uri.len(),
                                id_bytes = id_norm.map_or(0, str::len),
                                retained = self.hyperlinks.len(),
                                reason = admission
                                    .as_ref()
                                    .err()
                                    .map_or("unknown", |reason| reason.code()),
                                "OSC 8 hyperlink rejected by memory limits"
                            );
                        }
                    }
                }
            }
            Some(4) => {
                // OSC 4 ; <index> ; ? ST — query a palette colour. xterm
                // allows multiple `index ; spec` pairs in one OSC 4, so we
                // walk the params two at a time. A `?` spec is a query → reply
                // `ESC ] 4 ; <index> ; rgb:RRRR/GGGG/BBBB ST` (16-bit channels,
                // xterm canonical). A non-`?` spec would be a *set*, which we
                // don't implement yet (theme owns the palette) — skip it.
                //
                // Some CLIs (GitHub Copilot's prompt frame) gate their richer
                // UI on being able to read backgroundSecondary via the full
                // OSC 4 palette + OSC 10/11 set; without these replies they
                // treat SonicTerm as colourless and disable the frame.
                //
                // vte 0.15 keeps the full OSC payload in a private buffer but
                // exposes only MAX_OSC_PARAMS split params here. Parser::advance
                // therefore captures raw OSC 4 bytes in parallel and handles
                // full 16-colour batch queries before this capped callback. Keep
                // this path as a fallback for synthetic/direct Performer tests
                // for any capture miss, but avoid duplicate replies.
                if self.suppress_next_osc4 {
                    self.suppress_next_osc4 = false;
                    return;
                }
                self.handle_osc4_pairs(params, bell_terminated);
            }
            Some(code @ 10..=12) => {
                // OSC 10/11/12 ; ? ST — query default fg/bg/cursor colour.
                // Reply format (xterm): `ESC ] N ; rgb:RRRR/GGGG/BBBB ST`
                // where each channel is duplicated to 16 bits (xterm
                // canonical form, accepted by every consumer including
                // nvim). Terminator matches the request's terminator
                // (BEL → BEL, ST → ST) so we don't surprise the client.
                //
                // Without this reply nvim falls back to a hard-coded
                // guess for the bg (NeoTreeNormal 27,29,30), which
                // doesn't match SonicTerm's actual theme bg — neo-tree
                // icon cells (painted with `bg=NONE`) then visibly
                // differ from the surrounding theme-clear surface.
                // OSC 10/11/12 *set* (payload is a colour, not `?`)
                // is intentionally not implemented yet — diagnosis
                // shows query-reply is sufficient to fix.
                let payload = params.get(1).and_then(|s| std::str::from_utf8(s).ok());
                if payload != Some("?") {
                    return;
                }
                let rgb = match code {
                    10 => self.theme_fg,
                    11 => self.theme_bg,
                    12 => self.theme_cursor.or(self.theme_fg),
                    _ => None,
                };
                let Some((r, g, b)) = rgb else { return };
                let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
                let mut buf = Vec::with_capacity(24);
                buf.extend_from_slice(b"\x1b]");
                buf.extend_from_slice(code.to_string().as_bytes());
                buf.extend_from_slice(
                    format!(";rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}").as_bytes(),
                );
                buf.extend_from_slice(terminator);
                self.reply(&buf);
            }
            Some(52) => {
                let sel = params.get(1).and_then(|s| s.first().copied()).unwrap_or(b'c') as char;
                let data = params
                    .get(2)
                    .and_then(|s| std::str::from_utf8(s).ok())
                    .unwrap_or_default()
                    .to_string();
                self.events.push(VtEvent::Clipboard { selection: sel, data });
            }
            Some(1337) => {
                let payload = join_osc_params(params.get(1..).unwrap_or(&[]));
                let row = self.grid.cursor.row;
                let col = self.grid.cursor.col;
                if let Some(event) = parse_iterm2_file_event(payload.as_bytes(), row, col) {
                    self.events.push(VtEvent::Media(event));
                }
            }
            Some(133) => {
                // OSC 133 ; <kind> [; <args>] ST — FinalTerm/WezTerm shell
                // integration. Kinds:
                //   A → prompt start
                //   B → command-line edit start / command start in SonicTerm
                //   C → command output start
                //   D [; exit_code] → command finished
                let kind = params.get(1).and_then(|s| s.first().copied());
                match kind {
                    Some(b'A') => {
                        self.grid.record_prompt_start();
                        self.events.push(VtEvent::Command(CommandEvent::PromptStart));
                    }
                    Some(b'B') | Some(b'C') => {
                        self.events.push(VtEvent::Command(CommandEvent::CmdStart));
                    }
                    Some(b'D') => {
                        let exit_i32 = params
                            .get(2)
                            .and_then(|s| std::str::from_utf8(s).ok())
                            .and_then(|s| s.parse::<i32>().ok());
                        self.grid.record_prompt_end(exit_i32);
                        let exit = exit_i32.and_then(|n| u8::try_from(n).ok());
                        self.events.push(VtEvent::Command(CommandEvent::CmdEnd(exit)));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, action: char) {
        // Entering DCS passthrough — stay out of the fast-path until unhook.
        self.ground = false;
        self.dcs_capture =
            (action == 'q').then(|| MediaCapture::new(MediaProtocol::Sixel, String::new()));
    }
    fn put(&mut self, byte: u8) {
        self.ground = false;
        if let Some(capture) = self.dcs_capture.as_mut() {
            capture.append_byte(byte);
        }
    }
    fn unhook(&mut self) {
        self.ground = false;
        self.sequence_dispatched = true;
        if let Some(capture) = self.dcs_capture.take() {
            if let Some(event) = capture.into_event(self.grid.cursor.row, self.grid.cursor.col) {
                self.events.push(VtEvent::Media(event));
            }
        }
    }
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        self.ground = false;
        self.sequence_dispatched = true;
        self.reset_last_printed_char();
        match byte {
            b'7' => {
                // DECSC — save cursor. Claude Code uses ESC 7 / ESC 8
                // around DECSTBM reset at startup; without this, CSI r
                // leaves the cursor at home and the trust prompt paints
                // over old scrollback instead of starting below the shell
                // prompt.
                self.saved_cursor = Some(self.grid.cursor);
            }
            b'8' => {
                // DECRC — restore cursor saved by DECSC / ?1048.
                if let Some(c) = self.saved_cursor {
                    self.grid.goto(c.row, c.col);
                }
            }
            b'c' => {
                // RIS — Reset to Initial State. TUI launchers such as
                // Claude Code may use this as their first "clean slate"
                // before painting. Ignoring it leaves shell scrollback
                // visually interleaved with the app's first frame.
                self.reset_terminal();
            }
            b'D' => {
                // IND — Index. Move cursor down one line; if at the
                // bottom margin of the scroll region, scroll the
                // region up. Must respect DECSTBM.
                let (top, bot) = self.effective_scroll_region();
                if self.grid.cursor.row == bot {
                    self.grid.scroll_region_up(top, bot, 1);
                } else {
                    let new_row = (self.grid.cursor.row + 1).min(self.grid.rows.saturating_sub(1));
                    let col = self.grid.cursor.col;
                    self.grid.goto(new_row, col);
                }
            }
            b'M' => {
                // RI — Reverse Index. Move cursor up; if at top
                // margin, scroll the region down.
                let (top, bot) = self.effective_scroll_region();
                if self.grid.cursor.row == top {
                    self.grid.scroll_region_down(top, bot, 1);
                } else {
                    let new_row = self.grid.cursor.row.saturating_sub(1);
                    let col = self.grid.cursor.col;
                    self.grid.goto(new_row, col);
                }
            }
            b'E' => {
                // NEL — Next Line. Like IND, but also moves cursor to col 0.
                let (top, bot) = self.effective_scroll_region();
                if self.grid.cursor.row == bot {
                    self.grid.scroll_region_up(top, bot, 1);
                    self.grid.goto(self.grid.cursor.row, 0);
                } else {
                    let new_row = (self.grid.cursor.row + 1).min(self.grid.rows.saturating_sub(1));
                    self.grid.goto(new_row, 0);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "vt_tests.rs"]
mod vt_tests;
