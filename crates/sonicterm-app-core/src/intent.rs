//! Winit-agnostic intent enum: what the platform shell hands to the
//! state machine as input. The state machine reduces an Intent into
//! zero or more `AppEffect`s (see `effect.rs`).
//!
//! Per spec §1 (M6a-expand-2 FINAL, #429): 63 variants. Payload types
//! are all pure data — zero winit / wgpu / glyphon / cosmic-text refs.
//!
//! M6a-expand-2a: enum + payload type surface. The per-Intent reducer
//! logic ships in 2b/2c; the stub reducer in `state_machine.rs`
//! returns an empty Effect batch for every variant.

use bytes::Bytes;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Instant;

use sonicterm_types::{ModKey, Pos, WindowKey};

use crate::supporting::{
    BroadcastScope, KeyCode, LogicalPos, MouseButton, PaletteChoice, PaneId,
    PendingDragOutcomeCore, PtyConfig, SplitDir, WindowRole,
};

/// Why a redraw was requested. The platform layer may use this to
/// coalesce (see LM-002 in CLAUDE.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedrawReason {
    /// New bytes arrived from a PTY.
    PtyBytes,
    /// User keystroke (immediate paint).
    UserInput,
    /// Window resize or DPI change.
    SurfaceChange,
    /// Cursor blink tick.
    CursorBlink,
    /// Theme or font reload.
    ConfigReload,
    /// Focus change.
    Focus,
    /// Layout change (split / pane add or remove).
    Layout,
    /// Selection visual change.
    Selection,
    /// Hover URL latch changed.
    Hover,
    /// Scroll viewport changed.
    Scroll,
    /// IME composition state changed.
    Ime,
    /// Overlay (search / palette) state changed.
    Overlay,
    /// Tab added.
    TabAdded,
    /// Tab removed.
    TabRemoved,
    /// Tab switched.
    TabSwitch,
    /// PTY burst coalesced flush.
    PtyBurst,
    /// Vsync-driven repaint.
    Vsync,
    /// Cursor / proc-name blink phase tick.
    Blink,
    /// Title or tab strip changed.
    TitleOrTab,
    /// Resize-only path.
    Resize,
}

/// Sub-mode of an active selection drag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionMode {
    /// Cell-by-cell selection.
    Cell,
    /// Snap to word boundaries.
    Word,
    /// Whole-line selection.
    Line,
    /// Rectangular (block) selection.
    Block,
}

/// Inputs to the state machine. See spec §1 for the full mapping
/// table; reducer arms land in M6a-expand-2b/2c.
#[derive(Debug, Clone)]
#[non_exhaustive]
#[allow(missing_docs)] // Each variant carries its own doc comment.
pub enum AppIntent {
    // ── Window lifecycle (6) ────────────────────────────────────────
    /// 01 — User asked to create a new top-level window with given role.
    NewWindow { role: WindowRole },
    /// 02 — OS reports a window we own has been requested to close.
    WindowCloseRequested { window: WindowKey },
    /// 03 — OS reports a window we own gained focus.
    WindowFocused { window: WindowKey },
    /// 04 — OS reports a window we own lost focus.
    WindowBlurred { window: WindowKey },
    /// 05 — OS reports a window resize (pre-DPI scaling already applied at boundary).
    WindowResized { window: WindowKey, cols: u16, rows: u16 },
    /// 06 — OS reports a window move.
    WindowMoved { window: WindowKey, pos: LogicalPos },

    // ── Tab lifecycle (6) ───────────────────────────────────────────
    /// 07 — New tab in target window with optional starting cwd.
    NewTab { window: WindowKey, cwd: Option<PathBuf> },
    /// 08 — Close tab by index in window.
    CloseTab { window: WindowKey, idx: usize },
    /// 09 — Switch active tab to the next one (wraps).
    NextTab { window: WindowKey },
    /// 10 — Switch active tab to the previous one (wraps).
    PrevTab { window: WindowKey },
    /// 11 — Switch active tab to absolute index.
    GoToTab { window: WindowKey, idx: usize },
    /// 12 — User dragged a tab out of its window.
    TearOutTab { src_window: WindowKey, src_tab: usize },

    // ── Pane lifecycle / navigation (7) ─────────────────────────────
    /// 13 — Split the active pane in active tab in given direction.
    SplitPane { window: WindowKey, dir: SplitDir },
    /// 14 — Close active pane in active tab; tab closes if last.
    ClosePane { window: WindowKey },
    /// 15 — Resize active pane in given direction by `cells` cells.
    ResizePane { window: WindowKey, dir: SplitDir, cells: u16 },
    /// 16 — Focus the pane to the left of the active pane.
    FocusPaneLeft { window: WindowKey },
    /// 17 — Focus the pane to the right of the active pane.
    FocusPaneRight { window: WindowKey },
    /// 18 — Focus the pane above the active pane.
    FocusPaneUp { window: WindowKey },
    /// 19 — Focus the pane below the active pane.
    FocusPaneDown { window: WindowKey },

    // ── PTY (4) ─────────────────────────────────────────────────────
    /// 20 — VT-thread observed a new generation of bytes for pane.
    PtyBurst { pane: PaneId, generation: u64 },
    /// 21 — PTY child exited with status.
    PtyExit { pane: PaneId, status: i32 },
    /// 22 — Explicit write of bytes into a pane's PTY.
    PtyWrite { pane: PaneId, bytes: Bytes },
    /// 23 — Foreground process snapshot tick produced an update for a pane.
    ForegroundProcChanged { pane: PaneId, name: Option<String> },

