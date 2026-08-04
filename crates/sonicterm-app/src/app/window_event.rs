//! `App::do_window_event` — the full `WindowEvent` dispatch body,
//! extracted from `ApplicationHandler::window_event` from the monolithic app module.
//!
//! This is mechanically the original body wrapped in a separate `impl App`
//! block; field access works because all referenced `App` fields are
//! `pub(super)`.

use std::sync::atomic::Ordering;
use std::time::Instant;

use sonicterm_cfg::keymap::Action;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::overlays::{
    search_bar_label, search_query_caret_prefix, SearchBarLayout, SEARCH_BAR_ICON_GAP,
    SEARCH_BAR_PAD_LEFT, SEARCH_BAR_PAD_RIGHT,
};
use sonicterm_ui::selection::{plain_text_from_grid_range, SelectMode, Selection};
use sonicterm_ui::tabbar_view::TabBarLayout;
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, NamedKey},
    window::{CursorIcon, WindowId},
};

use super::key_encoding::{encode_key, key_event_to_string, key_to_strings};
use super::{invalidate_selection_for_content, mark_all_panes_dirty, App, FrontmostKind, TabState};

const SPLITTER_HIT_THICKNESS: f32 = 8.0;
const SEARCH_BADGE_ICON: &str = "";

/// Encode `count` mouse-wheel reports for an app that has mouse tracking on.
/// Wheel buttons per xterm: 64 = up, 65 = down (press only, no release).
/// `sgr` true → SGR encoding `ESC[<Btn;col;row M` (1-based, unbounded).
/// `sgr` false → legacy X10 `ESC[M` + 3 bytes (button+32, col+32, row+32),
/// each byte clamped to the classic 223 ceiling (col/row+32 ≤ 255).
/// `col`/`row` are 1-based cell coordinates of the cell under the cursor.
pub(super) fn wheel_report_bytes(sgr: bool, up: bool, col: u32, row: u32, count: usize) -> Vec<u8> {
    let btn: u32 = if up { 64 } else { 65 };
    let mut out = Vec::new();
    for _ in 0..count {
        if sgr {
            out.extend_from_slice(format!("\x1b[<{btn};{col};{row}M").as_bytes());
        } else {
            // When: sgr is false, clamp each legacy X10 coordinate to one byte.
            // X10: parameters are value+32, capped so col/row+32 fit a byte.
            let cb = (btn + 32).min(255) as u8;
            let cx = (col.min(223) + 32) as u8;
            let cy = (row.min(223) + 32) as u8;
            out.extend_from_slice(&[0x1b, b'[', b'M', cb, cx, cy]);
        }
    }
    out
}

/// Decide whether a key chord should trigger the quit confirmation guard.
///
/// `key_str` is the normalized chord (e.g. `"super+q"`) and `bound` is the
/// action the active keymap maps it to, if any. Cmd+Q is a macOS system
/// chord, so on macOS an *unbound* `super+q` still quits — a user's edited or
/// symlinked keymap frequently omits it, and falling through to the PTY (which
/// types a literal `q`) is never what the user wants. An explicit `quit_app`
/// binding is honored on every platform. If the user deliberately rebound
/// `super+q` to some other action, we stand down and let that action run.
pub(super) fn is_quit_chord(key_str: &str, bound: Option<&Action>) -> bool {
    if matches!(bound, Some(Action::QuitApp)) {
        // When: matches finds bound is Some(Action::QuitApp), trigger the quit guard.
        return true;
    }
    cfg!(target_os = "macos") && key_str == "super+q" && bound.is_none()
}

