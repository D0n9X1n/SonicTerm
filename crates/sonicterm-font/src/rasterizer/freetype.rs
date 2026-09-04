use crate::color::{linear_u8_to_srgb8, SrgbaPixel};
use crate::ftwrap::{
    composite_mode_to_operator, vector_x_y, FT_Affine23, FT_ColorIndex, FT_ColorLine, FT_ColorStop,
    FT_Fixed, FT_Get_Colorline_Stops, FT_Int32, FT_PaintExtend, IsColr1OrLater, IsSvg,
    SelectedFontSize, FT_LOAD_NO_HINTING,
};
use crate::parser::ParsedFont;
use crate::rasterizer::colr::{
    apply_draw_ops_to_context, paint_linear_gradient, paint_radial_gradient, paint_sweep_gradient,
    ColorLine, ColorStop, PaintOp,
};
use crate::rasterizer::harfbuzz::{argb_to_rgba, HarfbuzzRasterizer};
use crate::rasterizer::{
    checked_glyph_rgba_len, checked_raster_pixel_size, FontRasterizer, FAKE_ITALIC_SKEW,
    MAX_RASTERIZED_GLYPH_BYTES,
};
use crate::units::*;
use crate::{ftwrap, FontRasterizerSelection, RasterizedGlyph};
use ::freetype::{
    FT_Color_Root_Transform, FT_GlyphSlotRec_, FT_Matrix, FT_Opaque_Paint_, FT_PaintFormat_,
};
use anyhow::{bail, Context as _};
use cairo::{Content, Context, Extend, Format, ImageSurface, Matrix, Operator, RecordingSurface};
use config::{DisplayPixelGeometry, FreeTypeLoadFlags, FreeTypeLoadTarget};
use std::cell::RefCell;
use std::f64::consts::PI;
use std::mem;
use std::mem::MaybeUninit;

pub struct FreeTypeRasterizer {
    has_color: bool,
    face: RefCell<ftwrap::Face>,
    _lib: ftwrap::Library,
    synthesize_bold: bool,
    freetype_load_target: Option<FreeTypeLoadTarget>,
    freetype_render_target: Option<FreeTypeLoadTarget>,
    freetype_load_flags: Option<FreeTypeLoadFlags>,
    display_pixel_geometry: DisplayPixelGeometry,
    scale: f64,
    hb_raster: HarfbuzzRasterizer,
}

// SAFETY: `slot.bitmap.buffer` must name `source_len` initialized bytes for the lifetime of `slot`.
unsafe fn glyph_bitmap_data(slot: &FT_GlyphSlotRec_, source_len: usize) -> &[u8] {
    if slot.bitmap.buffer.is_null() {
        &[]
    } else {
        // When: bitmap.buffer is non-null, borrow its initialized extent while this slot lives.
        // SAFETY: the caller binds the initialized bitmap extent to this glyph-slot borrow.
        unsafe { std::slice::from_raw_parts(slot.bitmap.buffer, source_len) }
    }
}

fn owned_bgra_image(
    width: u32,
    height: u32,
    data: &[u8],
) -> Option<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>> {
    image::ImageBuffer::from_raw(width, height, data.to_vec())
}

