//! Per-window state: tabs, panes, pointer and IME state, and the topology, redraw,
//! and focus transitions that keep them consistent.

use super::*;

/// Classification of a window tracked in the app's role-tagged window map.
///
/// Each tracked window carries a role, so callers can count or select windows
/// by kind rather than by identity. Only terminals are tracked today, so the
/// enum has a single variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRole {
    /// A terminal window created by tearing a tab off the bar.
    ///
    /// The detached pane's PTY threads keep running across the tear-out; their
    /// redraw target is repointed at the child's surface so shell output
    /// redraws the window that now contains it rather than the parent.
    Terminal,
}

#[derive(Debug, Clone)]
pub struct SplitterDragState {
    pub splitter: sonicterm_ui::pane::SplitterId,
    pub axis: sonicterm_ui::pane::SplitAxis,
    pub last_pos: (f32, f32),
}

/// One validated active-pane transition awaiting its visual feedback frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneFocusChange {
    pub(crate) pane_id: u64,
}

pub struct WindowState {
    /// Session-local user name; terminal-generated titles never write this field.
    pub(crate) custom_window_name: String,
    /// Window classification — see [`WindowRole`].
    pub role: WindowRole,
    /// promoted from `Arc<Window>` to
    /// `Option<Arc<Window>>` so test seeders can build a `WindowState`
    /// without running `do_resumed`. In production this is `Some(_)`
    /// the moment `do_resumed` (main) or `create_child_window`
    /// (torn-out) finishes; every call site either short-circuits via
    /// `if let Some(w) = ws.window.as_ref()` or early-returns via
    /// `ws.window.as_ref()?` when the window is gone.
    pub window: Option<Arc<Window>>,
    /// Per-window wgpu renderer. `Some(_)` once `do_resumed` (main
    /// window) or `create_child_window` (torn-out) populates it.
    /// the main window's renderer now lives here too —
    /// the legacy `App.renderer` field was deleted. Read through
    /// [`Self::renderer`] / [`Self::renderer_mut`] which unwrap (always
    /// safe after `do_resumed`).
    pub renderer: Option<GpuRenderer>,
    pub tabs: TabBar,
    pub tab_states: Vec<TabState>,
    pub panes: HashMap<u64, PaneState>,
    /// This window's owner in the governor hierarchy.
    ///
    /// `None` for synthetic windows built by tests that never registered one.
    /// Production windows always have it; the option exists so a test seam
    /// cannot silently register a phantom owner that never closes.
    ///
    /// An [`OwnerGuard`] for the same reason the pane's is: the window is
    /// removed from `self.windows` at three sites across two files, and the
    /// close has to be a property of ownership rather than something each of
    /// them remembers.
    ///
    /// **Declared after `panes`, and the order is load-bearing.** Rust drops
    /// fields in declaration order, and a pane's owner is a child of this one.
    /// `finish_close` refuses an owner that still has live children, so a
    /// window dropped with this field first would strand its own owner part
    /// closed while the panes beneath it were still open. Panes first means
    /// each child guard has already closed by the time this one runs.
    pub(crate) owner: Option<OwnerGuard>,
    pub cursor_pos: (f64, f64),
    pub mouse_down: bool,
    /// Press-latched routing for the current left-button grid gesture.
    pub(crate) pointer_gesture: Option<PointerGesture>,
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
    /// Accepted key presses retain each target's protocol ownership until release or cancellation.
    pub pty_pressed_keys: HashMap<PhysicalKey, std::collections::BTreeMap<u64, HeldKey>>,
    // `cursor_visible` lives on `PaneState`, not here: its per-pane Arc travels
    // with the pane through tear-out. Read it via
    // `ws.panes.get(&active_pane).map(|p| p.cursor_visible.load(...))`.
    pub last_render: Instant,
    /// Earliest collection retry after lock contention, independent of completed-frame pacing.
    pub(crate) retry_not_before: Option<Instant>,
    /// Invalid-topology episode latch; collection never arms a retry for it.
    pub(crate) visible_frame_invalid: bool,
    /// Owner-local redraw causes and native-monitor cadence; last_render remains the pacing clock.
    pub(crate) redraw: redraw::WindowRedrawState,
    /// pointer-cursor-is-link latch. Mirrors
    /// `App.hover_link` (now deleted). Per-window so a torn-out child can
    /// flip its own cursor independently of the main window.
    pub hover_link: bool,
    /// Tab index pressed in the child's bar — same role as
    /// `App::pressed_tab` but for the child window. Used for
    /// drag-from-child merging.
    pub pressed_tab: Option<usize>,
    /// Live drag session for a held-tab gesture in this child window.
    pub drag_session: Option<crate::tab_drag::DragSession<WindowId>>,
    /// Pending cross-window drop target chosen during a drag in the
    /// child's bar; consumed on mouse-up.
    pub drag_target: Option<crate::tab_drag::DropTarget<WindowId>>,
    /// Per-window DPI multiplier retained for renderer rasterization
    /// rebuilds when winit reports monitor changes. Cursor/layout math is
    /// raster-px and must not read this field.
    pub dpi_scale: f64,
    /// Per-window IME composition state, so torn-out windows compose CJK
    /// input independently of the main window.
    pub ime: ImeState,
    /// Per-window throttle for
    /// `Window::set_ime_cursor_area`, so each torn-out window throttles its
    /// own IMK runloop traffic independently. Every read path goes through
    /// `self.main()?.ime_cursor_throttle`.
    pub ime_cursor_throttle: sonicterm_ui::ime::ImeCursorThrottle,
    /// Per-window hovered URL or validated local-path span.
    pub hovered_url: Option<hovered_url::HoveredUrl>,
    /// Modifier-gated destination of the pointed URI.
    pub link_preview: Option<sonicterm_render_model::inputs::LinkPreview>,
    /// Epoch-guarded openability decision for the local target under this pointer.
    pub(in crate::app) path_probe: path_target::PathProbeState,
    pub notification: Option<NotificationBubble>,
    /// "this window is hidden / drained" latch.
    /// Promoted from the App-level `main_hidden` bool so the visibility
    /// state lives next to the `Window` Arc it gates. Today only the main
    /// window flips this to `true` (when its last tab is torn out and
    /// child windows keep the event loop alive); child windows leave it
    /// `false` and reap on empty instead.
    pub hidden: bool,
    /// Active scrollbar-drag gesture. `Some(_)` between a
    /// thumb mouse-down and the matching release; cursor moves while
    /// set route to the scrollbar instead of extending a selection.
    pub scrollbar_drag: Option<scrollbar_input::ScrollbarDragState>,
    /// Active split-pane divider drag. While set, cursor moves resize the
    /// captured split ratio instead of extending text selection.
    pub splitter_drag: Option<SplitterDragState>,
    /// Current split-divider hover axis, used to restore the OS cursor when
    /// the pointer leaves the divider.
    pub splitter_hover: Option<sonicterm_ui::pane::SplitAxis>,
    /// Per-pane scrollbar visibility/fade state. Inserted
    /// lazily on first interaction; entries for closed panes are
    /// pruned opportunistically on the next render.
    pub scrollbar_vis: HashMap<u64, scrollbar_visibility::ScrollbarVisState>,
    pub pending_tear_out_timing: Option<TearOutTiming>,
    /// Test-only mirror of the renderer's `drag_chip` overlay.
    /// Production code leaves this `None`. Headless tests use
    /// [`App::__test_set_window_drag_chip_marker`] to flip it `Some(true)`
    /// before calling [`App::cancel_drag_session`], then assert it is
    /// `Some(false)` afterward via [`App::__test_window_drag_chip_marker`].
    /// `cancel_drag_session` flips this in lock-step with the real
    /// `renderer.set_drag_chip(None)` call (when `Some(_)`), so the test
    /// observes the SAME loop iteration the production path runs — if
    /// someone deletes the per-window iteration the marker stays `Some(true)`
    /// and the test fails. Headless windows have `renderer: None`, so this seam
    /// is their only way to observe `set_drag_chip(None)`.
    pub test_drag_chip_marker: Option<bool>,
    /// Test-only renderer-focus marker. Headless child windows have
    /// `renderer: None`, so focus lifecycle regression tests seed this marker
    /// and expect the same focus transition that would call
    /// `GpuRenderer::set_window_focused` to update it. Production leaves this
    /// `None`.
    #[doc(hidden)]
    pub test_renderer_focus_marker: Option<bool>,
    /// Test-only viewport override for this window's pane layout, mirroring
    /// [`App::test_viewport_override`] for the MAIN window. When `Some((outer,
    /// cell_w, cell_h))`, `App::compute_pane_rects_for` uses `outer` instead
    /// of the (absent in headless tests) renderer's logical size, and
    /// `child_window::resize_visible_panes_in_child` uses
    /// `(cell_w, cell_h)` for cell metrics. Lets tests exercise the child
    /// split-pane Grid/PTY resize wiring (tear-out, Resized, close, split)
    /// without a live wgpu surface: synthetic children carry `renderer: None`,
    /// so without this override the resize helper silently no-ops.
    /// Stays `None` in release.
    #[doc(hidden)]
    pub test_pane_viewport: Option<(sonicterm_ui::pane::Rect, f32, f32)>,
}

