//! Inspect responsibilities of the renderer, preserving its single owner.

use super::*;

impl GpuRenderer {
    /// Return the pane targeted by the live focus flash, for integration diagnostics.
    #[doc(hidden)]
    pub fn __test_pane_focus_flash_target(&self) -> Option<u64> {
        self.pane_focus_flash.map(|(pane_id, _)| pane_id)
    }
}

impl GpuRenderer {
    /// Snapshot of every codepoint the previous `render()` call could
    /// not produce a glyph tile for (i.e. that drew a tofu outline).
    /// Whitespace is filtered out — those are intentionally blank.
    ///
    /// Test-only diagnostic. Production code MUST NOT depend on this
    /// surface — it exists so the renderer-capability matrix can
    /// assert "no character class regressed" without sniffing pixels
    /// off the swapchain. Doc-hidden to keep it out of the public
    /// rustdoc; still `pub` so integration tests under `tests/` can
    /// reach it.
    #[doc(hidden)]
    pub fn last_missing_tofu(&self) -> &[char] {
        &self.last_missing_chars
    }
}

impl GpuRenderer {
    /// Every chrome character (tab titles, palette rows, query, footer, search, preedit) the
    /// previous presented frame drew as tofu or dropped because its run could not be shaped or
    /// its tile was not placed; whitespace is excluded. The chrome counterpart of
    /// [`Self::last_missing_tofu`], so fallback is complete only when both are empty.
    ///
    /// Test-only diagnostic, doc-hidden like `last_missing_tofu`.
    #[doc(hidden)]
    pub fn last_missing_chrome(&self) -> &[char] {
        &self.last_missing_chrome_chars
    }
}

impl GpuRenderer {
    /// The perf-end completeness checkpoint: the distinct missing terminal and chrome characters the
    /// last presented `Full` frame certified, only while the retained scene and the current glyph
    /// atlas still match it; otherwise the reason it is unavailable.
    ///
    /// Test-only diagnostic, doc-hidden like `last_missing_tofu`.
    #[doc(hidden)]
    pub fn completeness_checkpoint(&self) -> crate::completeness::CompletenessCheckpoint {
        let retained_scene = self.last_frame_key.as_ref().map(FrameKey::scene);
        crate::completeness::read_checkpoint(
            self.completeness.as_ref(),
            retained_scene.as_ref(),
            &self.glyph_atlas_stamp(),
            (self.glyph_atlas.width(), self.glyph_atlas.height()),
        )
    }
}

impl GpuRenderer {
    /// Test hook: evict the glyph atlas's coldest quarter at `cap` entries instead of the
    /// production maximum, so a native test can drive a real eviction in the same assembly as a
    /// growth. Passing `sonicterm_text::glyph_atlas::MAX_ATLAS_ENTRIES` restores production.
    #[doc(hidden)]
    pub fn __set_glyph_atlas_entry_cap(&mut self, cap: usize) {
        self.glyph_atlas.__set_entry_cap_for_test(cap);
    }
}

impl GpuRenderer {
    /// Test hook: change the glyph atlas identity during the next assembly, so that frame returns
    /// `AtlasRetry` and presents nothing.
    #[doc(hidden)]
    pub fn __change_glyph_atlas_during_next_assembly(&mut self) {
        self.fault_atlas_change_during_assembly = true;
    }
}

impl GpuRenderer {
    /// Test hook: run `hook` at the start of every presentation, after the frame's source was
    /// released; when it returns true the device is stopped there, as a loss during present would.
    #[doc(hidden)]
    pub fn __set_present_hook(&mut self, hook: Option<Box<dyn FnMut() -> bool + Send>>) {
        self.present_hook = hook;
    }
}

impl GpuRenderer {
    /// Collect frame statistics from now on. The App calls this once, before the renderer draws.
    pub fn set_frame_counting(&mut self, counting: bool) {
        self.frame_sink = counting.then(crate::frame_stats::FrameStatsSink::default);
    }
}

impl GpuRenderer {
    /// Settle this renderer's statistics for a final read: count glyph atlas growths not yet
    /// counted and abandon a pending growth episode. The App calls it before it copies a retiring
    /// or exiting window's statistics; it is idempotent, and `Drop` repeats it as a fallback that
    /// then adds nothing.
    pub fn finalize_frame_stats(&mut self) {
        self.finalize_growth_episodes();
    }
}

