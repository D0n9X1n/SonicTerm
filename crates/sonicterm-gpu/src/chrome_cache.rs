//! Device-free chrome caches: tab titles by tab position, the search overlay's constant runs, and
//! the UI palette derived from the render theme.
//!
//! Each cache keeps shaped runs as [`PreparedChromeRun`]s and hands them back as borrowed
//! [`ChromeRunView`]s, so a warm frame draws chrome without shaping it and without copying glyphs.
//! Nothing here touches the device; the renderer owns one of each and clears the run caches at
//! every face replacement (see `GpuRenderer::adopt_font_stacks`, `rebuild_for_sf` and
//! `clear_shape_cache`) and, for the body-stack runs, at every applied fallback generation.

use sonicterm_engine::FontStack;
use sonicterm_render_model::boundary::cfg::theme::{Palette, Theme};
use sonicterm_render_model::boundary::ui::tabs::{fit_title_to_width, TITLE_FIT_TOLERANCE_PX};
use sonicterm_render_model::boundary::ui::ui_tokens::UiPalette;

#[cfg(test)]
use crate::chrome_text::CHROME_SHAPED_GLYPH_BYTES;
use crate::chrome_text::{ChromeAttrs, ChromeRunView, ChromeShapedRun, PreparedChromeRun};

#[cfg(test)]
#[path = "chrome_cache_tests.rs"]
mod chrome_cache_tests;

/// Tab positions the title cache can hold; a tab at or beyond this position draws uncached.
pub(crate) const TITLE_SLOTS: usize = 64;

/// Chrome runs the run cache can hold; a miss on a full table replaces the oldest entry.
pub(crate) const CHROME_RUN_SLOTS: usize = 32;

/// Longest text, in bytes, either cache admits.
pub(crate) const MAX_CACHED_TEXT_BYTES: usize = 256;

/// Most shaped glyphs either cache admits for one run.
pub(crate) const MAX_CACHED_GLYPHS: usize = 512;

/// Longest drawn title text: a cut title keeps a prefix of the admitted text plus `…` (3 bytes).
#[cfg(test)]
const MAX_DRAWN_TITLE_BYTES: usize = MAX_CACHED_TEXT_BYTES + '…'.len_utf8();

/// The admission bounds of one cache; tests lower them to reach the refusal paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdmissionLimits {
    /// Longest admitted text in bytes.
    pub(crate) text_bytes: usize,
    /// Most admitted glyphs per run.
    pub(crate) glyphs: usize,
}

impl Default for AdmissionLimits {
    fn default() -> Self {
        Self { text_bytes: MAX_CACHED_TEXT_BYTES, glyphs: MAX_CACHED_GLYPHS }
    }
}

impl AdmissionLimits {
    /// Whether a run of `glyph_count` glyphs shaped from `text` fits these bounds.
    fn admits(self, text: &str, glyph_count: usize) -> bool {
        text.len() <= self.text_bytes && glyph_count <= self.glyphs
    }
}

/// Shape `text` at `font_size_px` with the stack's native em equal to it, as tab titles are.
fn shape_title<'text>(
    stack: &FontStack,
    text: &'text str,
    font_size_px: f32,
) -> Option<ChromeShapedRun<'text>> {
    ChromeShapedRun::shape(stack, text, ChromeAttrs::default(), font_size_px, font_size_px)
}

/// One title fitted into its tab, with the run it measured last when that run shaped.
#[derive(Debug)]
pub(crate) struct TitleFit {
    /// The drawn text: the whole title, a grapheme prefix followed by `…`, or empty.
    pub(crate) text: String,
    /// The shaped run of `text`; `None` when its shaping failed or the title never shaped.
    pub(crate) run: Option<PreparedChromeRun>,
    /// Drawn width of `text` in raster pixels.
    pub(crate) width_px: f32,
    /// Whether every measure and the final shape succeeded, so the fit may be cached.
    pub(crate) complete: bool,
}

