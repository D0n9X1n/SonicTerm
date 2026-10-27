//! Configured font-chain matching, entity fonts and style/size-specific face construction.

use super::*;

impl FontConfigInner {
    pub(super) fn compute_title_font(
        &self,
        config: &ConfigHandle,
        make_bold: bool,
    ) -> (TextStyle, f64) {
        fn bold(family: &str) -> FontAttributes {
            FontAttributes {
                family: family.to_string(),
                weight: FontWeight::BOLD,
                ..Default::default()
            }
        }

        let mut fonts =
            vec![if make_bold { bold("Roboto") } else { FontAttributes::new("Roboto") }];

        // Fallback to their main font selection, so that we can pick up
        // any fallback fonts they might have configured in the main
        // config and so that they don't have to replicate that list for
        // the title font.
        for font in &config.font.font {
            let mut font = font.clone();
            font.is_fallback = true;
            fonts.push(font);
        }

        let font_size = if cfg!(windows) {
            // When: `cfg!(windows)` is true, use the Windows chrome font size.
            10.
        } else {
            // When: `cfg!(windows)` is false, use the non-Windows chrome font size.
            12.
        };

        (TextStyle { foreground: None, font: fonts }, font_size)
    }

    pub(super) fn make_entity_font_impl(
        &self,
        myself: &Rc<Self>,
        entity: Entity,
    ) -> anyhow::Result<Rc<LoadedFont>> {
        let config = self.config.borrow();
        let make_bold = entity != Entity::CommandPalette;
        let (sys_font, sys_size) = self.compute_title_font(&config, make_bold);

        let (font_size, text_style) = match entity {
            Entity::Title => (config.window_frame.font_size.unwrap_or(sys_size), None),
            Entity::CommandPalette => {
                (config.command_palette_font_size, config.command_palette_font.as_ref())
            }
            Entity::CharSelect => (config.char_select_font_size, config.char_select_font.as_ref()),
            Entity::PaneSelect => (config.pane_select_font_size, config.pane_select_font.as_ref()),
        };

        let text_style =
            text_style.unwrap_or(config.window_frame.font.as_ref().unwrap_or(&sys_font));

        let dpi = *self.dpi.borrow() as u32;
        let pixel_size = (font_size * dpi as f64 / 72.0) as u16;

        let attributes = text_style.font_with_fallback();
        let (handles, _loaded) = self.resolve_font_helper_impl(&attributes, pixel_size)?;

        let shaper = new_shaper(&config, &handles, handles.len())?;

        let metrics = shaper.metrics(font_size, dpi).with_context(|| {
            format!("obtaining metrics for font_size={} @ dpi {}", font_size, dpi)
        })?;

        let configured_handle_count = handles.len();
        let loaded = Rc::new(LoadedFont {
            rasterizers: RefCell::new(HashMap::new()),
            handles: RefCell::new(handles),
            configured_handle_count,
            shaper: RefCell::new(shaper),
            metrics,
            font_size,
            dpi,
            font_config: Rc::downgrade(myself),
            pending_fallback: Arc::new(Mutex::new(vec![])),
            text_style: text_style.clone(),
            id: alloc_font_id(),
            tried_glyphs: RefCell::new(HashSet::new()),
            pixel_geometry: config.display_pixel_geometry,
        });

        Ok(loaded)
    }

    pub(super) fn title_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut title_font = self.title_font.borrow_mut();

        if let Some(entry) = title_font.as_ref() {
            // When: `title_font` is cached, return its shared handle without resolving again.
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::Title)?;

        title_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    pub(super) fn command_palette_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut command_palette_font = self.command_palette_font.borrow_mut();

        if let Some(entry) = command_palette_font.as_ref() {
            // When: `command_palette_font` is cached, reuse it without resolving again.
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::CommandPalette)?;

        command_palette_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    pub(super) fn char_select_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut char_select_font = self.char_select_font.borrow_mut();

        if let Some(entry) = char_select_font.as_ref() {
            // When: `char_select_font` is cached, reuse it without resolving again.
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::CharSelect)?;

        char_select_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    pub(super) fn pane_select_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut pane_select_font = self.pane_select_font.borrow_mut();

        if let Some(entry) = pane_select_font.as_ref() {
            // When: `pane_select_font` is cached, reuse it without resolving again.
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::PaneSelect)?;

