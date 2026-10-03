use crate::db::FontDatabase;
use crate::locator::{new_locator, FontLocator};
use crate::parser::ParsedFont;
use crate::rangeset::RangeSet;
use crate::rasterizer::{new_rasterizer, FontRasterizer};
use crate::shaper::{new_shaper, FontShaper, PresentationWidth};
use anyhow::{Context, Error};
use config::{
    configuration, ConfigHandle, DisplayPixelGeometry, FontAttributes, FontRasterizerSelection,
    FontWeight, TextStyle,
};
use diagnostic_timing::{RequestTiming, Timing};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::{Rc, Weak};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Presentation {
    Text,
    Emoji,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Direction {
    LeftToRight,
    RightToLeft,
}

mod diagnostic_timing;
mod fallback_notice;
mod hbwrap;

pub mod color;
pub mod db;
pub mod ftwrap;
pub mod locator;
pub mod parser;
mod rangeset;
pub mod rasterizer;
pub mod shaper;
pub mod units;

#[cfg(all(unix, not(target_os = "macos")))]
pub mod fcwrap;

pub use crate::fallback_notice::{FallbackNotice, FallbackWaker};
pub use crate::rasterizer::RasterizedGlyph;
pub use crate::shaper::{FallbackIdx, FontMetrics, GlyphInfo};

/// Install SonicTerm's font configuration into the underlying font stack.
///
/// This preserves WezTerm's font selection logic internally while exposing a
/// Sonic-facing API to the rest of the workspace.
pub fn use_sonic_font_configuration(
    primary_family: &str,
    font_size_pt: f64,
    fallback_families: &[&str],
) {
    let mut cfg = config::Config::default_config();
    let mut font_attrs = Vec::with_capacity(1 + fallback_families.len());
    font_attrs.push(config::FontAttributes::new(primary_family));
    for fam in fallback_families {
        font_attrs.push(config::FontAttributes::new_fallback(fam));
    }
    cfg.font = config::TextStyle { font: font_attrs, foreground: None };
    cfg.font_size = font_size_pt;
    // Windows font-rendering default: prefer FreeType's *light* autohinter
    // over the unhinted path. `compute_load_flags_from_config` otherwise
    // selects `NO_HINTING` whenever dpi >= 100 (i.e. >= ~125% display
    // scaling, which is the norm on Windows laptops), producing soft,
    // smeared stems. `Light` hinting snaps stems to the pixel grid
    // vertically only (no horizontal distortion of advances, so monospace
    // metrics and shaping are preserved) and the autohinter gives the most
    // consistent result across the bundled + fallback faces. Grayscale AA
    // is unchanged. macOS keeps its CoreText-tuned defaults (this stack is
    // only the rasterizer there for fallback faces), so the override is
    // Windows-only.
    #[cfg(target_os = "windows")]
    {
        cfg.freetype_load_target = config::FreeTypeLoadTarget::Light;
        cfg.freetype_render_target = Some(config::FreeTypeLoadTarget::Light);
        cfg.freetype_load_flags = Some(config::FreeTypeLoadFlags::FORCE_AUTOHINT);
    }
    config::use_this_configuration(cfg);
}

#[derive(Debug, Error)]
#[error("Font fallback recalculated")]
pub struct ClearShapeCache {}

/// Synchronous shape attempts one frame may spend on a run before skipping it this frame.
///
/// This bounds the count of shapes, not their time; the next frame starts over.
pub const MAX_FRAME_SHAPE_ATTEMPTS: u32 = 8;

/// Run `attempt` until it stops asking to be shaped again, at most
/// [`MAX_FRAME_SHAPE_ATTEMPTS`] times; past the bound the run is an error for this frame.
pub(crate) fn bounded_frame_shape<Output>(
    mut attempt: impl FnMut() -> anyhow::Result<Output>,
) -> anyhow::Result<Output> {
    for _ in 0..MAX_FRAME_SHAPE_ATTEMPTS {
        match attempt() {
            Err(error) if error.downcast_ref::<ClearShapeCache>().is_some() => {
                // When: this attempt merged new faces, shape the run again with them.
            }
            result => return result,
        }
    }
    Err(anyhow::anyhow!("frame shaping gave up after {MAX_FRAME_SHAPE_ATTEMPTS} attempts"))
}

/// Test pause points inside the fallback worker; production installs none.
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct FallbackWorkerHooks {
    /// Runs while the worker holds `pending_fallback`, after appending its handles.
    pub during_append: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Runs after the worker releases `pending_fallback` and before it completes the notice.
    pub before_completion: Option<Arc<dyn Fn() + Send + Sync>>,
}

static FONT_ID: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
pub type LoadedFontId = usize;
/// Allocates a process-unique identifier for a loaded font instance.
// Ordering: `FONT_ID.fetch_add` uses `Relaxed`; IDs need uniqueness but publish no state.
pub fn alloc_font_id() -> LoadedFontId {
    FONT_ID.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
}

lazy_static::lazy_static! {
    static ref LAST_WARNING: Mutex<Option<(Instant, usize)>> = Mutex::new(None);
}

pub struct LoadedFont {
    rasterizers: RefCell<HashMap<FallbackIdx, Box<dyn FontRasterizer>>>,
    handles: RefCell<Vec<ParsedFont>>,
    shaper: RefCell<Box<dyn FontShaper>>,
    metrics: FontMetrics,
    pixel_geometry: DisplayPixelGeometry,
    font_size: f64,
    dpi: u32,
    font_config: Weak<FontConfigInner>,
    pending_fallback: Arc<Mutex<Vec<ParsedFont>>>,
    text_style: TextStyle,
    id: LoadedFontId,
    /// Glyphs for which no font was found and for which we should
    /// stop searching
    tried_glyphs: RefCell<HashSet<char>>,
}

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
    fn insert_fallback_handles(&self, extra_handles: Vec<ParsedFont>) -> anyhow::Result<bool> {
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
                *self.shaper.borrow_mut() =
                    new_shaper(&font_config.config.borrow(), &self.handles.borrow())?;
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

    // Lock order: `pending_fallback` -> `shaper`; afterward `shaper` ->
    // `tried_glyphs` -> `font_config`. The helper owns the first nested edge. With
    // `wait_for_pending` false the first lock is only tried, so a frame never waits on it.
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
            // When: an explicit caller may wait, take the lock as the worker releases it.
            Some(self.pending_fallback.lock().unwrap_or_else(PoisonError::into_inner))
        } else {
            match self.pending_fallback.try_lock() {
                Ok(guard) => Some(guard),
                Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    // When: the worker is appending, skip the merge; its completion follows the
                    // unlock, so a newer generation brings a later frame that merges.
                    None
                }
            }
        };
        if let Some(mut pending) = pending {
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

struct FallbackResolveInfo {
    timing: Option<RequestTiming>,
    no_glyphs: Vec<char>,
    pending: Arc<Mutex<Vec<ParsedFont>>>,
    completion: Box<dyn FnOnce() + Send>,
    font_dirs: Arc<FontDatabase>,
    built_in: Arc<FontDatabase>,
    locator: Arc<dyn FontLocator + Send + Sync>,
    config: ConfigHandle,
    /// Completed after the handles are appended and the pending lock is released.
    notice: Arc<FallbackNotice>,
    /// Set when the configuration is dropped or its fonts are reset; the request is obsolete.
    cancel: Arc<AtomicBool>,
    hooks: FallbackWorkerHooks,
}

fn select_fallback_fonts(
    extra_handles: &mut Vec<ParsedFont>,
    wanted: &mut RangeSet<u32>,
    sort_by_coverage: bool,
) {
    if wanted.len() > 1 && sort_by_coverage {
        extra_handles.sort_by_cached_key(|font| {
            font.coverage_intersection(wanted).map(|coverage| coverage.len()).unwrap_or(0)
        });
        extra_handles.reverse();
    }

    // Math faces can win raw coverage while drawing shared text symbols outside
    // their advances. Stable demotion preserves coverage order within each group.
    extra_handles.sort_by_key(|font| font.is_math_font);
    extra_handles.retain(|font| match font.coverage_intersection(wanted) {
        Ok(coverage) if coverage.is_empty() => false,
        Ok(coverage) => {
            // Reserve this face's coverage so later faces remain only when they
            // contribute another unresolved glyph.
            *wanted = wanted.difference(&coverage);
            true
        }
        Err(_) => false,
    });
}

/// Classify fallback failure without formatting errors that may contain the input run.
pub(crate) fn fallback_error_identity(error: &anyhow::Error) -> &'static str {
    if let Some(error) = error.root_cause().downcast_ref::<std::io::Error>() {
        match error.kind() {
            std::io::ErrorKind::NotFound => "not-found",
            std::io::ErrorKind::PermissionDenied => "permission-denied",
            std::io::ErrorKind::InvalidData => "invalid-data",
            _ => "io",
        }
    } else {
        // When: the source is not a typed I/O error, do not persist its potentially payload-bearing message.
        "font"
    }
}

