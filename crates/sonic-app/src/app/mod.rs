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
    config::Config,
    grid::Grid,
    keymap::Keymap,
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

/// Apply WezTerm-style integrated titlebar on macOS: the titlebar is
/// transparent and the content view extends underneath it, so there is
/// no visible separator line above our tab bar. Traffic lights remain
/// native (decorations stay on) — only the chrome strip is fused into
/// the content. No-op on non-macOS platforms.
///
/// We keep this in one place so all three window-creation sites
/// (main, tear-out, preferences) stay in sync.
#[doc(hidden)]
pub fn with_integrated_titlebar(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::WindowAttributesExtMacOS;
        attrs.with_fullsize_content_view(true).with_titlebar_transparent(true)
    }
    // FUTURE: equivalent integration on Windows would require custom
    // non-client area painting (WM_NCCALCSIZE); winit 0.30 does not
    // expose this directly. Left as a no-op for now.
    #[cfg(not(target_os = "macos"))]
    {
        attrs
    }
}

/// Reserved height (in **logical pixels**) under the macOS native titlebar
/// when [`with_integrated_titlebar`] is active. The content view extends
/// under the titlebar (fullsize_content_view), so we must shift our tab
/// bar and grid down by this amount or they paint underneath the traffic
/// lights and window title.
///
/// 28pt matches AppKit's standard titlebar height (the value
/// `NSWindow.titlebarHeight` returns for a window with the standard
/// `NSWindowStyleMask::Titled`). Querying it from objc2 at runtime would
/// give us the live value but adds a heavyweight dependency to
/// `sonic-shared` for a number that is stable across every macOS release
/// since 10.10. If Apple ever changes the default, bump this constant.
pub const MACOS_INTEGRATED_TITLEBAR_INSET: f32 = 28.0;

/// Returns the titlebar inset (logical px) the renderer should reserve
/// above the tab bar / grid for the current platform + window-style
/// combination. Returns 0 on platforms that don't extend content under
/// the titlebar (Windows, Linux), so non-macOS layout is unchanged.
pub fn integrated_titlebar_inset() -> f32 {
    #[cfg(target_os = "macos")]
    {
        MACOS_INTEGRATED_TITLEBAR_INSET
    }
    #[cfg(not(target_os = "macos"))]
    {
        0.0
    }
}

/// Windows-only integrated titlebar height (logical pixels) reserved for
/// our custom caption strip (drag region + min/max/close buttons drawn by
/// us when the HWND subclass zeros out the OS NC area).
///
/// Returns 0 on non-Windows platforms so the caption-button paint path
/// in [`crate::quad`] is a no-op on macOS / Linux — the macOS integrated
/// titlebar uses [`integrated_titlebar_inset`] (the AppKit traffic-light
/// inset) instead.
pub const WINDOWS_INTEGRATED_TITLEBAR_INSET: u32 = 32;

/// Integer-pixel inset for the Windows custom titlebar strip. Used by
/// `sonic-windows::chrome` (WM_NCHITTEST) and `sonic-shared::quad`
/// (caption-button paint). Returns 0 elsewhere so call sites stay
/// portable without per-platform branches.
#[must_use]
pub fn integrated_titlebar_inset_px() -> u32 {
    #[cfg(target_os = "windows")]
    {
        WINDOWS_INTEGRATED_TITLEBAR_INSET
    }
    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

use crate::config_watch::ConfigWatcher;
use sonic_shared::render::GpuRenderer;
use sonic_ui::cheatsheet::CheatsheetState;
use sonic_ui::command_palette::CommandPalette;
use sonic_ui::ime::ImeState;
use sonic_ui::pane::PaneTree;
use sonic_ui::prefs::PrefsState;
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
pub struct ChildWindow {
    pub window: Arc<Window>,
    pub renderer: GpuRenderer,
    pub tabs: TabBar,
    pub tab_states: Vec<TabState>,
    pub panes: HashMap<u64, PaneState>,
    pub cursor_pos: (f64, f64),
    pub mouse_down: bool,
    pub selection: Option<Selection>,
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
}

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

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

/// Entry point used by the platform bin crates.
pub fn run(theme: Theme, config: Config, keymap: Keymap) -> Result<()> {
    run_with(theme, config, keymap, None, None)
}

/// Loader callback type used by `run_with` to reload a theme by name
/// when the user picks a new one in the preferences window.
pub type ThemeLoader = Box<dyn Fn(&str) -> Result<Theme> + Send + 'static>;
/// Loader callback type used by `run_with` to reload a keymap by name.
pub type KeymapLoader = Box<dyn Fn(&str) -> Result<Keymap> + Send + 'static>;

/// Entry point that additionally accepts asset loaders so the prefs
/// window can apply theme + keymap changes live (no restart).
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
    )
}