impl FontRasterizer for FreeTypeRasterizer {
    fn rasterize_glyph(
        &self,
        glyph_pos: u32,
        size: f64,
        dpi: u32,
    ) -> anyhow::Result<RasterizedGlyph> {
        checked_raster_pixel_size(size, self.scale, dpi)?;
        let SelectedFontSize { width, height, is_scaled, .. } =
            self.face.borrow_mut().set_font_size(size * self.scale, dpi)?;
        if !width.is_finite() || !height.is_finite() || width < 0.0 || height < 0.0 {
            bail!("invalid selected FreeType strike dimensions {width}x{height}");
        }
        checked_glyph_rgba_len(width.ceil() as usize, height.ceil() as usize)?;

        let (load_flags, render_mode) = ftwrap::compute_load_flags_from_config(
            self.freetype_load_flags,
            self.freetype_load_target,
            self.freetype_render_target,
            Some(dpi),
        );

        let mut face = self.face.borrow_mut();
        let ft_glyph = match face.load_and_render_glyph(
            glyph_pos,
            load_flags,
            render_mode,
            self.synthesize_bold,
        ) {
            Ok(g) => g,
            Err(err) => {
                // When: err came back from load_and_render_glyph, so the glyph
                // may still be renderable by another path.
                if err.root_cause().downcast_ref::<IsSvg>().is_some()
                    || err.root_cause().downcast_ref::<IsColr1OrLater>().is_some()
                {
                    // When: the root_cause is an SVG or COLRv1 glyph, which
                    // FreeType cannot raster, so a colour path takes over.
                    drop(face);

                    let config = config::configuration();
                    match config.font_colr_rasterizer {
                        FontRasterizerSelection::FreeType => {
                            // When: FreeType is the configured COLR rasterizer,
                            // so outlines are walked with hinting disabled.
                            return self.rasterize_outlines(
                                glyph_pos,
                                load_flags | FT_LOAD_NO_HINTING as i32,
                            );
                        }
                        FontRasterizerSelection::Harfbuzz
                        | FontRasterizerSelection::DirectWrite => {
                            // When: Harfbuzz or DirectWrite is configured, so
                            // the HarfBuzz painter renders this glyph instead.
                            return self.hb_raster.rasterize_glyph(glyph_pos, size, dpi);
                        }
                    }
                }
                return Err(err);
            }
        };

        let mode: ftwrap::FT_Pixel_Mode =
            // SAFETY: pixel_mode is the FT_Pixel_Mode discriminant FreeType
            // just wrote into this slot, so it is a valid variant of that enum.
            unsafe { mem::transmute(u32::from(ft_glyph.bitmap.pixel_mode)) };

        // pitch is the number of bytes per source row
        let pitch = ft_glyph.bitmap.pitch.unsigned_abs() as usize;
        let raw_width =
            usize::try_from(ft_glyph.bitmap.width).context("negative FreeType bitmap width")?;
        let raw_rows =
            usize::try_from(ft_glyph.bitmap.rows).context("negative FreeType bitmap height")?;
        let (output_width, output_height) = match mode {
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_LCD => (raw_width / 3, raw_rows),
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_LCD_V => (raw_width, raw_rows / 3),
            _ => (raw_width, raw_rows),
        };
        checked_glyph_rgba_len(output_width, output_height)?;
        let source_len =
            raw_rows.checked_mul(pitch).context("FreeType bitmap source length overflow")?;
        if source_len > MAX_RASTERIZED_GLYPH_BYTES {
            bail!(
                "FreeType bitmap source requires {source_len} bytes, limit is {MAX_RASTERIZED_GLYPH_BYTES}"
            );
        }
        let data =
            // SAFETY: source_len is rows*pitch, the extent FreeType allocated
            // for this bitmap, and the borrowed face keeps the slot alive.
            unsafe { glyph_bitmap_data(ft_glyph, source_len) };

        let glyph = match mode {
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_LCD => {
                self.rasterize_lcd(pitch, ft_glyph, data, is_scaled)
            }
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_LCD_V => {
                self.rasterize_lcd_v(pitch, ft_glyph, data, is_scaled)
            }
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_BGRA => {
                self.rasterize_bgra(pitch, ft_glyph, data, is_scaled)?
            }
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_GRAY => {
                self.rasterize_gray(pitch, ft_glyph, data, is_scaled)
            }
            ftwrap::FT_Pixel_Mode::FT_PIXEL_MODE_MONO => {
                self.rasterize_mono(pitch, ft_glyph, data, is_scaled)
            }
            mode => bail!("unhandled pixel mode: {:?}", mode),
        };

        Ok(glyph)
    }
}

impl FreeTypeRasterizer {
    fn rasterize_mono(
        &self,
        pitch: usize,
        ft_glyph: &FT_GlyphSlotRec_,
        data: &[u8],
        is_scaled: bool,
    ) -> RasterizedGlyph {
        let width = ft_glyph.bitmap.width as usize;
        let height = ft_glyph.bitmap.rows as usize;
        let size = checked_glyph_rgba_len(width, height).expect("glyph size prevalidated");
        let mut rgba = vec![0u8; size];
        for y in 0..height {
            let src_offset = y * pitch;
            let dest_offset = y * width * 4;
            let mut x = 0;
            for i in 0..pitch {
                if x >= width {
                    // When: x reached width, so the remaining bytes in this row
                    // are pitch padding rather than glyph pixels.
                    break;
                }
                let mut b = data[src_offset + i];
                for _ in 0..8 {
                    if x >= width {
                        // When: x reached width mid-byte, so the low bits of
                        // this byte are padding past the glyph edge.
                        break;
                    }
                    if b & 0x80 == 0x80 {
                        for j in 0..4 {
                            rgba[dest_offset + (x * 4) + j] = 0xff;
                        }
                    }
                    b <<= 1;
                    x += 1;
                }
            }
        }
        RasterizedGlyph {
            data: rgba,
            height,
            width,
            bearing_x: PixelLength::new(ft_glyph.bitmap_left as f64),
            bearing_y: PixelLength::new(ft_glyph.bitmap_top as f64),
            has_color: false,
            is_scaled,
        }
    }