#[derive(Clone, Copy)]
pub(super) struct TopologyChange {
    pub(super) resize_visible: bool,
    pub(super) focus_feedback: Option<u64>,
}

impl WindowState {
    /// Record a press on the tab at `tab_index` of this window's bar at `press_pos`: the
    /// pointer is down, the tab is pressed and a drag session starts, so a drag can reorder,
    /// transfer or tear the tab out. Both left-button handlers call it after activating the tab.
    pub(super) fn begin_tab_press(
        &mut self,
        window_id: WindowId,
        tab_index: usize,
        press_pos: (f32, f32),
    ) {
        self.mouse_down = true;
        self.pressed_tab = Some(tab_index);
        self.drag_session = self
            .tabs
            .tabs()
            .get(tab_index)
            .map(|tab| crate::tab_drag::DragSession::new(window_id, tab.id, press_pos));
    }

    /// End the pointer press on this window: the pointer is up, and the drag session, the
    /// foreign drop target and the pressed tab are taken for the release to act on. Both
    /// left-button handlers call it on release.
    pub(super) fn end_tab_press(
        &mut self,
    ) -> (
        Option<crate::tab_drag::DragSession<WindowId>>,
        Option<crate::tab_drag::DropTarget<WindowId>>,
        Option<usize>,
    ) {
        self.mouse_down = false;
        (self.drag_session.take(), self.drag_target.take(), self.pressed_tab.take())
    }

