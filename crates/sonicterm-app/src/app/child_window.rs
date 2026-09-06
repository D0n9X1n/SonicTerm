//! Child-window event routing, per-child tab/pane mutators, and the child
//! PTY/VT wiring. `App`'s referenced fields are `pub(super)`; this submodule
//! lives in the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Direction, Keymap, ScrollAction};
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::PtyHandle;
use sonicterm_ui::command_palette::CommandPalette;
use sonicterm_ui::overlays::{
    command_palette_query_caret_prefix, search_bar_label, search_query_caret_prefix, PaletteLayout,
    SearchBarLayout, PALETTE_ROW_PAD_X, SEARCH_BAR_ICON_GAP, SEARCH_BAR_PAD_LEFT,
    SEARCH_BAR_PAD_RIGHT,
};
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::selection::{plain_text_from_grid_range, SelectMode, Selection};
use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use sonicterm_ui::tabs::{Tab, TabBar};
use sonicterm_vt::vt::Parser;
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::scrollbar_input::HitOutcome;
use super::{
    invalidate_selection_for_content,
    key_encoding::{encode_logical, key_event_to_string, key_event_to_strings, key_name},
    mark_all_panes_dirty, next_pane_id, pane_id_at_point, pick_prompt_target,
    poll_command_events_for_child_window, resize_all_panes, shell_quote_posix,
    with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, PointerCell,
    PointerGestureOwner, RuntimeSmokeFailure, TabState, UserEvent, WindowState,
};

const SEARCH_BADGE_ICON: &str = "";

/// Route live child-window motion after applying every child chrome owner.
pub(super) fn child_no_button_motion_report(
    child: &WindowState,
    cell: PointerCell,
    tracking: sonicterm_vt::vt::MouseTracking,
    sgr: bool,
    scrollbar_owned: bool,
) -> Option<super::window_event::PointerMotionRoute> {
    let ui_consumed = child.splitter_hover.is_some()
        || child.hovered_url.is_some()
        || child.hover_link
        || scrollbar_owned;
    super::window_event::no_button_motion_report(cell, tracking, sgr, child.modifiers, ui_consumed)
}

fn estimate_overlay_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().map(|ch| if ch.is_ascii() { 0.58 } else { 1.0 }).sum::<f32>() * font_size
}

/// Resize `renderer` to `width × height` and size every pane in `panes` to the
/// whole resulting cell grid, pushing the new size into each parser grid and
/// PTY winsize. Returns `true` when a renderer was present and accepted the
/// resize, so the caller knows a redraw is worth requesting.
///
/// Full-grid sizing is correct only for a single-pane tab; a split window must
/// use [`resize_renderer_and_split_panes`] so each pane keeps its own sub-rect.
#[doc(hidden)]
pub fn resize_renderer_and_panes_if_present(
    renderer: &mut Option<GpuRenderer>,
    panes: &HashMap<u64, PaneState>,
    width: u32,
    height: u32,
) -> bool {
    let Some(r) = renderer.as_mut() else {
        // When: `renderer` is absent — a headless or not-yet-initialized window
        // has no surface to size, and no pane geometry can be derived.
        return false;
    };
    if !r.try_resize(width, height) {
        // When: `try_resize` rejected `width`/`height` as unrepresentable, so
        // the old surface stands and resizing panes would desync them from it.
        return false;
    }
    let (cols, rows) = r.cells();
    for (pane_id, pane) in panes {
        pane.parser.lock().grid_mut().resize(cols, rows);
        pane.resize_pty(*pane_id, cols, rows);
    }
    true
}

/// Resize the child renderer to `width × height`, then size each pane to its
/// own split sub-rect via [`resize_visible_panes_in_child`] rather than to the
/// whole grid. Returns `true` if a renderer was present, so the caller can
/// request a redraw.
///
/// Sizing every pane to the full `(cols, rows)` — what
/// [`resize_renderer_and_panes_if_present`] does — makes a split overlap: each
/// pane stays full-window wide and wraps across the divider. The child
/// `Resized` handler routes here so per-split geometry survives every resize.
pub(super) fn resize_renderer_and_split_panes(
    child: &mut WindowState,
    width: u32,
    height: u32,
) -> bool {
    let Some(r) = child.renderer.as_mut() else {
        // When: this `child` has no `renderer`, so there is no surface to
        // resize and no cell metrics to lay the panes out against.
        return false;
    };
    if !r.try_resize(width, height) {
        // When: `try_resize` refused `width`/`height`, so the panes must keep
        // matching the surface that is still live.
        return false;
    }
    resize_visible_panes_in_child(child);
    true
}

/// Apply `dpi_scale` to `renderer` so glyph rasterization and overlay geometry
/// track the monitor the window now sits on. Returns `true` when a renderer was
/// present to receive the new scale, so the caller can request a redraw.
#[doc(hidden)]
pub fn apply_dpi_to_renderer_if_present(
    renderer: &mut Option<GpuRenderer>,
    dpi_scale: f64,
) -> bool {
    let Some(r) = renderer.as_mut() else {
        // When: `renderer` is absent, so there is nothing holding a scale
        // factor; the caller's recorded `dpi_scale` is applied at creation.
        return false;
    };
    r.set_scale_factor(dpi_scale as f32);
    true
}

/// Resize a child window's renderer and panes, requesting a redraw only when a
/// renderer was actually resized. Tolerates a child that has no renderer yet.
#[doc(hidden)]
pub fn child_window_resized_handles_no_renderer(child: &mut WindowState, width: u32, height: u32) {
    if resize_renderer_and_panes_if_present(&mut child.renderer, &child.panes, width, height) {
        child.request_redraw();
    }
}

/// Record `dpi_scale` on the child and push it into the renderer, requesting a
/// redraw only when a renderer took the new scale. Tolerates a child that has
/// no renderer yet, so the recorded scale still applies once one is built.
#[doc(hidden)]
pub fn child_window_dpi_changed_handles_no_renderer(child: &mut WindowState, dpi_scale: f64) {
    child.dpi_scale = dpi_scale;
    if apply_dpi_to_renderer_if_present(&mut child.renderer, dpi_scale) {
        child.request_redraw();
    }
}

impl App {
    /// Release a child through the same pane, owner, registry, renderer, and PTY boundary.
    pub(super) fn close_child_window(&mut self, win_id: WindowId) -> bool {
        let Some(mut removed) = self.windows.remove(&win_id) else {
            // When: `windows.remove(&win_id)` is `None`, no child resources remain to release.
            return false;
        };
        for pane in removed.panes.values() {
            *pane.redraw_target.lock() = None;
        }
        self.release_owners_of(&mut removed);
        self.release_child_window_registries(win_id);
        drop(removed);
        true
    }

