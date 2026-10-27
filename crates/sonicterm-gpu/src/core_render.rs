//! Render responsibilities of the renderer, preserving its single owner.

use super::*;

impl GpuRenderer {
    // `render` threads borrowed app state plus one copyable process flag through
    // wgpu submission. A parameter struct would still need separate
    // borrow fields (no win over positional args) or force the App layer
    // to construct an interior-mutable wrapper around its own state —
    // both worse than the current shape. Keep the suppression beside this
    // borrow-shape rationale.
    /// Render one frame: terminal grid + cursor + selection + overlays
    /// (tab bar, search, command palette, IME preedit). Submits to the
    /// wgpu queue and presents the surface. See the parameter comments
    /// above for the lifetime / borrow rationale.
    ///
    /// This compatibility entry point runs [`Self::render_with_outcome`] and maps
    /// its outcome through [`PresentOutcome::into_render_result`], restoring the native
    /// Timeout/Occluded retry for callers without typed scheduling. A frame that presents
    /// nothing is `Ok(())` and a failure keeps its error. Once the renderer's
    /// device stops accepting work, the first call returns an error and later
    /// calls return `Ok(())` without doing any work.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
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
        // Cmd-hovered auto-detected URL cell range (viewport coords),
        // or `None` when no URL is hovered while the open-URL modifier
        // is held. When set on the active pane, the URL's glyphs are
        // recolored with the theme accent (companion to the existing
        // hover underline). Same lifetime/gating as the underline.
        hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
        link_preview: Option<&sonicterm_render_model::inputs::LinkPreview>,
    ) -> Result<()> {
        let fonts = self.begin_frame_fonts();
        let outcome = self.render_with_outcome(
            &fonts,
            panes,
            theme,
            cursor_visible,
            selection,
            copy_mode,
            tabs,
            process_privileged,
            search,
            palette,
            ime,
            viewport_top_abs,
            notification,
            hovered_url_cells,
            link_preview,
        );
        if outcome.requires_legacy_redraw() {
            self.request_window_redraw();
        }
        outcome.into_render_result()
    }
}

impl GpuRenderer {
    // Same borrow shape as `render`, whose rationale covers this suppression too.
    /// Render one frame, taking the same arguments as [`Self::render`],
    /// and report what happened to it as a [`PresentOutcome`].
    ///
    /// A compatibility wrapper over [`Self::render_releasing`] with a [`BorrowedSource`]: the caller
    /// keeps its grids, so on [`PresentOutcome::Presented`] the frame's receipts are applied to them
    /// here. A skip, a cached reblit, an atlas or surface retry, a stopped device, and a failure all
    /// keep the dirty rows. It opens no counter scope of its own; `render_releasing` opens one.
    ///
    /// [`BorrowedSource`]: sonicterm_render_model::BorrowedSource
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_outcome(
        &mut self,
        fonts: &FrameFonts,
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
    ) -> PresentOutcome {
        let frame = self.render_releasing(
            fonts,
            sonicterm_render_model::BorrowedSource(&mut *panes),
            theme,
            cursor_visible,
            selection,
            copy_mode,
            tabs,
            process_privileged,
            search,
            palette,
            ime,
            viewport_top_abs,
            notification,
            hovered_url_cells,
            link_preview,
        );
        settle_borrowed_frame(frame, panes)
    }
}