impl FallbackResolveInfo {
    fn process(mut self) {
        let request_timing = self.timing.take();
        RequestTiming::run(request_timing, || self.process_inner());
    }

    /// Whether this request became obsolete; a cancelled request makes no native lookup.
    fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::SeqCst)
    }

    // Lock order: `pending` is released before the notice's `delivery` and before
    // `LAST_WARNING`; none of them nest.
    fn process_inner(self) {
        let requested_count = self.no_glyphs.len();
        let mut extra_handles = vec![];

        log::trace!(target: "sonicterm_font::payload", "Looking for {:?} in fallback fonts", self.no_glyphs);

        if self.cancelled() {
            // When: the request is obsolete, return before any lookup and without a completion.
            return;
        }
        match Timing::result("fallback_locator", || {
            self.locator.locate_fallback_for_codepoints(&self.no_glyphs)
        }) {
            Ok(ref mut handles) => extra_handles.append(handles),
            Err(err) => {
                log::error!("font fallback resolution failed: stage=font-locator requested={requested_count} error={}", fallback_error_identity(&err));
                log::trace!(target: "sonicterm_font::payload", "fallback error: {err:#}");
            }
        }

        if self.cancelled() {
            // When: the request became obsolete during the locator lookup, stop before the next stage.
            return;
        }
        if self.config.search_font_dirs_for_fallback {
            match Timing::result("fallback_font_dirs", || {
                self.font_dirs.locate_fallback_for_codepoints(&self.no_glyphs)
            }) {
                Ok(ref mut handles) => extra_handles.append(handles),
                Err(err) => {
                    log::error!("font fallback resolution failed: stage=font_dirs requested={requested_count} error={}", fallback_error_identity(&err));
                    log::trace!(target: "sonicterm_font::payload", "fallback error: {err:#}");
                }
            }
        }

        if self.cancelled() {
            // When: the request became obsolete before the built-in stage, stop without a completion.
            return;
        }
        match Timing::result("fallback_built_in", || {
            self.built_in.locate_fallback_for_codepoints(&self.no_glyphs)
        }) {
            Ok(ref mut handles) => extra_handles.append(handles),
            Err(err) => {
                log::error!("font fallback resolution failed: stage=built-in requested={requested_count} error={}", fallback_error_identity(&err));
                log::trace!(target: "sonicterm_font::payload", "fallback error: {err:#}");
            }
        }

        let mut wanted = RangeSet::new();
        for c in self.no_glyphs {
            wanted.add(c as u32);
        }
        log::trace!(target: "sonicterm_font::payload",
            "Fallback fonts for {wanted:?} before sorting are: {extra_handles:#?}"
        );

        let selection_timing = Timing::begin("fallback_selection");
        select_fallback_fonts(
            &mut extra_handles,
            &mut wanted,
            self.config.sort_fallback_fonts_by_coverage,
        );
        Timing::finish(selection_timing, "returned");
        log::trace!(target: "sonicterm_font::payload",
            "Fallback selection for requested={requested_count}: {extra_handles:#?}"
        );

        if !extra_handles.is_empty() {
            {
                let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
                pending.append(&mut extra_handles);
                if let Some(during_append) = &self.hooks.during_append {
                    during_append();
                }
            }
            // The handles are mergeable from here; the completion follows the unlock, so any
            // frame that merged them is followed by a newer generation.
            if let Some(before_completion) = &self.hooks.before_completion {
                before_completion();
            }
            if diagnostic_timing::enabled() {
                tracing::debug!(target: "render_timing", completion_called = true, phase = "enter", "font fallback completion");
            }
            self.notice.complete();
            (self.completion)();
        } else if diagnostic_timing::enabled() {
            // When: timing is enabled but extra_handles is empty, record that no fallback completion callback runs.
            tracing::debug!(target: "render_timing", completion_called = false, "font fallback completion");
        }

        if !wanted.is_empty() {
            log::trace!(target: "sonicterm_font::payload", "unresolved font codepoints: {wanted:?}");

            let current_gen = self.config.generation();
            let show_warning = self.config.warn_about_missing_glyphs
                && LAST_WARNING
                    .lock()
                    .unwrap()
                    .map(|(instant, generation)| {
                        generation != current_gen
                            || instant.elapsed() > Duration::from_secs(60 * 60)
                    })
                    .unwrap_or(true);

            if show_warning {
                LAST_WARNING.lock().unwrap().replace((Instant::now(), self.config.generation()));
                log::warn!(
                    "No fonts contain glyphs for {} unresolved codepoints. \
                     Placeholder glyphs are being displayed instead. \
                     Install fonts covering the missing characters or choose another \
                     [font].family in sonicterm.toml. See \
                     https://github.com/D0n9X1n/SonicTerm/wiki/Configuration for font configuration.",
                    wanted.len(),
                );
            } else {
                // When: `show_warning` is false, emit only debug diagnostics.
                log::debug!("No fonts contain glyphs for {} unresolved codepoints", wanted.len());
            }
        }
    }
}

