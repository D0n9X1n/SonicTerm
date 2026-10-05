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

/// Default primary font family. Matches `sonicterm_cfg::DEFAULT_FONT_FAMILY`
/// — the brand default the project ships with. Duplicated here (rather
/// than imported) because `sonicterm-engine` deliberately does not depend
/// on `sonicterm-cfg`; if a caller needs to override the family it should
/// invoke [`FontStack::try_new_with_family`].
pub const DEFAULT_FONT_FAMILY: &str = "Rec Mono St.Helens";

/// Synthesized fallback chain appended after the user's primary family.
/// Order matters: JetBrains Mono first (bundled by sonicterm-font itself,
/// always resolvable), then Symbols Nerd Font Mono for Powerline / Nerd
/// Font PUA glyphs the primary may lack, then Noto Color Emoji as the
/// last-resort color fallback.
const FALLBACK_FAMILIES: &[&str] =
    &["JetBrains Mono", "Symbols Nerd Font Mono", "Noto Color Emoji"];

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
    /// Construct a [`FontStack`] using the project's default font
    /// family (`DEFAULT_FONT_FAMILY` — "Rec Mono St.Helens") backed by
    /// the synthesized `FALLBACK_FAMILIES` chain. On first call this
    /// installs a process-wide wezterm `Config` so that
    /// `FontConfiguration::new(None, dpi)` selects sonicterm's primary
    /// family instead of sonicterm-font's bundled JetBrains Mono default.
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
                FALLBACK_FAMILIES,
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
    fn font_for_style(
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

fn sanitize_weight_scale(scale: f32) -> f32 {
    if scale.is_finite() && (0.5..=5.0).contains(&scale) {
        scale
    } else {
        // When: `scale` is nonfinite or outside the supported range, identity avoids pathological coverage math.
        1.0
    }
}

/// Glyph outline growth per unit of `weight_scale` above 1.0, expressed as a
/// fraction of the cell height. Tuned so `weight_scale = 2.0` adds roughly
/// half a pixel of radius at a 13pt Retina cell.
const EMBOLDEN_RADIUS_PER_CELL_H: f64 = 0.02;

/// Raster-space growth ceiling while the output stays inside the original tile.
///
/// Larger bitmap dilations flatten counters and saturate the tile edge after
/// crop-back. The ceiling is deliberately independent of the glyph's existing
/// margins: flat-sided glyphs often touch one bitmap edge, and consulting spare
/// margin would disable growth for those while still growing curved glyphs.
const MAX_EMBOLDEN_RADIUS_PX: f64 = 1.0;

/// Radius, in raster px, that monochrome text should grow at `scale`. Zero at or
/// below `1.0`, where [`thin_radius_px`] takes over instead.
fn embolden_radius_px(scale: f32, cell_h: f64) -> f64 {
    if scale <= 1.0 || !cell_h.is_finite() || cell_h <= 0.0 {
        // When: `scale` does not request growth or `cell_h` is unusable, disable emboldening instead of guessing a radius.
        return 0.0;
    }
    f64::from(scale - 1.0) * cell_h * EMBOLDEN_RADIUS_PER_CELL_H
}

/// Outline shrink per unit of `weight_scale` below 1.0, as a fraction of cell
/// height. Slightly larger per unit than [`EMBOLDEN_RADIUS_PER_CELL_H`]
/// because thinning only has 0.5 of range to work with (0.5..1.0) against
/// emboldening's 4.0. Kept low enough that a small thin such as `0.9` stays a
/// nudge and leaves stem cores opaque rather than washing the glyph out.
const THIN_RADIUS_PER_CELL_H: f64 = 0.012;

/// Radius, in raster px, that monochrome text should shrink at `scale`. Zero at
/// or above `1.0`. Like emboldening, this exists because the coverage remap
/// cannot move a pixel that is already fully opaque — at HiDPI a stem core is
/// solid, so gamma alone leaves the stem exactly as wide as it started.
fn thin_radius_px(scale: f32, cell_h: f64) -> f64 {
    if scale >= 1.0 || !cell_h.is_finite() || cell_h <= 0.0 {
        // When: `scale` does not request thinning or `cell_h` is unusable, disable erosion instead of guessing a radius.
        return 0.0;
    }
    f64::from(1.0 - scale) * cell_h * THIN_RADIUS_PER_CELL_H
}

fn checked_coverage_len(width: usize, height: usize, channels: usize) -> Option<usize> {
    if width == 0
        || height == 0
        || width > MAX_RASTERIZED_GLYPH_DIMENSION
        || height > MAX_RASTERIZED_GLYPH_DIMENSION
    {
        // When: `width` or `height` is zero or exceeds the raster limit, no legal allocation exists.
        return None;
    }
    width.checked_mul(height)?.checked_mul(channels)
}

/// Shrink `coverage` inward by `radius` px in place. Unlike growth, erosion
/// never needs padding — the glyph only loses ink — so tile dimensions and
/// offsets are unchanged and the caller can swap the buffer straight in.
///
/// Returns `None` when there is nothing to do.
fn erode_coverage(
    coverage: &[u8],
    width: usize,
    height: usize,
    radius: f64,
    is_subpixel: bool,
) -> Option<Vec<u8>> {
    if radius <= 0.0 || width == 0 || height == 0 {
        // When: `radius` or tile dimensions are nonpositive, erosion has no valid work to perform.
        return None;
    }
    let channels = if is_subpixel { 4 } else { 1 };
    let byte_len = checked_coverage_len(width, height, channels)?;
    if coverage.len() != byte_len {
        // When: `coverage.len()` differs from `byte_len`, morphology would index outside the supplied tile.
        return None;
    }
    let pixel_len = width.checked_mul(height)?;
    let mut out = vec![0u8; byte_len];
    for channel in 0..channels {
        let mut plane = vec![0u8; pixel_len];
        for pixel in 0..pixel_len {
            plane[pixel] = coverage[pixel * channels + channel];
        }
        let mut tmp = vec![0u8; pixel_len];
        morph_axis(&plane, &mut tmp, width, height, width, radius, true);
        let mut transposed = vec![0u8; pixel_len];
        for row in 0..height {
            for column in 0..width {
                transposed[column * height + row] = tmp[row * width + column];
            }
        }
        let mut tcol = vec![0u8; pixel_len];
        morph_axis(&transposed, &mut tcol, height, width, height, radius, true);
        for row in 0..height {
            for column in 0..width {
                out[(row * width + column) * channels + channel] = tcol[column * height + row];
            }
        }
    }
    if is_subpixel {
        for px in out.as_chunks_mut::<4>().0 {
            px[3] = px[0].max(px[1]).max(px[2]);
        }
    }
    Some(out)
}

/// One separable morphology pass with fractional radius. `radius` is split
/// into an integer core, taken at full strength, and a fractional outer ring
/// that is blended in proportionally so the change is smooth rather than
/// snapping a whole pixel at a time.
///
/// `erode` selects the operator: max-filter (grow) when false, min-filter
/// (shrink) when true. The two differ at the boundary — a max-filter ignores
/// out-of-bounds samples, while a min-filter must treat them as empty so the
/// glyph erodes inward from its own edge.
fn morph_axis(
    src: &[u8],
    dst: &mut [u8],
    len: usize,
    count: usize,
    stride: usize,
    radius: f64,
    erode: bool,
) {
    let whole = radius.floor() as usize;
    let frac = radius - radius.floor();
    for line in 0..count {
        let base = line * stride;
        for position in 0..len {
            let low = position.saturating_sub(whole);
            let high = (position + whole).min(len - 1);
            if erode {
                // A window that overhangs the tile edge sees empty space
                // there, so the glyph erodes inward from its own rim. Only
                // genuinely out-of-bounds samples count as empty — treating
                // in-bounds neighbours as empty would erode the whole glyph
                // rather than its edge.
                let mut best =
                    if position < whole || position + whole >= len { 0u8 } else { u8::MAX };
                for neighbor in low..=high {
                    best = best.min(src[base + neighbor]);
                }
                if frac > 0.0 && best > 0 {
                    // Outer ring one step beyond the integer core, on both
                    // sides. Out-of-bounds reads as empty.
                    let left = if position > whole {
                        src[base + position - whole - 1]
                    } else {
                        // When: `position` has no sample beyond the left core, erosion sees empty space at the tile edge.
                        0
                    };
                    let right = if position + whole + 1 < len {
                        src[base + position + whole + 1]
                    } else {
                        // When: the right outer sample exceeds `len`, erosion sees empty space at the tile edge.
                        0
                    };
                    let ring = left.min(right);
                    if ring < best {
                        // Pull toward the ring minimum in proportion to frac.
                        let blended = f64::from(best) - (f64::from(best) - f64::from(ring)) * frac;
                        best = blended.round().clamp(0.0, 255.0) as u8;
                    }
                }
                dst[base + position] = best;
            } else {
                // When: `erode` is false, use a max filter to grow coverage without empty edge samples.
                let mut best = 0u8;
                for neighbor in low..=high {
                    best = best.max(src[base + neighbor]);
                }
                if frac > 0.0 {
                    let mut ring = 0u8;
                    if position > whole {
                        ring = ring.max(src[base + position - whole - 1]);
                    }
                    if position + whole + 1 < len {
                        ring = ring.max(src[base + position + whole + 1]);
                    }
                    let blended = f64::from(ring) * frac;
                    best = best.max(blended.round().clamp(0.0, 255.0) as u8);
                }
                dst[base + position] = best;
            }
        }
    }
}

/// Grow `coverage` outward by `radius` raster pixels without changing the
/// returned tile dimensions or origin.
///
/// The operation pads scratch space so max-filter samples can cross the old
/// bitmap edge, then crops back to the original bounds. Radius is capped by
/// [`MAX_EMBOLDEN_RADIUS_PX`] rather than by spare bitmap margin: a flat-sided
/// glyph commonly touches an edge, and margin-dependent growth would make
/// weight behavior vary by glyph shape.
///
/// Returns `None` when there is nothing to do, the declared final dimensions
/// exceed the atlas limit, the input buffer does not match them exactly, or
/// checked scratch arithmetic overflows. The bounded scratch padding may exceed
/// the final atlas dimensions because it is cropped before return.
fn embolden_coverage(
    coverage: &[u8],
    width: usize,
    height: usize,
    radius: f64,
    is_subpixel: bool,
) -> Option<(Vec<u8>, usize, usize, usize)> {
    if radius <= 0.0 || width == 0 || height == 0 {
        // When: `radius` or tile dimensions are nonpositive, growth has no valid work to perform.
        return None;
    }
    let channels = if is_subpixel { 4 } else { 1 };
    let byte_len = checked_coverage_len(width, height, channels)?;
    if coverage.len() != byte_len {
        // When: `coverage.len()` differs from `byte_len`, dilation would index outside the supplied tile.
        return None;
    }
    // A shape-independent ceiling prevents high-weight crop saturation without
    // making flat glyphs (which commonly touch one bitmap edge) grow less than
    // curved glyphs. Fractional values below the ceiling still blend smoothly.
    let radius = radius.min(MAX_EMBOLDEN_RADIUS_PX);
    let pad = radius.ceil() as usize;
    let doubled_pad = pad.checked_mul(2)?;
    let new_w = width.checked_add(doubled_pad)?;
    let new_h = height.checked_add(doubled_pad)?;
    let scratch_pixels = new_w.checked_mul(new_h)?;
    let scratch_bytes = scratch_pixels.checked_mul(channels)?;
    let mut padded = vec![0u8; scratch_bytes];
    for row in 0..height {
        let src = row * width * channels;
        let dst = ((row + pad) * new_w + pad) * channels;
        padded[dst..dst + width * channels].copy_from_slice(&coverage[src..src + width * channels]);
    }

    // Dilate each channel independently. Interleaved BGRA is handled by
    // deinterleaving into a scratch plane, since the separable passes need a
    // contiguous stride per axis. Every byte is written below, so the buffer
    // starts zeroed rather than copied.
    let mut out = vec![0u8; scratch_bytes];
    for channel in 0..channels {
        let mut plane = vec![0u8; scratch_pixels];
        for pixel in 0..scratch_pixels {
            plane[pixel] = padded[pixel * channels + channel];
        }
        let mut tmp = vec![0u8; scratch_pixels];
        // Horizontal: new_h lines of new_w samples, stride new_w.
        morph_axis(&plane, &mut tmp, new_w, new_h, new_w, radius, false);
        // Vertical: transpose, reuse the same row-wise pass, transpose back.
        let mut transposed = vec![0u8; scratch_pixels];
        for row in 0..new_h {
            for column in 0..new_w {
                transposed[column * new_h + row] = tmp[row * new_w + column];
            }
        }
        let mut tcol = vec![0u8; scratch_pixels];
        morph_axis(&transposed, &mut tcol, new_h, new_w, new_h, radius, false);
        for row in 0..new_h {
            for column in 0..new_w {
                out[(row * new_w + column) * channels + channel] = tcol[column * new_h + row];
            }
        }
    }

    if is_subpixel {
        // Alpha is the envelope of the dilated RGB coverage.
        for px in out.as_chunks_mut::<4>().0 {
            px[3] = px[0].max(px[1]).max(px[2]);
        }
    }

    // Crop back to the original bounds. The padding exists so the dilation has
    // somewhere to expand into; keeping it would grow the tile, and a tile that
    // grows makes a *weight* control read as a *size* control. Every glyph then
    // changes size when the user asks for more ink, and glyphs from different
    // fonts change by different amounts — which is what makes two adjacent
    // markers drift apart.
    //
    // Ink that lands outside the original bounds is discarded. That is the
    // right trade: a rasterized glyph carries margin around its outline, so
    // there is room to thicken into, and where there is not, losing a fraction
    // of a pixel at the edge is less visible than every glyph resizing.
    let mut cropped = vec![0u8; byte_len];
    for row in 0..height {
        let src = ((row + pad) * new_w + pad) * channels;
        let dst = row * width * channels;
        cropped[dst..dst + width * channels].copy_from_slice(&out[src..src + width * channels]);
    }
    // `pad` is reported as zero: the caller shifts the tile offset by it, and
    // the tile no longer moved.
    Some((cropped, width, height, 0))
}

fn scale_coverage(coverage: u8, scale: f32) -> u8 {
    if coverage == 0 || coverage == u8::MAX || (scale - 1.0).abs() < f32::EPSILON {
        // When: `coverage` is an endpoint or `scale` is identity, gamma remapping cannot change the byte.
        return coverage;
    }
    let normalized = f32::from(coverage) / 255.0;
    let exponent = 1.0 / scale;
    (normalized.powf(exponent) * 255.0).round().clamp(0.0, 255.0) as u8
}

fn apply_weight_scale(coverage: &mut [u8], scale: f32, is_subpixel: bool) {
    let scale = sanitize_weight_scale(scale);
    if (scale - 1.0).abs() < f32::EPSILON {
        // When: sanitized `scale` is identity, leave every coverage byte and subpixel alpha untouched.
        return;
    }
    if is_subpixel {
        for pixel in coverage.as_chunks_mut::<4>().0 {
            pixel[0] = scale_coverage(pixel[0], scale);
            pixel[1] = scale_coverage(pixel[1], scale);
            pixel[2] = scale_coverage(pixel[2], scale);
            pixel[3] = pixel[0].max(pixel[1]).max(pixel[2]);
        }
    } else {
        // When: `is_subpixel` is false, each byte is a complete scalar coverage sample.
        for value in coverage {
            *value = scale_coverage(*value, scale);
        }
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
            FALLBACK_FAMILIES,
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
