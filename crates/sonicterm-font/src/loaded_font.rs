//! Loaded-face shaping, asynchronous fallback adoption and rasterizer lookup.

use super::*;

impl std::fmt::Debug for LoadedFont {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.debug_struct("LoadedFont")
            .field("handles", &self.handles)
            .field("metrics", &self.metrics)
            .field("font_size", &self.font_size)
            .field("dpi", &self.dpi)
            .field("pending_fallback", &self.pending_fallback)
            .field("text_style", &self.text_style)
            .finish()
    }
}

impl LoadedFont {
    /// Returns the metrics selected when this font stack was loaded.
    pub fn metrics(&self) -> FontMetrics {
        self.metrics
    }

    /// Returns the text style from which this font stack was resolved.
    pub fn style(&self) -> &TextStyle {
        &self.text_style
    }

    /// Returns this loaded font stack's process-unique identifier.
    pub fn id(&self) -> LoadedFontId {
        self.id
    }

    // Lock order: mutate `handles` first and release that `RefCell` borrow; only then
    // upgrade `font_config`, borrow its config, and borrow `shaper` plus `handles` for rebuild.
    pub(super) fn insert_fallback_handles(
        &self,
        extra_handles: Vec<ParsedFont>,
    ) -> anyhow::Result<bool> {
        let mut loaded = false;
        {
            let mut handles = self.handles.borrow_mut();
            for h in extra_handles {
                if !handles.contains(&h) {
                    handles.push(h);
                    loaded = true;
                }
            }
            if loaded {
                log::trace!("revised fallback: {:#?}", handles);
            }
        }
        if loaded {
            if let Some(font_config) = self.font_config.upgrade() {
                *self.shaper.borrow_mut() = new_shaper(
                    &font_config.config.borrow(),
                    &self.handles.borrow(),
                    self.configured_handle_count,
                )?;
            }
        }
        Ok(loaded)
    }

