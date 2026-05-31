//! App loop. Owns the window, the GPU renderer, all tab/pane state, the
//! per-pane PTYs and parsers, selection state, and clipboard. Drives keymap
//! dispatch.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use arboard::Clipboard;
use parking_lot::Mutex;
use sonic_core::{
    config::{BackdropKind, Config},
    grid::Grid,
    keymap::{Action, Keymap},
    pty::PtyHandle,
    theme::Theme,
    vt::{CommandEvent, Parser},
};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
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
use sonic_shared::render::GpuRenderer;
use sonic_ui::broadcast::BroadcastState;
use sonic_ui::cheatsheet::CheatsheetState;
use sonic_ui::command_palette::CommandPalette;
use sonic_ui::copy_mode::CopyModeState;
use sonic_ui::ime::ImeState;
use sonic_ui::pane::PaneTree;
use sonic_ui::search::SearchState;
use sonic_ui::selection::Selection;
use sonic_ui::tabs::{CommandStatus, Tab, TabBar};

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
    pub copy_mode: Option<CopyModeState>,
    pub modifiers: ModifiersState,
    pub cursor_visible: Arc<std::sync::atomic::AtomicBool>,
    pub last_render: Instant,
    /// Tab index pressed in the child's bar — same role as
    /// `App::pressed_tab` but for the child window. Used for
    /// drag-from-child merging.
    pub pressed_tab: Option<usize>,
    /// Live drag session for a held-tab gesture in this child window.
    pub drag_session: Option<crate::tab_drag::DragSession>,
    /// Pending cross-window drop target chosen during a drag in the
    /// child's bar; consumed on mouse-up.
    pub drag_target: Option<crate::tab_drag::DropTarget<WindowId>>,
    /// Per-window HiDPI scale factor (PR #244 lesson — cursor routing
    /// MUST use the *window's own* scale, not a global). Promoted onto
    /// `WindowState` by Phase B2 PR-A; the legacy `App.scale_factor`
    /// stays in lock-step until PR-B substitutes readers.
    pub scale_factor: f64,
    /// Per-window IME composition state. Phase B2 PR-A — promoted from
    /// `App.ime` (main-only) so torn-out windows can compose CJK input
    /// independently. The legacy `App.ime` continues to exist and is
    /// kept in sync on the main window until PR-B.
    pub ime: ImeState,
    /// Per-window hovered URL (Cmd-held underline + pointer cursor).
    /// Phase B2 PR-A — promoted from `App.hovered_url`. Legacy field
    /// stays in lock-step on the main window until PR-B.
    pub hovered_url: Option<hovered_url::HoveredUrl>,
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
    // use under crates/sonic-app/tests/. Production code never reaches
    // this function.
    unsafe { std::mem::transmute::<u64, WindowId>(u64::MAX) }
}

/// Phase B2 PR-A — snapshot of the cheap scalar fields mirrored from
/// the legacy `App.*` main-window fields into the shadow `WindowState`
/// entry. Built by [`App::shadow_main_snapshot`] for the legacy side
/// and by [`shadow_main_snapshot_from`] for the shadow side; equality
/// of the two snapshots is the invariant the test pins.
///
/// Heavy fields (`renderer`, `tabs`, `tab_states`, `panes`) are NOT
/// part of this snapshot — they move from `App` into the shadow in
/// PR-B; the shadow holds `None` / empty placeholders for them until
/// then. The snapshot deliberately excludes `cursor_visible` because
/// it's an `Arc<AtomicBool>` shared by pointer (see the dedicated
/// `Arc::ptr_eq` check on [`App::__test_shadow_main_in_sync`]).
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct ShadowMainSnapshot {
    pub cursor_pos: (f64, f64),
    pub mouse_down: bool,
    pub selection: Option<Selection>,
    pub copy_mode: Option<CopyModeState>,
    pub modifiers: ModifiersState,
    pub last_render: Instant,
    pub pressed_tab: Option<usize>,
    pub drag_session: Option<crate::tab_drag::DragSession>,
    pub drag_target: Option<crate::tab_drag::DropTarget<WindowId>>,
    pub scale_factor: f64,
    /// Compared via `Debug`-string equality (the IME state machine
    /// does not implement `PartialEq` upstream).
    pub ime: ImeState,
    pub hovered_url: Option<hovered_url::HoveredUrl>,
}

impl PartialEq for ShadowMainSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.cursor_pos == other.cursor_pos
            && self.mouse_down == other.mouse_down
            && self.selection == other.selection
            && self.copy_mode == other.copy_mode
            && self.modifiers == other.modifiers
            && self.last_render == other.last_render
            && self.pressed_tab == other.pressed_tab
            && format!("{:?}", self.drag_session) == format!("{:?}", other.drag_session)
            && format!("{:?}", self.drag_target) == format!("{:?}", other.drag_target)
            && self.scale_factor == other.scale_factor
            && format!("{:?}", self.ime) == format!("{:?}", other.ime)
            && self.hovered_url == other.hovered_url
    }
}

/// Build a [`ShadowMainSnapshot`] from a shadow `WindowState` entry.
#[doc(hidden)]
pub fn shadow_main_snapshot_from(ws: &WindowState) -> ShadowMainSnapshot {
    ShadowMainSnapshot {
        cursor_pos: ws.cursor_pos,
        mouse_down: ws.mouse_down,
        selection: ws.selection,
        copy_mode: ws.copy_mode.clone(),
        modifiers: ws.modifiers,
        last_render: ws.last_render,
        pressed_tab: ws.pressed_tab,
        drag_session: ws.drag_session,
        drag_target: ws.drag_target,
        scale_factor: ws.scale_factor,
        ime: ws.ime.clone(),
        hovered_url: ws.hovered_url.clone(),
    }
}

/// Apply a snapshot to the shadow `WindowState` entry — the pure half
/// of [`App::sync_shadow_main`]. Exposed for the invariant test so it
/// can drive sync without constructing a real winit window (`cargo
/// test` can't on headless macOS — same as
/// `tests/clear_shape_cache_event.rs`).
#[doc(hidden)]
pub fn apply_shadow_main_snapshot(ws: &mut WindowState, snap: ShadowMainSnapshot) {
    ws.cursor_pos = snap.cursor_pos;
    ws.mouse_down = snap.mouse_down;
    ws.selection = snap.selection;
    ws.copy_mode = snap.copy_mode;
    ws.modifiers = snap.modifiers;
    ws.last_render = snap.last_render;
    ws.pressed_tab = snap.pressed_tab;
    ws.drag_session = snap.drag_session;
    ws.drag_target = snap.drag_target;
    ws.scale_factor = snap.scale_factor;
    ws.ime = snap.ime;
    ws.hovered_url = snap.hovered_url;
}

