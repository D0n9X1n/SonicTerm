//! Assemble responsibilities of the renderer, preserving its single owner.

use super::*;

impl GpuRenderer {
    // Same borrow shape as `render`, whose rationale covers this suppression too.
    /// Plan and assemble one frame from the lent panes, deciding the typed exits first in their
    /// existing order. The result borrows no grid. Fallible steps use `?`; `render_releasing` turns
    /// an error into `PresentOutcome::Failed`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_frame(
        &mut self,
        subpixel_aa: SubpixelAaMode,
        panes: &mut [sonicterm_render_model::PaneRender<'_>],
        theme: &Theme,
        cursor_visible: bool,
        selection: Option<&Selection>,
        copy_mode: Option<&CopyModeState>,
        tabs: &TabBar,
        process_privileged: bool,
        search: Option<&SearchState>,
        palette: Option<&mut CommandPalette>,
        ime: Option<&ImeState>,
        viewport_top_abs: Option<u64>,
        notification: Option<&NotificationBubble>,
        hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
        link_preview: Option<&sonicterm_render_model::inputs::LinkPreview>,
        force_full: bool,
    ) -> Result<Assembled> {
        // `lend_and_assemble` has already taken the empty and stopped exits, so `panes` is not empty.
        let mut gpu_timing = tracing::enabled!(target: "render_timing", tracing::Level::DEBUG)
            .then(|| {
                let now = Instant::now();
                (now, now, Vec::<(&'static str, f32)>::with_capacity(12))
            });
        macro_rules! gpu_lap {
            ($name:literal) => {
                lap(&mut gpu_timing, $name)
            };
        }
        let now = Instant::now();
        let broadcast_participant_ids: Vec<u64> =
            panes.iter().filter(|pane| pane.is_broadcast_participant).map(|pane| pane.id).collect();
        // Kitty placements share a pixel buffer and `id`; each buffer is counted once.
        let retained_inline_media_bytes = {
            let mut seen = std::collections::HashSet::new();
            panes
                .iter()
                .flat_map(|pane| &pane.inline_images)
                .filter(|image| seen.insert(image.id))
                .fold(0usize, |total, image| total.saturating_add(image.bgra.len()))
        };
        self.retained_inline_media_bytes = retained_inline_media_bytes;
        let inline_media_hash = {
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            for pane in panes.iter() {
                pane.id.hash(&mut hash);
                pane.inline_images.len().hash(&mut hash);
                for image in &pane.inline_images {
                    (image.id, image.row, image.col, image.width, image.height).hash(&mut hash);
                    // A placement-only change (new box, deleted placement) must not read as an unchanged frame.
                    image.kitty.hash(&mut hash);
                }
            }
            hash.finish()
        };

        // Advance the atlas frame counter so LRU eviction can
        // distinguish glyphs touched this frame from cold ones. Cheap
        // (one integer increment) and unconditional — even on a fully
        // cached frame the bump is harmless and keeps the counter in
        // step with wall-clock frames for diagnostic dumps.
        self.glyph_atlas.tick_frame();
        let atlas_stamp_at_frame_start = self.glyph_atlas_stamp();
        let atlas_evictions_at_frame_start = self.glyph_atlas.evictions();
        // Build a fingerprint of every input that can affect the rendered
        // pixels. If it matches the last frame, nothing on screen would
        // change — skip text shaping, quad rebuild and GPU submit.
        // Highlights are drawn on the active pane from the same view top that
        // `FramePlan::build` resolves, so the search identity hashes that slice.
        let search_hash = search
            .map(|s| {
                let active = panes.iter().find(|pane| pane.is_active).unwrap_or(&panes[0]);
                let live_top = active.grid.scrollback_len() as u64;
                let view_top = viewport_top_abs.unwrap_or(live_top).min(live_top);
                s.presentation_hash(view_top, active.grid.rows)
            })
            .unwrap_or(0);
        // Per-component dirty flag for the command palette so that a
        // keystroke into the query box (which changes neither the grid
        // revision nor the active tab) still invalidates the cached frame.
        let palette_hash: u64 = palette
            .as_deref()
            .filter(|p| p.is_open())
            .map(|p| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                // The open-bit is implicit in the filter above; mark with
                // a salt so closed→empty-query opens differ from a stale
                // hash.
                0xC0DE_FA17_u64.hash(&mut h);
                p.query().hash(&mut h);
                p.cursor().hash(&mut h);
                p.selected().hash(&mut h);
                p.len().hash(&mut h);
                p.scroll_offset().hash(&mut h);
                p.presentation_hash().hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        // Likewise for IME preedit — composition changes don't bump grid
        // revision until commit.
        let ime_hash: u64 = ime
            .map(|i| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                i.preedit().hash(&mut h);
                i.is_composing().hash(&mut h);
                // Caret movement changes cursor placement even when the preedit text is unchanged.
                i.cursor().hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        let notification_hash: u64 = notification
            .map(|n| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                n.level.hash(&mut h);
                n.message.hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        // Include every tab's title, order, activity, color, command status, and
        // foreground privilege so inactive-tab changes cannot leave stale chrome.
        // Badges are judged at the instant the tab widths were measured, so the drawn text, the
        // stored widths and this hash all describe one moment.
        let tab_now = tabs.content_measured_at().unwrap_or(now);
        let tab_hash = tab_bar_hash(tabs, tab_now);
        let broadcast_participants_hash: u64 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            broadcast_participant_ids.hash(&mut h);
            h.finish()
        };
        // Blink phase stays outside the key so idle frames do not trigger full text assembly.
        // Compute hover state against the tab bar layout. Done before
        // the FrameKey is built so the cache invalidates as the cursor
        // moves between tabs.
        let hover_tab_idx = self.hovered_tab_index(tabs, self.hover_cursor);
        let quick_select_hint_count = copy_mode
            .and_then(|state| state.quick_select.as_ref())
            .map_or(0, |quick| quick.hints.len() as u32);
        let read_only_mode = copy_mode.is_some_and(CopyModeState::is_read_only);
        let pane_focus_flash_bucket = self.pane_focus_flash_bucket(now);
        let overlay_active = search.is_some()
            || palette.as_deref().is_some_and(CommandPalette::is_open)
            || notification.is_some()
            || link_preview.is_some()
            || ime.is_some_and(|i| i.is_composing() || !i.preedit().is_empty())
            || self.drag_chip.is_some()
            || self.pane_focus_flash.is_some();
        let renderer_hash = {
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            self.font_family.hash(&mut hash);
            self.font_size.to_bits().hash(&mut hash);
            self.scale_factor.to_bits().hash(&mut hash);
            self.font_weight_scale.to_bits().hash(&mut hash);
            self.line_height_mult.to_bits().hash(&mut hash);
            self.tab_bar_visible.hash(&mut hash);
            self.titlebar_inset.to_bits().hash(&mut hash);
            self.panel_padding.to_bits().hash(&mut hash);
            if let Some(preview) = link_preview {
                preview.uri.hash(&mut hash);
                preview.pointer.0.to_bits().hash(&mut hash);
                preview.pointer.1.to_bits().hash(&mut hash);
                preview.available.hash(&mut hash);
            }
            if let Some(chip) = &self.drag_chip {
                chip.title.hash(&mut hash);
                chip.top_left.0.to_bits().hash(&mut hash);
                chip.top_left.1.to_bits().hash(&mut hash);
                chip.scale.to_bits().hash(&mut hash);
                chip.drop_line_x.map(f32::to_bits).hash(&mut hash);
                chip.drop_line_y.0.to_bits().hash(&mut hash);
                chip.drop_line_y.1.to_bits().hash(&mut hash);
                chip.insertion_slot.hash(&mut hash);
                chip.source_tab_idx.hash(&mut hash);
                chip.source_alpha.to_bits().hash(&mut hash);
                chip.ghost_alpha.to_bits().hash(&mut hash);
            }
            hash.finish()
        };
        // The active pane and view the cursor is drawn from, resolved as `FramePlan::build` does.
        let cursor_pane = panes.iter().find(|pane| pane.is_active).unwrap_or(&panes[0]);
        let cursor_live_top = cursor_pane.grid.scrollback_len() as u64;
        let cursor_view_top = viewport_top_abs.unwrap_or(cursor_live_top).min(cursor_live_top);
        // Records are read only when a partial plan is possible: never on the degraded path, on a
        // first frame, or on the forced-Full pass of a fallback.
        let partial_possible =
            !force_full && !self.software_render_degrade && self.last_frame_key.is_some();
        let row_ink = &self.row_ink;
        let mut plan = FramePlan::build(
            FrameFacts {
                window: WindowIdentity {
                    selection: selection.copied(),
                    copy_mode: copy_mode.map(CopyModeIdentity::from),
                    quick_select_hint_count,
                    cursor_visible,
                    tab: tabs.active().map_or(0, |tab| tab.id.0),
                    search_hash,
                    palette_hash,
                    ime_hash,
                    notification_hash,
                    width: self.config.width,
                    height: self.config.height,
                    tab_hash,
                    status_hash: if self.status_line_visible {
                        self.status_line_view.pixel_hash
                    } else {
                        0
                    },
                    viewport_top_abs,
                    cursor_shape: self.cursor_shape as u8,
                    cursor_blink: self.cursor_blink,
                    window_focused: self.window_focused,
                    pane_focus_flash_bucket,
                    hover_tab: hover_tab_idx,
                    close_override: u8::from(self.tab_close_override.is_some()),
                    broadcast_participants_hash,
                    inline_media_hash,
                    hovered_url_cells,
                    process_privileged,
                    subpixel_aa,
                    background: [
                        self.bg.r.to_bits(),
                        self.bg.g.to_bits(),
                        self.bg.b.to_bits(),
                        self.bg.a.to_bits(),
                    ],
                    style_rev: self.style_rev,
                    renderer_hash,
                    overlay_active,
                    cursor_cell: drawn_cursor_cell(
                        &*cursor_pane.grid,
                        cursor_pane.id,
                        cursor_view_top,
                        cursor_visible,
                        self.window_focused,
                        read_only_mode,
                    ),
                },
                cell_w: self.cell_w,
                cell_h: self.cell_h,
                padding: [
                    self.padding_left_px(),
                    self.padding_right_px(),
                    self.padding_top_px(),
                    self.padding_bottom_px(),
                ],
                vertical_ink_pad: terminal_vertical_ink_pad(
                    self.cell_h,
                    self.font_stack.as_ref().and_then(|stack| stack.cell_metrics_raster_px().ok()),
                ),
                scrollbar_mode: self.scrollbar_mode,
                degraded: self.software_render_degrade,
                tab_bar_top: self.tab_bar_visible.then(|| self.tab_bar_y_offset()),
                status_band: self
                    .status_line_visible
                    .then(|| (self.status_line_top(), self.status_line_height())),
                scale: self.scale_factor,
                previous_recolor: self.last_recolor,
            },
            panes.iter().map(|pane| PaneMetadata {
                id: pane.id,
                revision: pane.grid.revision(),
                dirty_generation: pane.grid.dirty_generation(),
                rect: pane.rect_px,
                cols: pane.grid.cols,
                rows: pane.grid.rows,
                scrollback_len: pane.grid.scrollback_len() as u64,
                viewport_top_abs: pane.viewport_top_abs,
                is_active: pane.is_active,
                is_alt: pane.grid.is_alt(),
                scrollbar_alpha: pane.scrollbar_alpha,
                dirty_rows: pane.grid.dirty_rows().collect(),
                row_ink: if partial_possible {
                    row_ink.valid_records(
                        pane.id,
                        pane.grid,
                        crate::frame_plan::resolved_view_top(
                            pane.viewport_top_abs,
                            pane.grid.scrollback_len() as u64,
                        ),
                        pane.grid.rows,
                    )
                } else {
                    // When: `partial_possible` is false, the plan cannot be partial and needs no record.
                    Vec::new()
                },
            }),
            self.last_frame_key.as_ref(),
        );
        self.last_emit_origins =
            plan.panes.iter().map(|pane| (pane.id, [pane.layout.x, pane.layout.y])).collect();
        self.last_pane_layout = plan
            .panes
            .iter()
            .map(|pane| PaneLayoutSnapshot {
                id: pane.id,
                origin_x_logical: pane.layout.x,
                origin_y_logical: pane.layout.y,
                w_logical: pane.layout.w,
                h_logical: pane.layout.h,
                cell_w_logical: self.cell_w,
                cell_h_logical: self.cell_h,
                cols: pane.cols,
                rows: pane.row_count,
            })
            .collect();
        if plan.unchanged {
            // When: `plan.unchanged` holds, retain the no-assembly fast path; the reblit runs after release.
            return Ok(Assembled::Unchanged { focus_flash: pane_focus_flash_bucket != 0 });
        }
        if plan.mode == RenderMode::Noop {
            // When: `plan.mode` is Noop, remember its identity without acknowledging unpresented grid dirt.
            return Ok(Assembled::Noop(Box::new(plan.key)));
        }
        if force_full {
            plan.force_full();
        }
        // Every exit after the unchanged and no-op ones uses the renderer's frame scratch: the
        // lease restores it on any return, and a drawable frame hands it to presentation.
        let mut scratch_lease = self.frame_scratch.lease();
        let scratch = scratch_lease.get();
        let inline_media_changed = self.last_frame_key.as_ref().is_none_or(|previous| {
            previous.window.inline_media_hash != plan.key.window.inline_media_hash
        });
        // The cursor recolors this frame performs, accumulated across the copy-mode and block sites.
        let mut frame_recolor = crate::cursor::RecolorRecord::default();
        scratch.pane_rects.extend(plan.panes.iter().map(|pane| {
            let rect = pane.full_rect;
            (pane.id, PaneRect::new(rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32))
        }));
        let pane_views: Vec<_> = panes
            .iter()
            .zip(&plan.panes)
            .map(|(pane, planned)| AssemblyPaneView {
                grid: &*pane.grid,
                planned,
                pane_id: planned.id,
                origin_x: planned.layout.x,
                origin_y: planned.layout.y,
                rect_w: planned.layout.w,
                rect_h: planned.layout.h,
                scrollbar_alpha: planned.scrollbar_alpha,
                inline_images: &pane.inline_images,
            })
            .collect();
        let active = &plan.panes[plan.active_index];
        let active_origin_x = active.layout.x;
        let active_origin_y = active.layout.y;
        let active_pane_x = active_origin_x;
        let active_pane_y = active_origin_y;
        let active_pane_w = active.layout.w;
        let active_pane_h = active.layout.h;
        let grid = pane_views[plan.active_index].grid;
        gpu_lap!("frame_key");
        // CPU frame assembly runs from here to the overlays lap: one clock pair per assembled frame.
        let assembly_started = crate::frame_stats::assembly_clock();
        // Note: do NOT cache key here. If prepare()/get_current_texture()
        // fails on a transient surface state we'd cache a key for a frame
        // that never actually got drawn, and the next redraw could
        // early-exit silently. Cache only AFTER successful submit+present.

        // -------- Walk the grid once, emit one glyph instance per
        // visible cell, and send each atlas miss to its rasterizer: the
        // font stack, or the block-glyph path for characters `BlockKey`
        // recognizes. No per-row cache, no rich-text buffer, no glyphon
        // shape pass for the terminal grid.
        let fg_default = self.fg_default;
        // Underline runs collected per pane. We record
        // (origin_x, origin_y, pane_cols, row, col_a, col_b) where
        // origin_{x,y} is the PANE's origin (pad / top_inset) and
        // `pane_cols` is the originating pane's column count, captured at
        // insert time, and each entry carries its own `origin_{x,y}`.
        // Both are needed because the emit loop draws underlines from every
        // pane: without the origin an inactive pane's underlines land under
        // the active pane's coordinates, and without `pane_cols` the
        // per-origin snapped-edge cache is sized from the active pane, so a
        // wider inactive pane has its underlines clamped and truncated.
        // Per entry of `underlines`, the staging index of the row that pushed it, so each
        // underline's quads merge into that row's one ink record.
        // Per drawn pane, the staging indices its glyph rows took, in ascending slot order, so
        // the background loop merges each row's quads into the record the glyph loop staged.
        // Overlay glyph instances — palette text + (future) other modals.
        // Kept separate so they can be drawn AFTER `quad_overlay` paints
        // the modal backdrop, otherwise they'd be hidden by their own
        // background. (— palette text was previously routed through
        // glyphon's TextRenderer which bypassed the device-scale atlas
        // path used by `emit_tab_title_glyphs`, hence the HiDPI blur.)
        // Each emitted terminal row's glyph range in `glyph_instances` and its ink bounds, so
        // highlight recolors on the main list scan only rows whose ink meets the target.
        // Missing-glyph "tofu" outlines collected during the cell walk.
        // Drawn via the quad pipeline after the text instances.
        // Mirror of missing_tofu, recording just the codepoint so tests
        // can assert "no class regressed" without depending on pixel
        // layout. Cleared every frame; published into `self.last_missing_chars`
        // before render() returns.
        let mut missing_chars_this_frame: Vec<char> = Vec::new();
        // An earlier pass that did not present left its staged records; they are not on screen.
        self.staged_missing.clear();
        // The chrome counterpart: every chrome layout below notes its tofu into this scope, which
        // closes with the assembled frame; a frame that fails to assemble discards it on drop.
        let missing_chrome_scope = chrome_text::MissingChromeScope::enter();
        // Geometry is in raster pixels, so px_to_ndc uses the unscaled physical surface dimensions.
        let sw = self.config.width as f32;
        let sh = self.config.height as f32;
        // Each PaneView supplies its own origin instead of the window-level padding or inset.
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        // Baseline offset inside the cell box. Font-stack tiles carry
        // a vertical offset relative to the baseline; we want screen-y
        // relative to the cell top. Using ≈80% of cell height matches
        // a reasonable ascent for monospace fonts at the configured
        // line-height; finer baseline control would require querying
        // font metrics.
        let baseline_y_in_cell = self.baseline_y_in_cell();
        let software_presenter = self.uses_windows_software_presenter();

        let raster_px = self.raster_px(self.font_size);
        let geometry = AssemblyGeometry {
            fg_default,
            pane_views: &pane_views,
            grid,
            plan: &plan,
            active_origin_x,
            active_origin_y,
            active_pane_x,
            active_pane_y,
            active_pane_width_px: active_pane_w,
            active_pane_height_px: active_pane_h,
            cell_width_px: cell_w,
            cell_height_px: cell_h,
            baseline_y_in_cell,
            raster_px,
            software_presenter,
            surface_width_px: sw,
            surface_height_px: sh,
        };
        let inputs = AssemblyUi {
            theme,
            cursor_visible,
            selection,
            copy_mode,
            tabs,
            process_privileged,
            search,
            ime,
            notification,
            hovered_url_cells,
            link_preview,
            read_only_mode,
            quick_select_hint_count,
            hover_tab_idx,
            tab_now,
        };
        self.assemble_glyph_rows(geometry, inputs, scratch, &mut missing_chars_this_frame);
        if std::mem::take(&mut self.fault_assembly_error) {
            // When: `fault_assembly_error` is armed, assembly fails as a real `Err` would, after
            // its glyph rows were staged, so the error arm must discard them.
            return Err(anyhow!("injected assembly failure"));
        }

        // Overlay quads — drawn AFTER terminal text + main quads so that
        // palette / search-input / IME backgrounds visually cover the
        // terminal content underneath. Emitted into the same vector as the
        // main quads, terminal glyphs bleed through overlay dialogs.

        self.assemble_inline_images(
            geometry,
            scratch,
            retained_inline_media_bytes,
            inline_media_changed,
        );

        // build the active pane's shared device-pixel-snapped
        // column-edge cache once per frame, hoisted above every overlay
        // path. Every overlay anchored to the active pane (selection,
        // cursor, copy-mode, quick-select, hyperlink, search-highlight,
        // underline-decoration, IME preedit) reads its x edges from
        // this cache so it stays edge-aligned with adjacent glyph cells
        // at fractional DPI. Integer scales (1.0/2.0) are an identity
        // fast path inside `snap_to_device_pixels`, so mac dHash
        // baselines stay green by construction. Per diagnosis,
        // per-pane bg fill builds its OWN cache (see the per-pane bg
        // loop below) — it MUST NOT share the active pane's cache.
        let active_snapped_cell_x: Vec<f32> =
            build_snapped_cell_x(active_origin_x, self.cell_w, grid.cols);

        self.assemble_backgrounds(geometry, inputs, scratch, now);

        self.assemble_selection(
            geometry,
            inputs,
            scratch,
            &active_snapped_cell_x,
            &mut frame_recolor,
        );

        // OSC 8 hyperlinks are semantic, not visual. Do not tint/underline
        // every hyperlink cell permanently: prompts such as Oh My Posh wrap
        // the path segment in a `file:` hyperlink, and a permanent overlay
        // changes the segment's configured truecolor background compared with
        // Windows Terminal. Link affordance is drawn below only for the
        // currently hovered URL span.

        gpu_lap!("grid_walk");
        self.assemble_decorations(geometry, inputs, scratch, &broadcast_participant_ids);
        self.assemble_status_line(geometry, inputs, scratch);
        let tab_ink = self.assemble_tabs(geometry, inputs, scratch);
        let AssemblyFields {
            read_only_badge,
            search_bar_layout,
            field_environment,
            mut field_candidates,
            field_selection_bg,
            field_selection_fg,
            search_ime_anchor,
        } = self.assemble_search(geometry, inputs, scratch, &active_snapped_cell_x);

        self.assemble_messages(geometry, inputs, scratch, read_only_badge, search_bar_layout);

        let palette_layout = self.assemble_palette(
            geometry,
            inputs,
            scratch,
            palette,
            field_environment,
            &mut field_candidates,
            field_selection_bg,
            field_selection_fg,
        );

        self.assemble_preedit(
            geometry,
            inputs,
            scratch,
            &active_snapped_cell_x,
            search_ime_anchor,
            &palette_layout,
        );

        self.assemble_transients(geometry, inputs, scratch);

        gpu_lap!("overlays");
        crate::frame_stats::note_assembly(assembly_started);

        if std::mem::take(&mut self.fault_atlas_change_during_assembly) {
            // The test seam stands in for an atlas reset during assembly: only its identity moves.
            self.glyph_atlas_generation = self.glyph_atlas_generation.wrapping_add(1);
        }
        let pass_end = PassEnd {
            atlas_stamp_at_start: atlas_stamp_at_frame_start,
            atlas_stamp_now: self.glyph_atlas_stamp(),
            atlas_evictions_at_start: atlas_evictions_at_frame_start,
            previous_recolor: self.last_recolor,
            current_recolor: frame_recolor,
            previous_tab_ink: self.last_tab_ink,
            current_tab_ink: tab_ink,
        };
        let receipts = match Self::finish_assembly_pass(&mut plan, panes, pass_end) {
            Ok(receipts) => receipts,
            Err(early_exit) => {
                // When: the atlas changed or the partial plan reached unemitted ink, the helper
                // decided the exit: `AtlasRetry` or `PartialFallback`.
                return Ok(early_exit);
            }
        };

        #[cfg(debug_assertions)]
        {
            crate::quad::debug_assert_premultiplied_quads("base", &scratch.quads);
            crate::quad::debug_assert_premultiplied_quads("overlay", &scratch.overlay_quads);
        }

        // The pass assembled a drawable frame, so the restore trims by its use; the lease then
        // travels inside the layers, so no exit up to the end of presentation loses the scratch.
        scratch_lease.complete();
        Ok(Assembled::Layers(Box::new(AssembledLayers {
            surface_width: sw,
            surface_height: sh,
            subpixel_aa,
            scratch: scratch_lease,
            field_candidates,
            missing_chars: missing_chars_this_frame,
            missing_chrome_chars: missing_chrome_scope.finish(),
            gpu_timing,
            plan,
            receipts,
            recolor: frame_recolor,
            tab_ink,
        })))
    }
}

#[cfg(test)]
#[path = "core_assemble_tests.rs"]
mod core_assemble_tests;
