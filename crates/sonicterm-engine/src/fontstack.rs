//! Renderer-facing adapter over SonicTerm's font discovery, shaping, and
//! rasterization stack.
//!
//! Selected faces become fixed-geometry atlas tiles; weight scales monochrome ink, never color artwork.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Once;

use anyhow::Result;
use config::TextStyle;
use sonicterm_font::{
    rasterizer::{checked_glyph_rgba_len, MAX_RASTERIZED_GLYPH_DIMENSION},
    Direction, FontConfiguration, Presentation,
};
use sonicterm_text::glyph_atlas::{RasterTile, Rasterizer};
use sonicterm_types::glyph_key::GlyphKey;

/// Default primary font family: the host's preferred monospace face, the first of
/// [`config::DEFAULT_MONOSPACE_FAMILIES`]. Matches `sonicterm_cfg::config::DEFAULT_FONT_FAMILY`,
/// which `sonicterm-engine` does not import because it does not depend on `sonicterm-cfg`; a
/// caller that needs another family invokes [`FontStack::try_new_with_family`].
pub const DEFAULT_FONT_FAMILY: &str = config::DEFAULT_MONOSPACE_FAMILIES[0];

/// The fallback chain appended after `primary_family`: every platform default monospace family
/// other than the primary, then the bundled symbols font for Powerline and Nerd Font icons, then
/// Noto Color Emoji. A missing primary therefore takes the host's own text face, and icons reach
/// the bundled font before system fallback.
fn fallback_families(primary_family: &str) -> Vec<&'static str> {
    config::DEFAULT_MONOSPACE_FAMILIES
        .iter()
        .copied()
        .filter(|family| *family != primary_family)
        .chain([config::BUNDLED_SYMBOLS_FAMILY, "Noto Color Emoji"])
        .collect()
}

/// Global `use_this_configuration` install guard. The wezterm `config`
/// crate keeps a process-wide `Configuration` slot read by
/// `FontConfiguration::new(None, ..)`; we install exactly one Config
/// derived from sonicterm preferences on the first FontStack
/// construction. Subsequent calls re-use it.
static INSTALL_ONCE: Once = Once::new();

/// Cell metrics in raster pixels, sourced from the active font stack.
///
/// The renderer's coordinate system is raster
/// pixels end-to-end. [`FontStack::cell_metrics_raster_px`] emits this
/// renderer-friendly view without any `* scale_factor` math.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CellMetricsPx {
    /// Width of a single character cell, raster px.
    pub cell_w: f64,
    /// Height of a single character cell, raster px.
    pub cell_h: f64,
    /// Underline / strikethrough thickness, raster px.
    pub underline_h: f64,
    /// Descender (added to bottom y to find baseline; typically
    /// negative), raster px.
    pub descender: f64,
}

/// Holds a single wezterm `FontConfiguration` keyed to a logical DPI
/// + scale. Multiple sonicterm panes share one stack — sonicterm-font
///   itself caches per-font face state internally.
#[derive(Clone)]
pub struct FontStack {
    font_config: std::rc::Rc<FontConfiguration>,
    font_size_pt: f64,
    weight_scale: f32,
    /// Memoized cell height in raster px, used to size outline growth.
    /// `0.0` means "not yet computed"; invalidated on scaling changes.
    cell_h_px: Cell<f64>,
    /// This stack's resolved face per style, indexed `bold | italic << 1`. Clones and
    /// `with_font_size` views keep their own memo; all of them check `faces_epoch` against the
    /// shared configuration's `face_epoch`, so a replacement made through any sibling is seen.
    faces: RefCell<[Option<std::rc::Rc<sonicterm_font::LoadedFont>>; 4]>,
    /// The configuration's `face_epoch` when `faces` was last filled.
    faces_epoch: Cell<u64>,
}

impl FontStack {
    /// Construct a [`FontStack`] using the platform's default monospace
    /// family ([`DEFAULT_FONT_FAMILY`]) backed by the synthesized fallback
    /// chain. On first call this installs a process-wide font `Config` so that
    /// `FontConfiguration::new(None, dpi)` selects the same primary family.
    pub fn try_new(dpi: usize) -> Result<Self> {
        Self::try_new_full(DEFAULT_FONT_FAMILY, 14.0, dpi)
    }

