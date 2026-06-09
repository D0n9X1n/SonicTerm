//! App loop. Owns the window, the GPU renderer, all tab/pane state, the
//! per-pane PTYs and parsers, selection state, and clipboard. Drives keymap
//! dispatch.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use arboard::Clipboard;
use parking_lot::Mutex;
use sonicterm_cfg::config::{BackdropKind, Config};
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::PtyHandle;
use sonicterm_vt::vt::{CommandEvent, Parser};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::ModifiersState,
    window::{Window, WindowAttributes, WindowId},
};

/// Apply WezTerm-style integrated titlebar on macOS.
///
/// The tab bar is now always bottom-pinned, so there is no top tab strip to
/// fuse with the native titlebar. Keep this helper as a no-op compatibility
/// shim so all window creation sites stay in sync.
#[doc(hidden)]
pub fn with_integrated_titlebar(attrs: WindowAttributes) -> WindowAttributes {
    attrs
}

/// Enable OS-window alpha composition when a non-opaque compositor backdrop
/// is requested. Without this, winit creates an opaque client area and the
/// premultiplied swapchain is composited over that instead of Mica/acrylic.
#[doc(hidden)]
pub fn with_backdrop_transparency(
    attrs: WindowAttributes,
    backdrop: BackdropKind,
) -> WindowAttributes {
    if backdrop == BackdropKind::Opaque {
        attrs
    } else {
        attrs.with_transparent(true)
    }
}

use crate::config_watch::ConfigWatcher;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_ui::broadcast::BroadcastState;
use sonicterm_ui::command_palette::CommandPalette;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::ime::ImeState;
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::search::SearchState;
use sonicterm_ui::selection::{SelectMode, Selection};
use sonicterm_ui::tabs::{CommandStatus, Tab, TabBar};

/// A child terminal window spawned by tearing a tab off the bar.
///
/// v2 (review fix): each child window now owns its own `GpuRenderer`
/// bound to the new wgpu surface, plus the per-window interaction
/// state (cursor pos, mouse-down flag, selection) needed to render
/// the grid and route input back to the contained PTY. Single tab,
/// single pane in v2 — tab-bar interactions inside a child (open new
/// tab, close, drag) are intentionally deferred; the child is a
/// "follow-on session window," not a full second App.
///
/// The PTY threads that were spawned for the detached pane keep
/// running across the tear-out; their `redraw_target` Arc is swapped
/// to point at this child's window so output from the shell triggers
/// redraws on the correct surface (otherwise typing in the child
/// would render onto the parent's window, which was the v1 bug).
/// Epic #289 Phase B — kind of window stored in the unified
/// [`App::windows`] map. Today every torn-out terminal child window is
/// `Terminal`.
///
/// Note: the main terminal window's authoritative state still lives
/// directly on `App` (split across `App::tabs`, `App::panes`,
/// `App::renderer`, etc.) pending the Phase C struct-level absorption.
/// Phase B's deliverable is removing the `child_windows` field name
/// and folding torn-out windows under one role-tagged map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRole {
    /// A terminal window (torn-out child today; main + child after
    /// Phase C lands).
    Terminal,
}

#[derive(Debug, Clone)]
pub struct SplitterDragState {
    pub splitter: sonicterm_ui::pane::SplitterId,
    pub axis: sonicterm_ui::pane::SplitAxis,
    pub last_pos: (f32, f32),
}

/// Maximum gap (ms) between consecutive left-presses on the same cell for
/// them to count as a double/triple click. Beyond this the streak resets
/// to a single click.
pub const MULTI_CLICK_MS: u128 = 400;

/// Multi-click counter. Returns the new click count (1, 2, 3, then wraps
/// back to 1 after a triple). A click counts as a continuation when it
/// lands on the same cell within the multi-click interval; otherwise the
/// streak restarts at 1. Pure so it is unit-testable without a real
/// pointer event sequence.
pub fn next_click_count(prev: u8, same_cell: bool, within_interval: bool) -> u8 {
    if same_cell && within_interval && prev >= 1 && prev < 3 {
        prev + 1
    } else {
        1
    }
}

/// Vsync coalescing gate shared by the main-window (`window_event.rs`) and
/// torn-out child-window (`child_window.rs`) `RedrawRequested` arms.
///
/// Returns `true` when a `RedrawRequested` should be DEFERRED to the next
/// frame boundary instead of rendering now. A redraw is deferred only when
/// all three hold:
/// - `!was_dirty` — it is not input-driven (keystroke/resize/theme/IME stay
///   immediate; gating them adds perceptible latency, PR #132).
/// - `!pty_burst` — it does not carry a fresh PTY burst (a new burst always
///   renders so streamed bytes never stall).
/// - `since_last_render < frame_period` — we already drew inside this vsync
///   window, so another draw now would just burn a frame.
///
/// Extracted as a pure fn (Issue #43) so main and child use byte-identical
/// coalescing logic AND it is unit-testable without a winit loop. Deferral
/// is what lets a bursty `ls -al` coalesce to one frame per vsync; on a
/// torn-out child the same gate also stops the render path from busy-spinning
/// and starving the VT thread's parser lock.
#[must_use]
pub fn should_defer_streaming_redraw(
    was_dirty: bool,
    pty_burst: bool,
    since_last_render: std::time::Duration,
    frame_period: std::time::Duration,
) -> bool {
    !was_dirty && !pty_burst && since_last_render < frame_period
}

pub struct WindowState {
    /// Phase B classification — see [`WindowRole`].
    pub role: WindowRole,
    /// Phase B2 PR-B2-0 (#365): promoted from `Arc<Window>` to
    /// `Option<Arc<Window>>` so test seeders can build a `WindowState`
    /// without running `do_resumed`. In production this is `Some(_)`
    /// the moment `do_resumed` (main) or `create_child_window`
    /// (torn-out) finishes; every call site either short-circuits via
    /// `if let Some(w) = ws.window.as_ref()` or early-returns via
    /// `ws.window.as_ref()?` when the window is gone.
    pub window: Option<Arc<Window>>,
    /// Per-window wgpu renderer. `Some(_)` once `do_resumed` (main
    /// window) or `create_child_window` (torn-out) populates it.
    /// PR-B1b (#293): the main window's renderer now lives here too —
    /// the legacy `App.renderer` field was deleted. Read through
    /// [`Self::renderer`] / [`Self::renderer_mut`] which unwrap (always
    /// safe after `do_resumed`).
    pub renderer: Option<GpuRenderer>,
    pub tabs: TabBar,
    pub tab_states: Vec<TabState>,
    pub panes: HashMap<u64, PaneState>,
    pub cursor_pos: (f64, f64),
    pub mouse_down: bool,
    pub selection: Option<Selection>,
    /// Multi-click tracking for word/line selection. `last_click_time` is
    /// the timestamp of the most recent left-press; `last_click_cell` is
    /// the grid cell it landed on; `click_count` is the current streak
    /// (1 = single, 2 = double, 3 = triple, then wraps to 1). Updated via
    /// [`WindowState::register_click`].
    pub last_click_time: Option<Instant>,
    pub last_click_cell: (u16, u16),
    pub click_count: u8,
    /// WezTerm-style drag granularity, set on left-press from the click
    /// count: `Cell` (single), `Word` (double), `Line` (triple). While the
    /// button is held, `CursorMoved` extends the selection at this
    /// granularity. See [`SelectMode`] and `Selection::word_drag` /
    /// `Selection::line_drag`.
    pub select_mode: SelectMode,
    /// The grid cell of the press that started the current drag, as a
    /// scrollback-ABSOLUTE row (so word/line drags stay pinned to the same
    /// TEXT as the viewport scrolls). Word/line drags recompute the anchor
    /// word/line from THIS cell against the live grid on every move (robust
    /// to scrollback), so only the cell — not the resolved word/line bounds
    /// — needs to be retained.
    pub select_anchor: (u64, u16),
    pub copy_mode: Option<CopyModeState>,
    pub modifiers: ModifiersState,
    // PR #400 follow-up: `cursor_visible` moved to `PaneState` (per-pane
    // Arc travels with tear-out). Read from
    // `ws.panes.get(&active_pane).map(|p| p.cursor_visible.load(...))`.
    pub last_render: Instant,
    /// Phase B2 PR-B3b (#365): pointer-cursor-is-link latch. Mirrors
    /// `App.hover_link` (now deleted). Per-window so a torn-out child can
    /// flip its own cursor independently of the main window.
    pub hover_link: bool,
    /// Tab index pressed in the child's bar — same role as
    /// `App::pressed_tab` but for the child window. Used for
    /// drag-from-child merging.
    pub pressed_tab: Option<usize>,
    /// Live drag session for a held-tab gesture in this child window.
    pub drag_session: Option<crate::tab_drag::DragSession>,
    /// Pending cross-window drop target chosen during a drag in the
    /// child's bar; consumed on mouse-up.
    pub drag_target: Option<crate::tab_drag::DropTarget<WindowId>>,
    /// Per-window DPI multiplier retained for renderer rasterization
    /// rebuilds when winit reports monitor changes. Cursor/layout math is
    /// raster-px and must not read this field.
    pub dpi_scale: f64,
    /// Per-window IME composition state. Phase B2 PR-A — promoted from
    /// `App.ime` (main-only) so torn-out windows can compose CJK input
    /// independently. The legacy `App.ime` continues to exist and is
    /// kept in sync on the main window until PR-B.
    pub ime: ImeState,
    /// Phase B2 PR-B3d (#365) — per-window throttle for
    /// `Window::set_ime_cursor_area`. Promoted from `App.ime_cursor_throttle`
    /// so each torn-out window can throttle its own IMK runloop traffic
    /// independently. The legacy field stayed in lock-step on the main
    /// window via the shadow snapshot prior to PR-B3d; with the field
    /// deleted from `App`, every read path now goes through
    /// `self.main()?.ime_cursor_throttle`.
    pub ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle,
    /// Per-window hovered URL (Cmd-held underline + pointer cursor).
    /// Phase B2 PR-A — promoted from `App.hovered_url`. Legacy field
    /// stays in lock-step on the main window until PR-B.
    pub hovered_url: Option<hovered_url::HoveredUrl>,
    /// Phase B2 PR-B4 (#365): "this window is hidden / drained" latch.
    /// Promoted from the App-level `main_hidden` bool so the visibility
    /// state lives next to the `Window` Arc it gates. Today only the main
    /// window flips this to `true` (when its last tab is torn out and
    /// child windows keep the event loop alive); child windows leave it
    /// `false` and reap on empty instead.
    pub hidden: bool,
    /// Active scrollbar-drag gesture (#386 PR-C). `Some(_)` between a
    /// thumb mouse-down and the matching release; cursor moves while
    /// set route to the scrollbar instead of extending a selection.
    pub scrollbar_drag: Option<scrollbar_input::ScrollbarDragState>,
    /// Active split-pane divider drag. While set, cursor moves resize the
    /// captured split ratio instead of extending text selection.
    pub splitter_drag: Option<SplitterDragState>,
    /// Current split-divider hover axis, used to restore the OS cursor when
    /// the pointer leaves the divider.
    pub splitter_hover: Option<sonicterm_ui::pane::SplitAxis>,
    /// Per-pane scrollbar visibility/fade state (#386 PR-D). Inserted
    /// lazily on first interaction; entries for closed panes are
    /// pruned opportunistically on the next render.
    pub scrollbar_vis: HashMap<u64, scrollbar_visibility::ScrollbarVisState>,
    /// Test-only mirror of the renderer's `drag_chip` overlay (#438).
    /// Production code leaves this `None`. Headless tests use
    /// [`App::__test_set_window_drag_chip_marker`] to flip it `Some(true)`
    /// before calling [`App::cancel_drag_session`], then assert it is
    /// `Some(false)` afterward via [`App::__test_window_drag_chip_marker`].
    /// `cancel_drag_session` flips this in lock-step with the real
    /// `renderer.set_drag_chip(None)` call (when `Some(_)`), so the test
    /// observes the SAME loop iteration the production path runs — if
    /// someone deletes the per-window iteration the marker stays `Some(true)`
    /// and the test fails. This is the test seam Haiku review of PR #443
    /// asked for (the `renderer: None` headless windows could not otherwise
    /// observe `set_drag_chip(None)`).
    pub test_drag_chip_marker: Option<bool>,
    /// Test-only viewport override for this window's pane layout, mirroring
    /// [`App::test_viewport_override`] for the MAIN window. When `Some((outer,
    /// cell_w, cell_h))`, [`App::compute_pane_rects_for`] uses `outer` instead
    /// of the (absent in headless tests) renderer's logical size, and
    /// [`crate::app::child_window::resize_visible_panes_in_child`] uses
    /// `(cell_w, cell_h)` for cell metrics. Lets tests exercise the child
    /// split-pane Grid/PTY resize wiring (tear-out, Resized, close, split)
    /// without a live wgpu surface — the path that the #pane-geom tear-out
    /// regression slipped through because synthetic children have `renderer:
    /// None` and the resize helper silently no-opped. Stays `None` in release.
    #[doc(hidden)]
    pub test_pane_viewport: Option<(sonicterm_ui::pane::Rect, f32, f32)>,
}

impl WindowState {
    /// Borrow the renderer. Panics if the renderer field is `None`
    /// (pre-`do_resumed` for the main entry; never for child entries —
    /// every child construction site initializes it to `Some(_)`).
    #[inline]
    #[track_caller]
    pub fn renderer(&self) -> &GpuRenderer {
        self.renderer
            .as_ref()
            .expect("WindowState::renderer() called before do_resumed populated it")
    }

    /// Mutable counterpart of [`Self::renderer`]. Same panic semantics.
    #[inline]
    #[track_caller]
    pub fn renderer_mut(&mut self) -> &mut GpuRenderer {
        self.renderer
            .as_mut()
            .expect("WindowState::renderer_mut() called before do_resumed populated it")
    }

    /// Phase B2 PR-B2-0 (#365): convenience that short-circuits when
    /// `window` is `None`. Most call sites previously did
    /// `ws.window.request_redraw()` unconditionally; after the
    /// `Option` promotion they want a no-op when the window is gone.
    #[inline]
    pub fn request_redraw(&self) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Record a left-press at grid cell `(row, col)` and return the
    /// resulting click count (1 = single, 2 = double, 3 = triple, then
    /// wraps back to 1). A press counts as a continuation of the previous
    /// streak when it lands on the *same* cell within
    /// [`MULTI_CLICK_MS`] of the previous press. Updates the
    /// `last_click_time` / `last_click_cell` / `click_count` fields in
    /// place. Pure counting logic lives in [`next_click_count`] so it can
    /// be unit-tested without a `WindowState`.
    pub fn register_click(&mut self, row: u16, col: u16) -> u8 {
        let now = Instant::now();
        let within_interval = self
            .last_click_time
            .map(|t| now.duration_since(t).as_millis() <= MULTI_CLICK_MS)
            .unwrap_or(false);
        let same_cell = self.last_click_cell == (row, col);
        let count = next_click_count(self.click_count, same_cell, within_interval);
        self.last_click_time = Some(now);
        self.last_click_cell = (row, col);
        self.click_count = count;
        count
    }

    /// Compute the selection for a multi-click `count` (1 = point, 2 =
    /// word, 3 = line) at grid `(row, col)` using THIS window's active
    /// pane grid. Locks that pane's parser only long enough to read the
    /// grid and build the (Copy) `Selection`, then drops it — so the
    /// caller never holds a grid lock across the selection assignment /
    /// redraw (CLAUDE.md §4). Falls back to a point selection when there
    /// is no active pane or the parser is busy. Used by the child-window
    /// mouse path; the main-window path has equivalent `App`-level
    /// helpers (`word_selection_at` / `line_selection_at`) that resolve
    /// the pane through `App::active_pane`.
    /// Convert a VIEWPORT row (0 = top visible row, from `pixel_to_cell`) to
    /// a scrollback-ABSOLUTE row for THIS window's active pane, so a
    /// `Selection` tracks the same TEXT as the viewport scrolls. Same
    /// `try_lock`-then-drop discipline as [`Self::multi_click_selection`]
    /// (CLAUDE.md §4). Returns `None` when the pane is missing or the parser
    /// is busy; the child-window mouse path then treats the viewport row as
    /// absolute (correct while unscrolled).
    pub fn viewport_row_to_abs(&self, viewport_row: u16) -> Option<u64> {
        let pane = self
            .tab_states
            .get(self.tabs.active_index())
            .map(|st| st.active_pane)
            .and_then(|id| self.panes.get(&id))?;
        let guard = pane.parser.try_lock()?;
        let view_top =
            GpuRenderer::resolved_view_top_abs_legacy(guard.grid(), pane.viewport_top_abs);
        drop(guard);
        Some(view_top + viewport_row as u64)
    }

    pub fn multi_click_selection(&self, count: u8, abs_row: u64, col: u16) -> Selection {
        if count < 2 {
            return Selection::new(abs_row, col);
        }
        let pane = self
            .tab_states
            .get(self.tabs.active_index())
            .map(|st| st.active_pane)
            .and_then(|id| self.panes.get(&id));
        let Some(pane) = pane else {
            return Selection::new(abs_row, col);
        };
        let Some(guard) = pane.parser.try_lock() else {
            return Selection::new(abs_row, col);
        };
        let sel = match count {
            2 => Selection::word_at(guard.grid(), abs_row, col),
            _ => Selection::line_at(guard.grid(), abs_row),
        };
        drop(guard);
        sel
    }

    /// Word-mode drag for THIS window's active pane: union of the word at the
    /// scrollback-ABSOLUTE `anchor` cell and the word at the cursor cell.
    /// `cursor_viewport_row` is converted to an absolute row inside the same
    /// lock. Returns `None` when there is no active pane or the parser is
    /// busy, so the child-window mouse path SKIPS the move rather than
    /// shrinking an anchored word/line selection. Same `try_lock`-then-drop
    /// discipline as [`Self::multi_click_selection`] (CLAUDE.md §4).
    pub fn word_drag_selection(
        &self,
        anchor: (u64, u16),
        cursor_viewport_row: u16,
        col: u16,
    ) -> Option<Selection> {
        let pane = self
            .tab_states
            .get(self.tabs.active_index())
            .map(|st| st.active_pane)
            .and_then(|id| self.panes.get(&id))?;
        let guard = pane.parser.try_lock()?;
        let view_top =
            GpuRenderer::resolved_view_top_abs_legacy(guard.grid(), pane.viewport_top_abs);
        let cursor_abs = view_top + cursor_viewport_row as u64;
        let sel = Selection::word_drag(guard.grid(), anchor, (cursor_abs, col));
        drop(guard);
        Some(sel)
    }

    /// Line-mode drag for THIS window's active pane: whole rows from the
    /// scrollback-ABSOLUTE `anchor_row` to the cursor row inclusive.
    /// `cursor_viewport_row` is converted to an absolute row inside the lock.
    /// Returns `None` when the pane is missing or the parser is busy (see
    /// [`Self::word_drag_selection`]).
    pub fn line_drag_selection(
        &self,
        anchor_row: u64,
        cursor_viewport_row: u16,
    ) -> Option<Selection> {
        let pane = self
            .tab_states
            .get(self.tabs.active_index())
            .map(|st| st.active_pane)
            .and_then(|id| self.panes.get(&id))?;
        let guard = pane.parser.try_lock()?;
        let view_top =
            GpuRenderer::resolved_view_top_abs_legacy(guard.grid(), pane.viewport_top_abs);
        let cursor_abs = view_top + cursor_viewport_row as u64;
        let sel = Selection::line_drag(guard.grid(), anchor_row, cursor_abs);
        drop(guard);
        Some(sel)
    }