    fn rasterize_gray(
        &self,
        pitch: usize,
        ft_glyph: &FT_GlyphSlotRec_,
        data: &[u8],
        is_scaled: bool,
    ) -> RasterizedGlyph {
        let width = ft_glyph.bitmap.width as usize;
        let height = ft_glyph.bitmap.rows as usize;
        let size = checked_glyph_rgba_len(width, height).expect("glyph size prevalidated");
        let mut rgba = vec![0u8; size];
        for y in 0..height {
            let src_offset = y * pitch;
            let dest_offset = y * width * 4;
            for x in 0..width {
                let linear_gray = data[src_offset + x];
                let gray = linear_u8_to_srgb8(linear_gray);

                // Texture is SRGBA, which in OpenGL means
                // that the RGB values are gamma adjusted
                // non-linear values, but the A value is
                // linear!

                rgba[dest_offset + (x * 4)] = gray;
                rgba[dest_offset + (x * 4) + 1] = gray;
                rgba[dest_offset + (x * 4) + 2] = gray;
                rgba[dest_offset + (x * 4) + 3] = linear_gray;
            }
        }
        RasterizedGlyph {
            data: rgba,
            height,
            width,
            bearing_x: PixelLength::new(ft_glyph.bitmap_left as f64),
            bearing_y: PixelLength::new(ft_glyph.bitmap_top as f64),
            has_color: false,
            is_scaled,
        }
    }

    fn rasterize_lcd(
        &self,
        pitch: usize,
        ft_glyph: &FT_GlyphSlotRec_,
        data: &[u8],
        is_scaled: bool,
    ) -> RasterizedGlyph {
        let width = ft_glyph.bitmap.width as usize / 3;
        let height = ft_glyph.bitmap.rows as usize;
        let size = checked_glyph_rgba_len(width, height).expect("glyph size prevalidated");
        let mut rgba = vec![0u8; size];
        for y in 0..height {
            let src_offset = y * pitch;
            let dest_offset = y * width * 4;
            for x in 0..width {
                let red = data[src_offset + (x * 3)];
                let green = data[src_offset + (x * 3) + 1];
                let blue = data[src_offset + (x * 3) + 2];

                let linear_alpha = red.max(green).max(blue);

                // Texture is SRGBA, which in OpenGL means
                // that the RGB values are gamma adjusted
                // non-linear values, but the A value is
                // linear!

                let red = linear_u8_to_srgb8(red);
                let green = linear_u8_to_srgb8(green);
                let blue = linear_u8_to_srgb8(blue);

                let (red, blue) = match self.display_pixel_geometry {
                    DisplayPixelGeometry::RGB => (red, blue),
                    DisplayPixelGeometry::BGR => (blue, red),
                };

                rgba[dest_offset + (x * 4)] = red;
                rgba[dest_offset + (x * 4) + 1] = green;
                rgba[dest_offset + (x * 4) + 2] = blue;
                rgba[dest_offset + (x * 4) + 3] = linear_alpha;
            }
        }

        RasterizedGlyph {
            data: rgba,
            height,
            width,
            bearing_x: PixelLength::new(ft_glyph.bitmap_left as f64),
            bearing_y: PixelLength::new(ft_glyph.bitmap_top as f64),
            has_color: false,
            is_scaled,
        }
    }

    fn rasterize_lcd_v(
        &self,
        pitch: usize,
        ft_glyph: &FT_GlyphSlotRec_,
        data: &[u8],
        is_scaled: bool,
    ) -> RasterizedGlyph {
        let width = ft_glyph.bitmap.width as usize;
        let height = ft_glyph.bitmap.rows as usize / 3;
        let size = checked_glyph_rgba_len(width, height).expect("glyph size prevalidated");
        let mut rgba = vec![0u8; size];
        for y in 0..height {
            let src_offset = y * pitch * 3;
            let dest_offset = y * width * 4;
            for x in 0..width {
                let red = data[src_offset + x];
                let green = data[src_offset + x + pitch];
                let blue = data[src_offset + x + 2 * pitch];

                let linear_alpha = red.max(green).max(blue);

                // Texture is SRGBA, which in OpenGL means
                // that the RGB values are gamma adjusted
                // non-linear values, but the A value is
                // linear!

                let red = linear_u8_to_srgb8(red);
                let green = linear_u8_to_srgb8(green);
                let blue = linear_u8_to_srgb8(blue);

                let (red, blue) = match self.display_pixel_geometry {
                    DisplayPixelGeometry::RGB => (red, blue),
                    DisplayPixelGeometry::BGR => (blue, red),
                };

                rgba[dest_offset + (x * 4)] = red;
                rgba[dest_offset + (x * 4) + 1] = green;
                rgba[dest_offset + (x * 4) + 2] = blue;
                rgba[dest_offset + (x * 4) + 3] = linear_alpha;
            }
        }

        RasterizedGlyph {
            data: rgba,
            height,
            width,
            bearing_x: PixelLength::new(ft_glyph.bitmap_left as f64),
            bearing_y: PixelLength::new(ft_glyph.bitmap_top as f64),
            has_color: false,
            is_scaled,
        }
    }

