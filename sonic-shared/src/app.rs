//! App loop. Owns the window, the GPU renderer, all tab/pane state, the
//! per-pane PTYs and parsers, selection state, and clipboard. Drives keymap
//! dispatch.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
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
    keymap::{Action, Direction, Keymap, ScrollAction},
    pty::PtyHandle,
    theme::Theme,
    vt::{Parser, VtEvent},
};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowId},
};

use crate::{
    command_palette::CommandPalette,
    config_watch::ConfigWatcher,
    ime::ImeState,
    pane::PaneTree,
    prefs::{PrefsHit, PrefsState},
    render::GpuRenderer,
    search::SearchState,
    selection::Selection,
    tabbar_view::{detect_tear_out, TabBarLayout, TabHit},
    tabs::{Tab, TabBar},
};

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
fn window_geom(w: &Window) -> crate::tab_drag::WindowGeom {
    let origin = w.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
    let size = w.inner_size();
    crate::tab_drag::WindowGeom { inner_origin: origin, inner_size: (size.width, size.height) }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserEvent {
    /// A new `sonic.toml` parse is ready on the watcher channel.
    ConfigChanged,
}

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
}

impl PaneState {
    #[doc(hidden)]
    pub fn new(parser: Arc<Mutex<Parser>>, pty: Option<PtyHandle>) -> Self {
        Self { parser, pty, redraw_target: Arc::new(Mutex::new(None)), viewport_top_abs: None }
    }
}

/// Per-tab state. The `TabBar` keeps title/order; this struct tracks the
/// pane tree and the focused leaf inside the tab.
pub struct TabState {
    pub tree: PaneTree,
    pub active_pane: u64,
    pub search: Option<SearchState>,
}

#[doc(hidden)]
pub struct App {
    theme: Theme,
    config: Config,
    keymap: Keymap,
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    tabs: TabBar,
    /// Parallel to `tabs.tabs()` — same length, same order.
    tab_states: Vec<TabState>,
    panes: HashMap<u64, PaneState>,
    modifiers: ModifiersState,
    last_render: Instant,
    cursor_pos: (f64, f64),
    mouse_down: bool,
    selection: Option<Selection>,
    clipboard: Option<Clipboard>,
    scale_factor: f64,
    hover_link: bool,
    cursor_visible: std::sync::Arc<std::sync::atomic::AtomicBool>,
    // v0.6: optional graphical preferences window.
    prefs_window: Option<Arc<Window>>,
    prefs_state: Option<PrefsState>,
    pending_prefs_open: bool,
    /// IME composition state for CJK / other multi-key input methods.
    ime: ImeState,
    command_palette: CommandPalette,
    /// Tab index recorded on left-mouse-press inside a tab. Used to
    /// detect the tear-out gesture (press → drag below bar → release).
    pressed_tab: Option<usize>,
    /// Windows spawned by tearing tabs out of the parent bar. Keyed by
    /// winit WindowId so events route back to the right child.
    child_windows: HashMap<WindowId, ChildWindow>,
    /// Pending cross-window drag-merge target chosen on the most recent
    /// `CursorMoved` while a tab is held. On mouse-up we use this to
    /// decide between "tear out into new window" (None) and "merge into
    /// destination window at slot" (Some).
    drag_target: Option<crate::tab_drag::DropTarget<WindowId>>,
    /// True when the main window has been drained (its last tab moved
    /// out via cross-window merge) or its close button was clicked
    /// while child windows still owned tabs. In that state the main
    /// window is hidden but the event loop keeps spinning so live
    /// child windows continue to run.
    main_hidden: bool,
    /// Optional theme loader, set by `run_with`. Used by the prefs
    /// window's apply/close path to reload a theme by name live.
    theme_loader: Option<ThemeLoader>,
    /// Optional keymap loader, set by `run_with`.
    keymap_loader: Option<KeymapLoader>,
    /// Live-reload watcher for the user's `sonic.toml`. Spawned in
    /// `resumed`; `None` if the config path could not be resolved or
    /// the watcher failed to start (e.g. parent dir unwritable).
    config_watcher: Option<ConfigWatcher>,
    /// Proxy used by the watcher thread to wake the idle event loop
    /// on `sonic.toml` changes. `None` in tests that construct `App`
    /// directly via [`App::new`] without a real event loop.
    event_loop_proxy: Option<EventLoopProxy<UserEvent>>,
    /// Translation bundle. Rebuilt when the user picks a new locale in
    /// the preferences "Language" dropdown.
    i18n: crate::i18n::I18n,
}

impl App {
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
        let i18n = crate::i18n::I18n::new(if config.locale.is_empty() {
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
            pending_prefs_open: false,
            ime: ImeState::new(),
            command_palette: CommandPalette::new(),
            pressed_tab: None,
            child_windows: HashMap::new(),
            drag_target: None,
            main_hidden: false,
            theme_loader: None,
            keymap_loader: None,
            config_watcher: None,
            event_loop_proxy,
            i18n,
        }
    }

    /// Translate a UI message id. See [`crate::i18n::I18n::t`]. Returns
    /// the key itself if no bundle (active or English fallback) has it,
    /// so the UI never renders an empty label.
    pub fn t(&self, key: &str) -> String {
        self.i18n.t(key)
    }

    /// Translate with `{ $name }` arguments. See
    /// [`crate::i18n::I18n::t_args`].
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
            crate::i18n::I18n::new(if requested.is_empty() { None } else { Some(requested) });
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

