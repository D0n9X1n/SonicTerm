//! Child-window `RedrawRequested` handling: one coherent frame per request, the
//! OS IME candidate anchor, and the tab-bar snapshot for cross-window drags.

use std::collections::BTreeSet;
use std::time::Instant;

use sonicterm_cfg::{config::Config, theme::Theme};
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_ui::tabbar_view::TabBarLayout;
use winit::{event_loop::ActiveEventLoop, window::WindowId};

use super::scrollbar_visibility::ScrollbarMotion;
use super::{
    invalidate_selection_for_content, poll_command_events_for_child_window, App,
    RuntimeSmokeFailure,
};

impl App {
    /// Render this child for `RedrawRequested`, or defer it to its own next frame boundary.
    // Ordering: the active pane's cursor_visible flag is a Relaxed per-frame snapshot; no other
    // state is ordered by it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_child_redraw_requested(
        &mut self,
        event_loop: &ActiveEventLoop,
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
        // The bar holds its widths while a tab gesture runs in any window or this window's own
        // pointer rests on it.
        let tab_bar_band = self
            .windows
            .get(&win_id)
            .and_then(|window| window.renderer.as_ref())
            .and_then(|renderer| renderer.tab_bar_band());
        let hold_tab_widths = self.tab_widths_held_in(win_id, tab_bar_band);
        // A burst frame reads the cache only; other frames set demand, never probe.
        let fg_probes = (!pty_burst).then(|| std::sync::Arc::clone(&self.fg_probes));
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
            fg_probes.as_deref(),
            Instant::now(),
        );
        if let Some(timing) = timing.as_mut() {
            timing.lap("poll");
        }
        let Some(renderer) = child.renderer.as_ref() else {
            // When: `renderer` is absent, no child geometry is available yet.
            child.redraw.finish_admitted_yield(super::parser_yield::YieldLoss::Invalid);
            return;
        };
        let (surface_width, surface_height) = renderer.logical_size();
        let top = (renderer.top_inset() - renderer.padding_top_px()).max(0.0);
        let outer = sonicterm_ui::pane::Rect::new(
            0.0,
            top,
            surface_width.max(0.0),
            (surface_height - top - renderer.bottom_inset()).max(0.0),
        );
        let _ = child;
        let sources = match self.child_visible_frame_sources(win_id, outer) {
            Ok(sources) => sources,
            Err(why) => {
                // When: `why` rejects topology, skip all assembly rather than presenting a partial pane set.
                let now = self.dispatch_now();
                self.visible_frame_unavailable(win_id, why, was_dirty, now);
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
                    let now = self.dispatch_now();
                    self.visible_frame_unavailable(win_id, why, was_dirty, now);
                    return;
                }
            }
        };
        // Recheck the synchronized-output hold under the guards; a held frame is abandoned unsettled.
        let sync_states: Vec<_> =
            guards.iter().map(|(id, parser, _)| (*id, parser.synchronized_output())).collect();
        let now = self.dispatch_now();
        if self.abandon_synchronized_frame(win_id, &sync_states, now) {
            // When: `abandon_synchronized_frame` holds the frame, release the collection unsettled and unreceipted.
            drop(guards);
            drop(images);
            drop(sources);
            return;
        }
        self.refresh_target_hover_from_parsers(
            win_id,
            guards.iter().map(|(id, parser, _)| (*id, &**parser)),
        );
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: windows no longer contains win_id, discard its collected frame instead of presenting retained hover.
            return;
        };
        // Reconcile, then apply the previous frame's receipts under these guards, before planning.
        let frame_viewports = match sources.reconcile_and_apply_receipts(child, &mut guards) {
            Ok(viewports) => viewports,
            Err(why) => {
                // When: `why` rejects an owner, drop the entire collection before returning to the adapter.
                drop(guards);
                drop(images);
                drop(sources);
                let _ = child;
                let now = self.dispatch_now();
                self.visible_frame_unavailable(win_id, why, was_dirty, now);
                return;
            }
        };
        child.coherent_frame_collected();
        let mut palette_for_render = palette_here.then_some(&mut self.command_palette);
        if let Some(timing) = timing.as_mut() {
            timing.lap("inline_images");
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
            if let Some(probes) = fg_probes.as_deref() {
                super::privilege::demand_frame_foreground(probes, active_id, pane, Instant::now());
            }
            let _ = crate::app::refresh_active_tab_title(
                &mut child.tabs,
                pane,
                &guards[active_pos].1,
                tab_idx,
            );
            if let Some(search) =
                child.tab_states.get_mut(tab_idx).and_then(|tab_state| tab_state.search.as_mut())
            {
                let grid = guards[active_pos].1.grid();
                let view_top =
                    GpuRenderer::resolved_view_top_abs_legacy(grid, frame_viewports.active);
                super::search_handle::prepare_search(search, active_id, grid, view_top);
            }
            let search =
                child.tab_states.get(tab_idx).and_then(|tab_state| tab_state.search.as_ref());
            if let Some(timing) = timing.as_mut() {
                timing.lap("title_search");
            }
            // Compute the per-pane fade alpha so torn-out windows show
            // the scrollbar and auto-hide it like the main window.
            let scrollbar_now = Instant::now();
            let scrollbar_alpha_map: std::collections::HashMap<u64, f32> = {
                let mode = config.appearance.scrollbar;
                let drag_pane = child.scrollbar_drag.as_ref().map(|drag| drag.pane_id);
                let cursor = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
                let rects: Vec<(u64, f32, f32, f32, f32)> = pane_rects
                    .iter()
                    .map(|(id, rect)| (*id, rect.x, rect.y, rect.w, rect.h))
                    .collect();
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
                child.scrollbar_vis.values().any(|state| {
                    crate::app::scrollbar_visibility::is_animating(state, mode, scrollbar_motion)
                })
            };
            if let Some(timing) = timing.as_mut() {
                timing.lap("scrollbar");
            }
            // Every reader after the render call takes its copy here: the call releases the guards.
            let cursor_copy = {
                let grid = guards[active_pos].1.grid();
                (grid.cursor.row, grid.cursor.col)
            };
            let cursor_rect_copy = guards[active_pos].2;
            let recovery_sample = self.runtime_smoke.as_ref().and_then(|smoke| {
                smoke.recovery_marker_sample(
                    win_id,
                    guards
                        .iter()
                        .map(|(id, parser, _)| (*id, parser.grid(), frame_viewports.of(*id))),
                )
            });

            if let Some(timing) = timing.as_mut() {
                timing.lap("pane_slice");
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
            // The renderer call's settlement, completed once the frame's borrows end.
            let mut frame_completion = None;
            // Named by source-text tests that embed this file.
            #[allow(clippy::min_ident_chars)]
            if let Some(r) = child.renderer.as_mut() {
                // Keep the widths on screen, then measure changed titles with the tab font right
                // before drawing; hit-testing reads the stored widths of the frame on screen.
                let drawn_tab_widths = child.tabs.laid_out_widths();
                // Prepare fonts first: a published fallback face invalidates placeholders and
                // stored tab widths before anything is measured or planned.
                let fonts = r.begin_frame_fonts();
                r.measure_tab_widths(
                    &fonts,
                    &mut child.tabs,
                    process_privileged,
                    hold_tab_widths,
                    Instant::now(),
                );
                r.set_render_timing_label("child");
                // The source owns the guards and media; they are released before presentation.
                let sonicterm_gpu::core::FrameOutcome { outcome, receipts } = r.render_releasing(
                    &fonts,
                    super::visible_frame::HeldFrameSource {
                        guards,
                        images: std::mem::take(&mut images),
                        viewports: &frame_viewports,
                        active: active_id,
                        broadcast: broadcast_participants,
                        scrollbar_alpha: &scrollbar_alpha_map,
                    },
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
                    child.hovered_url.as_ref().map(|hovered_url| hovered_url.to_cells()),
                    child.link_preview.as_ref(),
                );
                // Keep the new widths only if this frame reached the screen, so hit-testing
                // matches the bar the user sees.
                super::tab_widths::settle_tab_widths(&mut child.tabs, drawn_tab_widths, &outcome);
                if let Some(recovery) = self.gpu_recovery.as_mut() {
                    recovery.observe_frame(r.device_generation(), &outcome, Instant::now());
                }
                // A presented frame's receipts replace the pending set emptied at collection.
                sources.store_presented(&mut child.pending_receipts, receipts);
                if let (Some(smoke), Some(sample)) =
                    (self.runtime_smoke.as_mut(), recovery_sample.as_ref())
                {
                    smoke.observe_recovery_frame(win_id, r.device_generation(), sample, &outcome);
                }
                if frame_snapshot.is_some() {
                    // A captured pre-lock snapshot completes with the renderer's own settlement.
                    let settlement = super::redraw::FrameSettlement::of(&outcome);
                    frame_completion = Some(settlement);
                }
                // Map the typed outcome back to the compatibility result: only a
                // failure or the device's first stopped frame is an error here.
                if let Err(error) = outcome.into_render_result() {
                    tracing::warn!(
                        target: "sonicterm_app::app::child_window",
                        "child render error: {error}"
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
            } else {
                // When: the child has no renderer, nothing is drawn; release the frame's guards here too.
                drop(guards);
            }
            if let (Some(snapshot), Some(settlement)) = (frame_snapshot.as_ref(), frame_completion)
            {
                // The renderer ran for a captured snapshot: complete through the shared adapter, which
                // stamps the dispatch clock and writes both clocks, pane generations and the surface probe.
                self.complete_window_redraw(win_id, snapshot, settlement);
            }
            let Some(child) = self.windows.get_mut(&win_id) else {
                // When: completion found `win_id` gone, no child remains to anchor IME or a drag bar.
                return;
            };
            // Borrowed again after completion; the frame's earlier borrow of the search ended there.
            let search =
                child.tab_states.get(tab_idx).and_then(|tab_state| tab_state.search.as_ref());
            if let Some(timing) = timing.as_mut() {
                timing.lap("render");
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
                        event_loop.exit();
                        return;
                    }
                };
                let presented = self
                    .runtime_smoke
                    .as_mut()
                    .is_some_and(|smoke| smoke.observe_adopted_present(win_id, count));
                if presented {
                    // When: `presented` is true, release the adopted child after dropping every frame borrow.
                    // The frame's parser guards were released by the render call.
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
                        event_loop.exit();
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
            // The cursor cell is the copy taken before the render call, the cursor the frame drew.
            {
                let (cur_row, cur_col) = cursor_copy;
                if let (Some(win), Some(renderer)) =
                    (child.window.as_ref(), child.renderer.as_ref())
                {
                    let anchor = super::overlays::field_ime_anchor(
                        renderer,
                        palette_here.then_some(&self.command_palette),
                        search,
                        child.ime.preedit(),
                    );
                    if let super::overlays::FieldImeAnchor::Field(caret) = anchor {
                        // A field owns IME and its caret was presented, so the candidate window follows it.
                        child.ime_cursor_throttle.reset();
                        let (pos, size) = super::overlays::field_ime_area(caret);
                        win.set_ime_cursor_area(pos, size);
                    } else if anchor == super::overlays::FieldImeAnchor::Pending {
                        // When: `anchor` is Pending, a field owns IME but its caret is unpresented; wait, never fall back to the terminal.
                        child.ime_cursor_throttle.reset();
                    } else {
                        // When: neither `palette_here` nor `search` owns input, publish the active terminal pane's IME anchor.
                        if let Some([origin_x, origin_y]) = renderer.pane_grid_origin(active_id) {
                            // Child IME follows the planned text origin rather than raw pane padding.
                            let pane = cursor_rect_copy;
                            let rect =
                                sonicterm_ui::pane::Rect::new(origin_x, origin_y, pane.w, pane.h);
                            super::update_terminal_ime_cursor_area(
                                &mut child.ime_cursor_throttle,
                                (active_id, rect),
                                (cur_row, cur_col),
                                (renderer.cell_w, renderer.cell_h),
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
                let inner_origin =
                    win.inner_position().map(|position| (position.x, position.y)).unwrap_or((0, 0));
                let isz = win.inner_size();
                let inner_size = (isz.width, isz.height);
                let raster_w = inner_size.0 as f32;
                let Some(renderer) = child.renderer.as_ref() else {
                    // When: this child has no `renderer`, so tab-bar
                    // height and visibility are unknown.
                    return;
                };
                let layout = TabBarLayout::compute_with_height(
                    &child.tabs,
                    raster_w,
                    renderer.tab_bar_logical_height(),
                )
                .with_top_offset(renderer.tab_bar_y_offset())
                .with_visible(renderer.tab_bar_visible());
                let snap = crate::app::os_drag::TabBarSnapshot::from_layout(
                    Some(win_id),
                    inner_origin,
                    inner_size,
                    &layout,
                );
                self.os_drag_bars.publish(snap);
            }
            if let Some(timing) = timing {
                timing.finish();
            }
            if scrollbar_needs_more_frames {
                child.request_window_redraw();
            }
        }
    }
}