/// Phase B2 PR-A — pure helper that copies the cheap scalar fields
/// from the legacy `App.*` snapshot into the shadow main `WindowState`
/// entry. Factored out of [`App::sync_shadow_main`] so the invariant
/// test can drive it on a synthetic `WindowState` without needing a
/// real winit window.
///
/// Heavy fields (`renderer`, `tabs`, `tab_states`, `panes`, `window`,
/// `role`) are NOT touched: ownership of those moves wholesale from
/// `App` into the shadow during PR-B; until then the shadow holds
/// `None` / empty placeholders for them and the test does not compare.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn apply_shadow_main_sync(
    ws: &mut WindowState,
    cursor_visible: Arc<std::sync::atomic::AtomicBool>,
    cursor_pos: (f64, f64),
    mouse_down: bool,
    selection: Option<Selection>,
    copy_mode: Option<CopyModeState>,
    modifiers: ModifiersState,
    last_render: Instant,
    pressed_tab: Option<usize>,
    drag_session: Option<crate::tab_drag::DragSession>,
    drag_target: Option<crate::tab_drag::DropTarget<WindowId>>,
    scale_factor: f64,
    ime: ImeState,
    hovered_url: Option<hovered_url::HoveredUrl>,
) {
    ws.cursor_visible = cursor_visible;
    apply_shadow_main_snapshot(
        ws,
        ShadowMainSnapshot {
            cursor_pos,
            mouse_down,
            selection,
            copy_mode,
            modifiers,
            last_render,
            pressed_tab,
            drag_session,
            drag_target,
            scale_factor,
            ime,
            hovered_url,
        },
    );
}

/// Epic #289 Phase A — classification of which terminal window currently
/// owns the OS-frontmost focus. Returned by [`App::frontmost_kind`] and
/// consumed by keymap_dispatch arms + menubar drain to decide where a
/// chord like Cmd+T / Cmd+W / Cmd+\\ should land.
///
/// `Other` covers any non-terminal Sonic window; it explicitly does NOT
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
    /// A non-terminal Sonic window is frontmost. Terminal actions fall
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
    crate::tab_drag::WindowGeom {
        inner_origin: origin,
        inner_size: (size.width, size.height),
        scale_factor: w.scale_factor() as f32,
    }
}

