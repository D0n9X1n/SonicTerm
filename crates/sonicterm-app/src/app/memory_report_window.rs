//! The Memory Usage report window: opening, refreshing, feeding, copying and
//! closing it, and the gate that keeps terminal actions out of it.
//!
//! The window is an ordinary entry in `App::windows` with
//! [`WindowRole::MemoryReport`], so selection, copy, search, scrolling and
//! fonts work through the child-window paths unchanged. Its one pane has no
//! PTY; the report text is fed straight into the pane's parser.
//!
//! OS figures (footprint, threads, each pane shell's process tree) are sampled
//! on a short-lived `sonicterm-memory-report` thread, so a process-table walk
//! never runs on the UI thread. A refresh bumps the generation and sets the
//! previous walk's cancel flag; a result for an older generation is dropped.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use sonicterm_cfg::keymap::Action;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_ui::ime::ImeState;
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::selection::SelectMode;
use sonicterm_ui::tabs::{Tab, TabBar};
use sonicterm_vt::vt::Parser;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use super::memory_report::{
    report_feed_bytes, report_key, report_route, sanitize_external, MemoryReport, OsFigures,
    OsSample, PaneRow, ReportHeader, ReportKey, ReportRoute, RootRow, WindowRow, COPY_ALL_HINT,
    REPORT_HISTORY_ROWS,
};
use super::memory_snapshot::RendererSummary;
use super::{App, PaneState, TabState, UserEvent, WindowRole, WindowState};

/// Grid size of a headless report pane, which has no renderer to measure.
#[cfg(test)]
const HEADLESS_GRID: (u16, u16) = (100, 40);

/// App-level state of the Memory Usage report.
#[derive(Debug, Default)]
pub(crate) struct MemoryReportState {
    /// `show_memory_usage` asked for a window that the next event-loop turn creates.
    pub(crate) open_requested: bool,
    /// Identity of the newest refresh; an OS sample tagged with another is stale.
    pub(crate) generation: u64,
    /// Cancel flag shared with the newest walk; a refresh or close sets it before replacing it.
    pub(crate) cancel: Arc<AtomicBool>,
    /// The newest report, waiting for its OS figures or already complete.
    pub(crate) current: Option<MemoryReport>,
    /// Exactly the text the report pane shows, which copy-all copies.
    pub(crate) last_text: Option<String>,
    /// Generation of the one walk running now; no second walk starts until it answers.
    pub(crate) in_flight: Option<u64>,
    /// The newest refresh's walk, waiting for the running one to answer; newer requests replace it.
    pub(crate) queued: Option<SampleRequest>,
    /// The current report changed while a selection or copy mode held the pane, so it waits.
    pub(crate) render_deferred: bool,
    /// Test-only: generations whose walk started, in order.
    #[cfg(test)]
    pub(crate) started: Vec<u64>,
    /// Test-only: hold each walk here instead of running it, so a test decides when it answers.
    #[cfg(test)]
    pub(crate) manual_sampler: bool,
    /// Test-only: the walk the manual sampler is holding.
    #[cfg(test)]
    pub(crate) manual_pending: Option<SampleRequest>,
}

/// One OS walk: its refresh generation, the pane roots, and its cancel flag.
#[derive(Debug)]
pub(crate) struct SampleRequest {
    /// The refresh this walk answers.
    pub(crate) generation: u64,
    /// Pane shells to walk, as `(pid, recorded start token)`.
    pub(crate) roots: Vec<(u32, Option<u64>)>,
    /// Set when a newer refresh or a close supersedes this walk.
    pub(crate) cancel: Arc<AtomicBool>,
}

