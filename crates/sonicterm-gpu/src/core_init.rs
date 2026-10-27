//! Init responsibilities of the renderer, preserving its single owner.

use super::*;

/// Create a wgpu instance for `event_loop`'s display, honoring `WGPU_BACKEND`.
///
/// Startup and GPU recovery both call this, so a rebuilt device is requested from
/// an instance configured exactly like the first one.
pub(super) fn new_instance(event_loop: &ActiveEventLoop) -> Instance {
    Instance::new(InstanceDescriptor::new_with_display_handle_from_env(Box::new(
        event_loop.owned_display_handle(),
    )))
}

pub(super) enum RendererBootstrap<'a> {
    Fresh(&'a ActiveEventLoop),
    Shared(GpuSharedContext),
    Recovered(RecoveredContext),
}

impl GpuRenderer {
    /// Build a renderer bound to `window`. Creates the wgpu surface +
    /// device + pipelines, the FontStack shaping and rasterization stacks, the glyph atlas,
    /// and seeds the initial cell metrics from `theme`'s configured
    /// font family / size / line height.
    pub fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        settings: RendererSettings<'_>,
    ) -> Result<Self> {
        let span = tracing::debug_span!(target: "render_timing", "renderer_init", window_id = ?window.id(), role = settings.role, shared = false);
        let _entered = span.enter();
        let timing = InitTiming::begin("renderer_new");
        let result = pollster::block_on(Self::new_async(
            window,
            theme,
            settings,
            RendererBootstrap::Fresh(event_loop),
        ));
        InitTiming::finish(
            timing,
            if result.is_ok() { InitOutcome::Ok } else { InitOutcome::Error },
        );
        result
    }
}

impl GpuRenderer {
    /// Build a renderer that shares an existing wgpu instance, adapter, device,
    /// and queue with another window.
    ///
    /// Every window after the first takes this path, including New Window,
    /// warm-pool, and tear-out windows: one device serves all of them, so opening
    /// a window neither re-enumerates adapters nor allocates a second device. The
    /// surface, pipelines, and atlases are still per-window.
    pub fn new_with_shared_context(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        settings: RendererSettings<'_>,
        shared: GpuSharedContext,
    ) -> Result<Self> {
        let span = tracing::debug_span!(target: "render_timing", "renderer_init", window_id = ?window.id(), role = settings.role, shared = true);
        let _entered = span.enter();
        let timing = InitTiming::begin("renderer_new");
        let _ = event_loop;
        let result = pollster::block_on(Self::new_async(
            window,
            theme,
            settings,
            RendererBootstrap::Shared(shared),
        ));
        InitTiming::finish(
            timing,
            if result.is_ok() { InitOutcome::Ok } else { InitOutcome::Error },
        );
        result
    }
}

impl GpuRenderer {
    /// Finish a negotiated startup on its window's event-loop thread without requesting another device or surface.
    pub fn finish_startup(
        prepared: RecoveredContext,
        theme: &Theme,
        settings: RendererSettings<'_>,
    ) -> Result<Self> {
        let window = Arc::clone(prepared.window());
        let span = tracing::debug_span!(target: "render_timing", "renderer_init", window_id = ?window.id(), role = settings.role, prepared = true);
        let _entered = span.enter();
        let timing = InitTiming::begin("renderer_finish");
        let result = pollster::block_on(Self::new_async(
            window,
            theme,
            settings,
            RendererBootstrap::Recovered(prepared),
        ));
        InitTiming::finish(
            timing,
            if result.is_ok() { InitOutcome::Ok } else { InitOutcome::Error },
        );
        result
    }
}

impl GpuRenderer {
    /// Clone the handles a sibling window needs to share this renderer's GPU
    /// context, for passing to [`Self::new_with_shared_context`].
    ///
    /// The clones are wgpu reference-counted handles to one underlying device,
    /// not copies of it. They also share that device's containment state: once
    /// the device stops accepting work, a renderer built from them fails to
    /// construct.
    pub fn shared_context(&self) -> GpuSharedContext {
        GpuSharedContext {
            instance: self.instance.clone(),
            adapter: self.adapter.clone(),
            device: self.device.clone(),
            queue: self.queue.clone(),
            device_errors: Arc::clone(&self.device_errors),
        }
    }
}