/// Like [`run_with_os_drag_pending_and_hook`] but also accepts a
/// one-shot `on_window_ready` hook invoked immediately after
/// `create_window` succeeds, with the raw window handle. The Windows
/// bin uses this slot to install the muda menubar (needs the HWND).
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
mod config_apply;
mod event_loop;
pub mod invariants;
mod key_encoding;
mod keymap_dispatch;
mod misc;
mod overlays;
mod prefs_window;
mod search_handle;
mod spawn_pane;
mod tab_state;
mod tear_out;
mod window_event;
pub use config_apply::config_diff_needs_font_apply;
pub use key_encoding::{encode_logical, key_name, KeyName};

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
    fn new(tree: PaneTree, active_pane: u64) -> Self {
        Self { tree, active_pane, search: None, command: CommandStatus::Idle }
    }
}

#[doc(hidden)]
pub struct App {
    pub(super) theme: Theme,
    pub(super) config: Config,
    pub(super) keymap: Keymap,
    pub(super) window: Option<Arc<Window>>,
    pub(super) renderer: Option<GpuRenderer>,
    pub(super) tabs: TabBar,
    /// Parallel to `tabs.tabs()` — same length, same order.
    pub(super) tab_states: Vec<TabState>,
    pub(super) panes: HashMap<u64, PaneState>,
    pub(super) modifiers: ModifiersState,
    pub(super) last_render: Instant,
    pub(super) cursor_pos: (f64, f64),
    pub(super) mouse_down: bool,
    pub(super) selection: Option<Selection>,
    pub(super) clipboard: Option<Clipboard>,
    pub(super) scale_factor: f64,
    pub(super) hover_link: bool,
    pub(super) cursor_visible: std::sync::Arc<std::sync::atomic::AtomicBool>,
    // v0.6: optional graphical preferences window.
    pub(super) prefs_window: Option<Arc<Window>>,
    pub(super) prefs_state: Option<PrefsState>,
    pub(super) prefs_renderer: Option<sonic_shared::prefs_renderer::PrefsRenderer>,
    pub(super) pending_prefs_open: bool,
    /// IME composition state for CJK / other multi-key input methods.
    pub(super) ime: ImeState,
    /// Throttle for `Window::set_ime_cursor_area`. Without this every
    /// render frame posts a message to macOS' InputMethodKit runloop and
    /// stderr fills with `IMKCFRunLoopWakeUpReliable` errors that users
    /// see as "Sonic is hanging". Only fire the winit call when the
    /// terminal cursor moves to a different cell.
    pub(super) ime_cursor_throttle: sonic_ui::ime::ImeCursorThrottle,
    pub(super) command_palette: CommandPalette,
    pub(super) cheatsheet_open: bool,
    pub(super) cheatsheet: CheatsheetState,
    /// Tab index recorded on left-mouse-press inside a tab. Used to
    /// detect the tear-out gesture (press → drag below bar → release).
    pub(super) pressed_tab: Option<usize>,
    /// Live drag session for the held-tab gesture in the MAIN window.
    /// Tracks press + current cursor position so the renderer can draw
    /// the translucent drag chip and `compute_action` can pick a
    /// commit-on-release outcome. `None` when no tab is being dragged.
    pub(super) drag_session: Option<crate::tab_drag::DragSession>,
    /// Windows spawned by tearing tabs out of the parent bar. Keyed by
    /// winit WindowId so events route back to the right child.
    pub(super) child_windows: HashMap<WindowId, ChildWindow>,
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
    /// Pending cross-window drag-merge target chosen on the most recent
    /// `CursorMoved` while a tab is held. On mouse-up we use this to
    /// decide between "tear out into new window" (None) and "merge into
    /// destination window at slot" (Some).
    pub(super) drag_target: Option<crate::tab_drag::DropTarget<WindowId>>,
    /// True when the main window has been drained (its last tab moved
    /// out via cross-window merge) or its close button was clicked
    /// while child windows still owned tabs. In that state the main
    /// window is hidden but the event loop keeps spinning so live
    /// child windows continue to run.
    pub(super) main_hidden: bool,
    /// Optional theme loader, set by `run_with`. Used by the prefs
    /// window's apply/close path to reload a theme by name live.
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
    /// View → Toggle Tab Bar state. When `false`, the menubar Toggle
    /// Tab Bar action has hidden the tab bar chrome. Defaults to
    /// `true`. Exposed via [`Self::tab_bar_visible`] so the renderer
    /// + hit-test code can read it on each frame.
    pub(super) tab_bar_visible: bool,
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
}