    /// Shapes text, waiting and retrying when asynchronous fallback discovery is required.
    pub fn blocking_shape(
        &self,
        text: &str,
        presentation: Option<Presentation>,
        direction: Direction,
        range: Option<Range<usize>>,
        presentation_width: Option<&PresentationWidth>,
    ) -> anyhow::Result<Vec<GlyphInfo>> {
        let mut iteration = 0_u64;
        loop {
            iteration += 1;
            let shape_span = diagnostic_timing::enabled().then(|| {
                tracing::debug_span!(
                    target: "render_timing", "font_shape", loaded_font_id = self.id, iteration
                )
            });
            let _entered = shape_span.as_ref().map(tracing::Span::enter);
            let (fallback_done_sender, fallback_done_receiver) = channel();
            let shape_timing = Timing::begin("shape_impl");
            let shaped = self.shape_impl(
                text,
                move || {
                    let _ = fallback_done_sender.send(());
                },
                |_| {},
                presentation,
                direction,
                range.clone(),
                presentation_width,
                true,
            );
            Timing::finish(shape_timing, if shaped.is_ok() { "ok" } else { "error" });
            let (async_resolve, glyphs) = match shaped {
                Ok(shaped_glyphs) => shaped_glyphs,
                Err(error) if error.downcast_ref::<ClearShapeCache>().is_some() => {
                    // When: `error.downcast_ref::<ClearShapeCache>().is_some()` is true, retry.
                    continue;
                }
                Err(error) => {
                    // When: the guarded `ClearShapeCache` arm did not match, return `error`.
                    return Err(error);
                }
            };

            if !async_resolve {
                // When: `async_resolve` is false, no fallback completion can improve this result.
                return Ok(glyphs);
            }
            let fallback_done =
                Timing::result("fallback_receive", || fallback_done_receiver.recv());
            if fallback_done.is_err() {
                // When: fallback_done is an error, retain the glyphs already shaped.
                return Ok(glyphs);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// Shapes text once and schedules any required fallback resolution asynchronously.
    pub fn shape<F: FnOnce() + Send + 'static, FS: FnOnce(&mut Vec<char>)>(
        &self,
        text: &str,
        completion: F,
        filter_out_synthetic: FS,
        presentation: Option<Presentation>,
        direction: Direction,
        range: Option<Range<usize>>,
        presentation_width: Option<&PresentationWidth>,
    ) -> anyhow::Result<Vec<GlyphInfo>> {
        let (_async_resolve, res) = self.shape_impl(
            text,
            completion,
            filter_out_synthetic,
            presentation,
            direction,
            range,
            presentation_width,
            true,
        )?;
        Ok(res)
    }

    /// Shapes text for a frame without ever waiting for fallback discovery.
    ///
    /// Faces already published are merged when `pending_fallback` is free; when the worker
    /// holds it, this attempt shapes with the faces merged so far. Missing characters are
    /// scheduled once and draw as notdef until a later frame merges their face. At most
    /// [`MAX_FRAME_SHAPE_ATTEMPTS`] shapes run per call.
    pub fn shape_for_frame(
        &self,
        text: &str,
        presentation: Option<Presentation>,
        direction: Direction,
        range: Option<Range<usize>>,
        presentation_width: Option<&PresentationWidth>,
    ) -> anyhow::Result<Vec<GlyphInfo>> {
        bounded_frame_shape(|| {
            let (_async_resolve, glyphs) = self.shape_impl(
                text,
                || {},
                |_| {},
                presentation,
                direction,
                range.clone(),
                presentation_width,
                false,
            )?;
            Ok(glyphs)
        })
    }

    // Lock order: `pending_fallback` -> `shaper` -> `tried_glyphs` -> `font_config`;
    // with `wait_for_pending` false the first is only tried, so a frame never waits.
    #[allow(clippy::too_many_arguments)]
    fn shape_impl<F: FnOnce() + Send + 'static, FS: FnOnce(&mut Vec<char>)>(
        &self,
        text: &str,
        completion: F,
        filter_out_synthetic: FS,
        presentation: Option<Presentation>,
        direction: Direction,
        range: Option<Range<usize>>,
        presentation_width: Option<&PresentationWidth>,
        wait_for_pending: bool,
    ) -> anyhow::Result<(bool, Vec<GlyphInfo>)> {
        let mut no_glyphs = vec![];

        let pending = if wait_for_pending {
            // An explicit caller may wait: take the lock as the worker releases it.
            Some(self.pending_fallback.lock().unwrap_or_else(PoisonError::into_inner))
        } else {
            // When: `wait_for_pending` is false (the frame path), only try the lock.
            match self.pending_fallback.try_lock() {
                Ok(guard) => Some(guard),
                Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    // The worker is appending: skip the merge. Its completion follows the unlock,
                    // so a newer generation brings a later frame that merges.
                    None
                }
            }
        };
        if let Some(mut pending) = pending {
            // When: `pending` holds the lock, merge whatever handles the worker published.
            if !pending.is_empty() {
                // When: `pending.is_empty()` is false, merge completed fallback handles before shaping.
                match self.insert_fallback_handles(pending.split_off(0)) {
                    Ok(true) => {
                        // When: `insert_fallback_handles(...)` returns `Ok(true)`, invalidate shapes.
                        return Err(ClearShapeCache {}.into());
                    }
                    Ok(false) => {
                        // When: `insert_fallback_handles(...)` returns `Ok(false)`, keep the shaper.
                    }
                    Err(err) => {
                        log::error!("Error adding fallback: {:#}", err);
                    }
                }
            }
        }

        let result = self.shaper.borrow().shape(
            text,
            self.font_size,
            self.dpi,
            &mut no_glyphs,
            presentation,
            direction,
            range,
            presentation_width,
        );

        no_glyphs.retain(|&c| c != '\u{FE0F}' && c != '\u{FE0E}');
        filter_out_synthetic(&mut no_glyphs);

        let mut tried_glyphs = self.tried_glyphs.borrow_mut();
        no_glyphs.retain(|c| !tried_glyphs.contains(c));
        for c in &no_glyphs {
            tried_glyphs.insert(*c);
        }

        no_glyphs.sort();
        no_glyphs.dedup();

        let mut async_resolve = false;

        if !no_glyphs.is_empty() {
            if let Some(font_config) = self.font_config.upgrade() {
                font_config.schedule_fallback_resolve(
                    no_glyphs,
                    &self.pending_fallback,
                    completion,
                );
                async_resolve = true;
            }
        }

        result.map(|r| (async_resolve, r))
    }