    fn rasterize_bgra(
        &self,
        pitch: usize,
        ft_glyph: &FT_GlyphSlotRec_,
        data: &[u8],
        is_scaled: bool,
    ) -> anyhow::Result<RasterizedGlyph> {
        let width = ft_glyph.bitmap.width as usize;
        let height = ft_glyph.bitmap.rows as usize;

        if width == 0 || height == 0 {
            // When: width or height is zero, so there is no ink to copy.

            // Handle this case separately; the ImageBuffer
            // constructor doesn't like 0-size dimensions
            return Ok(RasterizedGlyph {
                data: vec![],
                height: 0,
                width: 0,
                bearing_x: PixelLength::new(0.),
                bearing_y: PixelLength::new(0.),
                has_color: false,
                is_scaled,
            });
        }

        let mut source_image =
            owned_bgra_image(width as u32, height as u32, data).with_context(|| {
                format!(
                    "build image from data with \
                 width={width}, height={height} and pitch={pitch}.\
                 Expected pitch={}. format is {:?}",
                    width * 4,
                    ft_glyph.format
                )
            })?;

        // emoji glyphs don't always fill the bitmap size, so we compute
        // the non-transparent bounds

        let mut cropped = crate::rasterizer::crop_to_non_transparent(&mut source_image).to_image();
        crate::rasterizer::swap_red_and_blue(&mut cropped);

        let dest_width = cropped.width() as usize;
        let dest_height = cropped.height() as usize;

        Ok(RasterizedGlyph {
            data: cropped.into_vec(),
            height: dest_height,
            width: dest_width,
            bearing_x: PixelLength::new(
                f64::from(ft_glyph.bitmap_left) * (dest_width as f64 / width as f64),
            ),
            bearing_y: PixelLength::new(
                f64::from(ft_glyph.bitmap_top) * (dest_height as f64 / height as f64),
            ),
            has_color: self.has_color,
            is_scaled,
        })
    }

    /// Open the font behind `parsed`'s locator handle for FreeType rasterizing.
    ///
    /// Owns its own `FT_Library` and face, applies the synthetic-italic
    /// transform when `parsed` requests it, and builds the HarfBuzz rasterizer
    /// used as the fallback for SVG and COLRv1 glyphs FreeType cannot render.
    /// `display_pixel_geometry` selects the subpixel order used for LCD modes.
    pub fn from_locator(
        parsed: &ParsedFont,
        display_pixel_geometry: DisplayPixelGeometry,
    ) -> anyhow::Result<Self> {
        log::trace!("Rasterizier wants {:?}", parsed);
        let lib = ftwrap::Library::new()?;
        let mut face = lib.face_from_locator(&parsed.handle)?;
        let has_color =
            // SAFETY: face_from_locator returned an initialized FT_Face, and
            // `face` owns it for the whole read of its face_flags bitfield.
            unsafe { (((*face.face).face_flags as u32) & ftwrap::FT_FACE_FLAG_COLOR) != 0 };

        if parsed.synthesize_italic {
            face.set_transform(Some(FT_Matrix {
                xx: FT_Fixed::from_num(1),                // scale x
                yy: FT_Fixed::from_num(1),                // scale y
                xy: FT_Fixed::from_num(FAKE_ITALIC_SKEW), // skew x
                yx: FT_Fixed::from_num(0),                // skew y
            }));
        }

        Ok(Self {
            _lib: lib,
            face: RefCell::new(face),
            has_color,
            synthesize_bold: parsed.synthesize_bold,
            freetype_load_flags: parsed.freetype_load_flags,
            freetype_load_target: parsed.freetype_load_target,
            freetype_render_target: parsed.freetype_render_target,
            display_pixel_geometry,
            scale: parsed.scale.unwrap_or(1.),
            hb_raster: HarfbuzzRasterizer::from_locator(parsed)?,
        })
    }