impl App {
    /// The report window, if one is open.
    pub(crate) fn memory_report_window_id(&self) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|(_, window)| window.role == WindowRole::MemoryReport)
            .map(|(id, _)| *id)
    }

    /// Whether `id` is the report window.
    pub(crate) fn is_memory_report_window(&self, id: WindowId) -> bool {
        self.windows.get(&id).is_some_and(|window| window.role == WindowRole::MemoryReport)
    }

    /// Run `show_memory_usage`: focus and refresh an open report, or request one.
    pub(crate) fn show_memory_usage(&mut self) {
        let Some(report_id) = self.memory_report_window_id() else {
            // When: `memory_report_window_id` is None, no report is open, so the next event-loop turn creates one.
            self.memory_report.open_requested = true;
            return;
        };
        if let Some(window) = self.windows.get(&report_id).and_then(|state| state.window.as_ref()) {
            window.focus_window();
        }
        self.frontmost_window = Some(report_id);
        self.refresh_memory_report();
    }

    /// Create the requested report window now that an event loop is available.
    pub(super) fn drain_pending_memory_report(&mut self, event_loop: &ActiveEventLoop) {
        if !std::mem::take(&mut self.memory_report.open_requested) {
            // When: `open_requested` was false, nobody asked for a report window, so there is nothing to create.
            return;
        }
        if self.memory_report_window_id().is_some() {
            // When: `memory_report_window_id` found a window opened since the request; refresh it, never open a second.
            self.refresh_memory_report();
            return;
        }
        self.create_memory_report_window(event_loop);
    }

    /// Build the native window and its renderer, then install and fill the report pane.
    fn create_memory_report_window(&mut self, event_loop: &ActiveEventLoop) {
        let title = self.i18n.translate("memory-report-window-title");
        let request = self.window_request(self.main_window_id);
        let attrs = super::with_app_icon(super::with_backdrop_transparency(
            super::with_integrated_titlebar(
                Window::default_attributes()
                    .with_title(title.clone())
                    .with_visible(false)
                    .with_decorations(true)
                    .with_inner_size(request.inner_size),
            ),
            self.config.appearance.backdrop,
            self.config.appearance.software_render_mode,
        ));
        // The report accepts no drops, so it is never registered as a native drop target.
        let attrs = self.native_drop_attributes(attrs);
        let window = match event_loop.create_window(attrs) {
            Ok(created) => Arc::new(created),
            Err(error) => {
                // When: `create_window` returned an `error`, keep the terminals running and log why.
                tracing::error!("memory report: create_window failed: {error}");
                return;
            }
        };
        window.set_ime_allowed(true);
        let settings = sonicterm_gpu::core::RendererSettings {
            font_family: &self.config.font.family,
            font_dirs: &self.font_dirs,
            font_size: self.config.font.size,
            line_height_mult: self.config.font.line_height,
            font_weight_scale: self.config.font.effective_weight_scale(),
            subpixel_aa: self.config.font.subpixel_aa,
            padding: [
                self.config.window.padding_left,
                self.config.window.padding_right,
                self.config.window.padding_top,
                self.config.window.padding_bottom,
            ],
            appearance: sonicterm_gpu::core::SurfaceAppearance {
                backdrop: self.config.appearance.backdrop,
                opacity: self.config.appearance.opacity,
                scrollbar: self.config.appearance.scrollbar,
                panel_padding: self.config.appearance.panel_padding,
                software_render_mode: self.config.appearance.software_render_mode,
            },
            role: "memory-report",
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        };
        let renderer_result = self.shared_gpu_context().map_or_else(
            || GpuRenderer::new(window.clone(), event_loop, &self.theme, settings),
            |context| {
                GpuRenderer::new_with_shared_context(
                    window.clone(),
                    event_loop,
                    &self.theme,
                    settings,
                    context,
                )
            },
        );
        let mut renderer = match renderer_result {
            Ok(renderer) => renderer,
            Err(error) => {
                // When: `renderer_result` is an `error`, drop the hidden window instead of showing a blank one.
                tracing::error!("memory report: renderer init failed: {error}");
                return;
            }
        };
        if !self.configure_child_renderer(
            &mut renderer,
            &window,
            super::tear_out::ChildRendererOrigin::Fresh,
        ) {
            // When: the surface size was rejected, the grid would be unusable, so drop the window.
            tracing::error!("memory report: initial window size rejected");
            return;
        }
        let window_id = window.id();
        let cells = renderer.cells();
        self.install_memory_report_window(
            window_id,
            Some(window.clone()),
            Some(renderer),
            cells,
            title,
        );
        window.set_visible(true);
        crate::app::frame_counters::request_native_redraw(&window);
    }

    /// Insert a report window with one PTY-less pane, make it frontmost, and fill it.
    pub(super) fn install_memory_report_window(
        &mut self,
        window_id: WindowId,
        window: Option<Arc<Window>>,
        renderer: Option<GpuRenderer>,
        (cols, rows): (u16, u16),
        title: String,
    ) {
        let mut grid = Grid::new(cols, rows);
        grid.set_scrollback_limit(REPORT_HISTORY_ROWS);
        let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
            grid,
            None,
            Arc::clone(&self.capture_staging_pool),
        )));
        {
            let mut guard = crate::app::frame_counters::lock_parser(&parser);
            super::seed_parser_theme_colors(&mut guard, &self.theme);
        }
        let mut pane = PaneState::new_with_media_pool(parser, None, &self.inline_media_pool);
        pane.redraw_target = Arc::new(Mutex::new(Some(window_id)));
        let pane_id = super::next_pane_id();
        let mut panes = HashMap::new();
        panes.insert(pane_id, pane);
        let mut tabs = TabBar::new();
        tabs.push(Tab::new(title.clone()));
        let state = report_window_state(
            window,
            renderer,
            tabs,
            vec![TabState::new(PaneTree::leaf(pane_id), pane_id)],
            panes,
            title,
        );
        self.insert_window_registered(window_id, state);
        self.frontmost_window = Some(window_id);
        // A new window starts from a blank report, never an older window's text.
        self.memory_report.last_text = None;
        self.memory_report.current = None;
        self.refresh_memory_report();
    }

    /// Take a fresh sample: counted figures now, OS figures from the one sampling worker.
    ///
    /// An explicit refresh always replaces the shown report. The previous walk is cancelled, and
    /// if it is still running this refresh's walk waits for it, so at most one walk runs at a time.
    // Ordering: `cancel` is stored Relaxed; it is a standalone stop request that publishes no other memory.
    pub(crate) fn refresh_memory_report(&mut self) {
        if self.memory_report_window_id().is_none() {
            // When: `memory_report_window_id` is None, there is no window to show a sample in.
            return;
        }
        let report = self.collect_memory_report();
        let roots: Vec<(u32, Option<u64>)> =
            report.roots.iter().filter_map(|root| root.pid.map(|pid| (pid, root.start))).collect();
        self.memory_report.generation = self.memory_report.generation.wrapping_add(1);
        let generation = self.memory_report.generation;
        self.memory_report.cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.memory_report.cancel = Arc::clone(&cancel);
        self.memory_report.current = Some(report);
        self.render_current_memory_report();
        self.request_memory_sample(SampleRequest { generation, roots, cancel });
    }

    /// Start `request` now, or queue it behind the running walk, replacing any older queued one.
    fn request_memory_sample(&mut self, request: SampleRequest) {
        if self.memory_report.in_flight.is_some() {
            // When: `in_flight` names a running walk, this request waits; only the newest is kept.
            self.memory_report.queued = Some(request);
            return;
        }
        self.start_memory_sample(request);
    }

    /// Run one walk: on the worker thread, inline when there is no event loop, or held by a test.
    fn start_memory_sample(&mut self, request: SampleRequest) {
        self.memory_report.in_flight = Some(request.generation);
        #[cfg(test)]
        {
            self.memory_report.started.push(request.generation);
            if self.memory_report.manual_sampler {
                // When: `manual_sampler` is set, the test holds the walk and answers it explicitly.
                self.memory_report.manual_pending = Some(request);
                return;
            }
        }
        let SampleRequest { generation, roots, cancel } = request;
        let Some(proxy) = self.event_loop_proxy.clone() else {
            // When: no event-loop proxy exists (headless use), sample inline on this thread.
            let sample = sample_os(&roots, &cancel);
            self.handle_memory_report_sampled(generation, sample);
            return;
        };
        // The worker answers even when cancelled, so `in_flight` always clears and a queued walk starts.
        let spawned =
            std::thread::Builder::new().name("sonicterm-memory-report".into()).spawn(move || {
                let sample = sample_os(&roots, &cancel);
                let _ = proxy.send_event(UserEvent::MemoryReportSampled {
                    generation,
                    sample: Box::new(sample),
                });
            });
        if let Err(error) = spawned {
            // The worker could not start: show the counted figures and say the OS ones failed.
            tracing::warn!("memory report: sampling thread failed to start: {error}");
            self.memory_report.in_flight = None;
            if generation == self.memory_report.generation {
                if let Some(report) = self.memory_report.current.as_mut() {
                    report.os = OsFigures::Failed;
                }
                self.render_or_defer_memory_report();
            }
        }
    }

    /// Accept a walk's answer: free the worker, start any queued walk, and apply a current sample.
    pub(crate) fn handle_memory_report_sampled(&mut self, generation: u64, sample: OsSample) {
        if self.memory_report.in_flight == Some(generation) {
            self.memory_report.in_flight = None;
            if let Some(next) = self.memory_report.queued.take() {
                self.start_memory_sample(next);
            }
        }
        if generation != self.memory_report.generation {
            // When: `generation` is older than the newest refresh, the sample is stale.
            return;
        }
        let Some(report) = self.memory_report.current.as_mut() else {
            // When: the report was dropped (its window closed), there is nothing to complete.
            return;
        };
        report.os = OsFigures::Sampled(sample);
        self.render_or_defer_memory_report();
    }

    /// Show the current report unless a selection, a selection gesture, or copy mode holds the pane.
    ///
    /// A held pane keeps its text, so a selection is never wiped while the user copies it. A press
    /// starts an empty selection with a live gesture before any motion, so the button being down or
    /// a gesture owning the pointer also holds the pane; otherwise the first drag motion would find
    /// the history it anchored to replaced. The report waits in `current` and
    /// [`Self::apply_deferred_memory_report`] shows it later.
    fn render_or_defer_memory_report(&mut self) {
        let held = self.memory_report_window_id().and_then(|id| self.windows.get(&id)).is_some_and(
            |window| {
                window.copy_mode.is_some()
                    || window.mouse_down
                    || window.pointer_gesture.is_some()
                    || window.selection.as_ref().is_some_and(|selection| !selection.is_empty())
            },
        );
        if held {
            // When: `held`, a selection, gesture or copy mode owns the pane, so the new text waits.
            self.memory_report.render_deferred = true;
            return;
        }
        self.memory_report.render_deferred = false;
        self.render_current_memory_report();
    }

    /// Show a report that waited for a selection, gesture or copy mode, once none holds the pane.
    ///
    /// Runs once per event-loop turn and does nothing unless a report is waiting.
    pub(super) fn apply_deferred_memory_report(&mut self) {
        if self.memory_report.render_deferred {
            self.render_or_defer_memory_report();
        }
    }

    /// Render the current report into text and feed it to the report pane.
    fn render_current_memory_report(&mut self) {
        let Some(text) = self.memory_report.current.as_ref().map(MemoryReport::render) else {
            // When: `current` holds no report yet, nothing can be rendered.
            return;
        };
        if self.feed_memory_report(&text) {
            self.memory_report.last_text = Some(text);
        }
    }

    /// Replace the report pane's content with `text`, scrolled to its first line.
    ///
    /// Invalidates the window's retained frame, as a visibility change does: a replacement of
    /// equal length whose changed rows sit in history would otherwise plan as unchanged and leave
    /// the old figures on screen. Returns whether a report pane received the text.
    pub(super) fn feed_memory_report(&mut self, text: &str) -> bool {
        let Some(report_id) = self.memory_report_window_id() else {
            // When: `memory_report_window_id` is None, the report window closed and no pane can show the text.
            return false;
        };
        let scrollbar = self.config.appearance.scrollbar;
        let Some(window) = self.windows.get_mut(&report_id) else {
            // When: the window vanished between the lookup and the borrow.
            return false;
        };
        let Some(pane_id) =
            window.tab_states.get(window.tabs.active_index()).map(|tab| tab.active_pane)
        else {
            // When: the report window has no active tab, there is no pane to fill.
            return false;
        };
        let Some(pane) = window.panes.get_mut(&pane_id) else {
            // When: the active pane is missing, there is no parser to fill.
            return false;
        };
        {
            let mut parser = crate::app::frame_counters::lock_parser(&pane.parser);
            // RIS resets the screen and cursor; CSI 3 J drops the previous report's history.
            let _ = parser.advance(b"\x1bc\x1b[3J");
            let _ = parser.advance(&report_feed_bytes(text));
            parser.grid_mut().mark_all_dirty();
        }
        let _ = pane.release_viewport();
        window.selection = None;
        super::child_window::scroll_child_pane(window, pane_id, i32::MIN, scrollbar);
        window.invalidate_visibility_frame();
        window.request_window_redraw();
        true
    }

    /// Copy exactly the text the report pane shows. Returns whether the clipboard accepted it.
    pub(crate) fn copy_memory_report(&mut self) -> bool {
        let Some(report_id) = self.memory_report_window_id() else {
            // When: `memory_report_window_id` is None, no report window is open, so there is nothing to copy.
            return false;
        };
        let Some(text) = self.memory_report.last_text.clone() else {
            // When: `last_text` is None, nothing has been rendered yet, so there is nothing to copy.
            return false;
        };
        let copied = self.set_clipboard_text(text);
        if copied {
            let message = self.i18n.translate("memory-report-copied");
            self.show_notification_for_kind(
                super::FrontmostKind::Child(report_id),
                sonicterm_ui::overlays::NotificationLevel::Info,
                message,
            );
        }
        copied
    }

    /// Close the report window through the shared child-window teardown. Never quits the app.
    pub(crate) fn close_memory_report_window(&mut self) {
        let Some(report_id) = self.memory_report_window_id() else {
            // When: `memory_report_window_id` is None, the window is already gone; still drop its state.
            self.cancel_memory_report_sampling();
            return;
        };
        self.close_child_window(report_id);
    }

    /// Stop the running walk and drop every report the closed window would have shown.
    ///
    /// `in_flight` is kept: the cancelled walk still answers, which frees the one worker slot.
    // Ordering: `cancel` is stored Relaxed; it is a standalone stop request that publishes no other memory.
    pub(super) fn cancel_memory_report_sampling(&mut self) {
        self.memory_report.cancel.store(true, Ordering::Relaxed);
        self.memory_report.current = None;
        self.memory_report.last_text = None;
        self.memory_report.queued = None;
        self.memory_report.render_deferred = false;
    }

    /// Report-specific teardown, run by every path that removes the report window.
    ///
    /// Called from `close_child_window` (action close, native close button) and
    /// `reap_empty_child` (tab close button); `finish_session` cancels sampling directly.
    pub(super) fn forget_memory_report_window(&mut self, removed: WindowId) {
        self.cancel_memory_report_sampling();
        if self.frontmost_window != Some(removed) {
            // When: `frontmost_window` is another window, focus does not need repairing.
            return;
        }
        self.frontmost_window = self.memory_report_redirect_target();
        if let Some(window) = self
            .frontmost_window
            .and_then(|id| self.windows.get(&id))
            .and_then(|state| state.window.as_ref())
        {
            window.focus_window();
        }
    }

    /// The terminal window an action redirected from the report runs in.
    ///
    /// The visible main window first, then the lowest-id terminal child, then a hidden main.
    pub(crate) fn memory_report_redirect_target(&self) -> Option<WindowId> {
        let main = self.main_window_id.filter(|id| self.windows.contains_key(id));
        if let Some(main) = main.filter(|_| !self.main_is_hidden()) {
            // When: main is live and visible, it is where the user expects terminal actions.
            return Some(main);
        }
        self.windows
            .iter()
            .filter(|(id, window)| {
                window.role == WindowRole::Terminal && Some(**id) != self.main_window_id
            })
            .map(|(id, _)| *id)
            .min()
            .or(main)
    }

    /// Route an action whose source is the report window.
    ///
    /// `from_cached_focus` is true when the caller is the cached-frontmost dispatcher, whose
    /// scroll arm reads only the main window; a scroll from there takes the explicit-source path,
    /// which scrolls the report's own pane. Returns `Some(handled)` when the report consumed,
    /// closed on, or redirected the action, or `None` when it runs on the report normally.
    pub(super) fn gate_memory_report_action(
        &mut self,
        action: &Action,
        source: Option<WindowId>,
        from_cached_focus: bool,
    ) -> Option<bool> {
        let report_id = source.filter(|id| self.is_memory_report_window(*id))?;
        let route = report_route(action);
        let scroll_via_cached_focus = from_cached_focus && matches!(action, Action::Scroll(_));
        match route {
            ReportRoute::Local if scroll_via_cached_focus => {
                // A scroll that came through cached focus takes the explicit-source arm, which scrolls the report pane.
                Some(self.run_action_for_window(action, report_id))
            }
            ReportRoute::Local => None,
            ReportRoute::CloseReport => {
                self.close_memory_report_window();
                Some(true)
            }
            ReportRoute::Consume => Some(true),
            ReportRoute::Redirect => {
                // Run in the redirect target, or consume the action when no terminal window exists.
                let handled = self
                    .memory_report_redirect_target()
                    .map(|target| self.run_action_for_window(action, target));
                Some(handled.unwrap_or(true))
            }
        }
    }

    /// Handle a key in the report window after IME, search and copy mode declined it.
    ///
    /// Copy-all, R and Esc come first; then keymap actions go through the report gate. A key
    /// that matches neither is dropped: the report pane has no PTY, so typing reaches no shell.
    pub(super) fn handle_memory_report_key(
        &mut self,
        win_id: WindowId,
        chords: &[String],
        is_repeat: bool,
    ) {
        match report_key(chords) {
            Some(ReportKey::CopyAll) => {
                // When: CopyAll, copy the whole report; a refused clipboard leaves nothing else to do.
                let _ = self.copy_memory_report();
            }
            Some(ReportKey::Refresh) => {
                // Sample once per fresh press; auto-repeat of R would resample on every tick.
                if !is_repeat {
                    self.refresh_memory_report();
                }
            }
            Some(ReportKey::Close) => self.close_memory_report_window(),
            None => {
                let action = chords.iter().find_map(|chord| self.keymap.lookup(chord).cloned());
                if let Some(action) = action {
                    // A bound chord runs through the report gate; anything else reaches no shell.
                    self.run_action_for_window(&action, win_id);
                }
            }
        }
    }

    /// Gather the counted figures and the row identities for one report.
    pub(super) fn collect_memory_report(&self) -> MemoryReport {
        let (snapshot, measures) = self.build_memory_snapshot_with_panes();
        let graphics = self.build_graphics_reading();
        let retention: HashMap<(WindowId, u64), _> = measures
            .iter()
            .map(|measure| ((measure.window_id, measure.pane_id), measure.retention))
            .collect();
        let report_id = self.memory_report_window_id();
        let mut ordered: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(id, window)| {
                window.role == WindowRole::Terminal && Some(**id) != self.main_window_id
            })
            .map(|(id, _)| *id)
            .collect();
        ordered.sort_unstable();
        if let Some(main) = self.main_window_id.filter(|id| self.windows.contains_key(id)) {
            ordered.insert(0, main);
        }
        let numbered_windows = ordered.len();
        if let Some(report_id) = report_id {
            ordered.push(report_id);
        }
        let mut windows = Vec::new();
        let mut panes = Vec::new();
        let mut roots = Vec::new();
        let mut listed = std::collections::HashSet::new();
        for (window_index, window_id) in ordered.iter().enumerate() {
            let Some(window) = self.windows.get(window_id) else {
                // When: the id was collected from `windows` above, so this cannot miss; skip defensively.
                continue;
            };
            let is_report = Some(*window_id) == report_id;
            let window_label = if is_report {
                "memory report window".to_string()
            } else {
                // When: `is_report` is false, this is a terminal window, numbered in report order.
                format!("window {}", window_index + 1)
            };
            windows.push(WindowRow {
                label: window_label,
                is_report,
                renderer: window.renderer.as_ref().map(|renderer| {
                    summarize_renderer(
                        format!("{window_id:?}"),
                        renderer,
                        window.redraw.trimmed,
                        window.redraw.trim_released_requested_bytes,
                    )
                }),
            });
            let window_prefix = if numbered_windows > 1 && !is_report {
                format!("window {} ", window_index + 1)
            } else {
                // When: `numbered_windows` is one or `is_report` is set, a window number adds nothing.
                String::new()
            };
            for (tab_index, (tab, tab_state)) in
                window.tabs.tabs().iter().zip(window.tab_states.iter()).enumerate()
            {
                let title = sanitize_external(&tab.title);
                for (pane_index, pane_id) in tab_state.tree.leaves().into_iter().enumerate() {
                    listed.insert((*window_id, pane_id));
                    let position =
                        format!("{window_prefix}tab {} pane {}", tab_index + 1, pane_index + 1);
                    panes.push(PaneRow {
                        label: format!(
                            "{window_prefix}tab {} \"{title}\" pane {}",
                            tab_index + 1,
                            pane_index + 1
                        ),
                        is_report,
                        retention: retention.get(&(*window_id, pane_id)).copied().flatten(),
                    });
                    let Some(pty) = window.panes.get(&pane_id).and_then(|pane| pane.pty.as_ref())
                    else {
                        // When: `pane.pty` is None (the report's own pane), there is no shell to walk.
                        continue;
                    };
                    let identity = pty.process_identity();
                    let pid = identity.map(|identity| identity.pid).or_else(|| fallback_pid(pty));
                    roots.push(RootRow {
                        label: position,
                        pid,
                        start: identity.map(|identity| identity.start),
                    });
                }
            }
        }
        // Panes outside every tab tree still count toward the session total, so list them too.
        for measure in &measures {
            if !listed.contains(&(measure.window_id, measure.pane_id)) {
                panes.push(PaneRow {
                    label: format!("pane {} (not in a tab)", measure.pane_id),
                    is_report: Some(measure.window_id) == report_id,
                    retention: measure.retention,
                });
            }
        }
        MemoryReport {
            header: ReportHeader {
                version: env!("CARGO_PKG_VERSION").to_string(),
                timestamp: sonicterm_logging::process_report::local_timestamp(),
                pid: std::process::id(),
                copy_all_hint: COPY_ALL_HINT,
            },
            snapshot,
            graphics,
            windows,
            panes,
            roots,
            os: OsFigures::Pending,
        }
    }

    /// Test-only: answer the walk the manual sampler holds, as its worker would.
    #[cfg(test)]
    pub(crate) fn __test_answer_memory_sample(&mut self) -> Option<u64> {
        let request = self.memory_report.manual_pending.take()?;
        let sample = OsSample {
            footprint: sonicterm_logging::process_report::ProcessFootprint {
                current: sonicterm_logging::process_memory::MemoryMetric::Bytes(
                    request.generation * 1_000_000,
                ),
                current_label: "physical footprint",
                peak: sonicterm_logging::process_memory::MemoryMetric::Unsupported,
                peak_label: "peak",
            },
            uptime_secs: Some(request.generation),
            threads: sonicterm_logging::process_report::ThreadSummary {
                total: None,
                names: Vec::new(),
            },
            trees: Vec::new(),
        };
        self.handle_memory_report_sampled(request.generation, sample);
        Some(request.generation)
    }

    /// Test-only: open a headless report window with no native window or renderer.
    #[cfg(test)]
    pub(crate) fn __test_open_memory_report_window(&mut self) -> WindowId {
        if let Some(existing) = self.memory_report_window_id() {
            // When: `memory_report_window_id` found an open report, focus and refresh it like production.
            self.show_memory_usage();
            return existing;
        }
        self.memory_report.open_requested = false;
        let id = super::window_registry::next_synthetic_child_window_id();
        self.install_memory_report_window(id, None, None, HEADLESS_GRID, "Memory Usage".into());
        id
    }
}