impl GpuRenderer {
    // Same borrow shape as `render`, whose rationale covers this suppression too.
    /// Render one frame from `source` and report how it ended, with a metadata receipt per pane a
    /// presented frame acknowledges.
    ///
    /// Assembly runs inside `source.lend`; when it returns the source is dropped, so an owning source
    /// releases every parser guard before the surface is acquired, the retained frame is blitted, or
    /// the frame is submitted and presented. Nothing is acknowledged here: the caller applies the
    /// receipts. Assembly and presentation share one `&mut self` borrow, so no resize, font change,
    /// atlas reset or rebind can come between them.
    #[allow(clippy::too_many_arguments)]
    pub fn render_releasing(
        &mut self,
        fonts: &FrameFonts,
        source: impl sonicterm_render_model::FrameSource,
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
    ) -> FrameOutcome {
        let _scope = crate::frame_stats::RenderScope::enter(
            self.frame_sink.as_ref(),
            &mut self.unattributed_apply,
        );
        self.debug_assert_prepared(fonts);
        let frame_start = Instant::now();
        // Read before lending, so assembly itself never reaches the device.
        let subpixel_aa = self.effective_subpixel_aa_mode();
        let accepts_gpu_work = self.device_errors.accepts_gpu_work();
        let assembled = lend_and_assemble(source, accepts_gpu_work, |panes| {
            let mut palette = palette;
            assemble_with_fallback(|force_full| {
                self.assemble_frame(
                    subpixel_aa,
                    panes,
                    theme,
                    cursor_visible,
                    selection,
                    copy_mode,
                    tabs,
                    process_privileged,
                    search,
                    palette.as_deref_mut(),
                    ime,
                    viewport_top_abs,
                    notification,
                    hovered_url_cells,
                    link_preview,
                    force_full,
                )
            })
        });
        // The source is gone here: every arm below runs with no parser guard held.
        self.flush_image_upload_rebuild();
        // Any growth, from this assembly, outside it, or left by a reset retry, resizes the texture
        // here, after release and before any present, so a grown atlas never syncs into a smaller
        // texture and recreating one never holds the parser guards. Assembly reads no texture.
        self.rebuild_glyph_upload_if_needed();
        self.count_glyph_atlas_growths(frame_start);
        self.finalize_growth_episodes_if_device_stopped();
        let assembled = match assembled {
            Ok(assembled) => assembled,
            Err(error) => {
                // When: assembly returned `error`, nothing was drawn; report it as failed, with no
                // receipts, and forget the glyph slot keys its passes staged.
                self.row_glyph_cache.discard_staged();
                return FrameOutcome::without_receipts(PresentOutcome::Failed(error));
            }
        };
        let assembled = match settle_without_renderer(assembled) {
            Ok(outcome) => {
                // When: `settle_without_renderer` settled an empty source, return it with no receipts.
                return outcome;
            }
            Err(assembled) => assembled,
        };
        let outcome = match assembled {
            // Settled above; kept so the match names every exit.
            Assembled::NoPanes => PresentOutcome::Skipped(SkipReason::NoPanes),
            Assembled::Unavailable => self.rendering_unavailable(),
            Assembled::PartialFallback => {
                // Only a forced-Full pass reaches here, and force_full makes a fallback impossible;
                // present nothing and plan the next frame from scratch rather than trust the key.
                self.last_frame_key = None;
                self.row_glyph_cache.discard_staged();
                self.request_window_redraw();
                PresentOutcome::Skipped(SkipReason::Noop)
            }
            Assembled::Unchanged { focus_flash } => {
                // When: `Unchanged`, retain the no-assembly fast path and the Windows cached-frame reblit.
                self.skipped_frames = self.skipped_frames.wrapping_add(1);
                tracing::trace!(skipped = self.skipped_frames, "renderer: skipped unchanged frame");
                let outcome = if let Some(before) = self.prepare_cached_present() {
                    // When: `before` describes a retained CPU frame, admit its reblit at this render boundary.
                    let Some(reblit_scope) = self.device_errors.enter_gpu_work("render.reblit")
                    else {
                        // When: `enter_gpu_work` refuses, keep the stopped exit before the focus-flash redraw.
                        return FrameOutcome::without_receipts(self.rendering_unavailable());
                    };
                    self.present_unchanged_frame(before, reblit_scope)
                        .unwrap_or_else(PresentOutcome::Failed)
                } else {
                    // When: `before` is absent, no cached presenter is available for this unchanged plan.
                    PresentOutcome::Skipped(SkipReason::Unchanged)
                };
                if focus_flash
                    && !matches!(
                        outcome,
                        PresentOutcome::RenderingUnavailable(_) | PresentOutcome::Failed(_)
                    )
                {
                    self.request_window_redraw();
                }
                outcome
            }
            Assembled::Noop(key) => {
                // Nothing drawable changed: remember the key without acknowledging any dirt.
                self.last_frame_key = Some(*key);
                PresentOutcome::Skipped(SkipReason::Noop)
            }
            Assembled::AtlasRetry { stamp, evictions } => {
                // The atlas changed during assembly, so its UVs are stale; each path requests a redraw.
                let after = self.glyph_atlas_stamp();
                if growth_only_change(stamp, after, evictions, self.glyph_atlas.evictions()) {
                    self.retry_after_glyph_atlas_growth();
                } else {
                    // When: growth_only_change is false the atlas was evicted, reset or replaced,
                    // so the frame takes the reset path with eviction disabled for one retry.
                    self.reset_glyph_atlas_after_invalidation(stamp, evictions);
                }
                let _discarded = settle_retained_frame(
                    &mut self.last_frame_key,
                    &mut self.row_ink,
                    &mut self.row_glyph_cache,
                    &PresentOutcome::AtlasRetry,
                    None,
                    Vec::new(),
                );
                PresentOutcome::AtlasRetry
            }
            Assembled::Layers(layers) => {
                // When: `Layers` carries owned batches, present them; only a presented frame returns
                // receipts. A presenter `Err` skips settlement, so it discards staged slot keys here.
                return self.present_layers(*layers).unwrap_or_else(|error| {
                    self.row_glyph_cache.discard_staged();
                    FrameOutcome::without_receipts(PresentOutcome::Failed(error))
                });
            }
        };
        FrameOutcome::without_receipts(outcome)
    }
}