impl GpuRenderer {
    /// The cumulative frame statistics; zero while the renderer does not count.
    #[must_use]
    pub fn frame_stats(&self) -> crate::frame_stats::FrameStats {
        self.frame_sink.as_ref().map_or(
            crate::frame_stats::FrameStats::ZERO,
            crate::frame_stats::FrameStatsSink::snapshot,
        )
    }
}

impl GpuRenderer {
    /// Number of frames that completed a native presentation successfully.
    ///
    /// Skipped, occluded, outdated, lost, and failed frames do not advance it.
    #[must_use]
    pub fn successful_frame_count(&self) -> u64 {
        self.successful_frame_count
    }
}

impl GpuRenderer {
    /// Frames handed to a native presenter, counted at the present call.
    ///
    /// Unlike [`Self::successful_frame_count`], it also counts a presentation
    /// that stopped the device, so tests can prove a failed frame never reached
    /// the presenter.
    #[doc(hidden)]
    #[must_use]
    pub fn present_call_count(&self) -> u64 {
        self.present_calls
    }
}

impl GpuRenderer {
    /// Stop this renderer's device just before its next cached Windows CPU
    /// reblit, after `render`'s entry check has passed.
    ///
    /// Test seam for the one device checkpoint no production GPU call reaches:
    /// an unchanged frame issues no GPU work between the entry check and the
    /// reblit, so only another thread could stop the device there.
    #[cfg(target_os = "windows")]
    #[doc(hidden)]
    pub fn __stop_device_before_cached_present(&mut self) {
        self.fault_stop_before_cached_present = true;
    }
}

impl GpuRenderer {
    /// Force one typed backend-occlusion result on the next real wgpu frame, at the acquire step,
    /// without switching macOS Spaces or covering a window. The Windows software presenter has no
    /// surface acquire, so on that presenter the armed fault waits for a wgpu frame.
    #[doc(hidden)]
    pub fn __occlude_next_surface_acquire(&mut self) {
        self.fault_surface_occluded = true;
        self.invalidate_retained_frame();
    }
}

impl GpuRenderer {
    /// The containment state of this renderer's wgpu device, shared by every
    /// renderer built from the same shared context.
    #[must_use]
    pub fn device_error_state(&self) -> &Arc<DeviceErrorState> {
        &self.device_errors
    }
}

impl GpuRenderer {
    /// A point-in-time copy of this renderer's device containment state.
    #[must_use]
    pub fn device_error_snapshot(&self) -> DeviceErrorSnapshot {
        self.device_errors.snapshot()
    }
}

impl GpuRenderer {
    /// Whether this renderer's device still accepts GPU work.
    #[must_use]
    pub fn device_accepts_gpu_work(&self) -> bool {
        self.device_errors.accepts_gpu_work()
    }
}

impl GpuRenderer {
    /// Take the device's one-time stopped-frame report without assembling or presenting a frame.
    pub fn take_stopped_render_outcome(&mut self) -> Option<PresentOutcome> {
        // The App's stopped path refuses rendering before any assembly, so a pending growth
        // episode ends here, even when the stop was already reported.
        self.finalize_growth_episodes_if_device_stopped();
        if self.device_errors.accepts_gpu_work() || self.device_stop_reported {
            // When: `device_errors` accepts work or `device_stop_reported` is set, no stop report remains.
            return None;
        }
        Some(self.rendering_unavailable())
    }
}

impl GpuRenderer {
    /// Process-unique identity of this renderer's device. Renderers that share
    /// a device report the same generation.
    #[must_use]
    pub fn device_generation(&self) -> u64 {
        self.device_errors.generation()
    }
}

impl GpuRenderer {
    /// Return current allocator totals when the selected backend exposes them.
    ///
    /// `None` means allocator reporting is unavailable or the device has
    /// stopped accepting work; it does not represent an allocator with zero
    /// usage.
    #[must_use]
    pub fn allocator_snapshot(&self) -> Option<AllocatorSnapshot> {
        self.device_errors
            .gpu_work("allocator_snapshot", || {
                allocator_snapshot_from_report(self.device.generate_allocator_report())
            })
            .flatten()
    }
}