    pub(super) fn reconcile_pane_owners(&mut self) {
        let Some(parent) = self.owner.as_ref() else {
            // When: the window has no owner, preserve its explicit unregistered accounting state.
            return;
        };
        let governor = parent.governor.clone();
        let parent = parent.id();
        for (pane_id, pane) in &mut self.panes {
            if pane.owner.is_some() {
                // When: the pane already owns a registration, completion must not replace its existing charges.
                continue;
            }
            match governor.create_child(parent, OwnerKind::AppPane, pane_owner_limits()) {
                Ok(owner) => pane.owner = Some(OwnerGuard::new(governor.clone(), owner)),
                Err(error) => {
                    tracing::warn!(target: "memory", ?error, pane_id, "pane owner registration refused; pane remains unregistered")
                }
            }
        }
    }

    pub(super) fn complete_topology_change(
        &mut self,
        change: TopologyChange,
        viewport: Option<(sonicterm_ui::pane::Rect, f32, f32)>,
    ) -> bool {
        if self.tabs.len() != self.tab_states.len() {
            // When: tabs and tab_states lengths disagree, refuse completion rather than presenting mismatched titles and panes.
            return false;
        }
        if let Some(tab) = self.tab_states.get(self.tabs.active_index()) {
            // When: active_index resolves to a tab, its focus and visible leaves must agree before any completion effects.
            let leaves = tab.tree.leaves();
            if !leaves.contains(&tab.active_pane)
                || !leaves.iter().all(|id| self.panes.contains_key(id))
                || tab.tree.zoomed_pane_id().is_some_and(|id| id != tab.active_pane)
                || change.focus_feedback.is_some_and(|id| id != tab.active_pane)
            {
                // When: leaves, active_pane, zoomed_pane_id or focus_feedback disagree, refuse rather than inventing a replacement.
                return false;
            }
            if change.resize_visible {
                let metrics = viewport
                    .or(self.test_pane_viewport)
                    .map(|(outer, cell_width_px, cell_height_px)| {
                        (outer, cell_width_px, cell_height_px, [0.0; 4])
                    })
                    .or_else(|| {
                        self.renderer.as_ref().map(|renderer| {
                            let (width, height) = renderer.logical_size();
                            let top = (renderer.top_inset() - renderer.padding_top_px()).max(0.0);
                            let outer = sonicterm_ui::pane::Rect::new(
                                0.0,
                                top,
                                width.max(0.0),
                                (height - top - renderer.bottom_inset()).max(0.0),
                            );
                            let (cell_width_px, cell_height_px) = renderer.cell_size();
                            (
                                outer,
                                cell_width_px,
                                cell_height_px,
                                [
                                    renderer.padding_left_px(),
                                    renderer.padding_right_px(),
                                    renderer.padding_top_px(),
                                    renderer.padding_bottom_px(),
                                ],
                            )
                        })
                    });
                if let Some((outer, cell_width_px, cell_height_px, inset)) = metrics {
                    resize_panes_to_rects(
                        &self.panes,
                        &tab.tree.layout(outer),
                        cell_width_px,
                        cell_height_px,
                        inset,
                    );
                }
            }
        }
        self.reconcile_pane_owners();
        self.ime_cursor_throttle.reset();
        self.invalidate_path_hover();
        self.scrollbar_vis.retain(|id, _| self.panes.contains_key(id));
        if self
            .selection
            .as_ref()
            .and_then(|selection| selection.pane_id)
            .is_some_and(|id| !self.panes.contains_key(&id))
        {
            self.selection = None;
        }
        mark_all_panes_dirty(&self.panes);
        if let (Some(renderer), Some(pane)) = (self.renderer.as_mut(), change.focus_feedback) {
            renderer.flash_pane_focus(pane);
        }
        self.mark_redraw(redraw::RedrawCause::Topology);
        self.request_window_redraw();
        true
    }

