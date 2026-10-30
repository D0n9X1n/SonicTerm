//! VT/ANSI parser. The `vte` crate (the lexer alacritty also uses) tokenizes
//! the byte stream; this module turns its callbacks into mutations of a
//! [`sonicterm_grid::grid::Grid`], replies to terminal queries, and
//! [`VtEvent`]s for the application.
//!
//! It owns escape-sequence dispatch, terminal mode state, the reply path, and
//! size-bounded capture of media payloads (Sixel, kitty graphics, and iTerm2
//! inline images). Untrusted sequences and payloads are held to fixed byte
//! ceilings. The supported protocols and their user-visible behavior are
//! listed in the tracked `wiki/Terminal-IO-and-VT.md` page rather than
//! restated here, so the list lives in one place.

use std::{collections::HashSet, sync::Arc};

use crossbeam_channel::Sender;
use sonicterm_grid::{
    grid::{Cell, CellFlags, Color, Grid, Pos, UnderlineStyle},
    hyperlink::{HyperlinkId, HyperlinkRegistry, MAX_HYPERLINK_CLIENT_ID_BYTES},
};
// Governor accounting type, consumed rather than republished.
use sonicterm_types::ResourceAmount;
use vte::{Params, Perform};

mod program;
mod staging;

pub use program::{ProgressReport, ProgressState, MAX_NOTIFY_BYTES};
use staging::StagingReservation;
pub use staging::{
    CaptureStagingPool, GUARANTEED_CONCURRENT_CAPTURES, MAX_PROCESS_CAPTURE_STAGING_BYTES,
    MIN_CAPTURE_STAGING_BYTES,
};

/// Terminal identity reported in answer to CSI > q (XTVERSION): `SonicTerm`
/// followed by the crate's release version, which the workspace sets.
pub const SONIC_VERSION: &str = concat!("SonicTerm ", env!("CARGO_PKG_VERSION"));

/// The XTVERSION reply text: `kitty` followed by [`SONIC_VERSION`].
///
/// Clients such as Claude Code send kitty graphics only when the reply is `kitty` or starts with
/// `kitty `. The word names the graphics protocol SonicTerm answers; no kitty version is claimed.
pub const XTVERSION_REPLY: &str = concat!("kitty SonicTerm ", env!("CARGO_PKG_VERSION"));

/// Current DEC mouse tracking mode selected by the terminal application.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum MouseTracking {
    /// Mouse tracking is disabled.
    #[default]
    Off,
    /// Report button presses and releases.
    Button,
    /// Report button events and motion while a button is held.
    ButtonMotion,
    /// Report button events and all pointer motion.
    AnyMotion,
}

/// Terminal modes that decide how host pointer events are routed and encoded.
///
/// Packed into one byte so the VT worker can publish all five bits with one store
/// and a pointer handler can read them without the parser lock.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct PointerModes {
    tracking: MouseTracking,
    sgr: bool,
    is_alt: bool,
    application_cursor: bool,
}

/// Terminal modes that affect how host keyboard events are encoded for the PTY.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct KeyboardModes {
    application_cursor_keys: bool,
    application_keypad: bool,
    backarrow_key: bool,
    newline: bool,
    modify_other_keys: u8,
    win32_input: bool,
}

const KEYBOARD_PROTOCOL_EPOCH_MAX: u64 = (1 << 48) - 1;

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

/// Rejected OSC 8 links to skip after a sweep that freed nothing.
///
/// A sweep frees nothing only when every interned link is still on screen, in
/// which case the next link is no more likely to find garbage. Waiting this
/// many rejections bounds the scan to roughly one per that many links instead
/// of one per link, while still recovering as soon as content scrolls away.
const HYPERLINK_RECLAIM_BACKOFF_LINKS: u32 = 256;
const MAX_RAW_OSC4_BYTES: usize = 4096;
const MAX_SEMANTIC_OSC_BYTES: usize = 16 * 1024;

