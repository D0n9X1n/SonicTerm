//! Deferred terminal-window creation and tear-out queue installation.

use super::*;

impl App {
    pub(in crate::app) fn window_request(
        &self,
        source: Option<WindowId>,
    ) -> super::super::WindowRequest {
        let inherited = source
            .and_then(|id| self.windows.get(&id))
            .and_then(|state| state.window.as_ref())
            .and_then(|window| {
                super::super::inherited_window_size(
                    window.inner_size(),
                    window.scale_factor(),
                    window.is_minimized().unwrap_or(false),
                )
            });
        super::super::WindowRequest {
            inner_size: inherited.unwrap_or_else(|| {
                super::super::configured_window_size(&self.config, self.tab_bar_visible)
            }),
        }
    }

    pub(in crate::app) fn drain_pending_window_creates(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(request) = self.pending_new_window.take() {
            self.create_new_terminal_window(event_loop, request);
        }
        self.drain_pending_memory_report(event_loop);
        self.drain_pending_about(event_loop);
        // In-process tear-out drain. The `Command::new`-based spawn
        // (`spawn_tearout_child` + `--tear-out-payload`) is still reached from
        // the Windows OLE drop path, so both routes exist.
        // This drain MUST stay before `drain_pending_os_teardown`, so
        // `cancel_drag_session` sees the new child window already inserted.
        if let Some(req) = self.pending_tear_out.take() {
            self.drain_pending_tear_out(event_loop, req);
        }
    }

    /// Resolve a queued tear-out into a native window while the event loop can
    /// create one. The live tab, pane graph, and PTY move without restarting.
    /// Where the tab a queued tear-out names currently sits.
    ///
    /// The index in the request was recorded when the gesture began, and a tab
    /// closing at a lower index since then leaves it *in range* while naming a
    /// different tab — a bounds check cannot tell the two apart, so trusting it
    /// tears out whichever tab inherited the slot. Resolving through the
    /// recorded id instead follows the tab, and returns `None` when it is gone
    /// so the operation fails rather than promoting a neighbour.
    ///
    /// Split out of the drain so it can be tested: the drain itself needs an
    /// `ActiveEventLoop`, which exists only inside a running winit loop.
    pub(in crate::app) fn resolve_tear_out_source_index(
        &self,
        req: &crate::app::PendingTearOut,
    ) -> Option<usize> {
        match req.source_tab_id {
            Some(id) => self.tab_index_of_id(req.source_window, id),
            // Requests built before an id was available keep the old
            // behaviour rather than refusing outright.
            None => Some(req.source_tab_idx),
        }
    }

    pub(super) fn drain_pending_tear_out(
        &mut self,
        event_loop: &ActiveEventLoop,
        req: crate::app::PendingTearOut,
    ) {
        self.drain_pending_tear_out_with_installer(req, |app, transaction, screen_pos, source| {
            app.install_torn_out_window(event_loop, transaction, screen_pos, source)
        });
    }

    /// Run a deferred tear-out with only destination installation injected.
    pub(in crate::app) fn drain_pending_tear_out_with_installer<I>(
        &mut self,
        req: crate::app::PendingTearOut,
        install: I,
    ) where
        I: FnOnce(
            &mut App,
            super::super::tear_out::DetachedTab,
            Option<(i32, i32)>,
            &'static str,
        ) -> Option<WindowId>,
    {
        let source_is_main = Some(req.source_window) == self.main_window_id;
        let source_tab_idx = match self.resolve_tear_out_source_index(&req) {
            Some(idx) => idx,
            None => {
                // When: `resolve_tear_out_source_index` returns `None`, abandon
                // rather than tear out whichever tab inherited the old slot.
                tracing::warn!(
                    source = ?req.source_window,
                    recorded_idx = req.source_tab_idx,
                    "deferred tear-out source tab closed before the drop"
                );
                return;
            }
        };
        let source = if source_is_main {
            super::super::tear_out::TearOutSource::Main(req.source_window)
        } else {
            // When: `source_is_main` is false, retain the child identity so
            // rollback cannot restore this transaction into the main window.
            super::super::tear_out::TearOutSource::Child(req.source_window)
        };
        let Some(transaction) = self.detach_for_tear_out(source, source_tab_idx) else {
            // When: the source vanished after identity resolution, no payload
            // remains to install or restore.
            tracing::warn!(
                source = ?req.source_window,
                idx = source_tab_idx,
                "deferred tear-out source tab no longer exists"
            );
            return;
        };
        let source_label = if source_is_main { "main" } else { "child" };
        if install(self, transaction, req.drop_screen_pos, source_label).is_none() {
            // When: installation failed, the transaction has already restored
            // its source; success-only source cleanup must not run.
            return;
        }
        if source_is_main {
            self.tear_out_apply_source_side(source_tab_idx);
        } else {
            // When: `source_is_main` is false after commit, apply the child
            // source policy, which reaps only when no tabs remain.
            self.tear_out_apply_child_source_side(req.source_window, source_tab_idx);
        }
        tracing::info!(
            source = ?req.source_window,
            at = ?req.drop_screen_pos,
            "in-process tear-out completed"
        );
    }

    /// Drain a deferred `cancel_drag_session` request raised by
    /// `handle_os_drag_ended` on the `DroppedOnEmpty` branch. Callers MUST
    /// invoke this AFTER [`Self::drain_pending_window_creates`] so any
    /// tear-out-spawn has produced its new window before cross-window
    /// drag-residue cleanup mutates `self.windows`. The all-windows
    /// loop inside `cancel_drag_session` still runs UNCONDITIONALLY
    /// when this drain fires, preserving that cleanup's idempotence —
    /// the flag controls WHEN, not WHETHER.
    pub(in crate::app) fn drain_pending_os_teardown(&mut self) {
        if self.pending_os_teardown {
            self.pending_os_teardown = false;
            self.cancel_drag_session();
        }
    }