impl GpuRenderer {
    /// Whether `self` and `other` render through the same wgpu device.
    ///
    /// Test seam for the one-device-per-process contract. It compares the
    /// instance as well as the device: wgpu compares a device only by its id,
    /// and each instance allocates ids from the same start, so two unshared
    /// renderers can hold equal device ids. An instance compares by the address
    /// of its shared context, which [`GpuSharedContext`] clones rather than copies.
    #[doc(hidden)]
    #[must_use]
    pub fn shares_device_with(&self, other: &GpuRenderer) -> bool {
        self.instance == other.instance && self.device == other.device
    }
}

impl GpuRenderer {
    // Ordering: `LIVE_RENDERERS.fetch_add(1, Ordering::AcqRel)`, pairing with
    // the `Ordering::AcqRel` decrement in `Drop`. Publishes no payload.
    pub(super) async fn new_async(
        window: Arc<Window>,
        theme: &Theme,
        settings: RendererSettings<'_>,
        bootstrap: RendererBootstrap<'_>,
    ) -> Result<Self> {
        let RendererSettings {
            font_family,
            font_dirs,
            font_size,
            line_height_mult,
            font_weight_scale,
            subpixel_aa,
            padding,
            appearance,
            role,
            glyph_atlas_start,
        } = settings;
        let font_weight_scale = effective_font_weight_scale(font_weight_scale);
        let [padding_left, padding_right, padding_top, padding_bottom] = padding;
        let size = window.inner_size();
        // The OS scale converts logical font and chrome sizes into raster pixels.
        let sf = window.scale_factor() as f32;
        let (instance, surface, adapter, device, queue, errors, software_rendering) =
            match bootstrap {
                RendererBootstrap::Fresh(event_loop) => {
                    // Fresh has no device, so negotiation precedes the common renderer gate.
                    let timing = InitTiming::begin("instance_new");
                    let instance = new_instance(event_loop);
                    InitTiming::finish(timing, InitOutcome::Returned);
                    let timing = InitTiming::begin("create_surface");
                    let surface_result = instance.create_surface(window.clone());
                    InitTiming::finish(
                        timing,
                        if surface_result.is_ok() { InitOutcome::Ok } else { InitOutcome::Error },
                    );
                    let surface = surface_result.context("create surface")?;
                    let negotiated =
                        recovery_context::negotiate_device(&instance, &surface).await?;
                    (
                        instance,
                        surface,
                        negotiated.adapter,
                        negotiated.device,
                        negotiated.queue,
                        negotiated.device_errors,
                        negotiated.software_rendering,
                    )
                }
                RendererBootstrap::Shared(shared) => {
                    // When: Shared already owns a device, create only this window's surface before assembly.
                    if !shared.device_errors.accepts_gpu_work() {
                        // When: accepts_gpu_work rejects the shared device, no new surface can become drawable.
                        return Err(anyhow!("shared GPU device stopped accepting work"));
                    }
                    let timing = InitTiming::begin("instance_reuse");
                    let instance = shared.instance;
                    InitTiming::finish(timing, InitOutcome::Returned);
                    let timing = InitTiming::begin("create_surface");
                    let surface_result = instance.create_surface(window.clone());
                    InitTiming::finish(
                        timing,
                        if surface_result.is_ok() { InitOutcome::Ok } else { InitOutcome::Error },
                    );
                    let surface = surface_result.context("create surface")?;
                    let info = shared.adapter.get_info();
                    let software_rendering = detect_software_rendering(&info);
                    let device_memory_policy = device_memory_policy_from(software_rendering);
                    tracing::info!(backend = ?info.backend, name = %info.name, driver = %info.driver,
                    device_type = ?info.device_type, software_rendering,
                    device_memory_policy = ?device_memory_policy, "wgpu adapter reused");
                    (
                        instance,
                        surface,
                        shared.adapter,
                        shared.device,
                        shared.queue,
                        shared.device_errors,
                        software_rendering,
                    )
                }
                RendererBootstrap::Recovered(prepared) => {
                    // When: Recovered contains a negotiated device and surface, reuse both without native negotiation.
                    let (shared, candidate) = prepared.into_parts();
                    if !shared.device_errors.accepts_gpu_work() {
                        // When: accepts_gpu_work rejects the negotiated device, do not configure its retained surface.
                        return Err(anyhow!("prepared GPU device stopped accepting work"));
                    }
                    let software_rendering = detect_software_rendering(&shared.adapter.get_info());
                    (
                        shared.instance,
                        candidate.surface,
                        shared.adapter,
                        shared.device,
                        shared.queue,
                        shared.device_errors,
                        software_rendering,
                    )
                }
            };

        let format = TextureFormat::Bgra8UnormSrgb;
        let max_surface_dimension =
            device.limits().max_texture_dimension_2d.min(MAX_SURFACE_DIMENSION);
        let validated_size =
            validated_surface_size(size.width, size.height, max_surface_dimension).ok_or_else(
                || {
                    anyhow!(
                        "window surface {}x{} exceeds renderer limits (max dimension {}, max BGRA bytes {})",
                        size.width,
                        size.height,
                        max_surface_dimension,
                        MAX_SURFACE_BYTES
                    )
                },
            )?;
        tracing::debug!(
            target: "memory",
            requested_width = size.width,
            requested_height = size.height,
            width = validated_size.width,
            height = validated_size.height,
            bgra_bytes = validated_size.bytes,
            software_rendering,
            "renderer initial surface allocation accepted"
        );
        // Prefer Mailbox when the backend exposes it: Mailbox drops in-flight
        // superseded frames so a fast-typing user always sees the newest
        // keystroke without waiting a full vblank. Fall back to Fifo on
        // backends that don't advertise Mailbox (Fifo is universally supported
        // and remains the spec-mandated default).
        let timing = InitTiming::begin("surface_capabilities");
        let surface_caps = surface.get_capabilities(&adapter);
        InitTiming::finish(timing, InitOutcome::Returned);
        let hardware_present_mode = if surface_caps.present_modes.contains(&PresentMode::Mailbox) {
            PresentMode::Mailbox
        } else {
            PresentMode::Fifo
        };
        let hardware_alpha_mode = if appearance.backdrop == BackdropKind::Opaque {
            CompositeAlphaMode::Opaque
        } else {
            CompositeAlphaMode::PreMultiplied
        };
        let software_render_degrade =
            software_render_degrade_from(appearance.software_render_mode, software_rendering);
        let present_mode =
            if software_render_degrade { PresentMode::Fifo } else { hardware_present_mode };
        let alpha_mode =
            if software_render_degrade { CompositeAlphaMode::Opaque } else { hardware_alpha_mode };
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: validated_size.width,
            height: validated_size.height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: if software_render_degrade { 1 } else { 2 },
        };
        let init_scope = errors
            .enter_gpu_work("renderer.configure")
            .ok_or_else(|| anyhow!("GPU device stopped accepting work"))?;
        let timing = InitTiming::begin("surface_configure");
        surface.configure(&device, &config);
        InitTiming::finish(timing, InitOutcome::Returned);
        if !errors.accepts_gpu_work() {
            // When: `accepts_gpu_work` fails after `configure`, the surface cannot be acquired.
            tracing::debug!(target: "render_timing", operation = "surface_configure", phase = "gate", accepted = false, "renderer initialization");
            return Err(anyhow!("initial surface configure raised a contained GPU error"));
        }
        tracing::debug!(target: "render_timing", operation = "surface_configure", phase = "gate", accepted = true, "renderer initialization");
        init_scope.set_operation("renderer.init");