    fn rasterize_outlines(
        &self,
        glyph_pos: u32,
        load_flags: FT_Int32,
    ) -> anyhow::Result<RasterizedGlyph> {
        let mut face = self.face.borrow_mut();
        let paint = face.get_color_glyph_paint(
            glyph_pos,
            FT_Color_Root_Transform::FT_COLOR_INCLUDE_ROOT_TRANSFORM,
        )?;

        // The root transform produces extents that are larger than
        // our nominal pixel size. I'm not sure why that is, but the
        // factor corresponds to the metrics.(x|y)_scale in the root
        // transform.
        // It is desirable to retain the root transform as it includes
        // any skew that may have been applied to the font.
        // So let's extract the offending scaling factors and we'll
        // compensate when we rasterize the paths.
        let (scale_x, scale_y) = {
            // SAFETY: `rasterize_glyph` called `set_font_size` before this fallback,
            // so the borrowed face has a live size slot with initialized scale metrics.
            unsafe {
                let upem = (*face.face).units_per_EM as f64;
                let metrics = (*(*face.face).size).metrics;
                log::trace!("upem={upem}, metrics: {metrics:#?}");

                (1. / metrics.x_scale.to_num::<f64>(), 1. / metrics.y_scale.to_num::<f64>())
            }
        };

        let palette = face.get_palette_data()?;
        log::trace!("Palette: {palette:#?}");
        face.select_palette(0)?;

        let clip_box = face.get_color_glyph_clip_box(glyph_pos)?;
        log::trace!("got clip_box: {clip_box:?}");
        let mut walker = Walker { load_flags, face: &mut face, ops: vec![] };
        walker.walk_paint(paint, 0)?;

        log::trace!("ops: {:#?}", walker.ops);

        rasterize_from_ops(walker.ops, scale_x, -scale_y)
    }
}

#[cfg(test)]
#[path = "freetype_tests.rs"]
mod freetype_tests;

fn rasterize_from_ops(
    ops: Vec<PaintOp>,
    scale_x: f64,
    scale_y: f64,
) -> anyhow::Result<RasterizedGlyph> {
    let (surface, has_color) = record_to_cairo_surface(ops, scale_x, scale_y)?;
    let (left, top, width, height) = surface.ink_extents();
    log::trace!("extents: left={left} top={top} width={width} height={height}");

    if !width.is_finite() || !height.is_finite() || width < 0.0 || height < 0.0 {
        bail!("invalid color glyph extents {width}x{height}");
    }
    let width_px = width as usize;
    let height_px = height as usize;
    if width_px == 0 || height_px == 0 {
        // When: width_px or height_px collapsed to zero, so the paint ops
        // covered no pixels and an empty bitmap stands in.
        return Ok(RasterizedGlyph {
            data: vec![],
            height: 0,
            width: 0,
            bearing_x: PixelLength::new(0.),
            bearing_y: PixelLength::new(0.),
            has_color: false,
            is_scaled: true,
        });
    }
    checked_glyph_rgba_len(width_px, height_px)?;

    let mut bounds_adjust = Matrix::identity();
    bounds_adjust.translate(-left, -top);
    log::trace!("dims: {width}x{height} {bounds_adjust:?}");

    let target = ImageSurface::create(
        Format::ARgb32,
        i32::try_from(width_px).context("color glyph width exceeds i32")?,
        i32::try_from(height_px).context("color glyph height exceeds i32")?,
    )?;
    {
        let context = Context::new(&target)?;
        context.transform(bounds_adjust);
        context.set_antialias(cairo::Antialias::Best);
        context.set_source_surface(surface, 0., 0.)?;
        context.paint()?;
    }

    let mut data = target.take_data()?.to_vec();
    argb_to_rgba(&mut data);

    Ok(RasterizedGlyph {
        data,
        height: height_px,
        width: width_px,
        bearing_x: PixelLength::new(left.min(0.)),
        bearing_y: PixelLength::new(-top),
        has_color,
        is_scaled: true,
    })
}

struct Walker<'a> {
    load_flags: FT_Int32,
    face: &'a mut crate::ftwrap::Face,
    ops: Vec<PaintOp>,
}