/// Fit a tab's display `text` into `available_px` of the tab font at `font_size_px`.
///
/// Text that fits is drawn whole from the run it was measured with. Otherwise the longest
/// grapheme prefix that fits beside `…` is kept and the cut text is shaped again; when kerning or
/// a ligature makes it wider than its advances promised, the cut steps back a grapheme, so a drawn
/// title never passes its tab, and the accepted cut's run is the one drawn. A measure that fails
/// is tolerated as the renderer always tolerated it (the ellipsis counts 0 px, a cut counts its
/// estimated width), and the fit is then marked incomplete so no cache keeps it. A title whose
/// whole text cannot be shaped draws nothing.
pub(crate) fn fit_title_run(
    stack: &FontStack,
    text: &str,
    font_size_px: f32,
    available_px: f32,
) -> TitleFit {
    let Some(whole) = shape_title(stack, text, font_size_px) else {
        // When: the whole title cannot be shaped, nothing is drawn for it this frame.
        return TitleFit { text: String::new(), run: None, width_px: 0.0, complete: false };
    };
    let whole_px: f32 = whole.advances().map(|(_, advance)| advance).sum();
    if whole_px <= available_px + TITLE_FIT_TOLERANCE_PX {
        // When: `whole_px` fits `available_px`, the whole title draws from its measured run.
        return TitleFit {
            text: text.to_string(),
            run: Some(PreparedChromeRun::from_run(whole)),
            width_px: whole_px,
            complete: true,
        };
    }
    let mut complete = true;
    let ellipsis_px: f32 = match shape_title(stack, "…", font_size_px) {
        Some(ellipsis) => ellipsis.advances().map(|(_, advance)| advance).sum(),
        None => {
            // The ellipsis could not be shaped: it counts 0 px, as before, and the fit is not
            // cached.
            complete = false;
            0.0
        }
    };
    let advances: Vec<(usize, f32)> = whole.advances().collect();
    let mut budget_px = available_px;
    // Each pass keeps a strictly shorter prefix, so the text length bounds the passes.
    for _ in 0..=text.len() {
        let fitted = fit_title_to_width(text, &advances, ellipsis_px, budget_px);
        let (drawn_px, run) = match shape_title(stack, fitted.text.as_str(), font_size_px) {
            Some(cut) => {
                let drawn_px: f32 = cut.advances().map(|(_, advance)| advance).sum();
                (drawn_px, Some(PreparedChromeRun::from_run(cut)))
            }
            None => {
                // The cut could not be shaped: it counts its estimated width, as before, and
                // the fit is not cached.
                complete = false;
                (fitted.width_px, None)
            }
        };
        if fitted.text.is_empty() || drawn_px <= available_px + TITLE_FIT_TOLERANCE_PX {
            // When: `fitted` is empty or its shaped `drawn_px` fits `available_px`, draw it.
            let complete = complete && run.is_some();
            return TitleFit { text: fitted.text, run, width_px: drawn_px, complete };
        }
        budget_px = fitted.width_px - TITLE_FIT_TOLERANCE_PX - 0.01;
    }
    TitleFit { text: String::new(), run: None, width_px: 0.0, complete: false }
}

/// What a title's cached fit depends on besides color: its display text, the tab font's key and
/// fallback epoch, the raster size and the width available to the text.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TitleProbe<'text> {
    /// The tab's display text (badge and title).
    pub(crate) text: &'text str,
    /// `TabTitleFont::key`: family, size, weight, scale and stack presence.
    pub(crate) font_key: u64,
    /// `TabTitleFont::fallback_epoch`, bumped by every applied fallback generation.
    pub(crate) fallback_epoch: u64,
    /// Raster px em size the title is shaped and drawn at.
    pub(crate) raster_px: f32,
    /// Raster px available to the text inside the tab.
    pub(crate) text_px: f32,
}

/// A kept title's key: the probe with its text owned.
#[derive(Debug)]
struct TitleKey {
    text: Box<str>,
    font_key: u64,
    fallback_epoch: u64,
    raster_px_bits: u32,
    text_px_bits: u32,
}

impl TitleKey {
    /// The key a probe is stored under.
    fn of(probe: &TitleProbe<'_>) -> Self {
        Self {
            text: Box::from(probe.text),
            font_key: probe.font_key,
            fallback_epoch: probe.fallback_epoch,
            raster_px_bits: probe.raster_px.to_bits(),
            text_px_bits: probe.text_px.to_bits(),
        }
    }

    /// Whether `probe` asks for exactly this key; sizes compare bit for bit.
    fn matches(&self, probe: &TitleProbe<'_>) -> bool {
        self.font_key == probe.font_key
            && self.fallback_epoch == probe.fallback_epoch
            && self.raster_px_bits == probe.raster_px.to_bits()
            && self.text_px_bits == probe.text_px.to_bits()
            && *self.text == *probe.text
    }
}