/// Divide a `winit` `CursorMoved` position by the window's HiDPI
/// scale factor to land in LOGICAL pixel coordinates. The whole
/// tab-bar layout (`TabBarLayout`), drag-action thresholds
/// (`TAB_BAR_HEIGHT`, `TEAR_OUT_THRESHOLD_PX`) and the drag-chip
/// overlay are expressed in logical px, so every hit-test path must
/// normalize the raw cursor position through this helper. (PR #76
/// did the same for the cell grid via `pixel_to_cell`; this is the
/// matching fix for the chrome layer the haiku reviewer flagged.)
#[inline]
pub(super) fn to_logical_pos(position_x: f64, position_y: f64, scale_factor: f32) -> (f32, f32) {
    let sf = scale_factor.max(f32::EPSILON);
    ((position_x as f32) / sf, (position_y as f32) / sf)
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
/// `sonic-windows::os_drag_win::shell_quote` so file drops on either
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
    grid: &sonic_core::grid::Grid,
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
/// `sonic_ui::pane::Rect` (window-pixel logical rect produced by
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
/// `pub` + `#[doc(hidden)]` so integration tests can drive it without a
/// live wgpu surface; no `__test_support` shim (CLAUDE.md §5).
#[doc(hidden)]
pub fn resize_panes_to_rects(
    panes: &HashMap<u64, PaneState>,
    rects: &[(u64, sonic_ui::pane::Rect)],
    cell_w: f32,
    cell_h: f32,
) {
    for (id, rect) in rects {
        let Some(pane) = panes.get(id) else { continue };
        let cols = ((rect.w / cell_w).floor() as i32).max(1) as u16;
        let rows = ((rect.h / cell_h).floor() as i32).max(1) as u16;
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
    tabs: &mut sonic_ui::tabs::TabBar,
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
            .and_then(sonic_core::proc_info::foreground_process);
        pane.fg_proc_cache = Some((now, probed));
    }
    let proc_name = pane.fg_proc_cache.as_ref().and_then(|(_, v)| v.clone());
    let pretty = sonic_ui::tab_title::format_tab_title(
        tab_idx,
        cwd.as_deref(),
        proc_name.as_deref(),
        raw_title.as_deref(),
    );
    let cur = tabs.active().map(|t| t.title.clone());
    if cur.as_deref() == Some(pretty.as_str()) {
        return None;
    }
    tabs.set_active_title(pretty.clone());
    Some(pretty)
}

/// Entry point used by the platform bin crates.
pub fn run(theme: Theme, config: Config, keymap: Keymap) -> Result<()> {
    run_with(theme, config, keymap, None, None)
}

/// Loader callback type used by `run_with` to reload a theme by name.
pub type ThemeLoader = Box<dyn Fn(&str) -> Result<Theme> + Send + 'static>;
/// Loader callback type used by `run_with` to reload a keymap by name.
pub type KeymapLoader = Box<dyn Fn(&str) -> Result<Keymap> + Send + 'static>;

/// Entry point that additionally accepts asset loaders so theme +
/// keymap changes can apply live (no restart).
pub fn run_with(
    theme: Theme,
    config: Config,
    keymap: Keymap,
    theme_loader: Option<ThemeLoader>,
    keymap_loader: Option<KeymapLoader>,
) -> Result<()> {
    init_tracing();
    let event_loop =
        EventLoop::<UserEvent>::with_user_event().build().context("create event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let mut app = App::new_with_proxy(theme, config, keymap, Some(proxy));
    app.theme_loader = theme_loader;
    app.keymap_loader = keymap_loader;
    event_loop.run_app(&mut app).context("run event loop")?;
    Ok(())
}

/// Custom user events delivered through [`EventLoopProxy`].
///
/// Currently the only variant is [`UserEvent::ConfigChanged`], sent by
/// the [`ConfigWatcher`] thread whenever a fresh `sonic.toml` parse is
/// available. The handler wakes the loop, drains the watcher channel,
/// and applies the new config (theme/font/keymap). Without this the
/// channel-based delivery would sit queued under `ControlFlow::Wait`
/// until an unrelated event arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserEvent {
    /// A new `sonic.toml` parse is ready on the watcher channel.
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
    /// [`sonic_text::async_fallback::AsyncFallbackLoader`] background
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
/// [`sonic_text::async_fallback::default_load_font_family`] for actual
/// font resolution (zero-byte handle for OS-resident faces, which is
/// what we want — cosmic-text's `FontSystem` does the real install on
/// first use).
///
/// This is the production wire that `Haiku` flagged as missing on
/// PR #318: pre-fix, the loader was wired only inside tests, and
/// real frame-time misses never spawned `request_load` calls. With
/// this helper, every `GpuRenderer::new` site in `sonic-app`
/// constructs the loader from its event-loop proxy and hands it to
/// `GpuRenderer::set_async_loader`. From that point on, a
/// background font load completion bumps `style_rev` on every live
/// window and triggers a redraw — the tofu cells flip to real
/// glyphs without the user having to type anything.
#[must_use]
pub fn build_async_fallback_loader_for_proxy(
    proxy: EventLoopProxy<UserEvent>,
) -> sonic_text::async_fallback::AsyncFallbackLoader {
    use sonic_text::async_fallback::{
        default_load_font_family, AsyncFallbackLoader, LoadFn, NotifyFn,
    };
    let load_fn: LoadFn = Arc::new(default_load_font_family);
    let notify: NotifyFn = Arc::new(move || {
        // Best-effort: the only reason `send_event` fails is the loop
        // already exited, in which case the user has closed the app
        // and there is nothing to redraw.
        let _ = proxy.send_event(UserEvent::ClearShapeCache);
    });
    AsyncFallbackLoader::new(load_fn, notify)
}

/// Same as [`run`] but installs a platform-specific OS-drag sink.
/// `sonic-mac` calls this with a `NSPasteboard`-backed impl; future
/// `sonic-windows` work will pass an `IDataObject`/`DoDragDrop` impl.
/// When the cursor leaves every Sonic window during a tab tear-out,
/// the sink is invoked with a serialized [`crate::os_drag::TabPayload`]
/// instead of spawning a child window.
pub fn run_with_os_drag(
    theme: Theme,
    config: Config,
    keymap: Keymap,
    sink: Arc<dyn crate::os_drag::OsDragSink>,
    theme_loader: Option<ThemeLoader>,
    keymap_loader: Option<KeymapLoader>,
) -> Result<()> {
    run_with_os_drag_and_pending(theme, config, keymap, sink, theme_loader, keymap_loader, None)
}

/// Like [`run_with_os_drag`] but also seeds an already-received
/// [`crate::os_drag::TabPayload`] (e.g. one the platform shim found on
/// the pasteboard at startup). The payload becomes a real tab via
/// [`App::new_tab_from_payload`] before the event loop starts — this
/// is the receiver half of the (review) data-loss fix for PR #59:
/// without it the payload was only logged and the user's torn tab
/// vanished.
pub fn run_with_os_drag_and_pending(
    theme: Theme,
    config: Config,
    keymap: Keymap,
    sink: Arc<dyn crate::os_drag::OsDragSink>,
    theme_loader: Option<ThemeLoader>,
    keymap_loader: Option<KeymapLoader>,
    pending: Option<crate::os_drag::TabPayload>,
) -> Result<()> {
    run_with_os_drag_pending_and_hook(
        theme,
        config,
        keymap,
        sink,
        theme_loader,
        keymap_loader,
        pending,
        None,
    )
}

/// Like [`run_with_os_drag_and_pending`] but also accepts a one-shot
/// `on_resumed` hook invoked at the top of the first
/// `ApplicationHandler::resumed` tick. The macOS bin uses this slot to
/// install the native NSMenu — by then winit has built the AppKit event
/// loop and `setMainMenu` sticks. Installing it before `event_loop.
/// run_app` left AppKit with only the default `Apple, sonic-mac`
/// menubar (bug caught by the PR #114 release-binary smoke).
#[allow(clippy::too_many_arguments)]
pub fn run_with_os_drag_pending_and_hook(
    theme: Theme,
    config: Config,
    keymap: Keymap,
    sink: Arc<dyn crate::os_drag::OsDragSink>,
    theme_loader: Option<ThemeLoader>,
    keymap_loader: Option<KeymapLoader>,
    pending: Option<crate::os_drag::TabPayload>,
    on_resumed: Option<Box<dyn FnOnce() + Send>>,
) -> Result<()> {
    run_with_os_drag_pending_and_window_hook(
        theme,
        config,
        keymap,
        sink,
        theme_loader,
        keymap_loader,
        pending,
        on_resumed,
        None,
        None,
    )
}

/// Like [`run_with_os_drag_pending_and_hook`] but also accepts a
/// one-shot `on_window_ready` hook invoked immediately after
/// `create_window` succeeds, with the raw window handle. The Windows
/// bin uses this slot to install the muda menubar (needs the HWND).
///
/// Phase C2: `os_drag_backend` is the platform OS-level drag-session
/// backend (NSDraggingSession on Mac, OLE DoDragDrop on Windows).
/// Installed onto the constructed App via
/// [`App::set_os_drag_backend`]. Pass `None` on platforms / tests
/// without a backend — the App falls back to the legacy `OsDragSink`
/// path.
#[allow(clippy::too_many_arguments)]
pub fn run_with_os_drag_pending_and_window_hook(
    theme: Theme,
    config: Config,
    keymap: Keymap,
    sink: Arc<dyn crate::os_drag::OsDragSink>,
    theme_loader: Option<ThemeLoader>,
    keymap_loader: Option<KeymapLoader>,
    pending: Option<crate::os_drag::TabPayload>,
    on_resumed: Option<Box<dyn FnOnce() + Send>>,
    on_window_ready: Option<Box<dyn FnOnce(raw_window_handle::RawWindowHandle) + Send>>,
    os_drag_backend: Option<Box<dyn os_drag::OsTabDragBackend>>,
) -> Result<()> {
    init_tracing();
    let event_loop =
        EventLoop::<UserEvent>::with_user_event().build().context("create event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    // Install the same proxy for the macOS native menubar bridge so
    // NSMenu selectors can wake the event loop and dispatch through
    // `run_action`. Safe + cheap on platforms without a menubar.
    crate::menubar_bridge::install_proxy(proxy.clone());
    crate::os_drag_bridge::install_proxy(proxy.clone());
    let mut app = App::new_with_proxy(theme, config, keymap, Some(proxy));
    app.theme_loader = theme_loader;
    app.keymap_loader = keymap_loader;
    app.os_drag_sink = Some(sink);
    if let Some(b) = os_drag_backend {
        app.set_os_drag_backend(b);
    }
    if let Some(hook) = on_resumed {
        app.on_resumed = Some(hook);
    }
    if let Some(hook) = on_window_ready {
        app.on_window_ready = Some(hook);
    }
    if let Some(p) = pending {
        let _ = app.new_tab_from_payload(&p);
    }
    event_loop.run_app(&mut app).context("run event loop")?;
    Ok(())
}

mod child_window;
pub use child_window::{
    child_window_resized_handles_no_renderer,
    child_window_scale_factor_changed_handles_no_renderer, resize_renderer_and_panes_if_present,
    set_scale_factor_if_renderer_present,
};
mod config_apply;
mod event_loop;
pub mod hovered_url;
pub mod invariants;
mod key_encoding;
mod keymap_dispatch;
mod misc;
pub mod os_drag;
mod overlays;
mod search_handle;
mod spawn_pane;
mod tab_state;
pub mod tab_transfer;
mod tear_out;
mod window_event;
pub use config_apply::config_diff_needs_font_apply;
pub use key_encoding::{encode_logical, key_name, key_to_string, key_to_strings, KeyName};

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("sonic=info"));
    let _ = fmt().with_env_filter(filter).try_init();
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
    /// CPU on an idle window (regression caught by
    /// `scripts/bench_headless_gui.sh`). TTL is short enough that
    /// `nvim foo` still flips the tab title quickly.
    pub fg_proc_cache: Option<(std::time::Instant, Option<String>)>,
    /// Cross-thread queue populated by the VT loop when OSC 133 command
    /// lifecycle markers are parsed for this pane.
    pub command_events: Arc<Mutex<Vec<PaneCommandEvent>>>,
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
    pub(super) modifiers: ModifiersState,
    pub(super) last_render: Instant,
    pub(super) cursor_pos: (f64, f64),
    pub(super) mouse_down: bool,
    pub(super) selection: Option<Selection>,
    pub(super) copy_mode: Option<CopyModeState>,
    pub(super) clipboard: Option<Clipboard>,
    pub(super) scale_factor: f64,
    pub(super) hover_link: bool,
    /// Currently-hovered auto-detected URL (focused pane only), with
    /// row + char-col span. Drives the Cmd-held underline overlay and
    /// the pointer-cursor transition. `None` when the cursor isn't on
    /// a URL OR the open-URL modifier isn't held. See
    /// `crate::app::hovered_url` for the pure helpers.
    pub(super) hovered_url: Option<hovered_url::HoveredUrl>,
    pub(super) cursor_visible: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Epic #289 Phase E (Haiku follow-up): Action::NewWindow sets this
    /// flag, then `drain_pending_window_creates` consumes it by calling
    /// `create_new_terminal_window(el)`. Window creation requires an
    /// `ActiveEventLoop` reference
    /// that isn't reachable from the keymap dispatcher. Works from BOTH
    /// the windows-non-empty case (Cmd+N from a focused window) AND the
    /// windows-empty post-close-last-window dock-alive case on macOS.
    pub(super) pending_new_window: bool,
    /// Deferred app-exit request. Set from `run_action` when the user's
    /// Cmd+W chain has just closed the last tab of the last window AND
    /// `Config::quit_on_last_window_close` is true (or non-macOS).
    /// `do_about_to_wait` drains it by calling `el.exit()`. The flag is
    /// needed because `run_action` does not have an `ActiveEventLoop`
    /// handle.
    pub(super) pending_exit: bool,
    /// IME composition state for CJK / other multi-key input methods.
    pub(super) ime: ImeState,
    /// Throttle for `Window::set_ime_cursor_area`. Without this every
    /// render frame posts a message to macOS' InputMethodKit runloop and
    /// stderr fills with `IMKCFRunLoopWakeUpReliable` errors that users
    /// see as "Sonic is hanging". Only fire the winit call when the
    /// terminal cursor moves to a different cell.
    pub(super) ime_cursor_throttle: sonic_ui::ime::ImeCursorThrottle,
    pub(super) command_palette: CommandPalette,
    /// Epic #289 Phase A follow-up — which window the (single, modal)
    /// command palette is currently attached to. `None` means it's
    /// closed OR attached to the main window; `Some(id)` means it's
    /// attached to that child window. The render paths for the main
    /// window and each child window consult this so the palette only
    /// paints on the frontmost window at the moment it was opened,
    /// fixing the bug where Cmd+Shift+P typed in a torn-out child
    /// silently opened the palette on the original main window. See
    /// also `cheatsheet_attached_window`.
    pub(super) palette_attached_window: Option<WindowId>,
    pub(super) cheatsheet_open: bool,
    pub(super) cheatsheet: CheatsheetState,
    /// Sibling to `palette_attached_window` for the keymap cheat sheet
    /// overlay (super+?). Same rationale: the overlay is App-level and
    /// modal, so a tag is enough — we don't need per-window state.
    pub(super) cheatsheet_attached_window: Option<WindowId>,
    /// Tab index recorded on left-mouse-press inside a tab. Used to
    /// detect the tear-out gesture (press → drag below bar → release).
    pub(super) pressed_tab: Option<usize>,
    /// Live drag session for the held-tab gesture in the MAIN window.
    /// Tracks press + current cursor position so the renderer can draw
    /// the translucent drag chip and `compute_action` can pick a
    /// commit-on-release outcome. `None` when no tab is being dragged.
    pub(super) drag_session: Option<crate::tab_drag::DragSession>,
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
    /// Most-recently-focused window's id. `None` means the main window
    /// is focused (or no window has been focused yet). Set/cleared in
    /// the `WindowEvent::Focused` handler on both the main and child
    /// windows so menubar-driven actions (Cmd+T, Cmd+W, …) — which the
    /// OS delivers to the App, not the window — can be routed to the
    /// window the user is actually looking at. Without this routing,
    /// Cmd+T pressed in a torn-out child opened a new tab in the main
    /// window every time. User report v0.6: "拖拽形成新的窗口后，再新
    /// 的窗口按 ctrl+t 还是在原来的窗口打开新tab".
    pub(super) focused_child: Option<WindowId>,
    /// Epic #289 Phase A — most-recently-OS-frontmost window id, INCLUDING
    /// the main window. Where [`Self::focused_child`] historically only
    /// tracked torn-out windows (`None` meaning "main is focused"), the
    /// frontmost field tracks *every* sonic-owned terminal window with a
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
    /// Pending cross-window drag-merge target chosen on the most recent
    /// `CursorMoved` while a tab is held. On mouse-up we use this to
    /// decide between "tear out into new window" (None) and "merge into
    /// destination window at slot" (Some).
    pub(super) drag_target: Option<crate::tab_drag::DropTarget<WindowId>>,
    /// OS-drag tab payloads received before the main [`WindowState`] exists.
    /// Startup pasteboard / OLE deliveries can arrive before `do_resumed`
    /// inserts `main_window_id`; queue them so the destination tab is created
    /// after main is available instead of silently dropping the payload.
    pub(super) pending_os_drag_payloads: Vec<crate::os_drag::TabPayload>,
    /// True when the main window has been drained (its last tab moved
    /// out via cross-window merge) or its close button was clicked
    /// while child windows still owned tabs. In that state the main
    /// window is hidden but the event loop keeps spinning so live
    /// child windows continue to run.
    pub(super) main_hidden: bool,
    /// Optional theme loader, set by `run_with`. Used to reload a theme
    /// by name live.
    pub(super) theme_loader: Option<ThemeLoader>,
    /// Optional keymap loader, set by `run_with`.
    pub(super) keymap_loader: Option<KeymapLoader>,
    /// Live-reload watcher for the user's `sonic.toml`. Spawned in
    /// `resumed`; `None` if the config path could not be resolved or
    /// the watcher failed to start (e.g. parent dir unwritable).
    pub(super) config_watcher: Option<ConfigWatcher>,
    /// Proxy used by the watcher thread to wake the idle event loop
    /// on `sonic.toml` changes. `None` in tests that construct `App`
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
    pub(super) i18n: sonic_ui::i18n::I18n,
    /// Optional platform hook that takes a serialized tab payload and
    /// hands it off to the OS-level drag-and-drop system
    /// (`NSPasteboard` on macOS, OLE `DoDragDrop` on Windows). When
    /// set, [`Self::tear_out_tab`] checks whether the cursor sits
    /// outside every Sonic-owned window; if so, it invokes the sink
    /// and KILLS the local tab instead of spawning a child window.
    /// Installed by the platform bin via [`run_with_os_drag`].
    pub(super) os_drag_sink: Option<Arc<dyn crate::os_drag::OsDragSink>>,
    /// Phase C2 OS-level drag *session* backend. Distinct from
    /// `os_drag_sink` (cross-process wire format): this drives the
    /// NSDraggingSession / OLE DoDragDrop call that captures the
    /// cursor across window boundaries for same-process tab drags.
    /// Installed by the platform bin (`sonic-mac` / `sonic-windows`)
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
    /// `Apple, sonic-mac` menubar.
    pub(super) on_resumed: Option<Box<dyn FnOnce() + Send>>,

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
    pub test_viewport_override: Option<(sonic_ui::pane::Rect, f32, f32)>,
}

impl sonic_ui::broadcast::BroadcastTab for TabState {
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
    pub(crate) fn compute_active_pane_rects(&self) -> Vec<(u64, sonic_ui::pane::Rect)> {
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
        let top = r.top_inset();
        let pl = r.padding_left();
        let pr = r.padding_right();
        let bottom = r.bottom_inset();
        let pb = r.padding_bottom();
        let outer = sonic_ui::pane::Rect::new(
            pl,
            top,
            (w - pl - pr).max(0.0),
            (h - top - bottom - pb).max(0.0),
        );
        st.tree.layout(outer)
    }

    /// Same as [`Self::compute_active_pane_rects`] but for a torn-out
    /// child window (its own renderer + tab_states).
    pub(crate) fn compute_pane_rects_for(child: &WindowState) -> Vec<(u64, sonic_ui::pane::Rect)> {
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get(tab_idx) else { return Vec::new() };
        let Some(r) = child.renderer.as_ref() else { return Vec::new() };
        let (w, h) = r.logical_size();
        let top = r.top_inset();
        let pl = r.padding_left();
        let pr = r.padding_right();
        let bottom = r.bottom_inset();
        let pb = r.padding_bottom();
        let outer = sonic_ui::pane::Rect::new(
            pl,
            top,
            (w - pl - pr).max(0.0),
            (h - top - bottom - pb).max(0.0),
        );
        st.tree.layout(outer)
    }

    #[doc(hidden)]
    pub fn new(theme: Theme, config: Config, keymap: Keymap) -> Self {
        Self::new_with_proxy(theme, config, keymap, None)
    }

    #[doc(hidden)]
    pub fn new_with_proxy(
        mut theme: Theme,
        config: Config,
        keymap: Keymap,
        event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
    ) -> Self {
        theme.apply_accessibility(&config.accessibility);
        let i18n = sonic_ui::i18n::I18n::new(if config.locale.is_empty() {
            None
        } else {
            Some(config.locale.as_str())
        });
        Self {
            theme,
            config,
            keymap,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            selection: None,
            copy_mode: None,
            clipboard: Clipboard::new().ok(),
            scale_factor: 1.0,
            hover_link: false,
            hovered_url: None,
            cursor_visible: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            pending_new_window: false,
            pending_exit: false,
            ime: ImeState::new(),
            ime_cursor_throttle: sonic_ui::ime::ImeCursorThrottle::new(),
            command_palette: CommandPalette::new(),
            palette_attached_window: None,
            cheatsheet_open: false,
            cheatsheet: CheatsheetState::new(),
            cheatsheet_attached_window: None,
            pressed_tab: None,
            drag_session: None,
            os_drag_handoff_started: false,
            windows: HashMap::new(),
            main_window_id: None,
            focused_child: None,
            frontmost_window: None,
            drag_target: None,
            pending_os_drag_payloads: Vec::new(),
            main_hidden: false,
            theme_loader: None,
            keymap_loader: None,
            config_watcher: None,
            event_loop_proxy,
            // Default to 60 Hz until `resumed` probes the actual
            // monitor refresh rate. ~16.667 ms = 1/60 s.
            frame_period: Duration::from_micros(16_667),
            pending_redraw: false,
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

static TEST_COMMAND_NOTIFICATIONS: std::sync::Mutex<Option<Vec<String>>> =
    std::sync::Mutex::new(None);

#[doc(hidden)]
pub fn __test_capture_command_notifications() {
    *TEST_COMMAND_NOTIFICATIONS.lock().expect("test notification lock poisoned") = Some(Vec::new());
}

#[doc(hidden)]
pub fn __test_drain_command_notifications() -> Vec<String> {
    TEST_COMMAND_NOTIFICATIONS
        .lock()
        .expect("test notification lock poisoned")
        .take()
        .unwrap_or_default()
}

fn record_test_command_notification(body: &str) -> bool {
    let mut notifications =
        TEST_COMMAND_NOTIFICATIONS.lock().expect("test notification lock poisoned");
    let Some(notifications) = notifications.as_mut() else { return false };
    notifications.push(body.to_string());
    true
}

#[cfg(target_os = "windows")]
fn notify_command_done(body: String) {
    if record_test_command_notification(&body) {
        return;
    }
    if let Err(err) = notify_rust::Notification::new().summary("Command done").body(&body).show() {
        tracing::debug!(?err, "desktop notification failed");
    }
}

#[cfg(not(target_os = "windows"))]
fn notify_command_done(body: String) {
    record_test_command_notification(&body);
}

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

    /// Test-only accessor: returns `true` if a `RedrawRequested` arriving
    /// right now would be coalesced (deferred to the next vsync boundary)
    /// or `false` if it would render immediately. Mirrors the exact
    /// predicate used in the `WindowEvent::RedrawRequested` arm.
    #[doc(hidden)]
    pub fn would_coalesce_redraw(&self) -> bool {
        !self.input_dirty
            && self.pty_burst_gen.load(Ordering::Acquire) == self.last_seen_burst_gen
            && self.last_render.elapsed() < self.frame_period
    }

    /// Test-only snapshot of the PTY-burst generation counter.
    #[doc(hidden)]
    pub fn pty_burst_gen_for_test(&self) -> u32 {
        self.pty_burst_gen.load(Ordering::Acquire)
    }

    /// Test-only accessor for the last PTY-burst generation that render
    /// marked as seen.
    #[doc(hidden)]
    pub fn last_seen_burst_gen_for_test(&self) -> u32 {
        self.last_seen_burst_gen
    }

    /// Test-only marker for a PTY burst. Mirrors what the VT thread does
    /// when it processes a non-empty byte chunk.
    #[doc(hidden)]
    pub fn mark_pty_burst_for_test(&self) {
        let prev = self.pty_burst_gen.fetch_add(1, Ordering::Release);
        crate::app::invariants::debug_assert_burst_gen_monotonic(prev, prev.wrapping_add(1));
    }

    /// Test-only marker for render completing after sampling a PTY-burst
    /// generation at the start of `RedrawRequested`.
    #[doc(hidden)]
    pub fn mark_burst_gen_seen_for_test(&mut self, snapshot: u32) {
        self.last_seen_burst_gen = snapshot;
    }

    /// Test-only setter for the input-dirty flag.
    #[doc(hidden)]
    pub fn mark_input_dirty_for_test(&mut self) {
        self.input_dirty = true;
    }

    /// Test-only setter for `last_render` so tests can simulate "we
    /// just rendered" without driving an actual frame.
    #[doc(hidden)]
    pub fn set_last_render_for_test(&mut self, t: Instant) {
        self.last_render = t;
    }

    /// Test-only accessor: returns the current `pending_redraw` flag.
    /// Used by the Issue #175 regression test to verify that a
    /// lock-contention bail-out during `RedrawRequested` correctly
    /// schedules a follow-up vsync-paced redraw rather than dropping
    /// the request silently.
    #[doc(hidden)]
    pub fn pending_redraw_for_test(&self) -> bool {
        self.pending_redraw
    }

    /// Test-only setter for `pending_redraw`.
    #[doc(hidden)]
    pub fn set_pending_redraw_for_test(&mut self, v: bool) {
        self.pending_redraw = v;
    }

    /// Test-only accessor for the `input_dirty` flag.
    #[doc(hidden)]
    pub fn input_dirty_for_test(&self) -> bool {
        self.input_dirty
    }

    /// Test-only setter that clears the `input_dirty` flag. Lets a
    /// regression test (e.g. issue #167) establish a clean baseline
    /// before driving an action that is expected to set it.
    #[doc(hidden)]
    pub fn clear_input_dirty_for_test(&mut self) {
        self.input_dirty = false;
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

    /// Install a one-shot callback fired at the top of the first
    /// `ApplicationHandler::resumed` tick. macOS uses this to install
    /// the native NSMenu after winit has built the AppKit event loop —
    /// installing earlier leaves AppKit with only the default
    /// `Apple, sonic-mac` menu bar.
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

    /// Translate a UI message id. See [`sonic_ui::i18n::I18n::t`]. Returns
    /// the key itself if no bundle (active or English fallback) has it,
    /// so the UI never renders an empty label.
    pub fn t(&self, key: &str) -> String {
        self.i18n.t(key)
    }

    /// Translate with `{ $name }` arguments. See
    /// [`sonic_ui::i18n::I18n::t_args`].
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
        self.i18n =
            sonic_ui::i18n::I18n::new(if requested.is_empty() { None } else { Some(requested) });
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
            !self.main_hidden && !self.main_tabs().map(|t| t.is_empty()).unwrap_or(true);
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

    /// Test-only: read the `main_hidden` latch.
    #[doc(hidden)]
    pub fn __test_main_hidden(&self) -> bool {
        self.main_hidden
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

    /// Test-only: force-set the `main_hidden` latch so post-merge
    /// drain-policy tests can simulate the "main already retired" state
    /// without driving a real winit close event.
    #[doc(hidden)]
    pub fn __test_set_main_hidden(&mut self, v: bool) {
        self.main_hidden = v;
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
        (None, encode_logical(key, mods))
    }

    fn write_to_pane(&self, pane_id: u64, bytes: Vec<u8>) {
        if let Some(p) = self.main().and_then(|ws| ws.panes.get(&pane_id)) {
            if let Some(pty) = p.pty.as_ref() {
                let _ = pty.in_tx.send(bytes);
            }
        }
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
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            cursor_visible: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            last_render: Instant::now(),
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            scale_factor: 1.0,
            ime: ImeState::new(),
            hovered_url: None,
        };
        self.windows.insert(id, child);
        id
    }

    /// Test-only: install a `focused_child` id without going through a
    /// real `WindowEvent::Focused(true)` (which requires a winit window).
    /// Used by `tearout_newtab_routing.rs` to confirm `Action::NewTab`
    /// falls back to the main App when the recorded child no longer
    /// exists (and clears the stale `focused_child`).
    #[doc(hidden)]
    pub fn __test_set_focused_child(&mut self, id: Option<WindowId>) {
        self.__test_synthetic_main();
        self.focused_child = id;
    }

    /// Test-only: read back the current `focused_child`.
    #[doc(hidden)]
    pub fn __test_focused_child(&self) -> Option<WindowId> {
        self.focused_child
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

    /// Test-only sibling of `__test_palette_attached_window` for the
    /// keymap cheat sheet overlay.
    #[doc(hidden)]
    pub fn __test_cheatsheet_attached_window(&self) -> Option<WindowId> {
        self.cheatsheet_attached_window
    }

    /// Test-only: whether the command palette is currently open.
    #[doc(hidden)]
    pub fn __test_palette_open(&self) -> bool {
        self.command_palette.is_open()
    }

    /// Test-only: whether the keymap cheat sheet is currently open.
    #[doc(hidden)]
    pub fn __test_cheatsheet_open(&self) -> bool {
        self.cheatsheet_open
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

    /// Phase B2 PR-A — copy every cheaply-cloneable scalar from the
    /// legacy `App` main-window fields into the shadow `WindowState`
    /// entry in [`Self::windows`]. Called at the end of every event
    /// tick (window event, user event, redraw) so the shadow stays
    /// indistinguishable from the legacy authoritative copy. PR-B will
    /// invert this — readers move to the shadow, the legacy fields go
    /// away, and this helper is deleted.
    ///
    /// Fields NOT mirrored in PR-A (move in PR-B): `renderer` (owns a
    /// GpuRenderer, can't be cloned), `tabs` / `tab_states` / `panes`
    /// (own PtyHandle / Parser; ownership moves wholesale in PR-B).
    /// The shadow keeps `renderer: None` and empty collections until
    /// then; the invariant test does NOT compare those fields.
    #[doc(hidden)]
    pub fn sync_shadow_main(&mut self) {
        let Some(id) = self.main_window_id else { return };
        let cursor_visible = self.cursor_visible.clone();
        let cursor_pos = self.cursor_pos;
        let mouse_down = self.mouse_down;
        let selection = self.selection;
        let copy_mode = self.copy_mode.clone();
        let modifiers = self.modifiers;
        let last_render = self.last_render;
        let pressed_tab = self.pressed_tab;
        let drag_session = self.drag_session;
        let drag_target = self.drag_target;
        let scale_factor = self.scale_factor;
        let ime = self.ime.clone();
        let hovered_url = self.hovered_url.clone();
        if let Some(ws) = self.windows.get_mut(&id) {
            apply_shadow_main_sync(
                ws,
                cursor_visible,
                cursor_pos,
                mouse_down,
                selection,
                copy_mode,
                modifiers,
                last_render,
                pressed_tab,
                drag_session,
                drag_target,
                scale_factor,
                ime,
                hovered_url,
            );
        }
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
        dir: sonic_core::keymap::Direction,
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
    /// See `crates/sonic-app/tests/per_pane_resize.rs`.
    #[doc(hidden)]
    pub fn __test_invoke_close_active_pane(&mut self) {
        self.close_active_pane();
    }

    /// Test-only invoker for [`Self::focus_pane_dir_in_child`].
    #[doc(hidden)]
    pub fn __test_invoke_focus_pane_dir_in_child(
        &mut self,
        id: WindowId,
        dir: sonic_core::keymap::Direction,
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
        dir: sonic_core::keymap::Direction,
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
        self.drag_target = target;
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

    /// Phase B2 PR-A — snapshot of the cheap scalar fields the shadow
    /// `WindowState` must mirror. Built from the legacy `App.*` main
    /// fields; the shadow side is built via
    /// [`shadow_main_snapshot_from`]. Equality of the two is the
    /// invariant the test pins.
    #[doc(hidden)]
    pub fn shadow_main_snapshot(&self) -> ShadowMainSnapshot {
        ShadowMainSnapshot {
            cursor_pos: self.cursor_pos,
            mouse_down: self.mouse_down,
            selection: self.selection,
            copy_mode: self.copy_mode.clone(),
            modifiers: self.modifiers,
            last_render: self.last_render,
            pressed_tab: self.pressed_tab,
            drag_session: self.drag_session,
            drag_target: self.drag_target,
            scale_factor: self.scale_factor,
            ime: self.ime.clone(),
            hovered_url: self.hovered_url.clone(),
        }
    }

    /// Phase B2 PR-A invariant probe: returns `true` iff every
    /// cheaply-mirrored field on the shadow `windows[main]` entry equals
    /// the legacy `App.*` field. Heavy fields (`renderer`, `tabs`,
    /// `tab_states`, `panes`) are intentionally NOT compared — they
    /// move from `App` into the shadow in PR-B; until then the shadow
    /// holds `None` / empty placeholders.
    #[doc(hidden)]
    pub fn __test_shadow_main_in_sync(&self) -> bool {
        let Some(id) = self.main_window_id else { return false };
        let Some(ws) = self.windows.get(&id) else { return false };
        ws.role == WindowRole::Terminal
            && Arc::ptr_eq(&ws.cursor_visible, &self.cursor_visible)
            && ws.cursor_pos == self.cursor_pos
            && ws.mouse_down == self.mouse_down
            && ws.selection == self.selection
            && ws.copy_mode == self.copy_mode
            && ws.modifiers == self.modifiers
            && ws.last_render == self.last_render
            && ws.pressed_tab == self.pressed_tab
            && ws.scale_factor == self.scale_factor
            && ws.hovered_url == self.hovered_url
    }

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
            cursor_pos: self.cursor_pos,
            mouse_down: self.mouse_down,
            selection: self.selection,
            copy_mode: self.copy_mode.clone(),
            modifiers: self.modifiers,
            cursor_visible: self.cursor_visible.clone(),
            last_render: self.last_render,
            pressed_tab: self.pressed_tab,
            drag_session: self.drag_session,
            drag_target: self.drag_target,
            scale_factor: self.scale_factor,
            ime: self.ime.clone(),
            hovered_url: self.hovered_url.clone(),
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
        if let Some((r, g, b)) = self.theme.colors.foreground.rgb() {
            parser.set_theme_fg(r, g, b);
        }
        if let Some((r, g, b)) = self.theme.colors.background.rgb() {
            parser.set_theme_bg(r, g, b);
        }
        if let Some((r, g, b)) = self.theme.colors.cursor.rgb() {
            parser.set_theme_cursor(r, g, b);
        }
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
        self.split_active(sonic_core::keymap::Direction::Right);
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
    /// `sonic-mac` calls this with an NSDraggingSession impl,
    /// `sonic-windows` with an OLE DoDragDrop impl. Tests use it via
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
    /// Returns `None` when no event-loop proxy has been wired (test
    /// harnesses that construct `App` via plain `new` without a
    /// proxy). In that case the OS drag is not startable, which the
    /// caller treats as "fall back to the existing within-process
    /// tear_out path".
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
    /// Platform glue (e.g. `sonic-windows::os_drag_win`) calls this to
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
        use sonic_ui::tabbar_view::TabBarLayout;
        let Some(w) = self.main_window() else { return };
        let Some(r) = self.main_renderer() else { return };
        let inner_origin = w.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let inner_size = {
            let s = w.inner_size();
            (s.width, s.height)
        };
        let sf = w.scale_factor() as f32;
        let logical_w = inner_size.0 as f32 / sf;
        let empty_tabs_pub = sonic_ui::tabs::TabBar::new();
        let layout = TabBarLayout::compute_with_height(
            self.main_tabs().unwrap_or(&empty_tabs_pub),
            logical_w,
            r.tab_bar_logical_height(),
        )
        .with_top_offset(r.tab_bar_y_offset())
        .with_visible(r.tab_bar_visible());
        let snap = os_drag::TabBarSnapshot::from_layout(
            Some(w.id()),
            inner_origin,
            inner_size,
            sf,
            &layout,
        );
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
        use sonic_ui::tabbar_view::TabBarLayout;
        let Some(child) = self.windows.get(&id) else { return };
        let Some(win) = child.window.as_ref() else { return };
        let inner_origin = win.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
        let inner_size = {
            let s = win.inner_size();
            (s.width, s.height)
        };
        let sf = win.scale_factor() as f32;
        let logical_w = inner_size.0 as f32 / sf;
        let Some(r) = child.renderer.as_ref() else { return };
        let layout =
            TabBarLayout::compute_with_height(&child.tabs, logical_w, r.tab_bar_logical_height())
                .with_top_offset(r.tab_bar_y_offset())
                .with_visible(r.tab_bar_visible());
        let snap =
            os_drag::TabBarSnapshot::from_layout(Some(id), inner_origin, inner_size, sf, &layout);
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
                    "os_drag_session: DroppedOnEmpty — existing path handles new-window spawn"
                );
                // The existing tear_out path is driven by within-process
                // state machines; Phase C2 leaves window-spawn semantics
                // unchanged. Clear residue so the next gesture starts
                // fresh.
                self.cancel_drag_session();
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
        self.pressed_tab
    }

    #[doc(hidden)]
    pub fn __test_mouse_down(&self) -> bool {
        self.mouse_down
    }

    #[doc(hidden)]
    pub fn __test_set_pressed_tab(&mut self, v: Option<usize>) {
        self.pressed_tab = v;
    }

    #[doc(hidden)]
    pub fn __test_set_mouse_down(&mut self, v: bool) {
        self.mouse_down = v;
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

    /// Test-only accessor: current live font size.
    #[doc(hidden)]
    pub fn font_size_for_test(&self) -> f32 {
        self.config.font.size
    }

    /// Test-only accessor: current live theme name.
    #[doc(hidden)]
    pub fn theme_name_for_test(&self) -> &str {
        &self.theme.name
    }

    /// Test-only accessor: live theme.
    #[doc(hidden)]
    pub fn theme_for_test(&self) -> &sonic_core::theme::Theme {
        &self.theme
    }

    /// Test-only accessor: snapshot of the live `Config`.
    #[doc(hidden)]
    pub fn config_for_test(&self) -> &sonic_core::config::Config {
        &self.config
    }

    /// Test-only: install a [`ThemeLoader`].
    #[doc(hidden)]
    pub fn set_theme_loader_for_test(&mut self, loader: ThemeLoader) {
        self.theme_loader = Some(loader);
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
        let app_had = self.drag_session.take().is_some();
        let mut win_had = false;
        for (id, ws) in self.windows.iter_mut() {
            // Phase B2 PR-B1b: skip shadow main entry by id (renderer
            // is now Some on main — `is_none()` no longer identifies it).
            if Some(*id) == self.main_window_id {
                continue;
            }
            if ws.drag_session.take().is_some() {
                win_had = true;
            }
        }
        // pressed_tab / mouse_down / drag_target are the gesture
        // residue from `tear_out`; clearing them prevents an ESC mid-
        // drag from leaving the next mouse-up still believing a drag
        // is in flight (Haiku-flagged regression class).
        self.pressed_tab = None;
        self.mouse_down = false;
        self.drag_target = None;
        self.os_drag_handoff_started = false;
        app_had || win_had
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
        self.sync_shadow_main();
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        self.do_user_event(el, event);
        self.sync_shadow_main();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, win_id: WindowId, event: WindowEvent) {
        self.do_window_event(el, win_id, event);
        self.sync_shadow_main();
    }

    fn new_events(&mut self, _el: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.do_new_events(_el, cause);
        self.sync_shadow_main();
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.do_about_to_wait(el);
        self.sync_shadow_main();
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // Forward to sonic-logging so every Cmd+Q / WM_CLOSE /
        // last-window exit lands in sonic.log. See
        // `crates/sonic-logging/src/exit_trace.rs`.
        sonic_logging::record_loop_exiting();
    }
}