    /// #447 follow-up to PR #443: clear the drag-chip overlay in one
    /// place. The renderer's persistent overlay (drawn by the per-frame
    /// emitter at render/core.rs:3945+) and the headless-test marker
    /// (`test_drag_chip_marker`, asserted by os_drag_cleanup.rs) used
    /// to be cleared by two parallel statements in `cancel_drag_session`.
    /// A future refactor that split them would leave the regression
    /// test green while breaking production. Unify both clears here so
    /// every caller flips them in lock-step.
    ///
    /// **Contract (#462 speculative fix codification):** this helper is
    /// **tolerant** — it is safe to call on a `WindowState` whose
    /// `renderer` is `None` (e.g. a transitional window that hasn't
    /// finished initialization yet, or a headless test window) AND on
    /// a window whose `test_drag_chip_marker` is `None`. Both branches
    /// short-circuit cleanly. This matters because the deferred
    /// `pending_os_teardown` drain (see [`App::cancel_drag_session`]
    /// and `App::drain_pending_os_teardown`) iterates a snapshot of
    /// `self.windows.keys()`, and a tear-out spawn that just landed
    /// (#462 race) may have produced a `WindowState` whose renderer
    /// is still being constructed. Both fields are flipped together —
    /// callers MUST NOT split them, or the headless-test lock-step
    /// guarantee in `tests/os_drag_cleanup.rs` regresses.
    #[inline]
    pub(crate) fn clear_drag_chip(&mut self) {
        if let Some(r) = self.renderer.as_mut() {
            r.set_drag_chip(None);
        }
        if let Some(marker) = self.test_drag_chip_marker.as_mut() {
            *marker = false;
        }
    }

    /// #535 + #540 — intra-window tab reorder that keeps `tabs` and
    /// `tab_states` in lock-step. Extracted from `window_event.rs`'s
    /// main-window `ReorderTab` branch so the production path and the
    /// regression tests exercise the SAME code.
    ///
    /// Semantics match `tab_transfer::reorder_within`:
    /// - `from` out of range → no-op.
    /// - `to` clamped to `len - 1` (#540 — drop-past-last must land at
    ///   the end, not silently no-op like `TabBar::reorder` does).
    /// - `to == from` after clamp → no-op.
    /// - Otherwise: `tabs.reorder(from, to)` AND
    ///   `tab_states.remove(from) → insert(to)` so the title's TabState
    ///   (active pane id + PaneTree leaf-ids) travels WITH the title.
    ///
    /// Returns `true` if any mutation happened.
    pub fn reorder_tab(&mut self, from: usize, to: usize) -> bool {
        let len = self.tabs.len();
        if from >= len || len == 0 {
            return false;
        }
        let last = len - 1;
        let to = to.min(last);
        if to == from {
            return false;
        }
        self.tabs.reorder(from, to);
        if from < self.tab_states.len() && to < self.tab_states.len() {
            let st = self.tab_states.remove(from);
            self.tab_states.insert(to, st);
        }
        true
    }
}

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SYNTHETIC_CHILD_WINDOW_TAG: AtomicU64 = AtomicU64::new(1);

fn next_synthetic_child_window_id() -> WindowId {
    let tag = NEXT_SYNTHETIC_CHILD_WINDOW_TAG.fetch_add(1, Ordering::Relaxed);
    // SAFETY: WindowId is `#[repr(transparent)] pub struct WindowId(u64)`
    // in winit; use values below the synthetic main id so test-only child
    // entries never collide with `synthetic_main_window_id()`.
    unsafe { std::mem::transmute::<u64, WindowId>(u64::MAX - tag) }
}

/// Phase B2 PR-B2a (#365): stable synthetic `WindowId` used by the
/// test-only [`App::__test_synthetic_main`] seam so the main entry in
/// `App.windows` can be addressed without a live winit window. winit's
/// `WindowId` is `#[repr(transparent)] struct WindowId(u64)` so a
/// transmute from `u64::MAX` is a stable, collision-free id (real OS
/// window ids never reach `u64::MAX` in practice; the existing
/// per-test `synth_window_id(tag)` helpers also use the transmute
/// pattern — see `tests/os_drag_dispatch_flow.rs`). Production never
/// constructs this id — `do_resumed` always uses the real
/// `window.id()` and explicitly clears any pre-existing synthetic
/// entry first.
#[doc(hidden)]
pub fn synthetic_main_window_id() -> WindowId {
    // SAFETY: WindowId is `#[repr(transparent)] pub struct WindowId(u64)`
    // in winit; this mirrors the test-only transmute pattern already in
    // use under crates/sonicterm-app/tests/. Production code never reaches
    // this function.
    unsafe { std::mem::transmute::<u64, WindowId>(u64::MAX) }
}

/// Phase B2 PR-A — snapshot of the cheap scalar fields mirrored from
/// Epic #289 Phase A — classification of which terminal window currently
/// owns the OS-frontmost focus. Returned by [`App::frontmost_kind`] and
/// consumed by keymap_dispatch arms + menubar drain to decide where a
/// chord like Cmd+T / Cmd+W / Cmd+\\ should land.
///
/// `Other` covers any non-terminal SonicTerm window; it explicitly does NOT
/// route terminal actions and falls back to main as a safe default.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontmostKind {
    /// No window has focus, or recorded id is stale.
    None,
    /// Main terminal window is OS-frontmost.
    Main,
    /// A torn-out child terminal window is OS-frontmost. Carries the
    /// window id so the caller can index `windows`.
    Child(WindowId),
    /// A non-terminal SonicTerm window is frontmost. Terminal actions fall
    /// back to main.
    Other,
}

/// Read a window's screen-global inner origin + inner size into the
/// pure helper struct used by the drag-merge module. Falls back to
/// (0, 0) origin if the platform refuses to report position (e.g. on
/// some Wayland configurations); on such platforms the drag-merge
/// path is best-effort.
pub(super) fn window_geom(w: &Window) -> crate::tab_drag::WindowGeom {
    let origin = w.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
    let size = w.inner_size();
    crate::tab_drag::WindowGeom { inner_origin: origin, inner_size: (size.width, size.height) }
}

#[inline]
pub(super) fn window_dpi(w: &Window) -> f32 {
    w.scale_factor() as f32
}

#[doc(hidden)]
#[doc(hidden)]
pub fn next_pane_id() -> u64 {
    NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Wrap clipboard text for paste, applying DECSET 2004 bracketed-paste
/// guards (`ESC [ 200 ~` / `ESC [ 201 ~`) when the active pane has
/// requested bracketed paste. Pure function, exported for unit tests.
pub fn wrap_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut v = Vec::with_capacity(text.len() + 12);
        v.extend_from_slice(b"\x1b[200~");
        v.extend_from_slice(text.as_bytes());
        v.extend_from_slice(b"\x1b[201~");
        v
    } else {
        text.as_bytes().to_vec()
    }
}

/// Quote a single path or word for POSIX-shell paste. Single-quotes
/// everything and escapes embedded `'` as `'\''`. Mirrors the helper in
/// `sonicterm-windows::os_drag_win::shell_quote` so file drops on either
/// platform paste the same bytes. Pure function, exported for tests.
pub fn shell_quote_posix(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Compute the absolute viewport-top row for "scroll to previous / next
/// prompt". Returns `None` if there is no prompt in the requested
/// direction. Pure function so tests can drive it without a window.
pub fn pick_prompt_target(
    grid: &sonicterm_grid::grid::Grid,
    current_top_abs: u64,
    forward: bool,
) -> Option<u64> {
    let pick = if forward {
        grid.prompt_after(current_top_abs)
    } else {
        grid.prompt_before(current_top_abs)
    };
    pick.map(|p| p.start_row)
}

/// Seed a freshly-created parser with the active theme's query-reply colours:
/// default fg/bg/cursor (OSC 10/11/12 `?`) AND the full 16-colour ANSI palette
/// (OSC 4 `?`). Centralizes what used to be duplicated at every pane-spawn site
/// so the OSC 4 palette wiring (#661) can't be added to one path and forgotten
/// on another. Per-slot colours that don't resolve are simply left unseeded
/// (the parser then suppresses that slot's reply rather than lying).
pub fn seed_parser_theme_colors(parser: &mut sonicterm_vt::vt::Parser, theme: &Theme) {
    if let Some((r, g, b)) = theme.colors.foreground.rgb() {
        parser.set_theme_fg(r, g, b);
    }
    if let Some((r, g, b)) = theme.colors.background.rgb() {
        parser.set_theme_bg(r, g, b);
    }
    if let Some((r, g, b)) = theme.colors.cursor.rgb() {
        parser.set_theme_cursor(r, g, b);
    }
    // OSC 4 palette: indices 0..=7 from `ansi.*`, 8..=15 from `bright.*`,
    // in the standard xterm slot order.
    let normal = [
        &theme.colors.ansi.black,
        &theme.colors.ansi.red,
        &theme.colors.ansi.green,
        &theme.colors.ansi.yellow,
        &theme.colors.ansi.blue,
        &theme.colors.ansi.magenta,
        &theme.colors.ansi.cyan,
        &theme.colors.ansi.white,
    ];
    let bright = [
        &theme.colors.bright.black,
        &theme.colors.bright.red,
        &theme.colors.bright.green,
        &theme.colors.bright.yellow,
        &theme.colors.bright.blue,
        &theme.colors.bright.magenta,
        &theme.colors.bright.cyan,
        &theme.colors.bright.white,
    ];
    for (i, hex) in normal.iter().chain(bright.iter()).enumerate() {
        if let Some((r, g, b)) = hex.rgb() {
            parser.set_theme_palette_color(i as u8, r, g, b);
        }
    }
}

/// Resize every pane in `panes` to `(cols, rows)`: both the parser's
/// grid and (if the pane owns one) the PTY child. Used by the window
/// resize handler and by the font live-reload path, where changing
/// cell metrics shifts how many cells fit inside the current window.
///
/// `pub` + `#[doc(hidden)]` so integration tests can exercise the
/// invariant on a synthetic pane map without needing a live wgpu
/// surface or a real shell.
#[doc(hidden)]
pub fn resize_all_panes(panes: &HashMap<u64, PaneState>, cols: u16, rows: u16) {
    for pane in panes.values() {
        pane.parser.lock().grid_mut().resize(cols, rows);
        if let Some(pty) = pane.pty.as_ref() {
            (pty.resize)(cols, rows);
        }
    }
}

/// Resize each pane in `panes` to the cells that fit inside its own
/// `sonicterm_ui::pane::Rect` (window-pixel logical rect produced by
/// `PaneTree::layout`). `cell_w` / `cell_h` are the logical cell metrics
/// from the renderer (`Renderer::cell_size()`).
///
/// This is the per-pane sizing counterpart to [`resize_all_panes`]: the
/// older helper sized every pane to the same whole-window `(cols, rows)`,
/// which is wrong as soon as a tab has more than one pane (an inactive
/// pane's grid then thinks it has more columns than it actually shows,
/// so TUIs like vim/htop draw past their visible border and the wrap
/// column is wrong on resize).
///
/// CLAUDE.md §4: uses `parser.lock()` (NOT `try_lock`) — same as
/// `resize_all_panes`. Call sites are app-thread (WindowEvent::Resized
/// and config-live-reload), not the render hot path, so the lock is
/// safe and a dropped resize would leave the grid wrong-sized for the
/// next burst of pty output.
///
/// `rects` whose `id` is missing from `panes` are silently skipped
/// (covers the brief window during tab close where the layout list
/// includes a pane that was just removed).
///
pub fn resize_panes_to_rects(
    panes: &HashMap<u64, PaneState>,
    rects: &[(u64, sonicterm_ui::pane::Rect)],
    cell_w: f32,
    cell_h: f32,
    content_inset: [f32; 4],
) {
    let [left, right, top, bottom] = content_inset;
    for (id, rect) in rects {
        let Some(pane) = panes.get(id) else { continue };
        let content_w = (rect.w - left - right).max(cell_w);
        let content_h = (rect.h - top - bottom).max(cell_h);
        let cols = ((content_w / cell_w).floor() as i32).max(1) as u16;
        let rows = ((content_h / cell_h).floor() as i32).max(1) as u16;
        pane.parser.lock().grid_mut().resize(cols, rows);
        if let Some(pty) = pane.pty.as_ref() {
            (pty.resize)(cols, rows);
        }
    }
}

/// Mark every pane's grid fully dirty. Used by triggers that change
/// the renderer's *presentation* invariant without mutating any cell
/// content (theme swap, font swap, focus transition, selection change).
/// This is the foundation hook the upcoming RowCache will use to know
/// when its cached row data is stale even though grid revision did not
/// bump.
///
/// `pub` + `#[doc(hidden)]` so integration tests can exercise the
/// invariant on a synthetic pane map.
#[doc(hidden)]
pub fn mark_all_panes_dirty(panes: &HashMap<u64, PaneState>) {
    for pane in panes.values() {
        pane.parser.lock().grid_mut().mark_all_dirty();
    }
}

/// Compute the wezterm-style pretty tab title for the active pane and
/// (if it differs from the current `TabBar` active title) apply it via
/// `set_active_title`. Returns the title actually applied, or `None` if
/// no change was needed.
///
/// Refactored out of `app/window_event.rs` so the equivalent code path
/// in `app/child_window.rs` (Cmd+N / tear-out windows) can share the
/// same logic — otherwise child windows fall back to the literal
/// "shell N" placeholder set at spawn time.
pub fn refresh_active_tab_title(
    tabs: &mut sonicterm_ui::tabs::TabBar,
    pane: &mut PaneState,
    parser: &Parser,
    tab_idx: usize,
) -> Option<String> {
    let cwd = parser.cwd().map(str::to_string);
    let raw_title = parser.title().map(str::to_string);
    const TTL: std::time::Duration = std::time::Duration::from_millis(500);
    let now = Instant::now();
    let fresh = pane.fg_proc_cache.as_ref().is_some_and(|(t, _)| now.duration_since(*t) < TTL);
    if !fresh {
        let probed = pane
            .pty
            .as_ref()
            .and_then(|p| p.pid())
            .and_then(sonicterm_io::proc_info::foreground_process);
        pane.fg_proc_cache = Some((now, probed));
    }
    let proc_name = pane.fg_proc_cache.as_ref().and_then(|(_, v)| v.clone());
    let pretty = sonicterm_ui::tab_title::format_tab_title(
        tab_idx,
        cwd.as_deref(),
        proc_name.as_deref(),
        raw_title.as_deref(),
    );
    let pretty = tabs
        .active()
        .and_then(|tab| tab.custom_title.as_ref())
        .map(|custom| sonicterm_ui::tabs::title_with_replaced_body(&pretty, custom))
        .unwrap_or(pretty);
    let cur = tabs.active().map(|t| t.title.clone());
    if cur.as_deref() == Some(pretty.as_str()) {
        return None;
    }
    tabs.set_active_title(pretty.clone());
    Some(pretty)
}

/// Loader callback type used by the platform shell to reload a theme by name.
pub type ThemeLoader = Box<dyn Fn(&str) -> Result<Theme> + Send + 'static>;
/// Loader callback type used by the platform shell to reload a keymap by name.
pub type KeymapLoader = Box<dyn Fn(&str) -> Result<Keymap> + Send + 'static>;

/// Custom user events delivered through [`EventLoopProxy`].
///
/// Currently the only variant is [`UserEvent::ConfigChanged`], sent by
/// the [`ConfigWatcher`] thread whenever a fresh `sonicterm.toml` parse is
/// available. The handler wakes the loop, drains the watcher channel,
/// and applies the new config (theme/font/keymap). Without this the
/// channel-based delivery would sit queued under `ControlFlow::Wait`
/// until an unrelated event arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEvent {
    /// A new `sonicterm.toml` parse is ready on the watcher channel.
    ConfigChanged,
    /// A pending action arrived from the macOS native menubar. The
    /// payload itself is queued in the static
    /// [`crate::menubar_bridge`] buffer; this variant is just the
    /// wake-up signal so the loop drains it.
    MenuAction,
    /// A platform OS-drag drop landed and stashed payloads in
    /// [`crate::os_drag_bridge`]. The variant is just the wake-up
    /// signal so the loop drains the queues — separate from
    /// [`Self::MenuAction`] so a noisy drag stream does not flood the
    /// menubar drain path.
    OsDrag,
    /// Phase C2: an OS-level drag *session* (NSDraggingSession on
    /// macOS, OLE DoDragDrop on Windows) reported a cursor move. The
    /// actual position is in the [`os_drag::PendingDragOutcome`]
    /// mailbox shared with the backend.
    DragMoved,
    /// Phase C2: an OS-level drag *session* terminated (drop or
    /// cancel). The outcome (drop target, tear-out, or cancel) is in
    /// the [`os_drag::PendingDragOutcome`] mailbox; the dispatcher
    /// inspects it and routes to `App::transfer_tab` or
    /// `App::cancel_drag_session` accordingly.
    DragEnded,
    /// Epic #300 P4 follow-up: a previously-deferred font fallback
    /// family finished loading in the
    /// [`sonicterm_text::async_fallback::AsyncFallbackLoader`] background
    /// thread. The handler walks every live window's `GpuRenderer`,
    /// calls `clear_shape_cache()` (which bumps `style_rev` and drops
    /// the shape / row / line caches), and issues
    /// `window.request_redraw()` so the next frame re-shapes through
    /// the newly available face and the user's tofu cells get
    /// replaced by real glyphs.
    ClearShapeCache,
}

/// Build an [`AsyncFallbackLoader`] whose notifier fires
/// `UserEvent::ClearShapeCache` on `proxy`. The loader uses
/// [`sonicterm_text::async_fallback::default_load_font_family`] for actual
/// font resolution (zero-byte handle for OS-resident faces, which is
/// what we want — cosmic-text's `FontSystem` does the real install on
/// first use).
///
/// This is the production wire that `Haiku` flagged as missing on
/// PR #318: pre-fix, the loader was wired only inside tests, and
/// real frame-time misses never spawned `request_load` calls. With
/// this helper, every `GpuRenderer::new` site in `sonicterm-app`
/// constructs the loader from its event-loop proxy and hands it to
/// `GpuRenderer::set_async_loader`. From that point on, a
/// background font load completion bumps `style_rev` on every live
/// window and triggers a redraw — the tofu cells flip to real
/// glyphs without the user having to type anything.
/// T13/T14: the legacy `AsyncFallbackLoader` (cosmic-text/swash
/// driven background-load helper) is gone with the rest of the
/// glyphon plumbing. sonicterm-font handles CJK/emoji/Nerd-font
/// fallback synchronously via its built-in vendor chain
/// (`vendor-jetbrains`, `vendor-noto-emoji`, `vendor-nerd-font-symbols`),
/// so the per-window `set_async_loader(...)` plumbing is now a no-op
/// `()`. Keeping the function shape and call site survives so the
/// renderer's `Option<()>` slot stays populated and any future
/// re-introduction of an async hook lands without breaking callers.
#[must_use]
pub fn build_async_fallback_loader_for_proxy(_proxy: EventLoopProxy<UserEvent>) -> () {
    ()
}