impl GpuRenderer {
    /// End one assembly pass with the decisions every pass takes, in this order, so production
    /// and the fallback tests run the same code: an atlas whose content stamp moved since the
    /// pass started has stale UVs, so the pass is `AtlasRetry`; otherwise the plan's damage is
    /// widened by this pass's recolors and tab-title ink against the last presented frame's; a
    /// partial plan whose widened damage reaches a row it did not emit is `PartialFallback`,
    /// since the scissor would erase that row's ink; otherwise the plan's receipts are read under
    /// the same guards the plan was built from. `Err` carries the early exit.
    pub(super) fn finish_assembly_pass(
        plan: &mut FramePlan,
        panes: &[sonicterm_render_model::PaneRender<'_>],
        pass: PassEnd,
    ) -> std::result::Result<Vec<sonicterm_render_model::AckReceipt>, Assembled> {
        if atlas_changed_during_frame(pass.atlas_stamp_at_start, pass.atlas_stamp_now) {
            // When: atlas_changed_during_frame detects stale UVs, discard them after the source is released.
            return Err(Assembled::AtlasRetry {
                stamp: pass.atlas_stamp_at_start,
                evictions: pass.atlas_evictions_at_start,
            });
        }
        plan.widen_for_recolor(pass.previous_recolor, pass.current_recolor);
        plan.widen_for_tab_ink(pass.previous_tab_ink, pass.current_tab_ink);
        if plan.partial_reaches_unemitted_ink() {
            // When: `partial_reaches_unemitted_ink` holds, the scissor would erase a skipped row's ink.
            return Err(Assembled::PartialFallback);
        }
        Ok(presented_receipts(plan, panes))
    }
}

impl GpuRenderer {
    /// Hand assembled batches to the presenter; on `Presented`, finish the frame and return its receipts.
    pub(super) fn present_layers(&mut self, assembled: AssembledLayers) -> Result<FrameOutcome> {
        let AssembledLayers {
            surface_width,
            surface_height,
            subpixel_aa,
            scratch,
            field_candidates,
            missing_chars,
            missing_chrome_chars,
            mut gpu_timing,
            plan,
            receipts,
            recolor,
            tab_ink,
        } = assembled;
        // The lease returns the scratch to the renderer when it drops, on every outcome, including
        // a presenter `Err` and unwinding.
        let batches = scratch.held();
        // The presenter borrows only the owned drawable layers; no grid or parser guard is held.
        let layers = FrameLayers {
            surface_width,
            surface_height,
            first_frame: plan.first_frame,
            damage: plan.damage,
            partial: plan.mode == RenderMode::Partial,
            subpixel_aa,
            batches: FrameBatches {
                quads: &batches.quads,
                images: &batches.images,
                glyphs: &batches.glyphs,
                overlay_quads: &batches.overlay_quads,
                overlay_glyphs: &batches.overlay_glyphs,
            },
        };
        if std::mem::take(&mut self.fault_present_error) {
            // When: `fault_present_error` is armed, the presenter call fails as a real `Err` would,
            // and the frame's staged strip is discarded with it.
            self.status_line_layouts.settle(false);
            return Err(anyhow!("injected presentation failure"));
        }
        let outcome = self.present_frame(&layers, &mut gpu_timing);
        // Only a presented frame's strip is what the user sees and may be routed or read aloud.
        self.status_line_layouts.settle(matches!(outcome, Ok(PresentOutcome::Presented)));
        let outcome = outcome?;
        // Only a presented frame's field geometry is what the user sees and may be hit-tested.
        self.presented_fields
            .settle(field_candidates, matches!(outcome, PresentOutcome::Presented));
        if !matches!(outcome, PresentOutcome::Presented) {
            // When: `matches!` finds any outcome but `Presented`, no receipt is issued, so its dirty rows stay.
            self.finalize_growth_episodes_if_device_stopped();
            let _discarded = settle_retained_frame(
                &mut self.last_frame_key,
                &mut self.row_ink,
                &mut self.row_glyph_cache,
                &outcome,
                None,
                receipts,
            );
            return Ok(FrameOutcome::without_receipts(outcome));
        }
        // Only a presented frame's recolors are on screen, so only it becomes the next baseline.
        self.last_recolor = recolor;
        self.last_tab_ink = tab_ink;
        let receipts = self.finish_successful_frame(
            plan,
            receipts,
            missing_chars,
            missing_chrome_chars,
            gpu_timing,
        );
        Ok(FrameOutcome { outcome: PresentOutcome::Presented, receipts })
    }
}

impl GpuRenderer {
    /// Raise one test fault on this renderer's device.
    ///
    /// Compiled into every build so the release smoke can prove containment.
    #[doc(hidden)]
    pub fn __inject_gpu_fault(&mut self, kind: GpuFaultKind) {
        match kind {
            GpuFaultKind::IsolatedOperation => {
                if let Some(_scope) = self.device_errors.enter_gpu_work("fault.isolated") {
                    // A stopped device refuses the scope, so it takes no isolated fault.
                    let device = &self.device;
                    run_isolated_validation(device, &self.device_errors, || {
                        let _invalid = device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("sonic-isolated-fault"),
                            size: 4,
                            usage: wgpu::BufferUsages::empty(),
                            mapped_at_creation: false,
                        });
                    });
                }
            }
            GpuFaultKind::RetainedResourceCreation => {
                self.fault_invalid_glyph_upload = true;
            }
            GpuFaultKind::FrameValidation => {
                self.fault_frame_probe = self
                    .device_errors
                    .gpu_work("fault.frame_probe", || create_frame_fault_probe(&self.device));
            }
            GpuFaultKind::DestroyDevice => {
                destroy_and_await_loss(&self.device, &self.device_errors);
            }
        }
        // The next frame must take the full path so its outcome is observable.
        self.last_frame_key = None;
        self.request_window_redraw();
    }
}

