use anyhow::Context;
use config::DisplayPixelGeometry;
use dwrote::{
    DWRITE_TEXTURE_CLEARTYPE_3x1, FontFace, FontFile, GlyphRunAnalysis,
    DWRITE_FONT_SIMULATIONS_NONE, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_RENDERING_MODE_CLEARTYPE_NATURAL_SYMMETRIC,
};
use winapi::um::dwrite::{
    DWriteCreateFactory, DWRITE_FACTORY_TYPE_SHARED, DWRITE_GLYPH_OFFSET, DWRITE_GLYPH_RUN,
};
use winapi::um::dwrite_1::DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE;
use winapi::um::dwrite_2::{IDWriteFactory2, DWRITE_GRID_FIT_MODE_DISABLED};
use winapi::Interface;
use wio::com::ComPtr;

use crate::locator::FontDataSource;
use crate::parser::ParsedFont;
use crate::rasterizer::{
    checked_glyph_rgba_len, checked_raster_pixel_size, freetype::FreeTypeRasterizer,
    FontRasterizer, RasterizedGlyph,
};
use crate::units::PixelLength;

pub struct DirectWriteRasterizer {
    factory: ComPtr<IDWriteFactory2>,
    face: FontFace,
    fallback: FreeTypeRasterizer,
    scale: f64,
}