/// The live pid on platforms that record no start token; `None` where one should exist.
fn fallback_pid(pty: &sonicterm_io::pty::PtyHandle) -> Option<u32> {
    // macOS and Windows always record a start token, so a missing identity there means the query
    // failed and a bare pid could name another process; elsewhere the bounded pid read is all there is.
    (!cfg!(any(target_os = "macos", windows))).then(|| pty.pid()).flatten()
}

/// Sample the OS figures for one report; runs on the worker thread.
fn sample_os(roots: &[(u32, Option<u64>)], cancel: &AtomicBool) -> OsSample {
    use sonicterm_logging::process_report as report;
    OsSample {
        footprint: report::sample_footprint(),
        uptime_secs: report::process_uptime_secs(),
        threads: report::sample_threads(cancel),
        trees: report::sample_trees(roots, cancel),
    }
}

/// One renderer's retained storage as the report's renderer row.
fn summarize_renderer(
    label: String,
    renderer: &GpuRenderer,
    trimmed: bool,
    gpu_released_requested_bytes: u64,
) -> RendererSummary {
    let retention = renderer.retained_amounts();
    RendererSummary {
        label,
        role: "visible",
        glyph_atlas: retention.glyph_atlas,
        image_atlas: retention.image_atlas,
        row_glyph_cache: retention.row_glyph_cache,
        row_quad_cache: retention.row_quad_cache,
        software_frame: retention.software_frame,
        vertex_scratch: retention.vertex_scratch,
        row_ink: retention.row_ink,
        frame_scratch: retention.frame_scratch,
        chrome_cache: retention.chrome_cache,
        glyph_atlas_facts: renderer.glyph_atlas_facts(),
        trimmed,
        gpu_released_requested_bytes,
    }
}