/// One kept title: its key, the run it draws from, and its fitted width.
#[derive(Debug)]
struct PreparedTitle {
    key: TitleKey,
    run: PreparedChromeRun,
    width_px: f32,
}

/// How one title draws this frame: from a kept slot, or from a fit made this frame.
#[derive(Debug)]
pub(crate) enum TitleDraw {
    /// The title cache holds it at this position.
    Cached(usize),
    /// Fitted this frame and not kept.
    Fresh(TitleFit),
}

/// The text, run and width a [`TitleDraw`] draws with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DrawnTitle<'title> {
    /// The drawn text.
    pub(crate) text: &'title str,
    /// The run to draw; `None` when only `text` is known and must be shaped at draw time.
    pub(crate) view: Option<ChromeRunView<'title>>,
    /// Drawn width in raster pixels, which places the badge and the text.
    pub(crate) width_px: f32,
}

/// Tab titles kept by tab position: a fixed table of [`TITLE_SLOTS`] slots, allocated on the
/// first stored title. A slot holds the fitted run of the tab at that position, keyed by
/// everything its fit depends on, so a warm, unchanged title draws with no shaping.
#[derive(Debug, Default)]
pub(crate) struct TitleCache {
    slots: Option<Box<[Option<PreparedTitle>; TITLE_SLOTS]>>,
    limits: AdmissionLimits,
}

impl TitleCache {
    /// Drop every slot at or beyond `tab_count`, so a closed tab's title is not kept.
    pub(crate) fn retain_tabs(&mut self, tab_count: usize) {
        if let Some(slots) = self.slots.as_mut() {
            for slot in slots.iter_mut().skip(tab_count) {
                *slot = None;
            }
        }
    }

    /// Drop every kept title; the table itself stays allocated.
    pub(crate) fn clear(&mut self) {
        if let Some(slots) = self.slots.as_mut() {
            slots.iter_mut().for_each(|slot| *slot = None);
        }
    }

    /// The title at `position`: kept and matching `probe` is a reuse, otherwise it is fitted with
    /// `stack` and, when `reuse` is on, the fit is complete and within the admission limits, kept
    /// at `position`. Counts one `tab_title_reuses` or `tab_title_prepares`.
    pub(crate) fn prepare(
        &mut self,
        position: usize,
        probe: &TitleProbe<'_>,
        stack: &FontStack,
        reuse: bool,
    ) -> TitleDraw {
        let kept =
            self.slots.as_ref().and_then(|slots| slots.get(position)).and_then(Option::as_ref);
        if reuse && kept.is_some_and(|title| title.key.matches(probe)) {
            // When: the slot holds this exact key, the title draws from it without shaping.
            crate::frame_stats::note_tab_title(true);
            return TitleDraw::Cached(position);
        }
        crate::frame_stats::note_tab_title(false);
        let fit = fit_title_run(stack, probe.text, probe.raster_px, probe.text_px);
        let admitted = reuse
            && position < TITLE_SLOTS
            && fit.complete
            && self
                .limits
                .admits(probe.text, fit.run.as_ref().map_or(0, |run| run.view().glyph_count()));
        if !admitted {
            // When: the fit is not `admitted`, a stale slot at this position is dropped and the
            // fit draws this frame only.
            if let Some(slot) = self.slots.as_mut().and_then(|slots| slots.get_mut(position)) {
                *slot = None;
            }
            return TitleDraw::Fresh(fit);
        }
        let TitleFit { run: Some(run), width_px, .. } = fit else {
            // A complete fit always carries its run, so this arm is never reached.
            unreachable!("a complete title fit holds its run");
        };
        let slots = self.slots.get_or_insert_with(|| Box::new(std::array::from_fn(|_| None)));
        slots[position] = Some(PreparedTitle { key: TitleKey::of(probe), run, width_px });
        TitleDraw::Cached(position)
    }

    /// What `draw` draws with, borrowed from this cache or from `draw` itself.
    pub(crate) fn drawn<'title>(&'title self, draw: &'title TitleDraw) -> DrawnTitle<'title> {
        match draw {
            TitleDraw::Cached(position) => {
                let title = self
                    .slots
                    .as_ref()
                    .and_then(|slots| slots[*position].as_ref())
                    .expect("a cached draw names a kept slot");
                DrawnTitle {
                    text: title.run.view().text(),
                    view: Some(title.run.view()),
                    width_px: title.width_px,
                }
            }
            TitleDraw::Fresh(fit) => DrawnTitle {
                text: &fit.text,
                view: fit.run.as_ref().map(PreparedChromeRun::view),
                width_px: fit.width_px,
            },
        }
    }