    /// Construct a [`FontStack`] with `primary_family` as the baseline
    /// `[font] family` setting. The first call to this function (or
    /// [`Self::try_new`]) installs a process-wide font configuration; later
    /// calls still pass an explicit per-stack configuration so live family
    /// changes build a stack for the requested family.
    pub fn try_new_with_family(primary_family: &str, dpi: usize) -> Result<Self> {
        Self::try_new_full(primary_family, 14.0, dpi)
    }

    /// Construct a [`FontStack`] with explicit primary family, point size, and
    /// DPI using native monochrome coverage.
    pub fn try_new_full(primary_family: &str, font_size_pt: f64, dpi: usize) -> Result<Self> {
        Self::try_new_full_with_weight(primary_family, font_size_pt, dpi, 1.0)
    }

    /// Construct a [`FontStack`] with explicit primary family, point size, DPI,
    /// and monochrome coverage scale. Pass `dpi = 72 * scale_factor` so
    /// sonicterm-font's point-size conversion yields raster pixels.
    pub fn try_new_full_with_weight(
        primary_family: &str,
        font_size_pt: f64,
        dpi: usize,
        weight_scale: f32,
    ) -> Result<Self> {
        Self::try_new_full_with_weight_and_font_dirs(
            primary_family,
            font_size_pt,
            dpi,
            weight_scale,
            &[],
        )
    }

    /// Construct a [`FontStack`] with packaged font directories searched before
    /// platform-native discovery and the built-in fallback database.
    ///
    /// Native CoreText, GDI, or Fontconfig lookup remains enabled so a user's
    /// nonbundled configured family and system fallback fonts keep resolving.
    pub fn try_new_full_with_weight_and_font_dirs(
        primary_family: &str,
        font_size_pt: f64,
        dpi: usize,
        weight_scale: f32,
        font_dirs: &[PathBuf],
    ) -> Result<Self> {
        install_default_config(primary_family, font_size_pt);
        let font_config = FontConfiguration::new(
            Some(build_config_with_font_dirs(
                primary_family,
                font_size_pt,
                &fallback_families(primary_family),
                font_dirs,
            )),
            dpi,
        )?;
        Ok(Self {
            font_config: std::rc::Rc::new(font_config),
            font_size_pt,
            weight_scale: sanitize_weight_scale(weight_scale),
            cell_h_px: Cell::new(0.0),
            faces: RefCell::default(),
            faces_epoch: Cell::new(0),
        })
    }

    /// Construct from exact font directories and an explicit style.
    ///
    /// Hidden because this is a deterministic test seam, not user-facing font
    /// discovery. The caller supplies tracked font files/directories and can
    /// mark each family configured or fallback without consulting OS fonts.
    #[doc(hidden)]
    pub fn try_new_with_font_dirs_for_test(
        families: &[(&str, bool)],
        font_dirs: Vec<PathBuf>,
        font_size_pt: f64,
        dpi: usize,
        weight_scale: f32,
    ) -> Result<Self> {
        let mut cfg = config::Config::default_config();
        cfg.font.font = families
            .iter()
            .map(|(family, is_fallback)| config::FontAttributes {
                family: (*family).to_string(),
                is_fallback: *is_fallback,
                ..config::FontAttributes::default()
            })
            .collect();
        cfg.font_size = font_size_pt;
        cfg.font_dirs = font_dirs;
        cfg.font_locator = config::FontLocatorSelection::ConfigDirsOnly;
        cfg.search_font_dirs_for_fallback = true;
        let font_config = FontConfiguration::new(Some(config::ConfigHandle::new(cfg)), dpi)?;
        Ok(Self {
            font_config: std::rc::Rc::new(font_config),
            font_size_pt,
            weight_scale: sanitize_weight_scale(weight_scale),
            cell_h_px: Cell::new(0.0),
            faces: RefCell::default(),
            faces_epoch: Cell::new(0),
        })
    }