        pane_select_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    pub(super) fn resolve_font_helper_impl(
        &self,
        attributes: &[FontAttributes],
        pixel_size: u16,
    ) -> anyhow::Result<(Vec<ParsedFont>, HashSet<FontAttributes>)> {
        let preferred_attributes =
            attributes.iter().filter(|a| !a.is_fallback).cloned().collect::<Vec<_>>();
        let fallback_attributes =
            attributes.iter().filter(|a| a.is_fallback).cloned().collect::<Vec<_>>();
        let mut loaded = HashSet::new();
        let mut handles = vec![];

        for &attrs in &[&preferred_attributes, &fallback_attributes] {
            let mut candidates = vec![];

            let font_dirs = self.font_dirs.borrow();
            for attr in attrs {
                candidates.append(&mut font_dirs.candidates(attr));
            }

            let mut loaded_ignored = HashSet::new();
            let located = self.locator.load_fonts(attrs, &mut loaded_ignored, pixel_size)?;
            for font in &located {
                candidates.push(font);
            }

            let built_in = self.built_in.borrow();
            for attr in attrs {
                candidates.append(&mut built_in.candidates(attr));
            }

            let mut is_fallback = false;

            for attr in attrs {
                if attr.is_fallback {
                    is_fallback = true;
                }

                if loaded.contains(attr) {
                    // When: `loaded` already contains this attribute, avoid a duplicate handle.
                    continue;
                }
                let named_candidates: Vec<&ParsedFont> = candidates
                    .iter()
                    .filter_map(|&p| {
                        if p.matches_name(attr) {
                            Some(p)
                        } else {
                            // When: `p.matches_name(attr)` is false, exclude this named candidate.
                            None
                        }
                    })
                    .collect();
                if let Some(idx) =
                    ParsedFont::best_matching_index(attr, &named_candidates, pixel_size)
                {
                    if let Some(&p) = named_candidates.get(idx) {
                        loaded.insert(attr.clone());
                        handles.push(p.clone().synthesize(attr));
                    }
                }
            }

            if !is_fallback && loaded.is_empty() {
                // We didn't explicitly match any names.
                // When using fontconfig, the system may have expanded a family name
                // like "monospace" into the real font, in which case we wouldn't have
                // found a match in the `named_candidates` vec above, because of the
                // name mismatch.
                // So what we do now is make a second pass over all the located candidates,
                // ignoring their names, and just match based on font attributes.
                let located_candidates: Vec<_> = located.iter().collect();
                for attr in attrs {
                    if let Some(idx) =
                        ParsedFont::best_matching_index(attr, &located_candidates, pixel_size)
                    {
                        if let Some(&p) = located_candidates.get(idx) {
                            loaded.insert(attr.clone());
                            handles.push(p.clone().synthesize(attr));
                        }
                    }
                }
            }
        }

        Ok((handles, loaded))
    }

    pub(super) fn resolve_font_helper(
        &self,
        style: &TextStyle,
        config: &ConfigHandle,
        pixel_size: u16,
    ) -> anyhow::Result<(Box<dyn FontShaper>, Vec<ParsedFont>)> {
        let attributes = style.font_with_fallback();

        let (handles, loaded) = self.resolve_font_helper_impl(&attributes, pixel_size)?;
        // The first loaded face supplies text and cell metrics, so it is the one a miss falls back to.
        let replacement = match handles.first() {
            Some(face) => format!("Using {:?} instead", face.names().family),
            None => "No replacement face loaded".to_string(),
        };

        for attr in &attributes {
            if attr.is_synthetic || attr.is_fallback {
                // When: `attr` is synthetic or a fallback, a miss there is expected and never reported.
                continue;
            }
            let using = if !loaded.contains(attr) {
                replacement.clone()
            } else if let Some(face) = fontconfig_substitute(attr, &handles) {
                // When: `fontconfig_substitute` finds another family under `attr`'s name, it is not installed.
                format!("Using {:?} instead", face.names().family)
            } else {
                // When: `attr` loaded under its own name, there is no miss to report.
                continue;
            };
            let identity = format!(
                "{:?} (weight={}, stretch={}, style={})",
                attr.family, attr.weight, attr.stretch, attr.style
            );
            if config::DEFAULT_MONOSPACE_FAMILIES.contains(&attr.family.as_str()) {
                // When: `DEFAULT_MONOSPACE_FAMILIES` contains `attr.family`, the next default stands in; log at info, not as an error.
                let message =
                    format!("Platform default font {identity} is not installed. {using}.");
                if first_report(&message) {
                    log::info!("{message}");
                }
                continue;
            }
            let is_primary = config.font.font.iter().any(|a| !a.is_fallback && a == attr);
            let derived_from_primary =
                config.font.font.iter().any(|a| !a.is_fallback && a.family == attr.family);
            let explanation = if is_primary {
                format!("Unable to load the configured primary font {identity}")
            } else if derived_from_primary {
                // When: derived_from_primary matches a non-fallback family, identify the unmatched variant without blaming a fallback.
                format!(
                        "Unable to load a font variant derived from the configured primary font: {identity}"
                    )
            } else {
                // When: neither is_primary nor derived_from_primary matches, the request is not the configured primary selection.
                format!("Unable to load the requested font {identity}")
            };

            let message = format!(
                    "{explanation}. {using}, and text may not render as intended. \
                     Check [font].family in sonicterm.toml and ensure the font is available \
                     to SonicTerm. See \
                     https://github.com/D0n9X1n/SonicTerm/wiki/Configuration for font configuration."
                );
            if first_report(&message) {
                config::show_error(&message);
            }
        }

        Ok((new_shaper(config, &handles, handles.len())?, handles))
    }