    /// Whether a title is kept at `position`.
    #[cfg(test)]
    pub(crate) fn is_stored(&self, position: usize) -> bool {
        self.slots.as_ref().and_then(|slots| slots.get(position)).is_some_and(Option::is_some)
    }

    /// Slots in the table: [`TITLE_SLOTS`] once allocated, otherwise 0.
    #[cfg(test)]
    pub(crate) fn slot_count(&self) -> usize {
        self.slots.as_ref().map_or(0, |slots| slots.len())
    }

    /// Heap bytes held: the table, then each kept title's key text and run.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.slots.as_ref().map_or(0, |slots| {
            std::mem::size_of::<[Option<PreparedTitle>; TITLE_SLOTS]>()
                + slots
                    .iter()
                    .flatten()
                    .map(|title| title.key.text.len() + title.run.retained_bytes())
                    .sum::<usize>()
        })
    }

    /// Kept titles.
    pub(crate) fn len(&self) -> usize {
        self.slots.as_ref().map_or(0, |slots| slots.iter().flatten().count())
    }
}

/// The most the title cache can hold: its table plus every slot at the admission limits.
#[cfg(test)]
pub(crate) const fn title_cache_envelope_bytes() -> usize {
    std::mem::size_of::<[Option<PreparedTitle>; TITLE_SLOTS]>()
        + TITLE_SLOTS
            * (MAX_CACHED_TEXT_BYTES
                + MAX_DRAWN_TITLE_BYTES
                + MAX_CACHED_GLYPHS * CHROME_SHAPED_GLYPH_BYTES)
}

/// Which renderer stack shaped a kept chrome run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromeStack {
    /// The body stack.
    Body,
    /// The tab-title stack.
    TabTitle,
    /// The palette-footer stack.
    PaletteFooter,
}

/// What a kept chrome run depends on besides its text: style, sizes and the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChromeRunKey {
    /// Bold and italic.
    pub(crate) attrs: ChromeAttrs,
    /// `font_size_px` bits.
    pub(crate) font_size_bits: u32,
    /// `native_em_px` bits.
    pub(crate) native_em_bits: u32,
    /// The stack that shapes the run.
    pub(crate) stack: ChromeStack,
}

impl ChromeRunKey {
    /// The key of a run shaped by `stack` with `attrs` at `font_size_px` over `native_em_px`.
    pub(crate) fn new(
        stack: ChromeStack,
        attrs: ChromeAttrs,
        font_size_px: f32,
        native_em_px: f32,
    ) -> Self {
        Self {
            attrs,
            font_size_bits: font_size_px.to_bits(),
            native_em_bits: native_em_px.to_bits(),
            stack,
        }
    }

    fn font_size_px(self) -> f32 {
        f32::from_bits(self.font_size_bits)
    }

    fn native_em_px(self) -> f32 {
        f32::from_bits(self.native_em_bits)
    }
}

/// One kept chrome run. The key holds no text: the run's own text is the entry's only copy.
#[derive(Debug)]
struct ChromeRunEntry {
    key: ChromeRunKey,
    run: PreparedChromeRun,
    last_used: u64,
}

/// One chrome-run lookup's result: a kept run, a run shaped this frame and not kept, or nothing
/// when shaping failed.
#[derive(Debug)]
pub(crate) enum ChromeRunHandle {
    /// Kept at this slot.
    Cached(usize),
    /// Shaped this frame but not admitted.
    Fresh(PreparedChromeRun),
    /// The run could not be shaped.
    Failed,
}

/// Constant or slowly changing chrome runs, kept in a fixed table of [`CHROME_RUN_SLOTS`] slots
/// and found by a linear scan on key and text. The search overlay looks its icon and label up
/// here, so a warm overlay shapes neither.
#[derive(Debug, Default)]
pub(crate) struct ChromeRunCache {
    slots: Option<Box<[Option<ChromeRunEntry>; CHROME_RUN_SLOTS]>>,
    clock: u64,
    limits: AdmissionLimits,
}