mod child_window;
pub use child_window::{
    apply_dpi_to_renderer_if_present, child_window_dpi_changed_handles_no_renderer,
    child_window_resized_handles_no_renderer, resize_renderer_and_panes_if_present,
};
mod config_apply;
mod event_loop;
pub mod hovered_url;
pub mod invariants;
mod key_encoding;
mod keymap_dispatch;
mod media;
mod misc;
pub mod os_drag;
mod overlays;
mod scroll;
pub mod scrollbar_input;
pub mod scrollbar_visibility;
mod search_handle;
mod spawn_pane;
mod tab_state;
pub mod tab_transfer;
mod tear_out;
mod window_event;
pub use config_apply::{config_diff_needs_font_apply, renderer_scrollbar_mode_differs};
pub use key_encoding::{encode_logical, key_name, key_to_string, key_to_strings, KeyName};

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("sonic=info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

/// Public re-export of [`init_tracing`] for the M6b platform shell
/// (`crate::shell::MacShell::run`). Same idempotent `try_init`
/// behaviour — no-op if a subscriber is already installed.
pub fn init_tracing_public() {
    init_tracing();
}

/// Per-pane runtime state. The parser is shared with a per-pane VT thread
/// that drains the pty out-channel; the pty handle owns the writer side.
///
/// `redraw_target` is the window the pane's VT thread should request a
/// redraw on. Wrapped in `Arc<Mutex<Option<Arc<Window>>>>` so the main
/// thread can atomically swap it when the pane migrates to a torn-out
/// child window — the VT thread reads the current target on each batch
/// and notifies whichever window currently owns the pane.
pub struct PaneState {
    pub parser: Arc<Mutex<Parser>>,
    pub pty: Option<PtyHandle>,
    pub redraw_target: Arc<Mutex<Option<Arc<Window>>>>,
    /// Absolute row (scrollback-relative) that should appear at the top of
    /// the visible viewport. `None` = "follow the live tail" (default).
    /// Currently set by the OSC 133 prompt-navigation actions. The render
    /// layer treats this as a hint — the grid itself always exposes the
    /// live visible window.
    pub viewport_top_abs: Option<u64>,
    /// Cached foreground-process name + the wall-clock instant we last
    /// probed. The probe walks the whole macOS process table (~600 procs)
    /// so we MUST NOT re-run it on every render — when the cursor blinks,
    /// the render path fires ~26Ã—/sec and an uncached probe burned ~17%
    /// CPU on an idle window. TTL is short enough that `nvim foo` still
    /// flips the tab title quickly.
    pub fg_proc_cache: Option<(std::time::Instant, Option<String>)>,
    /// Cross-thread queue populated by the VT loop when OSC 133 command
    /// lifecycle markers are parsed for this pane.
    pub command_events: Arc<Mutex<Vec<PaneCommandEvent>>>,
    /// Per-pane DECTCEM cursor-visibility flag (`CSI ?25h/l`). Written
    /// by the VT loop, read by the render path for the active pane.
    /// **Per-pane (not per-window)** so the Arc travels with the pane
    /// when a tab is torn out into a new window — pre-fix #400 the Arc
    /// lived on `WindowState`, so tear-out's destination got a fresh
    /// Arc and the moved pane's VT thread kept writing to an orphaned
    /// AtomicBool that nobody read. Init `true`.
    pub cursor_visible: Arc<std::sync::atomic::AtomicBool>,
    /// Decoded inline media images captured from terminal protocols.
    pub inline_images: Arc<Mutex<Vec<sonicterm_render_model::InlineImage>>>,
}

#[derive(Debug, Clone)]
pub struct PaneCommandEvent {
    pub event: CommandEvent,
    pub at: Instant,
    pub duration: Option<Duration>,
}

impl PaneState {
    #[doc(hidden)]
    pub fn new(parser: Arc<Mutex<Parser>>, pty: Option<PtyHandle>) -> Self {
        Self {
            parser,
            pty,
            redraw_target: Arc::new(Mutex::new(None)),
            viewport_top_abs: None,
            fg_proc_cache: None,
            command_events: Arc::new(Mutex::new(Vec::new())),
            cursor_visible: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            inline_images: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// Per-tab state. The `TabBar` keeps title/order; this struct tracks the
/// pane tree and the focused leaf inside the tab.
pub struct TabState {
    pub tree: PaneTree,
    pub active_pane: u64,
    pub search: Option<SearchState>,
    pub command: CommandStatus,
}

impl TabState {
    #[doc(hidden)]
    pub fn new(tree: PaneTree, active_pane: u64) -> Self {
        Self { tree, active_pane, search: None, command: CommandStatus::Idle }
    }
}

/// Issue #553 Phase A: typed in-process tear-out request. Created by
/// `handle_os_drag_ended` on the `DroppedOnEmpty` branch (Win32
/// `GetCursorPos` provides the screen-position field); drained by
/// `drain_pending_window_creates` which calls the in-process child-window
/// builder factored out of `tear_out.rs` — so no `Command::new` is ever
/// invoked for the tear-out path on Phase A.
#[derive(Debug, Clone)]
pub struct PendingTearOut {
    pub source_window: WindowId,
    pub source_tab_idx: usize,
    pub drop_screen_pos: (i32, i32),
}

#[doc(hidden)]
pub struct App {
    pub(super) theme: Theme,
    pub(super) config: Config,
    pub(super) keymap: Keymap,
    // PR-B1b (#293): `App.renderer` field removed; the main window's
    // `GpuRenderer` is now owned by `self.windows[main_window_id].renderer`.
    // Access via `Self::main_renderer()` / `Self::main_renderer_mut()`.
    // PR-B2b (#365): `App.tabs` + `App.tab_states` fields removed; the
    // main window's TabBar + TabState vec are now owned by
    // `self.windows[main_window_id]`. Access via `Self::main_tabs()` /
    // `Self::main_tabs_mut()` / `Self::main_tab_states()` /
    // `Self::main_tab_states_mut()`. Callers needing both at once should
    // go through `self.main_mut()` directly to avoid double-borrow.
    // PR-B2c (#365): `App.panes` field removed; the main window's pane
    // map is now owned by `self.windows[main_window_id]`. Access via
    // `Self::main_panes()` / `Self::main_panes_mut()`. Callers needing
    // panes + tabs/tab_states/renderer together should go through
    // `self.main_mut()` and split-borrow the fields disjointly.
    // PR-B3b (#365): `App.last_render`, `App.cursor_visible`, and
    // `App.hover_link` fields removed; now owned by
    // `self.windows[main_window_id]`. Access via
    // `self.main()?.last_render` / `self.main()?.cursor_visible` /
    // `self.main()?.hover_link`.
    // PR-B3c (#365): `App.selection`, `App.copy_mode`, and `App.modifiers`
    // fields removed; now owned by `self.windows[main_window_id]`.
    // Access via [`Self::main_selection`] / [`Self::main_modifiers`] /
    // direct field access through `self.main()?.copy_mode` etc.
    pub(super) clipboard: Option<Clipboard>,
    // #404: `App`-level DPI and hovered_url fields deleted — both
    // now live exclusively on `WindowState`. Readers go through
    // `self.main()?.dpi_scale` / `self.main()?.hovered_url`
    // (with safe-default fallbacks at call sites). The shadow-sync
    // path was deleted as the final Phase B2 leftover.
    /// Epic #289 Phase E (Haiku follow-up): Action::NewWindow sets this
    /// flag, then `drain_pending_window_creates` consumes it by calling
    /// `create_new_terminal_window(el)`. Window creation requires an
    /// `ActiveEventLoop` reference
    /// that isn't reachable from the keymap dispatcher. Works from BOTH
    /// the windows-non-empty case (Cmd+N from a focused window) AND the
    /// windows-empty post-close-last-window dock-alive case on macOS.
    pub(super) pending_new_window: bool,
    /// Issue #553 Phase A: typed in-process tear-out request. Set by
    /// `handle_os_drag_ended` on the `DroppedOnEmpty` branch with the
    /// recorded source tab handle + Win32 cursor screen position.
    /// Drained by `drain_pending_window_creates` AFTER `pending_new_window`
    /// in the SAME pass (NewShell then TearOut). Replaces the legacy
    /// child-process spawn (`spawn_tearout_child`) — that code path
    /// becomes dead in Phase A and is removed in Phase B.
    pub(super) pending_tear_out: Option<PendingTearOut>,
    /// Issue #462 (speculative defensive fix): deferred
    /// `cancel_drag_session` request. Set by `handle_os_drag_ended`
    /// on the `DroppedOnEmpty` branch instead of cancelling inline,
    /// so any tear-out-spawn produced by the existing
    /// `pending_new_window` drain runs to completion BEFORE
    /// cross-window drag-residue cleanup mutates `self.windows`.
    /// Drained by `App::drain_pending_os_teardown` AFTER
    /// `App::drain_pending_window_creates` at the natural event-loop
    /// boundary in `event_loop.rs::do_user_event`. The
    /// `cancel_drag_session` all-windows loop runs **unconditionally**
    /// when drained — this flag controls only WHEN it runs, not
    /// WHETHER (preserves the `os_drag_cleanup.rs:172-201`
    /// idempotence guarantee).
    pub(super) pending_os_teardown: bool,
    /// PR #533 Haiku Step-4 2nd-pass REVISE: test-only callback fired
    /// inside [`Self::cancel_drag_session`] AFTER the `self.windows.keys()`
    /// snapshot is collected but BEFORE the per-id iteration body runs.
    /// Lets the regression test (`os_drag_cleanup.rs::
    /// cancel_drag_session_tolerates_window_removed_before_iteration`)
    /// mutate `self.windows` in the exact race window that the
    /// `get_mut(&id).else { continue }` arm is designed to tolerate.
    /// Consumed (`take()`-d) at the call site so the closure is invoked
    /// at most once per `cancel_drag_session` run and the mutable
    /// borrow on `self.windows` is not held while it runs. Production
    /// cost is one extra `Option::take()` per `cancel_drag_session`
    /// invocation (always `None` outside tests) — gated by
    /// `#[doc(hidden)]` rather than `#[cfg(test)]` because the test
    /// living in `tests/os_drag_cleanup.rs` is an INTEGRATION test
    /// that compiles the crate without `cfg(test)`.
    #[doc(hidden)]
    pub(super) test_post_snapshot_hook: Option<Box<dyn FnOnce(&mut App) + Send>>,
    /// Deferred app-exit request. Set from `run_action` when the user's
    /// Cmd+W chain has just closed the last tab of the last window AND
    /// `Config::quit_on_last_window_close` is true (or non-macOS).
    /// `do_about_to_wait` drains it by calling `el.exit()`. The flag is
    /// needed because `run_action` does not have an `ActiveEventLoop`
    /// handle.
    pub(super) pending_exit: bool,
    // PR-B3d (#365): `App.ime` and `App.ime_cursor_throttle` fields
    // removed; now owned by `self.windows[main_window_id]`. Access via
    // `self.main()?.ime` / `self.main_mut()?.ime_cursor_throttle`.
    pub(super) command_palette: CommandPalette,
    /// Epic #289 Phase A follow-up — which window the (single, modal)
    /// command palette is currently attached to. `None` means it's
    /// closed OR attached to the main window; `Some(id)` means it's
    /// attached to that child window. The render paths for the main
    /// window and each child window consult this so the palette only
    /// paints on the frontmost window at the moment it was opened,
    /// fixing the bug where Cmd+Shift+P typed in a torn-out child
    /// silently opened the palette on the original main window.
    pub(super) palette_attached_window: Option<WindowId>,
    // PR-B3d (#365): `App.drag_session` field removed; per-window
    // drag sessions live on `WindowState`. Access via
    // `self.main_mut()?.drag_session` / per-window iteration.
    /// Phase C2 (PR #295 review fix): set the moment a held-tab drag
    /// crosses [`os_drag::OS_DRAG_THRESHOLD_PX`] from its press point,
    /// before the user releases the button. Guards
    /// [`Self::try_os_drag_handoff`] in the `CursorMoved` path so the
    /// OS-level drag session starts mid-gesture (cursor still down)
    /// rather than waiting until mouse-up — which was too late for
    /// `DoDragDrop` to capture the cursor across windows. Cleared on
    /// `cancel_drag_session` and at every fresh mouse-down so a new
    /// gesture re-arms cleanly.
    pub(super) os_drag_handoff_started: bool,
    /// Windows spawned by tearing tabs out of the parent bar. Keyed by
    /// winit WindowId so events route back to the right child.
    pub(super) windows: HashMap<WindowId, WindowState>,
    /// Phase B2 PR-A: id of the main window. Set in `do_resumed` once
    /// the main `Window` is created and `WindowState` shadow entry is
    /// inserted into [`Self::windows`]. Readers MUST still use the
    /// legacy `App.window`/`renderer`/`tabs`/... fields — PR-B will
    /// switch them to read off `self.windows[main_window_id]`.
    pub(super) main_window_id: Option<WindowId>,
    // PR-B4 (#365): `App.focused_child` removed; its job
    // ("which torn-out child currently owns focus, or None for main")
    // is now strictly a subset of `frontmost_window` (which discriminates
    // main vs child via `frontmost_kind()`). All readers route through
    // `frontmost_window` / `frontmost_kind()`.
    /// Epic #289 Phase A — most-recently-OS-frontmost window id, INCLUDING
    /// the main window. The frontmost field tracks *every* sonic-owned
    /// terminal window with a single non-`Option` discriminant once the
    /// first focus arrives:
    /// single non-`Option` discriminant once the first focus arrives:
    ///
    ///   * `Some(main_window_id)`  → main window is OS-frontmost
    ///   * `Some(child_window_id)` → that child window is OS-frontmost
    ///   * `None`                  → no sonic window has been focused yet,
    ///     OR focus has moved out of every sonic window to another app.
    ///
    /// Keyboard / menubar / accelerator actions (Cmd+T, Cmd+W, Cmd+\\, …)
    /// route through this id so a chord typed in window B never mutates
    /// window A's tab vec. Set in both the main and child `Focused(true)`
    /// arms; on `Focused(false)` we only clear when the dropped window was
    /// the current frontmost (focus moving to a *different* sonic window
    /// arrives as that other window's `Focused(true)` and overwrites
    /// frontmost in the right order). Bug reports addressed by this field:
    ///   * #2: Cmd+T after tear-out opens tab in WRONG window
    ///   * #3: Cmd+W in new window closes OLD window's tab
    pub(super) frontmost_window: Option<WindowId>,
    // PR-B3d (#365): `App.drag_target` field removed; per-window
    // pending drop target lives on `WindowState`. Access via
    // `self.main_mut()?.drag_target`.
    /// OS-drag tab payloads received before the main [`WindowState`] exists.
    /// Startup pasteboard / OLE deliveries can arrive before `do_resumed`
    /// inserts `main_window_id`; queue them so the destination tab is created
    /// after main is available instead of silently dropping the payload.
    pub(super) pending_os_drag_payloads: Vec<crate::os_drag::TabPayload>,
    // PR-B4 (#365): `App.main_hidden` removed; the "main window is
    // drained / hidden" latch lives on `WindowState.hidden`. Access via
    // `self.main_is_hidden()` (true when the field is set OR the main
    // entry is gone — both shapes mean "no visible main").
    /// Optional theme loader, set by `run_with`. Used to reload a theme
    /// by name live.
    pub(crate) theme_loader: Option<ThemeLoader>,
    /// Optional keymap loader, set by `run_with`.
    pub(crate) keymap_loader: Option<KeymapLoader>,
    /// Live-reload watcher for the user's `sonicterm.toml`. Spawned in
    /// `resumed`; `None` if the config path could not be resolved or
    /// the watcher failed to start (e.g. parent dir unwritable).
    pub(super) config_watcher: Option<ConfigWatcher>,
    /// Proxy used by the watcher thread to wake the idle event loop
    /// on `sonicterm.toml` changes. `None` in tests that construct `App`
    /// directly via [`App::new`] without a real event loop.
    pub(super) event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
    /// Minimum interval between two successive frames. Defaults to 1/60s
    /// and is updated in `resumed` from the current monitor's reported
    /// refresh rate. Used by the RedrawRequested handler to skip an
    /// over-render and by `about_to_wait` to schedule the next vsync
    /// boundary via `ControlFlow::WaitUntil`. See perf audit #9.
    pub(super) frame_period: Duration,
    /// Set when a RedrawRequested arrives sooner than `frame_period`
    /// after the previous render. `about_to_wait` schedules a
    /// `WaitUntil(last_render + frame_period)` and `new_events`'
    /// `ResumeTimeReached` arm calls `request_redraw()` so we coalesce
    /// the pending request onto the next vsync tick rather than
    /// burning a frame.
    pub(super) pending_redraw: bool,
    /// Per-CHILD-window analogue of [`Self::pending_redraw`]. The main
    /// window's deferred-redraw latch is a single bool keyed off
    /// `main().last_render`; torn-out child windows each carry their own
    /// `WindowState.last_render` and `request_redraw()`, so a child that
    /// defers a PTY-streaming or lock-contended redraw records its
    /// `WindowId` here. `about_to_wait` folds each pending child's
    /// `last_render + frame_period` into the next `WaitUntil` deadline,
    /// and `new_events`' `ResumeTimeReached` arm re-requests a redraw on
    /// exactly those windows. An entry is cleared when that child next
    /// renders past the coalescing gate (or when the window is reaped).
    pub(super) pending_redraw_windows: HashSet<WindowId>,
    /// Set true whenever a user-driven event (keyboard, mouse click,
    /// cursor move while dragging, resize, IME, modifier change) or a
    /// live-reload of theme/font/keymap occurs. The next
    /// `WindowEvent::RedrawRequested` will bypass the vsync coalescing
    /// gate so the first frame after input is immediate (zero added
    /// latency). Subsequent redraws driven purely by streaming PTY
    /// bytes within the same `frame_period` still coalesce onto the
    /// next vsync boundary via `pending_redraw`. Cleared on every
    /// frame we actually render. See PR #132 Haiku review.
    pub(super) input_dirty: bool,
    /// Shared with every VT-thread spawned in `spawn_pty_for_pane` (one
    /// per pane). Incremented by the VT loop whenever a non-empty chunk
    /// of PTY bytes is processed; sampled on each `RedrawRequested` to
    /// decide whether to bypass the vsync coalescing gate. PR #133/#162.
    pub(super) pty_burst_gen: Arc<AtomicU32>,
    /// Last PTY-burst generation that a completed render observed. If
    /// the VT thread increments [`Self::pty_burst_gen`] during render,
    /// this remains behind the current generation so the next redraw
    /// bypasses the vsync gate instead of losing the burst.
    pub(super) last_seen_burst_gen: u32,
    /// Translation bundle. Rebuilt when the user picks a new locale in
    /// the preferences "Language" dropdown.
    pub(super) i18n: sonicterm_ui::i18n::I18n,
    /// Optional platform hook that takes a serialized tab payload and
    /// hands it off to the OS-level drag-and-drop system
    /// (`NSPasteboard` on macOS, OLE `DoDragDrop` on Windows). When
    /// set, [`Self::tear_out_tab`] checks whether the cursor sits
    /// outside every SonicTerm-owned window; if so, it invokes the sink
    /// and KILLS the local tab instead of spawning a child window.
    /// Installed by the platform shell via
    /// [`crate::shell::MacShell::with_os_drag_sink`] /
    /// [`crate::shell::WindowsShell::with_os_drag_sink`].
    pub(crate) os_drag_sink: Option<Arc<dyn crate::os_drag::OsDragSink>>,
    /// Phase C2 OS-level drag *session* backend. Distinct from
    /// `os_drag_sink` (cross-process wire format): this drives the
    /// NSDraggingSession / OLE DoDragDrop call that captures the
    /// cursor across window boundaries for same-process tab drags.
    /// Installed by the platform bin (`sonicterm-mac` / `sonicterm-windows`)
    /// at startup. `None` in tests + on platforms without an impl.
    pub(super) os_drag_backend: Option<Box<dyn os_drag::OsTabDragBackend>>,
    /// Shared mailbox the [`os_drag::OsTabDragBackend`] writes pending
    /// drag outcomes into. Drained by `do_user_event` on every
    /// `UserEvent::DragMoved` / `DragEnded` wake.
    pub(super) os_drag_pending: Arc<os_drag::PendingDragOutcome>,
    /// Shared tab-bar snapshot registry. The App publishes the live
    /// per-window tab bar layout into this on every redraw (see
    /// `publish_os_drag_bar_snapshot`); a Phase-C2 OS-drag backend
    /// reads from it inside its drop callback (Windows
    /// IDropTarget::Drop / macOS NSDraggingDestination::performDrop)
    /// to resolve the raw screen-coordinate drop into a real
    /// `(WindowId, slot)` pair before posting a `DroppedOnBar` outcome.
    pub(super) os_drag_bars: Arc<os_drag::TabBarRegistry>,
    /// Phase C2: tracks the source-side bookkeeping while an OS drag
    /// is in flight. `Some((source_window, source_tab_idx))` from
    /// `begin_session` until `UserEvent::DragEnded` is drained; back
    /// to `None` once the dispatcher routes the outcome.
    pub(super) os_drag_source: Option<(WindowId, usize)>,
    /// View → Toggle Tab Bar state. When `false`, the menubar Toggle
    /// Tab Bar action has hidden the tab bar chrome. Defaults to
    /// `true`. Exposed via [`Self::tab_bar_visible`] so the renderer
    /// + hit-test code can read it on each frame.
    pub(super) tab_bar_visible: bool,
    /// Broadcast-input mode. When enabled, bytes typed into `source_pane`
    /// are mirrored into matching receiver panes after the source PTY write.
    pub(super) broadcast: BroadcastState,
    /// One-shot hook fired the first time the winit `ApplicationHandler::
    /// resumed` callback runs — i.e. when NSApp / the platform event
    /// loop is fully initialized but BEFORE we hand control back to
    /// winit's `run_app`. macOS uses this slot to install the native
    /// NSMenu; calling `setMainMenu` earlier (before winit builds the
    /// AppKit loop) leaves AppKit with only the default
    /// `Apple, sonicterm-mac` menubar.
    pub(crate) on_resumed: Option<Box<dyn FnOnce() + Send>>,

    /// One-shot hook fired the moment the main window has been created
    /// (immediately after `el.create_window` succeeds, before the first
    /// redraw is requested). Receives the `raw-window-handle` of the
    /// window. Windows uses this slot to install the muda menubar,
    /// which requires the HWND at install time. Unused on macOS.
    pub(super) on_window_ready: Option<Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>>,
    /// Test-only redraw request counter (PR #271 follow-up). Every
    /// production code path that calls `window.request_redraw()` after
    /// a `run_action` dispatch also bumps this counter in lock-step.
    /// Tests assert against this rather than the live winit window
    /// (which has no public introspection API). Stays at zero in
    /// release builds whose tests don't touch it.
    #[doc(hidden)]
    pub redraw_request_count: std::sync::atomic::AtomicUsize,
    /// Test-only counter incremented on every call to
    /// [`Self::reap_empty_child`] (PR #302 Haiku follow-up). Lets tests
    /// distinguish "child window cleanup went through the unified reap
    /// contract" from "a direct `windows.remove` happened" — both would
    /// shrink the `windows` map, but only the former nulls out straggler
    /// `redraw_target`s and fires the reap trace. Stays at zero in
    /// release builds whose tests don't touch it.
    #[doc(hidden)]
    pub reap_call_count: std::sync::atomic::AtomicUsize,
    /// Test-only viewport override (PR #393 follow-up for #387). When
    /// `Some((outer, cell_w, cell_h))`, [`Self::compute_active_pane_rects`]
    /// uses `outer` instead of fetching the renderer's logical size and
    /// [`Self::resize_visible_panes`] uses `(cell_w, cell_h)` instead of
    /// the renderer's `cell_size()`. Lets tests exercise the production
    /// `close_active_pane` path (Grid + PtyHandle resize wiring) without
    /// a live wgpu surface. Stays `None` in release builds whose tests
    /// don't touch it.
    #[doc(hidden)]
    pub test_viewport_override: Option<(sonicterm_ui::pane::Rect, f32, f32)>,
    /// M6a-expand-2b — winit-agnostic state machine. Routed Intents
    /// (PTY write, scroll, hyperlink open, …) flow through here and
    /// the platform shell's [`Self::dispatch_effects`] translates the
    /// resulting [`AppEffect`] batch into concrete calls against the
    /// existing renderer / clipboard / PTY plumbing. Non-leaf paths
    /// (tab/pane/window lifecycle) continue to take the legacy direct
    /// route until M6a-expand-2c lifts those into the reducer.
    pub(crate) machine: sonicterm_app_core::AppStateMachine,
}

impl sonicterm_ui::broadcast::BroadcastTab for TabState {
    fn pane_tree(&self) -> &PaneTree {
        &self.tree
    }
}

impl App {
    /// Compute window-pixel rects for every pane in the active tab,
    /// using the main window renderer's logical size + insets + padding.
    /// Returns an empty Vec if there is no renderer yet (pre-Resumed) or
    /// no active tab. Mirrors the inline computation in
    /// `window_event.rs` (~line 110); factored so resize/config-reload
    /// call sites stay one-liners.
    pub(crate) fn compute_active_pane_rects(&self) -> Vec<(u64, sonicterm_ui::pane::Rect)> {
        let Some(ws) = self.main() else { return Vec::new() };
        let tab_idx = ws.tabs.active_index();
        let Some(st) = ws.tab_states.get(tab_idx) else { return Vec::new() };
        // Test-only viewport override (PR #393 follow-up for #387) — lets
        // tests exercise this path without a live wgpu renderer. Production
        // leaves `test_viewport_override` at `None` and falls through to the
        // renderer-derived metrics below.
        if let Some((outer, _, _)) = self.test_viewport_override {
            return st.tree.layout(outer);
        }
        let Some(r) = self.main_renderer() else { return Vec::new() };
        let (w, h) = r.logical_size();
        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
        let bottom = r.bottom_inset();
        let outer =
            sonicterm_ui::pane::Rect::new(0.0, top, w.max(0.0), (h - top - bottom).max(0.0));
        st.tree.layout(outer)
    }

    /// Same as [`Self::compute_active_pane_rects`] but for a torn-out
    /// child window (its own renderer + tab_states).
    pub(crate) fn compute_pane_rects_for(
        child: &WindowState,
    ) -> Vec<(u64, sonicterm_ui::pane::Rect)> {
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get(tab_idx) else { return Vec::new() };
        // Test-only viewport override (mirrors main `test_viewport_override`):
        // headless child windows have `renderer: None`, so without this the
        // child resize path can't be unit-tested. #pane-geom
        if let Some((outer, _, _)) = child.test_pane_viewport {
            return st.tree.layout(outer);
        }
        let Some(r) = child.renderer.as_ref() else { return Vec::new() };
        let (w, h) = r.logical_size();
        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
        let bottom = r.bottom_inset();
        let outer =
            sonicterm_ui::pane::Rect::new(0.0, top, w.max(0.0), (h - top - bottom).max(0.0));
        st.tree.layout(outer)
    }

    #[doc(hidden)]
    pub fn new(theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self::new_with_proxy(theme, config, keymap, None)
    }

    #[doc(hidden)]
    pub fn new_with_proxy(
        theme: Theme,
        config: Config,
        keymap: Keymap,
        event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
    ) -> Self {
        Self::new_with_proxy_and_machine(
            theme,
            config,
            keymap,
            event_loop_proxy,
            sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default()),
        )
    }

    /// M6b: constructor that accepts an externally-built
    /// [`sonicterm_app_core::AppStateMachine`]. The platform shell
    /// ([`crate::shell::MacShell`]) constructs the machine first,
    /// then hands it in so all state mutation routes through the
    /// reducer that the shell already owns — instead of `App`
    /// silently building a parallel machine inside its `new_with_proxy`.
    pub fn new_with_proxy_and_machine(
        mut theme: Theme,
        config: Config,
        keymap: Keymap,
        event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
        machine: sonicterm_app_core::AppStateMachine,
    ) -> Self {
        theme.apply_accessibility(&config.accessibility);
        let i18n = sonicterm_ui::i18n::I18n::new(if config.locale.is_empty() {
            None
        } else {
            Some(config.locale.as_str())
        });
        let mut command_palette = CommandPalette::new();
        command_palette.set_keymap(&keymap);
        Self {
            theme,
            config,
            keymap,
            clipboard: Clipboard::new().ok(),
            pending_new_window: false,
            pending_tear_out: None,
            pending_os_teardown: false,
            test_post_snapshot_hook: None,
            pending_exit: false,
            command_palette,
            palette_attached_window: None,
            os_drag_handoff_started: false,
            windows: HashMap::new(),
            main_window_id: None,
            frontmost_window: None,
            pending_os_drag_payloads: Vec::new(),
            theme_loader: None,
            keymap_loader: None,
            config_watcher: None,
            event_loop_proxy,
            // Default to 60 Hz until `resumed` probes the actual
            // monitor refresh rate. ~16.667 ms = 1/60 s.
            frame_period: Duration::from_micros(16_667),
            pending_redraw: false,
            pending_redraw_windows: HashSet::new(),
            input_dirty: false,
            pty_burst_gen: Arc::new(AtomicU32::new(0)),
            last_seen_burst_gen: 0,
            i18n,
            os_drag_sink: None,
            os_drag_backend: None,
            os_drag_pending: Arc::new(os_drag::PendingDragOutcome::default()),
            os_drag_bars: Arc::new(os_drag::TabBarRegistry::default()),
            os_drag_source: None,
            tab_bar_visible: true,
            broadcast: BroadcastState::Off,
            on_resumed: None,
            on_window_ready: None,
            redraw_request_count: std::sync::atomic::AtomicUsize::new(0),
            reap_call_count: std::sync::atomic::AtomicUsize::new(0),
            test_viewport_override: None,
            machine,
        }
    }

    #[doc(hidden)]
    pub fn poll_command_events_for_all_tabs(&mut self) {
        let n = self.main_tab_states().map(|ts| ts.len()).unwrap_or(0);
        for tab_idx in 0..n {
            self.poll_command_events_for_tab(tab_idx);
        }
    }

    pub(super) fn poll_command_events_for_tab(&mut self, tab_idx: usize) {
        let Some(id) = self.main_window_id else { return };
        let Some(ws) = self.windows.get_mut(&id) else { return };
        poll_command_events_for_tab_state(
            &ws.panes,
            &mut ws.tab_states,
            &mut ws.tabs,
            &self.config,
            tab_idx,
        );
    }

    #[doc(hidden)]
    pub fn __test_push_pane_command_event(
        &mut self,
        pane_id: u64,
        event: CommandEvent,
        at: Instant,
        duration: Option<Duration>,
    ) {
        if let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) {
            pane.command_events.lock().push(PaneCommandEvent { event, at, duration });
        }
    }

    #[doc(hidden)]
    pub fn __test_command_status_for_tab(&self, tab_idx: usize) -> Option<CommandStatus> {
        self.main_tab_states()?.get(tab_idx).map(|st| st.command.clone())
    }

    #[doc(hidden)]
    pub fn __test_tab_badge(&self, tab_idx: usize, now: Instant) -> Option<&'static str> {
        let tabs = self.main_tabs()?;
        tabs.tabs()
            .get(tab_idx)
            .and_then(|tab| tab.command.clone().badge(now, tab_idx == tabs.active_index()))
    }
}

#[doc(hidden)]
pub fn poll_command_events_for_tab_state(
    panes: &HashMap<u64, PaneState>,
    tab_states: &mut [TabState],
    tabs: &mut TabBar,
    config: &Config,
    tab_idx: usize,
) {
    let Some(tab_state) = tab_states.get_mut(tab_idx) else { return };
    let pane_ids = tab_state.tree.leaves();
    let mut events = Vec::new();
    for pane_id in pane_ids {
        if let Some(pane) = panes.get(&pane_id) {
            let mut q = pane.command_events.lock();
            events.extend(q.drain(..));
        }
    }
    if events.is_empty() {
        return;
    }
    for ev in events {
        match ev.event {
            CommandEvent::CmdStart => tab_state.command = CommandStatus::Running(ev.at),
            CommandEvent::CmdEnd(exit) => {
                tab_state.command =
                    CommandStatus::Done { exit, until: ev.at + Duration::from_secs(3) };
                maybe_notify_long_command(config, ev.duration, exit);
            }
            CommandEvent::PromptStart => {}
        }
    }
    if let Some(t) = tab_states.get(tab_idx).map(|st| st.command.clone()) {
        tabs.set_command_status(tab_idx, t);
    }
}

#[doc(hidden)]
pub fn poll_command_events_for_child_window(child: &mut WindowState, config: &Config) {
    for tab_idx in 0..child.tab_states.len() {
        poll_command_events_for_tab_state(
            &child.panes,
            &mut child.tab_states,
            &mut child.tabs,
            config,
            tab_idx,
        );
    }
}

fn maybe_notify_long_command(config: &Config, duration: Option<Duration>, exit: Option<u8>) {
    let Some(duration) = duration else { return };
    if !config.notifications.long_command {
        return;
    }
    if duration.as_secs() <= config.notifications.threshold_secs {
        return;
    }
    let result = match exit {
        Some(0) => "completed successfully",
        Some(code) => return notify_command_done(format!("Command failed with exit code {code}")),
        None => "completed",
    };
    notify_command_done(format!("Command {result} after {}s", duration.as_secs()));
}

#[cfg(target_os = "windows")]
fn notify_command_done(body: String) {
    if let Err(err) = notify_rust::Notification::new().summary("Command done").body(&body).show() {
        tracing::debug!(?err, "desktop notification failed");
    }
}

#[cfg(not(target_os = "windows"))]
fn notify_command_done(_body: String) {}

impl App {
    /// Returns `true` when closing the last window should exit the
    /// process, given a config. On macOS we honor
    /// [`Config::quit_on_last_window_close`] (default `true` →
    /// traditional terminal: closing the last window quits the app;
    /// set to `false` for Chrome/Firefox-style dock-alive). On other platforms there is no dock concept, so we
    /// always exit once the last window is gone — the config is
    /// ignored. Exposed (test-only) so behavior is verifiable without
    /// building a real winit event loop.
    #[doc(hidden)]
    pub fn should_exit_on_last_window_close(config: &Config) -> bool {
        #[cfg(target_os = "macos")]
        {
            config.quit_on_last_window_close
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = config;
            true
        }
    }

    /// Called from the `RedrawRequested` handler when the active pane's
    /// parser lock is contended (held by the VT thread mid-parse).
    /// Marks `pending_redraw` so `about_to_wait` schedules a
    /// `WaitUntil` at the next vsync boundary, and preserves the
    /// `input_dirty` flag captured at the start of the handler so the
    /// rescheduled redraw still bypasses the vsync coalescing gate.
    ///
    /// Without this, a single contended `try_lock` during the
    /// input→output transition of a multi-round prompt (e.g.
    /// `gh auth login`'s device-code flow, Issue #175) would silently
    /// drop the redraw request — the parsed bytes sat in the grid
    /// unrendered until an unrelated event (Ctrl+C, mouse move) woke
    /// the loop and triggered a fresh `RedrawRequested`.
    #[doc(hidden)]
    pub fn defer_redraw_on_lock_contention(&mut self, was_dirty: bool) {
        self.pending_redraw = true;
        self.input_dirty = was_dirty;
    }

    /// Child-window analogue of [`Self::defer_redraw_on_lock_contention`]
    /// plus the vsync coalescing gate. Records `win_id` in
    /// [`Self::pending_redraw_windows`] so `about_to_wait` schedules a
    /// `WaitUntil` at that child's next frame boundary and
    /// `new_events` re-requests the redraw there — instead of the child
    /// busy-spinning a bare `request_redraw()` that re-contends the very
    /// parser lock the VT thread needs to drain a burst (Issue #43:
    /// `ls -al` was smooth in main but laggy in a torn-out child because
    /// the child render path had neither the gate nor this backoff).
    /// Preserves the `input_dirty` flag captured at the top of the
    /// handler so a deferred input-driven redraw still bypasses the gate
    /// when it re-fires.
    #[doc(hidden)]
    pub fn defer_child_redraw(&mut self, win_id: WindowId, was_dirty: bool) {
        self.pending_redraw_windows.insert(win_id);
        self.input_dirty = was_dirty;
    }

    /// Test-only: `true` if `win_id` has a deferred redraw queued in
    /// [`Self::pending_redraw_windows`] (the child-window coalescing latch).
    #[doc(hidden)]
    pub fn __test_child_redraw_deferred(&self, win_id: WindowId) -> bool {
        self.pending_redraw_windows.contains(&win_id)
    }

    /// Test-only: read the shared input-driven-redraw flag.
    #[doc(hidden)]
    pub fn __test_input_dirty(&self) -> bool {
        self.input_dirty
    }

    /// Install a one-shot callback fired at the top of the first
    /// `ApplicationHandler::resumed` tick. macOS uses this to install
    /// the native NSMenu after winit has built the AppKit event loop —
    /// installing earlier leaves AppKit with only the default
    /// `Apple, sonicterm-mac` menu bar.
    pub fn set_on_resumed<F: FnOnce() + Send + 'static>(&mut self, hook: F) {
        self.on_resumed = Some(Box::new(hook));
    }

    /// Set the one-shot hook fired right after window creation, with
    /// the window's raw handle. See the field docs for the use-case
    /// (Windows muda menubar install).
    pub fn set_on_window_ready<F>(&mut self, hook: F)
    where
        F: FnOnce(raw_window_handle::RawWindowHandle) + Send + 'static,
    {
        self.on_window_ready = Some(Box::new(hook));
    }

    /// Translate a UI message id. See [`sonicterm_ui::i18n::I18n::t`]. Returns
    /// the key itself if no bundle (active or English fallback) has it,
    /// so the UI never renders an empty label.
    pub fn t(&self, key: &str) -> String {
        self.i18n.t(key)
    }

    /// Translate with `{ $name }` arguments. See
    /// [`sonicterm_ui::i18n::I18n::t_args`].
    pub fn t_args(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.i18n.t_args(key, Some(args))
    }

    /// Currently active locale tag (e.g. `"en"`, `"zh-CN"`).
    pub fn locale(&self) -> String {
        self.i18n.locale()
    }

    /// Live-apply a new locale. Persists the choice to `self.config.locale`.
    /// Pass `""` to mean "auto-detect from OS locale".
    pub fn set_locale(&mut self, requested: &str) {
        self.config.locale = requested.to_string();
        self.i18n = sonicterm_ui::i18n::I18n::new(if requested.is_empty() {
            None
        } else {
            Some(requested)
        });
    }

    /// Decide whether the event loop should exit. The app should keep
    /// running as long as ANY window owns at least one tab — that is,
    /// the main window has tabs AND is visible, OR any child window is
    /// still alive. This is shared by both the main-window
    /// `CloseRequested` handler and the post-merge drain check so a
    /// drained-but-still-visible main with live children doesn't kill
    /// the app.
    #[doc(hidden)]
    pub fn should_exit(&self) -> bool {
        let main_alive =
            !self.main_is_hidden() && !self.main_tabs().map(|t| t.is_empty()).unwrap_or(true);
        // Phase B2 PR-A: subtract the shadow main entry so
        // "no torn-out children" still tips this to true.
        !main_alive && self.child_window_count() == 0
    }

    /// Test-only: pure policy fn mirroring `should_exit` so integration
    /// tests can exercise the rule without constructing a real
    /// `WindowState` (which requires a live winit Window + GpuRenderer).
    #[doc(hidden)]
    pub fn should_exit_pure(main_tabs: usize, main_hidden: bool, child_count: usize) -> bool {
        let main_alive = !main_hidden && main_tabs > 0;
        !main_alive && child_count == 0
    }

    /// PR-B4 (#365): is the main window currently hidden / drained?
    /// `true` when the main `WindowState` is gone OR its `hidden` latch
    /// is set. The two shapes mean the same thing operationally — no
    /// visible main — so callers don't need to discriminate.
    #[doc(hidden)]
    pub fn main_is_hidden(&self) -> bool {
        match self.main() {
            Some(ws) => ws.hidden,
            None => true,
        }
    }

    /// Test-only: read the main window's `hidden` latch via the unified
    /// accessor.
    #[doc(hidden)]
    pub fn __test_main_hidden(&self) -> bool {
        self.main_is_hidden()
    }

    /// Test-only: drive the production `hide_main_window` path from
    /// integration tests (the helper itself is `pub(super)`).
    #[doc(hidden)]
    pub fn __test_hide_main_window(&mut self) {
        self.hide_main_window();
    }

    /// Test-only: read the deferred-exit flag set by `run_action`
    /// when the user's Cmd+W chain has drained the last tab of the
    /// last window in `quit_on_last_window_close = true` mode.
    #[doc(hidden)]
    pub fn __test_pending_exit(&self) -> bool {
        self.pending_exit
    }

    /// Unified "did this close just empty the affected window?" check
    /// for the keymap path. Mirrors what the mouse-click close-button
    /// path in `window_event.rs` and the OS `CloseRequested` arm do —
    /// hide the main window (or exit, on the last window) when its
    /// tabs vec is empty, and reap child windows the same way the drag-
    /// merge path does. The flag set here is drained in
    /// `do_about_to_wait`.
    pub(super) fn reap_empty_main_window_after_close(&mut self) {
        if !self.main_tabs().map(|t| t.is_empty()).unwrap_or(true) {
            return;
        }
        if self.child_window_count() == 0 {
            if Self::should_exit_on_last_window_close(&self.config) {
                self.pending_exit = true;
            } else {
                // Chrome-style: keep the process alive but hide the
                // empty main window.
                self.hide_main_window();
            }
        } else {
            // Children still own tabs — just hide main; exit decision
            // happens when the last child closes.
            self.hide_main_window();
        }
    }

    /// Test-only: force-set the main window's `hidden` latch so
    /// post-merge drain-policy tests can simulate the "main already
    /// retired" state without driving a real winit close event.
    #[doc(hidden)]
    pub fn __test_set_main_hidden(&mut self, v: bool) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.hidden = v;
        }
    }

    fn active_pane_id(&self) -> Option<u64> {
        let ws = self.main()?;
        let i = ws.tabs.active_index();
        ws.tab_states.get(i).map(|t| t.active_pane)
    }

    fn active_pane(&self) -> Option<&PaneState> {
        let id = self.active_pane_id()?;
        self.main()?.panes.get(&id)
    }

    fn write_to_pty(&self, bytes: Vec<u8>) {
        let Some(active_id) = self.active_pane_id() else { return };
        self.write_to_pane(active_id, bytes.clone());
        self.broadcast_from(active_id, bytes);
    }

    /// Test-only mirror of the normal KeyboardInput dispatch order: try every
    /// keymap spelling before encoding bytes for PTY forwarding.
    #[doc(hidden)]
    pub fn __test_dispatch_key_or_encode_pty(
        &mut self,
        key: &winit::keyboard::Key,
        mods: winit::keyboard::ModifiersState,
    ) -> (Option<Action>, Option<Vec<u8>>) {
        self.__test_dispatch_key_or_encode_pty_with_drain(key, mods, false)
    }

    /// Test-only mirror of the child-window KeyboardInput action path.
    /// The production child handler drains `pending_new_window` immediately
    /// after `run_action`; this helper exposes the same post-dispatch state
    /// without requiring a live `ActiveEventLoop`.
    #[doc(hidden)]
    pub fn __test_dispatch_key_or_encode_pty_with_drain(
        &mut self,
        key: &winit::keyboard::Key,
        mods: winit::keyboard::ModifiersState,
        simulate_drain: bool,
    ) -> (Option<Action>, Option<Vec<u8>>) {
        for key_str in key_to_strings(key, mods) {
            if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                if self.run_action(&action) {
                    if simulate_drain && self.pending_new_window {
                        self.pending_new_window = false;
                    }
                    return (Some(action), None);
                }
            }
        }
        let kitty_flags =
            self.active_pane().map(|pane| pane.parser.lock().kitty_keyboard_flags()).unwrap_or(0);
        (None, encode_logical(key, mods, kitty_flags))
    }

    fn write_to_pane(&self, pane_id: u64, bytes: Vec<u8>) {
        // M6a-expand-2b leaf-routing demonstration: the keystroke /
        // broadcast / encoded-input path now flows through the
        // winit-agnostic `AppStateMachine`. The reducer translates
        // `AppIntent::PtyWrite` into `AppEffect::PtyWrite { pane,
        // data }`, and `dispatch_pty_write_effect` is the boundary
        // method that performs the actual `pty.in_tx.send(...)`. The
        // net behaviour is identical to the pre-2b direct call; the
        // boundary is what changes so subsequent migration PRs
        // (2c+) can lift more state into the reducer without
        // touching this call site again.
        let intent = sonicterm_app_core::AppIntent::PtyWrite {
            pane: sonicterm_app_core::PaneId(pane_id),
            bytes: bytes::Bytes::from(bytes),
        };
        // The state machine is owned by `&mut self` in production
        // code paths; `write_to_pane` is `&self` because broadcast
        // fan-out borrows immutably. Run the reducer through a
        // throwaway transient machine — the reducer for PtyWrite is
        // pure (it does not touch `AppState`), so this is
        // semantically equivalent to dispatching through `self.machine`
        // and avoids a structural borrow refactor (deferred to 2c).
        let mut transient =
            sonicterm_app_core::AppStateMachine::new(sonicterm_app_core::AppState::default());
        for effect in transient.handle(intent) {
            self.dispatch_pty_write_effect(&effect);
        }
    }

    /// Boundary handler for [`sonicterm_app_core::AppEffect::PtyWrite`].
    ///
    /// Resolves the pane id back to a live [`PtyHandle`] on the main
    /// window and forwards the bytes. M6a-expand-2b boundary layer
    /// per spec §9.
    pub(crate) fn dispatch_pty_write_effect(&self, effect: &sonicterm_app_core::AppEffect) {
        if let sonicterm_app_core::AppEffect::PtyWrite { pane, data } = effect {
            let pane_id = pane.0;
            if let Some(p) = self.main().and_then(|ws| ws.panes.get(&pane_id)) {
                if let Some(pty) = p.pty.as_ref() {
                    let _ = pty.in_tx.send(data.to_vec());
                }
            }
        }
    }

    /// Generic boundary dispatcher for an Effect batch produced by the
    /// state machine. M6a-expand-2b handles the leaf classes (PTY,
    /// clipboard set, OpenURL, Quit, Render-reasons that map to a
    /// redraw request). Non-leaf classes (WindowOpen, ChildSpawn,
    /// MenubarUpdate, …) intentionally fall through to a tracing
    /// debug — they land in 2c.
    pub(crate) fn dispatch_effects(
        &mut self,
        effects: smallvec::SmallVec<[sonicterm_app_core::AppEffect; 4]>,
    ) {
        use sonicterm_app_core::AppEffect;
        for effect in effects {
            match effect {
                AppEffect::PtyWrite { .. } => {
                    self.dispatch_pty_write_effect(&effect);
                }
                AppEffect::ClipboardSet { text } => {
                    if !text.is_empty() {
                        if let Some(cb) = self.clipboard.as_mut() {
                            let _ = cb.set_text(text);
                        }
                    }
                    // Empty text sentinel (M6a-expand-2b CopySelection):
                    // the boundary's existing `copy_selection` already
                    // resolved the selection; the sentinel exists so
                    // the Intent→Effect contract is observable in
                    // tests. Real text payloads land in 2c.
                }
                AppEffect::OpenURL { url } => {
                    if sonicterm_cfg::url_open::validate(&url).is_ok() {
                        let _ = sonicterm_cfg::url_open::open(&url);
                    }
                }
                AppEffect::Quit => {
                    self.pending_exit = true;
                }
                AppEffect::Render { .. } | AppEffect::RenderDirtyRect { .. } => {
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                        self.redraw_request_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                // ── PTY class (M6a-expand-2c-wire) ────────────────────
                //
                // PtyClose: the per-pane `PtyHandle::Drop` impl already
                // SIGKILLs the child (CLAUDE.md §4 land-mine). Removing
                // the pane entry from `WindowState.panes` is what
                // actually triggers the drop. We try the main window
                // first; if not found, scan child windows.
                AppEffect::PtyClose { pane } => {
                    let pane_id = pane.0;
                    let closed = self.close_pty_pane(pane_id);
                    tracing::debug!(target: "state_machine", pane = pane_id, closed, "dispatch_effects: PtyClose");
                }
                // ChildExitPropagate: observability — the renderer's
                // poll loop already noticed the child exit and updated
                // the per-pane status. Surface a structured log so the
                // session-restore layer (post-v1.0) can correlate.
                AppEffect::ChildExitPropagate { pane, status } => {
                    tracing::info!(target: "state_machine", pane = pane.0, status, "child exit propagated");
                }
                // ChildSpawn: record-only at the boundary. Production
                // pane spawning flows through `App::spawn_pane` /
                // `spawn_tab_in_child`, which constructs the PTY
                // directly; the effect here is the observable contract.
                AppEffect::ChildSpawn { pane, argv0 } => {
                    tracing::debug!(target: "state_machine", pane = pane.0, %argv0, "dispatch_effects: ChildSpawn (record-only)");
                }
                // ── OS drag class ────────────────────────────────────
                //
                // The actual platform OS drag is initiated by the
                // tear-out / tab-drag path which talks directly to the
                // platform backend (NSPasteboard / OLE). The reducer
                // emits OsDragStart for observability + future
                // session-restore.
                AppEffect::OsDragStart { src_window, payload_tab } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = src_window.0,
                        tab = payload_tab,
                        "dispatch_effects: OsDragStart (platform path owns the actual drag)"
                    );
                }
                // OsDragEnd: settle the pending-drag table so the
                // tear-out boundary can finalize. The os_drag layer's
                // PendingDragOutcome already tracks the outcome
                // bilaterally; we surface a log here.
                AppEffect::OsDragEnd { src_window, committed } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = src_window.0,
                        committed,
                        "dispatch_effects: OsDragEnd"
                    );
                }
                // ── Clipboard / notification side channels ───────────
                //
                // ClipboardRequest: async paste handshake. The actual
                // read happens through `clipboard.get_text()` at the
                // boundary's paste path; here we surface the request.
                AppEffect::ClipboardRequest { window, bracketed } => {
                    if let Some(cb) = self.clipboard.as_mut() {
                        if let Ok(text) = cb.get_text() {
                            tracing::debug!(
                                target: "state_machine",
                                window = window.0,
                                bracketed,
                                len = text.len(),
                                "dispatch_effects: ClipboardRequest fulfilled"
                            );
                        }
                    }
                }
                // Notification: route through the existing
                // `notify_command_done` path (test capture friendly).
                AppEffect::Notification { title, body } => {
                    let combined = if title.is_empty() { body } else { format!("{title}: {body}") };
                    notify_command_done(combined);
                }
                // ── Window ops ───────────────────────────────────────
                //
                // WindowOpen: defer to the existing pending-new-window
                // flag drained by event_loop on the next tick. The
                // platform-creation requires `&ActiveEventLoop` which
                // dispatch_effects doesn't carry — flagging keeps the
                // request observable without changing the dispatcher
                // signature.
                AppEffect::WindowOpen { role, initial_size } => {
                    self.pending_new_window = true;
                    tracing::debug!(
                        target: "state_machine",
                        ?role,
                        ?initial_size,
                        "dispatch_effects: WindowOpen queued (drained by event_loop)"
                    );
                }
                // WindowClose: best-effort. Without a WindowKey→WindowId
                // map (lifted in 2d), close the main window or, if it's
                // a child, the matching entry. We at minimum surface a
                // log and set pending_exit when it's the last live
                // window per the reducer's contract.
                AppEffect::WindowClose { window } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        "dispatch_effects: WindowClose (platform path closes via WindowEvent::CloseRequested)"
                    );
                }
                // WindowResize: programmatic resize. winit's
                // `set_inner_size` is the API; since `LogicalSize` here
                // is f64 cells (not pixels) per the reducer's contract,
                // emit a redraw so the boundary re-measures.
                AppEffect::WindowResize { window, size } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        w = size.width,
                        h = size.height,
                        "dispatch_effects: WindowResize (observability)"
                    );
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }
                // WindowMove: record-only; OS already moved the window.
                AppEffect::WindowMove { window, pos } => {
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        x = pos.x,
                        y = pos.y,
                        "dispatch_effects: WindowMove (record-only)"
                    );
                }
                // WindowSetTitle: programmatic title set. Best-effort
                // against the main window.
                AppEffect::WindowSetTitle { window, title } => {
                    if let Some(w) = self.main_window() {
                        w.set_title(&title);
                    }
                    tracing::debug!(
                        target: "state_machine",
                        window = window.0,
                        %title,
                        "dispatch_effects: WindowSetTitle"
                    );
                }
                // TimerSchedule / TimerCancel: the boundary's redraw
                // pacing uses winit's ControlFlow::WaitUntil directly
                // (#132). The reducer emitting these surfaces a
                // contract for future schedulers (e.g. cursor-blink
                // refactor); record-only today.
                AppEffect::TimerSchedule { id, at } => {
                    tracing::trace!(
                        target: "state_machine",
                        id,
                        ?at,
                        "dispatch_effects: TimerSchedule (record-only — winit ControlFlow drives pacing)"
                    );
                }
                AppEffect::TimerCancel { id } => {
                    tracing::trace!(
                        target: "state_machine",
                        id,
                        "dispatch_effects: TimerCancel (record-only)"
                    );
                }
                // ── Menubar ──────────────────────────────────────────
                //
                // MenubarUpdate: macOS rebuilds the NSMenu through the
                // existing `menubar_bridge`; Windows is a log-only
                // no-op per FINAL spec §5 (muda's menubar is owned by
                // the platform code path directly). We surface a debug
                // log either way so the request is observable.
                AppEffect::MenubarUpdate(model) => {
                    tracing::debug!(
                        target: "state_machine",
                        items = model.items.len(),
                        "dispatch_effects: MenubarUpdate (platform path owns NSMenu/muda mutation)"
                    );
                }
                // ── Log ──────────────────────────────────────────────
                //
                // LogEvent: forward to tracing at the requested level.
                AppEffect::LogEvent { level, target, msg } => {
                    use sonicterm_app_core::LogLevel;
                    // `target` is &'static str from the reducer but
                    // tracing's `target:` slot needs a literal at the
                    // call site, so capture both as fields instead.
                    match level {
                        LogLevel::Trace => {
                            tracing::trace!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Debug => {
                            tracing::debug!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Info => {
                            tracing::info!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Warn => {
                            tracing::warn!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                        LogLevel::Error => {
                            tracing::error!(target: "state_machine.log", reducer_target = target, "{msg}")
                        }
                    }
                }
                // `AppEffect` is #[non_exhaustive]; future variants
                // surface here as an unrouted log until wired.
                _ => {
                    tracing::trace!(target: "state_machine", "dispatch_effects: unrouted effect {:?}", effect);
                }
            }
        }
    }

    fn close_pty_pane(&mut self, pane_id: u64) -> bool {
        let mut closed = false;
        let mut resize_main = false;
        let mut redraw_main = false;

        if let Some(ws) = self.main_mut() {
            let active_tab = ws.tabs.active_index();
            for (tab_idx, st) in ws.tab_states.iter_mut().enumerate() {
                let leaves = st.tree.leaves();
                if !leaves.contains(&pane_id) {
                    continue;
                }
                if leaves.len() > 1 && st.tree.close(pane_id) {
                    if st.active_pane == pane_id {
                        st.active_pane =
                            leaves.into_iter().find(|id| *id != pane_id).unwrap_or(st.active_pane);
                    }
                    if tab_idx == active_tab {
                        resize_main = true;
                        redraw_main = true;
                    }
                }
                break;
            }
            closed = ws.panes.remove(&pane_id).is_some();
        }

        if resize_main {
            self.resize_visible_panes();
        }
        if redraw_main {
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
        if closed {
            return true;
        }

        for ws in self.windows.values_mut() {
            let mut resize_child = false;
            let mut redraw_child = false;
            let active_tab = ws.tabs.active_index();
            for (tab_idx, st) in ws.tab_states.iter_mut().enumerate() {
                let leaves = st.tree.leaves();
                if !leaves.contains(&pane_id) {
                    continue;
                }
                if leaves.len() > 1 && st.tree.close(pane_id) {
                    if st.active_pane == pane_id {
                        st.active_pane =
                            leaves.into_iter().find(|id| *id != pane_id).unwrap_or(st.active_pane);
                    }
                    if tab_idx == active_tab {
                        resize_child = true;
                        redraw_child = true;
                    }
                }
                break;
            }
            if ws.panes.remove(&pane_id).is_some() {
                if resize_child {
                    child_window::resize_visible_panes_in_child(ws);
                }
                if redraw_child {
                    ws.request_redraw();
                }
                return true;
            }
        }

        false
    }

    /// Drive a single [`AppIntent`] through the state machine and
    /// dispatch the resulting Effects through the boundary layer.
    /// M6a-expand-2b entry point — wires the winit-flavoured shell
    /// into the winit-agnostic reducer.
    pub fn dispatch_intent(&mut self, intent: sonicterm_app_core::AppIntent) {
        let effects = self.machine.handle(intent);
        self.dispatch_effects(effects);
    }

    fn broadcast_from(&self, active_id: u64, bytes: Vec<u8>) {
        let BroadcastState::On { source_pane, .. } = self.broadcast else { return };
        if active_id != source_pane {
            return;
        }
        let receivers = self.broadcast_receivers();
        for pane_id in receivers {
            self.write_to_pane(pane_id, bytes.clone());
        }
    }

    pub(crate) fn broadcast_receivers(&self) -> std::collections::BTreeSet<u64> {
        let Some(ws) = self.main() else { return Default::default() };
        self.broadcast.receiving_panes(&ws.tab_states, ws.tabs.active_index())
    }

    /// Test-only: how many tabs the named child window currently owns.
    #[doc(hidden)]
    pub fn __test_child_tab_count(&self, id: WindowId) -> Option<usize> {
        self.windows.get(&id).map(|c| c.tabs.len())
    }

    /// Test-only: how many panes the named child window currently owns.
    #[doc(hidden)]
    pub fn __test_child_pane_count(&self, id: WindowId) -> Option<usize> {
        self.windows.get(&id).map(|c| c.panes.len())
    }

    /// Test-only: pane ids owned by the named child window.
    #[doc(hidden)]
    pub fn __test_child_pane_ids(&self, id: WindowId) -> Option<Vec<u64>> {
        self.windows.get(&id).map(|c| c.panes.keys().copied().collect())
    }

    /// Test-only: install the headless per-window pane-viewport seam on a child
    /// so the split/close resize wiring runs without a renderer. #pane-geom
    #[doc(hidden)]
    pub fn __test_set_child_pane_viewport(
        &mut self,
        id: WindowId,
        outer: sonicterm_ui::pane::Rect,
        cell_w: f32,
        cell_h: f32,
    ) -> bool {
        match self.windows.get_mut(&id) {
            Some(c) => {
                c.test_pane_viewport = Some((outer, cell_w, cell_h));
                true
            }
            None => false,
        }
    }

    /// Test-only: split the active pane of the named child window to the right,
    /// driving the same `split_active_pane_in_child` path the keymap uses.
    #[doc(hidden)]
    pub fn __test_child_split_active_right(&mut self, id: WindowId) -> bool {
        self.split_active_pane_in_child(id, sonicterm_cfg::keymap::Direction::Right)
    }

    /// Test-only: grid (cols, rows) of a specific pane in the named child.
    #[doc(hidden)]
    pub fn __test_child_pane_grid_size(&self, id: WindowId, pane_id: u64) -> Option<(u16, u16)> {
        let pane = self.windows.get(&id)?.panes.get(&pane_id)?;
        let parser = pane.parser.lock();
        let grid = parser.grid();
        Some((grid.cols, grid.rows))
    }

    /// Test-only: the active pane id in the named child's active tab.
    #[doc(hidden)]
    pub fn __test_child_active_pane(&self, id: WindowId) -> Option<u64> {
        let child = self.windows.get(&id)?;
        let tab_idx = child.tabs.active_index();
        child.tab_states.get(tab_idx).map(|st| st.active_pane)
    }

    /// Test-only: `true` when the named child pane's scrollbar is currently
    /// inside its idle-visible window (i.e. `mark_active` fired recently).
    /// Used to assert wheel-scroll / view_top jumps light the auto-hide bar
    /// on torn-out windows the same way they do on the main window.
    #[doc(hidden)]
    pub fn __test_child_scrollbar_active(&self, id: WindowId, pane_id: u64) -> Option<bool> {
        let st = self.windows.get(&id)?.scrollbar_vis.get(&pane_id)?;
        let idle_ms = match st.last_active {
            Some(t) => t.elapsed().as_millis() as u64,
            None => u64::MAX,
        };
        Some(idle_ms < scrollbar_visibility::IDLE_HIDE_MS)
    }

    /// Test-only: write a child pane's `viewport_top_abs` through the same
    /// production path the scrollbar uses (`set_child_pane_view_top`), so a
    /// test can drive a scroll and observe the visibility side effect.
    #[doc(hidden)]
    pub fn __test_child_set_pane_view_top(
        &mut self,
        id: WindowId,
        pane_id: u64,
        view_top: u64,
        live_top: u64,
    ) {
        self.set_child_pane_view_top(id, pane_id, view_top, live_top);
    }

    /// Test-only: seed a synthetic child WindowState without constructing a
    /// real winit Window / GpuRenderer. The pane/tab bookkeeping mirrors a
    /// tear-out child, but `window` and `renderer` stay `None` so cargo-test
    /// can exercise App-level multi-window ownership invariants headlessly.
    #[doc(hidden)]
    pub fn __test_seed_child_window(&mut self, titles: &[&str]) -> WindowId {
        self.__test_synthetic_main();
        let id = next_synthetic_child_window_id();
        let mut tabs = TabBar::new();
        let mut tab_states = Vec::new();
        let mut panes = HashMap::new();
        for title in titles {
            let pane_id = next_pane_id();
            let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
            panes.insert(pane_id, PaneState::new(parser, None));
            tabs.push(Tab::new(*title));
            tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        let child = WindowState {
            role: WindowRole::Terminal,
            window: None,
            renderer: None,
            tabs,
            tab_states,
            panes,
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            selection: None,
            last_click_time: None,
            last_click_cell: (0, 0),
            click_count: 0,
            select_mode: SelectMode::Cell,
            select_anchor: (0, 0),
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: HashMap::new(),
            test_drag_chip_marker: None,
            test_pane_viewport: None,
        };
        self.windows.insert(id, child);
        id
    }

    /// Test-only (#438): inspect drag-gesture residue on a specific
    /// child window so an integration test can assert
    /// [`Self::cancel_drag_session`] clears EVERY window's state, not
    /// just the main one.
    #[doc(hidden)]
    pub fn __test_child_pressed_tab(&self, id: WindowId) -> Option<Option<usize>> {
        self.windows.get(&id).map(|ws| ws.pressed_tab)
    }

    #[doc(hidden)]
    pub fn __test_child_mouse_down(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|ws| ws.mouse_down)
    }

    #[doc(hidden)]
    pub fn __test_child_has_drag_session(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|ws| ws.drag_session.is_some())
    }

    #[doc(hidden)]
    pub fn __test_child_has_drag_target(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|ws| ws.drag_target.is_some())
    }

    /// Test-only (#438, PR #443 cycle-2): seed the headless drag-chip
    /// marker on a window so a subsequent [`Self::cancel_drag_session`]
    /// can be observed to have cleared it. Returns `false` if the window
    /// id is unknown. The marker is the cross-platform stand-in for
    /// `renderer.set_drag_chip(_)` on `renderer: None` test windows —
    /// production code flips it in the same loop iteration as the real
    /// renderer call, so the assertion fails if the per-window iteration
    /// is ever removed.
    #[doc(hidden)]
    pub fn __test_set_window_drag_chip_marker(&mut self, id: WindowId, present: bool) -> bool {
        if let Some(ws) = self.windows.get_mut(&id) {
            ws.test_drag_chip_marker = Some(present);
            true
        } else {
            false
        }
    }

    /// Test-only (#438, PR #443 cycle-2): read the drag-chip marker for
    /// a window. `None` ⇒ window absent OR marker never seeded;
    /// `Some(true)` ⇒ marker set & not yet cleared by cancel;
    /// `Some(false)` ⇒ marker was set and cancel ran on this window.
    #[doc(hidden)]
    pub fn __test_window_drag_chip_marker(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).and_then(|ws| ws.test_drag_chip_marker)
    }

    /// Test-only convenience: same as
    /// [`Self::__test_set_window_drag_chip_marker`] but for the
    /// synthetic main window (id from [`synthetic_main_window_id`]).
    #[doc(hidden)]
    pub fn __test_set_main_drag_chip_marker(&mut self, present: bool) -> bool {
        self.__test_set_window_drag_chip_marker(synthetic_main_window_id(), present)
    }

    /// Test-only convenience: read the main window's drag-chip marker.
    #[doc(hidden)]
    pub fn __test_main_drag_chip_marker(&self) -> Option<bool> {
        self.__test_window_drag_chip_marker(synthetic_main_window_id())
    }

    /// Test-only (#438): seed drag-gesture residue on a specific child
    /// window — `pressed_tab`, `mouse_down`, and a synthetic
    /// `drag_session` — without driving a real winit pointer event
    /// sequence. Returns true on success.
    #[doc(hidden)]
    pub fn __test_seed_child_drag_residue(
        &mut self,
        id: WindowId,
        pressed_tab: Option<usize>,
        mouse_down: bool,
        with_drag_session: bool,
    ) -> bool {
        let Some(ws) = self.windows.get_mut(&id) else {
            return false;
        };
        ws.pressed_tab = pressed_tab;
        ws.mouse_down = mouse_down;
        if with_drag_session {
            ws.drag_session = Some(crate::tab_drag::DragSession::new(0, (0.0, 0.0)));
        }
        true
    }

    /// Test-only: install a frontmost child id without going through a
    /// real `WindowEvent::Focused(true)` (which requires a winit window).
    /// PR-B4 (#365) replaced `focused_child` with `frontmost_window`;
    /// this kept the old name so the existing regression tests don't
    /// need touching, but it now drives the unified tracker.
    #[doc(hidden)]
    pub fn __test_set_focused_child(&mut self, id: Option<WindowId>) {
        self.__test_synthetic_main();
        self.frontmost_window = id;
    }

    /// Test-only: read back the current frontmost-child id.
    /// PR-B4 (#365) — returns `Some(id)` when `frontmost_window` points
    /// at a non-main entry, mirroring the old `focused_child` semantics.
    #[doc(hidden)]
    pub fn __test_focused_child(&self) -> Option<WindowId> {
        match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        }
    }

    /// Test-only: read back the current `frontmost_window`.
    #[doc(hidden)]
    pub fn __test_frontmost_window(&self) -> Option<WindowId> {
        self.frontmost_window
    }

    /// Test-only: install a `frontmost_window` id without going through a
    /// real `WindowEvent::Focused(true)` (which requires a winit window).
    /// Used by Epic #289 Phase A regression tests to assert that
    /// keymap-dispatched actions route to the right window's tab vec.
    #[doc(hidden)]
    pub fn __test_set_frontmost_window(&mut self, id: Option<WindowId>) {
        self.frontmost_window = id;
    }

    /// Test-only: resolve a chord string through the App's keymap.
    /// Used by `child_window_tab_actions_dispatch.rs` (issue #370) to
    /// pin down that the chords the child-window handler now dispatches
    /// (cmd+1, cmd+2, cmd+Right, cmd+Left) actually resolve to their
    /// expected Action variants.
    #[doc(hidden)]
    pub fn __test_keymap_lookup(&self, keys: &str) -> Option<Action> {
        self.keymap.lookup(keys).cloned()
    }

    /// Test-only: read the window the command palette is currently
    /// attached to. `None` = main window OR closed; `Some(id)` = that
    /// child window. Used by overlay-routing regression tests.
    #[doc(hidden)]
    pub fn __test_palette_attached_window(&self) -> Option<WindowId> {
        self.palette_attached_window
    }

    /// Test-only: whether the command palette is currently open.
    #[doc(hidden)]
    pub fn __test_palette_open(&self) -> bool {
        self.command_palette.is_open()
    }

    /// Test-only invoker for `open_search_in_child`. Mirrors the
    /// pattern used by `__test_invoke_close_active_tab_in_child` so
    /// integration tests can assert the stale-id no-op contract for
    /// the overlay routing follow-up.
    #[doc(hidden)]
    pub fn __test_invoke_open_search_in_child(&mut self, id: WindowId) -> bool {
        self.open_search_in_child(id)
    }

    /// Epic #289 Phase A — classify [`Self::frontmost_window`] without
    /// borrowing anything mutably. Returns:
    ///   * `FrontmostKind::None` if no sonic window has been focused yet,
    ///     focus is currently outside every sonic window, or the recorded
    ///     id no longer matches any live window (stale-id race).
    ///   * `FrontmostKind::Main` if the recorded id matches the main
    ///     window we currently own.
    ///   * `FrontmostKind::Child(id)` if the recorded id matches a live
    ///     entry in [`Self::windows`].
    ///   * `FrontmostKind::Other` for any non-terminal window — actions
    ///     should fall through to the safe
    ///     main-window default in that case.
    ///
    /// Pure read; no mutation, no logging. The keymap_dispatch arms call
    /// this first, then route to the matching mutator + redraw target.
    /// Phase B2 PR-A — borrow the main window's [`WindowState`] shadow
    /// entry from `self.windows`, keyed by [`Self::main_window_id`].
    /// Returns `None` before `do_resumed` has run (no main window yet)
    /// OR if the shadow entry is missing for any reason. PR-B will
    /// migrate readers (`self.tabs`, `self.renderer`, …) to go through
    /// this helper.
    #[doc(hidden)]
    pub fn main(&self) -> Option<&WindowState> {
        let id = self.main_window_id?;
        self.windows.get(&id)
    }

    /// Mutable counterpart of [`Self::main`].
    #[doc(hidden)]
    pub fn main_mut(&mut self) -> Option<&mut WindowState> {
        let id = self.main_window_id?;
        self.windows.get_mut(&id)
    }

    /// Phase B2 PR-B1a — borrow the main window's `Arc<Window>` from
    /// the shadow [`WindowState`]. Sole source of truth for the main
    /// window handle (the legacy `App.window` field was deleted in
    /// PR-B1a). Returns `None` before `do_resumed` has run.
    #[doc(hidden)]
    pub fn main_window(&self) -> Option<&Arc<Window>> {
        self.windows.get(&self.main_window_id?)?.window.as_ref()
    }

    /// Phase B2 PR-B1b (#293) — borrow the main window's `GpuRenderer`
    /// from its `WindowState`. Sole source of truth for the main
    /// renderer (legacy `App.renderer` field was deleted in PR-B1b).
    /// Returns `None` before `do_resumed` has run.
    #[doc(hidden)]
    pub fn main_renderer(&self) -> Option<&GpuRenderer> {
        self.windows.get(&self.main_window_id?)?.renderer.as_ref()
    }

    /// Mutable counterpart of [`Self::main_renderer`].
    #[doc(hidden)]
    pub fn main_renderer_mut(&mut self) -> Option<&mut GpuRenderer> {
        let id = self.main_window_id?;
        self.windows.get_mut(&id)?.renderer.as_mut()
    }

    /// Phase B2 PR-B2b (#365) — borrow the main window's [`TabBar`] from
    /// its [`WindowState`]. Sole source of truth (legacy `App.tabs` was
    /// deleted in PR-B2b). Returns `None` before `do_resumed` /
    /// `__test_synthetic_main` has populated the shadow entry.
    #[doc(hidden)]
    pub fn main_tabs(&self) -> Option<&TabBar> {
        Some(&self.windows.get(&self.main_window_id?)?.tabs)
    }

    /// Mutable counterpart of [`Self::main_tabs`].
    #[doc(hidden)]
    pub fn main_tabs_mut(&mut self) -> Option<&mut TabBar> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.tabs)
    }

    /// Phase B2 PR-B2b (#365) — borrow the main window's `Vec<TabState>`
    /// from its [`WindowState`]. Sole source of truth.
    #[doc(hidden)]
    pub fn main_tab_states(&self) -> Option<&[TabState]> {
        Some(self.windows.get(&self.main_window_id?)?.tab_states.as_slice())
    }

    /// Mutable counterpart of [`Self::main_tab_states`].
    #[doc(hidden)]
    pub fn main_tab_states_mut(&mut self) -> Option<&mut Vec<TabState>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.tab_states)
    }

    /// Phase B2 PR-B2c (#365) — borrow the main window's pane map from
    /// its [`WindowState`]. Sole source of truth (legacy `App.panes`
    /// was deleted in PR-B2c). Returns `None` before `do_resumed` /
    /// `__test_synthetic_main` has populated the shadow entry.
    #[doc(hidden)]
    pub fn main_panes(&self) -> Option<&HashMap<u64, PaneState>> {
        Some(&self.windows.get(&self.main_window_id?)?.panes)
    }

    /// Mutable counterpart of [`Self::main_panes`]. NOTE: this borrows
    /// the full main [`WindowState`] mutably via `windows.get_mut`, so
    /// callers needing panes + tabs/tab_states/renderer in one expression
    /// must instead `let ws = self.main_mut()?;` and field-disjoint
    /// split-borrow.
    #[doc(hidden)]
    pub fn main_panes_mut(&mut self) -> Option<&mut HashMap<u64, PaneState>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.panes)
    }

    /// Phase B2 PR-B3c (#365) — borrow the main window's selection
    /// `Option<Selection>` from its [`WindowState`]. Sole source of
    /// truth (legacy `App.selection` field was deleted in PR-B3c).
    /// Returns `None` (no main window) — `Some(None)` (no selection)
    /// — `Some(Some(_))` (active selection).
    #[doc(hidden)]
    pub fn main_selection(&self) -> Option<&Option<Selection>> {
        Some(&self.windows.get(&self.main_window_id?)?.selection)
    }

    /// Mutable counterpart of [`Self::main_selection`].
    #[doc(hidden)]
    pub fn main_selection_mut(&mut self) -> Option<&mut Option<Selection>> {
        let id = self.main_window_id?;
        Some(&mut self.windows.get_mut(&id)?.selection)
    }

    /// Phase B2 PR-B3c (#365) — borrow the main window's
    /// `ModifiersState` from its [`WindowState`]. Returns
    /// `ModifiersState::empty()` if the main window does not yet
    /// exist (safe default — no modifiers held).
    #[doc(hidden)]
    pub fn main_modifiers(&self) -> ModifiersState {
        self.main_window_id
            .and_then(|id| self.windows.get(&id))
            .map(|ws| ws.modifiers)
            .unwrap_or_else(ModifiersState::empty)
    }

    /// PR-B3c (#365) — replace the main window's selection.
    /// No-op when the main window does not yet exist.
    #[doc(hidden)]
    pub fn selection_set(&mut self, sel: Option<Selection>) {
        if let Some(ws) = self.main_mut() {
            ws.selection = sel;
        }
    }

    /// PR-B3c (#365) — replace the main window's copy-mode state.
    /// No-op when the main window does not yet exist.
    #[doc(hidden)]
    pub fn copy_mode_set(&mut self, st: Option<CopyModeState>) {
        if let Some(ws) = self.main_mut() {
            ws.copy_mode = st;
        }
    }

    /// Phase B2 PR-A — borrow the [`WindowState`] of whichever terminal
    /// window is OS-frontmost. Falls back to the main window when no
    /// frontmost has been recorded yet (matches the safe default in
    /// [`Self::frontmost_kind`]).
    #[doc(hidden)]
    pub fn frontmost(&self) -> Option<&WindowState> {
        let id = self.frontmost_window.or(self.main_window_id)?;
        self.windows.get(&id)
    }

    /// Mutable counterpart of [`Self::frontmost`].
    #[doc(hidden)]
    pub fn frontmost_mut(&mut self) -> Option<&mut WindowState> {
        let id = self.frontmost_window.or(self.main_window_id)?;
        self.windows.get_mut(&id)
    }

    #[doc(hidden)]
    pub fn frontmost_kind(&self) -> FrontmostKind {
        let Some(id) = self.frontmost_window else { return FrontmostKind::None };
        if let Some(w) = self.main_window() {
            if w.id() == id {
                return FrontmostKind::Main;
            }
        }
        if self.windows.contains_key(&id) {
            return FrontmostKind::Child(id);
        }
        // Recorded id doesn't match anything live (rare: window closed
        // between the focus event and the action dispatch). Treat as
        // "no frontmost" so callers fall back to the main-window default.
        FrontmostKind::None
    }

    /// Epic #289 Phase A — if [`Self::frontmost_window`] is `Some(_)`
    /// but classifies as `None` (recorded id no longer matches any
    /// live window), clear it. Called by keymap_dispatch arms BEFORE
    /// falling back to main, so the next dispatch doesn't retry the
    /// dead id. Returns `true` if a stale id was cleared (purely
    /// informational; callers ignore it today).
    #[doc(hidden)]
    pub fn clear_stale_frontmost(&mut self) -> bool {
        if self.frontmost_window.is_some() && self.frontmost_kind() == FrontmostKind::None {
            self.frontmost_window = None;
            return true;
        }
        false
    }

    /// Test-only invoker for [`Self::close_active_tab_in_child`]. Exists
    /// because the helper is `pub(super)` and tests live outside the
    /// `app` module tree.
    #[doc(hidden)]
    pub fn __test_invoke_close_active_tab_in_child(&mut self, id: WindowId) -> bool {
        self.close_active_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::reap_empty_child`]. Used by the
    /// PR #302 follow-up regression that pins `App::transfer_tab` onto
    /// the unified empty-window cleanup contract: a stale id is a
    /// silent no-op (no panic, no spurious `windows` mutation), which
    /// is the only behaviour we can reliably pin without a live
    /// `WindowState` (needs a wgpu surface + winit `Window`).
    #[doc(hidden)]
    pub fn __test_invoke_reap_empty_child(&mut self, id: WindowId) {
        self.reap_empty_child(id);
    }

    /// Test-only invoker for [`Self::close_tab_at_in_child`] — the
    /// per-index helper the close-button (×) hit-test path uses in a
    /// torn-out child window's tab bar.
    #[doc(hidden)]
    pub fn __test_invoke_close_tab_at_in_child(&mut self, id: WindowId, idx: usize) -> bool {
        self.close_tab_at_in_child(id, idx)
    }

    /// Test-only invoker for [`Self::close_active_pane_or_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_close_active_pane_or_tab_in_child(&mut self, id: WindowId) -> bool {
        self.close_active_pane_or_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::next_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_next_tab_in_child(&mut self, id: WindowId) -> bool {
        self.next_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::prev_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_prev_tab_in_child(&mut self, id: WindowId) -> bool {
        self.prev_tab_in_child(id)
    }

    /// Test-only invoker for [`Self::activate_tab_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_activate_tab_in_child(&mut self, id: WindowId, idx: usize) -> bool {
        self.activate_tab_in_child(id, idx)
    }

    /// Test-only invoker for [`Self::split_active_pane_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_split_active_pane_in_child(
        &mut self,
        id: WindowId,
        dir: sonicterm_cfg::keymap::Direction,
    ) -> bool {
        self.split_active_pane_in_child(id, dir)
    }

    /// Test-only invoker for [`Self::close_active_pane_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_close_active_pane_in_child(&mut self, id: WindowId) -> bool {
        self.close_active_pane_in_child(id)
    }

    /// Test-only invoker for [`Self::close_active_pane`] (the main-window
    /// pane close path). Pairs with [`Self::test_viewport_override`] so
    /// tests can exercise the production close path — including the #387
    /// post-close `resize_visible_panes` call that re-fits the surviving
    /// sibling's Grid + PtyHandle — without a live wgpu renderer.
    /// See `crates/sonicterm-app/tests/per_pane_resize.rs`.
    #[doc(hidden)]
    pub fn __test_invoke_close_active_pane(&mut self) {
        self.close_active_pane();
    }

    /// Test-only invoker for [`Self::focus_pane_dir_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_focus_pane_dir_in_child(
        &mut self,
        id: WindowId,
        dir: sonicterm_cfg::keymap::Direction,
    ) -> bool {
        self.focus_pane_dir_in_child(id, dir)
    }

    /// Test-only invoker for [`Self::toggle_active_pane_zoom_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_toggle_active_pane_zoom_in_child(&mut self, id: WindowId) -> bool {
        self.toggle_active_pane_zoom_in_child(id)
    }

    /// Test-only invoker for [`Self::resize_active_split_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_resize_active_split_in_child(
        &mut self,
        id: WindowId,
        dir: sonicterm_cfg::keymap::Direction,
    ) -> bool {
        self.resize_active_split_in_child(id, dir)
    }

    /// Test-only: count of tabs in the main App.
    #[doc(hidden)]
    pub fn __test_main_tab_count(&self) -> usize {
        self.main_tabs().map(|t| t.len()).unwrap_or(0)
    }

    /// Test-only: read the `pending_new_window` flag. Set by the
    /// `Action::NewWindow` dispatcher arm; consumed by
    /// `drain_pending_window_creates` (which needs a live
    /// `ActiveEventLoop` and so can't run in a unit test). The flag
    /// is the testable seam — see Phase E Haiku follow-up on PR #297.
    #[doc(hidden)]
    pub fn __test_pending_new_window(&self) -> bool {
        self.pending_new_window
    }

    /// Issue #553 Phase A test seam: read whether a typed in-process
    /// tear-out request has been queued (drained by
    /// `drain_pending_window_creates`). The request carries the
    /// source tab handle + Win32 cursor screen position from the
    /// `DroppedOnEmpty` branch of `handle_os_drag_ended`.
    #[doc(hidden)]
    pub fn __test_pending_tear_out(&self) -> Option<(WindowId, usize, (i32, i32))> {
        self.pending_tear_out
            .as_ref()
            .map(|t| (t.source_window, t.source_tab_idx, t.drop_screen_pos))
    }

    /// Issue #462 test seam: read the `pending_os_teardown` flag set
    /// by `handle_os_drag_ended` on the `DroppedOnEmpty` branch.
    #[doc(hidden)]
    pub fn __test_pending_os_teardown(&self) -> bool {
        self.pending_os_teardown
    }

    /// Issue #462 test seam: directly set `pending_os_teardown` so
    /// the race test can simulate the `DroppedOnEmpty` branch without
    /// forging a full OS-drag pending state.
    #[doc(hidden)]
    pub fn __test_set_pending_os_teardown(&mut self, v: bool) {
        self.pending_os_teardown = v;
    }

    /// Issue #462 test seam: drive `drain_pending_os_teardown` from
    /// integration tests (no `ActiveEventLoop` needed — the teardown
    /// drain doesn't create windows; only the window-create drain
    /// does).
    #[doc(hidden)]
    pub fn __test_drain_pending_os_teardown(&mut self) {
        self.drain_pending_os_teardown();
    }

    /// Test-only: count of entries in `self.windows`. Used by the
    /// `new_window_*` regression tests to assert that a real drain
    /// would change the windows-map cardinality (the post-drain
    /// state itself requires an `ActiveEventLoop`).
    ///
    /// Phase B2 PR-A: the shadow main entry inserted by
    /// [`Self::do_resumed`] is excluded so existing call sites that
    /// expected this to be "number of torn-out child terminal windows"
    /// keep their pre-PR-A semantics.
    #[doc(hidden)]
    pub fn __test_windows_len(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// Test-only: install a synthetic `drag_target` so the
    /// cross-window-merge gate can be exercised without driving a
    /// live winit cursor through `CursorMoved`.
    /// Pure decision used by the CursorMoved tear-out branch: would a
    /// call to `tear_out_tab` right now be a guaranteed no-op (because
    /// we have only one tab AND no cross-window drop target)? Hoisted
    /// out of `tear_out_tab` so the CursorMoved caller can decide
    /// *whether to invoke at all* and, crucially, leave gesture state
    /// (`pressed_tab`, `mouse_down`) intact when the answer is "yes".
    /// Without this gate, the production sequence (lone tab → cursor
    /// crosses tear-out threshold → cursor finally enters another
    /// window's bar) is impossible: the threshold trip would clear the
    /// gesture before the user ever reaches a sibling bar. Haiku
    /// review of PR #62 caught this.
    #[doc(hidden)]
    #[doc(hidden)]
    pub fn __test_set_drag_target(
        &mut self,
        target: Option<crate::tab_drag::DropTarget<WindowId>>,
    ) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.drag_target = target;
        }
    }

    /// Test-only (#533 Haiku Step-4): remove a window from `self.windows`
    /// without going through the production teardown paths. Used by
    /// `os_drag_cleanup.rs` to simulate the "window vanished between
    /// snapshot collection and iteration" race that `cancel_drag_session`
    /// tolerates via its `windows.get_mut(...) else { continue }` branch
    /// (see #462 defensive snapshot at `mod.rs:3337`). Returns `true` if
    /// the window existed and was removed, `false` otherwise.
    #[doc(hidden)]
    pub fn __test_remove_window(&mut self, id: WindowId) -> bool {
        self.windows.remove(&id).is_some()
    }

    /// Test-only (#533 Haiku Step-4 2nd-pass REVISE): install a callback
    /// that fires INSIDE [`Self::cancel_drag_session`], AFTER the
    /// `self.windows.keys()` snapshot is collected but BEFORE the
    /// per-id iteration body runs. Lets tests exercise the exact
    /// `get_mut(&id).else { continue }` race-tolerance branch by
    /// removing (or inserting) a window in between.
    #[doc(hidden)]
    pub fn __test_set_post_snapshot_hook<F>(&mut self, f: F)
    where
        F: FnOnce(&mut App) + Send + 'static,
    {
        self.test_post_snapshot_hook = Some(Box::new(f));
    }

    #[doc(hidden)]
    pub fn child_window_count(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// Phase B2 PR-A — `1` if the shadow main entry is present in
    /// [`Self::windows`], else `0`. Used by every "count torn-out
    /// child windows" path that pre-existed PR-A so they keep the
    /// same number.
    #[inline]
    #[doc(hidden)]
    pub fn shadow_main_count(&self) -> usize {
        match self.main_window_id {
            Some(id) if self.windows.contains_key(&id) => 1,
            _ => 0,
        }
    }

    /// Epic #289 Phase B — number of windows in the unified
    /// [`Self::windows`] map.
    /// Used by the regression suite to pin the rename + role tagging.
    #[doc(hidden)]
    pub fn unified_window_count(&self) -> usize {
        self.windows.len().saturating_sub(self.shadow_main_count())
    }

    /// Epic #289 Phase B — count entries in [`Self::windows`] whose
    /// role matches the argument. Today every entry is `Terminal`;
    #[doc(hidden)]
    pub fn windows_with_role(&self, role: crate::app::WindowRole) -> usize {
        self.windows
            .iter()
            .filter(|(id, w)| w.role == role && Some(**id) != self.main_window_id)
            .count()
    }

    /// Test-only: seed a synthetic tab with one pane that has no PTY
    /// attached (just a Parser owning a fresh Grid). Lets integration
    /// Read-back of [`Self::main_window_id`] for tests.
    #[doc(hidden)]
    pub fn __test_main_window_id(&self) -> Option<WindowId> {
        self.main_window_id
    }

    // #404: ShadowMainSnapshot helpers deleted — dpi + hovered_url
    // now live exclusively on WindowState.

    /// tests exercise tab/pane bookkeeping without spawning shells.
    #[doc(hidden)]
    pub fn __test_seed_tab(&mut self, title: &str) -> u64 {
        // Phase B2 PR-B2a (#365): ensure the synthetic main WindowState
        // entry exists before seeding. Future PRs B2b/c/d delete the
        // App.tabs/tab_states/panes fields outright, so seed writes
        // MUST land in `self.main_mut()` to survive that migration.
        self.__test_synthetic_main();
        let pane_id = next_pane_id();
        let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
        if let Some(ws) = self.main_mut() {
            ws.panes.insert(pane_id, PaneState::new(parser, None));
            ws.tabs.push(Tab::new(title));
            ws.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        pane_id
    }

    /// Phase B2 PR-B2a (#365): for tests that build an `App` without
    /// `do_resumed` running, insert a synthetic main `WindowState`
    /// entry (window=None, renderer=None) under a stable synthetic
    /// `WindowId` so test seeders can route writes through
    /// [`Self::main_mut`]. No-op if `main_window_id` is already set.
    /// In production [`Self::do_resumed`] detects the synthetic entry
    /// and removes it before inserting the real one.
    #[doc(hidden)]
    pub fn __test_synthetic_main(&mut self) {
        if self.main_window_id.is_some() {
            return;
        }
        let id = synthetic_main_window_id();
        let ws = WindowState {
            role: WindowRole::Terminal,
            window: None,
            renderer: None,
            tabs: TabBar::new(),
            tab_states: Vec::new(),
            panes: HashMap::new(),
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            selection: None,
            last_click_time: None,
            last_click_cell: (0, 0),
            click_count: 0,
            select_mode: SelectMode::Cell,
            select_anchor: (0, 0),
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: HashMap::new(),
            test_drag_chip_marker: None,
            test_pane_viewport: None,
        };
        self.windows.insert(id, ws);
        self.main_window_id = Some(id);
    }

    /// tests exercise tab/pane bookkeeping with a reply-capable parser but
    /// without spawning shells.
    #[doc(hidden)]
    pub fn __test_seed_tab_with_reply(
        &mut self,
        title: &str,
    ) -> (u64, crossbeam_channel::Receiver<Vec<u8>>) {
        self.__test_synthetic_main();
        let pane_id = next_pane_id();
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        let parser = Arc::new(Mutex::new(Parser::new_with_reply(Grid::new(80, 24), tx)));
        if let Some(ws) = self.main_mut() {
            ws.panes.insert(pane_id, PaneState::new(parser, None));
            ws.tabs.push(Tab::new(title));
            ws.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        (pane_id, rx)
    }

    /// Test-only: seed an existing synthetic pane parser with the app's
    /// current theme defaults. Mirrors the production spawn path without
    /// requiring a live PTY or reply-forwarder thread.
    #[doc(hidden)]
    pub fn __test_seed_pane_theme_colors(&mut self, pane_id: u64) -> bool {
        let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) else {
            return false;
        };
        let mut parser = pane.parser.lock();
        seed_parser_theme_colors(&mut parser, &self.theme);
        true
    }

    /// Test-only: feed bytes into an existing pane parser. Used by integration
    /// tests that need to assert reply bytes from the real pane parser.
    #[doc(hidden)]
    pub fn __test_advance_pane_parser(&self, pane_id: u64, bytes: &[u8]) -> bool {
        let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) else {
            return false;
        };
        pane.parser.lock().advance(bytes);
        true
    }

    /// Test-only: read-only access to the internal panes map so tests
    /// can assert "this pane id is gone after detach".
    #[doc(hidden)]
    pub fn __test_pane_ids(&self) -> Vec<u64> {
        self.main().map(|ws| ws.panes.keys().copied().collect()).unwrap_or_default()
    }

    /// Test-only: read a pane's current `viewport_top_abs`. Used by #412
    /// scrollback-scroll wiring tests to assert wheel + Scroll-keymap
    /// dispatch actually mutates the canonical field.
    #[doc(hidden)]
    pub fn __test_pane_viewport_top_abs(&self, pane_id: u64) -> Option<Option<u64>> {
        self.main()?.panes.get(&pane_id).map(|p| p.viewport_top_abs)
    }

    /// Test-only: synthesize scrollback by feeding `n` numbered lines and
    /// returns the resulting `scrollback_len()`. Each line is 4 chars +
    /// CRLF so callers can predict the row count.
    #[doc(hidden)]
    pub fn __test_grow_pane_scrollback(&self, pane_id: u64, n: u32) -> u64 {
        let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) else { return 0 };
        let mut buf = Vec::with_capacity((n as usize) * 8);
        for i in 0..n {
            use std::io::Write;
            let _ = write!(&mut buf, "{:04}\r\n", i % 10_000);
        }
        let mut parser = pane.parser.lock();
        parser.advance(&buf);
        parser.grid().scrollback_len() as u64
    }

    /// Test-only: viewport rows of a pane (for #412 PageUp/Down asserts).
    #[doc(hidden)]
    pub fn __test_pane_viewport_rows(&self, pane_id: u64) -> Option<u16> {
        let pane = self.main()?.panes.get(&pane_id)?;
        Some(pane.parser.lock().grid().rows)
    }

    /// Test-only: current grid size for a pane.
    #[doc(hidden)]
    pub fn __test_pane_grid_size(&self, pane_id: u64) -> Option<(u16, u16)> {
        let pane = self.main()?.panes.get(&pane_id)?;
        let parser = pane.parser.lock();
        let grid = parser.grid();
        Some((grid.cols, grid.rows))
    }

    /// Test-only: id of the active pane in a given tab. Returns `None`
    /// when `tab_idx` is out of range. Used by `split_focus.rs` to
    /// assert that splitting a pane plus the click-to-focus path
    /// actually flips the focused leaf.
    #[doc(hidden)]
    pub fn __test_active_pane_in_tab(&self, tab_idx: usize) -> Option<u64> {
        self.main_tab_states()?.get(tab_idx).map(|st| st.active_pane)
    }

    /// Test-only: set the active pane in `tab_idx` to `pane_id`. The
    /// click-to-focus logic in `window_event.rs` is the production
    /// caller; tests exercise the same state transition without
    /// driving a synthetic winit `MouseInput` event.
    #[doc(hidden)]
    pub fn __test_set_active_pane(&mut self, tab_idx: usize, pane_id: u64) -> bool {
        if let Some(st) = self.main_tab_states_mut().and_then(|ts| ts.get_mut(tab_idx)) {
            st.active_pane = pane_id;
            true
        } else {
            false
        }
    }

    /// Test-only: drive `split_active(Direction::Right)`. Mirrors the
    /// `Action::SplitRight` dispatch but skips the `Action` round-trip.
    #[doc(hidden)]
    pub fn __test_split_active_right(&mut self) {
        self.split_active(sonicterm_cfg::keymap::Direction::Right);
    }

    /// Test-only: tab count.
    #[doc(hidden)]
    pub fn __test_tab_count(&self) -> usize {
        self.main_tabs().map(|t| t.len()).unwrap_or(0)
    }

    /// Test-only: pending OS-drag payload count.
    #[doc(hidden)]
    pub fn __test_pending_os_drag_payload_count(&self) -> usize {
        self.pending_os_drag_payloads.len()
    }

    /// Test-only: drain queued OS-drag payloads after a synthetic main has
    /// been inserted. Mirrors the production `do_resumed` drain point without
    /// constructing a real winit window.
    #[doc(hidden)]
    pub fn __test_drain_pending_os_drag_payloads(&mut self) {
        self.drain_pending_os_drag_payloads();
    }

    /// Test-only: number of leaf panes in the given tab. Returns
    /// `None` when the tab index is out of range. Used by the
    /// `close_pane_or_tab_semantics` regression suite to assert that
    /// `Action::CloseActivePaneOrTab` shrinks the active tab's pane
    /// tree rather than the tab bar when the tab still has > 1 pane.
    #[doc(hidden)]
    pub fn __test_pane_count_in_tab(&self, tab_idx: usize) -> Option<usize> {
        self.main_tab_states()?.get(tab_idx).map(|st| st.tree.leaves().len())
    }

    /// Test-only: install an `OsDragSink` so [`Self::try_os_drag_handoff`]
    /// can be exercised without going through the platform entry point.
    #[doc(hidden)]
    pub fn __test_set_os_drag_sink(&mut self, sink: Arc<dyn crate::os_drag::OsDragSink>) {
        self.os_drag_sink = Some(sink);
    }

    /// Phase C2: install the platform OS-level drag-session backend.
    /// `sonicterm-mac` calls this with an NSDraggingSession impl,
    /// `sonicterm-windows` with an OLE DoDragDrop impl. Tests use it via
    /// [`Self::__test_set_os_drag_backend`] to inject a mock.
    #[doc(hidden)]
    pub fn set_os_drag_backend(&mut self, backend: Box<dyn os_drag::OsTabDragBackend>) {
        self.os_drag_backend = Some(backend);
    }

    /// Phase C2 test-only: install a mock [`os_drag::OsTabDragBackend`].
    #[doc(hidden)]
    pub fn __test_set_os_drag_backend(&mut self, backend: Box<dyn os_drag::OsTabDragBackend>) {
        self.os_drag_backend = Some(backend);
    }

    /// Phase C2 test-only: hand out the shared pending-outcome mailbox
    /// so tests can drive [`Self::handle_os_drag_ended`] without
    /// constructing a real [`winit::event_loop::EventLoopProxy`].
    #[doc(hidden)]
    pub fn __test_os_drag_pending(&self) -> Arc<os_drag::PendingDragOutcome> {
        self.os_drag_pending.clone()
    }

    /// Phase C2 test-only: seed the in-flight source bookkeeping that
    /// [`Self::begin_os_tab_drag`] normally sets. Used by tests that
    /// drive the dispatcher directly without first calling
    /// `begin_os_tab_drag`.
    #[doc(hidden)]
    pub fn __test_set_os_drag_source(&mut self, source: Option<(WindowId, usize)>) {
        self.os_drag_source = source;
    }

    /// Phase C2: build an [`os_drag::AppHandle`] tied to the App's
    /// event-loop proxy and the shared pending-outcome mailbox. The
    /// returned handle is what gets passed to
    /// [`os_drag::OsTabDragBackend::begin_session`] so the backend can
    /// post `DragMoved` / `DragEnded` events back to the main loop.
    ///
    /// Returns `None` when no event-loop proxy has been wired. In that
    /// case the OS drag is not startable, which the caller treats as
    /// "fall back to the existing within-process tear_out path".
    pub fn os_drag_app_handle(&self) -> Option<os_drag::AppHandle> {
        self.event_loop_proxy.clone().map(|p| {
            os_drag::AppHandle::with_pending_and_bars(
                p,
                self.os_drag_pending.clone(),
                self.os_drag_bars.clone(),
            )
        })
    }

    /// Hand out an `Arc` clone of the shared [`os_drag::TabBarRegistry`].
    /// Platform glue (e.g. `sonicterm-windows::os_drag_win`) calls this to
    /// stash a reference for use inside the OLE IDropTarget::Drop
    /// callback, where the AppHandle isn't always available.
    pub fn os_drag_bar_registry(&self) -> Arc<os_drag::TabBarRegistry> {
        self.os_drag_bars.clone()
    }

    /// Publish the current tab bar layout for `window` into the shared
    /// registry. Called from the App's per-frame render path with
    /// already-resolved screen coordinates (caller is responsible for
    /// converting logical-px / window-local to screen via
    /// winit's `Window::outer_position`).
    pub fn publish_os_drag_bar_snapshot(&self, snapshot: os_drag::TabBarSnapshot) {
        self.os_drag_bars.publish(snapshot);
    }

    /// Convenience: build a [`os_drag::TabBarSnapshot`] from the main
    /// window's current geometry + tab bar and publish it. No-op if the
    /// main window or renderer aren't yet initialized (pre-`resumed`).
    /// Called from the per-frame `RedrawRequested` handler so the
    /// snapshot registry tracks every visible tab-bar state change.
    pub(super) fn publish_main_window_tab_bar(&self) {
        use sonicterm_ui::tabbar_view::TabBarLayout;
        let Some(w) = self.main_window() else { return };
        let Some(r) = self.main_renderer() else { return };
        let inner_origin = w.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let inner_size = {
            let s = w.inner_size();
            (s.width, s.height)
        };
        let raster_w = inner_size.0 as f32;
        let empty_tabs_pub = sonicterm_ui::tabs::TabBar::new();
        let layout = TabBarLayout::compute_with_height(
            self.main_tabs().unwrap_or(&empty_tabs_pub),
            raster_w,
            r.tab_bar_logical_height(),
        )
        .with_top_offset(r.tab_bar_y_offset())
        .with_visible(r.tab_bar_visible());
        let snap =
            os_drag::TabBarSnapshot::from_layout(Some(w.id()), inner_origin, inner_size, &layout);
        self.publish_os_drag_bar_snapshot(snap);
    }

    /// Remove a window's snapshot from the registry (called on window
    /// close). Safe to call with `None` (matches main-window convention).
    pub fn remove_os_drag_bar_snapshot(&self, window: Option<WindowId>) {
        self.os_drag_bars.remove(window);
    }

    /// Publish the tab bar snapshot for the child window keyed by `id`.
    /// No-op if the child isn't found. Called from the child's redraw
    /// path right after `Renderer::render`.
    pub fn publish_child_window_tab_bar(&self, id: WindowId) {
        use sonicterm_ui::tabbar_view::TabBarLayout;
        let Some(child) = self.windows.get(&id) else { return };
        let Some(win) = child.window.as_ref() else { return };
        let inner_origin = win.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let inner_size = {
            let s = win.inner_size();
            (s.width, s.height)
        };
        let raster_w = inner_size.0 as f32;
        let Some(r) = child.renderer.as_ref() else { return };
        let layout =
            TabBarLayout::compute_with_height(&child.tabs, raster_w, r.tab_bar_logical_height())
                .with_top_offset(r.tab_bar_y_offset())
                .with_visible(r.tab_bar_visible());
        let snap =
            os_drag::TabBarSnapshot::from_layout(Some(id), inner_origin, inner_size, &layout);
        self.publish_os_drag_bar_snapshot(snap);
    }

    /// Phase C2: begin an OS-level tab drag session via the installed
    /// backend. Returns `true` when the backend was invoked, `false`
    /// when no backend is installed or no event-loop proxy exists (in
    /// which case the caller falls back to the existing tear_out path).
    ///
    /// Records `(source_window, source_tab_idx)` so the
    /// `UserEvent::DragEnded` dispatcher knows where the gesture
    /// originated when routing the outcome.
    pub fn begin_os_tab_drag(
        &mut self,
        source_window: WindowId,
        source_tab_idx: usize,
        payload_json: String,
        drag_image_png: Vec<u8>,
    ) -> bool {
        let Some(handle) = self.os_drag_app_handle() else { return false };
        let Some(backend) = self.os_drag_backend.as_mut() else { return false };
        backend.begin_session(handle, source_window, source_tab_idx, payload_json, drag_image_png);
        self.os_drag_source = Some((source_window, source_tab_idx));
        true
    }

    /// Phase C2: does the installed backend own the gesture end-to-end?
    /// `try_os_drag_handoff` consults this to decide whether to skip
    /// the legacy `OsDragSink` after `begin_os_tab_drag` returns —
    /// running both on Windows would invoke `DoDragDrop` twice.
    pub fn os_drag_backend_handles_full_gesture(&self) -> bool {
        self.os_drag_backend.as_ref().map(|b| b.handles_full_gesture()).unwrap_or(false)
    }

    /// Phase C2: register a winit window with the installed OS-drag
    /// backend so OS-level drops landing on that window's HWND /
    /// NSWindow are routed back into the App. Called once per window
    /// at creation time — main window from `App::resumed`, torn-out
    /// child windows from `tear_out_tab` / `tear_out_from_child`.
    ///
    /// No-op if no backend is installed (mac, tests) — the trait's
    /// default `register_window` impl is itself a no-op, so a backend
    /// that does not need per-window registration (mac) can opt out
    /// cleanly while still implementing the unified entry point.
    ///
    /// Without this call, drops on torn-out child windows on Windows
    /// silently never reach `IDropTarget::Drop` (Haiku #295 blocker).
    pub fn register_window_with_os_drag_backend(
        &mut self,
        window_id: WindowId,
        window: &std::sync::Arc<winit::window::Window>,
    ) {
        let Some(handle) = self.os_drag_app_handle() else { return };
        let Some(backend) = self.os_drag_backend.as_mut() else { return };
        backend.register_window(handle, window_id, window);
    }

    /// Phase C2: dispatcher entry point for `UserEvent::DragMoved`.
    /// Drains the mailbox; currently a no-op beyond logging — the
    /// drag-chip overlay is rendered from `tab_drag` state, not from
    /// the OS cursor stream. Reserved for future "highlight drop
    /// target in destination bar" feedback.
    pub fn handle_os_drag_moved(&mut self) -> Option<(i32, i32)> {
        let pos = self.os_drag_pending.take_moved();
        if let Some(p) = pos {
            tracing::trace!(?p, "os_drag_session: cursor moved");
        }
        pos
    }

    /// Phase C2: dispatcher entry point for `UserEvent::DragEnded`.
    /// Drains the mailbox outcome and routes it: `DroppedOnBar` →
    /// [`Self::transfer_tab`]; `Cancelled` → [`Self::cancel_drag_session`];
    /// `DroppedOnEmpty` is left for the existing tear_out path (this
    /// dispatcher just clears the in-flight bookkeeping). Returns the
    /// outcome that was processed for tests to assert on.
    pub fn handle_os_drag_ended(&mut self) -> Option<os_drag::DragOutcome> {
        let outcome = self.os_drag_pending.take_ended()?;
        let source = self.os_drag_source.take();
        match outcome {
            os_drag::DragOutcome::DroppedOnBar { target_window, target_slot } => {
                let Some((src_win, src_idx)) = source else {
                    tracing::warn!(
                        "os_drag_session: DroppedOnBar arrived with no recorded source — cancelling"
                    );
                    self.cancel_drag_session();
                    return Some(outcome);
                };
                // `source` / `target` are `Option<WindowId>`, where
                // `None` means "the App's main window". In Phase C2 the
                // backend always reports a concrete WindowId on the
                // source side, but the *target* may legitimately be the
                // main window. Detect that by comparing against the
                // App's `window` field.
                let src_opt = self
                    .main_window()
                    .map(|w| w.id())
                    .filter(|&id| id == src_win)
                    .map_or(Some(src_win), |_| None);
                let tgt_opt = match target_window {
                    Some(id) if self.main_window().map(|w| w.id() == id).unwrap_or(false) => None,
                    other => other,
                };
                if let Err(e) = self.transfer_tab(src_opt, src_idx, tgt_opt, target_slot) {
                    tracing::warn!(?e, "os_drag_session: transfer_tab refused — cancelling");
                    self.cancel_drag_session();
                }
            }
            os_drag::DragOutcome::DroppedOnEmpty { drop_screen_pos } => {
                tracing::debug!(
                    ?drop_screen_pos,
                    "os_drag_session: DroppedOnEmpty — in-process tear-out (Phase A)"
                );
                // Issue #553 Phase A: replace the legacy
                // out-of-process tear-out (child-window via
                // `spawn_tearout_child` → `Command::new`) with an
                // in-process create. Enqueue a typed `PendingTearOut`
                // request carrying the recorded source tab handle and
                // the Win32 cursor screen position; the next
                // event-loop tick drains it via the existing
                // `drain_pending_window_creates` slot, which now
                // builds the child window directly from the reusable
                // helper extracted from `tear_out.rs`.
                if let Some((src_win, src_idx)) = source {
                    self.pending_tear_out = Some(PendingTearOut {
                        source_window: src_win,
                        source_tab_idx: src_idx,
                        drop_screen_pos,
                    });
                } else {
                    tracing::warn!(
                        "os_drag_session: DroppedOnEmpty without recorded source — no tear-out"
                    );
                }
                // Issue #462 (speculative defensive fix per PM
                // override): do NOT call `cancel_drag_session` inline
                // here. The `DroppedOnEmpty` path triggers a
                // tear-out-spawn that creates a brand new top-level
                // window via the `pending_new_window` /
                // `pending_tear_out` drain. If we cancel inline,
                // cross-window drag-residue cleanup runs BEFORE the
                // new window exists, racing the spawn and potentially
                // freezing Explorer's drag thread on Windows when the
                // OLE drop-target tear-down sequence overlaps with new
                // HWND creation. Defer cancellation to
                // `drain_pending_os_teardown`, which runs AFTER
                // `drain_pending_window_creates` at the event-loop
                // boundary. Order matters; this flag controls only
                // WHEN cancel runs, not WHETHER — the all-windows
                // loop still runs unconditionally on drain (preserves
                // the `os_drag_cleanup.rs:172-201` idempotence
                // guarantee).
                self.pending_os_teardown = true;
            }
            os_drag::DragOutcome::Cancelled => {
                self.cancel_drag_session();
            }
        }
        Some(outcome)
    }

    /// Test-only: drive the OS-drag handoff path with a forced "cursor
    /// is outside any window" precondition (trivially true in tests
    /// since no winit window is created). Returns the same bool as the
    /// internal implementation: `true` = source-tab was detached,
    /// `false` = source tab preserved.
    #[doc(hidden)]
    pub fn __test_try_os_drag_handoff(&mut self, index: usize) -> bool {
        self.try_os_drag_handoff(index)
    }

    /// Test-only: inspect and mutate the drag-gesture state
    /// (`pressed_tab`, `mouse_down`) so an integration test can
    /// reproduce the production sequence "tab pressed → cursor
    /// crosses tear-out threshold → eventually drops on sibling
    /// window" without needing a live winit `ActiveEventLoop`.
    #[doc(hidden)]
    pub fn __test_pressed_tab(&self) -> Option<usize> {
        self.main().and_then(|ws| ws.pressed_tab)
    }

    #[doc(hidden)]
    pub fn __test_mouse_down(&self) -> bool {
        self.main().map(|ws| ws.mouse_down).unwrap_or(false)
    }

    #[doc(hidden)]
    pub fn __test_set_pressed_tab(&mut self, v: Option<usize>) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.pressed_tab = v;
        }
    }

    #[doc(hidden)]
    pub fn __test_set_mouse_down(&mut self, v: bool) {
        self.__test_synthetic_main();
        if let Some(ws) = self.main_mut() {
            ws.mouse_down = v;
        }
    }

    /// Test-only: borrow the redraw target Arc for a given pane id,
    /// so a test can assert the per-pane redraw indirection survives
    /// state transfers.
    #[doc(hidden)]
    pub fn __test_pane_redraw_target(&self, id: u64) -> Option<Arc<Mutex<Option<Arc<Window>>>>> {
        self.main()?.panes.get(&id).map(|p| p.redraw_target.clone())
    }

    /// Test-only: install or clear a pane's PTY handle so tear-out tests
    /// can verify ownership moves without spawning a real shell.
    #[doc(hidden)]
    pub fn __test_set_pane_pty(&mut self, id: u64, pty: Option<PtyHandle>) -> bool {
        let Some(pane) = self.main_mut().and_then(|ws| ws.panes.get_mut(&id)) else {
            return false;
        };
        pane.pty = pty;
        true
    }

    /// Test-only: report whether a pane still has a PTY handle.
    #[doc(hidden)]
    pub fn __test_pane_pty_present(&self, id: u64) -> Option<bool> {
        self.main()?.panes.get(&id).map(|pane| pane.pty.is_some())
    }

    /// Drain the config-watcher channel and apply any incoming config/keymap.
    /// Idempotent and cheap when nothing changed.
    #[doc(hidden)]
    pub fn poll_config_reload(&mut self) {
        let Some(watcher) = self.config_watcher.as_ref() else {
            return;
        };
        let (latest_config, latest_keymap) = watcher.try_latest_updates();
        if let Some(km) = latest_keymap {
            tracing::info!(
                "live-reload: keymap.toml -> {} ({} bindings)",
                km.meta.name,
                km.bindings.len()
            );
            self.command_palette.set_keymap(&km);
            self.keymap = km;
            self.input_dirty = true;
        }
        if let Some(cfg) = latest_config {
            self.apply_new_config(cfg);
        }
    }

    /// Read-only accessor used by tests and (eventually) the
    /// renderer to honor the View → Toggle Tab Bar menu item.
    #[doc(hidden)]
    pub fn tab_bar_visible(&self) -> bool {
        self.tab_bar_visible
    }

    /// Epic #289 Phase C — cancel an in-flight drag session. Wired
    /// to the ESC key handler in `window_event.rs` (any window's
    /// `WindowEvent::KeyboardInput` with `NamedKey::Escape` clears
    /// the App's drag_session AND every per-window drag_session) so
    /// the gesture is abandoned with the source tab left in place.
    /// Returns `true` if a drag session was actively cleared, `false`
    /// when no drag was in progress.
    #[doc(hidden)]
    pub fn cancel_drag_session(&mut self) -> bool {
        let mut had = false;
        // Issue #462 (defensive): snapshot window-id keys BEFORE the
        // mutation loop. The loop body calls `clear_drag_chip` /
        // `request_redraw`, neither of which mutate `self.windows`
        // today, but a future per-window handler (or a winit reentrant
        // callback on Windows under HOT-FILE PR pressure) could
        // insert/remove a window mid-iteration. Iterating a snapshot of
        // `Vec<WindowId>` is panic-free and matches intent: cancel
        // residue on the set of windows that exist RIGHT NOW. The
        // all-windows loop runs UNCONDITIONALLY — never short-circuit;
        // `os_drag_cleanup.rs:172-201` asserts this on a re-armed
        // second invocation.
        let ids: Vec<_> = self.windows.keys().copied().collect();
        // PR #533 Haiku Step-4 2nd-pass REVISE: invoke the test-only
        // post-snapshot hook AFTER `ids` is collected but BEFORE the
        // iteration body starts. The `take()` releases the hook so it
        // never re-fires, and (more importantly) leaves no live borrow
        // on `self` — the closure can freely mutate `self.windows`,
        // which is the exact race we need to exercise to prove the
        // `get_mut(&id).else { continue }` arm below fires. Always
        // `None` in production (the setter is `__test_*`-gated).
        if let Some(hook) = self.test_post_snapshot_hook.take() {
            hook(self);
        }
        // Issue #438: clear ALL per-window drag residue, not just
        // drag_session / drag_target. Previously `pressed_tab` and
        // `mouse_down` were only cleared on the main window, and the
        // renderer's `drag_chip` overlay was never cleared by this path
        // at all — so an OS-drag end (which bypasses the normal
        // MouseInput::Released handlers in window_event.rs / child_window.rs
        // that DO clear drag_chip) left a stale grey chip rectangle
        // floating in empty pane space until the next render forced a
        // refresh. Iterate every WindowState (main + children) and wipe
        // the lot.
        for id in ids {
            let Some(ws) = self.windows.get_mut(&id) else {
                // Window vanished between snapshot and iteration —
                // nothing to clean. Tolerant by design (#462).
                continue;
            };
            if ws.drag_session.take().is_some() {
                had = true;
            }
            ws.drag_target = None;
            ws.pressed_tab = None;
            ws.mouse_down = false;
            // #447 follow-up to PR #443: clear the renderer's persistent
            // drag-chip overlay AND the headless-test marker via a single
            // helper so production and test paths can never diverge. The
            // per-frame emitter at render/core.rs:3945+ keeps drawing
            // whatever Some(_) value sits in the renderer, so leaving it
            // behind ships a stale chip until something else triggers a
            // set_drag_chip(None). For headless test windows the renderer
            // is None — the `test_drag_chip_marker` mirror is what
            // os_drag_cleanup.rs asserts against.
            ws.clear_drag_chip();
            // Force a repaint so the cleared chip actually leaves the
            // screen instead of waiting for the next external event.
            if let Some(w) = ws.window.as_ref() {
                w.request_redraw();
            }
        }
        self.os_drag_handoff_started = false;
        had
    }

    /// Epic #289 Phase C — pure cross-window transfer API. Operates
    /// on the App's MAIN window only (`source` / `target` are both
    /// `None` ⇒ main↔main reorder). Tests exercise the pure-container
    /// form in `crate::app::tab_transfer` directly; the App wrapper
    /// here delegates to the existing detach/attach pairs so the four
    /// real-window flavors (main↔main, main↔child, child↔main,
    /// child↔child) all funnel through one entry point.
    ///
    /// Returns `Ok(())` when the transfer happened, or a
    /// [`TransferError`] describing the validation failure. The
    /// pre-validation step is intentional: PR #294 review (Haiku) found
    /// that the prior `bool` API silently dropped the detached tab —
    /// killing its child shell via `PtyHandle::Drop` — when the target
    /// window vanished between gesture-start and drop. We now refuse to
    /// touch source state until *both* endpoints have been proven
    /// reachable.
    #[doc(hidden)]
    pub fn transfer_tab(
        &mut self,
        source: Option<WindowId>,
        source_idx: usize,
        target: Option<WindowId>,
        target_idx: usize,
    ) -> Result<(), TransferError> {
        // 0) pre-validate BOTH endpoints before mutating any window.
        //    Data-loss fix (PR #294, Haiku review): detaching and then
        //    failing to attach drops the `PaneState`, which kills the
        //    child shell via `PtyHandle::Drop`.
        match source {
            None => {
                let main = self.main().ok_or(TransferError::SourceMissing)?;
                if source_idx >= main.tab_states.len() || source_idx >= main.tabs.len() {
                    return Err(TransferError::SourceIndexOutOfBounds);
                }
            }
            Some(id) => {
                let src = self.windows.get(&id).ok_or(TransferError::SourceMissing)?;
                if source_idx >= src.tab_states.len() || source_idx >= src.tabs.len() {
                    return Err(TransferError::SourceIndexOutOfBounds);
                }
            }
        }
        if let Some(id) = target {
            if !self.windows.contains_key(&id) {
                return Err(TransferError::TargetMissing);
            }
        }

        // 1) detach from source — guaranteed to succeed after step 0.
        let detached = match source {
            None => self.detach_tab_state(source_idx),
            Some(id) => self.detach_from_child(id, source_idx),
        };
        let Some((tab, state, panes)) = detached else {
            // Shouldn't happen — step 0 validated. Defensive bail.
            return Err(TransferError::SourceIndexOutOfBounds);
        };

        // 2) attach to target — also guaranteed reachable after step 0.
        match target {
            None => self.attach_tab_state(target_idx, tab, state, panes),
            Some(id) => {
                if !self.attach_to_child(id, target_idx, tab, state, panes) {
                    // Defensive: target was present at step 0 but vanished
                    // (e.g. closed on another thread). Re-attach to source
                    // would require holding the moved values, but we've
                    // already passed ownership to attach_to_child which
                    // returned false; treat as TargetMissing.
                    return Err(TransferError::TargetMissing);
                }
            }
        }

        // 3) focus target window + bookkeeping
        match target {
            None => {
                if let Some(w) = self.main_window().cloned() {
                    self.frontmost_window = Some(w.id());
                    w.request_redraw();
                }
            }
            Some(id) => {
                self.frontmost_window = Some(id);
                if let Some(ws) = self.windows.get(&id) {
                    if let Some(w) = ws.window.as_ref() {
                        w.focus_window();
                        w.request_redraw();
                    }
                }
            }
        }

        // 4) source-empty → close source window
        let source_empty = match source {
            None => self.main_tabs().map(|t| t.is_empty()).unwrap_or(true),
            Some(id) => self.windows.get(&id).map(|w| w.tabs.is_empty()).unwrap_or(true),
        };
        if source_empty {
            if let Some(id) = source {
                // child window — route through the unified empty-window
                // cleanup contract so straggler redraw targets get nulled
                // and the "child reaped" trace fires (PR #302 follow-up:
                // bypassing this dropped to a raw `windows.remove` which
                // skipped both bits of bookkeeping).
                self.reap_empty_child(id);
            } else {
                // main window — leave the App to its existing
                // last-tab-closed handling (Phase B already covers
                // hiding the main window when its tabs vec empties).
            }
        }
        Ok(())
    }
}

