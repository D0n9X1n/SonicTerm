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
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowId},
};

use crate::{
    command_palette::CommandPalette,
    ime::ImeState,
    pane::PaneTree,
    prefs::{PrefsHit, PrefsState},
    render::GpuRenderer,
    search::SearchState,
    selection::Selection,
    tabbar_view::{TabBarLayout, TabHit},
    tabs::{Tab, TabBar},
};

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);

#[doc(hidden)]
#[doc(hidden)]
pub fn next_pane_id() -> u64 {
    NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Entry point used by the platform bin crates.
pub fn run(theme: Theme, config: Config, keymap: Keymap) -> Result<()> {
    init_tracing();
    let event_loop = EventLoop::new().context("create event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(theme, config, keymap);
    event_loop.run_app(&mut app).context("run event loop")?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("sonic=info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

/// Per-pane runtime state. The parser is shared with a per-pane VT thread
/// that drains the pty out-channel; the pty handle owns the writer side.
pub struct PaneState {
    pub parser: Arc<Mutex<Parser>>,
    pub pty: Option<PtyHandle>,
}

impl PaneState {
    fn new(parser: Arc<Mutex<Parser>>, pty: Option<PtyHandle>) -> Self {
        Self { parser, pty }
    }
}

/// Per-tab state. The `TabBar` keeps title/order; this struct tracks the
/// pane tree and the focused leaf inside the tab.
pub struct TabState {
    pub tree: PaneTree,
    pub active_pane: u64,
    pub search: Option<SearchState>,
}

struct App {
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
}

impl App {
    fn new(theme: Theme, config: Config, keymap: Keymap) -> Self {
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
        }
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
        let pty = match PtyHandle::spawn_default_shell(cols, rows) {
            Ok(pty) => {
                let parser_clone = parser.clone();
                let out_rx = pty.out_rx.clone();
                let window = self.window.clone();
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
                                                if let Some(w) = &window {
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
                                        if let Some(w) = &window {
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
                                        if let Some(w) = &window {
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
        PaneState::new(parser, pty)
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

    /// Run a keymap-bound action. Returns true if handled (= consume the key).
    fn run_action(&mut self, action: &Action) -> bool {
        match action {
            Action::CopyToClipboard => self.copy_selection(),
            Action::PasteFromClipboard => self.paste_clipboard(),
            Action::ReloadConfig => tracing::info!("reload_config: not yet implemented"),
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
                let bytes = if bracketed {
                    let mut v = Vec::with_capacity(text.len() + 12);
                    v.extend_from_slice(b"\x1b[200~");
                    v.extend_from_slice(text.as_bytes());
                    v.extend_from_slice(b"\x1b[201~");
                    v
                } else {
                    text.into_bytes()
                };
                self.write_to_pty(bytes);
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

    /// Handle events arriving for the preferences window.
    fn handle_prefs_event(&mut self, _el: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.prefs_window = None;
                self.prefs_state = None;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let (x, y) = (self.cursor_pos.0 as f32, self.cursor_pos.1 as f32);
                let Some(s) = self.prefs_state.as_mut() else { return };
                match s.classify_click(x, y) {
                    Some(PrefsHit::Apply) => {
                        if let Err(e) = s.apply() {
                            tracing::error!("prefs apply failed: {e}");
                        }
                    }
                    Some(PrefsHit::Cancel) => {
                        s.cancel();
                        self.prefs_window = None;
                        self.prefs_state = None;
                    }
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
                    None => {
                        s.blur_text_fields();
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

impl ApplicationHandler for App {
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
    }

    fn window_event(&mut self, el: &ActiveEventLoop, win_id: WindowId, event: WindowEvent) {
        // v0.6: route events to the preferences window if it owns this id.
        if let Some(pw) = self.prefs_window.as_ref() {
            if pw.id() == win_id {
                self.handle_prefs_event(el, event);
                return;
            }
        }
        match event {
            WindowEvent::CloseRequested => el.exit(),

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
                    {
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
                        ) {
                            tracing::warn!("render error: {e}");
                        }
                        self.last_render = Instant::now();
                    }
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
                            TabHit::Activate(i) => self.tabs.activate(i),
                            TabHit::Close(i) => self.close_tab_at(i),
                            TabHit::NewTab => {
                                let n = self.tabs.len() + 1;
                                self.new_tab(format!("shell {n}"));
                            }
                        }
                        if self.tabs.is_empty() {
                            el.exit();
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        self.mouse_down = false;
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
                    self.mouse_down = false;
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
                // instead. Forwarding them here would double-type. Escape
                // remains a usable "cancel" for the user.
                if self.ime.is_composing()
                    && !matches!(event.logical_key, Key::Named(NamedKey::Escape))
                {
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