impl GpuRenderer {
    /// Read one BGRA pixel out of the Windows software presentation buffer.
    ///
    /// Test-only inspector for the software path: it is the only way to assert
    /// what that path actually wrote without a GPU readback. Returns `None`
    /// when no software frame is allocated or the coordinates fall outside it.
    #[cfg(target_os = "windows")]
    #[doc(hidden)]
    pub fn __test_software_frame_pixel_bgra(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        self.software_frame.as_ref()?.pixel_bgra_at(x, y)
    }
}

impl GpuRenderer {
    /// This renderer's requested GPU bytes and resource counts by class. Requests, not
    /// residency: the driver decides what it keeps, and a released resource may stay resident.
    #[must_use]
    pub fn graphics_ledger(&self) -> crate::graphics_ledger::GraphicsLedgerSnapshot {
        self.graphics_ledger.snapshot()
    }
}

impl GpuRenderer {
    /// Test hook: a shared handle on this renderer's ledger, readable after the renderer drops.
    #[doc(hidden)]
    #[must_use]
    pub fn __graphics_ledger_handle(&self) -> crate::graphics_ledger::GraphicsLedgerHandle {
        self.graphics_ledger.clone()
    }
}

impl GpuRenderer {
    /// The adapter this renderer selected and its graphics API, as `(name, API)`, such as
    /// `("Apple M4 Max", "Metal")`. The name is empty when the platform reports none.
    #[must_use]
    pub fn adapter_summary(&self) -> (String, &'static str) {
        let info = self.adapter.get_info();
        (info.name, backend_display_name(info.backend))
    }
}

impl GpuRenderer {
    /// The adapter's PCI vendor and device ids while this renderer presents through DX12 rather
    /// than the Windows software presenter, so the OS graphics figure can find the same DXGI
    /// adapter; `None` on every other backend and under the software presenter.
    #[must_use]
    pub fn dxgi_adapter_ids(&self) -> Option<(u32, u32)> {
        let info = self.adapter.get_info();
        (info.backend == wgpu::Backend::Dx12 && !self.uses_windows_software_presenter())
            .then_some((info.vendor, info.device))
    }
}

impl GpuRenderer {
    /// Frame textures installed since construction by a rebuild or a recovery commit, so a native
    /// test can tell whether a present rebuilt the texture.
    #[doc(hidden)]
    #[must_use]
    pub fn __frame_texture_rebuilds(&self) -> u64 {
        self.frame_texture_installs
    }
}

impl GpuRenderer {
    /// Whether a covered-window trim left the frame texture at 1x1 awaiting the next present.
    #[doc(hidden)]
    #[must_use]
    pub fn __frame_texture_trimmed(&self) -> bool {
        self.frame_texture_trimmed
    }
}

impl GpuRenderer {
    /// The glyph atlas's pending compaction retry and its configured eviction permission, so a
    /// native test can check that a trim leaves both alone.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_glyph_atlas_retry_state(&self) -> (bool, bool) {
        (self.glyph_atlas_retry_without_eviction, self.glyph_atlas.eviction_enabled())
    }
}

impl GpuRenderer {
    /// Test hook: append one glyph drawing `rect_px` (`x, y, w, h` in surface pixels) in `color`
    /// to every later frame's terminal glyphs, before the cursor recolors, with the atlas
    /// coordinates of the frame's first terminal glyph; `None` removes it. It is not part of the
    /// frame key: a test changes it together with grid dirt, as a real glyph change would.
    #[doc(hidden)]
    pub fn __inject_test_glyph(&mut self, glyph: Option<((f32, f32, f32, f32), [f32; 4])>) {
        self.injected_test_glyph = glyph;
    }
}

impl GpuRenderer {
    /// Test hook: attach a glyph `(x, y, w, h)` in surface pixels and its color to the row at viewport
    /// slot `slot` of pane `pane_id`, drawn after that row's own glyphs whenever the row is emitted,
    /// so its ink joins the row's record; `None` removes it. It is not part of the frame key: a test
    /// changes it together with that row's dirt, as a real glyph change would.
    #[doc(hidden)]
    pub fn __inject_row_glyph(&mut self, glyph: Option<InjectedRowGlyph>) {
        self.injected_row_glyph = glyph;
    }
}