    /// Computes metrics for one resolved fallback index at this font's size and DPI.
    pub fn metrics_for_idx(&self, font_idx: usize) -> anyhow::Result<FontMetrics> {
        self.shaper.borrow().metrics_for_idx(font_idx, self.font_size, self.dpi)
    }

    /// Returns the brightness multiplier used for a resolved font index.
    pub fn brightness_adjust(&self, font_idx: usize) -> f32 {
        let synthesize_dim =
            self.handles.borrow().get(font_idx).map(|p| p.synthesize_dim).unwrap_or(false);
        if synthesize_dim {
            0.5
        } else {
            // When: `synthesize_dim` is false, preserve the glyph's original brightness.
            1.0
        }
    }

    /// Rasterizes a glyph, creating and caching its fallback-specific rasterizer on first use.
    pub fn rasterize_glyph(
        &self,
        glyph_pos: u32,
        fallback: FallbackIdx,
    ) -> anyhow::Result<RasterizedGlyph> {
        let raster_span = diagnostic_timing::enabled().then(|| tracing::debug_span!(
            target: "render_timing", "font_raster", loaded_font_id = self.id, fallback_idx = fallback
        ));
        let _entered = raster_span.as_ref().map(tracing::Span::enter);
        let mut rasterizers = self.rasterizers.borrow_mut();
        if let Some(raster) = rasterizers.get(&fallback) {
            Timing::result("rasterize_glyph", || {
                raster.rasterize_glyph(glyph_pos, self.font_size, self.dpi)
            })
        } else {
            // When: no rasterizer is cached for `fallback`, construct one from its parsed handle.
            let raster_selection = self
                .font_config
                .upgrade()
                .map_or(FontRasterizerSelection::default(), |c| c.config.borrow().font_rasterizer);
            let rasterizer_timing = Timing::begin("rasterizer_new");
            let raster = new_rasterizer(
                raster_selection,
                &(self.handles.borrow())[fallback],
                self.pixel_geometry,
            );
            Timing::finish(rasterizer_timing, if raster.is_ok() { "ok" } else { "error" });
            let raster = raster?;
            let result = Timing::result("rasterize_glyph", || {
                raster.rasterize_glyph(glyph_pos, self.font_size, self.dpi)
            });
            rasterizers.insert(fallback, raster);
            result
        }
    }

    /// Clones the resolved primary and fallback font handles in shaping order.
    pub fn clone_handles(&self) -> Vec<ParsedFont> {
        self.handles.borrow().clone()
    }

    /// Diagnostic only: the data handle of the face at `fallback` and the raster pixel size
    /// [`Self::rasterize_glyph`] requests for it, `font_size × face scale × dpi / 72`, as the
    /// FreeType rasterizer computes it. `None` when no face holds that slot.
    #[doc(hidden)]
    pub fn face_raster_request(
        &self,
        fallback: FallbackIdx,
    ) -> Option<(locator::FontDataHandle, f64)> {
        let handles = self.handles.borrow();
        let parsed = handles.get(fallback)?;
        let raster_px = self.font_size * parsed.scale.unwrap_or(1.0) * f64::from(self.dpi) / 72.0;
        Some((parsed.handle.clone(), raster_px))
    }

    /// Whether the handle at `font_idx` is a font the user configured, as
    /// opposed to one resolved to cover a glyph the configured font lacked.
    ///
    /// Index alone cannot answer this. Resolution pushes a handle only when a
    /// family actually matches, so a configured family that fails to load is
    /// simply absent and the first fallback inherits index 0. A predicate
    /// written as `font_idx == 0` therefore reports "configured" for a font
    /// the user never named, in exactly the case where the distinction matters
    /// most.
    ///
    /// Answered by asking the handle whether it matches any non-fallback entry
    /// of the style this font was resolved from, which is what "the user
    /// configured this" means.
    pub fn is_configured_family(&self, font_idx: usize) -> bool {
        let handles = self.handles.borrow();
        let Some(handle) = handles.get(font_idx) else {
            // When: `font_idx` is outside the resolved handle list, it cannot be configured.
            return false;
        };
        self.text_style
            .font_with_fallback()
            .iter()
            .any(|attr| !attr.is_fallback && handle.matches_name(attr))
    }
}

#[cfg(test)]
#[path = "loaded_font_tests.rs"]
mod loaded_font_tests;