    // ── Keyboard / IME (5) ──────────────────────────────────────────
    /// 24 — One keyboard key transition for the focused pane of `window`.
    Key { window: WindowKey, code: KeyCode, mods: ModKey, pressed: bool },
    /// 25 — IME began composing in window.
    ImeStart { window: WindowKey },
    /// 26 — IME pre-edit text update.
    ImePreedit { window: WindowKey, text: String, cursor: Range<usize> },
    /// 27 — IME commit — text is final and is written to PTY.
    ImeCommit { window: WindowKey, text: String },
    /// 28 — IME composition ended without commit.
    ImeEnd { window: WindowKey },

    // ── Mouse (4 — MouseDown+Up collapsed per spec §0) ──────────────
    /// 29 — Mouse button transition. `pressed = true` is down, `false` is up.
    MouseButton {
        window: WindowKey,
        pressed: bool,
        button: MouseButton,
        mods: ModKey,
        pos: LogicalPos,
    },
    /// 30 — Mouse moved.
    MouseMove { window: WindowKey, pos: LogicalPos },
    /// 31 — Mouse wheel / trackpad scroll delta.
    MouseWheel { window: WindowKey, dy: f64, dx: f64, mods: ModKey },
    /// 32 — Hover URL (set/clear).
    HoverUrl { window: WindowKey, url: Option<String> },

    // ── Scrollback (7) ──────────────────────────────────────────────
    /// 33 — Scroll active pane up by `lines`.
    ScrollUp { window: WindowKey, lines: u32 },
    /// 34 — Scroll active pane down by `lines`.
    ScrollDown { window: WindowKey, lines: u32 },
    /// 35 — Scroll one page up.
    ScrollPageUp { window: WindowKey },
    /// 36 — Scroll one page down.
    ScrollPageDown { window: WindowKey },
    /// 37 — Scroll to oldest scrollback row.
    ScrollToTop { window: WindowKey },
    /// 38 — Scroll back to the live (cursor-following) bottom.
    ScrollToBottom { window: WindowKey },
    /// 39 — Reset viewport so cursor is in view.
    ScrollToCursor { window: WindowKey },

    // ── Selection / copy mode / clipboard (6) ───────────────────────
    /// 40 — Begin selection at pos.
    SelectionStart { window: WindowKey, anchor: Pos, mode: SelectionMode },
    /// 41 — Extend selection to pos.
    SelectionExtend { window: WindowKey, to: Pos },
    /// 42 — Finalize selection.
    SelectionEnd { window: WindowKey },
    /// 43 — Clear selection.
    ClearSelection { window: WindowKey },
    /// 44 — Copy current selection to clipboard.
    CopySelection { window: WindowKey },
    /// 45 — Paste text into focused pane.
    Paste { window: WindowKey, text: String, bracketed: bool },

    // ── Search overlay (4) ──────────────────────────────────────────
    /// 46 — Open search overlay against active pane scrollback.
    OpenSearch { window: WindowKey },
    /// 47 — Update query; reducer recomputes hit list.
    SearchQuery { window: WindowKey, q: String },
    /// 48 — Move to next/prev hit.
    SearchStep { window: WindowKey, forward: bool },
    /// 49 — Close search overlay.
    CloseSearch { window: WindowKey },

    // ── Command palette (4) ─────────────────────────────────────────
    /// 50 — Open/close palette toggle.
    ToggleCommandPalette { window: WindowKey },
    /// 51 — Update palette filter string.
    PaletteFilter { window: WindowKey, filter: String },
    /// 52 — Move highlight by `delta`.
    PaletteStep { window: WindowKey, delta: i32 },
    /// 53 — Activate highlighted item; reducer cascades the resulting Intent.
    PaletteSubmit { window: WindowKey, choice: PaletteChoice },

    // ── OS drag / drop (2) ──────────────────────────────────────────
    /// 54 — A platform-initiated OS drag resolved with a winit-laundered outcome.
    OsDragOutcome(PendingDragOutcomeCore),
    /// 55 — Files were dropped onto a window from the OS file manager.
    FilesDropped { window: WindowKey, paths: Vec<PathBuf> },

    // ── Hyperlinks (1) ──────────────────────────────────────────────
    /// 56 — User clicked a hyperlink in the grid.
    ClickUrl { window: WindowKey, url: String },

    // ── Config / theming (3) ────────────────────────────────────────
    /// 57 — Config file watcher saw a change.
    ConfigChanged { new: Box<PtyConfig> },
    /// 58 — Switch active theme by name.
    ApplyTheme { name: String },
    /// 59 — Adjust font size by signed delta.
    FontSizeDelta { delta: i32 },

    // ── Broadcast input (1) ─────────────────────────────────────────
    /// 60 — Change broadcast scope.
    SetBroadcastScope { scope: BroadcastScope },

    // ── Frame timing / lifecycle (3) ────────────────────────────────
    /// 61 — Compositor requests a redraw.
    RedrawRequested { window: WindowKey },
    /// 62 — Periodic tick.
    Tick { now: Instant },
    /// 63 — User requested application exit.
    Exit,
}
