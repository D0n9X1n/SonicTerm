//! Child-window `RedrawRequested` handling: one coherent frame per request, the
//! OS IME candidate anchor, and the tab-bar snapshot for cross-window drags.

use std::collections::BTreeSet;
use std::time::Instant;

use sonicterm_cfg::{config::Config, theme::Theme};
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_ui::{
    overlays::{
        command_palette_query_caret_prefix, search_bar_label, search_query_caret_prefix,
        PaletteLayout, SearchBarLayout, PALETTE_ROW_PAD_X, SEARCH_BAR_ICON_GAP,
        SEARCH_BAR_PAD_LEFT, SEARCH_BAR_PAD_RIGHT,
    },
    tabbar_view::TabBarLayout,
};
use winit::{event_loop::ActiveEventLoop, window::WindowId};

use super::scrollbar_visibility::ScrollbarMotion;
use super::{
    invalidate_selection_for_content, poll_command_events_for_child_window, App,
    RuntimeSmokeFailure,
};

const SEARCH_BADGE_ICON: &str = "";

fn estimate_overlay_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().map(|ch| if ch.is_ascii() { 0.58 } else { 1.0 }).sum::<f32>() * font_size
}

impl App {
    /// Render this child for `RedrawRequested`, or defer it to its own next frame boundary.
    // Ordering: the active pane's cursor_visible flag is a Relaxed per-frame snapshot; no other
    // state is ordered by it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_child_redraw_requested(
        &mut self,
        el: &ActiveEventLoop,
        win_id: WindowId,
        theme: &Theme,
        config: &Config,
        process_privileged: bool,
        palette_here: bool,
        was_dirty: bool,
        pty_burst: bool,
        scrollbar_motion: ScrollbarMotion,
        broadcast_participants: &BTreeSet<u64>,
    ) {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so this child closed and
            // has no frame left to render.
            return;
        };
        let mut timing = crate::app::render_timing::RenderTiming::start("child");
        // Rendering this frame: drop any pending-deferral marker
        // so the frame-boundary wakeup loop stops re-requesting
        // redraws on this child (an idle re-request loop would be
        // the forbidden unconditional heartbeat redraw).
        self.pending_redraw_windows.remove(&win_id);
        child.tabs.clear_expired_command_badges(Instant::now());
        poll_command_events_for_child_window(child, config);
        crate::app::refresh_window_tab_privileges(
            &mut child.tabs,
            &child.tab_states,
            &mut child.panes,
            !pty_burst,
        );
        if let Some(t) = timing.as_mut() {
            t.lap("poll");
        }
        let Some(renderer) = child.renderer.as_ref() else {
            // When: `renderer` is absent, no child geometry is available yet.
            return;
        };
        let (w, h) = renderer.logical_size();
        let top = (renderer.top_inset() - renderer.padding_top_px()).max(0.0);
        let outer = sonicterm_ui::pane::Rect::new(
            0.0,
            top,
            w.max(0.0),
            (h - top - renderer.bottom_inset()).max(0.0),
        );
        let _ = child;
        let sources = match self.child_visible_frame_sources(win_id, outer) {
            Ok(sources) => sources,
            Err(why) => {
                // When: `why` rejects topology, skip all assembly rather than presenting a partial pane set.
                self.visible_frame_unavailable(win_id, why, was_dirty, Instant::now());
                return;
            }
        };
        let tab_idx = sources.tab_index;
        let active_id = sources.active_id();
        let active_pos = sources.active_pos;
        let pane_rects = sources.rects();
        // The scheduler captures generations before the first parser or image lock.
        let super::visible_frame::HeldVisibleFrame {
            snapshot: frame_snapshot,
            mut guards,
            mut images,
        } = {
            // End the Result's drop scope before later branches release its borrowed sources.
            let collected = sources.try_collect(|| self.snapshot_window_redraw(win_id));
            match collected {
                Ok(frame) => frame,
                Err(why) => {
                    // When: `why` is contention, partial guards and image clones have already been released.
                    drop(collected);
                    drop(sources);
                    self.visible_frame_unavailable(win_id, why, was_dirty, Instant::now());
                    return;
                }
            }
        };
        self.refresh_target_hover_from_parsers(
            win_id,
            guards.iter().map(|(id, parser, _)| (*id, &**parser)),
        );
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: windows no longer contains win_id, discard its collected frame instead of presenting retained hover.
            return;
        };
        let frame_viewports = match sources.reconcile_viewports(&mut child.panes, &guards) {
            Ok(viewports) => viewports,
            Err(why) => {
                // When: `why` rejects an owner, drop the entire collection before returning to the adapter.
                drop(guards);
                drop(images);
                drop(sources);
                let _ = child;
                self.visible_frame_unavailable(win_id, why, was_dirty, Instant::now());
                return;
            }
        };
        child.coherent_frame_collected();
        let mut palette_for_render = palette_here.then_some(&mut self.command_palette);
        if let Some(t) = timing.as_mut() {
            t.lap("inline_images");
        }
        if let Some(palette) = palette_for_render.as_deref_mut().filter(|palette| palette.is_open())
        {
            palette.set_context(super::overlays::command_palette_context(
                child,
                Some(guards[active_pos].1.grid()),
            ));
            palette.set_tabs(&child.tabs, &self.i18n);
        }
        if let Some(pane) = child.panes.get_mut(&active_id) {
            // When: `panes` holds `active_id`, use the validated `active_pos` for title and overlays.
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
            if let Some(search) = child.tab_states.get_mut(tab_idx).and_then(|t| t.search.as_mut())
            {
                let grid = guards[active_pos].1.grid();
                let view_top =
                    GpuRenderer::resolved_view_top_abs_legacy(grid, frame_viewports.active);
                super::search_handle::prepare_search(search, active_id, grid, view_top);
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
            let mut panes_slice = super::visible_frame::pane_renders(
                &mut guards,
                &mut images,
                &frame_viewports,
                active_id,
                broadcast_participants,
                &scrollbar_alpha_map,
            );
            if let Some(t) = timing.as_mut() {
                t.lap("pane_slice");
            }
            // cursor_visible is per-pane (lives on
            // PaneState). Read from the active pane (already
            // borrowed mutably above) so the DECTCEM flag
            // survives tear-out of this child.
            let cursor_visible_now = pane.cursor_visible.load(std::sync::atomic::Ordering::Relaxed);
            let smoke_waiting_for_present = self
                .runtime_smoke
                .as_ref()
                .is_some_and(|smoke| smoke.is_waiting_for_adopted_present(win_id));
            let mut smoke_presented_count = None;
            let mut frame_settlement = None;
            if let Some(r) = child.renderer.as_mut() {
                r.set_render_timing_label("child");
                let outcome = r.render_with_outcome(
                    &mut panes_slice,
                    theme,
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
                    frame_viewports.active,
                    child.notification.as_ref(),
                    // The child's own hovered-URL cells, so torn-out
                    // windows get the same yellow-hint /
                    // accent-when-Cmd underline and glyph recolor as the
                    // main window.
                    child.hovered_url.as_ref().map(|h| h.to_cells()),
                    child.link_preview.as_ref(),
                );
                if let Some(recovery) = self.gpu_recovery.as_mut() {
                    recovery.observe_frame(r.device_generation(), &outcome, Instant::now());
                }
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.observe_recovery_frame(
                        win_id,
                        r.device_generation(),
                        &panes_slice,
                        &outcome,
                    );
                }
                if let Some(snapshot) = frame_snapshot.as_ref() {
                    let at = Instant::now();
                    child.last_render = at;
                    let settlement = super::redraw::FrameSettlement::of(&outcome);
                    frame_settlement = Some(settlement);
                    child.redraw.settle(snapshot.causes, settlement, at);
                    if child.hidden {
                        child.redraw.cancel_surface_probe();
                    }
                    if matches!(
                        settlement,
                        super::redraw::FrameSettlement::Presented
                            | super::redraw::FrameSettlement::Cached
                            | super::redraw::FrameSettlement::Settled
                    ) {
                        for captured in &snapshot.panes {
                            // Other panes are reconciled after the active PaneState borrow ends.
                            if captured.id == active_id {
                                pane.observed_output_generation = captured.generation;
                            }
                        }
                    }
                }
                // Map the typed outcome back to the compatibility result: only a
                // failure or the device's first stopped frame is an error here.
                if let Err(e) = outcome.into_render_result() {
                    tracing::warn!(
                        target: "sonicterm_app::app::child_window",
                        "child render error: {e}"
                    );
                    if smoke_waiting_for_present {
                        // Retain the presentation failure only while the adopted child proof is pending.
                        smoke_presented_count = Some(Err(RuntimeSmokeFailure::Present));
                    }
                } else if smoke_waiting_for_present {
                    // When: rendering succeeded while `smoke_waiting_for_present` is true, retain its frame count.
                    smoke_presented_count = Some(Ok(r.successful_frame_count()));
                }
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.note_render_attempt();
                }
            }
            let request_consumed = matches!(
                frame_settlement,
                Some(
                    super::redraw::FrameSettlement::Presented
                        | super::redraw::FrameSettlement::Cached
                        | super::redraw::FrameSettlement::Settled
                )
            );
            if request_consumed {
                // When: `request_consumed` is true, record every captured pane identity after releasing the active borrow.
                let _ = pane;
                if let Some(snapshot) = frame_snapshot.as_ref() {
                    super::redraw::settle_pane_generations(&mut child.panes, snapshot);
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
                    drop(sources);
                    let _ = child;
                    let released = self.close_child_window(win_id);
                    self.warm_window_pool.clear();
                    let complete = self
                        .runtime_smoke
                        .as_mut()
                        .is_some_and(|smoke| smoke.finish_warm_release(win_id, released));
                    tracing::info!(
                        target: "sonicterm_app::app::child_window",
                        released,
                        "runtime smoke child presented and released"
                    );
                    if !complete {
                        tracing::error!(
                            target: "sonicterm_app::app::child_window",
                            "runtime smoke could not prove adopted child release"
                        );
                    }
                    if !complete
                        || self
                            .runtime_smoke
                            .as_ref()
                            .is_some_and(|smoke| smoke.outcome().is_some())
                    {
                        // A terminal outcome stops the smoke; otherwise a fresh-window phase still remains.
                        el.exit();
                    }
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
                if let (Some(win), Some(r)) = (child.window.as_ref(), child.renderer.as_ref()) {
                    if palette_here && self.command_palette.is_open() {
                        child.ime_cursor_throttle.reset();
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
                            let prefix =
                                command_palette_query_caret_prefix(&palette, child.ime.preedit());
                            let text_x = layout.query_row.x + PALETTE_ROW_PAD_X * scale;
                            let caret_x = text_x + estimate_overlay_text_width(&prefix, font_size);
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
                        child.ime_cursor_throttle.reset();
                        let preedit = child.ime.preedit();
                        let search_label = search_bar_label(search, preedit);
                        let search_prefix = search_query_caret_prefix(search, preedit);
                        let window_size = win.inner_size();
                        let scale = r.scale_factor();
                        let font_size = r.font_size() * scale;
                        let icon_w = r.measure_overlay_text_width(SEARCH_BADGE_ICON, font_size);
                        let content_w = icon_w
                            + SEARCH_BAR_ICON_GAP * scale
                            + r.measure_overlay_text_width(&search_label, font_size);
                        let row =
                            u8::from(child.copy_mode.as_ref().is_some_and(|cm| cm.is_read_only()));
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
                        let prefix_w = r.measure_overlay_text_width(&search_prefix, font_size);
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
                    } else {
                        // When: neither `palette_here` nor `search` owns input, publish the active terminal pane's IME anchor.
                        if let Some([x, y]) = r.pane_grid_origin(active_id) {
                            // Child IME follows the planned text origin rather than raw pane padding.
                            let pane = guards[active_pos].2;
                            let rect = sonicterm_ui::pane::Rect::new(x, y, pane.w, pane.h);
                            super::update_terminal_ime_cursor_area(
                                &mut child.ime_cursor_throttle,
                                (active_id, rect),
                                (cur_row, cur_col),
                                (r.cell_w, r.cell_h),
                                (0.0, 0.0),
                                |pos, size| win.set_ime_cursor_area(pos, size),
                            );
                        }
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
                let inner_origin = win.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
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
}