        // B3 GPU text path. Allocate independent glyph and inline-image
        // atlases up front so media pressure cannot recycle text UVs.
        // No more SwashRasterizer
        // prebake — chrome and grid share the glyph atlas, populated
        // on demand by the wezterm rasterizer on every miss.
        let timing = InitTiming::begin("present_pipeline");
        let mut present_pipeline = WeztermPipeline::new(&device, format, 4096);
        let graphics_ledger = crate::graphics_ledger::GraphicsLedgerHandle::default();
        present_pipeline.attach_graphics_ledger(graphics_ledger.clone());
        InitTiming::finish(timing, InitOutcome::Returned);
        let software_presenter = cfg!(target_os = "windows") && software_render_degrade;
        let timing = InitTiming::begin("frame_texture");
        let (frame_texture, frame_view) = build_frame_texture(
            &device,
            software_presenter,
            config.width,
            config.height,
            format,
            false,
        );
        InitTiming::finish(timing, InitOutcome::Returned);
        let timing = InitTiming::begin("frame_blitter");
        let frame_blitter = wgpu::util::TextureBlitter::new(&device, format);
        InitTiming::finish(timing, InitOutcome::Returned);
        let timing = InitTiming::begin("glyph_atlas");
        // The glyph atlas is the only growable atlas: it starts at its measured size and doubles
        // up to ATLAS_DIM before it evicts.
        let glyph_atlas = GlyphAtlas::growable(
            start_dim(sf, glyph_atlas_start),
            sonicterm_text::glyph_atlas::ATLAS_DIM,
        );
        InitTiming::finish(timing, InitOutcome::Returned);
        let timing = InitTiming::begin("image_atlas");
        let image_atlas = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);
        InitTiming::finish(timing, InitOutcome::Returned);
        let glyph_gpu_dimensions = desired_gpu_atlas_dimensions(software_presenter, &glyph_atlas);
        let image_gpu_dimensions = desired_gpu_atlas_dimensions(software_presenter, &image_atlas);
        let timing = InitTiming::begin("glyph_upload");
        let glyph_upload = AtlasUpload::new_sized(
            &device,
            glyph_gpu_dimensions.0,
            glyph_gpu_dimensions.1,
            present_pipeline.glyph_bind_group_layout(),
            AtlasBindingKind::Glyph,
        );
        InitTiming::finish(timing, InitOutcome::Returned);
        let timing = InitTiming::begin("image_upload");
        let image_upload = AtlasUpload::new_sized(
            &device,
            image_gpu_dimensions.0,
            image_gpu_dimensions.1,
            present_pipeline.image_bind_group_layout(),
            AtlasBindingKind::Image,
        );
        InitTiming::finish(timing, InitOutcome::Returned);
        tracing::debug!(
            target: "memory",
            renderer_role = role,
            window_id = ?window.id(),
            software_presenter,
            glyph_cpu_width = glyph_atlas.width(),
            glyph_cpu_height = glyph_atlas.height(),
            glyph_cpu_payload_bytes = atlas_payload_bytes(glyph_atlas.width(), glyph_atlas.height()),
            glyph_gpu_width = glyph_gpu_dimensions.0,
            glyph_gpu_height = glyph_gpu_dimensions.1,
            glyph_gpu_payload_bytes = glyph_upload.payload_bytes(),
            image_cpu_width = image_atlas.width(),
            image_cpu_height = image_atlas.height(),
            image_cpu_payload_bytes = atlas_payload_bytes(image_atlas.width(), image_atlas.height()),
            image_gpu_width = image_gpu_dimensions.0,
            image_gpu_height = image_gpu_dimensions.1,
            image_gpu_payload_bytes = image_upload.payload_bytes(),
            glyph_resident = glyph_atlas.len(),
            image_resident = image_atlas.len(),
            retained_inline_media_bytes = 0,
            payload_estimate = true,
            "renderer atlas payload initialized"
        );

        // FontStack derives raster metrics from point size at 72 * sf DPI; missing fonts use a scaled size estimate.
        let fs_dpi = (72.0 * sf).round() as usize;
        let timing = InitTiming::begin("font_stacks");
        let font_stacks =
            renderer_font_stacks(font_family, font_size, fs_dpi, font_weight_scale, font_dirs);
        InitTiming::finish(timing, InitOutcome::Returned);
        let timing = InitTiming::begin("cell_metrics");
        let (cell_w, natural_cell_h) =
            match font_stacks.body.as_ref().and_then(|s| s.cell_metrics_raster_px().ok()) {
                Some(m) => (m.cell_w as f32, m.cell_h as f32),
                None => (font_size * 0.6 * sf, font_size * 1.2 * sf),
            };
        InitTiming::finish(timing, InitOutcome::Returned);
        let line_height = natural_cell_h * line_height_mult.max(0.0).max(0.01);
        let cell_h = line_height;

        let bg = hex_to_wgpu_with_alpha(theme.colors.background.0.as_str(), appearance.opacity);
        let bg_rgba = hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
        let fg_default = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        let cursor_color = cursor_color_from_theme(theme);
        let cursor_text_color = cursor_text_color_from_theme(theme);
        let selection_color = hex_to_premultiplied_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        let tab_bar_bg = hex_to_premultiplied_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        let tab_active_bg = hex_to_premultiplied_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        let tab_inactive_bg =
            hex_to_premultiplied_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        let tab_active_fg = hex_to_chrome_color(theme.colors.tab.active_fg.0.as_str());
        let tab_inactive_fg = hex_to_chrome_color(theme.colors.tab.inactive_fg.0.as_str());
        let tab_separator =
            hex_to_premultiplied_rgba(theme.colors.tab.inactive_fg.0.as_str(), 0.45);
        // Hyperlink visuals: theme-aware. Use the theme's cursor color as the
        // accent (every bundled theme designates it). Underline reads as
        // deliberate at high opacity; the tint behind the run is subtle.
        let hyperlink_underline = hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 0.9);
        let splitter_color = splitter_color_from_theme(theme);
        let tint_alpha = match theme.appearance {
            sonicterm_render_model::boundary::cfg::theme::Appearance::Dark => {
                // When: `Appearance::Dark` — dark needs more accent before
                // the hyperlink tint reads as tinted at all.
                0.14
            }
            sonicterm_render_model::boundary::cfg::theme::Appearance::Light => {
                // When: `Appearance::Light` — 0.14 reads as a highlighter
                // stripe over the text rather than a hint beneath it.
                0.10
            }
        };
        let hyperlink_tint = hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), tint_alpha);
        let search_highlight =
            hex_to_premultiplied_rgba(theme.colors.bright.yellow.0.as_str(), 0.35);
        let search_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        let search_bg = hex_to_premultiplied_rgba(theme.colors.tab.bar_bg.0.as_str(), 0.95);
        // Cosmic-text Buffer / Metrics allocations deleted.
        // Chrome strings are shape+raster'd on demand inside `render()`
        // through `chrome_text::layout(...)`; there is no persistent
        // per-overlay text buffer to size at construction.

        if !errors.accepts_gpu_work() {
            // When: `accepts_gpu_work` fails after creation, the pipelines may be invalid.
            return Err(anyhow!("renderer creation raised a contained GPU error"));
        }
        drop(init_scope);
        // Counted here rather than earlier in `new`: every `?` above this
        // point returns without producing a renderer, so incrementing sooner
        // would charge for instances that never existed and never drop.
        LIVE_RENDERERS.fetch_add(1, Ordering::AcqRel);

        let renderer = Self {
            instance,
            adapter,
            software_rendering,
            software_render_degrade,
            device,
            queue,
            device_errors: errors,
            device_stop_reported: false,
            fault_invalid_glyph_upload: false,
            fault_frame_probe: None,
            fault_assembly_error: false,
            emitted_rows_probe: None,
            fault_present_error: false,
            fault_surface_occluded: false,
            #[cfg(target_os = "windows")]
            fault_stop_before_cached_present: false,
            present_calls: 0,
            surface,
            config,
            hardware_present_mode,
            hardware_alpha_mode,
            window,
            present_pipeline,
            frame_texture,
            frame_view,
            frame_blitter,
            glyph_atlas,
            glyph_upload,
            glyph_atlas_generation: 0,
            fault_atlas_change_during_assembly: false,
            image_atlas,
            image_upload,
            retained_inline_media_bytes: 0,
            frames_without_inline_media: 0,
            image_upload_rebuild_pending: false,
            inline_media_absent_since: None,
            glyph_atlas_retry_without_eviction: false,
            growth_episodes: crate::frame_stats::GrowthEpisodes::default(),
            retained_frame_readback: false,
            glyph_atlas_resets: 0,
            frame_texture_trimmed: false,
            frame_texture_installs: 0,
            graphics_ledger,
            font_family: font_family.to_string(),
            font_dirs: font_dirs.to_vec(),
            font_size,
            line_height,
            font_weight_scale,
            subpixel_aa,
            line_height_mult: line_height_mult.max(0.0).max(0.01),
            scale_factor: sf,
            cell_w,
            cell_h,
            padding_left,
            padding_right,
            padding_top,
            padding_bottom,
            bg,
            bg_opacity: appearance.opacity.clamp(0.0, 1.0),
            scrollbar_mode: appearance.scrollbar,
            panel_padding: appearance.panel_padding.max(0.0),
            fg_default,
            cursor_color,
            cursor_text_color,
            bg_rgba,
            cursor_shape: CursorShape::default(),
            cursor_blink: true,
            blink_epoch: Instant::now(),
            window_focused: true,
            inactive_pane_cursors: Vec::new(),
            pane_focus_flash: None,
            selection_color,
            tab_bar_bg,
            tab_active_bg,
            tab_inactive_bg,
            tab_active_fg,
            tab_inactive_fg,
            tab_close_override: None,
            hover_cursor: None,
            tab_separator,
            hyperlink_underline,
            splitter_color,
            hyperlink_tint,
            search_highlight,
            search_fg,
            search_bg,
            drag_chip_visual: None,
            last_frame_key: None,
            last_recolor: crate::cursor::RecolorRecord::default(),
            last_tab_ink: crate::cursor::RecolorBounds::Empty,
            injected_test_glyph: None,
            injected_row_glyph: None,
            fault_surface_acquire: None,
            row_ink: crate::row_ink::RowInkTable::default(),
            presented_damage: PresentedDamageRecorder::default(),
            presented_fields: PresentedFields::default(),
            preedit_glyph_cache: None,
            skipped_frames: 0,
            successful_frame_count: 0,
            frame_sink: None,
            present_hook: None,
            #[cfg(target_os = "windows")]
            software_frame: None,
            render_timing_label: role,
            tab_bar_visible: true,
            status_line_visible: false,
            status_line_position: Default::default(),
            status_line_font_scale: crate::status_strip::default_font_scale(),
            status_line_view: StatusLineView::default(),
            status_line_fit: None,
            status_line_cap_height: None,
            status_line_layouts: Default::default(),
            status_line_focus: None,
            titlebar_inset: 0.0,
            last_missing_chars: Vec::new(),
            presented_missing: std::collections::HashMap::new(),
            staged_missing: Vec::new(),
            unresolved_snapshot: None,
            acknowledged_unresolved: None,
            last_missing_chrome_chars: Vec::new(),
            completeness: None,
            // `shape_cache` field deleted with the cosmic-text path.
            font_stack: font_stacks.body,
            tab_title_font: TabTitleFont::new(
                font_family,
                font_size,
                font_weight_scale,
                sf,
                font_stacks.tab_title,
            ),
            palette_footer_font_stack: font_stacks.palette_footer,
            status_line_font_stack: font_stacks.status_line,
            // Seeded from the constructor theme, so the first frame derives no palette.
            chrome_caches: crate::chrome_cache::ChromeCaches::new(theme),
            chrome_reuse: true,
            frame_scratch: frame_scratch::ScratchHome::new(),
            row_glyph_cache: sonicterm_text::row_glyph_cache::RowGlyphCache::new(),
            line_quad_cache: crate::row_quad_cache::LineQuadCache::new(),
            last_emit_origins: Vec::new(),
            last_pane_layout: Vec::new(),
            style_rev: 0,
            applied_fonts: None,
            unattributed_apply: false,
            fallback_waker: None,
            drag_chip: None,
            async_loader: None,
        };
        renderer.record_surface_request();
        renderer.record_frame_texture_request();
        renderer.record_glyph_atlas_request();
        renderer.record_image_atlas_request();
        renderer.log_subpixel_aa_policy();
        Ok(renderer)
    }
}

#[cfg(test)]
#[path = "core_init_tests.rs"]
mod core_init_tests;