    /// Spawn a fresh PTY + parser pair sized to the current renderer.
    fn spawn_pane(&self) -> PaneState {
        let (cols, rows) = self.renderer.as_ref().map(|r| r.cells()).unwrap_or((80, 24));
        let parser = Arc::new(Mutex::new(Parser::new(Grid::new(cols, rows))));
        // Pre-create the redraw target Arc bound to the current parent
        // window. If the pane later tears out, `tear_out_tab` swaps the
        // inner Option to the child window's Arc<Window> so the VT
        // thread re-targets without restarting.
        let redraw_target: Arc<Mutex<Option<Arc<Window>>>> =
            Arc::new(Mutex::new(self.window.clone()));
        let pty = match PtyHandle::spawn_default_shell(cols, rows) {
            Ok(pty) => {
                let parser_clone = parser.clone();
                let out_rx = pty.out_rx.clone();
                let redraw_target_thread = redraw_target.clone();
                let cursor_visible = self.cursor_visible.clone();
                std::thread::Builder::new()
                    .name("sonic-vt-loop".into())
                    .spawn(move || {
                        // Coalesce redraw requests so a burst of pty output
                        // (oh-my-zsh banners, `cat largefile`) doesn't pin
                        // the main thread at 100% CPU re-rendering for every
                        // byte. Drain at least min_interval between bursts,
                        // but ALWAYS schedule a trailing redraw when the
                        // channel briefly quiesces so the final batch lands
                        // on screen (this is the "Enter needs 2 presses" bug
                        // — without the trailing flush, the redraw request
                        // after the prompt redraw was dropped silently).
                        let mut last_request = Instant::now() - Duration::from_secs(1);
                        let mut pending = false;
                        // 4ms is small enough to stay below one frame even
                        // when a key triggers an echo immediately. Keeps the
                        // CPU-spin guard for bursty output (cat largefile,
                        // shell startup banner) while making typing feel
                        // instant.
                        let min_interval = Duration::from_millis(4);
                        loop {
                            // Try to drain quickly; if nothing comes for
                            // ~min_interval and we have a pending redraw,
                            // flush it before going back to blocking recv.
                            match out_rx.recv_timeout(if pending {
                                min_interval
                            } else {
                                Duration::from_secs(3600)
                            }) {
                                Ok(bytes) => {
                                    let mut p = parser_clone.lock();
                                    for ev in p.advance(&bytes) {
                                        match ev {
                                            VtEvent::SetTitle(t) => {
                                                if let Some(w) =
                                                    redraw_target_thread.lock().as_ref()
                                                {
                                                    w.set_title(&format!("Sonic — {t}"));
                                                }
                                            }
                                            VtEvent::CursorVisibility(v) => {
                                                cursor_visible
                                                    .store(v, std::sync::atomic::Ordering::Relaxed);
                                            }
                                            _ => {}
                                        }
                                    }
                                    drop(p);
                                    if last_request.elapsed() >= min_interval {
                                        if let Some(w) = redraw_target_thread.lock().as_ref() {
                                            w.request_redraw();
                                        }
                                        last_request = Instant::now();
                                        pending = false;
                                    } else {
                                        pending = true;
                                    }
                                }
                                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                                    // Quiescent: flush trailing redraw.
                                    if pending {
                                        if let Some(w) = redraw_target_thread.lock().as_ref() {
                                            w.request_redraw();
                                        }
                                        last_request = Instant::now();
                                        pending = false;
                                    }
                                }
                                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                    })
                    .expect("spawn vt loop");
                Some(pty)
            }
            Err(e) => {
                tracing::error!("failed to spawn pty: {e}");
                None
            }
        };
        let mut state = PaneState::new(parser, pty);
        state.redraw_target = redraw_target;
        state
    }

    fn new_tab(&mut self, title: impl Into<String>) {
        let pane_id = next_pane_id();
        let pane = self.spawn_pane();
        self.panes.insert(pane_id, pane);
        self.tabs.push(Tab::new(title));
        self.tab_states.push(TabState {
            tree: PaneTree::leaf(pane_id),
            active_pane: pane_id,
            search: None,
        });
    }

    fn close_tab_at(&mut self, index: usize) {
        if index >= self.tab_states.len() {
            return;
        }
        let st = self.tab_states.remove(index);
        for id in st.tree.leaves() {
            self.panes.remove(&id);
        }
        if let Some(id) = self.tabs.tabs().get(index).map(|t| t.id) {
            self.tabs.close(id);
        }
    }

    /// Pure state transfer: pop tab at `index` out of this App's tab bar
    /// + tab_states + panes map and return the detached pieces. The
    /// returned `(Tab, TabState, HashMap<u64, PaneState>)` tuple has
    /// every pane that belonged to the tab — PTY handles and parsers
    /// transfer intact, so the underlying shell processes keep running
    /// in the new owner. Caller is responsible for placing them into a
    /// `ChildWindow` (or dropping them, which kills the shells via
    /// `PtyHandle::Drop`).
    ///
    /// Returns `None` if `index` is out of range.
    #[doc(hidden)]
    pub fn detach_tab_state(
        &mut self,
        index: usize,
    ) -> Option<(Tab, TabState, HashMap<u64, PaneState>)> {
        if index >= self.tab_states.len() || index >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.tabs().get(index).cloned()?;
        let state = self.tab_states.remove(index);
        let mut panes: HashMap<u64, PaneState> = HashMap::new();
        for id in state.tree.leaves() {
            if let Some(p) = self.panes.remove(&id) {
                panes.insert(id, p);
            }
        }
        self.tabs.close(tab.id);
        Some((tab, state, panes))
    }

    /// Inverse of `detach_tab_state`: insert a previously-detached tab +
    /// pane bundle into this App's tab bar at `index` and adopt the
    /// panes. The bundle's PTY threads keep running — we just swap each
    /// pane's `redraw_target` so output now triggers redraws on THIS
    /// window's surface. Used by the cross-window drag-merge flow when
    /// a tab from a child window is dropped onto the main bar.
    ///
    /// Caller is responsible for matching pty/grid size to the
    /// destination renderer (we resize panes to current cells here).
    #[doc(hidden)]
    pub fn attach_tab_state(
        &mut self,
        index: usize,
        tab: Tab,
        state: TabState,
        panes: HashMap<u64, PaneState>,
    ) {
        let (cols, rows) = self.renderer.as_ref().map(|r| r.cells()).unwrap_or((80, 24));
        for (id, pane) in panes {
            pane.parser.lock().grid_mut().resize(cols, rows);
            if let Some(pty) = pane.pty.as_ref() {
                (pty.resize)(cols, rows);
            }
            *pane.redraw_target.lock() = self.window.clone();
            self.panes.insert(id, pane);
        }
        let idx = index.min(self.tabs.len());
        self.tabs.insert(idx, tab);
        self.tab_states.insert(idx, state);
    }

    /// Detach a tab + pane bundle from the child window `src_id`.
    /// Mirror of `App::detach_tab_state` but for child windows.
    /// Returns `None` if the child window or index is unknown.
    #[doc(hidden)]
    pub fn detach_from_child(
        &mut self,
        src_id: WindowId,
        index: usize,
    ) -> Option<(Tab, TabState, HashMap<u64, PaneState>)> {
        let child = self.child_windows.get_mut(&src_id)?;
        if index >= child.tabs.len() || index >= child.tab_states.len() {
            return None;
        }
        let tab = child.tabs.tabs().get(index).cloned()?;
        let state = child.tab_states.remove(index);
        let mut panes: HashMap<u64, PaneState> = HashMap::new();
        for id in state.tree.leaves() {
            if let Some(p) = child.panes.remove(&id) {
                panes.insert(id, p);
            }
        }
        child.tabs.close(tab.id);
        Some((tab, state, panes))
    }

    /// Insert a tab + pane bundle into the child window `dst_id` at
    /// `index`. Panes are resized to the child's renderer and have
    /// their `redraw_target` swapped to the child's window. Returns
    /// false if the child doesn't exist (in which case caller must
    /// decide whether to drop the bundle — losing those shells).
    #[doc(hidden)]
    pub fn attach_to_child(
        &mut self,
        dst_id: WindowId,
        index: usize,
        tab: Tab,
        state: TabState,
        panes: HashMap<u64, PaneState>,
    ) -> bool {
        let Some(child) = self.child_windows.get_mut(&dst_id) else { return false };
        let (cols, rows) = child.renderer.cells();
        for (id, pane) in panes {
            pane.parser.lock().grid_mut().resize(cols, rows);
            if let Some(pty) = pane.pty.as_ref() {
                (pty.resize)(cols, rows);
            }
            *pane.redraw_target.lock() = Some(child.window.clone());
            child.panes.insert(id, pane);
        }
        let idx = index.min(child.tabs.len());
        child.tabs.insert(idx, tab);
        child.tab_states.insert(idx, state);
        child.window.request_redraw();
        true
    }

    /// Close and reap a child window whose bar has become empty. The
    /// VT threads for the panes that were already moved out have had
    /// their redraw target swapped; this just drops the renderer +
    /// window + (now-empty) bookkeeping maps.
    fn reap_empty_child(&mut self, win_id: WindowId) {
        if let Some(child) = self.child_windows.get(&win_id) {
            if child.tabs.is_empty() {
                if let Some(removed) = self.child_windows.remove(&win_id) {
                    // panes map should already be empty; defensively
                    // null out any stragglers' redraw targets.
                    for pane in removed.panes.values() {
                        *pane.redraw_target.lock() = None;
                    }
                    drop(removed);
                    tracing::info!(
                        "child window reaped after drag-merge; remaining children={}",
                        self.child_windows.len()
                    );
                }
            }
        }
    }

    /// Test-only: how many tabs the named child window currently owns.
    #[doc(hidden)]
    pub fn __test_child_tab_count(&self, id: WindowId) -> Option<usize> {
        self.child_windows.get(&id).map(|c| c.tabs.len())
    }

    /// Look at the global cursor position derived from the main
    /// window's local coordinates and pick a drop target on one of the
    /// CHILD windows' tab bars (the main window itself is the source
    /// — dragging back to it is a future reorder concern). Returns
    /// `None` if the cursor is not over any child bar.
    fn compute_main_drag_target(
        &self,
        local_in_main: (f64, f64),
    ) -> Option<crate::tab_drag::DropTarget<WindowId>> {
        let main_window = self.window.as_ref()?;
        let main_origin =
            main_window.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(main_origin, local_in_main);
        let candidates = self.child_windows.iter().map(|(id, c)| {
            let geom = window_geom(&c.window);
            let layout = TabBarLayout::compute(&c.tabs, c.renderer.width() as f32);
            (*id, geom, layout)
        });
        crate::tab_drag::find_drop_target(global, candidates)
    }

    /// Same as [`Self::compute_main_drag_target`] but for a drag that
    /// originated in the child window `src_id`. Considers the main
    /// window AND the other child windows as candidates.
    fn compute_child_drag_target(
        &self,
        src_id: WindowId,
        local_in_src: (f64, f64),
    ) -> Option<crate::tab_drag::DropTarget<WindowId>> {
        let src_child = self.child_windows.get(&src_id)?;
        let src_origin =
            src_child.window.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(src_origin, local_in_src);
        let mut candidates: Vec<(WindowId, crate::tab_drag::WindowGeom, TabBarLayout)> = Vec::new();
        if let Some(main) = self.window.as_ref() {
            let geom = window_geom(main);
            let width = self.renderer.as_ref().map(|r| r.width()).unwrap_or(0) as f32;
            candidates.push((main.id(), geom, TabBarLayout::compute(&self.tabs, width)));
        }
        for (id, c) in &self.child_windows {
            if *id == src_id {
                continue;
            }
            let geom = window_geom(&c.window);
            let layout = TabBarLayout::compute(&c.tabs, c.renderer.width() as f32);
            candidates.push((*id, geom, layout));
        }
        crate::tab_drag::find_drop_target(global, candidates)
    }

    /// Move the main window's tab at `src_idx` into the destination
    /// described by `target`. The destination is always a child window
    /// (the main is the source in this path). Source-window emptiness
    /// is impossible since the main window's bar can't be drained
    /// through detach_tab_state alone — `__test_seed_tab` / `new_tab`
    /// guarantee at least one remains, and the existing tear-out
    /// pathway also refuses to detach when `len() <= 1`.
    fn merge_main_into_child(
        &mut self,
        src_idx: usize,
        target: crate::tab_drag::DropTarget<WindowId>,
    ) {
        let Some((tab, state, panes)) = self.detach_tab_state(src_idx) else { return };
        if !self.attach_to_child(target.window, target.slot, tab, state, panes) {
            tracing::warn!(
                "drag-merge: destination child {:?} disappeared mid-drop; panes dropped",
                target.window
            );
        }
        // If main has been drained but child windows are still alive,
        // hide the main window without exiting the app.
        if self.tabs.is_empty() && !self.child_windows.is_empty() {
            self.hide_main_window();
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Hide the main window and latch `main_hidden = true`. Used when
    /// the main window has been drained of its last tab via a
    /// cross-window merge, or when the user clicks the close button
    /// while child windows are still alive. Both paths keep the event
    /// loop running so the surviving children continue to function.
    fn hide_main_window(&mut self) {
        if let Some(w) = &self.window {
            w.set_visible(false);
        }
        self.main_hidden = true;
        tracing::info!("main window hidden (drained); child_windows={}", self.child_windows.len());
    }

    /// Reveal the main window again, e.g. when a tab is merged back
    /// into it from a child. Clears the `main_hidden` latch.
    fn show_main_window(&mut self) {
        if let Some(w) = &self.window {
            w.set_visible(true);
        }
        self.main_hidden = false;
    }

    /// Move the source child window's tab at `src_idx` into `target`.
    /// `target.window` can be the main window OR another child. If the
    /// source child empties out as a result, it is reaped.
    fn merge_child_into_target(
        &mut self,
        src_id: WindowId,
        src_idx: usize,
        target: crate::tab_drag::DropTarget<WindowId>,
    ) {
        let Some((tab, state, panes)) = self.detach_from_child(src_id, src_idx) else { return };
        let main_id = self.window.as_ref().map(|w| w.id());
        let attached = if Some(target.window) == main_id {
            self.attach_tab_state(target.slot, tab, state, panes);
            // Receiving a tab back into main un-hides the window if it
            // had been drained.
            if self.main_hidden {
                self.show_main_window();
            }
            true
        } else {
            self.attach_to_child(target.window, target.slot, tab, state, panes)
        };
        if !attached {
            tracing::warn!(
                "drag-merge: destination {:?} disappeared mid-drop; panes dropped",
                target.window
            );
        }
        self.reap_empty_child(src_id);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Tear the tab at `index` out of this App and spawn it as a new
    /// native window. Creates a winit Window AND a `GpuRenderer`
    /// bound to that window's surface, transfers the dragged tab's
    /// panes, and swaps each pane's redraw target so the VT thread
    /// notifies the child window from now on. If the index is
    /// invalid OR the remaining bar would be empty (so tearing out
    /// is a no-op), this is silently skipped.
    fn tear_out_tab(&mut self, el: &ActiveEventLoop, index: usize) {
        // Don't tear the only tab — that's a no-op (the new window
        // would be identical to the old one, minus its renderer).
        if self.tabs.len() <= 1 {
            return;
        }
        let Some((tab, state, panes)) = self.detach_tab_state(index) else { return };

        let attrs = Window::default_attributes()
            .with_title(format!("Sonic — {}", tab.title))
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 500.0));
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!("tear-out: create_window failed: {e}; pane state dropped");
                // panes drop here, which kills the child shells via
                // PtyHandle::Drop — acceptable for an OS-level failure.
                return;
            }
        };
        window.set_ime_allowed(true);

        // Build the renderer for the new surface. If GPU init fails
        // we drop the panes (kills shells) and bail — the child
        // window would otherwise be invisible/unusable.
        let renderer = match GpuRenderer::new(
            window.clone(),
            el,
            &self.theme,
            &self.config.font.family,
            self.config.font.size,
            self.config.font.line_height,
            self.config.window.padding,
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("tear-out: renderer init failed: {e}; pane state dropped");
                return;
            }
        };