#[derive(PartialEq, Eq)]
enum Entity {
    Title,
    CommandPalette,
    CharSelect,
    PaneSelect,
}

/// Loaded-face identity includes native point size because bitmap-strike
/// selection and shaping metrics can differ even when the text style is equal.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LoadedFontKey {
    style: TextStyle,
    font_size_bits: u64,
}

impl LoadedFontKey {
    fn new(style: &TextStyle, font_size: f64) -> Self {
        Self { style: style.clone(), font_size_bits: font_size.to_bits() }
    }
}

struct FontConfigInner {
    fonts: RefCell<HashMap<LoadedFontKey, Rc<LoadedFont>>>,
    metrics: RefCell<HashMap<u64, FontMetrics>>,
    dpi: RefCell<usize>,
    font_scale: RefCell<f64>,
    config: RefCell<ConfigHandle>,
    locator: Arc<dyn FontLocator + Send + Sync>,
    font_dirs: RefCell<Arc<FontDatabase>>,
    built_in: RefCell<Arc<FontDatabase>>,
    title_font: RefCell<Option<Rc<LoadedFont>>>,
    pane_select_font: RefCell<Option<Rc<LoadedFont>>>,
    char_select_font: RefCell<Option<Rc<LoadedFont>>>,
    command_palette_font: RefCell<Option<Rc<LoadedFont>>>,
    fallback_channel: RefCell<Option<Sender<FallbackResolveInfo>>>,
    /// Shared by every stack cloned from this configuration; never replaced on a reset.
    notice: Arc<FallbackNotice>,
    /// Cancels queued requests on drop and on a reset that drops loaded fonts.
    cancel: RefCell<Arc<AtomicBool>>,
    hooks: RefCell<FallbackWorkerHooks>,
    /// The current worker, kept so a test can join it; production never joins.
    fallback_worker: RefCell<Option<JoinHandle<()>>>,
    fallback_spawns: Cell<usize>,
    fallback_send_failures: Cell<usize>,
}