/// The report window's state: one tab, one PTY-less pane, its title as the window name.
fn report_window_state(
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    tabs: TabBar,
    tab_states: Vec<TabState>,
    panes: HashMap<u64, PaneState>,
    title: String,
) -> WindowState {
    // Both pacing clocks start at one instant, so neither paces the window differently.
    let created_at = Instant::now();
    WindowState {
        // Registered when the window is inserted.
        owner: None,
        pending_receipts: Vec::new(),
        role: WindowRole::MemoryReport,
        custom_window_name: title,
        window,
        renderer,
        tabs,
        tab_states,
        panes,
        cursor_pos: (0.0, 0.0),
        mouse_down: false,
        pointer_gesture: None,
        selection: None,
        last_click_time: None,
        last_click_cell: (0, 0),
        click_count: 0,
        select_mode: SelectMode::Cell,
        select_anchor: (0, 0),
        copy_mode: None,
        modifiers: ModifiersState::empty(),
        pty_pressed_keys: HashMap::new(),
        last_render: created_at,
        stream_clock: created_at,
        retry_not_before: None,
        visible_frame_invalid: false,
        redraw: Default::default(),
        display_link: Default::default(),
        hover_link: false,
        hover_unresolved: false,
        pressed_tab: None,
        drag_session: None,
        drag_target: None,
        dpi_scale: 1.0,
        ime: ImeState::new(),
        ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
        hovered_url: None,
        link_preview: None,
        path_probe: super::path_target::PathProbeState::default(),
        status_strip: None,
        sel_status: Default::default(),
        ime_discarded_at: None,
        alert_settings: None,
        hover_detection: super::path_target::HoverDetectionCache::default(),
        notification: None,
        hidden: false,
        scrollbar_drag: None,
        splitter_drag: None,
        splitter_hover: None,
        scrollbar_vis: HashMap::new(),
        pending_tear_out_timing: None,
        test_drag_chip_marker: None,
        test_renderer_focus_marker: None,
        test_pane_viewport: None,
        #[cfg(test)]
        test_image_atlas_release: None,
    }
}

#[cfg(test)]
#[path = "memory_report_window_tests.rs"]
mod memory_report_window_tests;