impl App {
    // Ordering: pty_burst_gen uses Acquire; cursor_visible, kitty_flags, and app_cursor_keys are independent Relaxed snapshots.
    pub(super) fn do_window_event(
        &mut self,
        el: &ActiveEventLoop,
        win_id: WindowId,
        event: WindowEvent,
    ) {
        // mark any user-driven event so the next
        // RedrawRequested bypasses the vsync coalescing gate. This
        // covers main and child windows uniformly. PTY-byte
        // redraws (the high-volume path) arrive as RedrawRequested
        // with this flag still false and continue to coalesce.
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::ModifiersChanged(_)
                | WindowEvent::Ime(_)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Focused(_)
        ) {
            self.input_dirty = true;
        }
        if self.is_warm_window_id(win_id) {
            // When: is_warm_window_id identifies win_id as unpromoted, ignore its event.
            return;
        }
        // Tear-out child windows: route to the dedicated handler so
        // each child renders/handles input on its own surface.
        // the main window also lives in `self.windows`
        // now (shadow entry, `Some(main_window_id)`), but its events
        // must continue to flow through the legacy `App.*` paths
        // below. Skip the main window's id explicitly.
        if self.windows.contains_key(&win_id) && Some(win_id) != self.main_window_id {
            // When: windows contains win_id and main_window_id differs, delegate to child state.
            self.handle_child_window_event(el, win_id, event);
            return;
        }
        match event {
            WindowEvent::DroppedFile(path) => {
                self.paste_file_paths_for_kind(FrontmostKind::Main, [path]);
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }
            WindowEvent::CloseRequested => {
                // Notify the reducer of the close request. It mutates
                // `AppState::{live_window_count, focused_window}` and
                // emits `WindowClose` [+ `Quit` if last]. The
                // boundary's existing macOS-style "hide instead of
                // exit" policy below is the source of truth for what
                // the platform actually does; the reducer's Effects
                // are observability-only in this slice (the
                // `dispatch_effects` arms for `WindowClose` /
                // `WindowOpen` / `WindowResize` are trace-stubs per
                // §9). The `Quit` cascade does flip `pending_exit` —
                // suppress that here so we don't override the
                // "hide-on-last-close" policy. Real Quit cascading
                // moves to the reducer in 2c-misc.
                let intent = sonicterm_app_core::AppIntent::WindowCloseRequested {
                    window: sonicterm_types::WindowKey::new(0),
                };
                for effect in self.machine.handle(intent) {
                    if !matches!(
                        effect,
                        sonicterm_app_core::AppEffect::Quit
                            | sonicterm_app_core::AppEffect::WindowClose { .. }
                    ) {
                        self.dispatch_effects(smallvec::smallvec![effect]);
                    }
                }
                // If child windows still own tabs, hide the main
                // window instead of exiting the app — the children
                // are independent live terminals and must keep
                // running. When no child remains, closing the main
                // leaves no active terminal window, so quit.
                if self.child_window_count() == 0 {
                    self.hide_main_window();
                    el.exit();
                } else {
                    // When: child_window_count remains nonzero, hide main while child terminals stay live.
                    self.hide_main_window();
                }
            }

            WindowEvent::RedrawRequested => {
                // When: RedrawRequested renders the main surface or defers a streaming-only frame.
                let was_dirty = self.input_dirty;
                let pty_burst_snapshot = self.pty_burst_gen.load(Ordering::Acquire);
                let pty_burst = pty_burst_snapshot != self.last_seen_burst_gen;
                // Perf audit #9: if we already rendered within the
                // current vsync window, defer this redraw until the
                // next monitor refresh boundary. `about_to_wait` will
                // see `pending_redraw` and call
                // `set_control_flow(WaitUntil(last_render +
                // frame_period))`; `new_events`' ResumeTimeReached arm
                // then re-requests the redraw. Net effect: bursty PTY
                // output coalesces into one frame per vsync instead of
                // burning the GPU at the VT thread's 16ms tick rate.
                // Input-driven redraws must be immediate — gating them
                // on the vsync deadline adds
                // perceptible latency to typing/resize/theme changes.
                // Only redraws that arrive purely from streaming PTY
                // bytes (input_dirty stays false) get coalesced.
                let last_render = self.main().map(|ws| ws.last_render).unwrap_or_else(Instant::now);
                // while composing an IME preedit on the software
                // rasterizer, drop to a lower frame cap so a long pinyin run
                // doesn't drive a full-surface raster at full cadence.
                let composing = self.main().map(|ws| ws.ime.is_composing()).unwrap_or(false);
                let frame_period = crate::app::effective_frame_period(
                    self.software_render_degrade,
                    composing,
                    self.frame_period,
                );
                if crate::app::should_defer_streaming_redraw(
                    was_dirty,
                    pty_burst,
                    self.software_render_degrade,
                    last_render.elapsed(),
                    frame_period,
                ) {
                    // When: should_defer_streaming_redraw is true, schedule the frame for the next refresh.
                    self.pending_redraw = true;
                    return;
                }
                let mut timing = crate::app::render_timing::RenderTiming::start("main");
                self.pending_redraw = false;
                let main_id_opt = self.main_window_id;
                if let Some(id) = main_id_opt {
                    if let Some(ws) = self.windows.get_mut(&id) {
                        ws.tabs.clear_expired_command_badges(Instant::now());
                    }
                }
                self.poll_command_events_for_all_tabs();
                if let Some(t) = timing.as_mut() {
                    t.lap("poll");
                }
                let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                // Compute per-pane rects in window pixels so the renderer can
                // draw a border around each one (and a brighter one around
                // the focused pane). The active pane's grid is rendered into
                // the full content area; per-pane Buffer rendering is v0.4.
                let pane_rects: Vec<(u64, sonicterm_ui::pane::Rect)> = self
                    .main_tab_states()
                    .and_then(|ts| ts.get(tab_idx))
                    .map(|st| {
                        if let Some(r) = self.main_renderer() {
                            // Renderer geometry lays out every pane in the drawable content area.
                            let (w, h) = r.logical_size();
                            let top = (r.top_inset() - r.padding_top_px()).max(0.0);
                            let bottom = r.bottom_inset();
                            let outer = sonicterm_ui::pane::Rect::new(
                                0.0,
                                top,
                                w.max(0.0),
                                (h - top - bottom).max(0.0),
                            );
                            st.tree.layout(outer)
                        } else {
                            // When: main_renderer is absent, no pane rectangles can be derived.
                            Vec::new()
                        }
                    })
                    .unwrap_or_default();
                let active_id = self
                    .main_tab_states()
                    .and_then(|ts| ts.get(tab_idx))
                    .map(|st| st.active_pane)
                    .unwrap_or(0);
                let broadcast_receivers = self.broadcast_receivers();
                if let Some(t) = timing.as_mut() {
                    t.lap("layout");
                }

                // per-pane scrollbar visibility/fade tick.
                // Built BEFORE the try_lock pass since it only needs
                // logical-px rects (already in `pane_rects`) and the
                // already-captured cursor pos / scrollbar_drag — no
                // parser lock needed. Result feeds each PaneRender's
                // `scrollbar_alpha` below.
                let scrollbar_alpha_map: std::collections::HashMap<u64, f32> = {
                    let mode = self.config.appearance.scrollbar;
                    let drag_pane =
                        self.main().and_then(|ws| ws.scrollbar_drag.as_ref().map(|s| s.pane_id));
                    let (cx, cy) = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                    let cursor = (cx as f32, cy as f32);
                    let rects: Vec<(u64, f32, f32, f32, f32)> =
                        pane_rects.iter().map(|(id, r)| (*id, r.x, r.y, r.w, r.h)).collect();
                    let now = Instant::now();
                    if let Some(ws) = self.main_mut() {
                        crate::app::scrollbar_visibility::update_and_collect(
                            &mut ws.scrollbar_vis,
                            &rects,
                            cursor,
                            active_id,
                            drag_pane,
                            mode,
                            now,
                        )
                    } else {
                        // When: main_mut is None, no scrollbar visibility state can be collected.
                        std::collections::HashMap::new()
                    }
                };
                // Keep redrawing while any pane's scrollbar fade is
                // animating so a paused mouse-leave still completes the
                // 300 ms fade-out (otherwise the bar would stay frozen
                // mid-fade until the next external event).
                let scrollbar_needs_more_frames = {
                    let mode = self.config.appearance.scrollbar;
                    let drag_pane =
                        self.main().and_then(|ws| ws.scrollbar_drag.as_ref().map(|s| s.pane_id));
                    self.main()
                        .map(|ws| {
                            ws.scrollbar_vis.iter().any(|(id, st)| {
                                crate::app::scrollbar_visibility::is_animating(
                                    st,
                                    mode,
                                    drag_pane == Some(*id),
                                )
                            })
                        })
                        .unwrap_or(false)
                };
                if let Some(t) = timing.as_mut() {
                    t.lap("scrollbar");
                }
                if scrollbar_needs_more_frames && !self.software_render_degrade {
                    // An accelerated scrollbar fade requests its next animation frame.
                    // in the no-GPU path, skip the fade-driven
                    // extra frames — the bar snaps instead of animating, but
                    // we don't burn CPU rasterizing a 300ms fade.
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }

                // Fix 1: try_lock EVERY pane in the tab and pass
                // them ALL through to the renderer. The previous single-
                // element slice meant the per-pane loop inside
                // `GpuRenderer::render` never iterated inactive panes in
                // production frames — that was the visible "right pane
                // empty after split" bug.
                //
                // Strategy: clone every pane's parser Arc, try to lock
                // all of them in one pass. If ANY lock fails, defer the
                // redraw (§4 land-mine) and bail — partial frames are
                // not allowed because the renderer needs a coherent
                // multi-pane view, and a re-locked sub-pane would
                // produce torn output. Order is pane_rects order;
                // active position is recorded separately.
                let main_panes_for_arcs = self.main_panes();
                // `try_lock`, never a blocking `lock`: the VT worker holds
                // this while merging a decoded batch, and blocking here would
                // stall the event loop behind it. On contention this defers
                // the redraw exactly as the parser locks below do — the
                // renderer needs a coherent view of every pane, so reusing a
                // stale image list for one pane while the rest advance would
                // tear the frame rather than merely delay it.
                let mut inline_images_by_pane: std::collections::HashMap<
                    u64,
                    Vec<sonicterm_render_model::InlineImage>,
                > = std::collections::HashMap::new();
                let mut inline_images_locked = true;
                if let Some(panes) = main_panes_for_arcs {
                    // When: main_panes_for_arcs is Some, snapshot each pane's inline images.
                    for (id, pane) in panes.iter() {
                        match pane.inline_images.try_lock() {
                            Some(images) => {
                                // Available image locks contribute to the coherent frame snapshot.
                                inline_images_by_pane.insert(*id, images.clone());
                            }
                            None => {
                                // When: try_lock returns None, reject the partial multi-pane snapshot.
                                inline_images_locked = false;
                                break;
                            }
                        }
                    }
                }
                if !inline_images_locked {
                    // When: inline_images_locked is false, release snapshots and retry the frame later.
                    drop(inline_images_by_pane);
                    self.defer_redraw_on_lock_contention(was_dirty);
                    return;
                }
                let parser_arcs: Vec<(
                    u64,
                    std::sync::Arc<parking_lot::Mutex<sonicterm_vt::vt::Parser>>,
                    sonicterm_ui::pane::Rect,
                )> = pane_rects
                    .iter()
                    .filter_map(|(id, rect)| {
                        main_panes_for_arcs
                            .and_then(|panes| panes.get(id))
                            .map(|p| (*id, std::sync::Arc::clone(&p.parser), *rect))
                    })
                    .collect();
                if let Some(t) = timing.as_mut() {
                    t.lap("inline_images");
                }
                let mut guards: Vec<(
                    u64,
                    parking_lot::MutexGuard<'_, sonicterm_vt::vt::Parser>,
                    sonicterm_ui::pane::Rect,
                )> = Vec::with_capacity(parser_arcs.len());
                let mut all_locked = true;
                for (id, arc, rect) in &parser_arcs {
                    match arc.try_lock() {
                        Some(g) => {
                            // When: try_lock returns Some(g), retain its guard for the coherent frame.

                            // Available parser locks retain their guards for the coherent pane frame.
                            // Extending the guard's lifetime to the outer scope is
                            // valid because `arc` lives in `parser_arcs`, which is
                            // dropped strictly after `guards`, so the underlying
                            // Mutex outlives every guard. parking_lot guards carry
                            // a `*const Mutex` internally and no `'a` tied to `arc`.
                            let g_ext: parking_lot::MutexGuard<'_, sonicterm_vt::vt::Parser> =
                                // SAFETY: parser_arcs outlives guards, preserving every guard's backing Mutex.
                                unsafe { std::mem::transmute(g) };
                            guards.push((*id, g_ext, *rect));
                        }
                        None => {
                            // When: try_lock returns None, reject the partial multi-pane frame.
                            all_locked = false;
                            break;
                        }
                    }
                }
                if let Some(t) = timing.as_mut() {
                    t.lap("try_lock");
                }
                if !all_locked {
                    // When: all_locked is false, release every parser guard before deferring.
                    drop(guards);
                    drop(parser_arcs);
                    self.defer_redraw_on_lock_contention(was_dirty);
                    return;
                }

                if let Some(r) = self.main_renderer_mut() {
                    r.set_inactive_pane_cursors(Vec::new());
                }

                // lift the main window Arc clone before the
                // mut borrow on `self.renderer` below, so the IME
                // cursor-area branch can still touch
                // `ws.ime_cursor_throttle` (mut) without re-borrowing
                // `self`.
                let main_window_for_ime = self.main_window().cloned();
                let main_palette_ime_area = main_window_for_ime.as_ref().and_then(|w| {
                    let r = self.main_renderer()?;
                    if self.palette_attached_window.is_some() || !self.command_palette.is_open() {
                        // When: palette_attached_window is Some or command_palette is closed, main has no IME anchor.
                        return None;
                    }
                    self.command_palette_ime_cursor_area(
                        w.inner_size().width as f32,
                        w.inner_size().height as f32,
                        self.config.appearance.panel_padding,
                        r.scale_factor(),
                        sonicterm_ui::tab_spans::tab_title_font_size(r.font_size())
                            * r.scale_factor(),
                        r.cell_w,
                    )
                });
                // Search-bar IME geometry: the full marker-free label drives
                // box width, but the caret/candidate-window anchor must follow
                // the current query caret, not the end of the label. Produce
                // both strings from the same state so the OS candidate area
                // agrees with the renderer-owned block cursor.
                let (search_ime_label, search_ime_prefix) = self
                    .main()
                    .and_then(|ws| {
                        let preedit = ws.ime.preedit();
                        let i = ws.tabs.active_index();
                        ws.tab_states.get(i).and_then(|st| st.search.as_ref()).map(|s| {
                            (search_bar_label(s, preedit), search_query_caret_prefix(s, preedit))
                        })
                    })
                    .unzip();
                // Borrow-split: pull the renderer out via direct
                // map-lookup on `self.windows` (NOT through `main_renderer_mut`,
                // which would borrow all of `self`). That keeps
                // `self.command_palette`, `self.ime` available for the
                // disjoint mut borrows the render call needs in the same
                // expression scope.
                // panes now live in `ws` too, so they're
                // pulled from the same field-disjoint split borrow.
                let main_id_opt = self.main_window_id;
                let mut ws_opt = main_id_opt.and_then(|id| self.windows.get_mut(&id));
                if let Some(ws) = ws_opt.as_deref_mut() {
                    if let Some(active_pos) = guards.iter().position(|(id, _, _)| *id == active_id)
                    {
                        invalidate_selection_for_content(
                            &mut ws.selection,
                            active_id,
                            guards[active_pos].1.grid(),
                        );
                    }
                }
                #[allow(clippy::type_complexity)]
                let (
                    renderer_opt,
                    tabs_opt,
                    tab_states_opt,
                    panes_opt,
                    cursor_visible_now,
                    last_render_slot,
                    ws_selection_ref,
                    ws_copy_mode_ref,
                    ws_ime_ref,
                    ws_ime_throttle_ref,
                    ws_viewport_tops,
                    ws_hovered_url_cells,
                    ws_notification_ref,
                ): (
                    Option<&mut GpuRenderer>,
                    Option<&mut sonicterm_ui::tabs::TabBar>,
                    Option<&mut Vec<TabState>>,
                    Option<&mut std::collections::HashMap<u64, crate::app::PaneState>>,
                    bool,
                    Option<&mut Instant>,
                    Option<&Selection>,
                    Option<&CopyModeState>,
                    Option<&sonicterm_ui::ime::ImeState>,
                    Option<&mut sonicterm_ui::ime::ImeCursorThrottle>,
                    std::collections::HashMap<u64, Option<u64>>,
                    Option<sonicterm_render_model::inputs::HoveredUrlCells>,
                    Option<&sonicterm_ui::overlays::NotificationBubble>,
                ) = match ws_opt {
                    Some(ws) => {
                        // Split the available WindowState render inputs into disjoint borrows.
                        // cursor_visible is now per-pane; read
                        // it from the active pane before splitting the
                        // mut borrow of `ws.panes`. Bool read, no
                        // lasting borrow.
                        let cv = ws
                            .panes
                            .get(&active_id)
                            .map(|p| p.cursor_visible.load(std::sync::atomic::Ordering::Relaxed))
                            .unwrap_or(true);
                        // selection + copy_mode now live on
                        // `ws`. Pull immutable refs disjoint from the mut
                        // borrows of `ws.{renderer,tabs,tab_states,panes,last_render}`.
                        // ime + ime_cursor_throttle also live
                        // on `ws`; split-borrow disjointly too.
                        let sel_ref = ws.selection.as_ref();
                        let cm_ref = ws.copy_mode.as_ref();
                        // Map the per-window Cmd-hovered URL (set only
                        // while the open-URL modifier is held; cleared on
                        // release / pointer drift) into the Copy
                        // `HoveredUrlCells` the renderer recolors with the
                        // theme accent. Immutable read, disjoint from the
                        // mut borrows of ws.{renderer,tabs,...}.
                        let hovered_url_cells = ws.hovered_url.as_ref().map(|h| h.to_cells());
                        let notification_ref = ws.notification.as_ref();
                        let viewport_tops = ws
                            .panes
                            .iter()
                            .map(|(id, pane)| (*id, pane.viewport_top_abs))
                            .collect();
                        (
                            ws.renderer.as_mut(),
                            Some(&mut ws.tabs),
                            Some(&mut ws.tab_states),
                            Some(&mut ws.panes),
                            cv,
                            Some(&mut ws.last_render),
                            sel_ref,
                            cm_ref,
                            Some(&ws.ime),
                            Some(&mut ws.ime_cursor_throttle),
                            viewport_tops,
                            hovered_url_cells,
                            notification_ref,
                        )
                    }
                    None => {
                        // An absent WindowState produces empty render inputs without borrowing self.
                        (
                            None,
                            None,
                            None,
                            None,
                            true,
                            None,
                            None,
                            None,
                            None,
                            None,
                            std::collections::HashMap::new(),
                            None,
                            None,
                        )
                    }
                };
                if let (Some(r), Some(pane), Some(tabs_mref), Some(tab_states_mref)) = (
                    renderer_opt,
                    panes_opt.and_then(|p| p.get_mut(&active_id)),
                    tabs_opt,
                    tab_states_opt,
                ) {
                    // When: renderer_opt, pane, tabs_mref, and tab_states_mref are Some, render one coherent frame.
                    let cursor_rc = {
                        // Fix 1: the active pane's parser guard is
                        // already in `guards` from the global try_lock pass
                        // above; locking it again here would AB-BA deadlock
                        // (we already hold it). Find the active guard via
                        // a mut borrow over `guards`.
                        let active_pos = guards
                            .iter()
                            .position(|(id, _, _)| *id == active_id)
                            // PANIC: safe — `guards` was populated immediately
                            // above from `tab.panes` keyed by `active_id`, so
                            // a guard with this id is present. Render hot
                            // path: do NOT convert to Result (CLAUDE.md §4 —
                            // this fn must never block or crash the terminal).
                            .expect("active pane guard collected above");
                        // Wezterm-style tab title: `#N icon parent/leaf`.
                        // Pull cwd from OSC 7, the foreground process from
                        // the pid probe (macOS only for now), and the OSC
                        // 0/2 title as the last-resort body (so `ssh
                        // user@host` still labels itself).
                        //
                        // Shared with `app/child_window.rs` via
                        // `refresh_active_tab_title` so Cmd+N / tear-out
                        // windows pick up cwd-based titles too (was
                        // previously stuck on the literal "shell N"
                        // placeholder set at spawn time).
                        let _ = crate::app::refresh_active_tab_title(
                            tabs_mref,
                            pane,
                            &guards[active_pos].1,
                            tab_idx,
                            !pty_burst,
                        );
                        if let Some(search) =
                            tab_states_mref.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                        {
                            search.maybe_refresh_for_revision(guards[active_pos].1.grid_mut());
                        }
                        let search = tab_states_mref.get(tab_idx).and_then(|t| t.search.as_ref());
                        // Fix 1: build the slice from ALL panes
                        // (was previously a single-element slice for the
                        // active pane only). The renderer's per-pane loop
                        // now actually iterates every pane in production
                        // frames, so split panes paint.
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
                                viewport_top_abs: ws_viewport_tops.get(id).copied().flatten(),
                                is_active: *id == active_id,
                                cursor_style: sonicterm_render_model::CursorStyle::default(),
                                is_broadcast_receiver: broadcast_receivers.contains(id),
                                scrollbar_alpha: scrollbar_alpha_map
                                    .get(id)
                                    .copied()
                                    .unwrap_or(0.0),
                                inline_images: inline_images_by_pane
                                    .get(id)
                                    .cloned()
                                    .unwrap_or_default(),
                            })
                            .collect();
                        r.set_render_timing_label("main");
                        if let Err(e) = r.render(
                            &mut panes_slice,
                            &self.theme,
                            cursor_visible_now
                                && !(self.command_palette.is_open()
                                    && self.palette_attached_window.is_none()),
                            ws_selection_ref,
                            ws_copy_mode_ref,
                            tabs_mref,
                            search,
                            // Feed the palette only to its attached window;
                            // `None` denotes main, and routing it elsewhere
                            // would paint the overlay on the wrong window.
                            self.palette_attached_window
                                .is_none()
                                .then_some(&mut self.command_palette),
                            ws_ime_ref,
                            pane.viewport_top_abs,
                            ws_notification_ref,
                            ws_hovered_url_cells,
                        ) {
                            tracing::warn!("render error: {e}");
                        }
                        if let Some(t) = timing.as_mut() {
                            t.lap("render");
                        }
                        self.input_dirty = false;
                        // mark only the generation sampled at
                        // the start of this RedrawRequested as seen.
                        // A burst arriving during render keeps the
                        // counter ahead of last_seen_burst_gen so the
                        // next redraw bypasses the vsync gate.
                        self.last_seen_burst_gen = pty_burst_snapshot;
                        if let Some(lr) = last_render_slot {
                            *lr = Instant::now();
                        }
                        let g = guards[active_pos].1.grid_mut();
                        (g.cursor.row, g.cursor.col)
                    };
                    // refresh the OS-drag tab bar
                    // snapshot so cross-window drop hit-tests see the
                    // current layout. Cross-window drops read this; an
                    // empty registry means every drop resolves to
                    // `DroppedOnEmpty` instead of a concrete destination.
                    // (moved after the renderer borrow scope below)
                    // Tell the OS where the active text cursor lives so the
                    // IME candidate window (pinyin candidates, Japanese
                    // romaji selector, Korean Hangul composer) appears
                    // immediately below the cell being edited — not
                    // pinned to the top-left corner of the screen as
                    // happens when the area is never set.
                    if let Some(w) = main_window_for_ime {
                        if let Some((pos, size)) = main_palette_ime_area {
                            // The main-hosted palette anchors the candidate window to its caret.
                            w.set_ime_cursor_area(pos, size);
                        } else if let Some(search_label) = search_ime_label.as_ref() {
                            // When: search_ime_label is Some, derive the candidate anchor from its query caret.
                            let window_size = w.inner_size();
                            // window_size + the SearchBarLayout it feeds are
                            // physical px, so every logical-px term here must be
                            // scaled by the renderer's scale factor or the IME
                            // caret rect drifts on HiDPI displays.
                            let scale = r.scale_factor();
                            let font_size =
                                sonicterm_ui::tab_spans::tab_title_font_size(r.font_size()) * scale;
                            let icon_w = r.measure_overlay_text_width(SEARCH_BADGE_ICON, font_size);
                            let content_w = icon_w
                                + SEARCH_BAR_ICON_GAP * scale
                                + r.measure_overlay_text_width(search_label, font_size);
                            let row =
                                u8::from(ws_copy_mode_ref.is_some_and(|cm| cm.is_read_only()));
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
                            // Right inner edge: the candidate window must never
                            // push past the box padding.
                            let right_edge = (layout.border.x + layout.border.w
                                - SEARCH_BAR_PAD_RIGHT * scale)
                                .max(text_x);
                            // Anchor the OS candidate window at the END OF THE
                            // QUERY (`text_x + width("/ " + query)`), matching
                            // the inline preedit caret, then clamp to the right
                            // inner edge. `font_size` already folds in `scale`.
                            let prefix_w = search_ime_prefix
                                .as_ref()
                                .map(|p| r.measure_overlay_text_width(p, font_size))
                                .unwrap_or(0.0);
                            let caret_x = (text_x + prefix_w).clamp(text_x, right_edge);
                            let pos = winit::dpi::PhysicalPosition::new(
                                caret_x as i32,
                                layout.border.y as i32,
                            );
                            let size = winit::dpi::PhysicalSize::new(
                                r.cell_w.ceil() as u32,
                                layout.border.h.ceil() as u32,
                            );
                            w.set_ime_cursor_area(pos, size);
                        } else if let Some(throttle) = ws_ime_throttle_ref {
                            // When: ws_ime_throttle_ref is Some(throttle), use terminal cell IME geometry.
                            // Terminal input updates its cell-based candidate anchor through the throttle.
                            if throttle.should_update(cursor_rc.0, cursor_rc.1) {
                                // A changed cursor cell publishes its physical rectangle to the OS IME.
                                let x = r.padding_left_px() + f32::from(cursor_rc.1) * r.cell_w;
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
                }
                // refresh OS-drag tab bar snapshot
                // for the main window. Outside the renderer borrow scope
                // so the immutable self borrow doesn't conflict with `r`.
                self.publish_main_window_tab_bar();
                if let Some(t) = timing {
                    t.finish();
                }
            }

            WindowEvent::Focused(focused) => {
                // Focused updates reducer, frontmost routing, IME, renderer, and terminal focus reporting.
                // Route focus transitions through the reducer. It mutates
                // `AppState::focused_window` and emits a
                // `Render(Focus)` only on actual transition (no spam
                // on duplicate Focused(true)). The boundary's
                // existing per-pane dirty-mark + `request_redraw`
                // below stays as the production paint path; the
                // reducer's Render is observability-only here (and
                // dedups via `dispatch_effects`' redraw counter).
                let wk = sonicterm_types::WindowKey::new(0);
                let intent = if focused {
                    // The focused reducer transition records main-window focus.
                    sonicterm_app_core::AppIntent::WindowFocused { window: wk }
                } else {
                    // When: focused is false, publish the blurred reducer transition.
                    sonicterm_app_core::AppIntent::WindowBlurred { window: wk }
                };
                self.dispatch_intent(intent);
                if focused {
                    // Focus entering main makes it the destination for subsequent global actions.
                    // record the main window as
                    // OS-frontmost so keymap_dispatch / menubar drain
                    // route subsequent Cmd+T / Cmd+W / Cmd+\\ to the
                    // main window's tabs vec. `frontmost_window` subsumed the
                    // sibling `focused_child` clear — `frontmost_window`
                    // discriminates main vs child via `frontmost_kind()`.
                    self.frontmost_window = Some(win_id);
                } else if self.frontmost_window == Some(win_id) {
                    // When: frontmost_window equals Some(win_id), clear its routing claim.

                    // Only clear if WE were the recorded frontmost.
                    // Focus moving to a sibling sonic window arrives as
                    // that window's own `Focused(true)` and overwrites
                    // frontmost in the right order; if the user is just
                    // switching to another app we end up at `None` here
                    // which makes terminal actions fall back to main
                    // (safe default).
                    self.frontmost_window = None;
                }
                // Reset IME state across focus transitions. When focus is
                // lost mid-composition, the OS IME panel detaches without
                // sending us a Commit; dropping the preedit avoids replaying
                // stale composition state on the next focus-in. Toggling
                // `set_ime_allowed` nudges the OS to re-attach the input
                // context cleanly on macOS / Windows.
                if let Some(ws) = self.main_mut() {
                    ws.ime.cancel();
                    if !focused {
                        // Focus loss cancels drags that cannot receive their release event.
                        // A drag interrupted by focus loss never gets its
                        // button-release; drop the gesture so it doesn't
                        // resume on the next stray cursor move.
                        ws.scrollbar_drag = None;
                        ws.splitter_drag = None;
                    }
                }
                // Propagate window focus to the renderer so the text cursor
                // disappears when the window is inactive.
                if let Some(r) = self.main_renderer_mut() {
                    r.set_window_focused(focused);
                }
                // Focus transition changes cursor visibility only, so mark
                // every pane dirty without bumping grid revision.
                if let Some(panes) = self.main_panes() {
                    mark_all_panes_dirty(panes);
                }
                // Forward focus in/out to the active pane if it asked for
                // focus reporting via DECSET ?1004 (CSI ?1004h).
                if let Some(pane) = self.active_pane() {
                    let enabled = pane.parser.lock().focus_reporting_enabled();
                    if enabled {
                        // DEC focus reporting forwards the transition to the active PTY.
                        if let Some(pty) = pane.pty.as_ref() {
                            let seq: &[u8] = if focused {
                                // Focus-in emits the DEC focus-in sequence.
                                b"\x1b[I"
                            } else {
                                // Focus-out emits the DEC focus-out sequence.
                                b"\x1b[O"
                            };
                            Self::queue_pty_input(
                                self.event_loop_proxy.as_ref(),
                                pty,
                                seq.to_vec(),
                            );
                        }
                    }
                }
                if let Some(w) = self.main_window().cloned() {
                    // Intentionally do NOT toggle `set_ime_allowed` on
                    // focus transitions. macOS' IMK posts a runloop
                    // wake message on every toggle; doing it on every
                    // focus in/out (which SonicTerm also receives when the
                    // OS shows a notification, switches Spaces, etc.)
                    // floods stderr with
                    // `IMKCFRunLoopWakeUpReliable` errors and is a
                    // suspected cause of long-session hangs. IME is
                    // already enabled once at window creation; winit
                    // suspends delivery on focus-out automatically.
                    // Also invalidate the cursor-area throttle so the
                    // first redraw after refocus re-teaches the OS the
                    // current cell position.
                    if focused {
                        // Focus return forces the next redraw to republish the current IME cursor cell.
                        if let Some(ws) = self.main_mut() {
                            ws.ime_cursor_throttle.reset();
                        }
                    }
                    w.request_redraw();
                }
            }

            WindowEvent::Resized(size) => {
                // When: WindowEvent::Resized supplies size, update geometry before scheduling.

                // Resized updates renderer and pane geometry before scheduling the replacement frame.
                if self
                    .main_renderer_mut()
                    .is_some_and(|renderer| !renderer.try_resize(size.width, size.height))
                {
                    // When: try_resize returns false for size, retain the previous surface.
                    tracing::warn!(
                        width = size.width,
                        height = size.height,
                        "main window resize ignored after renderer safety rejection"
                    );
                    return;
                }
                // Notify the reducer of the new logical grid dimensions.
                // Derive cols/rows from
                // the renderer's cell size; fall back to zero when
                // unavailable (smoke-test environments). The
                // reducer's `WindowResize` Effect is observability-
                // only — the boundary above already drove the wgpu
                // resize, and the existing `request_redraw` below is
                // the production paint path.
                let (cols_u16, rows_u16) = {
                    let cell = self.main_renderer().map(GpuRenderer::cell_size);
                    match cell {
                        Some((cw, ch)) if cw > 0.0 && ch > 0.0 => (
                            ((size.width as f32 / cw).floor() as u32).min(u16::MAX as u32) as u16,
                            ((size.height as f32 / ch).floor() as u32).min(u16::MAX as u32) as u16,
                        ),
                        _ => (0u16, 0u16),
                    }
                };
                self.dispatch_intent(sonicterm_app_core::AppIntent::WindowResized {
                    window: sonicterm_types::WindowKey::new(0),
                    cols: cols_u16,
                    rows: rows_u16,
                });
                // Per-pane sizing: each pane's grid + PTY is resized to
                // its own PaneRect within the new window content area,
                // never to the whole window's dimensions.
                let rects = self.compute_active_pane_rects();
                let metrics = self.main_renderer().map(|r| {
                    (
                        r.cell_size(),
                        [
                            r.padding_left_px(),
                            r.padding_right_px(),
                            r.padding_top_px(),
                            r.padding_bottom_px(),
                        ],
                    )
                });
                if let (Some(((cw, ch), inset)), Some(panes)) = (metrics, self.main_panes()) {
                    crate::app::resize_panes_to_rects(panes, &rects, cw, ch, inset);
                }
                // Cell geometry changed — force the next render to
                // re-publish the IME cursor area even if (row, col) is
                // unchanged, otherwise the OS candidate window stays
                // pinned to the pre-resize pixel location.
                if let Some(ws) = self.main_mut() {
                    ws.ime_cursor_throttle.reset();
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor: dpi_scale, .. } => {
                if let Some(ws) = self.main_mut() {
                    ws.dpi_scale = dpi_scale;
                }
                if let Some(id) = self.main_window_id {
                    if let Some(ws) = self.windows.get_mut(&id) {
                        crate::app::apply_dpi_to_renderer_if_present(&mut ws.renderer, dpi_scale);
                    }
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }

            WindowEvent::ModifiersChanged(m) => {
                if let Some(ws) = self.main_mut() {
                    ws.modifiers = m.state();
                }
                // Releasing the open-URL modifier must clear any
                // visible Cmd+hover URL underline (and revert the
                // pointer to default if it was previously shown). We
                // recompute hover state from the last cursor position
                // so a subsequent re-press while still hovering brings
                // the affordance back without needing a CursorMoved.
                self.refresh_hovered_url();
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }

            // -- Mouse --
            WindowEvent::CursorLeft { .. } => {
                let mut redraw = false;
                if let Some(r) = self.main_renderer_mut() {
                    redraw = r.set_hover_cursor(None);
                }
                if let Some(ws) = self.main_mut() {
                    ws.splitter_hover = None;
                }
                if let Some(w) = self.main_window() {
                    w.set_cursor(CursorIcon::Default);
                }
                if self.main_mut().and_then(|ws| ws.hovered_url.take()).is_some() {
                    redraw = true;
                }
                if self.clear_scrollbar_hover() {
                    redraw = true;
                }
                if redraw {
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // When: WindowEvent::CursorMoved supplies position, update pointer interaction.

                // CursorMoved refreshes pointer-driven overlays, drags, selection, and cross-window targets.
                if let Some(ws) = self.main_mut() {
                    ws.cursor_pos = (position.x, position.y);
                }
                let (lx, ly) = (position.x as f32, position.y as f32);
                // Notify the reducer so last_mouse_pos tracks the cursor; its
                // identity check implicitly coalesces sub-pixel jitter
                // bursts into a single Render(Hover) per frame.
                self.dispatch_intent(sonicterm_app_core::AppIntent::MouseMove {
                    window: sonicterm_types::WindowKey::new(0),
                    pos: sonicterm_app_core::LogicalPos { x: lx as f64, y: ly as f64 },
                });
                let mut hover_redraw = false;
                if let Some(r) = self.main_renderer_mut() {
                    hover_redraw = r.set_hover_cursor(Some((lx, ly)));
                }
                if hover_redraw {
                    // A bare hover-move over the tab bar must repaint —
                    // otherwise the muted × → bright × transition lags
                    // until the next unrelated event.
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }
                // Auto scrollbar hover is also pure cursor state. Terminal
                // cursor moves from normal PTY output do not wake the renderer,
                // so request a frame exactly when the pointer crosses the
                // right-edge proximity threshold.
                let _ = self.refresh_scrollbar_hover_from_cursor();
                if self.apply_splitter_drag(lx, ly) {
                    // When: apply_splitter_drag consumes the motion, skip tab and text drag routing.
                    return;
                }
                // Update the live drag session position so the chip
                // can follow the cursor in the renderer overlay.
                let drag_snapshot = self.main_mut().and_then(|ws| {
                    ws.drag_session.as_mut().map(|s| {
                        s.current_pos = (lx, ly);
                        (s.press_tab_index, *s)
                    })
                });
                if let Some((press_idx, session_snapshot)) = drag_snapshot {
                    let title = self
                        .main_tabs()
                        .and_then(|t| t.tabs().get(press_idx).map(|tab| tab.title.clone()))
                        .unwrap_or_default();
                    let window_width =
                        self.main_window().map(|w| w.inner_size().width as f32).unwrap_or(0.0);
                    let (bar_h, top_off, visible) = self
                        .main_renderer()
                        .map(|r| {
                            (r.tab_bar_logical_height(), r.tab_bar_y_offset(), r.tab_bar_visible())
                        })
                        .unwrap_or((sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT, 0.0, true));
                    let empty_tabs = sonicterm_ui::tabs::TabBar::new();
                    let layout = TabBarLayout::compute_with_height(
                        self.main_tabs().unwrap_or(&empty_tabs),
                        window_width,
                        bar_h,
                    )
                    .with_top_offset(top_off)
                    .with_visible(visible);
                    let chip =
                        crate::tab_drag::build_drag_chip_overlay(&session_snapshot, &layout, title);
                    if let Some(r) = self.main_renderer_mut() {
                        r.set_drag_chip(chip);
                    }
                }
                // Cross-window drag-merge: if a tab is held, update the
                // pending drop target based on the global cursor
                // position. The actual decision (tear / merge / cancel)
                // is deferred to mouse-up via `compute_action`.
                let (mouse_down, has_press) = self
                    .main()
                    .map(|ws| (ws.mouse_down, ws.pressed_tab.is_some()))
                    .unwrap_or((false, false));
                if mouse_down && has_press {
                    // When: mouse_down and has_press are true, update the tab drop target and OS handoff.
                    let target = self.compute_main_drag_target((position.x, position.y));
                    if let Some(ws) = self.main_mut() {
                        ws.drag_target = target;
                    }
                    // start the OS-level
                    // drag session AS SOON AS the cursor crosses the
                    // drag-start threshold from its press point, not on
                    // mouse-release. Windows `DoDragDrop` needs the live
                    // button state for cursor capture. The current macOS
                    // backend is pasteboard-only, but shares this trigger so
                    // the payload is published once per gesture. The `os_drag_handoff_started` flag
                    // ensures we only attempt the handoff once per
                    // gesture; if it succeeds the backend owns the
                    // gesture end-to-end (Windows) or has already
                    // written the pasteboard (macOS).
                    if !self.os_drag_handoff_started {
                        // When: os_drag_handoff_started is false, test whether the gesture crossed the threshold.
                        let started_idx = self.main().and_then(|ws| {
                            ws.drag_session
                                .as_ref()
                                .filter(|s| crate::tab_drag::drag_moved_enough(s))
                                .map(|s| s.press_tab_index)
                        });
                        if let Some(idx) = started_idx {
                            // When: started_idx is Some, transfer this tab gesture to the OS backend once.
                            self.os_drag_handoff_started = true;
                            let _ = self.try_os_drag_handoff(idx);
                        }
                    }
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                    return;
                }
                if self.main().map(|ws| ws.mouse_down).unwrap_or(false) {
                    // When: mouse_down is true, apply scrollbar or text-selection drag semantics.

                    // scrollbar drag takes priority over
                    // selection extension while a thumb is held. Match
                    // CLAUDE.md §4 — keep this branch fast; no parser
                    // lock is needed (geometry was snapshotted at press).
                    if let Some((pane_id, new_view_top)) = self.scrollbar_drag_apply(lx, ly) {
                        // When: scrollbar_drag_apply returns pane_id and new_view_top, update that viewport.

                        // Resolve `live_top` for the dragged pane (not
                        // necessarily the active one — keep the gesture
                        // pinned to the press pane even if focus shifted).
                        let live_top_opt = self.main().and_then(|ws| {
                            ws.panes.get(&pane_id).and_then(|p| {
                                p.parser.try_lock().map(|parser| {
                                    let g = parser.grid();
                                    g.scrollback_len() as u64
                                })
                            })
                        });
                        if let Some(live_top) = live_top_opt {
                            // The parser snapshot clamps the dragged viewport against live output.
                            if let Some(ws) = self.main_mut() {
                                if let Some(pane) = ws.panes.get_mut(&pane_id) {
                                    pane.viewport_top_abs = if new_view_top >= live_top {
                                        // Reaching current output resumes following the live bottom.
                                        None
                                    } else {
                                        // When: new_view_top is below live_top, retain its absolute history position.
                                        Some(new_view_top)
                                    };
                                }
                                super::mark_all_panes_dirty(&ws.panes);
                                if let Some(w) = ws.window.as_ref() {
                                    w.request_redraw();
                                }
                            }
                        }
                        // drag also counts as scrollbar activity.
                        self.mark_scrollbar_active(pane_id);
                        return;
                    }
                    if let Some(r) = self.main_renderer() {
                        if let Some((row, col)) =
                            r.pixel_to_cell(position.x as f32, position.y as f32)
                        {
                            // WezTerm-style drag granularity.
                            // The press recorded `select_mode` + `select_anchor`;
                            // extend by Cell / Word / Line accordingly.
                            //
                            // Word/Line need the live grid, so compute the
                            // replacement Selection up front while we still
                            // hold only &self (via try_lock inside the helper,
                            // which drops the grid lock before we redraw —
                            // CLAUDE.md §4). `r`'s last use was pixel_to_cell,
                            // so the &self / &mut self borrows below are fine.
                            let (mode, anchor) = self
                                .main()
                                .map(|ws| (ws.select_mode, ws.select_anchor))
                                .unwrap_or((SelectMode::Cell, (0, 0)));
                            // Some(Some(_)) = recomputed region; Some(None) =
                            // parser was busy → SKIP this move (a cell-extend
                            // would shrink the word/line region); None = Cell
                            // mode (handled by the extend branch below).
                            // `anchor.0` is ABSOLUTE; the helpers convert the
                            // viewport `row` to absolute internally.
                            let replacement = match mode {
                                SelectMode::Word => {
                                    Some(self.word_drag_selection_at(anchor, row, col))
                                }
                                SelectMode::Line => {
                                    Some(self.line_drag_selection_at(anchor.0, row))
                                }
                                SelectMode::Cell => None,
                            };
                            // Cell-mode extend needs the cursor's ABSOLUTE row
                            // too. Resolve it before the &mut borrow below.
                            // None = no active pane / parser busy → SKIP this
                            // move rather than fall back to the viewport row:
                            // treating a viewport row as absolute while scrolled
                            // would extend a far-away anchor (e.g. abs 1000) down
                            // to a small on-screen row and balloon the selection.
                            // Only Cell mode consumes it, so skip the extra
                            // try_lock for word/line drags.
                            let cursor_selection_state = if matches!(mode, SelectMode::Cell) {
                                self.viewport_row_selection_state(row)
                            } else {
                                // When: matches does not find SelectMode::Cell, replacement has absolute state.
                                None
                            };
                            // selection lives on WindowState.
                            // Split-borrow `ws.selection` and `ws.panes`
                            // disjointly.
                            if let Some(ws) = self.main_mut() {
                                if let Some(sel) = ws.selection.as_mut() {
                                    match ws.select_mode {
                                        SelectMode::Cell => {
                                            // Don't let a stray CursorMoved
                                            // collapse a double/triple-click
                                            // (word/line) selection down to the
                                            // cursor cell. Only a plain
                                            // point-drag extends. Skip if
                                            // the absolute row was unavailable.
                                            if !sel.anchored {
                                                if let Some((abs, pane_id, seq, is_alt, evicted)) =
                                                    cursor_selection_state
                                                {
                                                    sel.extend_with_content_state(
                                                        abs, col, pane_id, seq, is_alt, evicted,
                                                    );
                                                    mark_all_panes_dirty(&ws.panes);
                                                    if let Some(w) = ws.window.as_ref() {
                                                        w.request_redraw();
                                                    }
                                                }
                                            }
                                        }
                                        SelectMode::Word | SelectMode::Line => {
                                            // Replace with the recomputed union
                                            // / row-span; on Some(None) (busy
                                            // parser) skip — never shrink below
                                            // the anchor word/line.
                                            if let Some(Some(new_sel)) = replacement {
                                                *sel = new_sel;
                                                mark_all_panes_dirty(&ws.panes);
                                                if let Some(w) = ws.window.as_ref() {
                                                    w.request_redraw();
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // When: mouse_down is false, update hover affordances instead of drag state.

                    // Hover-without-button: recompute the OSC8/auto-URL
                    // hover state. Auto-detected URLs are gated on the
                    // platform open-URL modifier (Cmd / Ctrl) per the
                    // v1.0 Cmd-held-hover affordance; OSC 8 keeps its
                    // unconditional pointer affordance.
                    if !self.refresh_splitter_hover(lx, ly) {
                        self.refresh_hovered_url();
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // route wheel events to the pane under the cursor.
                // Default 3 lines per LineDelta tick (matches stock GTK
                // / Cocoa wheel feel). PixelDelta divides by the live
                // cell height so trackpad scrolls match font size.
                let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                let (lx, ly) = (cursor_pos.0 as f32, cursor_pos.1 as f32);
                let cell_h = self
                    .main_renderer()
                    .map(|r| r.cell_size().1)
                    .filter(|h| *h > 0.0)
                    .unwrap_or(16.0);
                let lines_per_tick: f32 = 3.0;
                let delta_lines_f: f32 = match delta {
                    // winit's y is positive when scrolling UP (away from
                    // user); we want negative delta_lines for "scroll
                    // back into history".
                    MouseScrollDelta::LineDelta(_x, y) => -y * lines_per_tick,
                    MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32) / cell_h,
                };
                // Round away from zero so a tiny trackpad nudge still
                // produces at least one line of motion.
                let delta_lines = if delta_lines_f >= 0.0 {
                    // Positive wheel motion rounds upward to preserve a fractional tick.
                    delta_lines_f.ceil() as i32
                } else {
                    // When: delta_lines_f is negative, round downward to preserve a fractional tick.
                    delta_lines_f.floor() as i32
                };
                if delta_lines != 0 {
                    // Nonzero wheel motion routes to the hovered pane.
                    if let Some(pane_id) = self.pane_at_cursor(lx, ly) {
                        // The hovered pane receives terminal or scrollback wheel semantics.
                        // Alt-screen wheel handling. Full-screen TUIs
                        // live on the alt screen. Two cases:
                        //   * mouse tracking ON (?1000/?1002/?1003): the app
                        //     wants wheel as MOUSE events — send SGR (or legacy)
                        //     wheel reports (button 64=up / 65=down) so claude /
                        //     copilot scroll their own transcript.
                        //   * tracking OFF: translate to arrow keys so pagers
                        //     (less/vim/man) scroll.
                        // NOTE: ?1006 (SGR) is an ENCODING modifier, NOT a
                        // tracking enable — it must be excluded from the
                        // "tracking on" test (xterm ctlseqs).
                        // Snapshot the flags + the cell under the cursor under
                        // the lock, then DROP it before any PTY write
                        // (CLAUDE.md §4).
                        let cell = self.main_renderer().and_then(|r| r.pixel_to_cell(lx, ly));
                        let (is_alt, tracking_on, sgr, app_cursor) = self
                            .main()
                            .and_then(|ws| ws.panes.get(&pane_id))
                            .map(|pane| {
                                let parser = pane.parser.lock();
                                let is_alt = parser.grid().is_alt();
                                let tracking_on = parser.mouse_tracking_enabled();
                                let sgr = parser.mouse_sgr_enabled();
                                let app_cursor = parser.application_cursor_keys();
                                (is_alt, tracking_on, sgr, app_cursor)
                            })
                            .unwrap_or((false, false, false, false));
                        if is_alt && tracking_on {
                            // Alternate-screen mouse tracking encodes wheel reports for the PTY.
                            // App wants mouse events: emit one wheel report per
                            // line of motion at the cell under the cursor.
                            let up = delta_lines < 0;
                            let (col1, row1) =
                                cell.map(|(r, c)| (c as u32 + 1, r as u32 + 1)).unwrap_or((1, 1));
                            let count = delta_lines.unsigned_abs() as usize;
                            let payload = wheel_report_bytes(sgr, up, col1, row1, count);
                            if let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) {
                                if let Some(pty) = pane.pty.as_ref() {
                                    Self::queue_pty_input(
                                        self.event_loop_proxy.as_ref(),
                                        pty,
                                        payload,
                                    );
                                }
                            }
                        } else if is_alt {
                            // When: is_alt is true without tracking_on, translate wheel motion to arrows.

                            // Build the arrow sequence: ESC O A/B in
                            // application-cursor-keys mode, else ESC [ A/B.
                            // Up when scrolling back into history
                            // (delta_lines < 0), down otherwise. Emit one
                            // copy per line of motion.
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
                            if let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) {
                                if let Some(pty) = pane.pty.as_ref() {
                                    Self::queue_pty_input(
                                        self.event_loop_proxy.as_ref(),
                                        pty,
                                        payload,
                                    );
                                }
                            }
                        } else {
                            // When: is_alt is false, move SonicTerm's primary-screen scrollback viewport.
                            self.scroll_pane(pane_id, delta_lines);
                        }
                    }
                }
            }

            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                // When: MouseInput uses MouseButton::Left, route state to primary-pointer interaction.
                match state {
                    ElementState::Pressed => {
                        // When: state is ElementState::Pressed, begin primary-pointer interaction.

                        // Notify the reducer of the press transition so selection
                        // observability emits Render(Selection).
                        {
                            let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                            let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                            self.dispatch_intent(sonicterm_app_core::AppIntent::MouseButton {
                                window: sonicterm_types::WindowKey::new(0),
                                pressed: true,
                                button: sonicterm_app_core::MouseButton::Left,
                                mods: sonicterm_types::ModKey::empty(),
                                pos: sonicterm_app_core::LogicalPos { x: lx as f64, y: ly as f64 },
                            });
                        }
                        let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                        if self.dismiss_notification_at(
                            FrontmostKind::Main,
                            cursor_pos.0 as f32,
                            cursor_pos.1 as f32,
                        ) {
                            // When: dismiss_notification_at returns true, consume the press before terminal interaction.
                            return;
                        }
                        if let Some(ws) = self.main_mut() {
                            ws.mouse_down = true;
                        }
                        // re-arm the OS-drag
                        // handoff gate so the CursorMoved threshold check
                        // can fire once for the new gesture.
                        self.os_drag_handoff_started = false;
                        let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                        let (px, py) = (cursor_pos.0 as f32, cursor_pos.1 as f32);
                        let window_width =
                            self.main_window().map(|w| w.inner_size().width as f32).unwrap_or(0.0);
                        let empty_tabs2 = sonicterm_ui::tabs::TabBar::new();
                        let layout = TabBarLayout::compute_with_height(
                            self.main_tabs().unwrap_or(&empty_tabs2),
                            window_width,
                            self.main_renderer()
                                .map(|r| r.tab_bar_logical_height())
                                .unwrap_or(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT),
                        )
                        .with_top_offset(
                            self.main_renderer().map(|r| r.tab_bar_y_offset()).unwrap_or(0.0),
                        )
                        .with_visible(self.tab_bar_visible);
                        let tab_action = layout.hit(px, py);
                        if tab_action.is_some() {
                            // When: tab_action is Some, activate or close it before pane input.
                            match tab_action {
                                Some(sonicterm_ui::tabbar_view::TabHit::Activate(i)) => {
                                    self.activate_main_tab(i);
                                    // Record the press so a subsequent drag
                                    // below the tab bar can be promoted to a
                                    // tear-out gesture.
                                    if let Some(ws) = self.main_mut() {
                                        ws.pressed_tab = Some(i);
                                        ws.drag_session =
                                            Some(crate::tab_drag::DragSession::new(i, (px, py)));
                                    }
                                }
                                Some(sonicterm_ui::tabbar_view::TabHit::Close(i)) => {
                                    self.close_tab_at(i)
                                }
                                None => unreachable!("tab_action.is_some() checked above"),
                            }
                            if self.main_tabs().map(|t| t.is_empty()).unwrap_or(true) {
                                // Empty main_tabs hides main and exits only if no child terminal survives.
                                if self.child_window_count() == 0 {
                                    self.hide_main_window();
                                    el.exit();
                                } else {
                                    // When: child_window_count is nonzero, keep the app alive after hiding main.
                                    self.hide_main_window();
                                }
                            }
                            if let Some(w) = self.main_window() {
                                w.request_redraw();
                            }
                            // Keep mouse_down=true when we recorded a tab
                            // press so cursor-move can promote it to a
                            // tear-out. Close hits consume the click fully.
                            if let Some(ws) = self.main_mut() {
                                if ws.pressed_tab.is_none() {
                                    ws.mouse_down = false;
                                }
                            }
                            return;
                        }
                        if let Some(hit) = self.splitter_hit_at(px, py) {
                            // When: splitter_hit_at returns hit, capture a resize gesture instead of selecting text.
                            if let Some(ws) = self.main_mut() {
                                ws.splitter_drag = Some(super::SplitterDragState {
                                    splitter: hit.id,
                                    axis: hit.axis,
                                    last_pos: (px, py),
                                });
                                ws.selection = None;
                            }
                            self.set_splitter_cursor(hit.axis);
                            if let Some(w) = self.main_window() {
                                w.request_redraw();
                            }
                            return;
                        }
                        // B1b borrow-split: snapshot renderer geometry up front so the
                        // pane-rect compute can run alongside `self.tab_states.get_mut()`
                        // and the hyperlink path can re-borrow `self`.
                        let renderer_geom = self.main_renderer().map(|r| {
                            let (w, h) = r.logical_size();
                            (
                                w,
                                h,
                                (r.top_inset() - r.padding_top_px()).max(0.0),
                                0.0,
                                0.0,
                                r.bottom_inset(),
                                0.0,
                            )
                        });
                        let pixel_to_cell = {
                            let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                            self.main_renderer()
                                .and_then(|r| r.pixel_to_cell(cp.0 as f32, cp.1 as f32))
                        };
                        // scrollbar input has priority over
                        // selection start. Done BEFORE the pane-focus switch
                        // and selection-anchor path so a thumb-drag never
                        // doubles as a text drag. `scrollbar_hit_at` returns
                        // `Miss` for any click outside the active pane's bar,
                        // including clicks on inactive panes' bars (those
                        // need a focus-switch click first — matches the
                        // behaviour of other terminals).
                        {
                            let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                            let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                            match self.scrollbar_hit_at(lx, ly) {
                                crate::app::scrollbar_input::HitOutcome::Miss => {
                                    // When: HitOutcome::Miss leaves the press for pane selection routing.
                                }
                                crate::app::scrollbar_input::HitOutcome::StartDrag(state) => {
                                    // When: HitOutcome::StartDrag carries state, capture the scrollbar drag.
                                    if let Some(ws) = self.main_mut() {
                                        ws.scrollbar_drag = Some(state);
                                        // Suppress the residual selection-drag
                                        // path: mouse_down stays true (so
                                        // CursorMoved routes here) but no
                                        // Selection was created.
                                    }
                                    if let Some(w) = self.main_window() {
                                        w.request_redraw();
                                    }
                                    return;
                                }
                                crate::app::scrollbar_input::HitOutcome::PageUp => {
                                    // When: HitOutcome::PageUp pages the track toward older scrollback.
                                    self.scrollbar_track_page(false);
                                    return;
                                }
                                crate::app::scrollbar_input::HitOutcome::PageDown => {
                                    // When: HitOutcome::PageDown pages the track toward live output.
                                    self.scrollbar_track_page(true);
                                    return;
                                }
                            }
                        }
                        if let Some((w, h, top, pl, pr_pad, bottom, pb)) = renderer_geom {
                            // When: renderer_geom is Some, derive pane hit regions for focus and selection.
                            let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                            let pane_rects = self
                                .main_tab_states()
                                .and_then(|ts| ts.get(tab_idx))
                                .map(|st| {
                                    let outer = sonicterm_ui::pane::Rect::new(
                                        pl,
                                        top,
                                        (w - pl - pr_pad).max(0.0),
                                        (h - top - bottom - pb).max(0.0),
                                    );
                                    st.tree.layout(outer)
                                })
                                .unwrap_or_default();
                            if pane_rects.len() > 1 {
                                // When: pane_rects has multiple entries, focus the pane containing the press.
                                let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                                let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                                let mut newly_focused = None;
                                for (id, rect) in &pane_rects {
                                    if lx >= rect.x
                                        && lx < rect.x + rect.w
                                        && ly >= rect.y
                                        && ly < rect.y + rect.h
                                    {
                                        // When: lx and ly lie within rect, make that pane leaf active.
                                        if let Some(st) = self
                                            .main_tab_states_mut()
                                            .and_then(|ts| ts.get_mut(tab_idx))
                                        {
                                            if st.active_pane != *id {
                                                st.active_pane = *id;
                                                newly_focused = Some(*id);
                                                if let Some(panes) = self.main_panes() {
                                                    mark_all_panes_dirty(panes);
                                                }
                                            }
                                        }
                                        break;
                                    }
                                }
                                if let Some(id) = newly_focused {
                                    // A newly focused pane flashes its border to expose the focus change.
                                    if let Some(r) = self.main_renderer_mut() {
                                        r.flash_pane_focus(id);
                                    }
                                }
                            }
                            // `pixel_to_cell` expects PHYSICAL px.
                            if let Some((row, col)) = pixel_to_cell {
                                // When: pixel_to_cell is Some(row, col), handle links and initialize selection.

                                // Modifier-click on a hyperlink opens it.
                                // On macOS the modifier is Cmd (super); on
                                // Windows / Linux it's Ctrl. The parser lock
                                // is released inside hyperlink_uri_at before
                                // we ever call sonicterm_cfg::url_open::open,
                                // so no grid lock is held across the spawn.
                                // Dispatch decision lives in the pure
                                // `dispatch_modifier_click` helper so it can
                                // be unit-tested without a real winit mouse
                                // event (see its tests in sonicterm-cfg).
                                let opened = sonicterm_cfg::url_open::dispatch_modifier_click(
                                    self.url_open_modifier_held(),
                                    self.hyperlink_uri_at(row, col),
                                    |uri| {
                                        let r = sonicterm_cfg::url_open::open(uri);
                                        if let Err(ref e) = r {
                                            tracing::warn!("url_open failed: {e}");
                                        }
                                        r
                                    },
                                );
                                if opened.is_some() {
                                    // When: opened is Some, consume the modifier-click before selection.
                                    if let Some(ws) = self.main_mut() {
                                        ws.mouse_down = false;
                                    }
                                    return;
                                }
                                // Multi-click selection: 1 = point, 2 = word,
                                // 3 = line. Record the click against the main
                                // window's streak state, then build the right
                                // Selection. word_at/line_at need the grid; the
                                // helpers below lock the parser only to read it
                                // and return an owned (Copy) Selection, so no
                                // grid lock is held across selection_set/redraw
                                // (CLAUDE.md §4).
                                let click_count = self
                                    .main_mut()
                                    .map(|ws| ws.register_click(row, col))
                                    .unwrap_or(1);
                                // Resolve the absolute row and content baseline
                                // under one parser lock. A selection must not be
                                // born with dirty/content state older than itself.
                                let selection_state = self.viewport_row_selection_state(row);
                                let abs_row = selection_state.map_or(row as u64, |state| state.0);
                                let sel = match click_count {
                                    2 => self.word_selection_at(abs_row, col),
                                    3 => self.line_selection_at(abs_row),
                                    _ => selection_state.map_or_else(
                                        || Selection::new(abs_row, col),
                                        |(_, pane_id, seq, is_alt, evicted)| {
                                            Selection::new(abs_row, col)
                                                .with_content_state(pane_id, seq, is_alt, evicted)
                                        },
                                    ),
                                };
                                // Record the WezTerm-style drag granularity +
                                // anchor cell so a subsequent CursorMoved (button
                                // held) extends by word / line / cell. The anchor
                                // is the press cell (ABSOLUTE row); word/line drags
                                // recompute the anchor word/line from it on each
                                // move.
                                if let Some(ws) = self.main_mut() {
                                    ws.select_mode = match click_count {
                                        2 => SelectMode::Word,
                                        3 => SelectMode::Line,
                                        _ => SelectMode::Cell,
                                    };
                                    ws.select_anchor = (abs_row, col);
                                }
                                self.selection_set(Some(sel));
                                if let Some(panes) = self.main_panes() {
                                    mark_all_panes_dirty(panes);
                                }
                            }
                        }
                        if let Some(w) = self.main_window() {
                            w.request_redraw();
                        }
                    }
                    ElementState::Released => {
                        // Released primary-pointer state commits or cancels active gestures.

                        // Notify the reducer of the release transition so selection
                        // observability emits Render(Selection).
                        {
                            let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                            let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                            self.dispatch_intent(sonicterm_app_core::AppIntent::MouseButton {
                                window: sonicterm_types::WindowKey::new(0),
                                pressed: false,
                                button: sonicterm_app_core::MouseButton::Left,
                                mods: sonicterm_types::ModKey::empty(),
                                pos: sonicterm_app_core::LogicalPos { x: lx as f64, y: ly as f64 },
                            });
                        }
                        // end any active scrollbar drag — do this
                        // unconditionally on release so a drag that ended
                        // outside the bar still clears state.
                        if let Some(ws) = self.main_mut() {
                            ws.scrollbar_drag = None;
                            ws.splitter_drag = None;
                            ws.splitter_hover = None;
                        }
                        // Commit-on-release: read the live drag session and
                        // foreign drop target, decide what to do via the
                        // pure compute_action helper, then execute.
                        let (session, foreign, pressed) = self
                            .main_mut()
                            .map(|ws| {
                                let s = ws.drag_session.take();
                                let f = ws.drag_target.take();
                                let p = ws.pressed_tab.take();
                                ws.mouse_down = false;
                                (s, f, p)
                            })
                            .unwrap_or((None, None, None));
                        if let Some(r) = self.main_renderer_mut() {
                            r.set_drag_chip(None);
                        }
                        if let (Some(s), Some(idx)) = (session, pressed) {
                            let window_width = self
                                .main_window()
                                .map(|w| w.inner_size().width as f32)
                                .unwrap_or(0.0);
                            let empty_tabs3 = sonicterm_ui::tabs::TabBar::new();
                            let layout = TabBarLayout::compute_with_height(
                                self.main_tabs().unwrap_or(&empty_tabs3),
                                window_width,
                                self.main_renderer()
                                    .map(|r| r.tab_bar_logical_height())
                                    .unwrap_or(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT),
                            )
                            .with_top_offset(
                                self.main_renderer().map(|r| r.tab_bar_y_offset()).unwrap_or(0.0),
                            );
                            let action = crate::tab_drag::compute_action(&s, foreign, &layout);
                            match action {
                                crate::tab_drag::DragAction::ReturnToOriginalBar => {
                                    // When: DragAction::ReturnToOriginalBar preserves the original position.

                                    // Source-bar release preserves the original tab position.
                                    // No-op — moving back over the source
                                    // bar before releasing cancels the drag.
                                }
                                crate::tab_drag::DragAction::ReorderTab { from, to } => {
                                    // Source-bar release at a new slot reorders model and state together.
                                    // — must move Tab +
                                    // TabState in lock-step, otherwise the
                                    // title moves but `tab_states[i]`
                                    // (active pane + PaneTree leaf-ids)
                                    // stays bound to the old slot →
                                    // title-N points at the OTHER tab's
                                    // PTY. Also clamps `to` for the
                                    // drag-past-last case (`TabBar::reorder`
                                    // silently no-ops when `to == len`,
                                    // which looked like the tab vanished).
                                    // Logic lives on `WindowState::reorder_tab`
                                    // so the regression tests in
                                    // `tests/reorder_main_window_pane_follows_title.rs`
                                    // exercise the same path production runs.
                                    if let Some(id) = self.main_window_id {
                                        if let Some(ws) = self.windows.get_mut(&id) {
                                            ws.reorder_tab(from, to);
                                        }
                                    }
                                }
                                crate::tab_drag::DragAction::MergeIntoWindow(target) => {
                                    // Another-window release transfers the tab into that target.
                                    self.merge_main_into_child(idx, target);
                                }
                                crate::tab_drag::DragAction::TearOutToNewWindow { .. } => {
                                    // A release without an existing bar moves the tab into a new window.
                                    self.tear_out_tab(el, idx);
                                }
                            }
                            if let Some(w) = self.main_window() {
                                w.request_redraw();
                            }
                        }
                        if let Some(sel_present) =
                            self.main().map(|ws| ws.selection.as_ref().map(|s| s.is_empty()))
                        {
                            // Main selection presence distinguishes no selection from an empty range.
                            if sel_present == Some(true) {
                                // An empty completed selection is cleared instead of rendered.
                                self.selection_set(None);
                                if let Some(panes) = self.main_panes() {
                                    mark_all_panes_dirty(panes);
                                }
                                if let Some(w) = self.main_window() {
                                    w.request_redraw();
                                }
                            }
                        }
                        let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                        if !self.refresh_splitter_hover(cp.0 as f32, cp.1 as f32) {
                            self.refresh_hovered_url();
                        }
                    }
                }
            }

            // -- IME (CJK / multi-key input methods) --
            WindowEvent::Ime(ime_event) => {
                // When: WindowEvent::Ime supplies ime_event, route it to the active text-input owner.
                if self.command_palette_handle_ime(&ime_event) {
                    // When: command_palette_handle_ime consumes ime_event, stop terminal IME routing.
                    return;
                }
                let committed = if let Some(ws) = self.main_mut() {
                    match ime_event {
                        Ime::Enabled => {
                            ws.ime.handle_enabled();
                            String::new()
                        }
                        Ime::Disabled => {
                            ws.ime.handle_disabled();
                            String::new()
                        }
                        Ime::Preedit(text, cursor) => {
                            ws.ime.handle_preedit(&text, cursor);
                            String::new()
                        }
                        Ime::Commit(text) => {
                            ws.ime.handle_commit(&text);
                            ws.ime.take_commits()
                        }
                    }
                } else {
                    // When: main_mut is None, there is no terminal IME state to commit.
                    String::new()
                };
                if !committed.is_empty() {
                    // When: committed is nonempty; search_active and copy_mode select search, discard, or PTY delivery.
                    if self.search_active() {
                        self.search_handle_ime_commit(&committed);
                    } else if self.main().map(|ws| ws.copy_mode.is_some()).unwrap_or(false) {
                        // Read-only/copy mode discards IME commits instead of forwarding them.
                        // Read-only/copy mode is navigation-only. IME commit
                        // events can arrive without a KeyboardInput path, so
                        // drop them explicitly instead of forwarding to PTY.
                    } else {
                        // With no search or copy mode, committed text goes to the PTY.
                        self.write_to_pty(committed.into_bytes());
                    }
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }

            // -- Keyboard --
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                // When: KeyboardInput event.state is Pressed, route the key through active modes.

                // Quit confirmation guard: intercept the Cmd+Q chord before any
                // mode routing (palette/search/copy-mode/PTY) so it behaves
                // identically everywhere. The first press only arms the guard
                // and shows the red prompt; a second non-repeat press quits.
                //
                // Cmd+Q is a macOS system chord, so on macOS it triggers the
                // guard even when the active keymap has no `super+q` binding
                // (a user's edited/symlinked keymap easily omits it). We only
                // stand down if the user deliberately rebound `super+q` to a
                // different action. An explicit `quit_app` binding on any
                // platform is always honored.
                if let Some(key_str) = key_event_to_string(&event, self.main_modifiers()) {
                    // When: key_event_to_string returns key_str, resolve its quit binding before mode routing.
                    if is_quit_chord(&key_str, self.keymap.lookup(&key_str)) {
                        // When: is_quit_chord accepts key_str, arm or confirm the quit guard.
                        self.on_quit_chord_pressed(event.repeat);
                        return;
                    }
                }
                if self.command_palette.is_open() {
                    // When: command_palette is open, consume keys in palette state.

                    // Let the toggle binding (super+shift+P) still close
                    // the palette; everything else routes into palette
                    // state and is NOT forwarded to the pty.
                    if let Some(key_str) = key_event_to_string(&event, self.main_modifiers()) {
                        // When: key_event_to_string returns key_str, check whether it toggles the open palette.
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            // When: keymap lookup returns action, allow only the palette toggle through dispatch.
                            if matches!(action, Action::OpenCommandPalette) {
                                // When: action matches OpenCommandPalette, dispatch it to close the palette.
                                self.run_action_for_window(&action, win_id);
                                if let Some(w) = self.main_window() {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.command_palette_handle_key(&event);
                    self.drain_pending_window_creates(el);
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                    return;
                }
                // While an IME composition is in flight, the OS owns the
                // keystrokes — they will be delivered to us as Ime events
                // instead. Forwarding them here would double-type. Esc
                // cancels the in-flight composition (preedit dropped, no
                // bytes sent to the PTY) instead of being forwarded.
                if self.main().map(|ws| ws.ime.is_composing()).unwrap_or(false) {
                    // When: ime.is_composing is true, keep raw KeyboardInput out of the PTY.
                    if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                        if let Some(ws) = self.main_mut() {
                            ws.ime.cancel();
                        }
                        if let Some(w) = self.main_window() {
                            w.request_redraw();
                        }
                    }
                    return;
                }
                if self.search_active() {
                    // When: search_active is true, route edits to search and other bindings through dispatch.
                    let mods = self.main_modifiers();
                    let is_search_text_edit =
                        super::text_edit::search_text_edit_for_key(&event.logical_key, mods)
                            .is_some();
                    if !is_search_text_edit {
                        // When: is_search_text_edit is false, resolve non-edit keymap actions first.
                        if let Some(key_str) = key_event_to_string(&event, mods) {
                            // When: key_event_to_string returns key_str, look up its search-mode action.
                            if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                                // When: keymap lookup returns action, preserve OpenSearch for the search handler.
                                if !matches!(action, Action::OpenSearch) {
                                    // When: matches does not find Action::OpenSearch, dispatch the action.
                                    self.run_action_for_window(&action, win_id);
                                    if let Some(w) = self.main_window() {
                                        w.request_redraw();
                                    }
                                    return;
                                }
                            }
                        }
                    }
                    self.search_handle_key(&event, mods);
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                    return;
                }
                if self.main().map(|ws| ws.copy_mode.is_some()).unwrap_or(false) {
                    // When: copy_mode is Some, allow only read-only-safe actions before local navigation.
                    for key_str in key_to_strings(&event.logical_key, self.main_modifiers()) {
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            // When: keymap lookup returns action, test it against the read-only whitelist.
                            if super::keymap_dispatch::read_only_allows_action(&action)
                                && self.run_action_for_window(&action, win_id)
                            {
                                // When: read_only_allows_action and run_action_for_window succeed, finish dispatch.
                                self.drain_pending_window_creates(el);
                                if let Some(w) = self.main_window() {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.copy_mode_handle_key(&event);
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                    return;
                }
                for key_str in key_to_strings(&event.logical_key, self.main_modifiers()) {
                    if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                        // When: keymap lookup returns action, choose passthrough or application dispatch.
                        if super::keymap_dispatch::terminal_input_passthrough_binding(
                            &key_str, &action,
                        ) {
                            // When: terminal_input_passthrough_binding accepts key_str and action, try the next encoding.
                            continue;
                        }
                        if self.run_action_for_window(&action, win_id) {
                            // When: run_action_for_window consumes action, complete pending window work.
                            self.drain_pending_window_creates(el);
                            if let Some(w) = self.main_window() {
                                w.request_redraw();
                            }
                            return;
                        }
                    }
                }
                // Read the focused pane's kitty keyboard flags from the
                // lock-free per-pane snapshot (the VT loop mirrors them out of
                // the parser after each batch). Avoids taking `parser.lock()`
                // on the keypress path — that lock is held by the VT thread
                // while parsing output, so blocking on it added input latency
                // whenever output was streaming. When non-zero,
                // `encode_key` emits CSI-u forms (e.g. Shift+Enter => CSI
                // 13;2u) so modern TUIs treat Shift+Enter as "insert newline".
                let kitty_flags = self
                    .active_pane()
                    .map(|pane| pane.kitty_flags.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(0);
                let app_cursor = self
                    .active_pane()
                    .map(|pane| pane.app_cursor_keys.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(false);
                if let Some(bytes) =
                    encode_key(&event, self.main_modifiers(), kitty_flags, app_cursor)
                {
                    // Encoded key bytes are sent before clearing transient view state.
                    self.write_to_pty(bytes);
                    // Scroll-to-bottom on Enter (#B12): pressing Enter while
                    // scrolled up in history should jump back to the live
                    // bottom so the latest input/output is visible. Plain Enter
                    // only — Shift+Enter inserts a newline and must not jump.
                    let is_plain_enter = matches!(event.logical_key, Key::Named(NamedKey::Enter))
                        && !self.main_modifiers().shift_key();
                    if is_plain_enter {
                        // Plain Enter returns the active pane to live output.
                        if let Some(id) = self.active_pane_id() {
                            // The active pane receives the live-output viewport update.
                            if let Some(pane) = self.main_mut().and_then(|ws| ws.panes.get_mut(&id))
                            {
                                // The resolved active pane is inspected for a historical viewport.
                                if pane.viewport_top_abs.is_some() {
                                    // A historical viewport is cleared back to live output.
                                    pane.viewport_top_abs = None; // back to live
                                    if let Some(panes) = self.main_panes() {
                                        mark_all_panes_dirty(panes);
                                    }
                                    if let Some(w) = self.main_window() {
                                        w.request_redraw();
                                    }
                                }
                            }
                        }
                    }
                    if self.main().map(|ws| ws.selection.is_some()).unwrap_or(false) {
                        // A selection becomes stale after terminal input and is cleared.
                        self.selection_set(None);
                        if let Some(panes) = self.main_panes() {
                            mark_all_panes_dirty(panes);
                        }
                        if let Some(w) = self.main_window() {
                            w.request_redraw();
                        }
                    }
                }
            }

            _ => {
                // When: event matches no handled WindowEvent variant, leave application state unchanged.
            }
        }
    }
}
impl App {
    fn copy_mode_handle_key(&mut self, event: &KeyEvent) {
        let Some(mut state) = self.main_mut().and_then(|ws| ws.copy_mode.take()) else {
            // When: copy_mode.take returns None, there is no copy-mode key state to update.
            return;
        };
        let mut should_copy = false;
        let mut should_exit = false;

        let active_pane_id = self.active_pane_id();
        if let Some(pane) =
            active_pane_id.and_then(|id| self.main().and_then(|ws| ws.panes.get(&id)))
        {
            // When: active_pane_id resolves to pane, handle copy-mode navigation against its grid.
            let guard = pane.parser.lock();
            let grid = guard.grid();
            if let Some(quick_select) = state.quick_select.as_ref() {
                // When: quick_select is Some, interpret a hint key or escape and finish immediately.
                let mut copied_text = None;
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => should_exit = true,
                    Key::Character(s) => {
                        // Character keys resolve their first quick-select hint.
                        if let Some(ch) = s.chars().next() {
                            // The first character selects a quick-select target.
                            if let Some(text) = quick_select.text_for_hint(ch) {
                                // Resolved hint text is staged for the clipboard.
                                copied_text = Some(text.to_string());
                                should_exit = true;
                            }
                        }
                    }
                    _ => {
                        // When: logical_key matches neither Escape nor Character, leave quick select unchanged.
                    }
                }
                drop(guard);
                if let Some(text) = copied_text {
                    self.set_clipboard_text(text);
                }
                if !should_exit {
                    // An unmatched quick-select key restores copy mode.
                    self.copy_mode_set(Some(state));
                }
                if let Some(panes) = self.main_panes() {
                    mark_all_panes_dirty(panes);
                }
                return;
            }
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
                    // When: logical_key has no copy-mode binding, leave state unchanged.
                }
            }

            if should_copy {
                if let Some(text) = copy_mode_selected_text(&state, grid) {
                    drop(guard);
                    self.set_clipboard_text(text);
                }
                should_exit = true;
            } else {
                // When: should_copy is false, update the viewport after copy-mode navigation.
                // Copy-mode navigation updates the viewport when no copy was requested.
                let new_view_top = GpuRenderer::copy_mode_view_top_after_move_legacy(
                    &state,
                    grid,
                    pane.viewport_top_abs,
                );
                drop(guard);
                if let Some(id) = active_pane_id {
                    if let Some(pane) = self.main_mut().and_then(|ws| ws.panes.get_mut(&id)) {
                        pane.viewport_top_abs = new_view_top;
                    }
                }
            }
        }

        if should_exit {
            self.copy_mode_set(None);
        } else {
            // When: should_exit is false, restore the updated copy-mode state.
            self.copy_mode_set(Some(state));
        }
        if let Some(panes) = self.main_panes() {
            mark_all_panes_dirty(panes);
        }
    }
}

impl App {
    fn main_pane_outer_rect(&self) -> Option<sonicterm_ui::pane::Rect> {
        let r = self.main_renderer()?;
        let (w, h) = r.logical_size();
        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
        let bottom = r.bottom_inset();
        Some(sonicterm_ui::pane::Rect::new(0.0, top, w.max(0.0), (h - top - bottom).max(0.0)))
    }

    fn splitter_hit_at(&self, x: f32, y: f32) -> Option<sonicterm_ui::pane::SplitterHit> {
        let outer = self.main_pane_outer_rect()?;
        let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
        self.main_tab_states()
            .and_then(|states| states.get(tab_idx))
            .and_then(|state| state.tree.hit_splitter(outer, SPLITTER_HIT_THICKNESS, x, y))
    }

    fn set_splitter_cursor(&self, axis: sonicterm_ui::pane::SplitAxis) {
        if let Some(w) = self.main_window() {
            let icon = match axis {
                sonicterm_ui::pane::SplitAxis::Vertical => CursorIcon::ColResize,
                sonicterm_ui::pane::SplitAxis::Horizontal => CursorIcon::RowResize,
            };
            w.set_cursor(icon);
        }
    }

    fn refresh_splitter_hover(&mut self, x: f32, y: f32) -> bool {
        if self.main().and_then(|ws| ws.splitter_drag.as_ref()).is_some() {
            // When: splitter_drag is Some, preserve its resize cursor and consume hover routing.
            return true;
        }
        let Some(hit) = self.splitter_hit_at(x, y) else {
            // When: splitter_hit_at returns None, clear any stale splitter hover cursor.
            let was_splitter =
                self.main_mut().map(|ws| ws.splitter_hover.take().is_some()).unwrap_or(false);
            if was_splitter {
                if let Some(w) = self.main_window() {
                    w.set_cursor(CursorIcon::Default);
                }
            }
            return false;
        };
        if let Some(ws) = self.main_mut() {
            ws.hovered_url = None;
            ws.hover_link = false;
            ws.splitter_hover = Some(hit.axis);
        }
        self.set_splitter_cursor(hit.axis);
        true
    }

    fn apply_splitter_drag(&mut self, x: f32, y: f32) -> bool {
        let Some(drag) = self.main().and_then(|ws| ws.splitter_drag.clone()) else {
            // When: splitter_drag is None, this motion is not a splitter gesture.
            return false;
        };
        let Some(outer) = self.main_pane_outer_rect() else {
            // When: main_pane_outer_rect is None, splitter geometry cannot be updated.
            return false;
        };
        let dx = x - drag.last_pos.0;
        let dy = y - drag.last_pos.1;
        if dx == 0.0 && dy == 0.0 {
            // When: dx and dy are both zero, consume the gesture without resizing.
            return true;
        }

        let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
        let changed = self
            .main_tab_states_mut()
            .and_then(|states| states.get_mut(tab_idx))
            .map(|state| state.tree.resize_splitter_by_delta(&drag.splitter, outer, dx, dy))
            .unwrap_or(false);

        if changed {
            if let Some(((cell_w, cell_h), inset)) = self.main_renderer().map(|r| {
                (
                    r.cell_size(),
                    [
                        r.padding_left_px(),
                        r.padding_right_px(),
                        r.padding_top_px(),
                        r.padding_bottom_px(),
                    ],
                )
            }) {
                let rects = self
                    .main_tab_states()
                    .and_then(|states| states.get(tab_idx))
                    .map(|state| state.tree.layout(outer))
                    .unwrap_or_default();
                if let Some(panes) = self.main_panes() {
                    crate::app::resize_panes_to_rects(panes, &rects, cell_w, cell_h, inset);
                }
            }
        }

        if let Some(ws) = self.main_mut() {
            if let Some(active) = ws.splitter_drag.as_mut() {
                active.last_pos = (x, y);
            }
            if changed {
                mark_all_panes_dirty(&ws.panes);
            }
        }
        self.set_splitter_cursor(drag.axis);
        if changed {
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
        true
    }
}

fn copy_mode_selected_text(state: &CopyModeState, grid: &Grid) -> Option<String> {
    let (start, end) = state.selected_range()?;
    if start == end {
        // When: start equals end, the copy-mode range contains no text.
        return None;
    }
    let out = plain_text_from_grid_range(grid, (start.0, start.1 as u64), (end.0, end.1 as u64));
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
#[path = "window_event_tests.rs"]
mod window_event_tests;
