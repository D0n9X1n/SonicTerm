//! Integration-test seam over the renderer's chrome caches.
//!
//! Integration tests cannot reach the crate-private caches, so this hidden module wraps a title
//! cache and a chrome-run cache behind the same calls the renderer makes: prepare a title or run,
//! then draw it with `layout_view`. It is not a supported API.

use sonicterm_engine::FontStack;
use sonicterm_text::glyph_atlas::{GlyphAtlas, Rasterizer};
use sonicterm_types::GlyphRasterVariant;

pub use crate::chrome_cache::ChromeStack;
use crate::chrome_cache::{ChromeRunCache, ChromeRunKey, TitleCache, TitleProbe};
use crate::chrome_text::{layout_view, ChromeAttrs, ChromeTextLayout};
use crate::color::ChromeColor;

/// What one prepared title reports: the cache's retained bytes and the title's glyph count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedTitleProbe {
    /// Bytes the title cache reports after the preparation.
    pub reported_bytes: usize,
    /// Glyphs in the title's drawn run; 0 when it has no run.
    pub glyph_count: usize,
}

/// Where and how a seam draw paints: the atlas rasterizer and atlas, color, origin and surface.
pub struct SeamDraw<'draw, Raster: Rasterizer> {
    /// Rasterizer backing the atlas.
    pub raster: &'draw mut Raster,
    /// Glyph atlas the tiles land in.
    pub atlas: &'draw mut GlyphAtlas,
    /// Monochrome glyph color.
    pub color: ChromeColor,
    /// `(x, baseline_y)` in raster px.
    pub origin: (f32, f32),
    /// Surface size in raster px.
    pub screen: (f32, f32),
}

/// A title cache and a chrome-run cache, driven as the renderer drives them.
#[derive(Default)]
pub struct ChromeCacheSeam {
    titles: TitleCache,
    runs: ChromeRunCache,
}

impl ChromeCacheSeam {
    /// Empty caches.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Prepare the title `text` at tab `position`, fitted into `text_px` at `raster_px`.
    pub fn prepare_tab_title(
        &mut self,
        stack: &FontStack,
        position: usize,
        text: &str,
        raster_px: f32,
        text_px: f32,
    ) -> PreparedTitleProbe {
        let probe = title_probe(text, raster_px, text_px);
        let draw = self.titles.prepare(position, &probe, stack, true);
        let glyph_count = self.titles.drawn(&draw).view.map_or(0, |view| view.glyph_count());
        PreparedTitleProbe { reported_bytes: self.titles.retained_bytes(), glyph_count }
    }

    /// Prepare the title as [`Self::prepare_tab_title`] does and draw it with `layout_view`, as
    /// the tab bar does; a warm title draws without shaping.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_prepared_title<Raster: Rasterizer>(
        &mut self,
        stack: &FontStack,
        position: usize,
        text: &str,
        raster_px: f32,
        text_px: f32,
        draw: SeamDraw<'_, Raster>,
    ) -> ChromeTextLayout {
        let probe = title_probe(text, raster_px, text_px);
        let title_draw = self.titles.prepare(position, &probe, stack, true);
        let drawn = self.titles.drawn(&title_draw);
        let view = drawn.view.expect("the seam draws only titles that shaped");
        layout_view(
            view,
            draw.raster,
            draw.atlas,
            draw.color,
            draw.origin,
            draw.screen,
            None,
            GlyphRasterVariant::TabTitle,
        )
    }

    /// Look `text` up in the chrome-run cache under the body stack at `font_size_px` and draw it
    /// with `layout_view`, as the search overlay does; a warm run draws without shaping.
    pub fn draw_chrome_run<Raster: Rasterizer>(
        &mut self,
        stack: &FontStack,
        text: &str,
        font_size_px: f32,
        draw: SeamDraw<'_, Raster>,
    ) -> ChromeTextLayout {
        let key = ChromeRunKey::new(
            ChromeStack::Body,
            ChromeAttrs::default(),
            font_size_px,
            font_size_px,
        );
        let handle = self.runs.prepare(stack, text, key, true);
        let view = self.runs.view(&handle).expect("the seam draws only runs that shaped");
        layout_view(
            view,
            draw.raster,
            draw.atlas,
            draw.color,
            draw.origin,
            draw.screen,
            None,
            GlyphRasterVariant::Normal,
        )
    }

    /// Bytes both caches report.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.titles.retained_bytes() + self.runs.retained_bytes()
    }
}

/// The probe of a title under a fixed font key and epoch.
fn title_probe(text: &str, raster_px: f32, text_px: f32) -> TitleProbe<'_> {
    TitleProbe { text, font_key: 0, fallback_epoch: 0, raster_px, text_px }
}