    /// Route one winit `WindowEvent` for the child window `win_id`: scrollbar,
    /// splitter and URL input first, then render, resize, focus, mouse,
    /// keyboard and IME handling against that child's own tabs and panes.
    // Ordering: `pty_burst_gen` Acquire pairs with the VT thread's Release so a
    // burst is seen; `cursor_visible`, `kitty_flags`, `keyboard_modes` Relaxed.
    pub(super) fn handle_child_window_event(
        &mut self,
        el: &ActiveEventLoop,
        win_id: WindowId,
        event: WindowEvent,
    ) {
        let theme = self.theme.clone();
        let config = self.config.clone();
        let process_privileged = self.process_privilege.is_privileged();
        // Snapshot the app-level overlay attachment before the mutable `child`
        // borrow below pins `self.windows` for the rest of the match. Used only
        // by the `RedrawRequested` arm but cheap enough to compute once.
        let palette_here = self.palette_attached_window == Some(win_id);
        // vsync coalescing inputs for the `RedrawRequested`
        // arm, snapshotted BEFORE the long-lived `child` borrow of
        // `self.windows` below (these read disjoint `self` fields the
        // borrow would otherwise pin). Mirrors the main-window gate in
        // `window_event.rs`: `was_dirty` carves out input-driven redraws
        // (keystroke/resize/theme) so they stay immediate, and
        // `pty_burst` carves out frames carrying fresh PTY bytes. Only a
        // pure PTY-streaming redraw with neither flag set is eligible for
        // the next-vsync deferral. `pty_burst_snapshot` is recorded as
        // `last_seen_burst_gen` after a successful child render so the
        // following streaming redraw coalesces instead of over-rendering.
        let was_dirty = self.input_dirty;
        let frame_period = self.frame_period;
        let software_render_degrade = self.software_render_degrade;
        let scrollbar_motion = crate::app::scrollbar_visibility::window_scrollbar_motion(
            self.windows
                .get(&win_id)
                .and_then(|window| window.renderer.as_ref())
                .map(GpuRenderer::is_software_render_degraded),
            software_render_degrade,
        );
        let pty_burst_snapshot = self.pty_burst_gen.load(Ordering::Acquire);
        let pty_burst = pty_burst_snapshot != self.last_seen_burst_gen;
        // Scrollbar input is handled HERE, before the long-lived `child` borrow
        // below, because the scrollbar helpers take `&self`/`&mut self` and
        // would conflict with that borrow inside a match arm. A press on the
        // thumb starts a drag (track click pages); a cursor move while a drag is
        // in flight scrolls the pane. On a Miss we fall through to the normal
        // match so pane-focus / selection still work.
        match &event {
            WindowEvent::DroppedFile(path) => {
                // When: a `DroppedFile` carries a `path`, which pastes as a
                // shell-quoted argument instead of routing as pointer input.
                self.paste_file_paths_for_kind(FrontmostKind::Child(win_id), [path.clone()]);
                if let Some(child) = self.windows.get(&win_id) {
                    child.request_redraw();
                }
                return;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // When: a left `MouseInput` press arrives, so notification,
                // scrollbar, splitter and URL hit-tests run before pane input.
                let cursor_pos =
                    self.windows.get(&win_id).map(|c| c.cursor_pos).unwrap_or((0.0, 0.0));
                if self.dismiss_notification_at(
                    FrontmostKind::Child(win_id),
                    cursor_pos.0 as f32,
                    cursor_pos.1 as f32,
                ) {
                    // When: `dismiss_notification_at` consumed the press, so the
                    // click dismissed a toast rather than reaching the grid.
                    return;
                }
                let (px, py) = self
                    .windows
                    .get(&win_id)
                    .map(|c| (c.cursor_pos.0 as f32, c.cursor_pos.1 as f32))
                    .unwrap_or((0.0, 0.0));
                match self.scrollbar_hit_at_in_child(win_id, px, py) {
                    HitOutcome::Miss => {
                        // When: `HitOutcome::Miss` — the press was not on a
                        // scrollbar, so it falls through to the main match.
                    }
                    HitOutcome::StartDrag(state) => {
                        // When: `StartDrag` — the press landed on the thumb, so
                        // a drag is armed and tracked until release.
                        if let Some(c) = self.windows.get_mut(&win_id) {
                            c.mouse_down = true;
                            c.scrollbar_drag = Some(state);
                            c.request_redraw();
                        }
                        return;
                    }
                    HitOutcome::PageUp => {
                        // When: `PageUp` — the press hit the track above the
                        // thumb, so the view pages back through scrollback.
                        self.scrollbar_track_page_in_child(win_id, false);
                        return;
                    }
                    HitOutcome::PageDown => {
                        // When: `PageDown` — the press hit the track below the
                        // thumb, so the view pages toward the live bottom.
                        self.scrollbar_track_page_in_child(win_id, true);
                        return;
                    }
                }
                // Start a divider drag if the press landed on a pane splitter.
                if let Some(hit) = self.splitter_hit_at_in_child(win_id, px, py) {
                    // When: `splitter_hit_at_in_child` reports a `hit`, so the
                    // press begins a divider drag instead of a selection.
                    if let Some(c) = self.windows.get_mut(&win_id) {
                        c.splitter_drag = Some(super::SplitterDragState {
                            splitter: hit.id,
                            axis: hit.axis,
                            last_pos: (px, py),
                        });
                        c.selection = None;
                        c.mouse_down = true;
                        c.request_redraw();
                    }
                    self.set_child_splitter_cursor(win_id, hit.axis);
                    return;
                }
                // Modifier-click opens a URI or existence-authorized path in
                // the exact rendered pane. Pane identity and cell coordinates
                // come from one device-pixel-snapped renderer snapshot.
                {
                    let pixel_target = self
                        .windows
                        .get(&win_id)
                        .and_then(|child| child.renderer.as_ref())
                        .and_then(|renderer| renderer.pixel_to_pane_cell(px, py));
                    let geometry_pane = self.windows.get(&win_id).and_then(|child| {
                        let rects = App::compute_pane_rects_for(child);
                        pane_id_at_point(&rects, px, py).or_else(|| {
                            let tab_idx = child.tabs.active_index();
                            child.tab_states.get(tab_idx).map(|tab| tab.active_pane)
                        })
                    });
                    let mods_held = self.child_url_open_modifier_held(win_id);
                    if let Some((rendered_pane, row, col)) = pixel_target {
                        // When: `pixel_target` identifies a rendered cell, activate only the pane identity from that same snapshot or its fallback.
                        let pane_id =
                            (rendered_pane != 0).then_some(rendered_pane).or(geometry_pane);
                        let opened = pane_id.is_some_and(|pane_id| {
                            mods_held && self.activate_target_at(win_id, pane_id, row, col)
                        });
                        if let Some(pane_id) = pane_id.filter(|_| opened) {
                            // When: `pane_id` survives the `opened` filter, focus and flash that exact pane before consuming the click.
                            if let Some(child) = self.windows.get_mut(&win_id) {
                                child.mouse_down = false;
                                if let Some(change) = child.begin_pointer_pane_focus_change(pane_id)
                                {
                                    child.finish_pane_focus_change(change);
                                }
                            }
                            return;
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // When: a `CursorMoved` event arrives, so an in-flight splitter or
                // scrollbar drag is applied before hover and selection work.

                // A splitter drag in flight resizes the divider, ahead of the
                // scrollbar and selection paths.
                let splitter_dragging =
                    self.windows.get(&win_id).map(|c| c.splitter_drag.is_some()).unwrap_or(false);
                if splitter_dragging {
                    // When: `splitter_dragging` — the pointer is moving a divider,
                    // so the move resizes panes rather than hovering or selecting.
                    let (cx, cy) = (position.x as f32, position.y as f32);
                    if let Some(c) = self.windows.get_mut(&win_id) {
                        c.cursor_pos = (position.x, position.y);
                    }
                    self.apply_splitter_drag_in_child(win_id, cx, cy);
                    return;
                }
                let dragging =
                    self.windows.get(&win_id).map(|c| c.scrollbar_drag.is_some()).unwrap_or(false);
                if dragging {
                    // When: `dragging` — a scrollbar thumb is held, so the move
                    // scrolls that pane instead of updating hover state.
                    let (cx, cy) = (position.x as f32, position.y as f32);
                    if let Some(c) = self.windows.get_mut(&win_id) {
                        c.cursor_pos = (position.x, position.y);
                    }
                    if let Some((pane_id, new_top)) =
                        self.scrollbar_drag_apply_in_child(win_id, cx, cy)
                    {
                        let live_top = self
                            .windows
                            .get(&win_id)
                            .and_then(|c| c.panes.get(&pane_id))
                            .map(|p| p.parser.lock().grid().scrollback_len() as u64)
                            .unwrap_or(new_top);
                        self.set_child_pane_view_top(win_id, pane_id, new_top, live_top);
                    }
                    return;
                }
                // Not dragging: update cursor pos + recompute the Cmd-hover URL
                // so the yellow hint / accent underline + pointer track the
                // cursor. Done here (free `self`) before the main match
                // re-borrows `child`. Mouse-down selection-drag still runs in the
                // main match below (it needs the renderer borrow).
                let mouse_down = self.windows.get(&win_id).map(|c| c.mouse_down).unwrap_or(false);
                if !mouse_down {
                    if let Some(c) = self.windows.get_mut(&win_id) {
                        c.cursor_pos = (position.x, position.y);
                    }
                    self.refresh_child_splitter_hover(win_id, position.x as f32, position.y as f32);
                    self.refresh_scrollbar_hover_from_cursor_in_child(win_id);
                    self.refresh_hovered_url_in_child(win_id);
                }
            }
            WindowEvent::ModifiersChanged(m) => {
                if let Some(c) = self.windows.get_mut(&win_id) {
                    c.modifiers = m.state();
                }
                self.refresh_hovered_url_in_child(win_id);
            }
            WindowEvent::Ime(ime_event) if self.command_palette_handle_ime(ime_event) => {
                // When: `command_palette_handle_ime` consumed the event, so the
                // palette owns this composition and the child must not see it.
                return;
            }
            _ => {
                // When: any other `event` needs no pre-match handling, so it
                // falls through to the main match below.
            }
        }
        if matches!(event, WindowEvent::RedrawRequested) {
            // Re-read before the long-lived palette/window borrows below so
            // PTY, CWD, and viewport changes revoke stale path authorization.
            self.refresh_target_hover(win_id);
        }
        // Split-borrow the palette out so the renderer can mutate it even though
        // `child` borrows `self.windows` below. Disjoint fields — safe. Computed
        // AFTER the scrollbar pre-match (which needs an unborrowed `self`).
        let broadcast_receivers = self.broadcast_receivers();
        let palette_for_render: Option<&mut CommandPalette> = if palette_here {
            Some(&mut self.command_palette)
        } else {
            // When: `palette_here` is false, so the palette belongs to another
            // window and this child must not draw it.
            None
        };
        let pty_event_proxy = self.event_loop_proxy.clone();
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the child was reaped
            // between the pre-match and here and has no state left to touch.
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                // When: `event` is `CloseRequested`, release this child's complete native and PTY state.
                let _ = child;
                self.close_child_window(win_id);
                // If this was the last child AND the main window had
                // been previously drained/hidden, nothing is alive
                // anymore — exit the loop.
                if self.should_exit() {
                    el.exit();
                }
            }
            WindowEvent::RedrawRequested => {
                // When: a `RedrawRequested` arrives, so this child either renders
                // now or defers to its own next frame boundary.

                // vsync coalescing gate — the torn-out child's mirror of the
                // main-window gate. A redraw that is neither input-driven
                // (`was_dirty`) nor carrying fresh PTY bytes (`pty_burst`), and
                // which lands inside the current vsync window of THIS child's own
                // frame clock, is deferred to the next frame boundary instead of
                // rendering now. `about_to_wait` turns the
                // `pending_redraw_windows` entry into a
                // `WaitUntil(child.last_render + frame_period)` and `new_events`
                // re-requests the redraw there. Net effect: a torn-out child
                // coalesces a PTY burst into one frame per vsync like the main
                // window, instead of rendering on every VT tick. Input-driven
                // redraws skip the gate so typing/resize/theme stay immediate.
                //
                // The cap is lowered while composing an IME preedit on the
                // software path.
                let child_frame_period = crate::app::effective_frame_period(
                    software_render_degrade,
                    child.ime.is_composing(),
                    frame_period,
                );
                if crate::app::should_defer_streaming_redraw(
                    was_dirty,
                    pty_burst,
                    software_render_degrade,
                    child.last_render.elapsed(),
                    child_frame_period,
                ) {
                    // When: `should_defer_streaming_redraw` says this frame lands
                    // inside the vsync window, so it waits for the next boundary.

                    // End the `child` / palette borrows on this path so the
                    // `&mut self` defer helper is callable.
                    let _ = child;
                    let _ = palette_for_render;
                    self.defer_child_redraw(win_id, was_dirty);
                    return;
                }
                let mut timing = crate::app::render_timing::RenderTiming::start("child");
                // Rendering this frame: drop any pending-deferral marker
                // so the frame-boundary wakeup loop stops re-requesting
                // redraws on this child (an idle re-request loop would be
                // the forbidden unconditional heartbeat redraw).
                self.pending_redraw_windows.remove(&win_id);
                child.tabs.clear_expired_command_badges(Instant::now());
                poll_command_events_for_child_window(child, &config);
                crate::app::refresh_window_tab_privileges(
                    &mut child.tabs,
                    &child.tab_states,
                    &mut child.panes,
                    !pty_burst,
                );
                if let Some(t) = timing.as_mut() {
                    t.lap("poll");
                }
                let tab_idx = child.tabs.active_index();
                let pane_rects: Vec<(u64, sonicterm_ui::pane::Rect)> = child
                    .tab_states
                    .get(tab_idx)
                    .and_then(|st| {
                        let r = child.renderer.as_ref()?;
                        let (w, h) = r.logical_size();
                        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
                        let bottom = r.bottom_inset();
                        let outer = sonicterm_ui::pane::Rect::new(
                            0.0,
                            top,
                            w.max(0.0),
                            (h - top - bottom).max(0.0),
                        );
                        Some(st.tree.layout(outer))
                    })
                    .unwrap_or_default();
                let active_id = child.tab_states.get(tab_idx).map(|st| st.active_pane).unwrap_or(0);
                if let Some(t) = timing.as_mut() {
                    t.lap("layout");
                }
                // try_lock EVERY pane in this child window's tab and pass them
                // all through to the renderer, so the frame shows one coherent
                // view rather than a mix of ages.
                let parser_arcs: Vec<(
                    u64,
                    std::sync::Arc<parking_lot::Mutex<sonicterm_vt::vt::Parser>>,
                    sonicterm_ui::pane::Rect,
                )> = pane_rects
                    .iter()
                    .filter_map(|(id, rect)| {
                        child.panes.get(id).map(|p| (*id, std::sync::Arc::clone(&p.parser), *rect))
                    })
                    .collect();
                let mut guards: Vec<(
                    u64,
                    parking_lot::MutexGuard<'_, sonicterm_vt::vt::Parser>,
                    sonicterm_ui::pane::Rect,
                )> = Vec::with_capacity(parser_arcs.len());
                let mut all_locked = true;
                for (id, arc, rect) in &parser_arcs {
                    match arc.try_lock() {
                        Some(g) => {
                            // When: `try_lock` yielded a guard, so this pane's
                            // parser is free and can be rendered this frame.
                            let g_ext: parking_lot::MutexGuard<'_, sonicterm_vt::vt::Parser> =
                                // SAFETY: `parser_arcs` owns the `Arc`s for the
                                // whole loop, outliving every guard in `guards`.
                                unsafe { std::mem::transmute(g) };
                            guards.push((*id, g_ext, *rect));
                        }
                        None => {
                            // When: `try_lock` found this parser held by the VT
                            // thread; abandon the frame rather than block on it.
                            all_locked = false;
                            break;
                        }
                    }
                }
                if let Some(t) = timing.as_mut() {
                    t.lap("try_lock");
                }
                if !all_locked {
                    // When: `all_locked` is false — a parser is held by the VT
                    // thread, so this frame defers rather than blocking on it.
                    drop(guards);
                    drop(parser_arcs);
                    // Lock-contention backoff: a bare `child.request_redraw()`
                    // here would re-enter this arm on the very next loop turn and
                    // re-contend the parser lock the VT thread holds almost
                    // continuously during a burst, busy-spinning through all
                    // per-frame setup and starving that thread. Deferring to the
                    // next frame boundary gives the VT thread roughly
                    // `frame_period` of uncontended drain. `was_dirty` is
                    // preserved so a deferred input-driven redraw still bypasses
                    // the coalescing gate.
                    let _ = child;
                    let _ = palette_for_render;
                    self.defer_child_redraw(win_id, was_dirty);
                    return;
                }
                // `try_lock`, never a blocking `lock`: the VT worker holds
                // this while merging a decoded batch, and blocking here would
                // stall the event loop behind it. On contention this defers
                // the redraw exactly as the parser locks do — a coherent view
                // of every pane matters more than one frame's latency.
                let mut inline_images_by_pane: std::collections::HashMap<
                    u64,
                    Vec<sonicterm_render_model::InlineImage>,
                > = std::collections::HashMap::new();
                let mut inline_images_locked = true;
                for (id, pane) in child.panes.iter() {
                    match pane.inline_images.try_lock() {
                        Some(images) => {
                            inline_images_by_pane.insert(*id, images.clone());
                        }
                        None => {
                            // When: `try_lock` found this pane's inline images
                            // held, so the frame defers instead of blocking.
                            inline_images_locked = false;
                            break;
                        }
                    }
                }
                if !inline_images_locked {
                    // When: `inline_images_locked` is false, so at least one pane
                    // would render stale media; defer the whole frame instead.
                    drop(inline_images_by_pane);
                    drop(guards);
                    self.defer_child_redraw(win_id, was_dirty);
                    return;
                }
                let viewport_tops: std::collections::HashMap<u64, Option<u64>> =
                    child.panes.iter().map(|(id, pane)| (*id, pane.viewport_top_abs)).collect();
                if let Some(t) = timing.as_mut() {
                    t.lap("inline_images");
                }
                if let Some(pane) = child.panes.get_mut(&active_id) {
                    // When: `panes` still holds `active_id`, so the frame has an
                    // active pane to draw the cursor, title and overlays against.
                    let active_pos = guards
                        .iter()
                        .position(|(id, _, _)| *id == active_id)
                        // PANIC: `active_id` must be a live visible leaf; `guards` covers the successfully locked layout.
                        .expect("active pane guard collected above");
                    invalidate_selection_for_content(
                        &mut child.selection,
                        &mut child.select_anchor,
                        active_id,
                        guards[active_pos].1.grid(),
                    );
                    // Run the same title formatter the main window uses, so OSC 7
                    // cwd and foreground-process probes flow into every window's
                    // tab bar uniformly instead of leaving a child on the literal
                    // "shell N" fallback.
                    let _ = crate::app::refresh_active_tab_title(
                        &mut child.tabs,
                        pane,
                        &guards[active_pos].1,
                        tab_idx,
                        !pty_burst,
                    );
                    if let Some(search) =
                        child.tab_states.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                    {
                        search.maybe_refresh_for_revision(guards[active_pos].1.grid_mut());
                    }
                    let search = child.tab_states.get(tab_idx).and_then(|t| t.search.as_ref());
                    if let Some(t) = timing.as_mut() {
                        t.lap("title_search");
                    }
                    // Compute the per-pane fade alpha so torn-out windows show
                    // the scrollbar and auto-hide it like the main window.
                    let scrollbar_now = Instant::now();
                    let scrollbar_alpha_map: std::collections::HashMap<u64, f32> = {
                        let mode = config.appearance.scrollbar;
                        let drag_pane = child.scrollbar_drag.as_ref().map(|s| s.pane_id);
                        let cursor = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
                        let rects: Vec<(u64, f32, f32, f32, f32)> =
                            pane_rects.iter().map(|(id, r)| (*id, r.x, r.y, r.w, r.h)).collect();
                        crate::app::scrollbar_visibility::update_and_collect(
                            &mut child.scrollbar_vis,
                            &rects,
                            cursor,
                            active_id,
                            drag_pane,
                            mode,
                            scrollbar_motion,
                            scrollbar_now,
                        )
                    };
                    let scrollbar_needs_more_frames = {
                        let mode = config.appearance.scrollbar;
                        let drag_pane = child.scrollbar_drag.as_ref().map(|s| s.pane_id);
                        child.scrollbar_vis.iter().any(|(id, st)| {
                            crate::app::scrollbar_visibility::is_animating(
                                st,
                                mode,
                                drag_pane == Some(*id),
                                scrollbar_motion,
                                scrollbar_now,
                            )
                        })
                    };
                    if let Some(t) = timing.as_mut() {
                        t.lap("scrollbar");
                    }
                    let mut panes_slice: Vec<sonicterm_render_model::PaneRender<'_>> = guards
                        .iter_mut()
                        .map(|(id, g, rect)| sonicterm_render_model::PaneRender {
                            id: *id,
                            rect_px: sonicterm_render_model::geometry::PixelRect {
                                x: rect.x as i32,
                                y: rect.y as i32,
                                w: rect.w as u32,
                                h: rect.h as u32,
                            },
                            grid: g.grid_mut(),
                            viewport_top_abs: viewport_tops.get(id).copied().flatten(),
                            is_active: *id == active_id,
                            cursor_style: sonicterm_render_model::CursorStyle::default(),
                            is_broadcast_receiver: broadcast_receivers.contains(id),
                            scrollbar_alpha: scrollbar_alpha_map.get(id).copied().unwrap_or(0.0),
                            inline_images: inline_images_by_pane
                                .get(id)
                                .cloned()
                                .unwrap_or_default(),
                        })
                        .collect();
                    if let Some(t) = timing.as_mut() {
                        t.lap("pane_slice");
                    }
                    // cursor_visible is per-pane (lives on
                    // PaneState). Read from the active pane (already
                    // borrowed mutably above) so the DECTCEM flag
                    // survives tear-out of this child.
                    let cursor_visible_now =
                        pane.cursor_visible.load(std::sync::atomic::Ordering::Relaxed);
                    let smoke_waiting_for_present = self
                        .runtime_smoke
                        .as_ref()
                        .is_some_and(|smoke| smoke.is_waiting_for_adopted_present(win_id));
                    let mut smoke_presented_count = None;
                    if let Some(r) = child.renderer.as_mut() {
                        r.set_render_timing_label("child");
                        if let Err(e) = r.render(
                            &mut panes_slice,
                            &theme,
                            cursor_visible_now && !palette_here,
                            child.selection.as_ref(),
                            child.copy_mode.as_ref(),
                            &child.tabs,
                            process_privileged,
                            search,
                            // The app-level command palette renders HERE when it
                            // was opened while this child window was OS
                            // frontmost, so it appears over the window the user
                            // opened it from rather than over main.
                            palette_for_render,
                            // Inline IME preedit at the child's terminal cursor —
                            // child windows self-draw the composition exactly
                            // like the main window, because the OS does not draw
                            // it for a terminal.
                            Some(&child.ime),
                            pane.viewport_top_abs,
                            child.notification.as_ref(),
                            // The child's own hovered-URL cells, so torn-out
                            // windows get the same yellow-hint /
                            // accent-when-Cmd underline and glyph recolor as the
                            // main window.
                            child.hovered_url.as_ref().map(|h| h.to_cells()),
                        ) {
                            tracing::warn!("child render error: {e}");
                            if smoke_waiting_for_present {
                                // Retain the presentation failure only while the adopted child proof is pending.
                                smoke_presented_count = Some(Err(RuntimeSmokeFailure::Present));
                            }
                        } else if smoke_waiting_for_present {
                            // When: rendering succeeded while `smoke_waiting_for_present` is true, retain its frame count.
                            smoke_presented_count = Some(Ok(r.successful_frame_count()));
                        }
                    }
                    if let Some(t) = timing.as_mut() {
                        t.lap("render");
                    }
                    if let Some(presented) = smoke_presented_count {
                        // When: `smoke_presented_count` contains `presented`, classify the adopted child frame.
                        let count = match presented {
                            Ok(count) => count,
                            Err(failure) => {
                                // When: `presented` is `Err(failure)`, retain it and stop the smoke.
                                if let Some(smoke) = self.runtime_smoke.as_mut() {
                                    smoke.fail(failure);
                                }
                                el.exit();
                                return;
                            }
                        };
                        let presented = self
                            .runtime_smoke
                            .as_mut()
                            .is_some_and(|smoke| smoke.observe_adopted_present(win_id, count));
                        if presented {
                            // When: `presented` is true, release the adopted child after dropping every frame borrow.
                            // Teardown workers may need the parser lock held by this frame.

                            drop(panes_slice);
                            drop(guards);
                            drop(parser_arcs);
                            let _ = pane;
                            let _ = child;
                            let released = self.close_child_window(win_id);
                            self.warm_window_pool.clear();
                            let complete = self
                                .runtime_smoke
                                .as_mut()
                                .is_some_and(|smoke| smoke.finish_warm_release(win_id, released));
                            tracing::info!(
                                released,
                                "runtime smoke warm renderer adopted and child released"
                            );
                            if !complete {
                                tracing::error!(
                                    "runtime smoke could not prove adopted child release"
                                );
                            }
                            el.exit();
                            return;
                        }
                    }
                    let first_render_at = Instant::now();
                    if let Some(tear) = child.pending_tear_out_timing.take() {
                        tracing::warn!(
                            target: "tear_out_timing",
                            source = tear.source,
                            create_window_ms = tear.create_window_ms,
                            renderer_init_ms = tear.renderer_init_ms,
                            resize_ms = tear.resize_ms,
                            install_ms = tear.install_ms,
                            first_render_total_ms = tear.total_until_first_render_ms(first_render_at),
                            "tear-out latency breakdown"
                        );
                    }
                    child.last_render = first_render_at;
                    // close the coalescing gate for the next
                    // streaming redraw. `input_dirty` is the shared
                    // main+child carve-out flag (see window_event.rs
                    // pre-dispatch block) — clear it now that this child
                    // has serviced the input-driven frame. Recording the
                    // burst gen sampled at the top of THIS handler as
                    // `last_seen_burst_gen` means a redraw arriving before
                    // the next PTY chunk has `pty_burst == false` and
                    // coalesces; a burst landing mid-render keeps the
                    // counter ahead so the next redraw still renders.
                    // Disjoint `self` fields — safe alongside the live
                    // `child`/`pane` borrows (mirror of the
                    // `self.os_drag_bars.publish` access below).
                    self.input_dirty = false;
                    self.last_seen_burst_gen = pty_burst_snapshot;
                    // Tell the OS where the child's active text cursor lives so
                    // the IME candidate window (pinyin/romaji/Hangul) appears
                    // under the edited cell instead of pinned to the screen's
                    // top-left. Throttled via the child's own ImeCursorThrottle.
                    // The active pane guard is still held here, so read the
                    // cursor cell from it.
                    {
                        let (cur_row, cur_col) = {
                            let g = guards[active_pos].1.grid_mut();
                            (g.cursor.row, g.cursor.col)
                        };
                        if let (Some(win), Some(r)) =
                            (child.window.as_ref(), child.renderer.as_ref())
                        {
                            if palette_here && self.command_palette.is_open() {
                                let mut palette = self.command_palette.clone();
                                let size = win.inner_size();
                                let scale = r.scale_factor();
                                let font_size = r.font_size() * scale;
                                if let Some(layout) = PaletteLayout::compute(
                                    &mut palette,
                                    size.width as f32,
                                    size.height as f32,
                                    config.appearance.panel_padding,
                                    scale,
                                ) {
                                    let prefix = command_palette_query_caret_prefix(
                                        &palette,
                                        child.ime.preedit(),
                                    );
                                    let text_x = layout.query_row.x + PALETTE_ROW_PAD_X * scale;
                                    let caret_x =
                                        text_x + estimate_overlay_text_width(&prefix, font_size);
                                    win.set_ime_cursor_area(
                                        winit::dpi::PhysicalPosition::new(
                                            caret_x as i32,
                                            layout.query_row.y as i32,
                                        ),
                                        winit::dpi::PhysicalSize::new(
                                            r.cell_w.ceil() as u32,
                                            layout.query_row.h.ceil() as u32,
                                        ),
                                    );
                                }
                            } else if let Some(search) = search {
                                // When: a `search` box is open, so the candidate
                                // window anchors to its caret, not the grid cursor.
                                let preedit = child.ime.preedit();
                                let search_label = search_bar_label(search, preedit);
                                let search_prefix = search_query_caret_prefix(search, preedit);
                                let window_size = win.inner_size();
                                let scale = r.scale_factor();
                                let font_size = r.font_size() * scale;
                                let icon_w =
                                    r.measure_overlay_text_width(SEARCH_BADGE_ICON, font_size);
                                let content_w = icon_w
                                    + SEARCH_BAR_ICON_GAP * scale
                                    + r.measure_overlay_text_width(&search_label, font_size);
                                let row = u8::from(
                                    child.copy_mode.as_ref().is_some_and(|cm| cm.is_read_only()),
                                );
                                let layout = SearchBarLayout::compute_at_row(
                                    window_size.width as f32,
                                    window_size.height as f32,
                                    content_w,
                                    row,
                                    scale,
                                );
                                let text_x = layout.border.x
                                    + SEARCH_BAR_PAD_LEFT * scale
                                    + icon_w
                                    + SEARCH_BAR_ICON_GAP * scale;
                                let right_edge = (layout.border.x + layout.border.w
                                    - SEARCH_BAR_PAD_RIGHT * scale)
                                    .max(text_x);
                                let prefix_w =
                                    r.measure_overlay_text_width(&search_prefix, font_size);
                                let caret_x = (text_x + prefix_w).clamp(text_x, right_edge);
                                let pos = winit::dpi::PhysicalPosition::new(
                                    caret_x as i32,
                                    layout.border.y as i32,
                                );
                                let size = winit::dpi::PhysicalSize::new(
                                    r.cell_w.ceil() as u32,
                                    layout.border.h.ceil() as u32,
                                );
                                win.set_ime_cursor_area(pos, size);
                            } else if child.ime_cursor_throttle.should_update(cur_row, cur_col) {
                                // When: `ime_cursor_throttle.should_update` allows
                                // it, so the OS learns the new terminal cell.
                                let x = r.padding_left_px() + f32::from(cur_col) * r.cell_w;
                                let y = r.top_inset() + f32::from(cur_row) * r.cell_h;
                                let pos = winit::dpi::PhysicalPosition::new(x as i32, y as i32);
                                let size = winit::dpi::PhysicalSize::new(
                                    r.cell_w.ceil() as u32,
                                    r.cell_h.ceil() as u32,
                                );
                                win.set_ime_cursor_area(pos, size);
                            }
                        }
                    }
                    // Publish this child's tab bar snapshot for cross-window OS
                    // drag hit-tests. See `App::publish_child_window_tab_bar` for
                    // the rationale on the main-window mirror.
                    {
                        let Some(win) = child.window.as_ref() else {
                            // When: this child has no `window`, so there is no
                            // screen origin to anchor a drag snapshot to.
                            return;
                        };
                        let inner_origin =
                            win.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
                        let isz = win.inner_size();
                        let inner_size = (isz.width, isz.height);
                        let raster_w = inner_size.0 as f32;
                        let Some(r) = child.renderer.as_ref() else {
                            // When: this child has no `renderer`, so tab-bar
                            // height and visibility are unknown.
                            return;
                        };
                        let layout = TabBarLayout::compute_with_height(
                            &child.tabs,
                            raster_w,
                            r.tab_bar_logical_height(),
                        )
                        .with_top_offset(r.tab_bar_y_offset())
                        .with_visible(r.tab_bar_visible());
                        let snap = crate::app::os_drag::TabBarSnapshot::from_layout(
                            Some(win_id),
                            inner_origin,
                            inner_size,
                            &layout,
                        );
                        self.os_drag_bars.publish(snap);
                    }
                    if let Some(t) = timing {
                        t.finish();
                    }
                    if scrollbar_needs_more_frames {
                        child.request_redraw();
                    }
                }
            }
            WindowEvent::Resized(size)
                if resize_renderer_and_split_panes(child, size.width, size.height) =>
            {
                // Cell geometry changed — force the next render to re-publish the
                // IME cursor area even if (row, col) is unchanged, else the OS
                // candidate window stays at the pre-resize pixel location.
                child.ime_cursor_throttle.reset();
                child.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor: dpi_scale, mut inner_size_writer } => {
                // When: ScaleFactorChanged arrives for a child, commit the same synchronous physical target as the main path.
                let _ = crate::app::apply_window_dpi_transition(
                    child,
                    dpi_scale,
                    &mut inner_size_writer,
                );
            }
            WindowEvent::ModifiersChanged(m) => {
                child.modifiers = m.state();
            }
            WindowEvent::Focused(focused) => {
                // When: a `Focused` change arrives, so menubar-routed actions
                // (Cmd+T, ...) target this child window instead of the main App.

                // Release the child borrow before touching `self`.
                let _ = child;
                self.handle_child_focus_changed(win_id, focused);
            }
            WindowEvent::CursorLeft { .. } => {
                // The pointer left this child, so every hover highlight it owns
                // must be dropped: URL, scrollbar and tab-bar alike.

                // Drop path authorization and all target visuals when the pointer leaves.
                child.path_probe.invalidate();
                if child.hovered_url.take().is_some() || child.hover_link {
                    child.hover_link = false;
                    child.request_redraw();
                }
                if crate::app::scrollbar_visibility::clear_hover_states(&mut child.scrollbar_vis) {
                    child.request_redraw();
                }
                if let Some(r) = child.renderer.as_mut() {
                    let changed = r.set_hover_cursor(None);
                    if changed {
                        child.request_redraw();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // When: a `CursorMoved` reaches the main match, so no drag was in
                // flight and this move drives hover, drag chips and selection.
                child.cursor_pos = (position.x, position.y);
                let pointer_cell = child
                    .renderer
                    .as_ref()
                    .and_then(|renderer| {
                        renderer.pixel_to_pane_cell(position.x as f32, position.y as f32)
                    })
                    .map(|(pane_id, row, col)| PointerCell { pane_id, row, col });
                let pointer_route = if child.mouse_down {
                    let modifiers = child.modifiers;
                    child.pointer_gesture.as_mut().map(|gesture| {
                        super::window_event::route_pressed_pointer_motion(
                            gesture,
                            pointer_cell,
                            modifiers,
                        )
                    })
                } else {
                    // When: `child.mouse_down` is false, child chrome may suppress no-button terminal motion.
                    let scrollbar_owned = pointer_cell.is_some_and(|cell| {
                        let pane = App::compute_pane_rects_for(child)
                            .into_iter()
                            .find_map(|(id, rect)| (id == cell.pane_id).then_some(rect));
                        pane.is_some_and(|pane| {
                            let (edge_active, visible) = child
                                .scrollbar_vis
                                .get(&cell.pane_id)
                                .map_or((false, false), |state| {
                                    (
                                        state.mouse_near_right_edge,
                                        state.alpha
                                            > crate::app::scrollbar_visibility::ALPHA_EMIT_FLOOR,
                                    )
                                });
                            let (content, gutter_width) = child.renderer.as_ref().map_or(
                                (pane, crate::app::scrollbar_input::SCROLLBAR_WIDTH_PX),
                                |renderer| {
                                    let content =
                                        super::window_event::pointer_scrollbar_content_rect(
                                            pane,
                                            [
                                                renderer.padding_left_px(),
                                                renderer.padding_right_px(),
                                                renderer.padding_top_px(),
                                                renderer.padding_bottom_px(),
                                            ],
                                            renderer.cell_size(),
                                        );
                                    (
                                        content,
                                        crate::app::scrollbar_input::SCROLLBAR_WIDTH_PX
                                            * renderer.scale_factor(),
                                    )
                                },
                            );
                            super::window_event::native_scrollbar_owns_pointer(
                                config.appearance.scrollbar,
                                content,
                                position.x as f32,
                                position.y as f32,
                                gutter_width,
                                edge_active,
                                visible,
                            )
                        })
                    });
                    pointer_cell.and_then(|cell| {
                        child.panes.get(&cell.pane_id).and_then(|pane| {
                            let parser = pane.parser.lock();
                            let (tracking, sgr) =
                                super::window_event::parser_mouse_profile(&parser);
                            child_no_button_motion_report(
                                child,
                                cell,
                                tracking,
                                sgr,
                                scrollbar_owned,
                            )
                        })
                    })
                };
                if let Some(route) = pointer_route {
                    // When: `pointer_route` exists, its latched or live owner decides whether child motion reaches the PTY.
                    match route {
                        super::window_event::PointerMotionRoute::Local => {
                            // When: `route` is Local, continue into child selection motion.
                        }
                        super::window_event::PointerMotionRoute::None => {
                            // When: `route` is None, Button mode consumes the child move without bytes.
                            return;
                        }
                        report @ super::window_event::PointerMotionRoute::Report { .. } => {
                            // When: `route` contains Report data, encode terminal motion for the child pane.
                            let kind = if child.mouse_down {
                                super::window_event::PointerReportKind::HeldLeftMotion
                            } else {
                                super::window_event::PointerReportKind::NoButtonMotion
                            };
                            let report = super::window_event::pointer_route_bytes(report, kind);
                            // Drop every child/parser/renderer borrow before the
                            // bounded effect path resolves and enqueues the PTY write.
                            let _ = child;
                            if let Some((pane_id, bytes)) = report {
                                self.write_to_pane(
                                    pane_id,
                                    bytes,
                                    super::PtyInputSource::PointerMotion,
                                );
                            }
                            return;
                        }
                    }
                }
                let Some(r) = child.renderer.as_mut() else {
                    // When: this child has no `renderer`, so pointer pixels
                    // cannot be resolved to cells or tab-bar geometry.
                    return;
                };
                let (lx, ly) = (position.x as f32, position.y as f32);
                // The child drives tab hover through its OWN renderer so each
                // torn-out window repaints independently.
                if r.set_hover_cursor(Some((lx, ly))) {
                    if let Some(w) = child.window.as_ref() {
                        w.request_redraw();
                    }
                }
                if let Some(s) = child.drag_session.as_mut() {
                    s.current_pos = (lx, ly);
                    let title = child
                        .tabs
                        .tabs()
                        .get(s.press_tab_index)
                        .map(|t| t.title.clone())
                        .unwrap_or_default();
                    let session_snapshot = *s;
                    let bar_width = r.width() as f32;
                    let layout = TabBarLayout::compute_with_height(
                        &child.tabs,
                        bar_width,
                        r.tab_bar_logical_height(),
                    )
                    .with_top_offset(r.tab_bar_y_offset())
                    .with_visible(r.tab_bar_visible());
                    let chip =
                        crate::tab_drag::build_drag_chip_overlay(&session_snapshot, &layout, title);
                    r.set_drag_chip(chip);
                }
                // Cross-window drag-merge from child: when a tab in the
                // child's bar is held, look for a destination on another
                // window (main or sibling). The final action (tear /
                // merge / cancel) is deferred to mouse-up.
                if child.mouse_down && child.pressed_tab.is_some() {
                    // When: `mouse_down` with a `pressed_tab`, so this move is a
                    // tab drag and only records a target until mouse-up.
                    let local = (position.x, position.y);
                    // child borrow ends at last use; safe to call &mut self next
                    let _ = child;
                    let tgt = self.compute_child_drag_target(win_id, local);
                    if let Some(c) = self.windows.get_mut(&win_id) {
                        c.drag_target = tgt;
                        c.request_redraw();
                    }
                    return;
                }
                if child.mouse_down {
                    if let Some((row, col)) = r.pixel_to_cell(position.x as f32, position.y as f32)
                    {
                        // Drag granularity: the press set `select_mode` +
                        // `select_anchor` (ABSOLUTE row); extend by Cell / Word /
                        // Line. Word/Line recompute the region from the live grid
                        // via try_lock, dropping the lock before redraw and
                        // converting the viewport `row` to absolute internally.
                        // `r`'s last use was pixel_to_cell, so the &child borrows
                        // below are fine.
                        let replacement = match child.select_mode {
                            SelectMode::Word => {
                                Some(child.word_drag_selection(child.select_anchor, row, col))
                            }
                            SelectMode::Line => {
                                Some(child.line_drag_selection(child.select_anchor.0, row))
                            }
                            SelectMode::Cell => None,
                        };
                        let cell_replacement = if matches!(child.select_mode, SelectMode::Cell) {
                            child.cell_drag_selection(child.select_anchor, row, col)
                        } else {
                            // When: `matches!(child.select_mode, SelectMode::Cell)` is false, `replacement` owns the word/line range.
                            None
                        };
                        if let Some(sel) = child.selection.as_mut() {
                            match child.select_mode {
                                SelectMode::Cell => {
                                    if !sel.anchored {
                                        if let Some(new_sel) = cell_replacement {
                                            *sel = new_sel;
                                            mark_all_panes_dirty(&child.panes);
                                            child.request_redraw();
                                        }
                                    }
                                }
                                SelectMode::Word | SelectMode::Line => {
                                    // Replace with the recomputed region; skip
                                    // on a busy parser (Some(None)) — never
                                    // shrink below the anchor word/line.
                                    if let Some(Some(new_sel)) = replacement {
                                        *sel = new_sel;
                                        mark_all_panes_dirty(&child.panes);
                                        child.request_redraw();
                                    }
                                }
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Route the tick to the pane under the cursor rather than the
                // active one, and on the alt screen translate to SGR/X10 wheel
                // reports (mouse tracking on) or arrow keys (off); otherwise
                // scroll that pane's scrollback.
                let (lx, ly) = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
                let cell_h = child
                    .renderer
                    .as_ref()
                    .map(|r| r.cell_size().1)
                    .filter(|h| *h > 0.0)
                    .unwrap_or(16.0);
                let lines_per_tick: f32 = 3.0;
                let delta_lines_f: f32 = match delta {
                    MouseScrollDelta::LineDelta(_x, y) => -y * lines_per_tick,
                    MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32) / cell_h,
                };
                let delta_lines = if delta_lines_f >= 0.0 {
                    delta_lines_f.ceil() as i32
                } else {
                    // When: `delta_lines_f` is negative, so rounding must go away
                    // from zero to keep a small upward tick from vanishing.
                    delta_lines_f.floor() as i32
                };
                if delta_lines != 0 {
                    if let Some(pane_id) = child_pane_at_cursor(child, lx, ly) {
                        let cell = child.renderer.as_ref().and_then(|r| r.pixel_to_cell(lx, ly));
                        let (is_alt, tracking, sgr, app_cursor) = child
                            .panes
                            .get(&pane_id)
                            .map(|pane| {
                                let parser = pane.parser.lock();
                                let (tracking, sgr) =
                                    super::window_event::parser_mouse_profile(&parser);
                                (
                                    parser.grid().is_alt(),
                                    tracking,
                                    sgr,
                                    parser.application_cursor_keys(),
                                )
                            })
                            .unwrap_or((false, sonicterm_vt::vt::MouseTracking::Off, false, false));
                        if is_alt && tracking != sonicterm_vt::vt::MouseTracking::Off {
                            let up = delta_lines < 0;
                            let (col1, row1) =
                                cell.map(|(r, c)| (c as u32 + 1, r as u32 + 1)).unwrap_or((1, 1));
                            let count = delta_lines.unsigned_abs() as usize;
                            let payload =
                                super::window_event::wheel_report_bytes(sgr, up, col1, row1, count);
                            if let Some(pane) = child.panes.get(&pane_id) {
                                if let Some(pty) = pane.pty.as_ref() {
                                    Self::queue_pty_input(
                                        pty_event_proxy.as_ref(),
                                        pty,
                                        pane_id,
                                        super::PtyInputSource::Wheel,
                                        payload,
                                    );
                                }
                            }
                        } else if is_alt {
                            // When: `is_alt` without tracking, so the wheel
                            // translates to arrow keys the TUI already reads.
                            let up = delta_lines < 0;
                            let seq: &[u8] = match (app_cursor, up) {
                                (true, true) => b"\x1bOA",
                                (true, false) => b"\x1bOB",
                                (false, true) => b"\x1b[A",
                                (false, false) => b"\x1b[B",
                            };
                            let count = delta_lines.unsigned_abs() as usize;
                            let mut payload = Vec::with_capacity(seq.len() * count);
                            for _ in 0..count {
                                payload.extend_from_slice(seq);
                            }
                            if let Some(pane) = child.panes.get(&pane_id) {
                                if let Some(pty) = pane.pty.as_ref() {
                                    Self::queue_pty_input(
                                        pty_event_proxy.as_ref(),
                                        pty,
                                        pane_id,
                                        super::PtyInputSource::Wheel,
                                        payload,
                                    );
                                }
                            }
                        } else {
                            // When: `is_alt` is false, so the pane has real
                            // scrollback and the wheel moves its view.
                            scroll_child_pane(child, pane_id, delta_lines);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                // When: a left `MouseInput` reaches the main match, so `state`
                // selects between beginning a gesture and resolving one.

                match state {
                    ElementState::Pressed => {
                        // When: the button was `Pressed`, so tab-bar hits, pane focus
                        // and a new selection anchor are resolved here.
                        let Some(r) = child.renderer.as_ref() else {
                            // When: this child has no `renderer`, so neither tab-bar
                            // layout nor cell coordinates can be computed.
                            return;
                        };
                        let (px, py) = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
                        let bar_width = r.width() as f32;
                        let layout = TabBarLayout::compute_with_height(
                            &child.tabs,
                            bar_width,
                            r.tab_bar_logical_height(),
                        )
                        .with_top_offset(r.tab_bar_y_offset())
                        .with_visible(r.tab_bar_visible());
                        if let Some(hit) = layout.hit(px, py) {
                            // When: `layout.hit` reports a tab-bar `hit`, so the press
                            // belongs to the bar and never reaches the grid.
                            match hit {
                                TabHit::Activate(i) => {
                                    child.tabs.activate(i);
                                    resize_visible_panes_in_child(child);
                                    child.pressed_tab = Some(i);
                                    child.mouse_down = true;
                                    child.drag_session =
                                        Some(crate::tab_drag::DragSession::new(i, (px, py)));
                                }
                                TabHit::Close(idx) => {
                                    // When: `TabHit::Close` at `idx`, so the × was
                                    // clicked and that tab closes immediately.

                                    // Drop the &mut child borrow before re-entering
                                    // &mut self via helpers. `close_tab_at_in_child`
                                    // performs the reap itself.
                                    let _ = child;
                                    self.close_tab_at_in_child(win_id, idx);
                                    if let Some(c) = self.windows.get(&win_id) {
                                        c.request_redraw();
                                    }
                                    return;
                                }
                            }
                            child.request_redraw();
                            return;
                        }
                        child.mouse_down = true;
                        child.pointer_gesture = None;
                        let (px, py) = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
                        let pane_rects = App::compute_pane_rects_for(child);
                        // Pane and cell must come from one renderer snapshot; app
                        // geometry is only the early-render fallback for pane id 0.
                        let pixel_target = r.pixel_to_pane_cell(px, py);
                        let geometry_pane = pane_id_at_point(&pane_rects, px, py);
                        let pointer_cell = pixel_target.and_then(|(rendered_pane, row, col)| {
                            (rendered_pane != 0)
                                .then_some(rendered_pane)
                                .or(geometry_pane)
                                .map(|pane_id| PointerCell { pane_id, row, col })
                        });
                        let pane_focus_change = pointer_cell
                            .and_then(|cell| child.begin_pointer_pane_focus_change(cell.pane_id));
                        if let Some(pointer_cell) = pointer_cell {
                            // When: `pointer_cell` resolves a rendered grid cell, route focus and press through its exact pane.
                            let PointerCell { pane_id, row, col } = pointer_cell;
                            if pane_id != 0 {
                                // When: `pane_id` is nonzero, snapshot that live pane's terminal mouse profile.
                                let (tracking, sgr) = child
                                    .panes
                                    .get(&pane_id)
                                    .map(|pane| {
                                        let parser = pane.parser.lock();
                                        super::window_event::parser_mouse_profile(&parser)
                                    })
                                    .unwrap_or((sonicterm_vt::vt::MouseTracking::Off, false));
                                let terminal_press =
                                    child.begin_pointer_press(pointer_cell, tracking, sgr);
                                if let Some(bytes) = terminal_press {
                                    // When: `terminal_press` contains bytes, the child latched terminal ownership before the unguarded enqueue.
                                    if let Some(change) = pane_focus_change {
                                        child.finish_pane_focus_change(change);
                                    }
                                    let _ = child;
                                    self.write_to_pane(
                                        pane_id,
                                        bytes,
                                        super::PtyInputSource::PointerButton,
                                    );
                                    return;
                                }
                            }
                            // Multi-click selection: 1 = point, 2 = word,
                            // 3 = line. Mirrors the main-window path in
                            // window_event.rs. `multi_click_selection` locks
                            // the active pane's parser only to read the grid
                            // and returns an owned (Copy) Selection, so no grid
                            // lock is held across the assignment / redraw
                            // (CLAUDE.md §4). For count == 1 it returns the same
                            // point Selection as before — single-click is
                            // unchanged. (`r`'s last use was pixel_to_cell
                            // above, so the &mut child borrows below are fine.)
                            let count = child.register_click(row, col);
                            // Resolve absolute row and content baseline under one
                            // parser lock so a fresh selection cannot inherit older
                            // changed-row state.
                            let selection_state = child.viewport_row_selection_state(row);
                            let abs_row = selection_state.map_or(row as u64, |state| state.0);
                            let sel = if count < 2 {
                                selection_state.map_or_else(
                                    || Selection::new(abs_row, col),
                                    |(_, pane_id, seq, is_alt, evicted)| {
                                        Selection::new(abs_row, col)
                                            .with_content_state(pane_id, seq, is_alt, evicted)
                                    },
                                )
                            } else {
                                // When: `count` reached two or more, so the click is a
                                // word or line select rather than a single point.
                                child.multi_click_selection(count, abs_row, col)
                            };
                            // Record drag granularity + anchor cell so a
                            // held-button CursorMoved extends by cell / word /
                            // line. The anchor row is ABSOLUTE.
                            child.select_mode = match count {
                                2 => SelectMode::Word,
                                3 => SelectMode::Line,
                                _ => SelectMode::Cell,
                            };
                            child.select_anchor = (abs_row, col);
                            child.selection = Some(sel);
                            if pane_focus_change.is_none() {
                                mark_all_panes_dirty(&child.panes);
                            }
                        }
                        if let Some(change) = pane_focus_change {
                            child.finish_pane_focus_change(change);
                        } else {
                            // When: `pane_focus_change` is `None`, no focus transition
                            // owns the final redraw request, so issue it directly.
                            child.request_redraw();
                        }
                    }
                    ElementState::Released => {
                        // When: the button was `Released`, so any drag, selection or
                        // tab-move started by the press is resolved here.
                        let modifiers = child.modifiers;
                        let terminal_owned = matches!(
                            child.pointer_gesture.as_ref().map(|gesture| gesture.owner),
                            Some(PointerGestureOwner::Terminal { .. })
                        );
                        let pointer_release = super::window_event::take_pointer_release(
                            &mut child.pointer_gesture,
                            modifiers,
                        );
                        let release_report = pointer_release.and_then(|route| {
                            super::window_event::pointer_route_bytes(
                                route,
                                super::window_event::PointerReportKind::LeftRelease,
                            )
                        });
                        if terminal_owned {
                            // When: `terminal_owned` is true, consume state before bounded enqueue so rejection cannot relatch it.
                            child.mouse_down = false;
                            child.scrollbar_drag = None;
                            child.splitter_drag = None;
                            child.request_redraw();
                            let _ = child;
                            if let Some((pane_id, bytes)) = release_report {
                                self.write_to_pane(
                                    pane_id,
                                    bytes,
                                    super::PtyInputSource::PointerButton,
                                );
                            }
                            return;
                        }
                        let session = child.drag_session.take();
                        let foreign = child.drag_target.take();
                        let pressed = child.pressed_tab.take();
                        child.mouse_down = false;
                        // End any in-flight scrollbar thumb drag.
                        if child.scrollbar_drag.take().is_some() {
                            child.request_redraw();
                        }
                        // End any in-flight splitter divider drag and restore the
                        // default cursor.
                        if child.splitter_drag.take().is_some() {
                            if let Some(w) = child.window.as_ref() {
                                w.set_cursor(CursorIcon::Default);
                            }
                            child.request_redraw();
                        }
                        if let Some(r) = child.renderer.as_mut() {
                            r.set_drag_chip(None);
                        }
                        if let Some(sel) = child.selection.as_ref() {
                            if sel.is_empty() {
                                child.selection = None;
                                mark_all_panes_dirty(&child.panes);
                                child.request_redraw();
                            }
                        }
                        if let (Some(s), Some(src_idx)) = (session, pressed) {
                            // When: both a drag `session` and a `pressed` tab index
                            // survived, so this release ends a real tab drag.
                            let Some(r) = child.renderer.as_ref() else {
                                // When: this child has no `renderer`, so no tab-bar
                                // layout exists to resolve the drop against.
                                return;
                            };
                            let bar_width = r.width() as f32;
                            let layout = TabBarLayout::compute_with_height(
                                &child.tabs,
                                bar_width,
                                r.tab_bar_logical_height(),
                            )
                            .with_top_offset(r.tab_bar_y_offset());
                            let action = crate::tab_drag::compute_action(&s, foreign, &layout);
                            // Release the child borrow before re-entering
                            // &mut self via the merge / tear path.
                            let _ = child;
                            match action {
                                crate::tab_drag::DragAction::ReturnToOriginalBar => {
                                    // When: `ReturnToOriginalBar` — the tab was
                                    // dropped where it started, so nothing moves.
                                }
                                crate::tab_drag::DragAction::ReorderTab { from, to } => {
                                    // Re-borrow via self.windows because the
                                    // `let _ = child;` above released the
                                    // long-lived mut borrow.
                                    if let Some(c) = self.windows.get_mut(&win_id) {
                                        c.tabs.reorder(from, to);
                                        if from < c.tab_states.len() && to < c.tab_states.len() {
                                            let st = c.tab_states.remove(from);
                                            c.tab_states.insert(to, st);
                                        }
                                        c.request_redraw();
                                    }
                                }
                                crate::tab_drag::DragAction::MergeIntoWindow(target) => {
                                    self.merge_child_into_target(win_id, src_idx, target);
                                }
                                crate::tab_drag::DragAction::TearOutToNewWindow { .. } => {
                                    // Tear out from a child window into a new
                                    // top-level window. The Tab + PaneState (incl.
                                    // PtyHandle) move rather than clone, so the
                                    // shell keeps running as the same child PID.
                                    self.tear_out_from_child(el, win_id, src_idx);
                                }
                            }
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // When: KeyboardInput supplies event to this child, releases
                // complete prior routes while presses pass through local owners.
                if event.state == ElementState::Released {
                    // When: event.state is Released, send it only when this physical
                    // key's press was previously forwarded to these terminals.
                    let targets = child.pty_pressed_keys.remove(&event.physical_key);
                    let mods = child.modifiers;
                    let _ = child;
                    if let Some(targets) = targets {
                        // Every pane that received the press independently
                        // encodes its release using its live protocol.
                        let writes = self.encoded_terminal_key_writes(&event, mods, &targets);
                        self.dispatch_terminal_key_writes(writes);
                    }
                    return;
                }
                if let Some(targets) = super::window_event::terminal_repeat_targets(
                    &child.pty_pressed_keys,
                    event.physical_key,
                    event.repeat,
                ) {
                    // When: terminal_repeat_targets returns targets, this repeat keeps
                    // its original destinations even if child-local UI opened later.
                    let mods = child.modifiers;
                    let _ = child;
                    let writes = self.encoded_terminal_key_writes(&event, mods, &targets);
                    self.dispatch_terminal_key_writes(writes);
                    return;
                }
                // A KeyboardInput press makes this child frontmost and routes below.
                self.frontmost_window = Some(win_id);
                if let Some(key_str) = key_event_to_string(&event, child.modifiers) {
                    // When: the press maps to a `key_str`, so it can be matched
                    // against the quit chord before any other routing.
                    if super::window_event::is_quit_chord(&key_str, self.keymap.lookup(&key_str)) {
                        // When: `is_quit_chord` matched, so quit handling owns
                        // this press and it never reaches the PTY.
                        let _ = child;
                        self.on_quit_chord_pressed(event.repeat);
                        return;
                    }
                }
                if child.copy_mode.is_some() {
                    // When: `copy_mode` is active, so keys navigate the scrollback
                    // instead of reaching the PTY.
                    if child.copy_mode.as_ref().is_some_and(|mode| mode.is_read_only()) {
                        // When: `is_read_only` — only whitelisted actions may run,
                        // so each key is checked against that list first.
                        let child_mods = child.modifiers;
                        let _ = child;
                        for key_str in key_event_to_strings(&event, child_mods) {
                            if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                                // When: `keymap.lookup` bound this `key_str`, so
                                // the action is tested against the READONLY list.
                                if super::keymap_dispatch::read_only_allows_action(&action)
                                    && self.run_action_for_window(&action, win_id)
                                {
                                    // When: `read_only_allows_action` passed and
                                    // the action ran, so the key is consumed.
                                    self.drain_pending_window_creates(el);
                                    if let Some(c) = self.windows.get(&win_id) {
                                        c.request_redraw();
                                    }
                                    return;
                                }
                            }
                        }
                        let Some(child) = self.windows.get_mut(&win_id) else {
                            // When: `windows` lost `win_id` while an action ran,
                            // so there is no child left to hand the key to.
                            return;
                        };
                        child_copy_mode_handle_key(child, &event);
                        child.request_redraw();
                    } else {
                        // When: copy mode is not read-only, so every key goes
                        // straight to the copy-mode key handler.
                        child_copy_mode_handle_key(child, &event);
                        child.request_redraw();
                    }
                    return;
                }
                // When the command palette is attached to THIS child window,
                // route the keystroke into the overlay handler exactly like the
                // main window does. Without this branch a key pressed while the
                // palette is open would reach the PTY instead of filtering the
                // palette query.
                let palette_here = self.palette_attached_window == Some(win_id);
                if palette_here {
                    // When: `palette_here` — the palette overlay owns this
                    // child's keystrokes until it closes.
                    let child_mods = child.modifiers;
                    let _ = child;
                    if let Some(key_str) = key_event_to_string(&event, child_mods) {
                        // When: the press maps to a `key_str`, so it can be
                        // checked for the palette's own toggle binding.
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            // When: `keymap.lookup` bound this `key_str`, so a
                            // toggle can close the palette rather than filter it.
                            if matches!(action, Action::OpenCommandPalette) {
                                // When: `matches` the palette toggle, so the
                                // action runs instead of editing the query.
                                self.run_action_for_window(&action, win_id);
                                self.drain_pending_window_creates(el);
                                if let Some(c) = self.windows.get(&win_id) {
                                    c.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.command_palette_handle_key(&event);
                    self.drain_pending_window_creates(el);
                    if let Some(c) = self.windows.get(&win_id) {
                        c.request_redraw();
                    }
                    return;
                }
                // While an IME composition is in flight the OS owns the
                // keystrokes — they arrive as Ime events instead, so forwarding
                // them here would double-type. Esc cancels the composition (no
                // bytes to the PTY). Mirrors the main-window guard.
                if child.ime.is_composing() {
                    // When: `ime.is_composing()` — the OS owns these keys, so
                    // forwarding them to the PTY would double-type the text.
                    if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                        // Escape cancels the in-flight composition; no bytes
                        // reach the PTY on this path.
                        child.ime.cancel();
                    }
                    child.request_redraw();
                    return;
                }
                // Search box routing: when this child's active tab has an open
                // search box, core editing chords belong to the field; other
                // keymap actions may still run.
                let child_search_open = {
                    let i = child.tabs.active_index();
                    child.tab_states.get(i).map(|t| t.search.is_some()).unwrap_or(false)
                };
                if child_search_open {
                    // When: `child_search_open` — the search field owns editing
                    // chords, while other bound actions may still run.
                    let child_mods = child.modifiers;
                    let _ = child;
                    let is_search_text_edit =
                        super::text_edit::search_text_edit_for_event(&event, child_mods).is_some()
                            || super::text_edit::printable_event_text(&event, child_mods).is_some();
                    if !is_search_text_edit {
                        // When: `is_search_text_edit` is false, so the key is not
                        // field editing and a bound action may claim it.
                        if let Some(key_str) = key_event_to_string(&event, child_mods) {
                            // When: the press maps to a `key_str`, so it can be
                            // looked up in the keymap.
                            if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                                // When: `keymap.lookup` bound this `key_str`, so
                                // the action may run instead of reaching search.
                                if !matches!(action, Action::OpenSearch) {
                                    // When: `matches` is false for OpenSearch, so
                                    // the toggle stays with the search handler.
                                    self.run_action_for_window(&action, win_id);
                                    if let Some(c) = self.windows.get(&win_id) {
                                        c.request_redraw();
                                    }
                                    return;
                                }
                            }
                        }
                    }
                    self.search_handle_key_in_child(win_id, &event, child_mods);
                    if let Some(c) = self.windows.get(&win_id) {
                        c.request_redraw();
                    }
                    return;
                }
                // Run the full keymap dispatch first and fall through to the
                // PTY-byte path only when no binding matches. `run_action`
                // routes to the frontmost child via `frontmost_kind()`, and the
                // Focused(true) arm records `frontmost_window`, so a chord typed
                // here reaches THIS child's per-window helpers.
                //
                // EnterCopyMode / EnterQuickSelect keep their child-local entry
                // helpers because they install copy/quick-select state on this
                // specific child WindowState, which `App::run_action`
                // (main-only) would not touch.
                let child_mods = child.modifiers;
                let _ = child;
                let mut handled = false;
                for key_str in key_event_to_strings(&event, child_mods) {
                    if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                        // When: `keymap.lookup` bound this `key_str`, so the
                        // action is dispatched before any PTY bytes are sent.
                        if super::keymap_dispatch::terminal_input_passthrough_binding(
                            &key_str, &action,
                        ) {
                            // When: `terminal_input_passthrough_binding` claims
                            // this chord for the terminal, so try the next form.
                            continue;
                        }
                        match action {
                            Action::EnterCopyMode => {
                                // When: `EnterCopyMode` — copy state installs on
                                // this child's own WindowState, not on main.
                                if let Some(c) = self.windows.get_mut(&win_id) {
                                    child_enter_copy_mode(c);
                                    c.request_redraw();
                                }
                                return;
                            }
                            Action::EnterQuickSelect => {
                                // When: `EnterQuickSelect` — the hint overlay
                                // installs on this child's own WindowState.
                                if let Some(c) = self.windows.get_mut(&win_id) {
                                    child_enter_quick_select(c);
                                    c.request_redraw();
                                }
                                return;
                            }
                            _ => {
                                // When: any other `action` is window-agnostic, so
                                // the shared dispatcher routes it to this child.
                                if self.run_action_for_window(&action, win_id) {
                                    // When: `run_action_for_window` consumed the
                                    // action, so no PTY bytes are sent for it.
                                    self.drain_pending_window_creates(el);
                                    if let Some(c) = self.windows.get(&win_id) {
                                        c.request_redraw();
                                    }
                                    handled = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                if handled {
                    // When: `handled` — a keymap action already consumed this
                    // press, so no PTY bytes are encoded for it.
                    return;
                }
                let Some(child) = self.windows.get_mut(&win_id) else {
                    // When: `windows` lost `win_id` while an action ran, so there
                    // is no child left to encode bytes for.
                    return;
                };
                let mods = child.modifiers;
                let tab_idx = child.tabs.active_index();
                let active_id = match child.tab_states.get(tab_idx) {
                    Some(st) => st.active_pane,
                    None => {
                        // When: `tab_states` has no entry at `tab_idx`, so there
                        // is no pane to receive the encoded bytes.
                        return;
                    }
                };
                if event.repeat {
                    // When: an unowned repeat survives local routing, never
                    // migrate it to this child's currently focused terminal.
                    return;
                }
                let _ = child;
                let targets = self.terminal_key_targets(active_id);
                let writes = self.encoded_terminal_key_writes(&event, mods, &targets);
                let delivered = self.dispatch_terminal_key_writes(writes);
                if !delivered.is_empty() {
                    // When: delivered is nonempty, retain only panes whose bounded
                    // PTY queues accepted this press and apply terminal-input cleanup.
                    if let Some(child) = self.windows.get_mut(&win_id) {
                        child.pty_pressed_keys.insert(event.physical_key, delivered);
                    }
                    // At least one target accepted input, so terminal-input UI cleanup applies.
                    let Some(child) = self.windows.get_mut(&win_id) else {
                        // When: `windows` lost `win_id` during the write, so
                        // there is no child left to scroll or repaint.
                        return;
                    };
                    // Pressing Enter while scrolled up in history jumps back to
                    // the live bottom; Shift+Enter inserts a newline and must
                    // NOT jump.
                    let is_plain_enter = matches!(event.logical_key, Key::Named(NamedKey::Enter))
                        && !mods.shift_key();
                    if is_plain_enter {
                        if let Some(pane) = child.panes.get_mut(&active_id) {
                            if pane.viewport_top_abs.is_some() {
                                pane.viewport_top_abs = None; // back to live
                                mark_all_panes_dirty(&child.panes);
                                child.request_redraw();
                            }
                        }
                    }
                    if child.selection.is_some() {
                        child.selection = None;
                        mark_all_panes_dirty(&child.panes);
                        child.request_redraw();
                    }
                }
            }
            // IME composition in a torn-out child window: update the child's own
            // ImeState for preedit display, and on commit write the committed
            // text to THIS child's active-pane PTY. Search and copy-mode commits
            // are routed the same way the main window routes them.
            WindowEvent::Ime(ime_event) => {
                // When: an `Ime` event arrives, so composition state belongs to
                // this child rather than the main window.
                let committed = match ime_event {
                    Ime::Enabled => {
                        child.ime.handle_enabled();
                        String::new()
                    }
                    Ime::Disabled => {
                        child.ime.handle_disabled();
                        String::new()
                    }
                    Ime::Preedit(text, cursor) => {
                        child.ime.handle_preedit(&text, cursor);
                        String::new()
                    }
                    Ime::Commit(text) => {
                        child.ime.handle_commit(&text);
                        child.ime.take_commits()
                    }
                };
                let search_open = {
                    let i = child.tabs.active_index();
                    child.tab_states.get(i).map(|t| t.search.is_some()).unwrap_or(false)
                };
                let copy_mode = child.copy_mode.is_some();
                child.request_redraw();
                if !committed.is_empty() {
                    // When: `committed` text exists, so a composition finished and
                    // its bytes must reach whichever surface owns input.

                    // Drop the `child` borrow before re-entering `self` helpers.
                    let _ = child;
                    if search_open {
                        // Search box owns the commit (Chinese/Japanese search).
                        self.search_handle_ime_commit_in_child(win_id, &committed);
                    } else if copy_mode {
                        // When: `copy_mode` is active — navigation only, so the
                        // committed text is dropped rather than typed.
                    } else if let Some(child) = self.windows.get(&win_id) {
                        // When: `windows` still holds `win_id`, so the commit can
                        // be written to that child's active pane.
                        let tab_idx = child.tabs.active_index();
                        if let Some(active_id) =
                            child.tab_states.get(tab_idx).map(|st| st.active_pane)
                        {
                            let bytes = committed.into_bytes();
                            self.write_to_pane(
                                active_id,
                                bytes.clone(),
                                super::PtyInputSource::Ime,
                            );
                            self.broadcast_from(active_id, bytes, super::PtyInputSource::Ime);
                        }
                    }
                }
            }
            _ => {
                // When: any other `event` has no child-window handling, so it is
                // left to winit's defaults.
            }
        }
    }
}

impl App {
    pub(super) fn handle_child_focus_changed(&mut self, win_id: WindowId, focused: bool) {
        let mut focus_report: Option<(u64, Vec<u8>)> = None;
        let mut pointer_release: Option<(u64, Vec<u8>)> = None;
        if let Some(child) = self.windows.get_mut(&win_id) {
            if focused {
                // Unified frontmost tracker; `frontmost_kind()` discriminates
                // main vs child, so the child-only subset is derivable from it.
                self.frontmost_window = Some(win_id);
                child.ime_cursor_throttle.reset();
            } else if self.frontmost_window == Some(win_id) {
                // When: `frontmost_window` still names `win_id`, so only the
                // window that held focus clears the tracker.

                // A sibling window's Focused(true) arrives separately and
                // overwrites this.
                self.frontmost_window = None;
            }

            child.ime.cancel();
            // Focus loss must release terminal ownership before clearing child
            // drags whose native button-up cannot arrive.
            if !focused {
                let modifiers = child.modifiers;
                pointer_release = super::window_event::take_focus_loss_pointer_release(
                    &mut child.pointer_gesture,
                    modifiers,
                )
                .and_then(|route| {
                    super::window_event::pointer_route_bytes(
                        route,
                        super::window_event::PointerReportKind::LeftRelease,
                    )
                });
                child.scrollbar_drag = None;
                child.splitter_drag = None;
                child.mouse_down = false;
                child.pty_pressed_keys.clear();
            }
            if let Some(r) = child.renderer.as_mut() {
                r.set_window_focused(focused);
            }
            if child.test_renderer_focus_marker.is_some() {
                child.test_renderer_focus_marker = Some(focused);
            }
            mark_all_panes_dirty(&child.panes);
            let tab_idx = child.tabs.active_index();
            if let Some(active_id) = child.tab_states.get(tab_idx).map(|state| state.active_pane) {
                let enabled = child
                    .panes
                    .get(&active_id)
                    .map(|pane| pane.parser.lock().focus_reporting_enabled())
                    .unwrap_or(false);
                if enabled {
                    let seq: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
                    focus_report = Some((active_id, seq.to_vec()));
                }
            }
            child.request_redraw();
        }
        // The child borrow ended before either bounded pane write; pointer
        // release targets the press pane while DEC focus targets the active pane.
        if let Some((pane_id, bytes)) = pointer_release {
            self.write_to_pane(pane_id, bytes, super::PtyInputSource::PointerButton);
        }
        if let Some((pane_id, bytes)) = focus_report {
            self.write_to_pane(pane_id, bytes, super::PtyInputSource::FocusReport);
        }
    }

    pub(super) fn merge_child_into_target(
        &mut self,
        src_id: WindowId,
        src_idx: usize,
        target: crate::tab_drag::DropTarget<WindowId>,
    ) {
        let Some((tab, state, panes)) = self.detach_from_child(src_id, src_idx) else {
            // When: `detach_from_child` found no tab at `src_idx`, so the drag
            // source vanished mid-drop and there is nothing to re-attach.
            return;
        };
        let main_id = self.main_window().map(|w| w.id());
        let attached = if Some(target.window) == main_id {
            self.attach_tab_state(target.slot, tab, state, panes);
            // Receiving a tab back into main un-hides the window if it
            // had been drained.
            if self.main_is_hidden() {
                self.show_main_window();
            }
            true
        } else {
            // When: `target.window` is not `main_id`, so the tab lands in a
            // sibling child window, which may itself have closed mid-drop.
            self.attach_to_child(target.window, target.slot, tab, state, panes)
        };
        if !attached {
            tracing::warn!(
                "drag-merge: destination {:?} disappeared mid-drop; panes dropped",
                target.window
            );
        }
        self.reap_empty_child(src_id);
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }
    // Ordering: `reap_call_count` is Relaxed — a test-observable tally with no
    // other state ordered against it.
    pub(super) fn reap_empty_child(&mut self, win_id: WindowId) {
        // Bump the test-observable counter on EVERY invocation (even no-ops on
        // stale ids) so tests can pin that child-window cleanup routed through
        // this contract rather than a raw `windows.remove`.
        self.reap_call_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(child) = self.windows.get(&win_id) {
            if child.tabs.is_empty() {
                if let Some(mut removed) = self.windows.remove(&win_id) {
                    // panes map should already be empty; defensively
                    // null out any stragglers' redraw targets.
                    for pane in removed.panes.values() {
                        *pane.redraw_target.lock() = None;
                    }
                    // Close the governor owners before the state drops.
                    self.release_owners_of(&mut removed);
                    self.release_child_window_registries(win_id);
                    drop(removed);
                    tracing::info!(
                        "child window reaped after drag-merge; remaining children={}",
                        self.child_window_count()
                    );
                    self.request_exit_if_no_active_windows();
                }
            }
        }
    }
    pub(super) fn merge_main_into_child(
        &mut self,
        src_idx: usize,
        target: crate::tab_drag::DropTarget<WindowId>,
    ) {
        let Some((tab, state, panes)) = self.detach_tab_state(src_idx) else {
            // When: `detach_tab_state` found no tab at `src_idx`, so the main
            // window's drag source vanished and there is nothing to move.
            return;
        };
        if !self.attach_to_child(target.window, target.slot, tab, state, panes) {
            tracing::warn!(
                "drag-merge: destination child {:?} disappeared mid-drop; panes dropped",
                target.window
            );
        }
        // If main has been drained but child windows are still alive,
        // hide the main window without exiting the app.
        if self.main_tabs().map(|t| t.is_empty()).unwrap_or(true) && self.child_window_count() > 0 {
            self.hide_main_window();
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }
    pub(super) fn hide_main_window(&mut self) {
        if let Some(w) = self.main_window() {
            w.set_visible(false);
        }
        if let Some(ws) = self.main_mut() {
            ws.hidden = true;
        }
        tracing::info!("main window hidden (drained); windows={}", self.windows.len());
    }
    pub(super) fn show_main_window(&mut self) {
        if let Some(w) = self.main_window() {
            w.set_visible(true);
        }
        if let Some(ws) = self.main_mut() {
            ws.hidden = false;
        }
    }

    /// Build a fresh `PaneState` bound to the given child window's
    /// `(cols, rows, Arc<Window>)` snapshot, spawning the pane's PTY, its VT
    /// loop and its reply-forwarder thread. Shared by `spawn_tab_in_child` and
    /// `split_active_pane_in_child` so both get identical thread wiring.
    ///
    /// The VT worker derives every shared handle from the completed `PaneState`,
    /// so command, media, cursor, and keyboard state cannot diverge from what the
    /// child window reads.
    pub(super) fn spawn_pane_state_for_child(
        &self,
        pane_id: u64,
        cols: u16,
        rows: u16,
        child_window: Arc<Window>,
    ) -> PaneState {
        use sonicterm_grid::grid::Grid;
        use sonicterm_vt::vt::Parser;
        let (reply_tx, reply_rx) =
            crossbeam_channel::bounded::<Vec<u8>>(crate::app::PTY_REPLY_QUEUE_CAPACITY);
        // Honour the user's configured scrollback depth; child
        // windows must match the main window, not the Grid's 10k default.
        let mut grid = Grid::new(cols, rows);
        grid.set_scrollback_limit(self.config.terminal.scrollback);
        let parser = Arc::new(Mutex::new(Parser::new_with_reply(grid, reply_tx)));
        // Seed theme defaults for OSC 10/11/12 + OSC 4 palette.
        {
            let mut p = parser.lock();
            super::seed_parser_theme_colors(&mut p, &self.theme);
        }
        let redraw_target = Arc::new(Mutex::new(Some(child_window.id())));
        let shell_opts = sonicterm_io::pty::ShellSpawnOpts {
            clean_e2e: self.runtime_smoke.is_some(),
            term_program: self.config.terminal.term_program.clone(),
            shell: self.config.terminal.shell.clone(),
            ..sonicterm_io::pty::ShellSpawnOpts::default()
        };
        let pty = match PtyHandle::spawn_default_shell(cols, rows, shell_opts) {
            Ok(pty) => Some(pty),
            Err(e) => {
                tracing::error!("failed to spawn pty for child pane: {e}");
                None
            }
        };
        let mut pane_state = PaneState::new(parser, pty);
        pane_state.redraw_target = redraw_target;
        if pane_state.pty.is_some() {
            super::spawn_pane::spawn_pane_workers(
                pane_id,
                &pane_state,
                reply_rx,
                self.pty_burst_gen.clone(),
                self.event_loop_proxy.clone(),
                "sonicterm-vt-reply-child",
                "sonicterm-vt-loop-child",
            );
        }
        pane_state
    }

    /// Spawn a new tab containing a single fresh pane inside the
    /// child window identified by `win_id`. Returns `false` if no
    /// such child window exists (caller should fall back to the main
    /// App's `new_tab`). The new pane's redraw target is bound to the
    /// child window so VT output redraws the child, not the main App.
    pub(super) fn spawn_tab_in_child(&mut self, win_id: WindowId) -> bool {
        // Snapshot everything we need from the child up-front so the
        // mutable borrow ends before we spawn the VT thread (which
        // captures clones), then re-borrow to install the new tab.
        let (cols, rows, child_window) = {
            let Some(child) = self.windows.get_mut(&win_id) else {
                // When: `windows` no longer holds `win_id`, so the recorded child
                // is gone and the caller falls back to the main App's `new_tab`.
                return false;
            };
            let Some(renderer) = child.renderer.as_ref() else {
                // When: this child has no `renderer`, so no cell grid size exists
                // to spawn the pane's PTY against.
                return false;
            };
            let Some(win) = child.window.as_ref() else {
                // When: this child has no `window`, so the new pane would have no
                // redraw target to bind its VT output to.
                return false;
            };
            let (c, r) = renderer.cells();
            (c, r, win.clone())
        };
        let pane_id = next_pane_id();
        let pane_state = self.spawn_pane_state_for_child(pane_id, cols, rows, child_window.clone());
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` lost `win_id` while the pane was spawning, so the
            // freshly built `pane_state` is dropped with its PTY.
            return false;
        };
        child.panes.insert(pane_id, pane_state);
        let n = child.tabs.len() + 1;
        child.tabs.push(Tab::new(format!("shell {n}")));
        child.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        let last = child.tabs.len().saturating_sub(1);
        child.tabs.activate(last);
        child.request_redraw();
        true
    }

    // ──────────────────────────────────────────────────────────────────
    // per-child action helpers
    //
    // These mirror the equivalent main-window mutators in
    // `app/misc.rs` and `app/spawn_pane.rs` but operate on a child
    // window's owned (tabs / tab_states / panes) triple. Each helper:
    //   * returns `true` if it mutated state (so the caller knows to
    //     bump `redraw_request_count`),
    //   * issues `child.request_redraw()` on the child handle
    //     when state changed,
    //   * returns `false` (no-op + no redraw) when the recorded child
    //     no longer exists — the keymap_dispatch caller then falls
    //     through to the main-window default.
    //
    // The empty-tab-vec post-condition (close the window? leave it
    // dangling? merge into main?) is deliberately left to the existing
    // teardown plumbing — `reap_empty_child` runs on user-event drain
    // and on the next focus event, so we don't replicate that
    // single-source-of-truth here.
    // ──────────────────────────────────────────────────────────────────

    /// Close the active tab of the given child window. Returns `true`
    /// on success.
    pub(super) fn close_active_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let idx = {
            let Some(child) = self.windows.get(&win_id) else {
                // When: `windows` no longer holds `win_id`, so the recorded child
                // is gone and the caller falls back to the main-window default.
                return false;
            };
            child.tabs.active_index()
        };
        self.close_tab_at_in_child(win_id, idx)
    }

    /// Close the tab at `idx` in the given child window. Used by the
    /// close-button (×) hit-test path in the child's tab bar, which
    /// passes the clicked index directly (not the active one). Returns
    /// `true` on success.
    ///
    /// When this drains the child to zero tabs it invokes
    /// [`Self::reap_empty_child`] itself, so callers never have to. Every close
    /// path (× button, Cmd+W, close-active-pane-or-tab) flows through here and
    /// gets the reap for free; a caller-responsible reap would leave a closed
    /// single-pane child window as a ghost frame.
    pub(super) fn close_tab_at_in_child(&mut self, win_id: WindowId, idx: usize) -> bool {
        let drained = {
            let Some(child) = self.windows.get_mut(&win_id) else {
                // When: `windows` no longer holds `win_id`, so the recorded child
                // is gone and there is no tab list to close from.
                return false;
            };
            if idx >= child.tab_states.len() {
                // When: `idx` is past the end of `tab_states`, so the click
                // resolved to a tab that has already been removed.
                return false;
            }
            let st = child.tab_states.remove(idx);
            for id in st.tree.leaves() {
                // PaneState::Drop → PtyHandle::Drop kills the shell.
                child.remove_pane(id);
            }
            if let Some(tab_id) = child.tabs.tabs().get(idx).map(|t| t.id) {
                child.tabs.close(tab_id);
            }
            child.request_redraw();
            child.tabs.is_empty()
        };
        if drained {
            self.reap_empty_child(win_id);
        }
        true
    }

    /// Close-active-pane-or-tab inside a child window. Mirrors the
    /// iTerm2/wezterm rule: > 1 pane → close the focused pane only,
    /// else → close the whole tab.
    pub(super) fn close_active_pane_or_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else {
            // When: `tab_states` has no entry at `tab_idx`, so there is neither a
            // pane tree nor a tab for this action to close.
            return false;
        };
        let pane_count = st.tree.leaves().len();
        if pane_count <= 1 {
            // When: `pane_count` is the last leaf, so closing it means closing
            // the tab. Drop the borrows so the tab path can re-borrow.
            let _ = st;
            let _ = child;
            return self.close_active_tab_in_child(win_id);
        }
        let focus = st.active_pane;
        let new_focus = st.tree.leaves().into_iter().find(|id| *id != focus).unwrap_or(focus);
        if st.tree.close(focus) {
            // When: `tree.close` accepted `focus`, so a pane really left the
            // layout and the survivors must be refocused and resized.
            st.active_pane = new_focus;
            // Same reason as the main-window path: the search was scanning the
            // grid that just went away.
            if let Some(search) = st.search.as_mut() {
                search.invalidate_for_new_grid();
            }
            child.remove_pane(focus);
            // The surviving sibling's PaneRect just grew to cover the closed
            // pane's area. Push the new layout into its Grid + PtyHandle so the
            // survivor (and TUIs like vim) reflow into the freed space; without
            // this the survivor keeps its narrow split-time column count until
            // the OS window is resized.
            resize_visible_panes_in_child(child);
            if let Some(r) = child.renderer.as_mut() {
                r.flash_pane_focus(new_focus);
            }
            child.request_redraw();
            return true;
        }
        false
    }

    /// Advance the active tab in the child window.
    pub(super) fn next_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        child.tabs.next();
        resize_visible_panes_in_child(child);
        child.request_redraw();
        true
    }

    /// Step back one tab in the child window.
    pub(super) fn prev_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        child.tabs.prev();
        resize_visible_panes_in_child(child);
        child.request_redraw();
        true
    }

    /// Activate a specific tab index in the child window.
    pub(super) fn activate_tab_in_child(&mut self, win_id: WindowId, idx: usize) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        child.tabs.activate(idx);
        resize_visible_panes_in_child(child);
        child.request_redraw();
        true
    }

    /// Activate the last tab in the child window.
    pub(super) fn activate_last_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let last = child.tabs.len().saturating_sub(1);
        child.tabs.activate(last);
        resize_visible_panes_in_child(child);
        child.request_redraw();
        true
    }

    // ──────────────────────────────────────────────────────────────────
    // per-child PANE mutators
    //
    // Mirror of the per-child tab helpers above, but for pane-level
    // actions (`Action::SplitRight`, `SplitDown`, `ClosePane`,
    // `FocusPane(_)`, `TogglePaneZoom`, `ResizePane{Left,Right,Up,Down}`).
    // Same contract as the tab helpers: return `true` if mutated state
    // and request_redraw on the child's window; return `false` (no-op)
    // when the recorded child no longer exists so keymap_dispatch can
    // fall back to the main-window default.
    //
    // Without these, Cmd+D / Cmd+Shift+D / Cmd+[ / Cmd+] / Cmd+Z typed
    // in a torn-out child window would silently mutate the MAIN App's
    // active tab instead of this child's.
    // ──────────────────────────────────────────────────────────────────

    /// Split the active pane of the given child window in `dir`. Returns
    /// `true` on success.
    pub(super) fn split_active_pane_in_child(&mut self, win_id: WindowId, dir: Direction) -> bool {
        let Some(child) = self.windows.get(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let Some(tab) = child.tab_states.get(child.tabs.active_index()) else {
            // When: `tab_states` has no active tab, refuse before creating a speculative PTY.
            return false;
        };
        if !tab.tree.leaves().contains(&tab.active_pane)
            || !child.panes.contains_key(&tab.active_pane)
        {
            // When: `active_pane` is not a live leaf, preserve topology without spawning another shell.
            return false;
        }
        let new_id = next_pane_id();
        let pane_state =
            if let (Some(renderer), Some(win)) = (child.renderer.as_ref(), child.window.as_ref()) {
                let (cols, rows) = renderer.cells();
                self.spawn_pane_state_for_child(new_id, cols, rows, win.clone())
            } else if child.renderer.is_none() && child.window.is_none() {
                // When: both `renderer` and `window` are absent — a headless
                // test child still needs pane ownership without a live PTY.
                let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
                PaneState::new(parser, None)
            } else {
                // When: only one of `renderer`/`window` exists, so the child is
                // mid-construction and cell metrics cannot be trusted yet.
                return false;
            };
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` lost `win_id` while the pane was spawning, so the
            // freshly built `pane_state` is dropped with its PTY.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else {
            // When: `tab_states` has no entry at `tab_idx`, so there is no pane
            // tree to receive the split.
            return false;
        };
        let focus = st.active_pane;
        if !st.tree.split(focus, dir, new_id) {
            // When: `tree.split` refused `focus`, so the layout is unchanged and
            // `new_id` is never installed.
            return false;
        }
        st.active_pane = new_id;
        child.panes.insert(new_id, pane_state);
        resize_visible_panes_in_child(child);
        if let Some(r) = child.renderer.as_mut() {
            r.flash_pane_focus(new_id);
        }
        child.request_redraw();
        true
    }

    /// Close the active pane in the given child window. If the active
    /// tab has only one pane left, degrades to closing the tab (same
    /// iTerm2/wezterm rule as the main-window `close_active_pane`).
    pub(super) fn close_active_pane_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else {
            // When: `tab_states` has no entry at `tab_idx`, so there is no pane
            // tree from which to remove the focused leaf.
            return false;
        };
        let focus = st.active_pane;
        if matches!(st.tree, PaneTree::Leaf { id, .. } if id == focus) {
            // When: `matches` finds a lone `Leaf` holding `focus`, so closing the
            // pane closes the whole tab rather than one split.

            // Release the &mut WindowState borrow so the tab path can re-borrow.
            let _ = child;
            return self.close_active_tab_in_child(win_id);
        }
        let new_focus = st.tree.leaves().into_iter().find(|id| *id != focus).unwrap_or(focus);
        if st.tree.close(focus) {
            // When: `tree.close` accepted `focus`, so the layout actually lost a
            // pane and the survivors must be resized and refocused.
            st.active_pane = new_focus;
            // Same reason as the main-window path: the search was scanning the
            // grid that just went away.
            if let Some(search) = st.search.as_mut() {
                search.invalidate_for_new_grid();
            }
            child.remove_pane(focus);
            resize_visible_panes_in_child(child);
            if let Some(r) = child.renderer.as_mut() {
                r.flash_pane_focus(new_focus);
            }
            child.request_redraw();
            return true;
        }
        false
    }

    /// Move pane focus in the given direction within the active tab of
    /// the given child window.
    pub(super) fn focus_pane_dir_in_child(&mut self, win_id: WindowId, dir: Direction) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(next) = child
            .tab_states
            .get(tab_idx)
            .and_then(|tab| tab.tree.focus_neighbor(tab.active_pane, dir))
        else {
            // When: no neighbor exists in `dir`, this child still consumes the
            // recognized action instead of allowing it to mutate the main window.
            return true;
        };
        if let Some(change) = child.begin_pane_focus_change(next) {
            child.finish_pane_focus_change(change);
        }
        true
    }

    /// Toggle zoom on the active pane in the given child window.
    pub(super) fn toggle_active_pane_zoom_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else {
            // When: `tab_states` has no entry at `tab_idx`, so there is no pane
            // tree holding a zoom flag to toggle.
            return false;
        };
        let active = st.active_pane;
        if st.tree.toggle_zoom(active) {
            resize_visible_panes_in_child(child);
            child.request_redraw();
        }
        // Routed regardless of toggle result so the action does not leak
        // to the main window.
        true
    }

    /// Resize the active split edge in the given direction within the
    /// active tab of the given child window.
    pub(super) fn resize_active_split_in_child(
        &mut self,
        win_id: WindowId,
        dir: Direction,
    ) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the recorded child is
            // gone and the caller falls back to the main-window default.
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else {
            // When: `tab_states` has no entry at `tab_idx`, so there is no split
            // tree whose edge could move.
            return false;
        };
        if st.tree.resize_split(st.active_pane, dir, 0.05) {
            resize_visible_panes_in_child(child);
            child.request_redraw();
        }
        // Routed regardless of resize result.
        true
    }

    // ── Child-window splitter (pane-divider) mouse drag ──
    // Mirrors the main-window splitter input. The pure tree ops
    // (`hit_splitter`, `resize_splitter_by_delta`, `layout`) are
    // window-agnostic; only the state lookups differ.

    /// Outer pane-layout rect for a child window (same basis the renderer
    /// + `compute_pane_rects_for` use).
    fn child_pane_outer_rect(&self, win_id: WindowId) -> Option<sonicterm_ui::pane::Rect> {
        let child = self.windows.get(&win_id)?;
        if let Some((outer, _, _)) = child.test_pane_viewport {
            // When: `test_pane_viewport` supplies `outer` directly, so headless
            // tests get pane geometry without a renderer.
            return Some(outer);
        }
        let r = child.renderer.as_ref()?;
        let (w, h) = r.logical_size();
        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
        let bottom = r.bottom_inset();
        Some(sonicterm_ui::pane::Rect::new(0.0, top, w.max(0.0), (h - top - bottom).max(0.0)))
    }

    /// Hit-test a splitter divider in the child window `win_id`.
    fn splitter_hit_at_in_child(
        &self,
        win_id: WindowId,
        x: f32,
        y: f32,
    ) -> Option<sonicterm_ui::pane::SplitterHit> {
        let outer = self.child_pane_outer_rect(win_id)?;
        let child = self.windows.get(&win_id)?;
        let tab_idx = child.tabs.active_index();
        child
            .tab_states
            .get(tab_idx)
            .and_then(|state| state.tree.hit_splitter(outer, CHILD_SPLITTER_HIT_THICKNESS, x, y))
    }

    fn set_child_splitter_cursor(&self, win_id: WindowId, axis: sonicterm_ui::pane::SplitAxis) {
        if let Some(child) = self.windows.get(&win_id) {
            if let Some(w) = child.window.as_ref() {
                let icon = match axis {
                    sonicterm_ui::pane::SplitAxis::Vertical => CursorIcon::ColResize,
                    sonicterm_ui::pane::SplitAxis::Horizontal => CursorIcon::RowResize,
                };
                w.set_cursor(icon);
            }
        }
    }

    fn refresh_child_splitter_hover(&mut self, win_id: WindowId, x: f32, y: f32) -> bool {
        let hit = self.splitter_hit_at_in_child(win_id, x, y);
        let axis = hit.map(|hit| hit.axis);
        let changed =
            self.windows.get(&win_id).map(|child| child.splitter_hover != axis).unwrap_or(false);
        if let Some(child) = self.windows.get_mut(&win_id) {
            child.splitter_hover = axis;
        }
        if let Some(axis) = axis {
            self.set_child_splitter_cursor(win_id, axis);
        } else if changed {
            // When: `changed` and no axis — the pointer just left a divider, so
            // the resize cursor must be handed back to the default.
            if let Some(child) = self.windows.get(&win_id) {
                if let Some(w) = child.window.as_ref() {
                    w.set_cursor(CursorIcon::Default);
                }
            }
        }
        changed || axis.is_some()
    }

    /// Test-only: prove the child splitter hit-test is reachable with headless
    /// viewport geometry.
    #[doc(hidden)]
    pub fn __test_child_splitter_hit_axis(
        &self,
        win_id: WindowId,
        x: f32,
        y: f32,
    ) -> Option<sonicterm_ui::pane::SplitAxis> {
        self.splitter_hit_at_in_child(win_id, x, y).map(|hit| hit.axis)
    }

    /// Test-only: drive the child splitter hover refresh directly and report
    /// whether hover state or cursor shape changed, so the no-button hover path
    /// is reachable with headless viewport geometry.
    #[doc(hidden)]
    pub fn __test_refresh_child_splitter_hover(
        &mut self,
        win_id: WindowId,
        x: f32,
        y: f32,
    ) -> bool {
        self.refresh_child_splitter_hover(win_id, x, y)
    }

    /// Test-only: read child splitter-hover state.
    #[doc(hidden)]
    pub fn __test_child_splitter_hover(
        &self,
        win_id: WindowId,
    ) -> Option<sonicterm_ui::pane::SplitAxis> {
        self.windows.get(&win_id).and_then(|child| child.splitter_hover)
    }

    /// Test-only: report where the child redraw path anchors the OS IME
    /// candidate area — `"palette"`, `"search"` or `"terminal"` — mirroring the
    /// precedence the child render path applies.
    #[doc(hidden)]
    pub fn __test_child_ime_candidate_anchor_kind(&self, win_id: WindowId) -> Option<&'static str> {
        let child = self.windows.get(&win_id)?;
        if self.command_palette.is_open() && self.palette_attached_window == Some(win_id) {
            // When: the palette is open and attached to `win_id`, so it owns the
            // caret and the candidate window anchors to its query row.
            return Some("palette");
        }
        let search_open = child
            .tab_states
            .get(child.tabs.active_index())
            .is_some_and(|state| state.search.is_some());
        Some(if search_open { "search" } else { "terminal" })
    }

    /// Apply an in-flight splitter drag in the child window `win_id`.
    fn apply_splitter_drag_in_child(&mut self, win_id: WindowId, x: f32, y: f32) -> bool {
        let Some(drag) = self.windows.get(&win_id).and_then(|c| c.splitter_drag.clone()) else {
            // When: no `splitter_drag` is recorded, so this cursor move is not
            // part of a divider drag and belongs to the ordinary hover path.
            return false;
        };
        let Some(outer) = self.child_pane_outer_rect(win_id) else {
            // When: `child_pane_outer_rect` is unavailable, so there is no
            // layout basis to convert the pointer delta into a split ratio.
            return false;
        };
        let dx = x - drag.last_pos.0;
        let dy = y - drag.last_pos.1;
        if dx == 0.0 && dy == 0.0 {
            // When: `dx` and `dy` are both zero, so the pointer has not moved
            // and the drag stays live without re-laying out the tree.
            return true;
        }
        let tab_idx = self.windows.get(&win_id).map(|c| c.tabs.active_index()).unwrap_or(0);
        let changed = self
            .windows
            .get_mut(&win_id)
            .and_then(|c| c.tab_states.get_mut(tab_idx))
            .map(|state| state.tree.resize_splitter_by_delta(&drag.splitter, outer, dx, dy))
            .unwrap_or(false);
        if changed {
            if let Some(child) = self.windows.get_mut(&win_id) {
                resize_visible_panes_in_child(child);
            }
        }
        if let Some(child) = self.windows.get_mut(&win_id) {
            if let Some(active) = child.splitter_drag.as_mut() {
                active.last_pos = (x, y);
            }
            if changed {
                mark_all_panes_dirty(&child.panes);
                child.request_redraw();
            }
        }
        self.set_child_splitter_cursor(win_id, drag.axis);
        true
    }
}

/// Splitter hit thickness in logical px (mirror of window_event's const).
const CHILD_SPLITTER_HIT_THICKNESS: f32 = 8.0;

/// Resize all panes in the active tab of a child window to match the
/// current pane tree layout. Mirrors `App::resize_visible_panes` for the
/// child case so split/close/zoom on a torn-out window propagate to the
/// PTY winsize the same way.
pub(super) fn resize_visible_panes_in_child(child: &mut WindowState) {
    let rects = App::compute_pane_rects_for(child);
    // Test-only metrics override (mirrors main `test_viewport_override`): a
    // headless child has `renderer: None`, so fall back to the seam so the
    // child split-resize wiring is unit-testable.
    if let Some((_, cw, ch)) = child.test_pane_viewport {
        // When: `test_pane_viewport` supplies `cw`/`ch`, so a headless child
        // sizes panes from those metrics with no renderer padding to apply.
        crate::app::resize_panes_to_rects(&child.panes, &rects, cw, ch, [0.0, 0.0, 0.0, 0.0]);
        return;
    }
    let Some(r) = child.renderer.as_ref() else {
        // When: no `renderer` and no test override, so cell metrics are unknown
        // and any resize would push a wrong winsize to the PTY.
        return;
    };
    let (cw, ch) = r.cell_size();
    let inset =
        [r.padding_left_px(), r.padding_right_px(), r.padding_top_px(), r.padding_bottom_px()];
    crate::app::resize_panes_to_rects(&child.panes, &rects, cw, ch, inset);
}

/// Scroll a pane's scrollback view in a child window by `delta_lines`
/// (negative = back into history). Child-scoped mirror of `App::scroll_pane`.
/// Returns early on the alt screen: the `MouseWheel` arm translates alt-screen
/// wheel input into key or mouse reports before ever calling this.
fn scroll_child_pane(child: &mut WindowState, pane_id: u64, delta_lines: i32) {
    if delta_lines == 0 {
        // When: `delta_lines` rounded to zero, so a sub-line wheel tick moves
        // the view nowhere and nothing needs marking dirty.
        return;
    }
    let Some(pane) = child.panes.get(&pane_id) else {
        // When: `panes` no longer holds `pane_id`, so the pane closed between
        // the wheel event and this scroll.
        return;
    };
    let (live_top, current_view_top) = {
        let parser = pane.parser.lock();
        let grid = parser.grid();
        if grid.is_alt() {
            // When: `grid.is_alt()` — the alt screen keeps no scrollback, and
            // the wheel was already translated into reports for the child.
            return;
        }
        let live_top = grid.scrollback_len() as u64;
        let current = GpuRenderer::resolved_view_top_abs_legacy(grid, pane.viewport_top_abs);
        (live_top, current)
    };
    let new_view_top: u64 = if delta_lines < 0 {
        current_view_top.saturating_sub((-(delta_lines as i64)) as u64)
    } else {
        // When: `delta_lines` is positive, so the view walks forward and clamps
        // at `live_top` rather than running past the newest row.
        current_view_top.saturating_add(delta_lines as u64).min(live_top)
    };
    if let Some(pane) = child.panes.get_mut(&pane_id) {
        pane.viewport_top_abs = if new_view_top >= live_top {
            None
        } else {
            // When: `new_view_top` stays above `live_top`, so the pane pins to
            // that scrollback row instead of following the live bottom.
            Some(new_view_top)
        };
    }
    // Parity with the main window's wheel path (`scroll.rs` →
    // `mark_scrollbar_active`): a wheel scroll briefly shows the auto-hide
    // scrollbar so the user can see where they are in the scrollback. Use
    // `entry().or_insert_with` (not `get_mut`) so a scroll BEFORE the first
    // render — common right after tear-out — still lights the bar.
    let now = Instant::now();
    child
        .scrollbar_vis
        .entry(pane_id)
        .or_insert_with(|| crate::app::scrollbar_visibility::ScrollbarVisState::new(now))
        .mark_active(now);
    mark_all_panes_dirty(&child.panes);
    child.request_redraw();
}

/// Pane id under logical-px `(lx, ly)` in a CHILD window's active tab, or
/// `None` outside every pane. Mirror of `App::pane_at_cursor`.
fn child_pane_at_cursor(child: &WindowState, lx: f32, ly: f32) -> Option<u64> {
    for (pane_id, rect) in App::compute_pane_rects_for(child) {
        if lx >= rect.x && lx < rect.x + rect.w && ly >= rect.y && ly < rect.y + rect.h {
            // When: `lx`/`ly` fall inside this `rect`, so this pane owns the
            // pointer and the walk stops at the first containing pane.
            return Some(pane_id);
        }
    }
    None
}
fn child_enter_copy_mode(child: &mut WindowState) {
    let tab_idx = child.tabs.active_index();
    let Some(active_id) = child.tab_states.get(tab_idx).map(|st| st.active_pane) else {
        // When: `tab_states` holds no entry at `tab_idx`, so there is no
        // `active_pane` whose cursor cell could seed copy mode.
        return;
    };
    let Some(pane) = child.panes.get(&active_id) else {
        // When: `panes` no longer holds `active_id`, so no parser grid exists
        // to read the starting cursor cell from.
        return;
    };
    let cursor = {
        let guard = pane.parser.lock();
        let grid = guard.grid();
        (grid.cursor.col as usize, grid.scrollback_len() + grid.cursor.row as usize)
    };
    child.copy_mode = Some(sonicterm_ui::copy_mode::CopyModeState::read_only_at(cursor));
    mark_all_panes_dirty(&child.panes);
}

fn child_enter_quick_select(child: &mut WindowState) {
    let tab_idx = child.tabs.active_index();
    let Some(active_id) = child.tab_states.get(tab_idx).map(|st| st.active_pane) else {
        // When: `tab_states` holds no entry at `tab_idx`, so there is no
        // `active_pane` grid to scan for quick-select hint labels.
        return;
    };
    let Some(pane) = child.panes.get(&active_id) else {
        // When: `panes` no longer holds `active_id`, so there is no grid to
        // build the hint overlay from.
        return;
    };
    let state = {
        let guard = pane.parser.lock();
        let grid = guard.grid();
        let mut state = sonicterm_ui::copy_mode::CopyModeState::new_at((0, grid.scrollback_len()));
        state.quick_select = Some(sonicterm_ui::copy_mode::QuickSelectState::from_grid(grid));
        state
    };
    child.copy_mode = Some(state);
    mark_all_panes_dirty(&child.panes);
}

fn child_copy_mode_handle_key(child: &mut WindowState, event: &KeyEvent) {
    let Some(mut state) = child.copy_mode.take() else {
        // When: `copy_mode` holds no state, so this child is not in copy mode
        // and the key belongs to the ordinary input path.
        return;
    };
    let mut should_copy = false;
    let mut should_exit = false;
    let mut copied_text: Option<String> = None;

    let tab_idx = child.tabs.active_index();
    let Some(active_id) = child.tab_states.get(tab_idx).map(|st| st.active_pane) else {
        // When: `tab_states` has no entry at `tab_idx`, so no `active_pane`
        // grid exists to navigate; the taken state is dropped with it.
        return;
    };
    if let Some(pane) = child.panes.get_mut(&active_id) {
        let guard = pane.parser.lock();
        let grid = guard.grid();
        if let Some(quick_select) = state.quick_select.as_ref() {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => should_exit = true,
                Key::Character(s) => {
                    if let Some(ch) = s.chars().next() {
                        if let Some(text) = quick_select.text_for_hint(ch) {
                            copied_text = Some(text.to_string());
                            should_exit = true;
                        }
                    }
                }
                _ => {
                    // When: any other `logical_key` is not a hint label, so the
                    // overlay stays open and the key is discarded.
                }
            }
        } else {
            // When: `quick_select` is absent, so this is ordinary copy-mode
            // navigation and the key moves or copies from the grid.

            match &event.logical_key {
                Key::Named(NamedKey::Escape) => should_exit = true,
                Key::Named(NamedKey::Enter) if !state.is_read_only() => should_copy = true,
                Key::Named(NamedKey::ArrowLeft) => state.move_left(grid),
                Key::Named(NamedKey::ArrowRight) => state.move_right(grid),
                Key::Named(NamedKey::ArrowUp) => state.move_up(grid),
                Key::Named(NamedKey::ArrowDown) => state.move_down(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("h") => state.move_left(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("j") => state.move_down(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("k") => state.move_up(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("l") => state.move_right(grid),
                Key::Character(s) if s == "v" && !state.is_read_only() => state.start_select(),
                Key::Character(s) if s == "y" && !state.is_read_only() => should_copy = true,
                Key::Character(s) if s == "w" => state.move_word_fwd(grid),
                Key::Character(s) if s == "b" => state.move_word_back(grid),
                Key::Character(s) if s == "0" => state.move_line_start(grid),
                Key::Character(s) if s == "$" => state.move_line_end(grid),
                Key::Character(s) if s == "g" => state.move_top(grid),
                Key::Character(s) if s == "G" => state.move_bottom(grid),
                _ => {
                    // When: any other `logical_key` has no copy-mode binding, so
                    // the cursor holds its cell and the key is discarded.
                }
            }
            if should_copy {
                copied_text = child_copy_mode_selected_text(&state, grid);
                should_exit = true;
            } else {
                // When: `should_copy` stayed false, so the key was a move —
                // follow it with the viewport so the cursor stays on screen.
                pane.viewport_top_abs = GpuRenderer::copy_mode_view_top_after_move_legacy(
                    &state,
                    grid,
                    pane.viewport_top_abs,
                );
            }
        }
    }

    if let Some(text) = copied_text {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Err(e) = cb.set_text(text.clone()) {
                tracing::warn!("clipboard set failed: {e}");
            } else {
                // When: `set_text` succeeded, so the copy is on the system
                // clipboard and the byte count is worth recording.
                tracing::info!("copied {} bytes", text.len());
            }
        }
    }
    if !should_exit {
        child.copy_mode = Some(state);
    }
    mark_all_panes_dirty(&child.panes);
}

fn child_copy_mode_selected_text(
    state: &sonicterm_ui::copy_mode::CopyModeState,
    grid: &Grid,
) -> Option<String> {
    let (start, end) = state.selected_range()?;
    if start == end {
        // When: `start` equals `end`, so the range covers no cell and there is
        // nothing to place on the clipboard.
        return None;
    }
    let out = plain_text_from_grid_range(grid, (start.0, start.1 as u64), (end.0, end.1 as u64));
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
#[path = "child_window_tests.rs"]
mod child_window_tests;