/// Whether `code` is a program-state OSC (`9` or `777`) the raw capture owns.
fn is_program_osc(code: u16) -> bool {
    matches!(code, 9 | 777)
}

/// The program-state OSC number `prefix` names: digits then `;`, valued 9 or 777, leading zeros
/// included. The probe holds at most its own few bytes, so this costs nothing per output byte.
fn program_osc_number(prefix: &[u8]) -> Option<u16> {
    let (digits, b";") = prefix.split_at(prefix.len().checked_sub(1)?) else {
        // When: prefix does not end in `;`, the OSC number is not complete yet.
        return None;
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        // When: digits is empty or holds a non-digit, this is no OSC number.
        return None;
    }
    let value = digits.iter().fold(0u32, |value, digit| {
        value.saturating_mul(10).saturating_add(u32::from(digit - b'0'))
    });
    is_program_osc(u16::try_from(value).ok()?).then_some(value as u16)
}

/// The raw capture's whole-payload bound for `code`; a longer payload is refused whole.
fn raw_osc_cap(code: u16) -> usize {
    if is_program_osc(code) {
        MAX_NOTIFY_BYTES
    } else {
        // When: code is a title, CWD or hyperlink, the semantic bound applies.
        MAX_SEMANTIC_OSC_BYTES
    }
}
const MAX_ITERM2_METADATA_BYTES: usize = 1024;
const ITERM2_OSC_PREFIX: &[u8; 10] = b"1337;File=";
const OSC4_PREFIX: &[u8; 2] = b"4;";
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
    /// OSC 9;4 — show this progress on the platform surfaces, or clear it on `None`.
    Progress(Option<ProgressReport>),
    /// OSC 9 text or OSC 777 `notify`, whole and unsanitized; a sequence past
    /// [`MAX_NOTIFY_BYTES`] is refused, never cut.
    Notify {
        /// The OSC 777 title; OSC 9 has none.
        title: Option<String>,
        /// The text.
        body: String,
    },
    /// RIS reset the terminal, so program-reported state (progress) is cleared.
    ProgramStateReset,
}

/// One in-flight media capture and its claim on a staging pool.
///
/// Deliberately not `Clone`: a copy would duplicate `data`, while a cloned
/// reservation claims only a fresh floor share, so the copy could hold staged
/// bytes no pool granted.
#[derive(Debug)]
struct MediaCapture {
    protocol: MediaProtocol,
    metadata: String,
    data: Vec<u8>,
    truncated: bool,
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
    /// This capture's claim on its parser's staging pool, released when the
    /// capture is dropped or destructured into an event.
    reservation: StagingReservation,
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
    /// Prompt ended and editable input began (`OSC 133 ; B`).
    PromptEnd,
    /// Command execution started (`OSC 133 ; C`).
    CmdStart,
    /// Command ended, optionally with an exit code (`OSC 133 ; D ; <code>`).
    CmdEnd(Option<u8>),
}

/// Host-aware working directory reported through OSC 7.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Osc7Cwd {
    /// URI authority from `file://authority/path`; empty for local shorthand.
    pub authority: String,
    /// Percent-decoded filesystem path.
    pub path: String,
}

/// Synchronized output (DEC private mode 2026) as the parser holds it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncState {
    /// Whether the application has an update open (`CSI ? 2026 h`).
    pub set: bool,
    /// Count of reset-to-set transitions; a repeated set keeps the current epoch.
    pub epoch: u64,
    /// Count of set-to-reset transitions, by `CSI ? 2026 l` or RIS; it never decreases.
    pub resets: u64,
}