    pub(super) fn arm_contention_retry(&mut self, now: Instant, period: Duration) {
        if self.retry_not_before.is_none_or(|deadline| deadline <= now) {
            self.retry_not_before = Some(now + period);
        }
    }

    pub(super) fn redraw_not_before(&self, period: Duration) -> Instant {
        let paced = self.last_render + period;
        self.retry_not_before.map_or(paced, |retry| paced.max(retry))
    }

    pub(super) fn contention_blocks_redraw(&self, now: Instant, period: Duration) -> bool {
        self.retry_not_before.is_some() && now < self.redraw_not_before(period)
    }

    /// Clear collection backoff and its invalidity warning only after held-frame reconciliation succeeds.
    pub(super) fn coherent_frame_collected(&mut self) {
        self.retry_not_before = None;
        self.visible_frame_invalid = false;
    }

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

    /// Remove a pane from this renderer owner and release both row-cache parts.
    ///
    /// Detach paths use the same seam as permanent close: once a pane leaves this
    /// window, its NDC glyphs and quads cannot be reused by the destination
    /// renderer. The renderer performs glyph eviction before quad eviction in one
    /// event-loop-owned mutable call, so retention snapshots cannot observe only
    /// half of the removal.
    pub(crate) fn remove_pane(&mut self, pane_id: u64) -> Option<PaneState> {
        let pane = self.panes.remove(&pane_id)?;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.invalidate_pane_caches(pane_id);
        }
        Some(pane)
    }

    /// Ask the native window to redraw; does nothing once `window` is `None`.
    #[inline]
    pub fn request_window_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            crate::app::frame_counters::request_native_redraw(window);
        }
    }

    /// Change focus to a leaf in the active tab and return the feedback token.
    pub(super) fn begin_pane_focus_change(&mut self, pane_id: u64) -> Option<PaneFocusChange> {
        let tab_idx = self.tabs.active_index();
        let tab = self.tab_states.get_mut(tab_idx)?;
        if tab.active_pane == pane_id
            || !tab.tree.leaves().contains(&pane_id)
            || !self.panes.contains_key(&pane_id)
            || tab.tree.zoomed_pane_id().is_some_and(|zoomed| zoomed != pane_id)
        {
            // When: the target is already active or belongs to another tab, no
            // focus transition occurred and existing feedback must not restart.
            return None;
        }
        tab.active_pane = pane_id;
        Some(PaneFocusChange { pane_id })
    }

    /// Begin pointer focus and discard selection owned by the previous pane.
    pub(super) fn begin_pointer_pane_focus_change(
        &mut self,
        pane_id: u64,
    ) -> Option<PaneFocusChange> {
        let change = self.begin_pane_focus_change(pane_id)?;
        self.selection = None;
        Some(change)
    }

    /// Present one validated pane-focus transition after related input work.
    pub(super) fn finish_pane_focus_change(&mut self, change: PaneFocusChange) {
        self.complete_topology_change(
            TopologyChange { resize_visible: false, focus_feedback: Some(change.pane_id) },
            None,
        );
    }

    /// Revoke any path authorization and remove pointer-owned target visuals.
    pub(super) fn invalidate_path_hover(&mut self) {
        let changed = self.path_probe.invalidate()
            | self.hovered_url.take().is_some()
            | self.link_preview.take().is_some()
            | self.hover_link;
        self.hover_link = false;
        if changed {
            if let Some(window) = self.window.as_ref() {
                window.set_cursor(winit::window::CursorIcon::Default);
            }
            self.request_window_redraw();
        }
    }

    /// Clear the drag-chip overlay through a single call site.
    ///
    /// Two things represent the chip: the renderer's persistent overlay drawn
    /// by the per-frame emitter, and the headless-test marker
    /// (`test_drag_chip_marker`). Clearing them from separate statements lets a
    /// later refactor split them, leaving the regression test green while
    /// production keeps drawing the chip. Both clears live here so every caller
    /// flips them in lock-step.
    ///
    /// **Contract:** this helper is
    /// **tolerant** — it is safe to call on a `WindowState` whose
    /// `renderer` is `None` (e.g. a transitional window that hasn't
    /// finished initialization yet, or a headless test window) AND on
    /// a window whose `test_drag_chip_marker` is `None`. Both branches
    /// short-circuit cleanly. This matters because the deferred
    /// `pending_os_teardown` drain (see [`App::cancel_drag_session`]
    /// and `App::drain_pending_os_teardown`) iterates a snapshot of
    /// `self.windows.keys()`, and a tear-out spawn that just landed
    /// may have produced a `WindowState` whose renderer
    /// is still being constructed. Both fields are flipped together —
    /// callers MUST NOT split them, or the headless-test marker stops
    /// mirroring the renderer's chip.
    #[inline]
    pub(crate) fn clear_drag_chip(&mut self) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_drag_chip(None);
        }
        if let Some(marker) = self.test_drag_chip_marker.as_mut() {
            *marker = false;
        }
    }

    /// — intra-window tab reorder that keeps `tabs` and
    /// `tab_states` in lock-step. Extracted from `window_event.rs`'s
    /// main-window `ReorderTab` branch so the production path and the
    /// regression tests exercise the SAME code.
    ///
    /// Semantics match `tab_transfer::reorder_within`:
    /// - `from` out of range → no-op.
    /// - `to` clamped to `len - 1` (— drop-past-last must land at
    ///   the end, not silently no-op like `TabBar::reorder` does).
    /// - `to == from` after clamp → no-op.
    /// - Otherwise: `tabs.reorder(from, to)` AND
    ///   `tab_states.remove(from) → insert(to)` so the title's TabState
    ///   (active pane id + PaneTree leaf-ids) travels WITH the title.
    ///
    /// Returns `true` if any mutation happened.
    pub fn reorder_tab(&mut self, from: usize, to: usize) -> bool {
        let len = self.tabs.len();
        if from >= len || len == 0 || self.tab_states.len() != len {
            // When: `from` names no live tab, so there is nothing to move and the
            // caller must not be told the order changed.
            return false;
        }
        let last = len - 1;
        let to = to.min(last);
        if to == from {
            // When: clamping `to` landed it back on `from`, so the drop target is
            // the tab's existing slot and reordering would be a no-op.
            return false;
        }
        self.tabs.reorder(from, to);
        let state = self.tab_states.remove(from);
        self.tab_states.insert(to, state);
        self.complete_topology_change(
            TopologyChange { resize_visible: false, focus_feedback: None },
            None,
        );
        true
    }
}

#[cfg(test)]
#[path = "window_state_tests.rs"]
mod window_state_tests;
