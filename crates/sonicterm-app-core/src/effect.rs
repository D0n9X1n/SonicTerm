//! Outputs of the state machine: side-effects the platform shell
//! must perform after a reduce.
//!
//! Per spec §2 (M6a-expand-2 FINAL, #429): 22 variants grouped into
//! the 7-class ordering used by `AppStateMachine::dispatch`.

use bytes::Bytes;
use std::time::Instant;

use sonicterm_types::WindowKey;

use crate::intent::RedrawReason;
use crate::supporting::{LogicalPos, LogicalSize, MenuModel, PaneId, WindowRole};

/// Severity for `AppEffect::LogEvent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    /// Trace-level diagnostics.
    Trace,
    /// Debug-level diagnostics.
    Debug,
    /// Informational message.
    Info,
    /// Warning message.
    Warn,
    /// Error message.
    Error,
}

/// Outputs of `AppStateMachine::dispatch`. The state machine sorts
/// the batch by `EffectClass` before returning (`sort_by_key`, stable).
///
/// See spec §6 for the canonical class ordering table.
#[derive(Debug, Clone)]
#[non_exhaustive]
#[allow(missing_docs)] // Each variant carries its own doc comment.
pub enum AppEffect {
    // ── PTY (class 0) ───────────────────────────────────────────────
    /// 01 — Write bytes into a pane's PTY (zero-copy via `bytes::Bytes`).
    PtyWrite { pane: PaneId, data: Bytes },
    /// 02 — Close PTY for pane (graceful shutdown + Drop kills child).
    PtyClose { pane: PaneId },

    // ── Render (class 1) ────────────────────────────────────────────
    /// 03 — Request a full redraw of `window`.
    Render { window: WindowKey, reason: RedrawReason },
    /// 04 — Request a partial redraw of a rectangular cell region.
    RenderDirtyRect { window: WindowKey, top: u32, left: u16, rows: u16, cols: u16 },

    // ── OS drag (class 2) ───────────────────────────────────────────
    /// 05 — Begin a platform OS drag (tab tear-out across windows / apps).
    OsDragStart { src_window: WindowKey, payload_tab: u64 },
    /// 06 — End the active OS drag.
    OsDragEnd { src_window: WindowKey, committed: bool },

    // ── Clipboard / side channels (class 3) ─────────────────────────
    /// 07 — Set clipboard contents.
    ClipboardSet { text: String },
    /// 08 — Request clipboard contents asynchronously.
    ClipboardRequest { window: WindowKey, bracketed: bool },
    /// 18 — Push an OS notification.
    Notification { title: String, body: String },
    /// 19 — Open a URL via the OS default handler (validated by sonic_cfg::url_open).
    OpenURL { url: String },

    // ── Window ops (class 4) ────────────────────────────────────────
    /// 09 — Create a new top-level platform window.
    WindowOpen { role: WindowRole, initial_size: Option<LogicalSize> },
    /// 10 — Close a top-level platform window.
    WindowClose { window: WindowKey },
    /// 11 — Resize a window programmatically.
    WindowResize { window: WindowKey, size: LogicalSize },
    /// 12 — Move a window programmatically.
    WindowMove { window: WindowKey, pos: LogicalPos },
    /// 13 — Set window title.
    WindowSetTitle { window: WindowKey, title: String },
    /// 15 — Spawn a PTY-backed child shell.
    ChildSpawn { pane: PaneId, argv0: String },
    /// 16 — Propagate a child exit to higher layers.
    ChildExitPropagate { pane: PaneId, status: i32 },
    /// 17 — Begin graceful app quit.
    Quit,
    /// 20 — Schedule a wake-up tick `at`.
    TimerSchedule { id: u64, at: Instant },
    /// 21 — Cancel a previously scheduled timer.
    TimerCancel { id: u64 },

    // ── Menubar (class 5) ───────────────────────────────────────────
    /// 14 — Rebuild the application menubar from a fresh `MenuModel`.
    MenubarUpdate(MenuModel),

    // ── Log (class 6) ───────────────────────────────────────────────
    /// 22 — Emit a structured log event from the reducer.
    LogEvent { level: LogLevel, target: &'static str, msg: String },
}

/// Effect ordering class used as a sort key by `AppStateMachine::dispatch`.
///
/// Per spec §6:
/// 0 PtyWrite → 1 Render → 2 OsDrag → 3 Clipboard → 4 WindowOp →
/// 5 MenubarUpdate → 6 Log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum EffectClass {
    /// 0 — Shell side-effect; must happen before frame reflects it.
    PtyWrite = 0,
    /// 1 — Frame paint, post-write world.
    Render = 1,
    /// 2 — OS drag begin/end, depends on freshly rendered frame.
    OsDrag = 2,
    /// 3 — Clipboard / notification / URL — user-perceivable, frame-independent.
    Clipboard = 3,
    /// 4 — Window / child / timer / quit operations.
    WindowOp = 4,
    /// 5 — Menubar mutation (macOS NSMenu reshape last + batched).
    MenubarUpdate = 5,
    /// 6 — Diagnostic; captures final outcome.
    Log = 6,
}

impl AppEffect {
    /// Stable sort key per spec §6. `const fn` so the compiler can
    /// fold it inline; exhaustive match enforces every variant is
    /// classified at compile time.
    #[must_use]
    pub const fn effect_class(&self) -> EffectClass {
        match self {
            AppEffect::PtyWrite { .. } | AppEffect::PtyClose { .. } => EffectClass::PtyWrite,
            AppEffect::Render { .. } | AppEffect::RenderDirtyRect { .. } => EffectClass::Render,
            AppEffect::OsDragStart { .. } | AppEffect::OsDragEnd { .. } => EffectClass::OsDrag,
            AppEffect::ClipboardSet { .. }
            | AppEffect::ClipboardRequest { .. }
            | AppEffect::Notification { .. }
            | AppEffect::OpenURL { .. } => EffectClass::Clipboard,
            AppEffect::WindowOpen { .. }
            | AppEffect::WindowClose { .. }
            | AppEffect::WindowResize { .. }
            | AppEffect::WindowMove { .. }
            | AppEffect::WindowSetTitle { .. }
            | AppEffect::ChildSpawn { .. }
            | AppEffect::ChildExitPropagate { .. }
            | AppEffect::Quit
            | AppEffect::TimerSchedule { .. }
            | AppEffect::TimerCancel { .. } => EffectClass::WindowOp,
            AppEffect::MenubarUpdate(_) => EffectClass::MenubarUpdate,
            AppEffect::LogEvent { .. } => EffectClass::Log,
        }
    }
}