/// Matches and loads fonts for a given input style
pub struct FontConfiguration {
    inner: Rc<FontConfigInner>,
}

impl FontConfigInner {
    /// Create a new empty configuration
    pub fn new(config: Option<ConfigHandle>, dpi: usize) -> anyhow::Result<Self> {
        let config = config.unwrap_or_else(configuration);
        let locator = new_locator(config.font_locator);
        Self::with_locator(config, dpi, locator)
    }

    fn with_locator(
        config: ConfigHandle,
        dpi: usize,
        locator: Arc<dyn FontLocator + Send + Sync>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            fonts: RefCell::new(HashMap::new()),
            locator,
            metrics: RefCell::new(HashMap::new()),
            title_font: RefCell::new(None),
            pane_select_font: RefCell::new(None),
            char_select_font: RefCell::new(None),
            command_palette_font: RefCell::new(None),
            font_scale: RefCell::new(1.0),
            dpi: RefCell::new(dpi),
            config: RefCell::new(config.clone()),
            font_dirs: RefCell::new(Arc::new(FontDatabase::with_font_dirs(&config)?)),
            built_in: RefCell::new(Arc::new(FontDatabase::with_built_in()?)),
            fallback_channel: RefCell::new(None),
            notice: FallbackNotice::new(),
            cancel: RefCell::new(Arc::new(AtomicBool::new(false))),
            hooks: RefCell::new(FallbackWorkerHooks::default()),
            fallback_worker: RefCell::new(None),
            fallback_spawns: Cell::new(0),
            fallback_send_failures: Cell::new(0),
        })
    }

    /// Cancel every queued request and install a fresh flag for requests made from now on.
    fn cancel_queued_fallback(&self) {
        self.cancel.borrow().store(true, std::sync::atomic::Ordering::SeqCst);
        *self.cancel.borrow_mut() = Arc::new(AtomicBool::new(false));
    }

    // Lock order: `fonts` -> each of `config`, `cancel`, `title_font`, `pane_select_font`,
    // `char_select_font`, `command_palette_font`, `metrics`, and `font_dirs`; those borrow serially.
    fn config_changed(&self, config: &ConfigHandle) -> anyhow::Result<()> {
        let mut fonts = self.fonts.borrow_mut();
        *self.config.borrow_mut() = config.clone();
        // Config was reloaded, invalidate our caches
        fonts.clear();
        self.cancel_queued_fallback();
        self.title_font.borrow_mut().take();
        self.pane_select_font.borrow_mut().take();
        self.char_select_font.borrow_mut().take();
        self.command_palette_font.borrow_mut().take();
        self.metrics.borrow_mut().clear();
        *self.font_dirs.borrow_mut() = Arc::new(FontDatabase::with_font_dirs(config)?);
        Ok(())
    }

    // Lock order: `font_dirs`, `built_in`, `config`, `cancel` and `hooks` borrow serially, then
    // `fallback_channel` -> `fallback_worker`.
    fn schedule_fallback_resolve<F: FnOnce() + Send + 'static>(
        &self,
        no_glyphs: Vec<char>,
        pending: &Arc<Mutex<Vec<ParsedFont>>>,
        completion: F,
    ) {
        if no_glyphs.is_empty() {
            // When: `no_glyphs.is_empty()` is true, there is no fallback work to enqueue.
            return;
        }

        let info = FallbackResolveInfo {
            timing: RequestTiming::capture_next(),
            completion: Box::new(completion),
            no_glyphs,
            pending: Arc::clone(pending),
            font_dirs: Arc::clone(&*self.font_dirs.borrow()),
            built_in: Arc::clone(&*self.built_in.borrow()),
            locator: Arc::clone(&self.locator),
            config: self.config.borrow().clone(),
            notice: Arc::clone(&self.notice),
            cancel: Arc::clone(&self.cancel.borrow()),
            hooks: self.hooks.borrow().clone(),
        };

        let mut fallback = self.fallback_channel.borrow_mut();

        if fallback.is_none() {
            let (tx, rx) = channel::<FallbackResolveInfo>();

            let worker = std::thread::spawn(move || {
                for info in rx {
                    info.process();
                }
            });
            *self.fallback_worker.borrow_mut() = Some(worker);
            self.fallback_spawns.set(self.fallback_spawns.get() + 1);

            fallback.replace(tx);
        }

        if let Err(error) = fallback.as_mut().expect("channel to exist").send(info) {
            // When: the worker has ended, drop this request and clear the channel, so the next
            // missing character starts a new worker. The frame path never retries or waits.
            log::error!("Failed to schedule font fallback resolve: {:#}", error);
            self.fallback_send_failures.set(self.fallback_send_failures.get() + 1);
            *fallback = None;
        }
    }

    fn compute_title_font(&self, config: &ConfigHandle, make_bold: bool) -> (TextStyle, f64) {
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

    fn make_entity_font_impl(
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

        let shaper = new_shaper(&config, &handles)?;

        let metrics = shaper.metrics(font_size, dpi).with_context(|| {
            format!("obtaining metrics for font_size={} @ dpi {}", font_size, dpi)
        })?;

        let loaded = Rc::new(LoadedFont {
            rasterizers: RefCell::new(HashMap::new()),
            handles: RefCell::new(handles),
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

    fn title_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut title_font = self.title_font.borrow_mut();

        if let Some(entry) = title_font.as_ref() {
            // When: `title_font` is cached, return its shared handle without resolving again.
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::Title)?;

        title_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    fn command_palette_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut command_palette_font = self.command_palette_font.borrow_mut();

        if let Some(entry) = command_palette_font.as_ref() {
            // When: `command_palette_font` is cached, reuse it without resolving again.
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::CommandPalette)?;

        command_palette_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    fn char_select_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut char_select_font = self.char_select_font.borrow_mut();

        if let Some(entry) = char_select_font.as_ref() {
            // When: `char_select_font` is cached, reuse it without resolving again.
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::CharSelect)?;

        char_select_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    fn pane_select_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        let mut pane_select_font = self.pane_select_font.borrow_mut();

        if let Some(entry) = pane_select_font.as_ref() {
            // When: `pane_select_font` is cached, reuse it without resolving again.
            return Ok(Rc::clone(entry));
        }

        let loaded = self.make_entity_font_impl(myself, Entity::PaneSelect)?;

        pane_select_font.replace(Rc::clone(&loaded));

        Ok(loaded)
    }

    fn resolve_font_helper_impl(
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

    fn resolve_font_helper(
        &self,
        style: &TextStyle,
        config: &ConfigHandle,
        pixel_size: u16,
    ) -> anyhow::Result<(Box<dyn FontShaper>, Vec<ParsedFont>)> {
        let attributes = style.font_with_fallback();

        let (handles, loaded) = self.resolve_font_helper_impl(&attributes, pixel_size)?;

        for attr in &attributes {
            if !attr.is_synthetic && !attr.is_fallback && !loaded.contains(attr) {
                let is_primary = config.font.font.iter().any(|a| !a.is_fallback && a == attr);
                let derived_from_primary =
                    config.font.font.iter().any(|a| !a.is_fallback && a.family == attr.family);
                let identity = format!(
                    "{:?} (weight={}, stretch={}, style={})",
                    attr.family, attr.weight, attr.stretch, attr.style
                );
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

                config::show_error(&format!(
                    "{explanation}. Fallback fonts are being used instead, and text \
                     may not render as intended. Check [font].family in sonicterm.toml \
                     and ensure the font is available to SonicTerm. See \
                     https://github.com/D0n9X1n/SonicTerm/wiki/Configuration for font configuration."
                ));
            }
        }

        Ok((new_shaper(config, &handles)?, handles))
    }

    /// Given a text style, load (with caching) the font that best
    /// matches according to the fontconfig pattern.
    fn resolve_font(&self, myself: &Rc<Self>, style: &TextStyle) -> anyhow::Result<Rc<LoadedFont>> {
        let font_size = self.config.borrow().font_size;
        self.resolve_font_at_size(myself, style, font_size)
    }

    fn resolve_font_at_size(
        &self,
        myself: &Rc<Self>,
        style: &TextStyle,
        font_size: f64,
    ) -> anyhow::Result<Rc<LoadedFont>> {
        let effective_size = font_size * *self.font_scale.borrow();
        self.resolve_font_at_effective_size(myself, style, effective_size)
    }

    fn resolve_font_at_effective_size(
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

        let loaded = Rc::new(LoadedFont {
            rasterizers: RefCell::new(HashMap::new()),
            handles: RefCell::new(handles),
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

    // Lock order: `font_scale` -> `dpi` -> `fonts` -> `cancel` -> `metrics` -> `title_font` ->
    // `pane_select_font` -> `char_select_font` -> `command_palette_font`; borrows are serial.
    pub fn change_scaling(&self, font_scale: f64, dpi: usize) -> (f64, usize) {
        let prior_font = *self.font_scale.borrow();
        let prior_dpi = *self.dpi.borrow();

        *self.dpi.borrow_mut() = dpi;
        *self.font_scale.borrow_mut() = font_scale;
        self.fonts.borrow_mut().clear();
        // The notice stays; requests for the dropped faces are obsolete.
        self.cancel_queued_fallback();
        self.metrics.borrow_mut().clear();
        self.title_font.borrow_mut().take();
        // E4: a DPI/scale change must drop the chrome font caches too,
        // not just title_font — otherwise the command palette / char-select /
        // pane-select chrome would re-rasterize at the old DPI. Mirror the full
        // invalidation that `config_changed` performs.
        self.pane_select_font.borrow_mut().take();
        self.char_select_font.borrow_mut().take();
        self.command_palette_font.borrow_mut().take();

        (prior_font, prior_dpi)
    }

    /// Returns the baseline font specified in the configuration
    pub fn default_font(&self, myself: &Rc<Self>) -> anyhow::Result<Rc<LoadedFont>> {
        self.resolve_font(myself, &self.config.borrow().font)
    }

    pub fn get_font_scale(&self) -> f64 {
        *self.font_scale.borrow()
    }

    pub fn get_dpi(&self) -> usize {
        *self.dpi.borrow()
    }

    pub fn default_font_metrics(&self, myself: &Rc<Self>) -> Result<FontMetrics, Error> {
        let font_size = self.config.borrow().font_size;
        self.default_font_metrics_at_size(myself, font_size)
    }

    fn default_font_metrics_at_size(
        &self,
        myself: &Rc<Self>,
        font_size: f64,
    ) -> Result<FontMetrics, Error> {
        let effective_size = font_size * *self.font_scale.borrow();
        let key = effective_size.to_bits();
        if let Some(metrics) = self.metrics.borrow().get(&key) {
            // When: this native size has cached metrics, return them without resolving again.
            return Ok(*metrics);
        }

        let style = self.config.borrow().font.clone();
        let font = self.resolve_font_at_effective_size(myself, &style, effective_size)?;
        let metrics = font.metrics();

        self.metrics.borrow_mut().insert(key, metrics);

        Ok(metrics)
    }
}

// Lifecycle: dropping `FontConfigInner` cancels its queued fallback requests, then drops the
// sender, so the worker skips its backlog and its `for info in rx` loop ends.
impl Drop for FontConfigInner {
    fn drop(&mut self) {
        self.cancel.borrow().store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl FontConfiguration {
    /// Create a new empty configuration
    pub fn new(config: Option<ConfigHandle>, dpi: usize) -> anyhow::Result<Self> {
        let inner = Rc::new(FontConfigInner::new(config, dpi)?);
        Ok(Self { inner })
    }

    /// The fallback notice this configuration's worker completes; shared by every clone.
    #[must_use]
    pub fn fallback_notice(&self) -> Arc<FallbackNotice> {
        Arc::clone(&self.inner.notice)
    }

    /// Test seam: a configuration that discovers fallback faces through `locator`.
    #[doc(hidden)]
    pub fn new_with_locator_for_test(
        config: ConfigHandle,
        dpi: usize,
        locator: Arc<dyn FontLocator + Send + Sync>,
    ) -> anyhow::Result<Self> {
        Ok(Self { inner: Rc::new(FontConfigInner::with_locator(config, dpi, locator)?) })
    }

    /// Test seam: pause points for the fallback worker, used by requests scheduled afterwards.
    #[doc(hidden)]
    pub fn set_fallback_worker_hooks_for_test(&self, hooks: FallbackWorkerHooks) {
        *self.inner.hooks.borrow_mut() = hooks;
    }

    /// Test seam: take the current worker's handle so a test can join it.
    #[doc(hidden)]
    pub fn take_fallback_worker_for_test(&self) -> Option<JoinHandle<()>> {
        self.inner.fallback_worker.borrow_mut().take()
    }

    /// Test seam: how many fallback workers this configuration has started.
    #[doc(hidden)]
    pub fn fallback_spawns_for_test(&self) -> usize {
        self.inner.fallback_spawns.get()
    }

    /// Test seam: how many requests were dropped because the worker had ended.
    #[doc(hidden)]
    pub fn fallback_send_failures_for_test(&self) -> usize {
        self.inner.fallback_send_failures.get()
    }

    /// Test seam: whether a sender to a fallback worker is held.
    #[doc(hidden)]
    pub fn has_fallback_channel_for_test(&self) -> bool {
        self.inner.fallback_channel.borrow().is_some()
    }

    /// Replaces the active configuration and invalidates every derived font cache.
    pub fn config_changed(&self, config: &ConfigHandle) -> anyhow::Result<()> {
        self.inner.config_changed(config)
    }

    /// Returns a clone of the active font configuration handle.
    pub fn config(&self) -> ConfigHandle {
        self.inner.config.borrow().clone()
    }

    /// Returns the cached or newly resolved window-title font.
    pub fn title_font(&self) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.title_font(&self.inner)
    }

    /// Returns the cached or newly resolved command-palette font.
    pub fn command_palette_font(&self) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.command_palette_font(&self.inner)
    }

    /// Returns the cached or newly resolved pane-selection font.
    pub fn pane_select_font(&self) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.pane_select_font(&self.inner)
    }

    /// Returns the cached or newly resolved character-selection font.
    pub fn char_select_font(&self) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.char_select_font(&self.inner)
    }

    /// Given a text style, load (with caching) the font that best
    /// matches according to the fontconfig pattern at the configured size.
    pub fn resolve_font(&self, style: &TextStyle) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.resolve_font(&self.inner, style)
    }

    /// Resolve a text style at an explicit logical point size.
    pub fn resolve_font_at_size(
        &self,
        style: &TextStyle,
        font_size: f64,
    ) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.resolve_font_at_size(&self.inner, style, font_size)
    }

    /// Updates font scaling and DPI, invalidating caches and returning prior values.
    pub fn change_scaling(&self, font_scale: f64, dpi: usize) -> (f64, usize) {
        self.inner.change_scaling(font_scale, dpi)
    }

    /// Returns the baseline font specified in the configuration
    pub fn default_font(&self) -> anyhow::Result<Rc<LoadedFont>> {
        self.inner.default_font(&self.inner)
    }

    /// Returns the active font-size scale multiplier.
    pub fn get_font_scale(&self) -> f64 {
        self.inner.get_font_scale()
    }

    /// Returns the active rasterization DPI.
    pub fn get_dpi(&self) -> usize {
        self.inner.get_dpi()
    }

    /// Returns cached metrics for the configured baseline font.
    pub fn default_font_metrics(&self) -> Result<FontMetrics, Error> {
        self.inner.default_font_metrics(&self.inner)
    }

    /// Returns cached metrics for the baseline font at an explicit point size.
    pub fn default_font_metrics_at_size(&self, font_size: f64) -> Result<FontMetrics, Error> {
        self.inner.default_font_metrics_at_size(&self.inner, font_size)
    }

    /// Lists parsed fonts from configured directories and bundled font data.
    pub fn list_fonts_in_font_dirs(&self) -> Vec<ParsedFont> {
        let mut font_dirs = self.inner.font_dirs.borrow().list_available();
        let mut built_in = self.inner.built_in.borrow().list_available();

        font_dirs.append(&mut built_in);
        font_dirs.sort();
        font_dirs
    }

    /// Enumerates all fonts exposed by the platform font locator.
    pub fn list_system_fonts(&self) -> anyhow::Result<Vec<ParsedFont>> {
        self.inner.locator.enumerate_all_fonts()
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
