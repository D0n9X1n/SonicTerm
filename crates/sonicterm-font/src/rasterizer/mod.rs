use crate::parser::ParsedFont;
use crate::units::*;
use config::FontRasterizerSelection;
use image::{ImageBuffer, Rgba};

/// The amount, as a number in [0,1], to horizontally skew a glyph when rendering synthetic
/// italics
pub(crate) const FAKE_ITALIC_SKEW: f64 = 0.2;

pub mod colr;
#[cfg(windows)]
pub mod directwrite;
pub mod freetype;
pub mod harfbuzz;

/// Largest glyph bitmap dimension accepted by the terminal atlas.
pub const MAX_RASTERIZED_GLYPH_DIMENSION: usize = 2048;
/// Largest RGBA glyph payload accepted before atlas insertion.
pub const MAX_RASTERIZED_GLYPH_BYTES: usize =
    MAX_RASTERIZED_GLYPH_DIMENSION * MAX_RASTERIZED_GLYPH_DIMENSION * 4;

/// Validate glyph dimensions and return the checked RGBA byte length.
pub fn checked_glyph_rgba_len(width: usize, height: usize) -> anyhow::Result<usize> {
    if width > MAX_RASTERIZED_GLYPH_DIMENSION || height > MAX_RASTERIZED_GLYPH_DIMENSION {
        anyhow::bail!(
            "glyph bitmap {width}x{height} exceeds {}x{} atlas limit",
            MAX_RASTERIZED_GLYPH_DIMENSION,
            MAX_RASTERIZED_GLYPH_DIMENSION
        );
    }
    let bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| anyhow::anyhow!("glyph bitmap byte length overflow for {width}x{height}"))?;
    if bytes > MAX_RASTERIZED_GLYPH_BYTES {
        anyhow::bail!("glyph bitmap requires {bytes} bytes, limit is {MAX_RASTERIZED_GLYPH_BYTES}");
    }
    Ok(bytes)
}

/// Convert a signed FreeType 26.6 extent to pixels and enforce the glyph limit.
pub fn checked_freetype_26_6_extent(extent: i64) -> anyhow::Result<usize> {
    let magnitude = extent
        .checked_abs()
        .ok_or_else(|| anyhow::anyhow!("FreeType 26.6 extent overflow: {extent}"))?
        as u64;
    let pixels = magnitude
        .checked_add(63)
        .ok_or_else(|| anyhow::anyhow!("FreeType 26.6 extent overflow: {extent}"))?
        / 64;
    if pixels > MAX_RASTERIZED_GLYPH_DIMENSION as u64 {
        anyhow::bail!(
            "FreeType outline extent {pixels}px exceeds {}px glyph limit",
            MAX_RASTERIZED_GLYPH_DIMENSION
        );
    }
    usize::try_from(pixels).map_err(|_| anyhow::anyhow!("FreeType extent exceeds usize"))
}

/// Validate the requested point-size/DPI conversion before native font calls.
pub fn checked_raster_pixel_size(point_size: f64, scale: f64, dpi: u32) -> anyhow::Result<f64> {
    let pixels = point_size * scale * f64::from(dpi) / 72.0;
    if !pixels.is_finite() || pixels <= 0.0 || pixels > MAX_RASTERIZED_GLYPH_DIMENSION as f64 {
        anyhow::bail!("invalid raster pixel size {pixels}");
    }
    Ok(pixels)
}

/// A bitmap representation of a glyph.
/// The data is stored as pre-multiplied RGBA 32bpp.
#[derive(Debug)]
pub struct RasterizedGlyph {
    pub data: Vec<u8>,
    pub height: usize,
    pub width: usize,
    pub bearing_x: PixelLength,
    pub bearing_y: PixelLength,
    pub has_color: bool,
    /// if true, glyphcache shouldn't need to scale the
    /// glyph to match metrics
    pub is_scaled: bool,
}

/// Rasterizes the specified glyph index in the associated font
/// and returns the generated bitmap
pub trait FontRasterizer {
    /// Rasterize one glyph at the requested point size and DPI.
    fn rasterize_glyph(
        &self,
        glyph_pos: u32,
        size: f64,
        dpi: u32,
    ) -> anyhow::Result<RasterizedGlyph>;
}

/// Construct the selected rasterizer, using FreeType as the non-Windows DirectWrite fallback.
pub fn new_rasterizer(
    rasterizer: FontRasterizerSelection,
    handle: &ParsedFont,
    pixel_geometry: config::DisplayPixelGeometry,
) -> anyhow::Result<Box<dyn FontRasterizer>> {
    match rasterizer {
        FontRasterizerSelection::FreeType => {
            Ok(Box::new(freetype::FreeTypeRasterizer::from_locator(handle, pixel_geometry)?))
        }
        FontRasterizerSelection::Harfbuzz => {
            Ok(Box::new(harfbuzz::HarfbuzzRasterizer::from_locator(handle)?))
        }
        FontRasterizerSelection::DirectWrite => {
            #[cfg(windows)]
            {
                directwrite::DirectWriteRasterizer::from_locator(handle, pixel_geometry)
                    .map(|r| Box::new(r) as Box<dyn FontRasterizer>)
                    .or_else(|_| {
                        freetype::FreeTypeRasterizer::from_locator(handle, pixel_geometry)
                            .map(|r| Box::new(r) as Box<dyn FontRasterizer>)
                    })
            }
            #[cfg(not(windows))]
            {
                Ok(Box::new(freetype::FreeTypeRasterizer::from_locator(handle, pixel_geometry)?))
            }
        }
    }
}

pub(crate) fn swap_red_and_blue<Container: std::ops::Deref<Target = [u8]> + std::ops::DerefMut>(
    image: &mut ImageBuffer<Rgba<u8>, Container>,
) {
    for pixel in image.pixels_mut() {
        let red = pixel[0];
        pixel[0] = pixel[2];
        pixel[2] = red;
    }
}

pub(crate) fn crop_to_non_transparent<Container>(
    image: &mut image::ImageBuffer<Rgba<u8>, Container>,
) -> image::SubImage<&mut ImageBuffer<Rgba<u8>, Container>>
where
    Container: std::ops::Deref<Target = [u8]>,
{
    let width = image.width();
    let height = image.height();

    let mut first_line = None;
    let mut first_col = None;
    let mut last_col = None;
    let mut last_line = None;

    for (y, row) in image.rows().enumerate() {
        for (x, pixel) in row.enumerate() {
            let alpha = pixel[3];
            if alpha != 0 {
                if first_line.is_none() {
                    first_line = Some(y);
                }
                first_col = match first_col.take() {
                    Some(other) if x < other => Some(x),
                    Some(other) => Some(other),
                    None => Some(x),
                };
            }
        }
    }
    for (y, row) in image.rows().enumerate().rev() {
        for (x, pixel) in row.enumerate().rev() {
            let alpha = pixel[3];
            if alpha != 0 {
                if last_line.is_none() {
                    last_line = Some(y);
                }
                last_col = match last_col.take() {
                    Some(other) if x > other => Some(x),
                    Some(other) => Some(other),
                    None => Some(x),
                };
            }
        }
    }

    let first_col = first_col.unwrap_or(0) as u32;
    let first_line = first_line.unwrap_or(0) as u32;
    let last_col = last_col.unwrap_or(width as usize) as u32;
    let last_line = last_line.unwrap_or(height as usize) as u32;

    image::imageops::crop(
        image,
        first_col,
        first_line,
        last_col - first_col,
        last_line - first_line,
    )
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