impl<'a> Walker<'a> {
    fn walk_paint(&mut self, paint: FT_Opaque_Paint_, level: usize) -> anyhow::Result<()> {
        use FT_PaintFormat_::*;

        let paint =
            // SAFETY: every recursive handle originates from `get_color_glyph_paint`,
            // this face's decoded paint, or this face's layer iterator and is consumed synchronously.
            unsafe { self.face.get_paint(paint) }?;

        // SAFETY: `paint.format` selects each union arm; layer iterators come from this face's
        // selected arm and are consumed by `get_paint_layers` only during this synchronous walk.
        unsafe {
            match paint.format {
                FT_COLR_PAINTFORMAT_COLR_LAYERS => {
                    log::trace!("{level:>3} {:?}", paint.format);
                    let mut iter = paint.u.colr_layers.as_ref().layer_iterator;
                    while let Ok(inner_paint) = self.face.get_paint_layers(&mut iter) {
                        self.walk_paint(inner_paint, level + 1)?;
                    }
                }
                FT_COLR_PAINTFORMAT_SOLID => {
                    let op = PaintOp::PaintSolid(
                        self.decode_color_index(&paint.u.solid.as_ref().color)?,
                    );
                    log::trace!("{level:>3} {:?} {op:x?}", paint.format);
                    self.ops.push(op);
                }
                FT_COLR_PAINTFORMAT_LINEAR_GRADIENT => {
                    let grad = paint.u.linear_gradient.as_ref();
                    log::trace!("{level:>3} {grad:?}");
                    let (x0, y0) = vector_x_y(&grad.p0);
                    let (x1, y1) = vector_x_y(&grad.p1);
                    let (x2, y2) = vector_x_y(&grad.p2);
                    // FIXME: gradient vectors are expressed as font units,
                    // do we need to adjust them here?
                    let paint = PaintOp::PaintLinearGradient {
                        x0,
                        y0,
                        x1,
                        y1,
                        x2,
                        y2,
                        color_line: self.decode_color_line(&grad.colorline)?,
                    };
                    self.ops.push(paint);
                }
                FT_COLR_PAINTFORMAT_RADIAL_GRADIENT => {
                    let grad = paint.u.radial_gradient.as_ref();
                    log::trace!("{level:>3} {grad:?}");
                    let (x0, y0) = vector_x_y(&grad.c0);
                    let (x1, y1) = vector_x_y(&grad.c1);

                    let paint = PaintOp::PaintRadialGradient {
                        x0,
                        y0,
                        x1,
                        y1,
                        r0: grad.r0.font_units() as f32,
                        r1: grad.r1.font_units() as f32,
                        color_line: self.decode_color_line(&grad.colorline)?,
                    };
                    self.ops.push(paint);
                }
                FT_COLR_PAINTFORMAT_SWEEP_GRADIENT => {
                    let grad = paint.u.sweep_gradient.as_ref();
                    log::trace!("{level:>3} {grad:?}");
                    let (x0, y0) = vector_x_y(&grad.center);
                    let start_angle = grad.start_angle.to_num();
                    let end_angle = grad.end_angle.to_num();

                    let paint = PaintOp::PaintSweepGradient {
                        x0,
                        y0,
                        start_angle,
                        end_angle,
                        color_line: self.decode_color_line(&grad.colorline)?,
                    };
                    self.ops.push(paint);
                }
                FT_COLR_PAINTFORMAT_GLYPH => {
                    // FIXME: harfbuzz, in COLR.hh, pushes the inverse of
                    // the root transform before emitting the glyph
                    // DrawOps, then pops it prior to recursing into
                    // the child paint
                    log::trace!("{level:>3} {:?}", paint.u.glyph.as_ref());

                    let glyph_index = paint.u.glyph.as_ref().glyphID;

                    let ops = self.face.load_glyph_outlines(glyph_index, self.load_flags)?;
                    log::trace!("{level:>3} -> {ops:?}");
                    self.ops.push(PaintOp::PushClip(ops));

                    self.walk_paint(paint.u.glyph.as_ref().paint, level + 1)?;

                    self.ops.push(PaintOp::PopClip);
                }
                FT_COLR_PAINTFORMAT_COLR_GLYPH => {
                    let g = paint.u.colr_glyph.as_ref();
                    log::trace!("{level:>3} {g:?}");
                    self.ops.push(PaintOp::PushGroup);
                    let paint = self.face.get_color_glyph_paint(
                        g.glyphID,
                        FT_Color_Root_Transform::FT_COLOR_NO_ROOT_TRANSFORM,
                    )?;

                    self.walk_paint(paint, level + 1)?;
                    self.ops.push(PaintOp::PopGroup(Operator::Over));
                }
                FT_COLR_PAINTFORMAT_TRANSFORM => {
                    let t = paint.u.transform.as_ref();
                    let matrix = affine2x3_to_matrix(t.affine);
                    log::trace!("{level:>3} {t:?} -> {matrix:?}");
                    self.ops.push(PaintOp::PushTransform(matrix));
                    self.walk_paint(t.paint, level + 1)?;
                    self.ops.push(PaintOp::PopTransform);
                }
                FT_COLR_PAINTFORMAT_TRANSLATE => {
                    let t = paint.u.translate.as_ref();
                    log::trace!("{level:>3} {t:?}");

                    let mut matrix = Matrix::identity();
                    matrix.translate(t.dx.to_num(), t.dy.to_num());
                    self.ops.push(PaintOp::PushTransform(matrix));
                    self.walk_paint(t.paint, level + 1)?;
                    self.ops.push(PaintOp::PopTransform);
                }
                FT_COLR_PAINTFORMAT_SCALE => {
                    let scale = paint.u.scale.as_ref();
                    log::trace!("{level:>3} {scale:?}");

                    // Scaling around a center coordinate
                    let center_x = scale.center_x.to_num();
                    let center_y = scale.center_x.to_num();

                    let mut p1 = Matrix::identity();
                    p1.translate(center_x, center_y);

                    let mut p2 = Matrix::identity();
                    p2.scale(scale.scale_x.to_num(), scale.scale_y.to_num());

                    let mut p3 = Matrix::identity();
                    p3.translate(-center_x, -center_y);

                    self.ops.push(PaintOp::PushTransform(p1));
                    self.ops.push(PaintOp::PushTransform(p2));
                    self.ops.push(PaintOp::PushTransform(p3));
                    self.walk_paint(scale.paint, level + 1)?;
                    self.ops.push(PaintOp::PopTransform);
                    self.ops.push(PaintOp::PopTransform);
                    self.ops.push(PaintOp::PopTransform);
                }
                FT_COLR_PAINTFORMAT_ROTATE => {
                    let rot = paint.u.rotate.as_ref();
                    log::trace!("{level:>3} {rot:?}");

                    // Rotating around a center coordinate
                    let center_x = rot.center_x.to_num();
                    let center_y = rot.center_x.to_num();

                    let mut p1 = Matrix::identity();
                    p1.translate(center_x, center_y);

                    let mut p2 = Matrix::identity();
                    p2.rotate(PI * rot.angle.to_num::<f64>());

                    let mut p3 = Matrix::identity();
                    p3.translate(-center_x, -center_y);

                    self.ops.push(PaintOp::PushTransform(p1));
                    self.ops.push(PaintOp::PushTransform(p2));
                    self.ops.push(PaintOp::PushTransform(p3));
                    self.walk_paint(rot.paint, level + 1)?;
                    self.ops.push(PaintOp::PopTransform);
                    self.ops.push(PaintOp::PopTransform);
                    self.ops.push(PaintOp::PopTransform);
                }
                FT_COLR_PAINTFORMAT_SKEW => {
                    let skew = paint.u.skew.as_ref();
                    log::trace!("{level:>3} {skew:?}");

                    // Skewing around a center coordinate
                    let center_x = skew.center_x.to_num();
                    let center_y = skew.center_x.to_num();

                    let mut p1 = Matrix::identity();
                    p1.translate(center_x, center_y);

                    let x_skew_angle: f64 = skew.x_skew_angle.to_num();
                    let y_skew_angle: f64 = skew.y_skew_angle.to_num();
                    let x = (PI * -x_skew_angle).tan();
                    let y = (PI * y_skew_angle).tan();

                    let p2 = Matrix::new(1., y, x, 1., 0., 0.);

                    let mut p3 = Matrix::identity();
                    p3.translate(-center_x, -center_y);

                    self.ops.push(PaintOp::PushTransform(p1));
                    self.ops.push(PaintOp::PushTransform(p2));
                    self.ops.push(PaintOp::PushTransform(p3));
                    self.walk_paint(skew.paint, level + 1)?;
                    self.ops.push(PaintOp::PopTransform);
                    self.ops.push(PaintOp::PopTransform);
                    self.ops.push(PaintOp::PopTransform);
                }
                FT_COLR_PAINTFORMAT_COMPOSITE => {
                    let comp = paint.u.composite.as_ref();
                    log::trace!("{level:>3} {comp:?}");

                    self.walk_paint(comp.backdrop_paint, level + 1)?;
                    self.ops.push(PaintOp::PushGroup);
                    self.walk_paint(comp.source_paint, level + 1)?;
                    self.ops
                        .push(PaintOp::PopGroup(composite_mode_to_operator(comp.composite_mode)));
                }
                wat => {
                    anyhow::bail!("unknown/unhandled FT_PaintFormat_ value {wat:?}");
                }
            }
        }

        Ok(())
    }