/// Why a transfer rejected the gesture without losing the tab. Returned
/// by [`App::transfer_tab`]; introduced in PR #294 to fix the
/// Haiku-flagged data-loss bug where a missing-target attach silently
/// dropped the detached `PaneState` (killing its child shell via
/// `PtyHandle::Drop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum TransferError {
    /// `source` was `Some(id)` but the id is not in `App::windows`.
    SourceMissing,
    /// `target` was `Some(id)` but the id is not in `App::windows`.
    TargetMissing,
    /// `source_idx` is beyond the source window's tab vector.
    SourceIndexOutOfBounds,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        self.do_resumed(el);
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        self.do_user_event(el, event);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, win_id: WindowId, event: WindowEvent) {
        self.do_window_event(el, win_id, event);
    }

    fn new_events(&mut self, _el: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.do_new_events(_el, cause);
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.do_about_to_wait(el);
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // Forward to sonicterm-logging so every Cmd+Q / WM_CLOSE /
        // last-window exit lands in sonicterm.log. See
        // `crates/sonicterm-logging/src/exit_trace.rs`.
        sonicterm_logging::record_loop_exiting();
    }
}

#[cfg(test)]
mod click_count_tests {
    use super::next_click_count;

    #[test]
    fn single_double_triple_then_wraps() {
        // Same cell, within interval: 1 → 2 → 3 → back to 1.
        let c1 = next_click_count(0, true, true); // fresh streak
        assert_eq!(c1, 1);
        let c2 = next_click_count(c1, true, true);
        assert_eq!(c2, 2);
        let c3 = next_click_count(c2, true, true);
        assert_eq!(c3, 3);
        let c4 = next_click_count(c3, true, true);
        assert_eq!(c4, 1); // wraps after triple
    }