impl App {
    /// Compute window-pixel rects for every pane in the active tab,
    /// using the main window renderer's logical size + insets + padding.
    /// Returns an empty Vec if there is no renderer yet (pre-Resumed) or
    /// no active tab. Mirrors the inline computation in
    /// `window_event.rs` (~line 110); factored so resize/config-reload
    /// call sites stay one-liners.
    pub(crate) fn compute_active_pane_rects(&self) -> Vec<(u64, sonic_ui::pane::Rect)> {
        let tab_idx = self.tabs.active_index();
        let Some(st) = self.tab_states.get(tab_idx) else { return Vec::new() };
        let Some(r) = self.renderer.as_ref() else { return Vec::new() };
        let (w, h) = r.logical_size();
        let top = r.top_inset();
        let pl = r.padding_left();
        let pr = r.padding_right();
        let pb = r.padding_bottom();
        let outer =
            sonic_ui::pane::Rect::new(pl, top, (w - pl - pr).max(0.0), (h - top - pb).max(0.0));
        st.tree.layout(outer)
    }

    /// Same as [`Self::compute_active_pane_rects`] but for a torn-out
    /// child window (its own renderer + tab_states).
    pub(crate) fn compute_pane_rects_for(child: &ChildWindow) -> Vec<(u64, sonic_ui::pane::Rect)> {
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get(tab_idx) else { return Vec::new() };
        let r = &child.renderer;
        let (w, h) = r.logical_size();
        let top = r.top_inset();
        let pl = r.padding_left();
        let pr = r.padding_right();
        let pb = r.padding_bottom();
        let outer =
            sonic_ui::pane::Rect::new(pl, top, (w - pl - pr).max(0.0), (h - top - pb).max(0.0));
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
            window: None,
            renderer: None,
            tabs: TabBar::new(),
            tab_states: Vec::new(),
            panes: HashMap::new(),
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            selection: None,
            clipboard: Clipboard::new().ok(),
            scale_factor: 1.0,
            hover_link: false,
            cursor_visible: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            prefs_window: None,
            prefs_state: None,
            prefs_renderer: None,
            pending_prefs_open: false,
            ime: ImeState::new(),
            ime_cursor_throttle: sonic_ui::ime::ImeCursorThrottle::new(),
            command_palette: CommandPalette::new(),
            cheatsheet_open: false,
            cheatsheet: CheatsheetState::new(),
            pressed_tab: None,
            drag_session: None,
            child_windows: HashMap::new(),
            focused_child: None,
            drag_target: None,
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
            tab_bar_visible: true,
            on_resumed: None,
            on_window_ready: None,
        }
    }

    pub(super) fn poll_command_events_for_tab(&mut self, tab_idx: usize) {
        let Some(tab_state) = self.tab_states.get_mut(tab_idx) else { return };
        let pane_ids = tab_state.tree.leaves();
        let mut latest = None;
        for pane_id in pane_ids {
            if let Some(pane) = self.panes.get(&pane_id) {
                let mut q = pane.command_events.lock();
                latest = q.drain(..).next_back().or(latest);
            }
        }
        let Some(ev) = latest else { return };
        match ev.event {
            CommandEvent::CmdStart => tab_state.command = CommandStatus::Running(ev.at),
            CommandEvent::CmdEnd(exit) => {
                tab_state.command =
                    CommandStatus::Done { exit, until: ev.at + Duration::from_secs(3) };
                maybe_notify_long_command(&self.config, ev.duration, exit);
            }
            CommandEvent::PromptStart => {}
        }
        if let Some(t) = self.tab_states.get(tab_idx).map(|st| st.command.clone()) {
            self.tabs.set_command_status(tab_idx, t);
        }
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

    /// Test-only setter for `prefs_state`. Mirrors what
    /// [`Self::create_prefs_window`] does at runtime when the user
    /// presses Cmd/Ctrl+, — minus the wgpu surface — so a regression
    /// test can drive [`Self::commit_prefs_and_apply_live`] without
    /// instantiating a real preferences window.
    #[doc(hidden)]
    pub fn install_prefs_state_for_test(&mut self, state: sonic_ui::prefs::PrefsState) {
        self.prefs_state = Some(state);
    }

    /// Test-only entry point for the exact slot the prefs UI invokes
    /// when the user clicks Apply (or hits Esc while dirty). Drives
    /// the real `commit_prefs_and_apply_live` path so a regression
    /// test (issue #167) can assert that font / theme / keymap edits
    /// flow through `apply_new_config` and reach the renderer — not
    /// just into `self.config`.
    #[doc(hidden)]
    pub fn commit_prefs_for_test(&mut self) {
        self.commit_prefs_and_apply_live();
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

    /// Live-apply a new locale. Persists the choice to `self.config.locale`
    /// so a subsequent prefs save writes it to disk. Pass `""` to mean
    /// "auto-detect from OS locale".
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
        let main_alive = !self.main_hidden && !self.tabs.is_empty();
        !main_alive && self.child_windows.is_empty()
    }

    /// Test-only: pure policy fn mirroring `should_exit` so integration
    /// tests can exercise the rule without constructing a real
    /// `ChildWindow` (which requires a live winit Window + GpuRenderer).
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

    /// Test-only: force-set the `main_hidden` latch so post-merge
    /// drain-policy tests can simulate the "main already retired" state
    /// without driving a real winit close event.
    #[doc(hidden)]
    pub fn __test_set_main_hidden(&mut self, v: bool) {
        self.main_hidden = v;
    }

    fn active_pane_id(&self) -> Option<u64> {
        let i = self.tabs.active_index();
        self.tab_states.get(i).map(|t| t.active_pane)
    }

    fn active_pane(&self) -> Option<&PaneState> {
        self.active_pane_id().and_then(|id| self.panes.get(&id))
    }

    fn write_to_pty(&self, bytes: Vec<u8>) {
        if let Some(p) = self.active_pane() {
            if let Some(pty) = p.pty.as_ref() {
                let _ = pty.in_tx.send(bytes);
            }
        }
    }

    /// Test-only: how many tabs the named child window currently owns.
    #[doc(hidden)]
    pub fn __test_child_tab_count(&self, id: WindowId) -> Option<usize> {
        self.child_windows.get(&id).map(|c| c.tabs.len())
    }

    /// Test-only: install a `focused_child` id without going through a
    /// real `WindowEvent::Focused(true)` (which requires a winit window).
    /// Used by `tearout_newtab_routing.rs` to confirm `Action::NewTab`
    /// falls back to the main App when the recorded child no longer
    /// exists (and clears the stale `focused_child`).
    #[doc(hidden)]
    pub fn __test_set_focused_child(&mut self, id: Option<WindowId>) {
        self.focused_child = id;
    }

    /// Test-only: read back the current `focused_child`.
    #[doc(hidden)]
    pub fn __test_focused_child(&self) -> Option<WindowId> {
        self.focused_child
    }

    /// Test-only: count of tabs in the main App.
    #[doc(hidden)]
    pub fn __test_main_tab_count(&self) -> usize {
        self.tabs.len()
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
        self.child_windows.len()
    }

    /// Test-only: seed a synthetic tab with one pane that has no PTY
    /// attached (just a Parser owning a fresh Grid). Lets integration
    /// tests exercise tab/pane bookkeeping without spawning shells.
    #[doc(hidden)]
    pub fn __test_seed_tab(&mut self, title: &str) -> u64 {
        let pane_id = next_pane_id();
        let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
        self.panes.insert(pane_id, PaneState::new(parser, None));
        self.tabs.push(Tab::new(title));
        self.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        pane_id
    }

    /// Test-only: read-only access to the internal panes map so tests
    /// can assert "this pane id is gone after detach".
    #[doc(hidden)]
    pub fn __test_pane_ids(&self) -> Vec<u64> {
        self.panes.keys().copied().collect()
    }

    /// Test-only: id of the active pane in a given tab. Returns `None`
    /// when `tab_idx` is out of range. Used by `split_focus.rs` to
    /// assert that splitting a pane plus the click-to-focus path
    /// actually flips the focused leaf.
    #[doc(hidden)]
    pub fn __test_active_pane_in_tab(&self, tab_idx: usize) -> Option<u64> {
        self.tab_states.get(tab_idx).map(|st| st.active_pane)
    }

    /// Test-only: set the active pane in `tab_idx` to `pane_id`. The
    /// click-to-focus logic in `window_event.rs` is the production
    /// caller; tests exercise the same state transition without
    /// driving a synthetic winit `MouseInput` event.
    #[doc(hidden)]
    pub fn __test_set_active_pane(&mut self, tab_idx: usize, pane_id: u64) -> bool {
        if let Some(st) = self.tab_states.get_mut(tab_idx) {
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
        self.tabs.len()
    }

    /// Test-only: install an `OsDragSink` so [`Self::try_os_drag_handoff`]
    /// can be exercised without going through the platform entry point.
    #[doc(hidden)]
    pub fn __test_set_os_drag_sink(&mut self, sink: Arc<dyn crate::os_drag::OsDragSink>) {
        self.os_drag_sink = Some(sink);
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
        self.panes.get(&id).map(|p| p.redraw_target.clone())
    }

    /// Drain the config-watcher channel and apply any incoming Config.
    /// Idempotent and cheap when nothing changed (early-returns when no
    /// new config is queued).
    #[doc(hidden)]
    pub fn poll_config_reload(&mut self) {
        let Some(latest) = self.config_watcher.as_ref().and_then(ConfigWatcher::try_latest) else {
            return;
        };
        self.apply_new_config(latest);
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
}

/// Test-only helper: simulate the command-palette path dispatching the
/// `OpenPreferences` action, and return whether the App's
/// `pending_prefs_open` flag was set as a result.
///
/// This is the cheapest possible regression for the palette → preferences
/// wiring (PR #41 review). The real prefs window can only be created from
/// a live winit `ActiveEventLoop`, but the flag-set is the necessary
/// pre-condition that the palette path was incorrectly skipping.
#[doc(hidden)]
pub fn __test_palette_dispatch_open_preferences_sets_pending() -> bool {
    use sonic_core::keymap::{Keymap, Meta};
    use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
    let hex = || Hex("#000000".to_string());
    let ansi = || AnsiColors {
        black: hex(),
        red: hex(),
        green: hex(),
        yellow: hex(),
        blue: hex(),
        magenta: hex(),
        cyan: hex(),
        white: hex(),
    };
    let theme = Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex(),
            foreground: hex(),
            cursor: hex(),
            cursor_text: hex(),
            selection_bg: hex(),
            selection_fg: hex(),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: hex(),
                active_bg: hex(),
                active_fg: hex(),
                inactive_bg: hex(),
                inactive_fg: hex(),
                hover_bg: hex(),
                hover_fg: hex(),
                close_button_fg: hex(),
            },
        },
    };
    let config = Config::default();
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() };
    let mut app = App::new(theme, config, keymap);
    // Simulate what the palette Enter branch does: pick the selected
    // action and dispatch via run_action — exactly the sequence run by
    // `command_palette_handle_key` on Enter.
    app.command_palette.open();
    // Filter so OpenPreferences becomes the current item; it's the only
    // action whose name contains "openpre".
    app.command_palette.set_query("openpre");
    let action =
        app.command_palette.current().cloned().expect("OpenPreferences should be filtered in");
    assert!(matches!(action, sonic_core::keymap::Action::OpenPreferences));
    app.command_palette.close();
    app.run_action(&action);
    app.pending_prefs_open
}

