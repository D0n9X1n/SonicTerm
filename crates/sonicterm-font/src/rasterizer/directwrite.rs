use anyhow::Context;
use config::DisplayPixelGeometry;
use dwrote::{
    DWRITE_TEXTURE_CLEARTYPE_3x1, FontFace, FontFile, GlyphRunAnalysis,
    DWRITE_FONT_SIMULATIONS_NONE, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_RENDERING_MODE_CLEARTYPE_NATURAL_SYMMETRIC,
};
use winapi::um::dwrite::{DWRITE_GLYPH_OFFSET, DWRITE_GLYPH_RUN};

use crate::locator::FontDataSource;
use crate::parser::ParsedFont;
use crate::rasterizer::{freetype::FreeTypeRasterizer, FontRasterizer, RasterizedGlyph};
use crate::units::PixelLength;

const TEXT_COVERAGE_EXPONENT: f32 = 1.20;

pub struct DirectWriteRasterizer {
    face: FontFace,
    fallback: FreeTypeRasterizer,
    scale: f64,
}

impl DirectWriteRasterizer {
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
        let fallback = FreeTypeRasterizer::from_locator(parsed, pixel_geometry)?;
        Ok(Self { face, fallback, scale: parsed.scale.unwrap_or(1.0) })
    }

    fn rasterize_directwrite_glyph(
        &self,
        glyph_pos: u32,
        size: f64,
        dpi: u32,
    ) -> anyhow::Result<RasterizedGlyph> {
        let glyph_index = u16::try_from(glyph_pos).context("DirectWrite glyph id exceeds u16")?;
        let em_size = (size * self.scale * dpi as f64 / 72.0) as f32;
        if em_size <= 0.0 {
            anyhow::bail!("invalid DirectWrite em size {em_size}");
        }

        let glyph_indices = [glyph_index];
        let glyph_advances = [em_size];
        let glyph_offsets = [DWRITE_GLYPH_OFFSET { advanceOffset: 0.0, ascenderOffset: 0.0 }];
        let glyph_run = DWRITE_GLYPH_RUN {
            fontFace: unsafe { self.face.as_ptr() },
            fontEmSize: em_size,
            glyphCount: 1,
            glyphIndices: glyph_indices.as_ptr(),
            glyphAdvances: glyph_advances.as_ptr(),
            glyphOffsets: glyph_offsets.as_ptr(),
            isSideways: 0,
            bidiLevel: 0,
        };

        let render_mode = self.face.get_recommended_rendering_mode_default_params(
            em_size,
            1.0,
            DWRITE_MEASURING_MODE_NATURAL,
        );
        let render_mode = if render_mode == dwrote::DWRITE_RENDERING_MODE_ALIASED
            || render_mode == dwrote::DWRITE_RENDERING_MODE_OUTLINE
            || render_mode == dwrote::DWRITE_RENDERING_MODE_DEFAULT
        {
            DWRITE_RENDERING_MODE_CLEARTYPE_NATURAL_SYMMETRIC
        } else {
            render_mode
        };

        let analysis = GlyphRunAnalysis::create(
            &glyph_run,
            1.0,
            None,
            render_mode,
            DWRITE_MEASURING_MODE_NATURAL,
            0.0,
            0.0,
        )
        .map_err(|hr| anyhow::anyhow!("CreateGlyphRunAnalysis failed: 0x{hr:08x}"))?;
        let bounds = analysis
            .get_alpha_texture_bounds(DWRITE_TEXTURE_CLEARTYPE_3x1)
            .map_err(|hr| anyhow::anyhow!("GetAlphaTextureBounds failed: 0x{hr:08x}"))?;
        let width = (bounds.right - bounds.left).max(0) as usize;
        let height = (bounds.bottom - bounds.top).max(0) as usize;
        if width == 0 || height == 0 {
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
        let texture = analysis
            .create_alpha_texture(DWRITE_TEXTURE_CLEARTYPE_3x1, bounds)
            .map_err(|hr| anyhow::anyhow!("CreateAlphaTexture failed: 0x{hr:08x}"))?;
        let mut data = vec![0u8; width * height * 4];
        for (src, dst) in texture.chunks_exact(3).zip(data.chunks_exact_mut(4)) {
            let r = enhance_text_coverage(src[0]);
            let g = enhance_text_coverage(src[1]);
            let b = enhance_text_coverage(src[2]);
            dst[0] = r;
            dst[1] = g;
            dst[2] = b;
            dst[3] = r.max(g).max(b);
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
        self.rasterize_directwrite_glyph(glyph_pos, size, dpi)
            .or_else(|_| self.fallback.rasterize_glyph(glyph_pos, size, dpi))
    }
}

fn enhance_text_coverage(coverage: u8) -> u8 {
    if coverage == 0 || coverage == u8::MAX {
        return coverage;
    }
    let c = coverage as f32 / 255.0;
    ((1.0 - (1.0 - c).powf(TEXT_COVERAGE_EXPONENT)) * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
#[path = "directwrite/tests.rs"]
mod tests;