impl ChromeRunCache {
    /// The run of `text` under `key`: a kept match is a reuse; otherwise `text` is shaped with
    /// `stack` and, when `reuse` is on and it fits the admission limits, kept, replacing an empty
    /// slot or else the least recently used one (the lower slot on a tie). Counts one
    /// `chrome_run_reuses` or `chrome_run_prepares`.
    pub(crate) fn prepare(
        &mut self,
        stack: &FontStack,
        text: &str,
        key: ChromeRunKey,
        reuse: bool,
    ) -> ChromeRunHandle {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        if reuse {
            // When: `reuse` is on, a kept run with this key and text is served before shaping.
            if let Some(slots) = self.slots.as_mut() {
                // When: the `slots` table exists, it is scanned for this key and text.
                let hit = slots.iter_mut().enumerate().find_map(|(index, slot)| {
                    slot.as_mut()
                        .filter(|entry| entry.key == key && entry.run.view().text() == text)
                        .map(|entry| (index, entry))
                });
                if let Some((index, entry)) = hit {
                    // When: `hit` names a slot with this key and text, it is served unshaped.
                    entry.last_used = clock;
                    crate::frame_stats::note_chrome_run(true);
                    return ChromeRunHandle::Cached(index);
                }
            }
        }
        crate::frame_stats::note_chrome_run(false);
        let Some(shaped) =
            ChromeShapedRun::shape(stack, text, key.attrs, key.font_size_px(), key.native_em_px())
        else {
            // When: no run is `shaped`, nothing is kept and the caller keeps its own fallback.
            return ChromeRunHandle::Failed;
        };
        let run = PreparedChromeRun::from_run(shaped);
        if !reuse || !self.limits.admits(text, run.view().glyph_count()) {
            // When: reuse is off or the run is too large, it is drawn this frame and not kept.
            return ChromeRunHandle::Fresh(run);
        }
        let slots = self.slots.get_or_insert_with(|| Box::new(std::array::from_fn(|_| None)));
        let index = slots.iter().position(Option::is_none).unwrap_or_else(|| {
            // Every slot is full, so the least recently used entry is replaced; `min_by_key`
            // keeps the first minimum, so a tie goes to the lower slot.
            slots
                .iter()
                .enumerate()
                .min_by_key(|(_, slot)| slot.as_ref().map_or(0, |entry| entry.last_used))
                .map_or(0, |(index, _)| index)
        });
        slots[index] = Some(ChromeRunEntry { key, run, last_used: clock });
        ChromeRunHandle::Cached(index)
    }

    /// The run `handle` names, borrowed from this cache or from the handle; `None` for a failure.
    pub(crate) fn view<'run>(
        &'run self,
        handle: &'run ChromeRunHandle,
    ) -> Option<ChromeRunView<'run>> {
        match handle {
            ChromeRunHandle::Cached(index) => self
                .slots
                .as_ref()
                .and_then(|slots| slots[*index].as_ref())
                .map(|entry| entry.run.view()),
            ChromeRunHandle::Fresh(run) => Some(run.view()),
            ChromeRunHandle::Failed => None,
        }
    }

    /// Drop every kept run; the table itself stays allocated.
    pub(crate) fn clear(&mut self) {
        if let Some(slots) = self.slots.as_mut() {
            slots.iter_mut().for_each(|slot| *slot = None);
        }
    }

    /// Kept runs.
    pub(crate) fn len(&self) -> usize {
        self.slots.as_ref().map_or(0, |slots| slots.iter().flatten().count())
    }

    /// Heap bytes held: the table, then each kept run's text and glyphs, counted once.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.slots.as_ref().map_or(0, |slots| {
            std::mem::size_of::<[Option<ChromeRunEntry>; CHROME_RUN_SLOTS]>()
                + slots.iter().flatten().map(|entry| entry.run.retained_bytes()).sum::<usize>()
        })
    }

    /// Test seam: lower the admission limits.
    #[cfg(test)]
    pub(crate) fn set_limits_for_test(&mut self, limits: AdmissionLimits) {
        self.limits = limits;
    }
}

/// The most the chrome-run cache can hold: its table plus every slot at the admission limits.
#[cfg(test)]
pub(crate) const fn chrome_run_cache_envelope_bytes() -> usize {
    std::mem::size_of::<[Option<ChromeRunEntry>; CHROME_RUN_SLOTS]>()
        + CHROME_RUN_SLOTS * (MAX_CACHED_TEXT_BYTES + MAX_CACHED_GLYPHS * CHROME_SHAPED_GLYPH_BYTES)
}

