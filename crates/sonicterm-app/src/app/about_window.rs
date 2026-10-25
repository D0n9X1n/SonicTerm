//! The About window: opening or focusing it, filling it with the build and runtime facts, copying
//! them, and closing it.
//!
//! Like the Memory Usage report, it is an ordinary entry in `App::windows`, with
//! [`WindowRole::About`] and one pane that has no PTY, so selection, search, scrolling and fonts
//! work through the child-window paths, and its rows are plain text in that pane. The content is
//! laid out by [`super::about`].

use std::path::PathBuf;

use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

use super::about::{
    arch_label, button_span, home_relative, os_name, render, AboutFacts, AboutLabels, AboutView,
    ConfigStatus, RendererFacts, RendererKind,
};
use super::memory_report::{report_key, ReportKey, COPY_ALL_HINT};
use super::memory_report_window::ReportPane;
use super::{App, WindowRole, WindowState};

/// History the About pane keeps: its content is a dozen rows, so none is needed beyond them.
const ABOUT_HISTORY_ROWS: usize = 64;

/// The About window's logical size: wide enough for its rows at the default font.
const ABOUT_WINDOW_SIZE: (f64, f64) = (640.0, 320.0);

/// App-level state of the About window.
#[derive(Debug, Default)]
pub(crate) struct AboutState {
    /// `show_about` asked for a window that the next event-loop turn creates.
    pub(crate) open_requested: bool,
    /// What the pane shows now, which Copy copies.
    pub(crate) view: Option<AboutView>,
    /// The last config load error, shown on the Config row until a load succeeds.
    pub(crate) config_error: Option<String>,
    /// The last theme reload error, shown beside the active theme until a reload succeeds.
    pub(crate) theme_error: Option<String>,
    /// The last keymap reload error, shown beside the active keymap until a reload succeeds.
    pub(crate) keymap_error: Option<String>,
    /// The pane grid's size generation the shown rows were laid out for; any resize, of the width
    /// or the height, advances it.
    pub(crate) laid_out_size: Option<u64>,
}

impl App {
    /// The About window, if one is open.
    pub(crate) fn about_window_id(&self) -> Option<WindowId> {
        self.windows.iter().find(|(_, window)| window.role == WindowRole::About).map(|(id, _)| *id)
    }

    /// Whether `id` is the About window.
    pub(crate) fn is_about_window(&self, id: WindowId) -> bool {
        self.windows.get(&id).is_some_and(|window| window.role == WindowRole::About)
    }

    /// Run `show_about`: focus and refresh the open About window, or request one.
    pub(crate) fn show_about(&mut self) {
        let Some(about_id) = self.about_window_id() else {
            // When: `about_window_id` is None, no About window is open, so the next event-loop turn creates one.
            self.about.open_requested = true;
            return;
        };
        if let Some(window) = self.windows.get(&about_id).and_then(|state| state.window.as_ref()) {
            window.focus_window();
        }
        self.frontmost_window = Some(about_id);
        self.refresh_about();
    }

    /// Create the requested About window now that an event loop is available.
    pub(super) fn drain_pending_about(&mut self, event_loop: &ActiveEventLoop) {
        if !std::mem::take(&mut self.about.open_requested) {
            // When: `open_requested` was false, nobody asked for an About window.
            return;
        }
        if self.about_window_id().is_some() {
            // When: `about_window_id` found a window opened since the request; refresh it, never open a second.
            self.refresh_about();
            return;
        }
        let title = self.i18n.translate("about-window-title");
        let size = winit::dpi::LogicalSize::new(ABOUT_WINDOW_SIZE.0, ABOUT_WINDOW_SIZE.1);
        let Some((window, renderer, cells)) =
            self.build_report_window(event_loop, &title, "about", size)
        else {
            // When: build_report_window failed, it logged why and the terminals keep running.
            return;
        };
        // The adapter goes on before the window is first shown, as accesskit requires.
        self.install_accessibility(event_loop, &window);
        self.install_about_window(window.id(), Some(window.clone()), Some(renderer), cells, title);
        window.set_visible(true);
        crate::app::frame_counters::request_native_redraw(&window);
    }