/// Streaming parser wrapping `vte::Parser` and an internal performer that owns
/// the grid plus the current SGR attributes.
pub struct Parser {
    inner: vte::Parser,
    performer: Performer,
    /// The kitty APC in flight: its bounded control section and, for an image, its payload.
    apc_scratch: Option<Box<vt_kitty_stream::ApcScratch>>,
    /// Set when a kitty event needs the host's reply, ending the advance prefix after it.
    kitty_yield: bool,
    pending_esc: bool,
    raw_osc: Option<RawOsc>,
    iterm2_metadata: [u8; MAX_ITERM2_METADATA_BYTES],
    iterm2_metadata_len: usize,
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

/// SonicTerm-side OSC capture for sequences that vte cannot retain under the
/// protocol's own bound or expose whole through its public callback.
enum RawOsc {
    /// Checking a bounded OSC prefix before choosing vte or media ownership.
    Probe { content: [u8; ITERM2_OSC_PREFIX.len()], content_len: u8 },
    /// Complete title, CWD, hyperlink or program-state (`9`, `777`) payload, independent of vte's
    /// parameter count.
    Semantic { code: u16, content: Vec<u8>, pending_esc: bool },
    /// Capturing bytes after `OSC 4 ;` until BEL or ST.
    Palette { content: Vec<u8> },
    /// Streaming an iTerm2 file payload through the parser's staging reservation.
    Iterm2 { capture: MediaCapture, metadata_done: bool, pending_esc: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeFamily {
    Ground,
    Esc,
    EscIntermediate,
    Csi,
    Osc,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsIgnore,
    DcsPassthrough,
    String,
}

impl Parser {
    /// Build a parser bound to `grid`, with no upstream reply channel — DSR /
    /// XTVERSION queries will be silently dropped. Captures stage in
    /// [`CaptureStagingPool::process_default`].
    pub fn new(grid: Grid) -> Self {
        Self::with_performer(Performer::new(grid, None, CaptureStagingPool::process_default()))
    }

    /// Construct a parser that can send replies (DSR, DA, XTVERSION, focus
    /// reporting) back to the pty via the given channel. Captures stage in
    /// [`CaptureStagingPool::process_default`].
    pub fn new_with_reply(grid: Grid, reply_tx: Sender<Vec<u8>>) -> Self {
        Self::with_performer(Performer::new(
            grid,
            Some(reply_tx),
            CaptureStagingPool::process_default(),
        ))
    }

    /// Construct a parser whose media captures stage in `staging_pool` rather
    /// than [`CaptureStagingPool::process_default`], with an optional reply
    /// channel.
    ///
    /// For tests: a private pool keeps a test's admission and accounting
    /// measurements free of captures that sibling tests hold elsewhere.
    pub fn new_with_staging_pool(
        grid: Grid,
        reply_tx: Option<Sender<Vec<u8>>>,
        staging_pool: Arc<CaptureStagingPool>,
    ) -> Self {
        Self::with_performer(Performer::new(grid, reply_tx, staging_pool))
    }

    /// Wrap `performer` in a parser whose capture and escape state starts at ground.
    fn with_performer(performer: Performer) -> Self {
        Self {
            inner: vte::Parser::new(),
            performer,
            apc_scratch: None,
            kitty_yield: false,
            pending_esc: false,
            raw_osc: None,
            iterm2_metadata: [0; MAX_ITERM2_METADATA_BYTES],
            iterm2_metadata_len: 0,
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
    pub fn set_theme_fg(&mut self, red: u8, green: u8, blue: u8) {
        self.performer.theme_fg = Some((red, green, blue));
    }

    /// Tell the parser the theme default background colour. Used to answer
    /// `OSC 11 ; ? ST` queries (see [`Parser::set_theme_fg`]).
    pub fn set_theme_bg(&mut self, red: u8, green: u8, blue: u8) {
        self.performer.theme_bg = Some((red, green, blue));
    }

    /// Tell the parser the theme cursor colour. Used to answer
    /// `OSC 12 ; ? ST` queries. When unset, OSC 12 falls back to the
    /// theme foreground.
    pub fn set_theme_cursor(&mut self, red: u8, green: u8, blue: u8) {
        self.performer.theme_cursor = Some((red, green, blue));
    }

    /// Seed one slot (0..=15) of the 16-colour ANSI palette used to answer
    /// `OSC 4 ; <index> ; ? ST` queries. Indices map to the standard xterm
    /// layout: 0..=7 normal, 8..=15 bright. Some CLIs (e.g. GitHub Copilot)
    /// require the full palette query reply to enable their richer prompt
    /// frame — without it they treat the terminal as colourless.
    pub fn set_theme_palette_color(&mut self, index: u8, red: u8, green: u8, blue: u8) {
        if (index as usize) < self.performer.theme_palette.len() {
            self.performer.theme_palette[index as usize] = Some((red, green, blue));
        }
    }

    /// Whether DECSET ?1004 (focus reporting) is currently enabled. App should
    /// send `\e[I` / `\e[O` on focus in/out when this is true.
    pub fn focus_reporting_enabled(&self) -> bool {
        self.performer.focus_reporting
    }

    /// Borrow the underlying [`Grid`] — used by the renderer to read cells.
    pub fn grid(&self) -> &Grid {
        &self.performer.grid
    }

    /// Mutably borrow the grid for cell and history operations; use [`Self::resize`] for geometry.
    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.performer.grid
    }

    /// Resize the grid and reset scrolling margins only when bounded cell dimensions change.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let before = (self.performer.grid.cols, self.performer.grid.rows);
        self.performer.grid.resize(cols, rows);
        if before != (self.performer.grid.cols, self.performer.grid.rows) {
            // Absolute margins must not survive a change to the bounded screen dimensions.
            self.performer.scroll_top = None;
            self.performer.scroll_bottom = None;
        }
    }

    /// Borrow the hyperlink registry (OSC 8 interned uris).
    pub fn hyperlinks(&self) -> &HyperlinkRegistry {
        &self.performer.hyperlinks
    }

    /// Currently-active hyperlink id, if any.
    pub fn current_hyperlink(&self) -> Option<HyperlinkId> {
        self.performer.current_hyperlink
    }

    /// Latest OSC 0/2 window title (sticky), or `None` if no title has been
    /// set. Used by the tab bar to label tabs with the shell's reported title.
    pub fn title(&self) -> Option<&str> {
        self.performer.title.as_deref()
    }

    /// Latest OSC 7 working-directory path (sticky), or `None` if the shell
    /// has not reported one. Kept for tab-title compatibility; security-sensitive
    /// consumers must use [`Self::osc7_cwd`] so authority provenance is retained.
    pub fn cwd(&self) -> Option<&str> {
        self.performer.cwd.as_deref()
    }

    /// Latest host-aware OSC 7 working-directory snapshot.
    pub fn osc7_cwd(&self) -> Option<&Osc7Cwd> {
        self.performer.osc7_cwd.as_ref()
    }

    /// Monotonic identity of the latest accepted OSC 7 update.
    pub fn cwd_revision(&self) -> u64 {
        self.performer.cwd_revision
    }
}

/// Constant-size classification of the only supported DECRQSS selector.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DecrqssRequest {
    Empty,
    Sgr,
    Unsupported,
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
    /// DECKPAM/DECKPNM application-keypad selection.
    app_keypad: bool,
    /// DECBKM ?67 — Backspace selects BS while set and DEL while reset.
    backarrow_key: bool,
    /// ANSI mode 20 — Return sends CR LF while set.
    newline_mode: bool,
    /// xterm `modifyOtherKeys` level selected by `CSI > 4 ; Ps m`.
    modify_other_keys: u8,
    /// DEC9001 is pane-global even though Kitty flags belong to each screen.
    win32_input: bool,
    /// Saturates at the packed 48-bit sentinel; RIS never reuses an old generation.
    keyboard_protocol_epoch: u64,
    /// DECSET ?1000/?1002/?1003 mouse tracking selected by the application.
    mouse_tracking: MouseTracking,
    focus_reporting: bool,
    /// DEC 2026: an application update is open and should be presented only once it ends.
    synchronized_output: bool,
    /// Reset-to-set transitions of `synchronized_output`; see [`SyncState::epoch`].
    sync_epoch: u64,
    /// Set-to-reset transitions of `synchronized_output`; see [`SyncState::resets`].
    sync_resets: u64,
    /// Latest OSC 0/2 title (sticky — survives consumed events).
    title: Option<String>,
    /// Latest permissively-decoded OSC 7 path used only for tab titles.
    cwd: Option<String>,
    /// Latest strictly parsed host-aware snapshot used for path resolution.
    osc7_cwd: Option<Osc7Cwd>,
    /// Monotonic identity of OSC 7 updates, including same-value ABA changes.
    cwd_revision: u64,
    reply_tx: Option<Sender<Vec<u8>>>,
    captured_replies: std::cell::RefCell<Option<Vec<u8>>>,
    reply_queue_full_warned: std::sync::atomic::AtomicBool,
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
    /// Whether direct ASCII printing may bypass vte.
    ///
    /// [`Parser::escape_family`] owns sequence boundaries. This flag only stays
    /// false while vte must retain byte-level ownership, including partial UTF-8
    /// and DCS passthrough.
    fast_path_ready: bool,
    /// Most-recently-printed graphic character, for CSI `b` (REP).
    /// ECMA-48: REP repeats the GRAPHIC CHARACTER immediately preceding
    /// REP in the data stream. Reset when a control function intervenes.
    last_printed_char: Option<char>,
    dcs_capture: Option<MediaCapture>,
    /// Pool this parser's media captures stage in.
    staging_pool: Arc<CaptureStagingPool>,
    /// A chunked kitty transfer spanning APCs, holding the parser's one staging reservation.
    ///
    /// Owned here rather than by the parser so the Sixel hook can end it before reserving.
    kitty_transfer: Option<vt_kitty_stream::KittyTransfer>,
    /// Query selectors never retain terminal-controlled payload bytes.
    decrqss: Option<DecrqssRequest>,
    /// vte unhooks at ESC, before the following byte proves an ST terminator.
    decrqss_awaiting_backslash: bool,
    /// Kitty keyboard flag stacks for the main and alternate screens.
    ///
    /// Each screen owns independent progressive-enhancement state, as required
    /// by the protocol. Empty stack means flags 0 and legacy encoding.
    kitty_kbd_flags: [Vec<u8>; 2],
}

/// Maximum depth of the kitty keyboard flag stack. The protocol allows nested
/// push/pop but a misbehaving app must not be able to grow it without bound.
const KITTY_KBD_STACK_MAX: usize = 32;

fn split_once_byte(bytes: &[u8], needle: u8) -> Option<(&[u8], &[u8])> {
    let pos = bytes.iter().position(|byte| *byte == needle)?;
    Some((&bytes[..pos], &bytes[pos + 1..]))
}

#[cfg(test)]
#[path = "vt_tests.rs"]
mod vt_tests;

#[path = "vt_modes.rs"]
mod vt_modes;

#[path = "vt_stream.rs"]
mod vt_stream;

#[path = "vt_capture.rs"]
mod vt_capture;

#[path = "vt_dispatch.rs"]
mod vt_dispatch;

#[path = "vt_osc.rs"]
mod vt_osc;

#[path = "vt_performer.rs"]
mod vt_performer;

#[path = "vt_raw_osc.rs"]
mod vt_raw_osc;

#[path = "vt_kitty.rs"]
mod vt_kitty;

#[path = "vt_kitty_stream.rs"]
mod vt_kitty_stream;
pub use vt_kitty::{KittyAction, KittyCommand, KittyCommandError, KittyDelete, KittyMedium};

use vt_capture::parse_iterm2_file_event;
pub use vt_osc::{parse_osc7_cwd, parse_osc7_cwd_snapshot};

#[cfg(test)]
#[path = "vt_test_support.rs"]
mod vt_test_support;