impl DirectWriteRasterizer {
    /// Build a DirectWrite rasterizer from an on-disk font, retaining FreeType as the glyph fallback.
    pub fn from_locator(
        parsed: &ParsedFont,
        pixel_geometry: DisplayPixelGeometry,
    ) -> anyhow::Result<Self> {
        let FontDataSource::OnDisk(path) = &parsed.handle.source else {
            anyhow::bail!("DirectWrite rasterizer requires an on-disk font source");
        };
        let file = FontFile::new_from_path(path)
            .with_context(|| format!("DirectWrite could not open font file {}", path.display()))?;
        let face = file
            .create_face(parsed.handle.index(), DWRITE_FONT_SIMULATIONS_NONE)
            .map_err(|hr| anyhow::anyhow!("DirectWrite CreateFontFace failed: 0x{hr:08x}"))?;
        let mut factory = std::ptr::null_mut();
        let hr =
            // SAFETY: factory receives the owned COM interface selected by the IDWriteFactory2 IID.
            unsafe {
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, &IDWriteFactory2::uuidof(), &mut factory)
            };
        if hr < 0 {
            // A failed factory cannot provide explicit grid-fit control.
            anyhow::bail!("DirectWrite factory creation failed: 0x{hr:08x}");
        }
        let factory =
            // SAFETY: successful DWriteCreateFactory returns one owned IDWriteFactory2 reference.
            unsafe { ComPtr::from_raw(factory.cast::<IDWriteFactory2>()) };
        let fallback = FreeTypeRasterizer::from_locator(parsed, pixel_geometry)?;
        Ok(Self { factory, face, fallback, scale: parsed.scale.unwrap_or(1.0) })
    }

    fn rasterize_directwrite_glyph(
        &self,
        glyph_pos: u32,
        size: f64,
        dpi: u32,
    ) -> anyhow::Result<RasterizedGlyph> {
        let glyph_index = u16::try_from(glyph_pos).context("DirectWrite glyph id exceeds u16")?;
        let em_size = checked_raster_pixel_size(size, self.scale, dpi)? as f32;

        let glyph_indices = [glyph_index];
        let glyph_advances = [em_size];
        let glyph_offsets = [DWRITE_GLYPH_OFFSET { advanceOffset: 0.0, ascenderOffset: 0.0 }];
        let glyph_run = DWRITE_GLYPH_RUN {
            fontFace:
                // SAFETY: `self.face` owns a live font face through the synchronous analysis call below.
                unsafe { self.face.as_ptr() },
            fontEmSize: em_size,
            glyphCount: 1,
            glyphIndices: glyph_indices.as_ptr(),
            glyphAdvances: glyph_advances.as_ptr(),
            glyphOffsets: glyph_offsets.as_ptr(),
            isSideways: 0,
            bidiLevel: 0,
        };

        let mut analysis = std::ptr::null_mut();
        // Grid fitting can contract equal-height outlines differently; preserve their design-space alignment.
        let hr =
            // SAFETY: factory, face, glyph arrays, and output pointer remain live through this synchronous call.
            unsafe {
                self.factory.CreateGlyphRunAnalysis(
                    &glyph_run,
                    std::ptr::null(),
                    DWRITE_RENDERING_MODE_CLEARTYPE_NATURAL_SYMMETRIC,
                    DWRITE_MEASURING_MODE_NATURAL,
                    DWRITE_GRID_FIT_MODE_DISABLED,
                    DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE,
                    0.0,
                    0.0,
                    &mut analysis,
                )
            };
        if hr < 0 {
            // Analysis failure retains the caller's FreeType fallback path.
            anyhow::bail!("CreateGlyphRunAnalysis failed: 0x{hr:08x}");
        }
        let analysis = GlyphRunAnalysis::take(
            // SAFETY: successful CreateGlyphRunAnalysis transfers one owned interface reference.
            unsafe { ComPtr::from_raw(analysis) },
        );
        let bounds = analysis
            .get_alpha_texture_bounds(DWRITE_TEXTURE_CLEARTYPE_3x1)
            .map_err(|hr| anyhow::anyhow!("GetAlphaTextureBounds failed: 0x{hr:08x}"))?;
        let width = (i64::from(bounds.right) - i64::from(bounds.left)).max(0) as usize;
        let height = (i64::from(bounds.bottom) - i64::from(bounds.top)).max(0) as usize;
        if width == 0 || height == 0 {
            // When: `width` or `height` is zero, return an empty glyph without requesting an alpha texture.
            return Ok(RasterizedGlyph {
                data: Vec::new(),
                width,
                height,
                bearing_x: PixelLength::new(0.0),
                bearing_y: PixelLength::new(0.0),
                has_color: false,
                is_scaled: true,
            });
        }
        let data_len = checked_glyph_rgba_len(width, height)?;
        let texture = analysis
            .create_alpha_texture(DWRITE_TEXTURE_CLEARTYPE_3x1, bounds)
            .map_err(|hr| anyhow::anyhow!("CreateAlphaTexture failed: 0x{hr:08x}"))?;
        let mut data = vec![0u8; data_len];
        for (src, dst) in
            texture.as_chunks::<3>().0.iter().zip(data.as_chunks_mut::<4>().0.iter_mut())
        {
            *dst = directwrite_coverage_pixel(*src);
        }

        Ok(RasterizedGlyph {
            data,
            width,
            height,
            bearing_x: PixelLength::new(bounds.left as f64),
            bearing_y: PixelLength::new(-(bounds.top as f64)),
            has_color: false,
            is_scaled: true,
        })
    }
}

impl FontRasterizer for DirectWriteRasterizer {
    fn rasterize_glyph(
        &self,
        glyph_pos: u32,
        size: f64,
        dpi: u32,
    ) -> anyhow::Result<RasterizedGlyph> {
        if self.fallback.has_color {
            // When: has_color identifies artwork support, a ClearType mask would discard its palette and misclassify its pixels.
            return self.fallback.rasterize_glyph(glyph_pos, size, dpi);
        }
        self.rasterize_directwrite_glyph(glyph_pos, size, dpi)
            .or_else(|_| self.fallback.rasterize_glyph(glyph_pos, size, dpi))
    }
}

fn directwrite_coverage_pixel([red, green, blue]: [u8; 3]) -> [u8; 4] {
    [red, green, blue, red.max(green).max(blue)]
}

#[cfg(test)]
#[path = "directwrite_tests.rs"]
mod directwrite_tests;
