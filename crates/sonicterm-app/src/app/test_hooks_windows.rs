//! Doc-hidden test hooks for window topology, visibility, focus, drag state, and redraw
//! bookkeeping.

use super::*;

impl App {
    /// Test-only: `true` if `win_id` has a deferred redraw queued in
    /// [`Self::pending_redraw_windows`] (the child-window coalescing latch).
    #[doc(hidden)]
    pub fn __test_child_redraw_deferred(&self, win_id: WindowId) -> bool {
        self.pending_redraw_windows.contains(&win_id)
    }

    /// Test-only: report whether any live window has unconsumed owner-local input.
    #[doc(hidden)]
    pub fn __test_input_dirty(&self) -> bool {
        self.windows.values().any(|window| window.redraw.input_pending())
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

    /// Test-only: read the deferred-exit flag, which a quit action or a close
    /// that leaves no active terminal window sets.
    #[doc(hidden)]
    pub fn __test_pending_exit(&self) -> bool {
        self.pending_exit
    }

    /// Test-only: force-set the main window's `hidden` latch so
    /// post-merge drain-policy tests can simulate the "main already
    /// retired" state without driving a real winit close event.
    #[doc(hidden)]
    pub fn __test_set_main_hidden(&mut self, hidden: bool) {
        self.__test_synthetic_main();
        if let Some(main) = self.main_mut() {
            main.hidden = hidden;
        }
    }

    /// Test-only: how many tabs the named child window currently owns.
    #[doc(hidden)]
    pub fn __test_child_tab_count(&self, id: WindowId) -> Option<usize> {
        self.windows.get(&id).map(|child| child.tabs.len())
    }

    /// Test-only: set the last cursor position for a synthetic child window.
    #[doc(hidden)]
    pub fn __test_set_child_cursor_pos(
        &mut self,
        id: WindowId,
        cursor_x_px: f64,
        cursor_y_px: f64,
    ) -> bool {
        match self.windows.get_mut(&id) {
            Some(child) => {
                child.cursor_pos = (cursor_x_px, cursor_y_px);
                true
            }
            None => false,
        }
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
            let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
                Grid::new(80, 24),
                None,
                Arc::clone(&self.capture_staging_pool),
            )));
            panes.insert(
                pane_id,
                PaneState::new_with_media_pool(parser, None, &self.inline_media_pool),
            );
            tabs.push(Tab::new(*title));
            tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        }
        let child = WindowState {
            // Registered when the window is inserted.
            owner: None,
            role: WindowRole::Terminal,
            custom_window_name: String::new(),
            window: None,
            renderer: None,
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
            last_render: Instant::now(),
            retry_not_before: None,
            visible_frame_invalid: false,
            redraw: Default::default(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            link_preview: None,
            path_probe: path_target::PathProbeState::default(),
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
        };
        self.insert_window_registered(id, child);
        id
    }

    /// Test-only: inspect drag-gesture residue on a specific
    /// child window so an integration test can assert
    /// [`Self::cancel_drag_session`] clears EVERY window's state, not
    /// just the main one.
    #[doc(hidden)]
    pub fn __test_child_pressed_tab(&self, id: WindowId) -> Option<Option<usize>> {
        self.windows.get(&id).map(|child| child.pressed_tab)
    }

    /// Test seam: whether a window is tracking a held mouse button.
    ///
    /// `None` when `id` names no tracked window, which distinguishes an
    /// unknown window from one with no button held.
    #[doc(hidden)]
    pub fn __test_child_mouse_down(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|window| window.mouse_down)
    }

    /// Test seam: whether a window has a tab drag in progress.
    ///
    /// `None` when `id` names no tracked window.
    #[doc(hidden)]
    pub fn __test_child_has_drag_session(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|window| window.drag_session.is_some())
    }

    /// Test seam: whether a window is a drop target for the current drag.
    ///
    /// `None` when `id` names no tracked window.
    #[doc(hidden)]
    pub fn __test_child_has_drag_target(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).map(|window| window.drag_target.is_some())
    }

    /// Test-only: seed the headless drag-chip
    /// marker on a window so a subsequent [`Self::cancel_drag_session`]
    /// can be observed to have cleared it. Returns `false` if the window
    /// id is unknown. The marker is the cross-platform stand-in for
    /// `renderer.set_drag_chip(_)` on `renderer: None` test windows —
    /// production code flips it in the same loop iteration as the real
    /// renderer call, so the assertion fails if the per-window iteration
    /// is ever removed.
    #[doc(hidden)]
    pub fn __test_set_window_drag_chip_marker(&mut self, id: WindowId, present: bool) -> bool {
        if let Some(window) = self.windows.get_mut(&id) {
            window.test_drag_chip_marker = Some(present);
            true
        } else {
            // When: `windows` tracks no entry for this id, so no drag-chip marker
            // could be seeded and the caller is told the seam did nothing.
            false
        }
    }

    /// Test-only: read the drag-chip marker for
    /// a window. `None` ⇒ window absent OR marker never seeded;
    /// `Some(true)` ⇒ marker set & not yet cleared by cancel;
    /// `Some(false)` ⇒ marker was set and cancel ran on this window.
    #[doc(hidden)]
    pub fn __test_window_drag_chip_marker(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).and_then(|window| window.test_drag_chip_marker)
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

    /// Test-only: seed drag-gesture residue on a specific child
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
        let Some(child) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so there is no child
            // state to seed drag residue onto.
            return false;
        };
        child.pressed_tab = pressed_tab;
        child.mouse_down = mouse_down;
        if with_drag_session {
            child.drag_session = child
                .tabs
                .tabs()
                .get(pressed_tab.unwrap_or(0))
                .map(|tab| crate::tab_drag::DragSession::new(id, tab.id, (0.0, 0.0)));
        }
        true
    }

    /// Test-only: install a frontmost child id without going through a
    /// real `WindowEvent::Focused(true)` (which requires a winit window).
    /// `frontmost_window` subsumes a separate focused-child field;
    /// this kept the old name so the existing regression tests don't
    /// need touching, but it now drives the unified tracker.
    #[doc(hidden)]
    pub fn __test_set_focused_child(&mut self, id: Option<WindowId>) {
        self.__test_synthetic_main();
        self.frontmost_window = id;
    }

    /// Test-only: read back the current frontmost-child id.
    /// returns `Some(id)` when `frontmost_window` points
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
    /// Used by regression tests to assert that
    /// keymap-dispatched actions route to the right window's tab vec.
    #[doc(hidden)]
    pub fn __test_set_frontmost_window(&mut self, id: Option<WindowId>) {
        self.frontmost_window = id;
    }

    /// Test seam: give a tracked window a live winit window and renderer.
    ///
    /// Lets a test promote a synthetic headless entry into one that can render,
    /// without going through real window creation. `false` when `id` is unknown.
    #[doc(hidden)]
    pub fn __test_attach_window_renderer(
        &mut self,
        id: WindowId,
        window: Arc<Window>,
        renderer: GpuRenderer,
    ) -> bool {
        let Some(state) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so there is no state to
            // hold the window handle or its renderer.
            return false;
        };
        state.window = Some(window);
        state.renderer = Some(renderer);
        true
    }

    /// Test seam: the characters a window's last drawn frame showed as tofu, so a test can
    /// redraw until non-blocking font fallback has resolved the ones it inspects.
    #[doc(hidden)]
    pub fn __test_window_missing_tofu(&self, id: WindowId) -> Option<Vec<char>> {
        Some(self.windows.get(&id)?.renderer.as_ref()?.last_missing_tofu().to_vec())
    }

    /// Test seam: the pane targeted by a window's real renderer flash state.
    #[doc(hidden)]
    pub fn __test_window_pane_focus_flash_target(&self, id: WindowId) -> Option<u64> {
        self.windows.get(&id)?.renderer.as_ref()?.__test_pane_focus_flash_target()
    }

    /// Test seam: one pixel of a window's software-rendered frame, as BGRA.
    ///
    /// Lets a test assert what the CPU rasterizer actually produced. `None`
    /// when the window is unknown, has no renderer, or the frame is absent.
    #[cfg(target_os = "windows")]
    #[doc(hidden)]
    pub fn __test_window_software_frame_pixel_bgra(
        &self,
        id: WindowId,
        pixel_x: u32,
        pixel_y: u32,
    ) -> Option<[u8; 4]> {
        self.windows.get(&id)?.renderer.as_ref()?.__test_software_frame_pixel_bgra(pixel_x, pixel_y)
    }

    /// Test seam: force the no-GPU degrade path on or off.
    ///
    /// Bypasses runtime detection so a test can exercise software-render
    /// pacing on a machine that has a working GPU.
    #[doc(hidden)]
    pub fn __test_set_software_render_degrade(&mut self, degrade: bool) {
        self.software_render_degrade = degrade;
    }

    /// Test seam: whether the main window has a redraw waiting on the gate.
    ///
    /// Lets a test assert that a redraw was coalesced rather than drawn.
    #[doc(hidden)]
    pub fn __test_main_redraw_deferred(&self) -> bool {
        self.pending_redraw
    }

    /// Test seam: observe a window's frame timestamp independently of its contention deadline.
    #[doc(hidden)]
    pub fn __test_window_last_render(&self, id: WindowId) -> Option<Instant> {
        self.windows.get(&id).map(|window| window.last_render)
    }

    /// Test seam: backdate a window's last-render instant.
    ///
    /// Frame pacing measures elapsed time since the last render, so moving
    /// this lets a test cross a frame boundary without waiting. `false` when
    /// `id` is unknown.
    #[doc(hidden)]
    pub fn __test_set_window_last_render(&mut self, id: WindowId, last_render: Instant) -> bool {
        let Some(state) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so no render timestamp
            // exists to backdate.
            return false;
        };
        state.last_render = last_render;
        true
    }

    /// Test seam: a window's cell width, cell height, and top inset.
    ///
    /// These are the metrics pane layout divides by, so a test can check
    /// geometry against production's planned active-pane origin. Returns `None`
    /// before layout or after its geometry is invalidated.
    #[doc(hidden)]
    pub fn __test_window_cell_geometry(&self, id: WindowId) -> Option<(f32, f32, f32)> {
        let window = self.windows.get(&id)?;
        let renderer = window.renderer.as_ref()?;
        let pane = window.tab_states.get(window.tabs.active_index())?.active_pane;
        let [_, top] = renderer.pane_grid_origin(pane)?;
        let (cell_w, cell_h) = renderer.cell_size();
        Some((cell_w, cell_h, top))
    }

    /// Test-only: read whether the main window is in read-only copy mode.
    #[doc(hidden)]
    pub fn __test_main_read_only(&self) -> bool {
        self.main().and_then(|main| main.copy_mode.as_ref()).is_some_and(|mode| mode.is_read_only())
    }

    /// Test-only: read whether a child window is in read-only copy mode.
    #[doc(hidden)]
    pub fn __test_child_read_only(&self, id: WindowId) -> Option<bool> {
        self.windows
            .get(&id)
            .map(|child| child.copy_mode.as_ref().is_some_and(|mode| mode.is_read_only()))
    }

    /// Test-only: seed the headless renderer-focus marker for a child window.
    #[doc(hidden)]
    pub fn __test_set_child_renderer_focus_marker(&mut self, id: WindowId, focused: bool) -> bool {
        let Some(child) = self.windows.get_mut(&id) else {
            // When: `windows` tracks no entry for this id, so no child carries the
            // renderer-focus marker to update.
            return false;
        };
        child.test_renderer_focus_marker = Some(focused);
        true
    }

    /// Test-only: read the headless renderer-focus marker for a child window.
    #[doc(hidden)]
    pub fn __test_child_renderer_focus_marker(&self, id: WindowId) -> Option<bool> {
        self.windows.get(&id).and_then(|child| child.test_renderer_focus_marker)
    }

    /// Test-only: invoke the child focus transition handler without constructing
    /// a winit `ActiveEventLoop`.
    #[doc(hidden)]
    pub fn __test_handle_child_focus_changed(&mut self, id: WindowId, focused: bool) {
        self.handle_window_focus_changed(id, focused);
    }

    /// Test-only invoker for [`Self::reap_empty_child`]. Pins
    /// `App::transfer_tab` onto
    /// the unified empty-window cleanup contract: a stale id is a
    /// silent no-op (no panic, no spurious `windows` mutation), which
    /// is the only behaviour we can reliably pin without a live
    /// `WindowState` (needs a wgpu surface + winit `Window`).
    #[doc(hidden)]
    pub fn __test_invoke_reap_empty_child(&mut self, id: WindowId) {
        self.reap_empty_child(id);
    }

    /// Test-only: read the `pending_new_window` flag. Set by the
    /// `Action::NewWindow` dispatcher arm; consumed by
    /// `drain_pending_window_creates` (which needs a live
    /// `ActiveEventLoop` and so can't run in a unit test). The flag
    /// is the testable seam.
    #[doc(hidden)]
    pub fn __test_pending_new_window(&self) -> bool {
        self.pending_new_window.is_some()
    }

    /// Test seam for deferred in-process tear-out requests.
    #[doc(hidden)]
    pub fn __test_pending_tear_out(&self) -> Option<(WindowId, usize, Option<(i32, i32)>)> {
        self.pending_tear_out.as_ref().map(|tear_out| {
            (tear_out.source_window, tear_out.source_tab_idx, tear_out.drop_screen_pos)
        })
    }

    /// test seam: read the `pending_os_teardown` flag set
    /// by `handle_os_drag_ended` on the `DroppedOnEmpty` branch.
    #[doc(hidden)]
    pub fn __test_pending_os_teardown(&self) -> bool {
        self.pending_os_teardown
    }

    /// test seam: directly set `pending_os_teardown` so
    /// the race test can simulate the `DroppedOnEmpty` branch without
    /// forging a full OS-drag pending state.
    #[doc(hidden)]
    pub fn __test_set_pending_os_teardown(&mut self, pending: bool) {
        self.pending_os_teardown = pending;
    }

    /// test seam: drive `drain_pending_os_teardown` from
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
    /// the shadow main entry inserted by
    /// [`Self::do_resumed`] is excluded so existing call sites that
    /// expected this to be "number of torn-out child terminal windows"
    /// keep meaning "number of torn-out child terminal windows".
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
    /// gesture before the user ever reaches a sibling bar.
    #[doc(hidden)]
    pub fn __test_set_drag_target(
        &mut self,
        target: Option<crate::tab_drag::DropTarget<WindowId>>,
    ) {
        self.__test_synthetic_main();
        if let Some(main) = self.main_mut() {
            main.drag_target = target;
        }
    }

    /// Test-only: remove a window from `self.windows` without going through
    /// the production teardown paths, releasing its resource-governor owner
    /// first. Overlay and governor tests use it to model a window that
    /// vanished. Returns `true` if the window existed and was removed,
    /// `false` otherwise.
    #[doc(hidden)]
    pub fn __test_remove_window(&mut self, id: WindowId) -> bool {
        self.release_window_owner(id);
        self.windows.remove(&id).is_some()
    }

    /// Test-only: drop a window without the explicit owner release first.
    ///
    /// [`Self::__test_remove_window`] calls `release_window_owner`, which takes
    /// each pane's owner in the right order before the window is removed. That
    /// hides what the struct does on its own, and the struct has to be right:
    /// a window removed from the map without that call, or held in the map
    /// until the process tears down, closes its owners purely by field drop
    /// order. This models that path.
    #[doc(hidden)]
    pub fn __test_drop_window_without_release(&mut self, id: WindowId) -> bool {
        self.windows.remove(&id).is_some()
    }

    /// Test-only: install a callback
    /// that fires INSIDE [`Self::cancel_drag_session`], AFTER the
    /// `self.windows.keys()` snapshot is collected but BEFORE the
    /// per-id iteration body runs. Lets tests exercise the exact
    /// `get_mut(&id).else { continue }` race-tolerance branch by
    /// removing (or inserting) a window in between.
    #[doc(hidden)]
    pub fn __test_set_post_snapshot_hook<F>(&mut self, hook: F)
    where
        F: FnOnce(&mut App) + Send + 'static,
    {
        self.test_post_snapshot_hook = Some(Box::new(hook));
    }

    /// Test-only: seed a synthetic tab with one pane that has no PTY
    /// attached (just a Parser owning a fresh Grid). Lets integration
    /// Read-back of [`Self::main_window_id`] for tests.
    #[doc(hidden)]
    pub fn __test_main_window_id(&self) -> Option<WindowId> {
        self.main_window_id
    }

    // ShadowMainSnapshot helpers deleted — dpi + hovered_url
    // now live exclusively on WindowState.

    /// for tests that build an `App` without
    /// `do_resumed` running, insert a synthetic main `WindowState`
    /// entry (window=None, renderer=None) under a stable synthetic
    /// `WindowId` so test seeders can route writes through
    /// [`Self::main_mut`]. No-op if `main_window_id` is already set.
    /// In production [`Self::do_resumed`] detects the synthetic entry
    /// and removes it before inserting the real one.
    #[doc(hidden)]
    pub fn __test_synthetic_main(&mut self) {
        if self.main_window_id.is_some() {
            // When: `main_window_id` is already set, so seeding a second entry
            // would displace the identity live state is keyed by.
            return;
        }
        let id = synthetic_main_window_id();
        let main = WindowState {
            // Registered when the window is inserted.
            owner: None,
            role: WindowRole::Terminal,
            custom_window_name: String::new(),
            window: None,
            renderer: None,
            tabs: TabBar::new(),
            tab_states: Vec::new(),
            panes: HashMap::new(),
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
            last_render: Instant::now(),
            retry_not_before: None,
            visible_frame_invalid: false,
            redraw: Default::default(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            dpi_scale: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            link_preview: None,
            path_probe: path_target::PathProbeState::default(),
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
        };
        self.insert_window_registered(id, main);
        self.main_window_id = Some(id);
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

    /// Test-only: install an `OsDragSink` so [`Self::try_os_drag_handoff`]
    /// can be exercised without going through the platform entry point.
    #[doc(hidden)]
    pub fn __test_set_os_drag_sink(&mut self, sink: Arc<dyn crate::os_drag::OsDragSink>) {
        self.os_drag_sink = Some(sink);
    }

    /// Test-only: install a mock [`os_drag::OsTabDragBackend`].
    #[doc(hidden)]
    pub fn __test_set_os_drag_backend(&mut self, backend: Box<dyn os_drag::OsTabDragBackend>) {
        self.os_drag_backend = Some(backend);
    }

    /// Test-only: hand out the shared pending-outcome mailbox
    /// so tests can drive [`Self::handle_os_drag_ended`] without
    /// constructing a real [`winit::event_loop::EventLoopProxy`].
    #[doc(hidden)]
    pub fn __test_os_drag_pending(&self) -> Arc<os_drag::PendingDragOutcome> {
        self.os_drag_pending.clone()
    }

    /// Test-only: seed the in-flight source bookkeeping that
    /// [`Self::begin_os_tab_drag`] normally sets. Used by tests that
    /// drive the dispatcher directly without first calling
    /// `begin_os_tab_drag`.
    #[doc(hidden)]
    pub fn __test_set_os_drag_source(&mut self, source: Option<(WindowId, usize)>) {
        self.os_drag_source = source
            .and_then(|(window, index)| self.tab_id_at(window, index).map(|tab| (window, tab)));
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
        self.main().and_then(|main| main.pressed_tab)
    }

    /// Test seam: whether the main window is tracking a held mouse button.
    ///
    /// Reports `false` when no main window exists, so a caller sees the same
    /// "nothing held" answer either way.
    #[doc(hidden)]
    pub fn __test_mouse_down(&self) -> bool {
        self.main().map(|main| main.mouse_down).unwrap_or(false)
    }

    /// Test seam: set which tab the main window treats as pressed.
    ///
    /// Seeds a synthetic main window first, so a test can drive tab-press
    /// behavior without a live winit window.
    #[doc(hidden)]
    pub fn __test_set_pressed_tab(&mut self, pressed_tab: Option<usize>) {
        self.__test_synthetic_main();
        if let Some(main) = self.main_mut() {
            main.pressed_tab = pressed_tab;
        }
    }

    /// Test seam: set whether the main window holds a mouse button.
    ///
    /// Seeds a synthetic main window first, so drag gestures can be driven
    /// without real pointer events.
    #[doc(hidden)]
    pub fn __test_set_mouse_down(&mut self, mouse_down: bool) {
        self.__test_synthetic_main();
        if let Some(main) = self.main_mut() {
            main.mouse_down = mouse_down;
        }
    }
}