    /// Given a text style, load (with caching) the font that best
    /// matches according to the fontconfig pattern.
    pub(super) fn resolve_font(
        &self,
        myself: &Rc<Self>,
        style: &TextStyle,
    ) -> anyhow::Result<Rc<LoadedFont>> {
        let font_size = self.config.borrow().font_size;
        self.resolve_font_at_size(myself, style, font_size)
    }

    pub(super) fn resolve_font_at_size(
        &self,
        myself: &Rc<Self>,
        style: &TextStyle,
        font_size: f64,
    ) -> anyhow::Result<Rc<LoadedFont>> {
        let effective_size = font_size * *self.font_scale.borrow();
        self.resolve_font_at_effective_size(myself, style, effective_size)
    }

    pub(super) fn resolve_font_at_effective_size(
        &self,
        myself: &Rc<Self>,
        style: &TextStyle,
        requested_size: f64,
    ) -> anyhow::Result<Rc<LoadedFont>> {
        let key = LoadedFontKey::new(style, requested_size);
        if let Some(entry) = self.fonts.borrow().get(&key) {
            // When: `fonts` contains `key`, reuse the face whose style and native size both match.
            return Ok(Rc::clone(entry));
        }

        let config = self.config.borrow().clone();
        let is_default = *style == config.font;
        let def_font = if !is_default && config.use_cap_height_to_scale_fallback_fonts {
            Some(self.resolve_font_at_effective_size(myself, &config.font, requested_size)?)
        } else {
            // When: `!is_default && config.use_cap_height_to_scale_fallback_fonts` is false,
            // avoid resolving the default font because baseline scaling is not requested.
            None
        };

        let mut font_size = requested_size;
        let dpi = *self.dpi.borrow() as u32;
        let pixel_size = (font_size * dpi as f64 / 72.0) as u16;

        let (mut shaper, mut handles) = Timing::result("resolve_font_helper", || {
            self.resolve_font_helper(style, &config, pixel_size)
        })?;

        let mut metrics = Timing::result("font_metrics", || shaper.metrics(font_size, dpi))
            .with_context(|| {
                format!("obtaining metrics for font_size={} @ dpi {}", font_size, dpi)
            })?;

        if let Some(def_font) = def_font {
            let def_metrics = def_font.metrics();
            if let (Some(d), Some(m)) = (def_metrics.cap_height, metrics.cap_height) {
                // Scale by the ratio of the pixel heights of the default
                // and this font; this causes the `I` glyphs to appear to
                // have the same height.
                let scale = d.get() / m.get();
                if scale != 1.0 {
                    let scaled_pixel_size = (pixel_size as f64 * scale) as u16;
                    let scaled_font_size = font_size * scale;
                    log::trace!(
                        "using cap height adjusted: pixel_size {} -> {}, font_size {} -> {}, {:?}",
                        pixel_size,
                        scaled_pixel_size,
                        font_size,
                        scaled_font_size,
                        metrics,
                    );
                    let (alt_shaper, alt_handles) =
                        Timing::result("resolve_scaled_font_helper", || {
                            self.resolve_font_helper(style, &config, scaled_pixel_size)
                        })?;
                    shaper = alt_shaper;
                    handles = alt_handles;

                    metrics = Timing::result("scaled_font_metrics", || {
                        shaper.metrics(scaled_font_size, dpi)
                    })
                    .with_context(|| {
                        format!(
                            "obtaining cap-height adjusted metrics for font_size={} @ dpi {}",
                            scaled_font_size, dpi
                        )
                    })?;

                    font_size = scaled_font_size;
                }
            }
        }

        let configured_handle_count = handles.len();
        let loaded = Rc::new(LoadedFont {
            rasterizers: RefCell::new(HashMap::new()),
            handles: RefCell::new(handles),
            configured_handle_count,
            shaper: RefCell::new(shaper),
            metrics,
            font_size,
            dpi,
            font_config: Rc::downgrade(myself),
            pending_fallback: Arc::new(Mutex::new(vec![])),
            text_style: style.clone(),
            id: alloc_font_id(),
            tried_glyphs: RefCell::new(HashSet::new()),
            pixel_geometry: config.display_pixel_geometry,
        });

        self.fonts.borrow_mut().insert(key, Rc::clone(&loaded));

        Ok(loaded)
    }
}

#[cfg(test)]
#[path = "font_resolution_tests.rs"]
mod font_resolution_tests;