    /// Insert the About window with one PTY-less pane, make it frontmost, and fill it.
    pub(super) fn install_about_window(
        &mut self,
        window_id: WindowId,
        window: Option<std::sync::Arc<winit::window::Window>>,
        renderer: Option<sonicterm_gpu::core::GpuRenderer>,
        cells: (u16, u16),
        title: String,
    ) {
        self.install_report_pane(
            ReportPane { window_id, window, renderer, cells, title },
            WindowRole::About,
            ABOUT_HISTORY_ROWS,
        );
        self.about.view = None;
        self.refresh_about();
    }

    /// Gather the facts now and show them; the values are read live, so a reopen shows changes.
    pub(crate) fn refresh_about(&mut self) {
        let Some(about_id) = self.about_window_id() else {
            // When: `about_window_id` is None, there is no window to fill.
            return;
        };
        // The width and the size generation are read under one lock, so they describe one grid.
        let (pane_cols, size) =
            self.windows.get(&about_id).and_then(about_pane).map_or((u16::MAX, None), |pane| {
                let parser = crate::app::frame_counters::lock_parser(&pane.parser);
                (parser.grid().cols, Some(parser.grid().size_generation()))
            });
        let view = render(&self.collect_about_facts(), &self.about_labels(), pane_cols);
        if self.feed_report_pane(about_id, &view.text) {
            self.about.view = Some(view);
            self.about.laid_out_size = size;
        }
    }

    /// Lay About out again when its pane grid was resized since the last layout, in width or
    /// height: a native resize, a font zoom or reset, a reload or a DPI change all resize it first.
    /// An unchanged grid costs one lock and one comparison.
    pub(crate) fn relayout_about_if_resized(&mut self) {
        let size =
            self.about_window_id().and_then(|id| self.windows.get(&id)).and_then(about_pane).map(
                |pane| {
                    crate::app::frame_counters::lock_parser(&pane.parser).grid().size_generation()
                },
            );
        if size.is_some() && size != self.about.laid_out_size {
            // The rows were laid out for another grid size, so they are laid out again for this one.
            self.refresh_about();
        }
    }

    /// Copy the About rows and report the real clipboard result in the About window. Returns
    /// whether the clipboard accepted them.
    pub(crate) fn copy_about(&mut self) -> bool {
        let Some(about_id) = self.about_window_id() else {
            // When: `about_window_id` is None, no About window is open, so there is nothing to copy.
            return false;
        };
        let Some(text) = self.about.view.as_ref().map(|view| view.copy_text.clone()) else {
            // When: `view` is None, nothing has been shown yet, so there is nothing to copy.
            return false;
        };
        let copied = self.set_clipboard_text(text);
        let (level, key) = if copied {
            (sonicterm_ui::overlays::NotificationLevel::Info, "about-copied")
        } else {
            // When: copied is false, the clipboard refused the text, so the warning says so.
            (sonicterm_ui::overlays::NotificationLevel::Warning, "about-copy-failed")
        };
        let message = self.i18n.translate(key);
        self.show_notification_for_kind(super::FrontmostKind::Child(about_id), level, message);
        copied
    }

    /// Close the About window through the shared child-window teardown. Never quits the app.
    pub(crate) fn close_about_window(&mut self) {
        if let Some(about_id) = self.about_window_id() {
            self.close_child_window(about_id);
        }
    }

    /// Report-specific teardown for a removed window of `role`: the Memory Usage report stops its
    /// sampler, the About window drops its text, and either gives focus back to a terminal window.
    pub(super) fn forget_report_window(&mut self, role: WindowRole, removed: WindowId) {
        match role {
            WindowRole::MemoryReport => self.forget_memory_report_window(removed),
            WindowRole::About => {
                self.about.view = None;
                self.about.laid_out_size = None;
                self.repair_focus_after_report(removed);
            }
            WindowRole::Terminal => {
                // When: role is Terminal, the window held no report state, so nothing is dropped.
            }
        }
    }