        let (cols, rows) = renderer.cells();
        // Resize the migrated panes to the child window's grid and
        // swap each pane's VT-thread redraw target so further pty
        // output triggers the CHILD window's redraw, not the parent.
        for pane in panes.values() {
            pane.parser.lock().grid_mut().resize(cols, rows);
            if let Some(pty) = pane.pty.as_ref() {
                (pty.resize)(cols, rows);
            }
            *pane.redraw_target.lock() = Some(window.clone());
        }

        let win_id = window.id();
        let mut child_tabs = TabBar::new();
        let active_pane = state.active_pane;
        child_tabs.push(tab);
        let child = ChildWindow {
            window: window.clone(),
            renderer,
            tabs: child_tabs,
            tab_states: vec![TabState { tree: state.tree, active_pane, search: state.search }],
            panes,
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            selection: None,
            modifiers: ModifiersState::empty(),
            cursor_visible: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            last_render: Instant::now(),
            pressed_tab: None,
            drag_target: None,
        };
        self.child_windows.insert(win_id, child);
        window.request_redraw();
        tracing::info!("tab torn out as new window; child_windows={}", self.child_windows.len());
    }

    /// Handle a winit event addressed to a torn-out child window.
    /// Mirrors the main window event dispatcher but operates against
    /// the per-child renderer, tabs, and pane state. The child is
    /// single-tab/single-pane in v2 — splits/new-tabs are deferred.
    fn handle_child_window_event(
        &mut self,
        el: &ActiveEventLoop,
        win_id: WindowId,
        event: WindowEvent,
    ) {
        let theme = self.theme.clone();
        let Some(child) = self.child_windows.get_mut(&win_id) else { return };
        match event {
            WindowEvent::CloseRequested => {
                // Clear redraw targets so the VT thread stops trying
                // to redraw a dropped window (it will then notice the
                // pty channel close on Drop and exit). Dropping the
                // ChildWindow drops PaneState → PtyHandle → kills the
                // child shells.
                if let Some(removed) = self.child_windows.remove(&win_id) {
                    for pane in removed.panes.values() {
                        *pane.redraw_target.lock() = None;
                    }
                    drop(removed);
                }
                // If this was the last child AND the main window had
                // been previously drained/hidden, nothing is alive
                // anymore — exit the loop.
                if self.should_exit() {
                    el.exit();
                }
            }
            WindowEvent::RedrawRequested => {
                let tab_idx = child.tabs.active_index();
                let pane_rects: Vec<(u64, crate::pane::Rect)> = child
                    .tab_states
                    .get(tab_idx)
                    .map(|st| {
                        let (w, h) =
                            (child.renderer.width() as f32, child.renderer.height() as f32);
                        let top = child.renderer.top_inset();
                        let pad = child.renderer.padding();
                        let outer = crate::pane::Rect::new(
                            pad,
                            top,
                            (w - pad * 2.0).max(0.0),
                            (h - top - pad).max(0.0),
                        );
                        st.tree.layout(outer)
                    })
                    .unwrap_or_default();
                let active_id = child.tab_states.get(tab_idx).map(|st| st.active_pane).unwrap_or(0);
                if let Some(pane) = child.panes.get(&active_id) {
                    let mut grid = pane.parser.lock();
                    if let Some(search) =
                        child.tab_states.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                    {
                        search.maybe_refresh_for_revision(grid.grid_mut());
                    }
                    let search = child.tab_states.get(tab_idx).and_then(|t| t.search.as_ref());
                    if let Err(e) = child.renderer.render(
                        grid.grid_mut(),
                        &theme,
                        child.cursor_visible.load(std::sync::atomic::Ordering::Relaxed),
                        child.selection.as_ref(),
                        &child.tabs,
                        &pane_rects,
                        active_id,
                        search,
                        None, // command palette: not exposed in child window yet
                        None, // ime preedit: not exposed in child window yet
                        pane.viewport_top_abs,
                    ) {
                        tracing::warn!("child render error: {e}");
                    }
                    child.last_render = Instant::now();
                }
            }
            WindowEvent::Resized(size) => {
                child.renderer.resize(size.width, size.height);
                let (cols, rows) = child.renderer.cells();
                for pane in child.panes.values() {
                    pane.parser.lock().grid_mut().resize(cols, rows);
                    if let Some(pty) = pane.pty.as_ref() {
                        (pty.resize)(cols, rows);
                    }
                }
                child.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => {
                child.modifiers = m.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                child.cursor_pos = (position.x, position.y);
                // Cross-window drag-merge from child: when a tab in the
                // child's bar is held, look for a destination on another
                // window (main or sibling). Drops the &mut child borrow
                // before calling compute_child_drag_target (which needs
                // &self.child_windows for sibling lookups).
                if child.mouse_down && child.pressed_tab.is_some() {
                    let local = (position.x, position.y);
                    // child borrow ends at last use; safe to call &mut self next
                    let _ = child;
                    let tgt = self.compute_child_drag_target(win_id, local);
                    if let Some(c) = self.child_windows.get_mut(&win_id) {
                        c.drag_target = tgt;
                        if tgt.is_some() {
                            c.window.request_redraw();
                            return;
                        }
                    }
                    return;
                }
                if child.mouse_down {
                    if let Some((row, col)) =
                        child.renderer.pixel_to_cell(position.x as f32, position.y as f32)
                    {
                        if let Some(sel) = child.selection.as_mut() {
                            sel.extend(row, col);
                            child.window.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Pressed => {
                    let (px, py) = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
                    let bar_width = child.renderer.width() as f32;
                    let layout = TabBarLayout::compute(&child.tabs, bar_width);
                    if let Some(hit) = layout.hit(px, py) {
                        match hit {
                            TabHit::Activate(i) => {
                                child.tabs.activate(i);
                                child.pressed_tab = Some(i);
                                child.mouse_down = true;
                            }
                            TabHit::Close(_) | TabHit::NewTab => {
                                // close/new-tab in child are deferred —
                                // single-tab children today. Swallow.
                            }
                        }
                        child.window.request_redraw();
                        return;
                    }
                    child.mouse_down = true;
                    if let Some((row, col)) = child.renderer.pixel_to_cell(px, py) {
                        child.selection = Some(Selection::new(row, col));
                    }
                    child.window.request_redraw();
                }
                ElementState::Released => {
                    let pending_drop = child.pressed_tab.zip(child.drag_target.take());
                    child.mouse_down = false;
                    child.pressed_tab = None;
                    child.drag_target = None;
                    if let Some(sel) = child.selection.as_ref() {
                        if sel.is_empty() {
                            child.selection = None;
                            child.window.request_redraw();
                        }
                    }
                    if let Some((src_idx, target)) = pending_drop {
                        // Release the child borrow before re-entering
                        // &mut self via the merge path.
                        let _ = child;
                        self.merge_child_into_target(win_id, src_idx, target);
                    }
                }
            },
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let mods = child.modifiers;
                let tab_idx = child.tabs.active_index();
                let active_id = match child.tab_states.get(tab_idx) {
                    Some(st) => st.active_pane,
                    None => return,
                };
                if let Some(bytes) = encode_key(&event, mods) {
                    if let Some(pane) = child.panes.get(&active_id) {
                        if let Some(pty) = pane.pty.as_ref() {
                            let _ = pty.in_tx.send(bytes);
                        }
                    }
                    if child.selection.is_some() {
                        child.selection = None;
                        child.window.request_redraw();
                    }
                }
            }
            _ => {}
        }
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
        self.tab_states.push(TabState {
            tree: PaneTree::leaf(pane_id),
            active_pane: pane_id,
            search: None,
        });
        pane_id
    }

    /// Test-only: read-only access to the internal panes map so tests
    /// can assert "this pane id is gone after detach".
    #[doc(hidden)]
    pub fn __test_pane_ids(&self) -> Vec<u64> {
        self.panes.keys().copied().collect()
    }

    /// Test-only: tab count.
    #[doc(hidden)]
    pub fn __test_tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Test-only: borrow the redraw target Arc for a given pane id,
    /// so a test can assert the per-pane redraw indirection survives
    /// state transfers.
    #[doc(hidden)]
    pub fn __test_pane_redraw_target(&self, id: u64) -> Option<Arc<Mutex<Option<Arc<Window>>>>> {
        self.panes.get(&id).map(|p| p.redraw_target.clone())
    }

    fn split_active(&mut self, dir: Direction) {
        let new_id = next_pane_id();
        let new_pane = self.spawn_pane();
        let i = self.tabs.active_index();
        let Some(st) = self.tab_states.get_mut(i) else { return };
        let focus = st.active_pane;
        if st.tree.split(focus, dir, new_id) {
            st.active_pane = new_id;
            self.panes.insert(new_id, new_pane);
        }
    }

    fn close_active_pane(&mut self) {
        let i = self.tabs.active_index();
        let Some(st) = self.tab_states.get_mut(i) else { return };
        let focus = st.active_pane;
        if matches!(st.tree, PaneTree::Leaf { id } if id == focus) {
            self.close_tab_at(i);
            return;
        }
        let new_focus = st.tree.leaves().into_iter().find(|id| *id != focus).unwrap_or(focus);
        if st.tree.close(focus) {
            st.active_pane = new_focus;
            self.panes.remove(&focus);
        }
    }

    fn focus_pane_dir(&mut self, dir: Direction) {
        let i = self.tabs.active_index();
        let Some(st) = self.tab_states.get_mut(i) else { return };
        if let Some(next) = st.tree.focus_neighbor(st.active_pane, dir) {
            st.active_pane = next;
        }
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

    /// Manual reload (bound to `Action::ReloadConfig`). Reads the config
    /// from disk and applies it, bypassing the watcher channel.
    fn force_reload_config(&mut self) {
        let Some(path) = sonic_core::config::Config::default_path() else { return };
        match Config::load_or_default(&path) {
            Ok(cfg) => self.apply_new_config(cfg),
            Err(e) => tracing::warn!("force_reload_config: parse failed: {e:#}"),
        }
    }

    /// Diff `new_cfg` against the live `self.config` and apply the
    /// minimal set of swaps: theme reload, font rebuild (atlas
    /// invalidated), keymap reload. Always replaces `self.config` last
    /// so observers see a consistent snapshot.
    fn apply_new_config(&mut self, new_cfg: Config) {
        let assets = crate::asset_dir();

        // Theme
        if new_cfg.theme != self.config.theme {
            let theme_path = assets.join("themes").join(format!("{}.toml", new_cfg.theme));
            match Theme::load(&theme_path) {
                Ok(t) => {
                    tracing::info!("live-reload: theme -> {}", t.name);
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_theme(&t);
                    }
                    self.theme = t;
                }
                Err(e) => tracing::warn!("live-reload: theme {:?} failed: {e:#}", theme_path),
            }
        }

        // Font
        let font_changed = new_cfg.font.family != self.config.font.family
            || (new_cfg.font.size - self.config.font.size).abs() > f32::EPSILON
            || (new_cfg.font.line_height - self.config.font.line_height).abs() > f32::EPSILON;
        if font_changed {
            if let Some(r) = self.renderer.as_mut() {
                r.set_font(&new_cfg.font.family, new_cfg.font.size, new_cfg.font.line_height);
                // Cell metrics changed → the renderer now fits a
                // different (cols, rows) inside the same window
                // pixels. Resize every pane's grid + PTY so the shell
                // and parser agree with what we'll actually draw.
                // Without this, the grid keeps drawing at the old
                // dimensions and `stty size` inside the shell reports
                // stale values until the user drags the window edge.
                let (cols, rows) = r.cells();
                resize_all_panes(&self.panes, cols, rows);
            }
            // Apply the same swap to every torn-out child window. Each
            // child owns its own GpuRenderer, so it needs the font
            // change AND the matching pane resize against its own cell
            // metrics (its window can be a different size from main).
            for child in self.child_windows.values_mut() {
                child.renderer.set_font(
                    &new_cfg.font.family,
                    new_cfg.font.size,
                    new_cfg.font.line_height,
                );
                let (cols, rows) = child.renderer.cells();
                resize_all_panes(&child.panes, cols, rows);
            }
            tracing::info!(
                "live-reload: font -> {} @ {}px x{}",
                new_cfg.font.family,
                new_cfg.font.size,
                new_cfg.font.line_height,
            );
        }

        // Keymap
        if new_cfg.keymap != self.config.keymap {
            let km_path = assets.join("keymaps").join(format!("{}.toml", new_cfg.keymap));
            match Keymap::load(&km_path) {
                Ok(km) => {
                    tracing::info!(
                        "live-reload: keymap -> {} ({} bindings)",
                        km.meta.name,
                        km.bindings.len()
                    );
                    self.keymap = km;
                }
                Err(e) => tracing::warn!("live-reload: keymap {:?} failed: {e:#}", km_path),
            }
        }

        self.config = new_cfg;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        for child in self.child_windows.values() {
            child.window.request_redraw();
        }
    }

    /// Run a keymap-bound action. Returns true if handled (= consume the key).
    fn run_action(&mut self, action: &Action) -> bool {
        match action {
            Action::CopyToClipboard => self.copy_selection(),
            Action::PasteFromClipboard => self.paste_clipboard(),
            Action::ReloadConfig => self.force_reload_config(),
            Action::NewTab => {
                let n = self.tabs.len() + 1;
                self.new_tab(format!("shell {n}"));
            }
            Action::CloseTab => {
                let i = self.tabs.active_index();
                self.close_tab_at(i);
            }
            Action::NextTab => self.tabs.next(),
            Action::PrevTab => self.tabs.prev(),
            Action::ActivateTab(i) => self.tabs.activate(*i),
            Action::ActivateLastTab => {
                let last = self.tabs.len().saturating_sub(1);
                self.tabs.activate(last);
            }
            Action::SplitRight => self.split_active(Direction::Right),
            Action::SplitDown => self.split_active(Direction::Down),
            Action::ClosePane => self.close_active_pane(),
            Action::FocusPane(d) => self.focus_pane_dir(*d),
            Action::OpenSearch => self.open_search(),
            Action::OpenPreferences => self.open_preferences(),
            Action::OpenCommandPalette => self.toggle_command_palette(),
            Action::ScrollToPrevPrompt => self.scroll_to_prompt(false),
            Action::ScrollToNextPrompt => self.scroll_to_prompt(true),
            Action::Scroll(_)
            | Action::IncreaseFontSize
            | Action::DecreaseFontSize
            | Action::ResetFontSize
            | Action::ToggleFullscreen
            | Action::ResizePane { .. }
            | Action::NewWindow => {
                tracing::info!("action {action:?} accepted but not yet wired up");
            }
        }
        true
    }

    fn open_search(&mut self) {
        let i = self.tabs.active_index();
        let pane_id = match self.tab_states.get(i) {
            Some(t) => t.active_pane,
            None => return,
        };
        let mut s = SearchState::new();
        if let Some(pane) = self.panes.get(&pane_id) {
            s.refresh(pane.parser.lock().grid());
        }
        if let Some(st) = self.tab_states.get_mut(i) {
            st.search = Some(s);
        }
    }

    /// Open (or re-focus) the v0.6 preferences window. The window itself
    /// is rendered by the OS chrome only for now — the prefs subsystem
    /// (controls, layout, edit buffer) is fully wired through
    /// [`PrefsState`]; visual control rendering inside the window is a
    /// Tier-2 follow-up that requires factoring `GpuRenderer` out of the
    /// terminal-grid render path.
    fn open_preferences(&mut self) {
        // Already open → just re-focus.
        if let Some(w) = self.prefs_window.as_ref() {
            w.focus_window();
            return;
        }
        // Defer until the event loop has resumed (we need an
        // ActiveEventLoop to create a Window).
        tracing::info!("OpenPreferences requested; awaiting resumed-event-loop hook");
        // The actual creation happens in window_event on next iteration
        // via a pending flag — but to keep diff small we lazily create
        // on the next `WindowEvent::RedrawRequested` of the main window.
        self.pending_prefs_open = true;
    }

    fn search_active(&self) -> bool {
        let i = self.tabs.active_index();
        self.tab_states.get(i).map(|t| t.search.is_some()).unwrap_or(false)
    }

    /// Toggle the command palette open/closed.
    fn toggle_command_palette(&mut self) {
        let now_open = self.command_palette.toggle();
        tracing::info!(open = now_open, "command palette toggled");
        self.draw_command_palette_overlay();
    }

    /// Visual overlay rendering for the command palette is intentionally
    /// deferred to a follow-up PR (see ROADMAP). For now this just logs
    /// the visible state so the wiring can be exercised end-to-end while
    /// the GPU overlay is being designed.
    ///
    /// TODO(palette-overlay): draw a centered floating panel via the
    /// existing `quad::QuadPipeline` + glyphon spans, mirroring the
    /// tab-bar's chrome helpers. Must NOT live inside `render.rs`; add a
    /// sibling module so the GPU renderer stays focused on grid cells.
    pub(crate) fn draw_command_palette_overlay(&self) {
        if !self.command_palette.is_open() {
            return;
        }
        tracing::info!(
            query = %self.command_palette.query(),
            selected = self.command_palette.selected(),
            visible_count = self.command_palette.len(),
            "command palette overlay (visual TODO)"
        );
    }

    /// Route a key event into the open command palette. Returns true if
    /// the event was consumed.
    fn command_palette_handle_key(&mut self, event: &KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if !self.command_palette.is_open() {
            return false;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.command_palette.close();
                true
            }
            Key::Named(NamedKey::Enter) => {
                let action = self.command_palette.current().cloned();
                self.command_palette.close();
                if let Some(a) = action {
                    self.run_action(&a);
                }
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.command_palette.move_selection_down();
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.command_palette.move_selection_up();
                true
            }
            Key::Named(NamedKey::Backspace) => {
                self.command_palette.backspace();
                true
            }
            Key::Character(s) => {
                for ch in s.chars() {
                    if !ch.is_control() {
                        self.command_palette.input_char(ch);
                    }
                }
                true
            }
            _ => true, // swallow other keys while palette is open
        }
    }

    /// Route a key event into the active search state. Returns true if the
    /// event was consumed (Esc closes, Enter/Shift+Enter cycle, printable
    /// chars extend the query, Backspace trims).
    fn search_handle_key(&mut self, event: &KeyEvent, mods: ModifiersState) -> bool {
        let i = self.tabs.active_index();
        let pane_id = match self.tab_states.get(i) {
            Some(t) if t.search.is_some() => t.active_pane,
            _ => return false,
        };
        let pane = match self.panes.get(&pane_id) {
            Some(p) => p,
            None => return false,
        };
        let grid_guard = pane.parser.lock();
        let grid = grid_guard.grid();

        let Some(st) = self.tab_states.get_mut(i) else { return false };
        let Some(search) = st.search.as_mut() else { return false };

        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                st.search = None;
                true
            }
            Key::Named(NamedKey::Enter) => {
                if mods.shift_key() {
                    search.prev();
                } else {
                    search.next();
                }
                true
            }
            Key::Named(NamedKey::Backspace) => {
                search.backspace(grid);
                true
            }
            Key::Named(NamedKey::Space) => {
                search.input_char(' ', grid);
                true
            }
            Key::Character(s) => {
                // Cmd+I toggles case sensitivity; Cmd+R toggles regex
                // mode; Cmd+G / Cmd+Shift+G jump to next/prev match.
                if mods.super_key() {
                    match s.as_ref() {
                        "i" | "I" => {
                            search.toggle_case_sensitive(grid);
                            return true;
                        }
                        "r" | "R" => {
                            search.toggle_regex(grid);
                            return true;
                        }
                        "g" | "G" => {
                            if mods.shift_key() {
                                search.prev();
                            } else {
                                search.next();
                            }
                            return true;
                        }
                        _ => {}
                    }
                }
                for ch in s.chars() {
                    search.input_char(ch, grid);
                }
                true
            }
            _ => false,
        }
    }

    fn copy_selection(&mut self) {
        let Some(sel) = self.selection.as_ref() else {
            return;
        };
        if sel.is_empty() {
            return;
        }
        let Some(pane) = self.active_pane() else { return };
        let text = sel.as_text(pane.parser.lock().grid());
        if text.is_empty() {
            return;
        }
        if let Some(cb) = self.clipboard.as_mut() {
            if let Err(e) = cb.set_text(text.clone()) {
                tracing::warn!("clipboard set failed: {e}");
            } else {
                tracing::info!("copied {} bytes", text.len());
            }
        }
    }

    fn paste_clipboard(&mut self) {
        if let Some(cb) = self.clipboard.as_mut() {
            if let Ok(text) = cb.get_text() {
                let bracketed = self
                    .active_pane()
                    .map(|p| p.parser.lock().bracketed_paste_enabled())
                    .unwrap_or(false);
                let bytes = wrap_paste(&text, bracketed);
                self.write_to_pty(bytes);
            }
        }
    }

    /// Compute the new viewport-top row for a "scroll to previous/next
    /// prompt" action, mutate the active pane's `viewport_top_abs`, and
    /// request a redraw. Pure logic (the row selection) is delegated to
    /// [`pick_prompt_target`] so it is unit-testable without a window.
    fn scroll_to_prompt(&mut self, forward: bool) {
        let i = self.tabs.active_index();
        let Some(st) = self.tab_states.get(i) else { return };
        let pane_id = st.active_pane;
        let Some(pane) = self.panes.get_mut(&pane_id) else { return };
        let new_top = {
            let guard = pane.parser.lock();
            let grid = guard.grid();
            let cur = pane.viewport_top_abs.unwrap_or_else(|| grid.scrollback_len() as u64);
            pick_prompt_target(grid, cur, forward)
        };
        if let Some(top) = new_top {
            pane.viewport_top_abs = Some(top);
            tracing::info!(target = top, "scrolled to prompt row");
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Resolve the OSC 8 URI at `(row, col)` in the active pane, if any.
    /// The parser/grid lock is acquired and released entirely within this
    /// call — callers must not hold it across spawn / IO.
    fn hyperlink_uri_at(&self, row: u16, col: u16) -> Option<String> {
        let pane = self.active_pane()?;
        let guard = pane.parser.try_lock()?;
        let grid = guard.grid();
        if row >= grid.rows || col >= grid.cols {
            return None;
        }
        let hid = grid.row(row)[col as usize].hyperlink?;
        let uri = guard.hyperlinks().lookup(hid).map(|h| h.uri.clone());
        drop(guard);
        uri
    }

    /// Create the v0.6 preferences window. Called from `window_event`
    /// after `open_preferences` set the pending flag (we need an
    /// `ActiveEventLoop` to create a `Window`).
    fn create_prefs_window(&mut self, el: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Sonic Preferences")
            .with_inner_size(winit::dpi::LogicalSize::new(
                crate::prefs::PREFS_WIN_W,
                crate::prefs::PREFS_WIN_H,
            ))
            .with_resizable(true);
        let w = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!("prefs window create failed: {e}");
                return;
            }
        };
        let path = sonic_core::config::Config::default_path()
            .unwrap_or_else(|| std::path::PathBuf::from("sonic.toml"));
        self.prefs_state = Some(PrefsState::new(self.config.clone(), path));
        self.prefs_window = Some(w);
    }

    /// Persist current prefs edit buffer to disk and live-apply
    /// theme/keymap changes if loaders are available. Idempotent and
    /// safe to call when nothing is dirty. Called on Apply-button click
    /// AND on prefs-window close (we treat close as save).
    fn commit_prefs_and_apply_live(&mut self) {
        let Some(s) = self.prefs_state.as_mut() else { return };
        if !s.is_dirty() {
            return;
        }
        // Snapshot fields that drive live-apply BEFORE apply() resets
        // the original snapshot (which is what we'd otherwise diff).
        let new_theme_name = s.config.theme.clone();
        let new_keymap_name = s.config.keymap.clone();
        let old_theme_name = self.config.theme.clone();
        let old_keymap_name = self.config.keymap.clone();
        if let Err(e) = s.apply() {
            tracing::error!("prefs apply failed: {e}");
            return;
        }
        // Mirror the saved config into the live App config so any new
        // panes / windows pick up the change.
        self.config = s.config.clone();
        // Live theme apply.
        if new_theme_name != old_theme_name {
            if let Some(loader) = self.theme_loader.as_ref() {
                match loader(&new_theme_name) {
                    Ok(t) => {
                        self.theme = t;
                        if let Some(w) = self.window.as_ref() {
                            w.request_redraw();
                        }
                    }
                    Err(e) => tracing::warn!("live theme reload '{new_theme_name}' failed: {e}"),
                }
            }
        }
        // Live keymap apply.
        if new_keymap_name != old_keymap_name {
            if let Some(loader) = self.keymap_loader.as_ref() {
                match loader(&new_keymap_name) {
                    Ok(k) => self.keymap = k,
                    Err(e) => tracing::warn!("live keymap reload '{new_keymap_name}' failed: {e}"),
                }
            }
        }
    }

    /// Handle events arriving for the preferences window.
    fn handle_prefs_event(&mut self, _el: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // Persist edits on close (per spec: "persist on close").
                self.commit_prefs_and_apply_live();
                self.prefs_window = None;
                self.prefs_state = None;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let (x, y) = (self.cursor_pos.0 as f32, self.cursor_pos.1 as f32);
                let hit = self.prefs_state.as_ref().and_then(|s| s.classify_click(x, y));
                match hit {
                    Some(PrefsHit::Apply) => {
                        self.commit_prefs_and_apply_live();
                    }
                    Some(PrefsHit::Cancel) => {
                        if let Some(s) = self.prefs_state.as_mut() {
                            s.cancel();
                        }
                        self.prefs_window = None;
                        self.prefs_state = None;
                    }
                    other => {
                        let Some(s) = self.prefs_state.as_mut() else { return };
                        match other {
                            Some(PrefsHit::Sidebar(cat)) => {
                                s.blur_text_fields();
                                s.set_category(cat);
                            }
                            Some(PrefsHit::Toggle(id)) => {
                                s.blur_text_fields();
                                let _ = s.flip_toggle(id);
                            }
                            Some(PrefsHit::SliderTrack(id)) => {
                                s.blur_text_fields();
                                let _ = s.drag_slider(id, x);
                            }
                            Some(PrefsHit::DropdownHeader(id)) => {
                                s.blur_text_fields();
                                let _ = s.toggle_dropdown(id);
                            }
                            Some(PrefsHit::DropdownOption { id, index }) => {
                                s.blur_text_fields();
                                let _ = s.select_dropdown(id, index);
                            }
                            Some(PrefsHit::ColorCell { id, index }) => {
                                s.blur_text_fields();
                                let _ = s.pick_color(id, index);
                            }
                            Some(PrefsHit::TextField(id)) => {
                                let _ = s.focus_text_field(id);
                            }
                            Some(PrefsHit::Apply) | Some(PrefsHit::Cancel) => unreachable!(),
                            None => {
                                s.blur_text_fields();
                            }
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let Some(s) = self.prefs_state.as_mut() else { return };
                match &event.logical_key {
                    Key::Named(NamedKey::Backspace) => {
                        if let Some(id) = s.focused_field {
                            let new_val = if let Some(crate::prefs::Control::TextField(tf)) =
                                s.controls.iter_mut().find(|c| c.id() == id)
                            {
                                tf.pop_char();
                                let v = tf.get().to_string();
                                Some(if v.is_empty() { None } else { Some(v) })
                            } else {
                                None
                            };
                            // Best-effort: only the Shell text field exists
                            // today; mirror its value into config.
                            if let Some(v) = new_val {
                                if s.config.terminal.shell != v {
                                    s.config.terminal.shell = v;
                                    s.dirty = true;
                                }
                            }
                        }
                    }
                    Key::Named(NamedKey::Escape) => {
                        s.cancel();
                        self.prefs_window = None;
                        self.prefs_state = None;
                    }
                    Key::Character(chs) => {
                        for ch in chs.chars() {
                            if !ch.is_control() {
                                s.type_into_focused(ch);
                            }
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
            }
            _ => {}
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let cols = self.config.window.cols;
        let rows = self.config.window.rows;

        let attrs = Window::default_attributes()
            .with_title(format!("Sonic Terminal — {}", self.theme.name))
            .with_inner_size(winit::dpi::LogicalSize::new(
                f32::from(cols) * 9.0 + self.config.window.padding * 2.0,
                f32::from(rows) * (self.config.font.size * self.config.font.line_height)
                    + self.config.window.padding * 2.0
                    + crate::tabbar_view::TAB_BAR_HEIGHT,
            ));
        let window = Arc::new(el.create_window(attrs).expect("create window"));
        // Enable IME so CJK input methods (Pinyin, Japanese, Korean…) can
        // deliver preedit + commit events instead of raw keystrokes.
        window.set_ime_allowed(true);
        self.scale_factor = window.scale_factor();

        let renderer = GpuRenderer::new(
            window.clone(),
            el,
            &self.theme,
            &self.config.font.family,
            self.config.font.size,
            self.config.font.line_height,
            self.config.window.padding,
        )
        .expect("init renderer");

        self.window = Some(window.clone());
        self.renderer = Some(renderer);

        // Seed the first tab + pane now that the window + renderer exist.
        self.new_tab("shell");

        let (rc, rr) = self.renderer.as_ref().map(|r| r.cells()).unwrap_or((0, 0));
        tracing::info!(
            "Sonic ready. theme={} keymap={} bindings={} grid={}x{}",
            self.theme.name,
            self.keymap.meta.name,
            self.keymap.bindings.len(),
            rc,
            rr,
        );
        window.request_redraw();

        // Spawn the sonic.toml live-reload watcher (best-effort; if the
        // user has no config path or the parent dir is unreadable, the
        // app still runs — just without live reload).
        if self.config_watcher.is_none() {
            if let Some(path) = sonic_core::config::Config::default_path() {
                let proxy = self.event_loop_proxy.clone();
                let spawn_result = if let Some(p) = proxy {
                    ConfigWatcher::spawn_with_wake(path.clone(), move || {
                        // Failure here means the event loop has shut
                        // down — nothing to wake, safe to ignore.
                        let _ = p.send_event(UserEvent::ConfigChanged);
                    })
                } else {
                    // No proxy (test harness) — fall back to the
                    // poll-only behavior; the watcher still delivers,
                    // it just won't wake an idle loop.
                    ConfigWatcher::spawn(path.clone())
                };
                match spawn_result {
                    Ok(w) => {
                        tracing::info!("config watcher: watching {path:?}");
                        self.config_watcher = Some(w);
                    }
                    Err(e) => tracing::warn!("config watcher disabled: {e:#}"),
                }
            }
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, event: UserEvent) {
        // Watcher-thread wake. Drain the channel and apply any new
        // config immediately so the reload doesn't sit queued until
        // the next OS event arrives. apply_new_config already
        // request_redraw()s every live window.
        match event {
            UserEvent::ConfigChanged => self.poll_config_reload(),
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, win_id: WindowId, event: WindowEvent) {
        // Drain any pending sonic.toml live-reload deliveries before
        // dispatching the event — guarantees font/theme/keymap swaps
        // land on the same redraw tick they were detected on.
        self.poll_config_reload();
        // v0.6: route events to the preferences window if it owns this id.
        if let Some(pw) = self.prefs_window.as_ref() {
            if pw.id() == win_id {
                self.handle_prefs_event(el, event);
                return;
            }
        }
        // Tear-out child windows: route to the dedicated handler so
        // each child renders/handles input on its own surface.
        if self.child_windows.contains_key(&win_id) {
            self.handle_child_window_event(el, win_id, event);
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                // If child windows still own tabs, hide the main
                // window instead of exiting the app — the children
                // are independent live terminals and must keep
                // running. Only exit when nothing else is alive.
                if self.child_windows.is_empty() {
                    el.exit();
                } else {
                    self.hide_main_window();
                }
            }

            WindowEvent::RedrawRequested => {
                // Compute per-pane rects in window pixels so the renderer can
                // draw a border around each one (and a brighter one around
                // the focused pane). The active pane's grid is rendered into
                // the full content area; per-pane Buffer rendering is v0.4.
                let tab_idx = self.tabs.active_index();
                let pane_rects: Vec<(u64, crate::pane::Rect)> = self
                    .tab_states
                    .get(tab_idx)
                    .map(|st| {
                        if let Some(r) = self.renderer.as_ref() {
                            let (w, h) = (r.width() as f32, r.height() as f32);
                            let top = r.top_inset();
                            let pad = r.padding();
                            let outer = crate::pane::Rect::new(
                                pad,
                                top,
                                (w - pad * 2.0).max(0.0),
                                (h - top - pad).max(0.0),
                            );
                            st.tree.layout(outer)
                        } else {
                            Vec::new()
                        }
                    })
                    .unwrap_or_default();
                let active_id = self.tab_states.get(tab_idx).map(|st| st.active_pane).unwrap_or(0);

                if let (Some(r), Some(pane)) = (self.renderer.as_mut(), self.panes.get(&active_id))
                {
                    // Block on the parser lock: the VT thread holds it only
                    // for the duration of a single `Parser::advance` call,
                    // which is sub-millisecond even on a `cat largefile`
                    // burst. The old try_lock() path returned early on a
                    // miss, which caused winit on macOS to immediately
                    // re-fire RedrawRequested in a tight loop (silently
                    // burning ~100% CPU as long as the VT thread had work
                    // queued). Holding the lock briefly is strictly cheaper
                    // than spinning the AppKit event loop.
                    let cursor_rc = {
                        let mut grid = pane.parser.lock();
                        // Mirror the latest OSC 0/2 title from the parser into
                        // the active tab so the tab bar reflects "vim foo" /
                        // "~/Code" / etc. Falls back to the prior title (e.g.
                        // "shell") when the pty hasn't sent one yet.
                        if let Some(t) = grid.title() {
                            let pretty = render_tab_title(t);
                            let cur = self.tabs.active().map(|tab| tab.title.clone());
                            if cur.as_deref() != Some(pretty.as_str()) {
                                self.tabs.set_active_title(pretty);
                            }
                        }
                        if let Some(search) =
                            self.tab_states.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                        {
                            search.maybe_refresh_for_revision(grid.grid_mut());
                        }
                        let search = self.tab_states.get(tab_idx).and_then(|t| t.search.as_ref());
                        if let Err(e) = r.render(
                            grid.grid_mut(),
                            &self.theme,
                            self.cursor_visible.load(std::sync::atomic::Ordering::Relaxed),
                            self.selection.as_ref(),
                            &self.tabs,
                            &pane_rects,
                            active_id,
                            search,
                            Some(&self.command_palette),
                            Some(&self.ime),
                            pane.viewport_top_abs,
                        ) {
                            tracing::warn!("render error: {e}");
                        }
                        self.last_render = Instant::now();
                        let g = grid.grid_mut();
                        (g.cursor.row, g.cursor.col)
                    };
                    // Tell the OS where the active text cursor lives so the
                    // IME candidate window (pinyin candidates, Japanese
                    // romaji selector, Korean Hangul composer) appears
                    // immediately below the cell being edited — not
                    // pinned to the top-left corner of the screen as
                    // happens when the area is never set.
                    if let Some(w) = &self.window {
                        let x = r.padding() + f32::from(cursor_rc.1) * r.cell_w;
                        let y = r.top_inset() + f32::from(cursor_rc.0) * r.cell_h;
                        let pos = winit::dpi::PhysicalPosition::new(x as i32, y as i32);
                        let size = winit::dpi::PhysicalSize::new(
                            r.cell_w.ceil() as u32,
                            r.cell_h.ceil() as u32,
                        );
                        w.set_ime_cursor_area(pos, size);
                    }
                }
            }

            WindowEvent::Focused(focused) => {
                // Reset IME state across focus transitions. When focus is
                // lost mid-composition, the OS IME panel detaches without
                // sending us a Commit; dropping the preedit avoids replaying
                // stale composition state on the next focus-in. Toggling
                // `set_ime_allowed` nudges the OS to re-attach the input
                // context cleanly on macOS / Windows.
                self.ime.cancel();
                if let Some(w) = &self.window {
                    w.set_ime_allowed(focused);
                    if focused {
                        w.set_ime_allowed(true);
                    }
                    w.request_redraw();
                }
            }

            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                    let (cols, rows) = r.cells();
                    for pane in self.panes.values() {
                        pane.parser.lock().grid_mut().resize(cols, rows);
                        if let Some(pty) = pane.pty.as_ref() {
                            (pty.resize)(cols, rows);
                        }
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale_factor = scale_factor;
            }

            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
            }

            // -- Mouse --
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
                // Cross-window drag-merge: if a tab is held, update the
                // pending drop target based on the global cursor
                // position. If a target is found, suppress tear-out —
                // mouse-up will merge instead. If no target, fall
                // through to the existing tear-out check.
                if self.mouse_down && self.pressed_tab.is_some() {
                    self.drag_target = self.compute_main_drag_target((position.x, position.y));
                    if self.drag_target.is_some() {
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        return;
                    }
                }
                // Tear-out detection: if a tab press is in flight and
                // the cursor has dragged far enough below the bar, pop
                // the tab into its own window.
                if self.mouse_down {
                    if let Some(idx) = self.pressed_tab {
                        if let Some(t) =
                            detect_tear_out(idx, (position.x as f32, position.y as f32))
                        {
                            self.pressed_tab = None;
                            self.mouse_down = false;
                            self.tear_out_tab(el, t.tab_index);
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                            return;
                        }
                    }
                }
                if self.mouse_down {
                    if let Some(r) = self.renderer.as_ref() {
                        if let Some((row, col)) =
                            r.pixel_to_cell(position.x as f32, position.y as f32)
                        {
                            if let Some(sel) = self.selection.as_mut() {
                                sel.extend(row, col);
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                            }
                        }
                    }
                } else {
                    // Hover-without-button: switch the OS cursor to a pointer
                    // when the cell under the mouse is part of a hyperlink,
                    // and reset to Default when leaving.
                    let over_link = self
                        .renderer
                        .as_ref()
                        .and_then(|r| r.pixel_to_cell(position.x as f32, position.y as f32))
                        .and_then(|(row, col)| self.hyperlink_uri_at(row, col))
                        .is_some();
                    if over_link != self.hover_link {
                        self.hover_link = over_link;
                        if let Some(w) = &self.window {
                            w.set_cursor(if over_link {
                                CursorIcon::Pointer
                            } else {
                                CursorIcon::Default
                            });
                        }
                    }
                }
            }

            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Pressed => {
                    self.mouse_down = true;
                    let px = self.cursor_pos.0 as f32;
                    let py = self.cursor_pos.1 as f32;
                    let window_width =
                        self.window.as_ref().map(|w| w.inner_size().width as f32).unwrap_or(0.0);
                    let layout = TabBarLayout::compute(&self.tabs, window_width);
                    if let Some(hit) = layout.hit(px, py) {
                        match hit {
                            TabHit::Activate(i) => {
                                self.tabs.activate(i);
                                // Record the press so a subsequent drag
                                // below the tab bar can be promoted to a
                                // tear-out gesture.
                                self.pressed_tab = Some(i);
                            }
                            TabHit::Close(i) => self.close_tab_at(i),
                            TabHit::NewTab => {
                                let n = self.tabs.len() + 1;
                                self.new_tab(format!("shell {n}"));
                            }
                        }
                        if self.tabs.is_empty() {
                            if self.child_windows.is_empty() {
                                el.exit();
                            } else {
                                self.hide_main_window();
                            }
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        // Keep mouse_down=true when we recorded a tab
                        // press so cursor-move can promote it to a
                        // tear-out. Other hits (Close, NewTab) consume
                        // the click fully.
                        if self.pressed_tab.is_none() {
                            self.mouse_down = false;
                        }
                        return;
                    }
                    if let Some(r) = self.renderer.as_ref() {
                        if let Some((row, col)) = r.pixel_to_cell(px, py) {
                            // Cmd/Super-click on a hyperlink opens it. The
                            // parser lock is released inside hyperlink_uri_at
                            // before we ever call sonic_core::url_open::open,
                            // so no grid lock is held across the spawn.
                            if self.modifiers.super_key() {
                                if let Some(uri) = self.hyperlink_uri_at(row, col) {
                                    if let Err(e) = sonic_core::url_open::open(&uri) {
                                        tracing::warn!("url_open failed: {e}");
                                    }
                                    self.mouse_down = false;
                                    return;
                                }
                            }
                            self.selection = Some(Selection::new(row, col));
                        }
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                ElementState::Released => {
                    // If a cross-window drop is pending, execute it
                    // before clearing drag state.
                    if let (Some(src_idx), Some(target)) =
                        (self.pressed_tab, self.drag_target.take())
                    {
                        self.merge_main_into_child(src_idx, target);
                    }
                    self.mouse_down = false;
                    self.pressed_tab = None;
                    self.drag_target = None;
                    if let Some(sel) = self.selection.as_ref() {
                        if sel.is_empty() {
                            self.selection = None;
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    }
                }
            },

            // -- IME (CJK / multi-key input methods) --
            WindowEvent::Ime(ime_event) => {
                match ime_event {
                    Ime::Enabled => self.ime.handle_enabled(),
                    Ime::Disabled => self.ime.handle_disabled(),
                    Ime::Preedit(text, cursor) => {
                        self.ime.handle_preedit(&text, cursor);
                    }
                    Ime::Commit(text) => {
                        self.ime.handle_commit(&text);
                        let committed = self.ime.take_commits();
                        if !committed.is_empty() {
                            self.write_to_pty(committed.into_bytes());
                        }
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            // -- Keyboard --
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if self.command_palette.is_open() {
                    // Let the toggle binding (super+shift+P) still close
                    // the palette; everything else routes into palette
                    // state and is NOT forwarded to the pty.
                    if let Some(key_str) = key_event_to_string(&event, self.modifiers) {
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            if matches!(action, Action::OpenCommandPalette) {
                                self.run_action(&action);
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.command_palette_handle_key(&event);
                    if self.pending_prefs_open {
                        self.pending_prefs_open = false;
                        self.create_prefs_window(el);
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // While an IME composition is in flight, the OS owns the
                // keystrokes — they will be delivered to us as Ime events
                // instead. Forwarding them here would double-type. Esc
                // cancels the in-flight composition (preedit dropped, no
                // bytes sent to the PTY) instead of being forwarded.
                if self.ime.is_composing() {
                    if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                        self.ime.cancel();
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    return;
                }
                if self.search_active() {
                    if let Some(key_str) = key_event_to_string(&event, self.modifiers) {
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            if !matches!(action, Action::OpenSearch) {
                                self.run_action(&action);
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.search_handle_key(&event, self.modifiers);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                if let Some(key_str) = key_event_to_string(&event, self.modifiers) {
                    if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                        if self.run_action(&action) {
                            if self.pending_prefs_open {
                                self.pending_prefs_open = false;
                                self.create_prefs_window(el);
                            }
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                            return;
                        }
                    }
                }
                if let Some(bytes) = encode_key(&event, self.modifiers) {
                    self.write_to_pty(bytes);
                    if self.selection.is_some() {
                        self.selection = None;
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }

            _ => {}
        }
    }
}

#[allow(dead_code)]
fn _scroll_used(_a: ScrollAction) {}

fn encode_key(event: &KeyEvent, mods: ModifiersState) -> Option<Vec<u8>> {
    encode_logical(&event.logical_key, mods)
}

#[doc(hidden)]
#[doc(hidden)]
pub fn encode_logical(key: &Key, mods: ModifiersState) -> Option<Vec<u8>> {
    let ctrl = mods.control_key();
    match key {
        Key::Named(n) => Some(match n {
            NamedKey::Enter => b"\r".to_vec(),
            NamedKey::Backspace => b"\x7f".to_vec(),
            NamedKey::Tab => b"\t".to_vec(),
            NamedKey::Escape => b"\x1b".to_vec(),
            NamedKey::Space => b" ".to_vec(),
            NamedKey::ArrowUp => b"\x1b[A".to_vec(),
            NamedKey::ArrowDown => b"\x1b[B".to_vec(),
            NamedKey::ArrowRight => b"\x1b[C".to_vec(),
            NamedKey::ArrowLeft => b"\x1b[D".to_vec(),
            NamedKey::Home => b"\x1b[H".to_vec(),
            NamedKey::End => b"\x1b[F".to_vec(),
            NamedKey::PageUp => b"\x1b[5~".to_vec(),
            NamedKey::PageDown => b"\x1b[6~".to_vec(),
            NamedKey::Delete => b"\x1b[3~".to_vec(),
            NamedKey::F1 => b"\x1bOP".to_vec(),
            NamedKey::F2 => b"\x1bOQ".to_vec(),
            NamedKey::F3 => b"\x1bOR".to_vec(),
            NamedKey::F4 => b"\x1bOS".to_vec(),
            _ => return None,
        }),
        Key::Character(s) => {
            if ctrl {
                let mut bytes = Vec::with_capacity(1);
                for ch in s.chars() {
                    let lower = ch.to_ascii_lowercase();
                    if lower.is_ascii_lowercase() {
                        bytes.push((lower as u8) - b'a' + 1);
                    } else {
                        bytes.extend(ch.to_string().as_bytes());
                    }
                }
                Some(bytes)
            } else {
                Some(s.as_bytes().to_vec())
            }
        }
        _ => None,
    }
}

fn key_event_to_string(event: &KeyEvent, mods: ModifiersState) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if mods.super_key() || mods.control_key() {
        parts.push("super".into());
    }
    if mods.alt_key() {
        parts.push("alt".into());
    }
    if mods.shift_key() {
        parts.push("shift".into());
    }
    let name = key_name(&event.logical_key)?;
    parts.push(name.as_str().to_string());
    Some(parts.join("+").to_ascii_lowercase())
}

#[doc(hidden)]
#[doc(hidden)]
pub fn key_name(key: &Key) -> Option<KeyName> {
    Some(match key {
        Key::Named(n) => KeyName::Static(match n {
            NamedKey::Enter => "enter",
            NamedKey::Backspace => "backspace",
            NamedKey::Tab => "tab",
            NamedKey::Escape => "escape",
            NamedKey::Space => "space",
            NamedKey::ArrowUp => "up",
            NamedKey::ArrowDown => "down",
            NamedKey::ArrowRight => "right",
            NamedKey::ArrowLeft => "left",
            NamedKey::Home => "home",
            NamedKey::End => "end",
            NamedKey::PageUp => "pageup",
            NamedKey::PageDown => "pagedown",
            NamedKey::Delete => "delete",
            NamedKey::F1 => "f1",
            NamedKey::F2 => "f2",
            NamedKey::F3 => "f3",
            NamedKey::F4 => "f4",
            _ => return None,
        }),
        Key::Character(s) => KeyName::Owned(s.to_string()),
        _ => return None,
    })
}

#[doc(hidden)]
#[doc(hidden)]
pub enum KeyName {
    Static(&'static str),
    Owned(String),
}

impl KeyName {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Static(s) => s,
            Self::Owned(s) => s.as_str(),
        }
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

/// Format an OSC 0/2 title for the tab bar with a Nerd Font icon prefix.
///
/// Heuristic: many shell prompts set the title to "user@host: cwd" or just
/// the cwd ; some programs set it to the program name (vim, htop, ssh).
/// We pick an icon based on the leading word, then keep the title compact.
fn render_tab_title(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "\u{f489}  shell".to_string(); // nf-oct-terminal
    }
    let lower = trimmed.to_ascii_lowercase();
    let icon = if lower.starts_with("vim") || lower.starts_with("nvim") {
        "\u{e7c5}" // nf-dev-vim
    } else if lower.starts_with("ssh") {
        "\u{f817}" // nf-mdi-ssh
    } else if lower.starts_with("git") {
        "\u{f1d3}" // nf-fa-git
    } else if lower.starts_with("docker") {
        "\u{f308}" // nf-linux-docker
    } else if lower.starts_with("python")
        || lower.starts_with("ipython")
        || lower.starts_with("python3")
    {
        "\u{e73c}" // nf-dev-python
    } else if lower.starts_with("node") || lower.starts_with("npm") || lower.starts_with("yarn") {
        "\u{e718}" // nf-dev-nodejs_small
    } else if lower.starts_with("cargo") || lower.starts_with("rustc") {
        "\u{e7a8}" // nf-dev-rust
    } else if lower.starts_with("htop") || lower.starts_with("top") || lower.starts_with("btm") {
        "\u{f085}" // nf-fa-cogs
    } else if lower.starts_with("less") || lower.starts_with("cat") || lower.starts_with("bat") {
        "\u{f15c}" // nf-fa-file_text
    } else if trimmed.contains('/') || trimmed.starts_with('~') {
        "\u{f413}" // nf-oct-file_directory
    } else {
        "\u{f489}" // nf-oct-terminal
    };

    // Compact text: keep last path segment if it looks like a path, else
    // first ~24 chars.
    let body = if let Some(last) = trimmed.rsplit('/').next() {
        if trimmed.contains('/') && !last.is_empty() {
            last.to_string()
        } else {
            trimmed.chars().take(24).collect()
        }
    } else {
        trimmed.chars().take(24).collect()
    };

    format!("{icon}  {body}")
}