impl GpuRenderer {
    /// Test hook: the committed and staged glyph-row keys of `slot` of `pane_id` (0 for none),
    /// or `None` while the pane is untracked by the row glyph cache.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_glyph_slot_keys(&self, pane_id: u64, slot: u16) -> Option<(u64, u64)> {
        let committed = self.row_glyph_cache.committed_slot(pane_id, slot)?;
        Some((committed, self.row_glyph_cache.staged_slot(pane_id, slot)?))
    }
}

impl GpuRenderer {
    /// Test hook: the committed ink record of viewport slot `slot` of pane `pane_id`, as the last
    /// presented frame left it; `None` when no record is kept.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_row_ink(&self, pane_id: u64, slot: u16) -> Option<PixelRect> {
        self.row_ink.committed_rect(pane_id, slot)
    }
}

impl GpuRenderer {
    /// Test hook: the next wgpu presentation reports `reason` at the acquire point instead of
    /// asking the surface, after its plan was assembled, and takes the production retry branch.
    /// Arming it keeps the frame key, so the failing frame plans against the last presented one.
    #[doc(hidden)]
    pub fn __fail_next_surface_acquire(&mut self, reason: SurfaceRetryReason) {
        self.fault_surface_acquire = Some(reason);
    }
}

impl GpuRenderer {
    /// Test hook: the next presented frame's submission fails validation after its retained draw,
    /// as `GpuFaultKind::FrameValidation` makes it, but arming it keeps the frame key, so the failing
    /// frame plans against the last presented one; the failure path clears the key itself.
    #[doc(hidden)]
    pub fn __fail_next_frame_submission(&mut self) {
        self.fault_frame_probe = self
            .device_errors
            .gpu_work("fault.frame_probe", || create_frame_fault_probe(&self.device));
    }
}

impl GpuRenderer {
    /// Test hook: the next assembly fails with `Err` after its glyph rows are staged, reaching
    /// the assembly error arm of `render_releasing` rather than an ordinary failed outcome.
    #[doc(hidden)]
    pub fn __fail_next_assembly(&mut self) {
        self.fault_assembly_error = true;
    }
}

impl GpuRenderer {
    /// Test hook: the next presentation fails with `Err` from the presenter call itself, rather
    /// than the ordinary failed outcome `__fail_next_frame_submission` produces.
    #[doc(hidden)]
    pub fn __fail_next_present(&mut self) {
        self.fault_present_error = true;
    }
}

impl GpuRenderer {
    /// Test hook: record the slots each later assembly pass emits, per drawn pane, for
    /// `__take_emitted_rows`. Production never calls it, so no frame records anything.
    #[doc(hidden)]
    pub fn __enable_emitted_rows(&mut self) {
        self.emitted_rows_probe = Some(Vec::new());
    }
}

impl GpuRenderer {
    /// Test hook: the emitted slots of every drawn pane in the last assembly pass, cleared by
    /// this read; empty when the inspector is off or no pass ran since the last read.
    #[doc(hidden)]
    #[must_use]
    pub fn __take_emitted_rows(&mut self) -> Vec<(u64, Vec<u16>)> {
        self.emitted_rows_probe.as_mut().map(std::mem::take).unwrap_or_default()
    }
}

impl GpuRenderer {
    /// Test hook: keep each later presented frame's damage for `__take_presented_damage`.
    /// Production never calls it, so a presented frame builds and keeps no snapshot.
    #[doc(hidden)]
    pub fn __enable_presented_damage(&mut self) {
        self.presented_damage.enable();
    }
}

impl GpuRenderer {
    /// Test hook: the damage of the last presented frame, cleared by this read, so a frame that
    /// presents nothing reads `None` instead of an older frame's damage. `None` until enabled.
    #[doc(hidden)]
    pub fn __take_presented_damage(&mut self) -> Option<PresentedDamage> {
        self.presented_damage.take()
    }
}

impl GpuRenderer {
    /// Test hook: whether a retained frame key is kept, so the next changed frame is not a
    /// first frame.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_has_frame_key(&self) -> bool {
        self.last_frame_key.is_some()
    }
}