    /// Handle a key in the About window after IME, search and copy mode declined it: the copy-all
    /// chord copies, R re-reads the facts, Esc closes, and keymap actions go through the report gate.
    pub(super) fn handle_about_key(
        &mut self,
        win_id: WindowId,
        chords: &[String],
        is_repeat: bool,
    ) {
        match report_key(chords) {
            Some(ReportKey::CopyAll) => {
                // When: CopyAll, copy every row; the notification reports a refused clipboard.
                let _ = self.copy_about();
            }
            Some(ReportKey::Refresh) => {
                // Re-read once per fresh press, not on every auto-repeat tick.
                if !is_repeat {
                    self.refresh_about();
                }
            }
            Some(ReportKey::Close) => self.close_about_window(),
            None => {
                let action = chords.iter().find_map(|chord| self.keymap.lookup(chord).cloned());
                if let Some(action) = action {
                    // A bound chord runs through the report gate; anything else reaches no shell.
                    self.run_action_for_window(&action, win_id);
                }
            }
        }
    }

    /// The facts the About window shows, read from the running app.
    pub(super) fn collect_about_facts(&self) -> AboutFacts {
        let renderer = self
            .main_window_id
            .and_then(|id| self.windows.get(&id))
            .and_then(|window| window.renderer.as_ref())
            .or_else(|| {
                self.windows
                    .values()
                    .filter(|window| window.role == WindowRole::Terminal)
                    .find_map(|window| window.renderer.as_ref())
            });
        let renderer_facts = renderer.map_or(
            RendererFacts {
                kind: RendererKind::NotStarted,
                adapter: String::new(),
                backend: String::new(),
            },
            |renderer| {
                let health = super::status_line::classify_render(
                    cfg!(windows),
                    renderer.is_software_rendering(),
                    renderer.is_software_render_degraded(),
                );
                let kind = match health {
                    sonicterm_ui::status_line::RenderHealth::Hardware => RendererKind::Gpu,
                    sonicterm_ui::status_line::RenderHealth::SoftwareGdi => RendererKind::Gdi,
                    sonicterm_ui::status_line::RenderHealth::SoftwareAdapter => {
                        RendererKind::SoftwareAdapter
                    }
                    sonicterm_ui::status_line::RenderHealth::SoftwareForced => {
                        RendererKind::SoftwareForced
                    }
                };
                let (adapter, backend) = renderer.adapter_summary();
                RendererFacts { kind, adapter, backend: backend.to_owned() }
            },
        );
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .filter(|home| !home.as_os_str().is_empty());
        let config_path =
            self.runtime_config_path.clone().or_else(sonicterm_cfg::config::Config::default_path);
        let config_status = match (&self.about.config_error, &config_path) {
            (Some(error), _) => ConfigStatus::Failed(error.clone()),
            (None, Some(path)) if path.exists() => ConfigStatus::Loaded,
            (None, _) => ConfigStatus::Missing,
        };
        let or_default = |value: &str, default: &str| {
            Some(value).filter(|value| !value.trim().is_empty()).unwrap_or(default).to_owned()
        };
        AboutFacts {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            commit: env!("SONICTERM_BUILD_COMMIT").to_owned(),
            date: env!("SONICTERM_BUILD_DATE").to_owned(),
            os: os_name(),
            arch: arch_label(std::env::consts::OS, std::env::consts::ARCH),
            renderer: renderer_facts,
            font_family: self.config.font.family.clone(),
            font_size_pt: renderer.map_or(self.config.font.size, |renderer| renderer.font_size()),
            theme: or_default(&self.theme.name, "wezterm"),
            theme_error: self.about.theme_error.clone(),
            keymap: or_default(
                &self.keymap.meta.name,
                sonicterm_cfg::keymap::platform_default_keymap_name(),
            ),
            keymap_error: self.about.keymap_error.clone(),
            config_path: config_path
                .as_deref()
                .map_or_else(String::new, |path| home_relative(path, home.as_deref())),
            config_status,
            log_dir: home_relative(&sonicterm_logging::log_dir(), home.as_deref()),
        }
    }