    /// Test seam: a stack whose primary family loads from `font_dirs` and whose fallback faces
    /// come only from `locator`, so a test controls when discovery answers.
    #[doc(hidden)]
    pub fn try_new_with_locator_for_test(
        family: &str,
        font_dirs: Vec<PathBuf>,
        locator: std::sync::Arc<dyn sonicterm_font::locator::FontLocator + Send + Sync>,
        font_size_pt: f64,
        dpi: usize,
    ) -> Result<Self> {
        let mut cfg = config::Config::default_config();
        cfg.font.font = vec![config::FontAttributes::new(family)];
        cfg.font_size = font_size_pt;
        cfg.font_dirs = font_dirs;
        cfg.search_font_dirs_for_fallback = false;
        cfg.warn_about_missing_glyphs = false;
        let font_config = FontConfiguration::new_with_locator_for_test(
            config::ConfigHandle::new(cfg),
            dpi,
            locator,
        )?;
        Ok(Self {
            font_config: std::rc::Rc::new(font_config),
            font_size_pt,
            weight_scale: 1.0,
            cell_h_px: Cell::new(0.0),
            faces: RefCell::default(),
            faces_epoch: Cell::new(0),
        })
    }

    /// Test seam: pause points for this stack's fallback worker, used by requests scheduled afterwards.
    #[doc(hidden)]
    pub fn set_fallback_worker_hooks_for_test(&self, hooks: sonicterm_font::FallbackWorkerHooks) {
        self.font_config.set_fallback_worker_hooks_for_test(hooks);
    }

    /// Test seam: run `hook` while this stack's fallback worker holds the pending-handle lock,
    /// after it appends a found face, so a test decides when a face can merge and publish.
    #[doc(hidden)]
    pub fn set_fallback_append_hook_for_test(&self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.set_fallback_worker_hooks_for_test(sonicterm_font::FallbackWorkerHooks {
            during_append: Some(hook),
            ..Default::default()
        });
    }

    /// Test seam: run `scheduled` when this stack queues a fallback request and `ended` on the
    /// worker when that request ends on any path, so a test can wait until none is in flight.
    #[doc(hidden)]
    pub fn set_fallback_request_hooks_for_test(
        &self,
        scheduled: std::sync::Arc<dyn Fn() + Send + Sync>,
        ended: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) {
        self.set_fallback_worker_hooks_for_test(sonicterm_font::FallbackWorkerHooks {
            on_schedule: Some(scheduled),
            after_request: Some(ended),
            ..Default::default()
        });
    }

    /// The fallback notice of this stack's configuration, shared by every `with_font_size` view.
    #[must_use]
    pub fn fallback_notice(&self) -> std::sync::Arc<sonicterm_font::FallbackNotice> {
        self.font_config.fallback_notice()
    }

    /// Create a native-size view that shares this stack's font configuration.
    ///
    /// Size stays part of sonicterm-font's loaded-face cache key, so bitmap
    /// strikes remain correct without duplicating font databases and fallback
    /// infrastructure for every chrome size in every window.
    #[must_use]
    pub fn with_font_size(&self, font_size_pt: f64) -> Self {
        Self {
            font_config: std::rc::Rc::clone(&self.font_config),
            font_size_pt,
            weight_scale: self.weight_scale,
            cell_h_px: Cell::new(0.0),
            faces: RefCell::default(),
            faces_epoch: Cell::new(0),
        }
    }