impl GpuRenderer {
    /// Append the test glyph seam's instance, if one is set, after the terminal rows.
    pub(super) fn push_injected_test_glyph(
        &mut self,
        glyphs: &mut Vec<GlyphInstance>,
        sw: f32,
        sh: f32,
    ) {
        let Some((rect_px, color)) = self.injected_test_glyph else {
            // When: injected_test_glyph is None, as in production, nothing is appended.
            return;
        };
        glyphs.extend(crate::cursor::seam_glyph(&mut self.glyph_atlas, rect_px, color, sw, sh));
    }
}

impl GpuRenderer {
    /// Test hook: recreate the retained frame texture copyable (`COPY_SRC`), now and on every later
    /// recreation, so [`Self::__copy_retained_frame`] can read it back. Production never calls it,
    /// so production frame textures keep their usage. The next frame draws in full.
    #[doc(hidden)]
    pub fn __enable_retained_frame_readback(&mut self) {
        self.retained_frame_readback = true;
        self.rebuild_frame_texture();
        self.last_frame_key = None;
    }
}

impl GpuRenderer {
    /// Test hook: copy the retained frame into a new readback buffer, through the device gate, and
    /// return what the test needs to map it. `None` until readback is enabled, or once the device
    /// stopped. The test maps and polls; this never does.
    #[doc(hidden)]
    pub fn __copy_retained_frame(&mut self) -> Option<RetainedFrameReadback> {
        if !self.retained_frame_readback {
            // When: retained_frame_readback is false the texture has no COPY_SRC to copy from.
            return None;
        }
        let Some(_scope) = self.device_errors.enter_gpu_work("frame_texture.readback") else {
            // When: enter_gpu_work refuses, a stopped device copies nothing.
            return None;
        };
        let (width_px, height_px) = (self.frame_texture.width(), self.frame_texture.height());
        let padded_row_bytes = padded_readback_row_bytes(width_px);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sonic-retained-frame-readback"),
            size: u64::from(padded_row_bytes) * u64::from(height_px),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("sonic-retained-frame-readback"),
        });
        encoder.copy_texture_to_buffer(
            self.frame_texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row_bytes),
                    rows_per_image: Some(height_px),
                },
            },
            wgpu::Extent3d { width: width_px, height: height_px, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        Some(RetainedFrameReadback {
            device: self.device.clone(),
            buffer,
            width_px,
            height_px,
            padded_row_bytes,
        })
    }
}

impl GpuRenderer {
    /// Test hook: resident glyph tiles that hold colour pixels, so a native test can tell a colour
    /// face resolved before it compares colour tiles.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_resident_color_tiles(&self) -> usize {
        self.glyph_atlas
            .resident_tile_keys()
            .into_iter()
            .filter(|key| self.glyph_atlas.get(*key).is_some_and(|info| info.is_color))
            .count()
    }
}

impl GpuRenderer {
    /// Test hook: the alpha census of `character`'s resident colour tile, read from the CPU atlas,
    /// so a native test can assert the colour glyph it selected holds translucent edge pixels.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_colour_tile_alpha(
        &self,
        character: char,
    ) -> Option<crate::glyph_working_set::ColourTileAlpha> {
        crate::glyph_working_set::colour_tile_alpha(&self.glyph_atlas, character)
    }
}

impl GpuRenderer {
    /// The retained frame texture's actual extent: 1x1 under the Windows software presenter, else
    /// the configured surface size. GPU memory, so it is not part of [`Self::retained_amounts`].
    #[must_use]
    pub fn frame_texture_extent(&self) -> (u32, u32) {
        (self.frame_texture.width(), self.frame_texture.height())
    }
}

impl GpuRenderer {
    /// Current OS display scale factor (physical px per logical px). Exposed so
    /// the app layer can scale window-event geometry (e.g. the search-bar IME
    /// caret rect) to match the renderer's physical-px coordinate space.
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }
}

impl GpuRenderer {
    /// Number of glyph tiles currently resident in the rasterizer atlas.
    /// Test-only; the atlas is cleared and rebuilt by [`Self::set_font`].
    #[doc(hidden)]
    pub fn glyph_atlas_len(&self) -> usize {
        self.glyph_atlas.len()
    }
}