    /// Create a fresh top-level terminal window, install its renderer,
    /// spawn one tab + PTY-backed pane, register it with the OS-drag
    /// backend, and mark it as the new frontmost window.
    ///
    /// The renderer shares the live GPU device that [`Self::shared_gpu_context`]
    /// selects, and opens its own device only when no renderer exists yet.
    ///
    /// It must work whether or not `self.windows` is empty, so it must not
    /// assume that another terminal window exists.
    pub(in crate::app) fn create_new_terminal_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        request: super::super::WindowRequest,
    ) {
        use sonicterm_ui::tabs::Tab;

        let attrs = super::super::with_app_icon(super::super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title(super::super::NATIVE_WINDOW_TITLE)
                    .with_visible(false)
                    .with_decorations(true)
                    .with_inner_size(request.inner_size),
            ),
            self.config.appearance.backdrop,
            self.config.appearance.software_render_mode,
        ));
        let attrs = self.native_drop_attributes(attrs);
        let window = match event_loop.create_window(attrs) {
            Ok(created) => Arc::new(created),
            Err(error) => {
                // When: create_window is refused by the OS; log and leave the
                // existing windows running rather than aborting the process.
                tracing::error!("Action::NewWindow: create_window failed: {error}");
                return;
            }
        };
        window.set_ime_allowed(true);
        // A dormant adapter must exist before the window is first shown.
        self.install_accessibility(event_loop, &window);

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
            role: "child",
            glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
        };
        // Share the live device, as warm-pool and tear-out windows do; only a
        // process with no renderer yet opens one here.
        let shared_gpu = self.shared_gpu_context();
        let renderer_result = shared_gpu.map_or_else(
            || GpuRenderer::new(window.clone(), event_loop, &self.theme, settings),
            |ctx| {
                GpuRenderer::new_with_shared_context(
                    window.clone(),
                    event_loop,
                    &self.theme,
                    settings,
                    ctx,
                )
            },
        );
        let mut renderer = match renderer_result {
            Ok(renderer) => renderer,
            Err(error) => {
                // When: renderer_result is Err from either GpuRenderer path; return before
                // the window is registered, so it closes instead of showing nothing forever.
                tracing::error!("Action::NewWindow: renderer init failed: {error}");
                return;
            }
        };
        if !self.configure_child_renderer(
            &mut renderer,
            &window,
            super::super::tear_out::ChildRendererOrigin::Fresh,
        ) {
            // When: configure_child_renderer rejects the surface size; drop the
            // window rather than register one whose cell grid would be unusable.
            let real_inner = window.inner_size();
            tracing::error!(
                width = real_inner.width,
                height = real_inner.height,
                "Action::NewWindow rejected unsafe initial size"
            );
            return;
        }

        let win_id = window.id();
        if let Err(error) = self.register_window_with_os_drag_backend(win_id, &window) {
            // When: register_window_with_os_drag_backend returns error, retire the hidden destination before spawning its shell.
            tracing::error!(?win_id, %error, "new window native drop-target registration failed");
            return;
        }
        let (cols, rows) = renderer.cells();
        let pane_id = super::super::next_pane_id();
        // New windows start from shell defaults rather than inheriting another window's directory.
        let pane_state = self.spawn_pane_state_for_child(
            pane_id,
            cols,
            rows,
            window.clone(),
            &super::super::pane_launch::PaneLaunch::default(),
        );
        let mut panes = HashMap::new();
        panes.insert(pane_id, pane_state);

        let mut tabs = TabBar::new();
        tabs.push(Tab::new("shell 1".to_string()));

        // Both pacing clocks start at one instant, so neither paces a new window differently.
        let created_at = Instant::now();
        let child = WindowState {
            // Registered when the window is inserted; construction has no
            // governor in scope.
            owner: None,
            pending_receipts: Vec::new(),
            role: crate::app::WindowRole::Terminal,
            custom_window_name: String::new(),
            native_title: Default::default(),
            window: Some(window.clone()),
            renderer: Some(renderer),
            tabs,
            tab_states: vec![TabState::new(PaneTree::leaf(pane_id), pane_id)],
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
            pty_pressed_keys: std::collections::HashMap::new(),
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
            path_probe: super::super::path_target::PathProbeState::default(),
            status_strip: None,
            sel_status: Default::default(),
            ime_discarded_at: None,
            alert_settings: None,
            hover_detection: super::super::path_target::HoverDetectionCache::default(),
            notification: None,
            hidden: false,
            scrollbar_drag: None,
            splitter_drag: None,
            splitter_hover: None,
            scrollbar_vis: std::collections::HashMap::new(),
            pending_tear_out_timing: None,
            test_drag_chip_marker: None,
            test_renderer_focus_marker: None,
            test_pane_viewport: None,
            #[cfg(test)]
            test_image_atlas_release: None,
            #[cfg(test)]
            test_strip_settings: None,
        };
        self.insert_window_registered(win_id, child);
        window.set_visible(true);
        crate::app::frame_counters::request_native_redraw(&window);
        // Eagerly mark frontmost so the next Cmd+T / Cmd+W routes
        // here before the OS Focus event arrives — mirrors the
        // tear_out_tab pattern.
        self.frontmost_window = Some(win_id);
        tracing::info!(
            "Action::NewWindow: spawned terminal window; windows={}",
            self.windows.len()
        );
    }
}

#[cfg(test)]
#[path = "misc_window_create_tests.rs"]
mod misc_window_create_tests;