    /// Whether two stacks share one font configuration and its databases.
    #[doc(hidden)]
    #[must_use]
    pub fn shares_configuration_with(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.font_config, &other.font_config)
    }

    /// Apply a logical font scale and raster DPI, returning the values accepted
    /// by the underlying font configuration.
    pub fn change_scaling(&self, font_scale: f64, dpi: usize) -> (f64, usize) {
        // Cell height is derived from the rasterizer scale, so the memoized
        // value cannot survive a scaling change.
        self.cell_h_px.set(0.0);
        // The configuration drops its faces below and bumps its epoch, which retires every
        // sibling's memo; this stack's own memo is cleared at once as well.
        *self.faces.borrow_mut() = Default::default();
        self.font_config.change_scaling(font_scale, dpi)
    }

    /// Current logical font scale (independent of DPI). Callers changing
    /// only the rasterizer DPI on a scale-factor move should reuse this so
    /// the user's font-scale preference is preserved across the change.
    pub fn get_font_scale(&self) -> f64 {
        self.font_config.get_font_scale()
    }

    /// Shape a regular text run using SonicTerm's current font stack policy.
    ///
    /// This may wait for fallback discovery; frame code uses [`Self::shape_text_for_frame`].
    pub fn shape_text(&self, text: &str) -> Result<Vec<sonicterm_font::shaper::GlyphInfo>> {
        self.shape_text_with_style(text, false, false)
    }

    /// Shape a text run using the face selected for its bold/italic style.
    ///
    /// This may wait for fallback discovery; frame code uses [`Self::shape_text_for_frame`].
    pub fn shape_text_with_style(
        &self,
        text: &str,
        bold: bool,
        italic: bool,
    ) -> Result<Vec<sonicterm_font::shaper::GlyphInfo>> {
        let font = self.font_for_style(bold, italic)?;
        font.blocking_shape(text, Some(Presentation::Text), Direction::LeftToRight, None, None)
    }

    /// Shape a run for a frame: never waits for fallback discovery, so a character whose face
    /// is not yet published shapes as notdef and is scheduled once.
    pub fn shape_text_for_frame(
        &self,
        text: &str,
        bold: bool,
        italic: bool,
    ) -> Result<Vec<sonicterm_font::shaper::GlyphInfo>> {
        let font = self.font_for_style(bold, italic)?;
        font.shape_for_frame(text, Some(Presentation::Text), Direction::LeftToRight, None, None)
    }

    /// Measure a run for a frame without waiting for fallback discovery; an unresolved
    /// character counts notdef's advance until its face is published.
    pub fn measure_text_width_for_frame(&self, text: &str) -> Result<f32> {
        let glyphs = self.shape_text_for_frame(text, false, false)?;
        Ok(glyphs.iter().map(|glyph| glyph.x_advance.get() as f32).sum())
    }

    /// The face for `(bold, italic)` at this stack's size, memoized per style until the shared
    /// configuration's `face_epoch` moves. A fallback merge mutates the shared face in place, so
    /// the memoized `Rc` shapes with merged handles without being resolved again.
    pub(crate) fn font_for_style(
        &self,
        bold: bool,
        italic: bool,
    ) -> Result<std::rc::Rc<sonicterm_font::LoadedFont>> {
        let slot = usize::from(bold) | (usize::from(italic) << 1);
        let epoch = self.font_config.face_epoch();
        if self.faces_epoch.get() != epoch {
            // The configuration dropped its faces since this memo was filled, so every
            // memoized face is stale, so all four slots are cleared before the lookup.
            *self.faces.borrow_mut() = Default::default();
            self.faces_epoch.set(epoch);
        }
        if let Some(face) = &self.faces.borrow()[slot] {
            // When: this style was resolved in the current epoch, the shared face is returned.
            return Ok(std::rc::Rc::clone(face));
        }
        let mut style: TextStyle = self.font_config.config().font.clone();
        if bold {
            style = style.make_bold();
        }
        if italic {
            style = style.make_italic();
        }
        let face = self.font_config.resolve_font_at_size(&style, self.font_size_pt)?;
        self.faces.borrow_mut()[slot] = Some(std::rc::Rc::clone(&face));
        Ok(face)
    }

    /// Measure a left-to-right text run in raster pixels using the same
    /// fallback-font shaping policy as the renderer.
    ///
    /// This may wait for fallback discovery; frame code uses
    /// [`Self::measure_text_width_for_frame`].
    pub fn measure_text_width(&self, text: &str) -> Result<f32> {
        let glyphs = self.shape_text(text)?;
        Ok(glyphs.iter().map(|glyph| glyph.x_advance.get() as f32).sum())
    }

    /// Resolve a family to its exact handle index in this stack.
    ///
    /// Hidden test seam for provenance regression tests that must address a
    /// fallback handle directly even when the primary also covers the glyph.
    #[doc(hidden)]
    pub fn font_index_for_test(&self, family: &str) -> Result<usize> {
        let font = self.font_for_style(false, false)?;
        let attr = config::FontAttributes::new(family);
        font.clone_handles()
            .iter()
            .position(|handle| handle.matches_name(&attr))
            .ok_or_else(|| anyhow::anyhow!("tracked font fixture {family:?} did not resolve"))
    }

    /// Shape a glyph directly with a tracked family and return its glyph id.
    #[doc(hidden)]
    pub fn glyph_id_for_family_for_test(&self, family: &str, character: char) -> Result<u32> {
        let style = TextStyle { font: vec![config::FontAttributes::new(family)], foreground: None };
        self.font_config
            .resolve_font_at_size(&style, self.font_size_pt)?
            .blocking_shape(
                &character.to_string(),
                Some(Presentation::Text),
                Direction::LeftToRight,
                None,
                None,
            )?
            .into_iter()
            .find(|glyph| glyph.glyph_pos != 0)
            .map(|glyph| glyph.glyph_pos)
            .ok_or_else(|| anyhow::anyhow!("tracked font fixture {family:?} lacks {character:?}"))
    }

    /// Cap height of the default font at this stack's size, in raster px: the ink height of a
    /// capital, as the font crate measured it when it selected the size. `None` when the font
    /// gives none (no `I` to measure) or the default font fails to load.
    pub fn cap_height_raster_px(&self) -> Option<f64> {
        let metrics = self.font_config.default_font_metrics_at_size(self.font_size_pt).ok()?;
        metrics.cap_height.map(|height| height.get()).filter(|height| *height > 0.0)
    }

    /// Return cell metrics for the default font, projected into the
    /// renderer-facing [`CellMetricsPx`] (raster px). Wezterm's
    /// `FontMetrics` already lives in raster px, so this is a plain
    /// field extraction — no `* scale_factor` multiplier here, and
    /// none at the call site.
    ///
    /// Errors when sonicterm-font fails to load the default font (e.g.
    /// no installed fallback covers the configured family). Callers
    /// in the hot path should propagate; tests can `unwrap` once
    /// they've confirmed sonicterm-font picked something up.
    pub fn cell_metrics_raster_px(&self) -> Result<CellMetricsPx> {
        let metrics = self.font_config.default_font_metrics_at_size(self.font_size_pt)?;
        Ok(CellMetricsPx {
            cell_w: metrics.cell_width.get(),
            cell_h: metrics.cell_height.get(),
            underline_h: metrics.underline_thickness.get(),
            descender: metrics.descender.get(),
        })
    }
}