    fn decode_color_index(&mut self, c: &FT_ColorIndex) -> anyhow::Result<SrgbaPixel> {
        let alpha: f64 = c.alpha.to_num();
        let (r, g, b, a) = if c.palette_index == 0xffff {
            // Foreground color.
            // We use white here because the rendering stage will
            // tint this with the actual color in the correct context
            (0xff, 0xff, 0xff, 1.0)
        } else {
            // When: palette_index names a real entry rather than the 0xffff
            // foreground sentinel, so the palette supplies the colour.
            let color = self.face.get_palette_entry(c.palette_index as _)?;
            (color.red, color.green, color.blue, color.alpha as f64 / 255.)
        };

        let alpha = (a * alpha * 255.) as u8;
        Ok(SrgbaPixel::rgba(r, g, b, alpha))
    }

    fn decode_color_line(&mut self, line: &FT_ColorLine) -> anyhow::Result<ColorLine> {
        let mut iter = line.color_stop_iterator;
        let mut color_stops = vec![];
        loop {
            let mut stop = MaybeUninit::<FT_ColorStop>::zeroed();

            let got_stop = {
                // SAFETY: the borrowed face keeps this FT_Face alive, and
                // FreeType writes through the out-pointer into the zeroed slot.
                unsafe { FT_Get_Colorline_Stops(self.face.face, stop.as_mut_ptr(), &mut iter) }
            };
            if got_stop == 0 {
                // When: got_stop is zero, the iterator is exhausted, so the
                // colour line has no further stops to decode.
                break;
            }

            let stop = {
                // SAFETY: got_stop was non-zero, so FreeType initialized the
                // stop slot this reads.
                unsafe { stop.assume_init() }
            };

            color_stops.push(ColorStop {
                offset: stop.stop_offset.to_num(),
                color: self.decode_color_index(&stop.color)?,
            });
        }

        Ok(ColorLine {
            extend: match line.extend {
                FT_PaintExtend::FT_COLR_PAINT_EXTEND_PAD => Extend::Pad,
                FT_PaintExtend::FT_COLR_PAINT_EXTEND_REPEAT => Extend::Repeat,
                FT_PaintExtend::FT_COLR_PAINT_EXTEND_REFLECT => Extend::Reflect,
            },
            color_stops,
        })
    }
}