impl GpuRenderer {
    /// Finish a presented frame: record its statistics and settle its plan, returning the receipts
    /// the settlement keeps.
    pub(super) fn finish_successful_frame(
        &mut self,
        plan: FramePlan,
        receipts: Vec<sonicterm_render_model::AckReceipt>,
        missing_chars_this_frame: Vec<char>,
        missing_chrome_chars: Vec<char>,
        gpu_timing: Option<(Instant, Instant, Vec<(&'static str, f32)>)>,
    ) -> Vec<sonicterm_render_model::AckReceipt> {
        let render_mode = plan.mode;
        let damaged_rows = plan.damaged_rows;
        let (surface_width, surface_height) = (self.config.width, self.config.height);
        crate::frame_stats::note_damage(|| {
            crate::frame_stats::damage_permille(&plan.damage, surface_width, surface_height)
        });
        // The single rectangle's waste over the parts it unions decides whether a rect list pays.
        crate::frame_stats::note_damage_waste(|| {
            crate::frame_stats::damage_waste_permille(
                &plan.damage,
                &plan.damage_parts,
                surface_width,
                surface_height,
            )
        });
        // The frame counts by the presenter `present_frame` used, via the same predicate.
        let software_presenter =
            crate::frame_stats::presents_software(self.software_render_degrade);
        crate::frame_stats::note_frame(software_presenter);
        self.successful_frame_count = self.successful_frame_count.saturating_add(1);
        // The successful-present seam both presenters share closes any pending growth timing.
        self.growth_episodes.present();
        self.finish_glyph_atlas_retry();
        // The certificate reads the presented plan's scene and the atlas as it is at present.
        let atlas_dims = (self.glyph_atlas.width(), self.glyph_atlas.height());
        let stamp = self.glyph_atlas_stamp();
        crate::completeness::record_presented(
            &mut self.completeness,
            crate::completeness::Presented {
                full: render_mode == RenderMode::Full,
                scene: plan.key.scene(),
                stamp,
                atlas_dims,
                missing_terminal: &missing_chars_this_frame,
                missing_chrome: &missing_chrome_chars,
            },
        );
        self.last_missing_chars = missing_chars_this_frame;
        self.last_missing_chrome_chars = missing_chrome_chars;
        crate::missing_glyphs::commit_missing(
            &mut self.presented_missing,
            &mut self.staged_missing,
        );
        self.presented_damage.record(|| PresentedDamage {
            first_frame: plan.first_frame,
            damage: plan.damage,
            surface: PixelRect { x: 0, y: 0, w: surface_width.max(1), h: surface_height.max(1) },
        });
        // The presented rows' records replace theirs; records of undrawn or vanished rows are pruned.
        let receipts = settle_retained_frame(
            &mut self.last_frame_key,
            &mut self.row_ink,
            &mut self.row_glyph_cache,
            &PresentOutcome::Presented,
            Some(plan),
            receipts,
        );
        if !self.presented_missing.is_empty() {
            // The slots just settled, so records of released or replaced rows go now.
            let cache = &self.row_glyph_cache;
            crate::missing_glyphs::prune_missing(
                &mut self.presented_missing,
                |pane_id, slot| cache.committed_slot(pane_id, slot),
                self.applied_fonts,
            );
        }
        if self.pane_focus_flash.is_some() {
            self.request_window_redraw();
        }
        if let Some((start, last, mut parts)) = gpu_timing {
            let now = Instant::now();
            parts.push(("cleanup", now.saturating_duration_since(last).as_secs_f32() * 1000.0));
            let total_ms = now.saturating_duration_since(start).as_secs_f32() * 1000.0;
            let mode = match render_mode {
                RenderMode::Full => "full",
                RenderMode::Partial => "partial",
                RenderMode::Noop => "noop",
            };
            let mut line = format!(
                "[gpu_render_timing] window={} mode={mode} damaged_rows={damaged_rows} total={total_ms:.2}ms",
                self.render_timing_label
            );
            for (name, ms) in parts {
                line.push_str(&format!(" {name}={ms:.2}ms"));
            }
            tracing::debug!(target: "render_timing", %line);
        }
        receipts
    }
}

#[cfg(test)]
#[path = "core_render_tests.rs"]
mod core_render_tests;