/// A loaded face's stable identity: its file path (canonical when it resolves) or the name of
/// built-in or in-memory data, and its index within a collection. Unlike a font slot, it does not
/// depend on the order a configuration resolved its faces in.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FaceIdentity {
    /// Canonical file path, or `builtin:<name>` / `memory:<name>` for data not on disk.
    pub source: String,
    /// Index of the face within its collection file.
    pub face_index: u32,
}

/// Diagnostic only: what a glyph key rasterizes from, resolved exactly as the rasterizer does.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedGlyphFace {
    /// The face the glyph is drawn from.
    pub face: FaceIdentity,
    /// The glyph id inside that face.
    pub glyph_id: u32,
    /// The raster pixel size requested for that face, in thousandths of a pixel. Strike
    /// selection is a pure function of the face and this size, so equal sizes select one strike.
    pub strike_px_milli: u64,
}

impl FaceIdentity {
    fn of(handle: &sonicterm_font::locator::FontDataHandle) -> Self {
        use sonicterm_font::locator::FontDataSource;
        let source = match &handle.source {
            FontDataSource::OnDisk(path) => {
                // A face found through two directory spellings is still one file.
                std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()).display().to_string()
            }
            FontDataSource::BuiltIn { name, .. } => format!("builtin:{name}"),
            FontDataSource::Memory { name, .. } => format!("memory:{name}"),
        };
        Self { source, face_index: handle.index }
    }
}

impl FontStack {
    /// The loaded face for `key`'s style, its fallback slot and its glyph id, resolved as the
    /// rasterizer resolves them; `None` when the character has no published face yet.
    fn resolve_key(
        &self,
        key: GlyphKey,
    ) -> Option<(std::rc::Rc<sonicterm_font::LoadedFont>, usize, u32)> {
        let font = self.font_for_style(key.weight_bold, key.italic).ok()?;
        if key.glyph_id != 0 {
            // When: `glyph_id` is set the key already names its slot and glyph.
            return Some((font, key.font_slot as usize, key.glyph_id));
        }
        // A zero glyph id shapes `ch` for this frame; an unresolved character returns `None` at
        // once, which the atlas caches as missing until its face is published.
        let text = key.ch.to_string();
        let infos = font
            .shape_for_frame(&text, Some(Presentation::Text), Direction::LeftToRight, None, None)
            .ok()?;
        let first = infos.into_iter().find(|glyph| glyph.glyph_pos != 0)?;
        Some((font, first.font_idx, first.glyph_pos))
    }

