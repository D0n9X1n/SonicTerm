//! Doc-hidden test hooks for tabs, panes, PTY input, broadcast, and command status.

use super::*;

impl App {
    /// Test seam: queue a command event on a pane without running a shell.
    ///
    /// Lets a test drive command-status and badge behavior from synthetic
    /// events instead of waiting on real process transitions.
    #[doc(hidden)]
    pub fn __test_push_pane_command_event(
        &mut self,
        pane_id: u64,
        event: CommandEvent,
        at: Instant,
        duration: Option<Duration>,
    ) {
        if let Some(pane) = self.main().and_then(|main| main.panes.get(&pane_id)) {
            pane.command_events.lock().push(PaneCommandEvent { event, at, duration });
        }
    }

    /// Test seam: the command status a tab currently reports.
    ///
    /// `None` when no main window or no tab sits at `tab_idx`.
    #[doc(hidden)]
    pub fn __test_command_status_for_tab(&self, tab_idx: usize) -> Option<CommandStatus> {
        self.main_tab_states()?.get(tab_idx).map(|tab| tab.command.clone())
    }

    /// Test seam: the badge a tab would render at `now`.
    ///
    /// Badge text depends on whether the tab is the active one, so this
    /// resolves activeness the same way the tab bar does.
    #[doc(hidden)]
    pub fn __test_tab_badge(&self, tab_idx: usize, now: Instant) -> Option<&'static str> {
        let tabs = self.main_tabs()?;
        tabs.tabs()
            .get(tab_idx)
            .and_then(|tab| tab.command.clone().badge(now, tab_idx == tabs.active_index()))
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
    // Ordering: keyboard_input loads Relaxed as one self-contained modes, Kitty, and epoch snapshot.
    #[doc(hidden)]
    pub fn __test_dispatch_key_or_encode_pty_with_drain(
        &mut self,
        key: &winit::keyboard::Key,
        mods: winit::keyboard::ModifiersState,
        simulate_drain: bool,
    ) -> (Option<Action>, Option<Vec<u8>>) {
        for key_str in key_to_strings(key, mods) {
            if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                // When: `keymap` resolves `key_str` to an action, so binding
                // dispatch is tried before falling back to PTY byte encoding.
                if keymap_dispatch::terminal_input_passthrough_binding(&key_str, &action) {
                    // When: this `action` is a passthrough binding, so the key
                    // belongs to the terminal and the next spelling is tried.
                    continue;
                }
                if self.run_action(&action) {
                    // When: `run_action` consumed the chord, so the caller gets
                    // the action and no encoded bytes reach the PTY.
                    if simulate_drain {
                        self.pending_new_window = None;
                    }
                    return (Some(action), None);
                }
            }
        }
        let snapshot = keyboard_protocol::KeyboardSnapshot::from_bits(
            self.active_pane().map(|pane| pane.keyboard_input.load(Ordering::Relaxed)).unwrap_or(0),
        );
        (None, encode_logical_with_modes(key, mods, snapshot.kitty_flags(), snapshot.modes()))
    }

    /// Test-only: active broadcast source pane, if broadcast is enabled.
    #[doc(hidden)]
    pub fn __test_broadcast_source(&self) -> Option<u64> {
        match self.broadcast {
            BroadcastState::On { source_pane, .. } => Some(source_pane),
            BroadcastState::Off => None,
        }
    }

    /// Test-only: receiver panes under the current broadcast state.
    #[doc(hidden)]
    pub fn __test_broadcast_receivers(&self) -> std::collections::BTreeSet<u64> {
        self.broadcast_receivers()
    }

    /// Test-only: clear the PTY write ledger before a broadcast assertion.
    #[doc(hidden)]
    pub fn __test_enable_pty_write_log(&mut self) {
        self.pty_write_log_enabled = true;
        self.test_pty_writes.lock().clear();
    }

    /// Test-only: snapshot logged `(pane_id, bytes)` PTY writes.
    #[doc(hidden)]
    pub fn __test_pty_write_log(&self) -> Vec<(u64, Vec<u8>)> {
        self.test_pty_writes.lock().clone()
    }

    /// Test-only: drive the same write + broadcast fan-out as normal input.
    #[doc(hidden)]
    pub fn __test_write_to_pane_with_broadcast(&mut self, pane_id: u64, bytes: Vec<u8>) {
        self.write_to_pane(pane_id, bytes.clone(), PtyInputSource::Keyboard);
        self.broadcast_from(pane_id, bytes, PtyInputSource::Keyboard);
    }

    /// Test-only: child render pane ids with the broadcast participant flag that
    /// would be passed into `sonicterm_render_model::PaneRender`.
    #[doc(hidden)]
    pub fn __test_child_broadcast_render_flags(&self, id: WindowId) -> Option<Vec<(u64, bool)>> {
        let child = self.windows.get(&id)?;
        let tab_idx = child.tabs.active_index();
        let panes = child.tab_states.get(tab_idx)?.tree.leaves();
        let participants = self.broadcast_participants();
        Some(panes.into_iter().map(|pane| (pane, participants.contains(&pane))).collect())
    }

    /// Test-only: how many panes the named child window currently owns.
    #[doc(hidden)]
    pub fn __test_child_pane_count(&self, id: WindowId) -> Option<usize> {
        self.windows.get(&id).map(|child| child.panes.len())
    }

    /// Test-only: pane ids owned by the named child window.
    #[doc(hidden)]
    pub fn __test_child_pane_ids(&self, id: WindowId) -> Option<Vec<u64>> {
        self.windows.get(&id).map(|child| child.panes.keys().copied().collect())
    }

    /// Test-only: a pane's parser handle in any window, so a test can hold its lock: to make a memory
    /// sample find that pane contended, or to lock it from a present hook.
    #[doc(hidden)]
    pub fn __test_child_pane_parser(
        &self,
        id: WindowId,
        pane_id: u64,
    ) -> Option<Arc<Mutex<Parser>>> {
        self.windows.get(&id)?.panes.get(&pane_id).map(|pane| Arc::clone(&pane.parser))
    }

    /// Test-only: install the headless pane-viewport seam on the main window
    /// so resize wiring runs without a renderer.
    #[doc(hidden)]
    pub fn __test_set_main_pane_viewport(
        &mut self,
        outer: sonicterm_ui::pane::Rect,
        cell_w: f32,
        cell_h: f32,
    ) -> bool {
        self.__test_synthetic_main();
        self.test_viewport_override = Some((outer, cell_w, cell_h));
        true
    }

    /// Test-only: drive main-window active-tab pane resizing through the same
    /// helper used by production window resize and tab activation.
    #[doc(hidden)]
    pub fn __test_resize_visible_panes(&mut self) {
        self.resize_visible_panes();
    }

    /// Test-only: activate a main tab through the same production helper used
    /// by keyboard/mouse tab activation.
    #[doc(hidden)]
    pub fn __test_invoke_activate_main_tab(&mut self, idx: usize) -> bool {
        self.activate_main_tab(idx)
    }

    /// Test-only: install the headless per-window pane-viewport seam on a child
    /// so the split/close resize wiring runs without a renderer.
    #[doc(hidden)]
    pub fn __test_set_child_pane_viewport(
        &mut self,
        id: WindowId,
        outer: sonicterm_ui::pane::Rect,
        cell_w: f32,
        cell_h: f32,
    ) -> bool {
        match self.windows.get_mut(&id) {
            Some(child) => {
                child.test_pane_viewport = Some((outer, cell_w, cell_h));
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
        child.tab_states.get(tab_idx).map(|tab| tab.active_pane)
    }

    /// Test-only: `true` when the named child pane's scrollbar is currently
    /// inside its idle-visible window (i.e. `mark_active` fired recently).
    /// Used to assert wheel-scroll / view_top jumps light the auto-hide bar
    /// on torn-out windows the same way they do on the main window.
    #[doc(hidden)]
    pub fn __test_child_scrollbar_active(&self, id: WindowId, pane_id: u64) -> Option<bool> {
        let visibility = self.windows.get(&id)?.scrollbar_vis.get(&pane_id)?;
        let idle_ms = match visibility.last_active {
            Some(last_active) => last_active.elapsed().as_millis() as u64,
            None => u64::MAX,
        };
        Some(idle_ms < scrollbar_visibility::IDLE_HIDE_MS)
    }

    /// Test-only: whether the child pane is currently marked as right-edge hovered.
    #[doc(hidden)]
    pub fn __test_child_scrollbar_near_edge(&self, id: WindowId, pane_id: u64) -> Option<bool> {
        self.windows
            .get(&id)?
            .scrollbar_vis
            .get(&pane_id)
            .map(|visibility| visibility.mouse_near_right_edge)
    }

    /// Test-only: clear child scrollbar hover state, mirroring CursorLeft.
    #[doc(hidden)]
    pub fn __test_clear_child_scrollbar_hover(&mut self, id: WindowId) -> bool {
        self.clear_scrollbar_hover_in_child(id)
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
        let at = self
            .windows
            .get(&id)
            .and_then(|window| window.panes.get(&pane_id))
            .map(|pane| viewport_anchor::ViewportBaseline::of(pane.parser.lock().grid()))
            .unwrap_or_default();
        self.set_child_pane_view_top(id, pane_id, view_top, live_top, at);
    }

    /// Test-only: refresh a child window's scrollbar hover state from its last
    /// cursor position, mirroring the production CursorMoved branch.
    #[doc(hidden)]
    pub fn __test_refresh_child_scrollbar_hover_from_cursor(&mut self, id: WindowId) -> bool {
        self.refresh_scrollbar_hover_from_cursor_in_child(id)
    }

    /// Test-only: drain the PTY write ledger populated by `write_to_pane`.
    #[doc(hidden)]
    pub fn __test_drain_pty_writes(&self) -> Vec<(u64, Vec<u8>)> {
        std::mem::take(&mut *self.test_pty_writes.lock())
    }

    /// Test-only: feed bytes into a child pane's parser.
    #[doc(hidden)]
    pub fn __test_advance_child_pane_parser(
        &self,
        id: WindowId,
        pane_id: u64,
        bytes: &[u8],
    ) -> bool {
        let Some(pane) = self.windows.get(&id).and_then(|child| child.panes.get(&pane_id)) else {
            // When: neither `windows` nor its `panes` resolve the request, so the
            // bytes have no parser to advance.
            return false;
        };
        let mut parser = pane.parser.lock();
        parser.advance(bytes);
        pane.__test_publish_input_modes(&parser);
        true
    }

    /// Test-only: clear all dirty row flags for a child pane.
    #[doc(hidden)]
    pub fn __test_clear_child_pane_dirty(&self, id: WindowId, pane_id: u64) -> bool {
        let Some(pane) = self.windows.get(&id).and_then(|child| child.panes.get(&pane_id)) else {
            // When: neither `windows` nor its `panes` resolve the request, so no
            // grid exists whose dirty rows could be cleared.
            return false;
        };
        pane.parser.lock().grid_mut().clear_dirty();
        true
    }

    /// Test-only: count dirty rows for a child pane.
    #[doc(hidden)]
    pub fn __test_child_pane_dirty_count(&self, id: WindowId, pane_id: u64) -> Option<usize> {
        let pane = self.windows.get(&id)?.panes.get(&pane_id)?;
        Some(pane.parser.lock().grid().dirty_count())
    }

    /// Test-only invoker for [`Self::close_active_tab_in_child`]. Exists
    /// because the helper is `pub(super)` and tests live outside the
    /// `app` module tree.
    #[doc(hidden)]
    pub fn __test_invoke_close_active_tab_in_child(&mut self, id: WindowId) -> bool {
        self.close_active_tab_in_child(id)
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

    /// Test-only: the active pane of each tab of window `id`, in tab order.
    #[doc(hidden)]
    pub fn __test_window_tab_panes(&self, id: WindowId) -> Option<Vec<u64>> {
        self.windows
            .get(&id)
            .map(|window| window.tab_states.iter().map(|tab| tab.active_pane).collect())
    }

    /// Test-only: the title the tab bar of window `id` holds for its active tab.
    #[doc(hidden)]
    pub fn __test_window_active_tab_title(&self, id: WindowId) -> Option<String> {
        self.windows.get(&id)?.tabs.active().map(|tab| tab.title.clone())
    }

    /// Test-only: do what pane `pane_id`'s VT worker does with `bytes`, with window `id` as the
    /// pane's redraw target: parse and publish the batch, then flush it. Returns the windows a
    /// `UserEvent::PaneOutput` would have been sent to, which the caller delivers itself; empty
    /// when one is already outstanding for the pane.
    #[doc(hidden)]
    pub fn __test_publish_pane_output(
        &self,
        id: WindowId,
        pane_id: u64,
        bytes: &[u8],
    ) -> Vec<WindowId> {
        let Some(pane) = self.windows.get(&id).and_then(|window| window.panes.get(&pane_id)) else {
            // When: window `id` holds no pane `pane_id`, there is no worker to imitate.
            return Vec::new();
        };
        *pane.redraw_target.lock() = Some(id);
        let handles = super::spawn_pane::PaneVtHandles::from_pane_state(pane);
        super::spawn_pane::process_pane_vt_batch_and_publish(
            &handles,
            bytes,
            &mut None,
            &mut super::spawn_pane::SyncLatch::default(),
            None,
            |_| {},
        );
        let mut queued = Vec::new();
        super::spawn_pane::send_output_redraw(
            &pane.redraw_target,
            &pane.output_outstanding,
            pane.frame_counters.as_ref(),
            Instant::now,
            |window| {
                queued.push(window);
                true
            },
        );
        queued
    }

    /// Test-only: a stand-in for pane `pane_id`'s VT worker in window `id`, keeping the worker's
    /// flush decision across batches; `None` when the window has no such pane.
    #[doc(hidden)]
    pub fn __test_pane_worker(&self, id: WindowId, pane_id: u64) -> Option<TestPaneWorker> {
        let pane = self.windows.get(&id)?.panes.get(&pane_id)?;
        *pane.redraw_target.lock() = Some(id);
        let handles = super::spawn_pane::PaneVtHandles::from_pane_state(pane);
        let flush = super::spawn_pane::OutputFlush::new(pane_id, &handles);
        Some(TestPaneWorker { handles, flush })
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
    /// tests can exercise the production close path — including the
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
        self.main_tabs().map(|tabs| tabs.len()).unwrap_or(0)
    }

    /// tests exercise tab/pane bookkeeping without spawning shells.
    #[doc(hidden)]
    pub fn __test_seed_tab(&mut self, title: &str) -> u64 {
        // ensure the synthetic main WindowState
        // entry exists before seeding. Future PRs B2b/c/d delete the
        // App.tabs/tab_states/panes fields outright, so seed writes
        // MUST land in `self.main_mut()` to survive that migration.
        self.__test_synthetic_main();
        let pane_id = next_pane_id();
        let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
            Grid::new(80, 24),
            None,
            Arc::clone(&self.capture_staging_pool),
        )));
        let media_pool = Arc::clone(&self.inline_media_pool);
        if let Some(main) = self.main_mut() {
            main.panes.insert(pane_id, PaneState::new_with_media_pool(parser, None, &media_pool));
            main.tabs.push(Tab::new(title));
            main.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        pane_id
    }

    /// Test-only: seed a tab like [`Self::__test_seed_tab`] whose pane carries the App's real
    /// counter handles, as a spawned pane does. Attaching them seals the frame-counter gate, so a
    /// test that forces counters on must do so before calling this.
    #[doc(hidden)]
    pub fn __test_seed_counting_tab(&mut self, title: &str) -> u64 {
        let pane_id = self.__test_seed_tab(title);
        let frame_counters = self.pane_frame_counters();
        if let Some(pane) = self.main_mut().and_then(|main| main.panes.get_mut(&pane_id)) {
            pane.frame_counters = frame_counters;
        }
        pane_id
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
        let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
            Grid::new(80, 24),
            Some(tx),
            Arc::clone(&self.capture_staging_pool),
        )));
        let media_pool = Arc::clone(&self.inline_media_pool);
        if let Some(main) = self.main_mut() {
            main.panes.insert(pane_id, PaneState::new_with_media_pool(parser, None, &media_pool));
            main.tabs.push(Tab::new(title));
            main.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        (pane_id, rx)
    }

    /// Test-only: seed an existing synthetic pane parser with the app's
    /// current theme defaults. Mirrors the production spawn path without
    /// requiring a live PTY or reply-forwarder thread.
    #[doc(hidden)]
    pub fn __test_seed_pane_theme_colors(&mut self, pane_id: u64) -> bool {
        let Some(pane) = self.main().and_then(|main| main.panes.get(&pane_id)) else {
            // When: `pane_id` resolves to no pane, so there is no parser whose
            // theme reply slots could be seeded.
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
        let Some(pane) = self.main().and_then(|main| main.panes.get(&pane_id)) else {
            // When: `pane_id` resolves to no pane, so the `bytes` have no parser
            // to advance and are dropped rather than misrouted.
            return false;
        };
        let mut parser = pane.parser.lock();
        parser.advance(bytes);
        pane.__test_publish_input_modes(&parser);
        true
    }

    /// Test-only: read-only access to the internal panes map so tests
    /// can assert "this pane id is gone after detach".
    #[doc(hidden)]
    pub fn __test_pane_ids(&self) -> Vec<u64> {
        self.main().map(|main| main.panes.keys().copied().collect()).unwrap_or_default()
    }

    /// Test-only: read a pane's current `viewport_top_abs`. Used
    /// scrollback-scroll wiring tests to assert wheel + Scroll-keymap
    /// dispatch actually mutates the canonical field.
    #[doc(hidden)]
    pub fn __test_pane_viewport_top_abs(&self, pane_id: u64) -> Option<Option<u64>> {
        self.main()?.panes.get(&pane_id).map(|pane| pane.viewport_top_abs)
    }

    /// Test-only: synthesize scrollback by feeding `line_count` numbered lines and
    /// returns the resulting `scrollback_len()`. Each line is 4 chars +
    /// CRLF so callers can predict the row count.
    #[doc(hidden)]
    pub fn __test_grow_pane_scrollback(&self, pane_id: u64, line_count: u32) -> u64 {
        let Some(pane) = self.main().and_then(|main| main.panes.get(&pane_id)) else {
            // When: `pane_id` resolves to no pane, so no scrollback was grown and
            // the reported row count is zero.
            return 0;
        };
        let mut buf = Vec::with_capacity((line_count as usize) * 8);
        for line_number in 0..line_count {
            use std::io::Write;
            let _ = write!(&mut buf, "{:04}\r\n", line_number % 10_000);
        }
        let mut parser = pane.parser.lock();
        parser.advance(&buf);
        parser.grid().scrollback_len() as u64
    }

    /// Test-only: viewport rows of a pane.
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
        self.main_tab_states()?.get(tab_idx).map(|tab| tab.active_pane)
    }

    /// Test-only: set the active pane in `tab_idx` to `pane_id`. The
    /// click-to-focus logic in `window_event.rs` is the production
    /// caller; tests exercise the same state transition without
    /// driving a synthetic winit `MouseInput` event.
    #[doc(hidden)]
    pub fn __test_set_active_pane(&mut self, tab_idx: usize, pane_id: u64) -> bool {
        if let Some(tab) =
            self.main_tab_states_mut().and_then(|tab_states| tab_states.get_mut(tab_idx))
        {
            tab.active_pane = pane_id;
            true
        } else {
            // When: `main_tab_states_mut` resolves no entry at `tab_idx`, so no
            // tab exists whose focus could be repointed.
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
        self.main_tabs().map(|tabs| tabs.len()).unwrap_or(0)
    }

    /// Test-only: number of leaf panes in the given tab. Returns
    /// `None` when the tab index is out of range. Used by the
    /// `close_pane_or_tab_semantics` regression suite to assert that
    /// `Action::CloseActivePaneOrTab` shrinks the active tab's pane
    /// tree rather than the tab bar when the tab still has > 1 pane.
    #[doc(hidden)]
    pub fn __test_pane_count_in_tab(&self, tab_idx: usize) -> Option<usize> {
        self.main_tab_states()?.get(tab_idx).map(|tab| tab.tree.leaves().len())
    }

    /// Test-only: borrow the redraw target Arc for a given pane id,
    /// so a test can assert the per-pane redraw indirection survives
    /// state transfers.
    #[doc(hidden)]
    pub fn __test_pane_redraw_target(&self, id: u64) -> Option<Arc<Mutex<Option<WindowId>>>> {
        self.main()?.panes.get(&id).map(|pane| pane.redraw_target.clone())
    }

    /// Test-only: install or clear a pane's PTY handle so tear-out tests
    /// can verify ownership moves without spawning a real shell.
    #[doc(hidden)]
    pub fn __test_set_pane_pty(&mut self, id: u64, pty: Option<PtyHandle>) -> bool {
        let Some(pane) = self.main_mut().and_then(|main| main.panes.get_mut(&id)) else {
            // When: `id` resolves to no pane, so the supplied `pty` has no owner
            // and is dropped instead of installed.
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
}

/// Test-only: one pane's VT worker without its thread. Each batch runs the production publisher,
/// `publish_pane_vt_batch_with`, then the worker's flush decision, so the pane's synchronized-output
/// state is published exactly as the worker publishes it.
#[doc(hidden)]
pub struct TestPaneWorker {
    handles: super::spawn_pane::PaneVtHandles,
    flush: super::spawn_pane::OutputFlush,
}

impl TestPaneWorker {
    /// Parse and publish `bytes` at `at`, then decide as the worker does; returns the windows a
    /// `UserEvent::PaneOutput` would have been sent to, which the caller delivers itself.
    #[doc(hidden)]
    pub fn batch(&mut self, bytes: &[u8], at: Instant) -> Vec<WindowId> {
        self.flush.receive(bytes.len(), at);
        super::spawn_pane::publish_pane_vt_batch_with(
            &self.handles,
            bytes,
            &mut None,
            &mut self.flush.sync_latch,
            super::media::decode_inline_image,
            |_| {},
            || at,
            |_| {},
        );
        let handles = &self.handles;
        let mut queued = Vec::new();
        self.flush.after_batch(handles, at, || {
            handles.send_output_with(|window| {
                queued.push(window);
                true
            });
        });
        queued
    }
}