/// Test-only helper: dispatch the menubar `OpenPreferences` action the
/// same way the macOS NSMenu bridge does (push onto the bridge queue,
/// then call the actions drain), and report whether the resulting state
/// is "ready for `drain_pending_window_creates` to materialize the
/// prefs window" — i.e. `pending_prefs_open == true`.
///
/// This is the regression for the bug where ⌘, (and the macOS
/// menubar > Preferences item, which routes through the same bridge)
/// logged "awaiting resumed-event-loop hook" but the prefs window
/// never appeared. The inline consumer for `pending_prefs_open` lived
/// only on the KeyboardInput path; menubar dispatch never hit it. The
/// fix centralizes the consumer in `drain_pending_window_creates` and
/// calls it from both `user_event` (menubar/UserEvent) and the
/// KeyboardInput arm of `window_event`.
///
/// We can't construct an `ActiveEventLoop` in a unit test, so this
/// helper stops one step short of `create_window` and asserts the
/// pre-condition the bug violated. The full GUI verify recipe lives
/// in the doc comment on `drain_pending_window_creates`.
#[doc(hidden)]
pub fn __test_menubar_dispatch_open_preferences_sets_pending() -> bool {
    use sonic_core::keymap::{Action, Keymap, Meta};
    use sonic_core::theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme};
    let hex = || Hex("#000000".to_string());
    let ansi = || AnsiColors {
        black: hex(),
        red: hex(),
        green: hex(),
        yellow: hex(),
        blue: hex(),
        magenta: hex(),
        cyan: hex(),
        white: hex(),
    };
    let theme = Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex(),
            foreground: hex(),
            cursor: hex(),
            cursor_text: hex(),
            selection_bg: hex(),
            selection_fg: hex(),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: hex(),
                active_bg: hex(),
                active_fg: hex(),
                inactive_bg: hex(),
                inactive_fg: hex(),
                hover_bg: hex(),
                hover_fg: hex(),
                close_button_fg: hex(),
            },
        },
    };
    let config = Config::default();
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() };
    let mut app = App::new(theme, config, keymap);
    // Mirror what NSMenu does: enqueue Action::OpenPreferences and let
    // the App run every queued action. This is the action-loop portion
    // of `drain_menubar_actions` — we stop just before the
    // `drain_pending_window_creates(el)` step (which needs a real
    // `ActiveEventLoop`) and verify the flag landed.
    let _ = crate::menubar_bridge::push_action(Action::OpenPreferences);
    for action in crate::menubar_bridge::__test_drain() {
        app.run_action(&action);
    }
    app.pending_prefs_open
}