    /// Diagnostic only: the face, glyph id and strike `key` rasterizes from in this stack, by the
    /// rasterizer's own resolution. `None` when the key does not resolve to a face.
    #[doc(hidden)]
    #[must_use]
    pub fn resolved_glyph_face(&self, key: GlyphKey) -> Option<ResolvedGlyphFace> {
        let (font, font_idx, glyph_id) = self.resolve_key(key)?;
        let (handle, raster_px) = font.face_raster_request(font_idx)?;
        Some(ResolvedGlyphFace {
            face: FaceIdentity::of(&handle),
            glyph_id,
            strike_px_milli: (raster_px * 1000.0).round() as u64,
        })
    }
}

impl FontStack {
    /// Diagnostic only: the content identity of the face `key` rasterizes from, the SHA-256 of the
    /// bytes it was loaded from in the namespace of their source (file, built-in or memory data).
    /// `None` when the key resolves to no face or its bytes cannot be read, so it names nothing.
    #[doc(hidden)]
    #[must_use]
    pub fn resolved_face_content(&self, key: GlyphKey) -> Option<String> {
        use sonicterm_font::locator::FontDataSource;
        use sonicterm_text::face_content::{face_content_id, FaceNamespace};
        let (font, font_idx, _) = self.resolve_key(key)?;
        let (handle, _) = font.face_raster_request(font_idx)?;
        let namespace = match &handle.source {
            FontDataSource::OnDisk(_) => FaceNamespace::File,
            FontDataSource::BuiltIn { .. } => FaceNamespace::BuiltIn,
            FontDataSource::Memory { .. } => FaceNamespace::Memory,
        };
        // An unreadable file names no content, so no exception can approve its failure.
        let bytes = handle.source.load_data().ok()?;
        Some(face_content_id(namespace, &bytes))
    }
}

impl Rasterizer for FontStack {
    fn rasterize(&mut self, key: GlyphKey) -> Option<RasterTile> {
        let (font, font_idx, glyph_pos) = self.resolve_key(key)?;
        let rasterized = font.rasterize_glyph(glyph_pos, font_idx).ok()?;
        self.rasterized_glyph_to_tile(rasterized)
    }
}