    #[test]
    fn different_cell_resets_to_one() {
        // A double-click is in progress (prev = 2) but the new press is
        // on a different cell → streak restarts at 1.
        assert_eq!(next_click_count(2, false, true), 1);
        assert_eq!(next_click_count(1, false, true), 1);
    }

    #[test]
    fn timeout_resets_to_one() {
        // Same cell but past the multi-click interval → restart at 1.
        assert_eq!(next_click_count(2, true, false), 1);
        assert_eq!(next_click_count(1, true, false), 1);
    }
}

#[cfg(test)]
mod redraw_coalescing_tests {
    //! Issue #43: the vsync coalescing gate shared by the main and child
    //! `RedrawRequested` arms. These pin the exact deferral policy that
    //! lets a bursty `ls -al` coalesce to one frame per vsync and stops a
    //! torn-out child from busy-spinning the VT thread's parser lock. The
    //! same predicate now backs BOTH windows, so this one spec covers
    //! main/child parity for the gate.

    use super::should_defer_streaming_redraw;
    use std::time::Duration;

    const FRAME: Duration = Duration::from_micros(16_667); // ~60Hz

    #[test]
    fn streaming_redraw_within_frame_defers() {
        // Pure PTY-streaming repaint that already drew this vsync window:
        // defer to the next boundary (the coalescing win).
        assert!(should_defer_streaming_redraw(
            false, // not input-driven
            false, // no fresh burst
            Duration::from_millis(2),
            FRAME,
        ));
    }

    #[test]
    fn input_driven_redraw_never_defers() {
        // Keystroke / resize / theme / IME must render immediately even
        // inside the vsync window — gating them adds perceptible latency.
        assert!(!should_defer_streaming_redraw(true, false, Duration::from_millis(1), FRAME));
    }

    #[test]
    fn fresh_pty_burst_never_defers() {
        // A redraw carrying new PTY bytes always renders so streamed output
        // never stalls behind the frame gate.
        assert!(!should_defer_streaming_redraw(false, true, Duration::from_millis(1), FRAME));
    }

    #[test]
    fn past_frame_boundary_never_defers() {
        // We're past this vsync window — render now, don't defer forever.
        assert!(!should_defer_streaming_redraw(false, false, Duration::from_millis(20), FRAME));
        // Exactly at the boundary also renders (`<` is strict).
        assert!(!should_defer_streaming_redraw(false, false, FRAME, FRAME));
    }

    #[test]
    fn input_and_burst_together_render() {
        // Belt-and-suspenders: any reason-to-render short-circuits deferral.
        assert!(!should_defer_streaming_redraw(true, true, Duration::ZERO, FRAME));
    }
}