    /// The About window's words in the current locale.
    fn about_labels(&self) -> AboutLabels {
        let word = |key: &str| self.i18n.translate(key);
        AboutLabels {
            version: word("about-version"),
            system: word("about-system"),
            renderer: word("about-renderer"),
            font: word("about-font"),
            theme: word("about-theme"),
            keymap: word("about-keymap"),
            config: word("about-config"),
            logs: word("about-logs"),
            gpu: word("about-renderer-gpu"),
            software: word("about-renderer-software"),
            software_forced: word("about-renderer-software-forced"),
            gdi: word("about-renderer-gdi"),
            not_started: word("about-renderer-not-started"),
            config_loaded: word("about-config-loaded"),
            config_missing: word("about-config-missing"),
            copy_button: word("about-copy-button"),
            hint: self.i18n.t_args("about-hint", Some(&[("copy", COPY_ALL_HINT)])),
        }
    }

    /// Test-only: a press on view cell `row`, `col`, through the same drawn-button hit test and
    /// copy the pointer handler runs. Returns whether it was on the button.
    #[cfg(test)]
    pub(crate) fn about_copy_at(&mut self, row: u16, col: u16) -> bool {
        let hit = self
            .about_window_id()
            .and_then(|id| self.windows.get(&id))
            .zip(self.about.view.as_ref())
            .is_some_and(|(window, view)| about_button_hit(window, &view.button_label, row, col));
        if !hit {
            // When: hit is false, the press is on the text, not the button.
            return false;
        }
        let _ = self.copy_about();
        true
    }

    /// Test-only: open a headless About window `cols` by `rows` cells.
    #[cfg(test)]
    pub(crate) fn __test_open_about_window_sized(&mut self, cols: u16, rows: u16) -> WindowId {
        self.about.open_requested = false;
        let id = super::window_registry::next_synthetic_child_window_id();
        self.install_about_window(id, None, None, (cols, rows), "About SonicTerm".into());
        id
    }

    /// Test-only: open a headless About window with no native window or renderer.
    #[cfg(test)]
    pub(crate) fn __test_open_about_window(&mut self) -> WindowId {
        if let Some(existing) = self.about_window_id() {
            // When: `about_window_id` found an open window, focus and refresh it like production.
            self.show_about();
            return existing;
        }
        self.about.open_requested = false;
        let id = super::window_registry::next_synthetic_child_window_id();
        let grid = super::memory_report_window::HEADLESS_GRID;
        self.install_about_window(id, None, None, grid, "About SonicTerm".into());
        id
    }
}

/// The About window's one pane.
fn about_pane(window: &WindowState) -> Option<&super::PaneState> {
    let tab = window.tab_states.get(window.tabs.active_index())?;
    window.panes.get(&tab.active_pane)
}

/// Whether view cell `row`, `col` of About window `window` is on its Copy button as drawn: the row
/// the view shows there now, after any wrapping and scrolling, is exactly `label`.
pub(super) fn about_button_hit(window: &WindowState, label: &str, row: u16, col: u16) -> bool {
    let Some(pane) = about_pane(window) else {
        // When: about_pane finds no pane, nothing is drawn to hit.
        return false;
    };
    let parser = crate::app::frame_counters::lock_parser(&pane.parser);
    let grid = parser.grid();
    let abs = pane.resolved_view_top(grid) + u64::from(row);
    let last_col = usize::from(grid.cols.saturating_sub(1));
    let text = sonicterm_ui::selection::plain_text_from_grid_range(grid, (0, abs), (last_col, abs));
    button_span(&text, label).is_some_and(|(start, end)| (start..end).contains(&col))
}

#[cfg(test)]
#[path = "about_window_tests.rs"]
mod about_window_tests;