impl FontStack {
    fn rasterized_glyph_to_tile(
        &self,
        rasterized: sonicterm_font::RasterizedGlyph,
    ) -> Option<RasterTile> {
        if rasterized.width == 0 || rasterized.height == 0 {
            // When: `rasterized.width` or `rasterized.height` is 0, the glyph has no coverage to build.
            if !rasterized.data.is_empty() {
                // When: a zero-area raster's `data` still carries bytes, the buffer is malformed.
                log::warn!(
                    "font rasterizer returned invalid {}x{} glyph buffer: {} bytes, expected 0",
                    rasterized.width,
                    rasterized.height,
                    rasterized.data.len()
                );
                return None;
            }
            // A valid empty glyph, such as a space: an empty tile with the glyph's bearings, so
            // the atlas keeps it apart from a glyph that could not be resolved.
            return Some(RasterTile {
                width: rasterized.width as u32,
                height: rasterized.height as u32,
                offset_x: rasterized.bearing_x.get() as i32,
                offset_y: -rasterized.bearing_y.get() as i32,
                advance: rasterized.width as f32,
                coverage: Vec::new(),
                is_color: rasterized.has_color,
                is_subpixel: false,
            });
        }
        let expected_len = checked_glyph_rgba_len(rasterized.width, rasterized.height).ok()?;
        if rasterized.data.len() != expected_len {
            // When: `rasterized.data.len()` differs from `expected_len`, reject malformed coverage before conversion or upload.
            log::warn!(
                "font rasterizer returned invalid {}x{} glyph buffer: {} bytes, expected {}",
                rasterized.width,
                rasterized.height,
                rasterized.data.len(),
                expected_len
            );
            return None;
        }
        let (mut coverage, is_color, is_subpixel) = if rasterized.has_color {
            let mut bgra = Vec::with_capacity(rasterized.data.len());
            for px in rasterized.data.as_chunks::<4>().0.iter() {
                bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
            (bgra, true, false)
        } else {
            // When: `has_color` is false, derive monochrome or subpixel coverage from the raster channels.
            let has_subpixel_coverage = rasterized
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[0] != px[1] || px[1] != px[2]);
            if has_subpixel_coverage {
                let mut bgra = Vec::with_capacity(rasterized.data.len());
                for px in rasterized.data.as_chunks::<4>().0.iter() {
                    bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
                (bgra, false, true)
            } else {
                // When: `has_subpixel_coverage` is false, one alpha mask replaces four redundant channel bytes.
                let mask: Vec<u8> =
                    rasterized.data.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
                (mask, false, false)
            }
        };
        let tile_w = rasterized.width;
        let tile_h = rasterized.height;
        let offset_x = rasterized.bearing_x.get() as i32;
        let offset_y = -rasterized.bearing_y.get() as i32;
        if !is_color {
            // All monochrome styles and fallback faces share this fixed-geometry adjustment.
            apply_weight_scale(&mut coverage, self.weight_scale, is_subpixel);
            // The coverage remap alone cannot thicken a stem whose core is
            // already fully opaque, which is the common case at HiDPI. Growing
            // the outline is what makes weight_scale visible there. The same
            // ceiling applies to thinning, so below 1.0 the outline shrinks.
            let cell_h = self.cell_h_px();
            let radius = embolden_radius_px(self.weight_scale, cell_h);
            if let Some((grown, grown_width, grown_height, pad)) =
                embolden_coverage(&coverage, tile_w, tile_h, radius, is_subpixel)
            {
                debug_assert_eq!((grown_width, grown_height, pad), (tile_w, tile_h, 0));
                coverage = grown;
            }
            let thin = thin_radius_px(self.weight_scale, cell_h);
            if let Some(eroded) = erode_coverage(&coverage, tile_w, tile_h, thin, is_subpixel) {
                // Erosion only removes ink, so dimensions and offsets hold.
                coverage = eroded;
            }
        }
        Some(RasterTile {
            width: tile_w as u32,
            height: tile_h as u32,
            offset_x,
            offset_y,
            // Advance stays keyed to the original bitmap. Emboldening adds ink
            // around the glyph but must not shift the cell grid.
            advance: rasterized.width as f32,
            coverage,
            is_color,
            is_subpixel,
        })
    }
}

impl FontStack {
    /// Cell height in raster px, memoized. Returns `0.0` when metrics cannot
    /// be resolved, which disables outline growth rather than guessing a size.
    fn cell_h_px(&self) -> f64 {
        let cached = self.cell_h_px.get();
        if cached > 0.0 {
            // When: positive `cached` metrics remain valid until `change_scaling` explicitly invalidates them.
            return cached;
        }
        let resolved = self.cell_metrics_raster_px().map(|metrics| metrics.cell_h).unwrap_or(0.0);
        self.cell_h_px.set(resolved);
        resolved
    }
}

/// Install the sonicterm-derived wezterm `Config` into the process-wide
/// `Configuration` slot exactly once. Idempotent — subsequent invocations
/// (even with a different `primary_family`) are no-ops; reconfiguring the
/// font at runtime is tracked separately and would need a `change_scaling`
/// / `config_changed` round-trip through every live `FontConfiguration`.
fn install_default_config(primary_family: &str, font_size_pt: f64) {
    INSTALL_ONCE.call_once(|| {
        sonicterm_font::use_sonic_font_configuration(
            primary_family,
            font_size_pt,
            &fallback_families(primary_family),
        );
    });
}

fn build_config_with_font_dirs(
    primary_family: &str,
    font_size_pt: f64,
    fallback_families: &[&str],
    font_dirs: &[PathBuf],
) -> config::ConfigHandle {
    let mut cfg = config::Config::default_config();
    let mut font_attrs = Vec::with_capacity(1 + fallback_families.len());
    font_attrs.push(config::FontAttributes::new(primary_family));
    for fam in fallback_families {
        font_attrs.push(config::FontAttributes::new_fallback(fam));
    }
    cfg.font = config::TextStyle { font: font_attrs, foreground: None };
    cfg.font_size = font_size_pt;
    cfg.font_dirs = font_dirs.to_vec();
    config::ConfigHandle::new(cfg)
}

#[cfg(test)]
#[path = "fontstack_tests.rs"]
mod fontstack_tests;

#[path = "fontstack_weight.rs"]
mod weight;

use weight::{
    apply_weight_scale, embolden_coverage, embolden_radius_px, erode_coverage,
    sanitize_weight_scale, thin_radius_px,
};
