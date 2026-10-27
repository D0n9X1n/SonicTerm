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
use std::collections::{BTreeSet, HashMap, HashSet};
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

pub mod catalog;
mod diagnostic_timing;
mod fallback_notice;
mod font_bytes;
mod hbwrap;

pub mod color;
pub mod db;
pub mod ftwrap;
pub mod locator;
pub mod parser;
mod rangeset;
pub mod rasterizer;
pub mod shaper;
#[cfg(test)]
mod source_pin;
pub mod units;

#[cfg(all(unix, not(target_os = "macos")))]
pub mod fcwrap;

pub use crate::fallback_notice::{
    FallbackNotice, FallbackWaker, UnresolvedPublication, UnresolvedSnapshot, UNRESOLVED_CAP,
};
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
                // When: the attempt returned `ClearShapeCache` after merging new faces, so shape again.
            }
            result => {
                // When: `result` is a shaped run or a real shaping error, return it unchanged.
                return result;
            }
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
    /// Runs after selection and before the worker publishes its unresolved codepoints.
    pub before_unresolved_publish: Option<Arc<dyn Fn() + Send + Sync>>,
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
    /// How many leading `handles` the configuration resolved; the shaper keeps their faces resident.
    /// Fallback discovery only appends (`insert_fallback_handles` pushes), so the handles below
    /// this boundary never move and it stays valid through every shaper rebuild.
    configured_handle_count: usize,
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
    /// The face epoch this request was scheduled under; its unresolved result is tagged with it.
    epoch: u64,
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
        // A failed stage means an uncovered codepoint is not known to be uncovered.
        let mut stage_failed = false;
        // The completion is consumed before publishing, so the cancel flag is read through this.
        let cancel = Arc::clone(&self.cancel);

        log::trace!(target: "sonicterm_font::payload", "Looking for {:?} in fallback fonts", self.no_glyphs);

        if self.cancelled() {
            // When: `cancelled()` says the request is obsolete, return before any lookup.
            return;
        }
        match Timing::result("fallback_locator", || {
            self.locator.locate_fallback_for_codepoints(&self.no_glyphs)
        }) {
            Ok(ref mut handles) => extra_handles.append(handles),
            Err(err) => {
                stage_failed = true;
                log::error!("font fallback resolution failed: stage=font-locator requested={requested_count} error={}", fallback_error_identity(&err));
                log::trace!(target: "sonicterm_font::payload", "fallback error: {err:#}");
            }
        }

        if self.cancelled() {
            // When: `cancelled()` turned true during the locator lookup, stop before the next stage.
            return;
        }
        if self.config.search_font_dirs_for_fallback {
            match Timing::result("fallback_font_dirs", || {
                self.font_dirs.locate_fallback_for_codepoints(&self.no_glyphs)
            }) {
                Ok(ref mut handles) => extra_handles.append(handles),
                Err(err) => {
                    stage_failed = true;
                    log::error!("font fallback resolution failed: stage=font_dirs requested={requested_count} error={}", fallback_error_identity(&err));
                    log::trace!(target: "sonicterm_font::payload", "fallback error: {err:#}");
                }
            }
        }

        if self.cancelled() {
            // When: `cancelled()` turned true before the built-in stage, stop without a completion.
            return;
        }
        match Timing::result("fallback_built_in", || {
            self.built_in.locate_fallback_for_codepoints(&self.no_glyphs)
        }) {
            Ok(ref mut handles) => extra_handles.append(handles),
            Err(err) => {
                stage_failed = true;
                log::error!("font fallback resolution failed: stage=built-in requested={requested_count} error={}", fallback_error_identity(&err));
                log::trace!(target: "sonicterm_font::payload", "fallback error: {err:#}");
            }
        }

        let mut wanted = RangeSet::new();
        for c in &self.no_glyphs {
            wanted.add(*c as u32);
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

        if let Some(before_publish) = &self.hooks.before_unresolved_publish {
            before_publish();
        }
        let uncovered: Vec<char> = wanted.iter_values().filter_map(char::from_u32).collect();
        let covered: Vec<char> = self
            .no_glyphs
            .iter()
            .copied()
            .filter(|codepoint| !wanted.contains(*codepoint as u32))
            .collect();
        let (unresolved, lookup_failed) = if stage_failed {
            (Vec::new(), uncovered)
        } else {
            // When: stage_failed is false, every stage ran, so what is left is truly uncovered.
            (uncovered, Vec::new())
        };
        if !cancel.load(std::sync::atomic::Ordering::SeqCst) {
            // A reload since scheduling cancels the request; the epoch check in `publish` catches
            // one that lands between this check and the publication.
            self.notice.publish(&UnresolvedPublication {
                epoch: self.epoch,
                unresolved,
                lookup_failed,
                covered,
            });
        }

        if !wanted.is_empty() && !stage_failed {
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
    /// Calls to the public `resolve_font_at_size`, hit or miss; a test seam for the face memo.
    resolve_calls: Cell<u64>,
    /// Moves each time `fonts` is cleared, so a face memoized from this configuration knows it
    /// may be stale. Any new writer that clears or replaces `fonts` must bump it too.
    face_epoch: Cell<u64>,
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
            resolve_calls: Cell::new(0),
            face_epoch: Cell::new(0),
        })
    }

    /// Mark every face memoized from this configuration stale; called beside each `fonts` clear.
    fn bump_face_epoch(&self) {
        self.face_epoch.set(self.face_epoch.get().wrapping_add(1));
    }

    /// Cancel every queued request and install a fresh flag for requests made from now on.
    fn cancel_queued_fallback(&self) {
        self.cancel.borrow().store(true, std::sync::atomic::Ordering::SeqCst);
        *self.cancel.borrow_mut() = Arc::new(AtomicBool::new(false));
    }

    // Lock order: fonts -> config, cancel, title_font, pane_select_font, char_select_font,
    // command_palette_font, metrics, font_dirs (serially).
    fn config_changed(&self, config: &ConfigHandle) -> anyhow::Result<()> {
        let mut fonts = self.fonts.borrow_mut();
        *self.config.borrow_mut() = config.clone();
        // Config was reloaded, invalidate our caches
        fonts.clear();
        // Bumped before the fallible reload below, so no reload outcome leaves a memo valid.
        self.bump_face_epoch();
        // The new configuration starts with no unresolved codepoints; older results are discarded.
        self.notice.reset(self.face_epoch.get());
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
            epoch: self.face_epoch.get(),
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
            // The worker has ended: drop this request and clear the channel, so the next missing
            // character starts a new worker. The frame path never retries or waits.
            tracing::error!("Failed to schedule font fallback resolve: {:#}", error);
            self.fallback_send_failures.set(self.fallback_send_failures.get() + 1);
            *fallback = None;
        }
    }

    // Lock order: font_scale -> dpi -> fonts -> cancel -> metrics -> title_font ->
    // pane_select_font -> char_select_font -> command_palette_font, each released first.
    pub fn change_scaling(&self, font_scale: f64, dpi: usize) -> (f64, usize) {
        let prior_font = *self.font_scale.borrow();
        let prior_dpi = *self.dpi.borrow();

        *self.dpi.borrow_mut() = dpi;
        *self.font_scale.borrow_mut() = font_scale;
        self.fonts.borrow_mut().clear();
        self.bump_face_epoch();
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

// Lifecycle: FontConfigInner drop releases its fallback worker: it sets `cancel`, then the dropped sender ends `for info in rx`.
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
        self.inner.resolve_calls.set(self.inner.resolve_calls.get().wrapping_add(1));
        self.inner.resolve_font_at_size(&self.inner, style, font_size)
    }

    /// Identity of the loaded faces: it moves whenever `config_changed` or `change_scaling` drops
    /// them, and never for a fallback merge, which extends a face in place. A caller that keeps a
    /// resolved face compares this to know the face is still the one this configuration serves.
    pub fn face_epoch(&self) -> u64 {
        self.inner.face_epoch.get()
    }

    /// Test seam: how many times `resolve_font_at_size` was called, cache hits included.
    #[doc(hidden)]
    pub fn resolve_count_for_test(&self) -> u64 {
        self.inner.resolve_calls.get()
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

/// Generic family names fontconfig resolves to a configured face; that is the alias working, not a
/// missing family.
const GENERIC_FAMILIES: [&str; 8] =
    ["monospace", "mono", "sans-serif", "sans", "serif", "system-ui", "emoji", "math"];

/// The face fontconfig substituted for `attr`: one tagged with `attr`'s family whose real family,
/// full name, PostScript name and aliases all differ from it. `None` for a generic alias.
fn fontconfig_substitute<'a>(
    attr: &config::FontAttributes,
    handles: &'a [ParsedFont],
) -> Option<&'a ParsedFont> {
    if GENERIC_FAMILIES.iter().any(|alias| alias.eq_ignore_ascii_case(&attr.family)) {
        // When: `attr.family` is a generic alias, resolving it to another face is expected.
        return None;
    }
    handles.iter().find(|face| {
        matches!(&face.handle.origin,
            crate::locator::FontOrigin::FontConfigMatch(requested) if *requested == attr.family)
            && !face.names().family.eq_ignore_ascii_case(&attr.family)
            && !face.matches_full_or_ps_name(attr)
            && !face.matches_alias(attr)
    })
}

/// Font diagnostics already reported in this process, so a miss that every resize, zoom, or new
/// window repeats reaches the log once.
static REPORTED_FONT_DIAGNOSTICS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Whether `message` is reported for the first time in this process; records it either way.
fn first_report(message: &str) -> bool {
    REPORTED_FONT_DIAGNOSTICS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(message.to_owned())
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;

mod font_resolution;
mod loaded_font;