/// The UI palette last derived from a theme, kept while the theme's colors are equal.
#[derive(Debug)]
pub(crate) struct PaletteCache {
    /// The colors `palette` was derived from.
    colors: Palette,
    /// `UiPalette::from_theme` of a theme with `colors`; it reads only the colors.
    palette: UiPalette,
    /// Derivations made after seeding, for tests.
    computes: u64,
}

impl PaletteCache {
    /// A cache seeded from `theme`, so the first frame derives nothing.
    pub(crate) fn seeded(theme: &Theme) -> Self {
        Self { colors: theme.colors.clone(), palette: UiPalette::from_theme(theme), computes: 0 }
    }

    /// The UI palette of `theme`: the kept one when `theme`'s colors equal the kept colors,
    /// otherwise derived once and kept.
    pub(crate) fn palette_for(&mut self, theme: &Theme) -> UiPalette {
        if theme.colors != self.colors {
            // The colors changed, so the palette is derived again and kept with them.
            self.palette = UiPalette::from_theme(theme);
            self.colors = theme.colors.clone();
            self.computes += 1;
        }
        self.palette
    }

    /// Derivations made since seeding.
    pub(crate) fn computes(&self) -> u64 {
        self.computes
    }

    /// Heap bytes held: the capacity of every kept color string.
    pub(crate) fn retained_bytes(&self) -> usize {
        palette_hex_capacity(&self.colors)
    }
}

/// Sum of the string capacities of every color in `palette`.
fn palette_hex_capacity(palette: &Palette) -> usize {
    let ansi = |colors: &sonicterm_render_model::boundary::cfg::theme::AnsiColors| {
        [
            &colors.black,
            &colors.red,
            &colors.green,
            &colors.yellow,
            &colors.blue,
            &colors.magenta,
            &colors.cyan,
            &colors.white,
        ]
        .iter()
        .map(|hex| hex.0.capacity())
        .sum::<usize>()
    };
    let tab = &palette.tab;
    [
        &palette.background,
        &palette.foreground,
        &palette.cursor,
        &palette.cursor_text,
        &palette.selection_bg,
        &palette.selection_fg,
        &tab.bar_bg,
        &tab.active_bg,
        &tab.active_fg,
        &tab.inactive_bg,
        &tab.inactive_fg,
        &tab.hover_bg,
        &tab.hover_fg,
        &tab.close_button_fg,
    ]
    .iter()
    .map(|hex| hex.0.capacity())
    .sum::<usize>()
        + ansi(&palette.ansi)
        + ansi(&palette.bright)
}

/// Bytes the envelope allows for the kept palette's color strings. A bundled theme's 30 colors
/// take about 210 bytes; a user theme may write longer strings, so this is an allowance, not a
/// bound, and the live report stays exact.
#[cfg(test)]
pub(crate) const PALETTE_ALLOWANCE_BYTES: usize = 4 * 1024;

/// The `ChromeCache` class envelope per renderer: both run tables full of maximal entries plus
/// the palette allowance. `sonicterm-types` records the same figure; a test ties the two.
#[cfg(test)]
pub(crate) const CHROME_CACHE_ENVELOPE_BYTES: usize =
    title_cache_envelope_bytes() + chrome_run_cache_envelope_bytes() + PALETTE_ALLOWANCE_BYTES;

/// Every chrome cache a renderer owns, reported together as its `chrome_cache` part.
#[derive(Debug)]
pub(crate) struct ChromeCaches {
    /// Tab titles by position.
    pub(crate) titles: TitleCache,
    /// Search-overlay runs.
    pub(crate) runs: ChromeRunCache,
    /// The UI palette.
    pub(crate) palette: PaletteCache,
}

impl ChromeCaches {
    /// Empty run caches and a palette seeded from `theme`.
    pub(crate) fn new(theme: &Theme) -> Self {
        Self {
            titles: TitleCache::default(),
            runs: ChromeRunCache::default(),
            palette: PaletteCache::seeded(theme),
        }
    }

    /// Drop every kept title and run, as a face replacement must.
    pub(crate) fn clear_runs(&mut self) {
        self.titles.clear();
        self.runs.clear();
    }

    /// Heap bytes held by all three caches.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.titles.retained_bytes() + self.runs.retained_bytes() + self.palette.retained_bytes()
    }

    /// Kept titles and runs, the part's item count.
    pub(crate) fn items(&self) -> usize {
        self.titles.len() + self.runs.len()
    }
}