impl GpuRenderer {
    /// Test hook: in-place glyph atlas resets since construction.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_glyph_atlas_resets(&self) -> u64 {
        self.glyph_atlas_resets
    }
}

impl GpuRenderer {
    /// Test hook: glyph atlas lookups that missed, each one a rasterization; a reset zeroes it.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_glyph_atlas_misses(&self) -> u64 {
        self.glyph_atlas.misses()
    }
}

impl GpuRenderer {
    /// Test hook: the glyph atlas's CPU size and its GPU upload's size, so a native test can check
    /// that the upload follows a growth (or stays the 1x1 placeholder under the software presenter).
    #[doc(hidden)]
    #[must_use]
    pub fn __test_glyph_atlas_dimensions(&self) -> ((u32, u32), (u32, u32)) {
        (
            (self.glyph_atlas.width(), self.glyph_atlas.height()),
            (self.glyph_upload.width(), self.glyph_upload.height()),
        )
    }
}

impl GpuRenderer {
    /// Test hook: every resident tile of raster role `variant` as `(key, width, height)`, the
    /// pixel size of its atlas rect, read from the atlas itself rather than through a font stack,
    /// so a stale tile reports the size it was drawn at.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_resident_tile_sizes(
        &self,
        variant: GlyphRasterVariant,
    ) -> Vec<(sonicterm_types::GlyphKey, u32, u32)> {
        self.glyph_atlas
            .resident_tile_keys()
            .into_iter()
            .filter(|key| key.raster_variant == variant)
            .filter_map(|key| {
                let [width, height] = self.glyph_atlas.get(key)?.px_size;
                Some((key, width, height))
            })
            .collect()
    }
}

impl GpuRenderer {
    /// Test hook: the keys of every glyph tile resident in the atlas, sentinels excluded, so a
    /// native test can compare them with the working-set helper's key set.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_resident_tile_keys(
        &self,
    ) -> std::collections::HashSet<sonicterm_types::GlyphKey> {
        self.glyph_atlas.resident_tile_keys()
    }
}

impl GpuRenderer {
    /// Test seam: every resident glyph tile by identity across font configurations, with its
    /// raster size, resolved through the stack that drew its raster variant; the second value
    /// lists the resident keys that resolved to no identity.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_resident_tile_identities(
        &self,
    ) -> (
        std::collections::HashMap<crate::glyph_working_set::TileIdentity, [u32; 2]>,
        Vec<sonicterm_types::GlyphKey>,
    ) {
        crate::glyph_working_set::resident_tile_identities(&self.glyph_atlas, |variant| {
            match variant {
                GlyphRasterVariant::Normal => self.font_stack.as_ref(),
                GlyphRasterVariant::TabTitle => self.tab_title_font.stack(),
                GlyphRasterVariant::PaletteFooter => self.palette_footer_font_stack.as_ref(),
                GlyphRasterVariant::StatusLine => self.status_line_font_stack.as_ref(),
            }
        })
    }
}

impl GpuRenderer {
    /// Test seam: every resident body-text tile with its raster size and, when its stack resolves
    /// it, its identity, so a test can find the tile the renderer drew for a codepoint.
    #[doc(hidden)]
    #[must_use]
    pub fn __test_resident_body_tiles(
        &self,
    ) -> Vec<(sonicterm_types::GlyphKey, [u32; 2], Option<crate::glyph_working_set::TileIdentity>)>
    {
        let mut tiles = Vec::new();
        for key in self.glyph_atlas.resident_tile_keys() {
            let Some(info) = self.glyph_atlas.get(key) else {
                // When: `get(key)` finds no entry, the tile left the atlas since the key list was taken.
                continue;
            };
            if key.raster_variant != GlyphRasterVariant::Normal || info.missing || info.oversize {
                // When: `key.raster_variant` is chrome, or `info` is a missing or oversize sentinel, it is no body tile.
                continue;
            }
            let identity = self
                .font_stack
                .as_ref()
                .and_then(|stack| crate::glyph_working_set::tile_identity(stack, key, &info));
            tiles.push((key, info.px_size, identity));
        }
        tiles
    }
}