fn affine2x3_to_matrix(t: FT_Affine23) -> Matrix {
    Matrix::new(
        t.xx.to_num(),
        t.yx.to_num(),
        t.xy.to_num(),
        t.yy.to_num(),
        t.dy.to_num(),
        t.dx.to_num(),
    )
}

fn record_to_cairo_surface(
    paint_ops: Vec<PaintOp>,
    scale_x: f64,
    scale_y: f64,
) -> anyhow::Result<(RecordingSurface, bool)> {
    let mut has_color = false;
    let surface = RecordingSurface::create(Content::ColorAlpha, None)?;
    let context = Context::new(&surface)?;
    context.scale(scale_x, scale_y);
    context.set_antialias(cairo::Antialias::Best);

    for pop in paint_ops {
        match pop {
            PaintOp::PushTransform(matrix) => {
                context.save()?;
                context.transform(matrix);
            }
            PaintOp::PopTransform => {
                context.restore()?;
            }
            PaintOp::PushClip(draw) => {
                context.save()?;
                apply_draw_ops_to_context(&draw, &context)?;
                context.clip();
            }
            PaintOp::PopClip => {
                context.restore()?;
            }
            PaintOp::PushGroup => {
                context.save()?;
                context.push_group();
            }
            PaintOp::PopGroup(operator) => {
                context.pop_group_to_source()?;
                context.set_operator(operator);
                context.paint()?;
                context.restore()?;
            }
            PaintOp::PaintSolid(color) => {
                if color.as_srgba32() != 0xffffffff {
                    has_color = true;
                }
                let (r, g, b, a) = color.as_srgba_tuple();
                context.set_source_rgba(r.into(), g.into(), b.into(), a.into());
                context.paint()?;
            }
            PaintOp::PaintLinearGradient { x0, y0, x1, y1, x2, y2, color_line } => {
                has_color = true;
                paint_linear_gradient(
                    &context,
                    x0.into(),
                    y0.into(),
                    x1.into(),
                    y1.into(),
                    x2.into(),
                    y2.into(),
                    color_line,
                )?;
            }
            PaintOp::PaintRadialGradient { x0, y0, r0, x1, y1, r1, color_line } => {
                has_color = true;
                paint_radial_gradient(
                    &context,
                    x0.into(),
                    y0.into(),
                    r0.into(),
                    x1.into(),
                    y1.into(),
                    r1.into(),
                    color_line,
                )?;
            }
            PaintOp::PaintSweepGradient { x0, y0, start_angle, end_angle, color_line } => {
                has_color = true;
                paint_sweep_gradient(
                    &context,
                    x0.into(),
                    y0.into(),
                    start_angle.into(),
                    end_angle.into(),
                    color_line,
                )?;
            }
        }
    }

    Ok((surface, has_color))
}
