#![cfg(target_os = "windows")]

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_text::{glyph_atlas::GlyphAtlas, GlyphInstance};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{
        GetDC, ReleaseDC, SetDIBitsToDevice, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        HDC, RGBQUAD,
    },
};
use winit::window::Window;

use crate::{
    color::{blend_premul_linear_over_srgb_bgra, grayscale_coverage},
    core::{validated_surface_size, MAX_SURFACE_DIMENSION},
    quad::QuadInstance,
    wezterm_pipeline::ndc_rect_to_pixels,
};

pub(crate) struct WindowsSoftwareFrame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl WindowsSoftwareFrame {
    pub(crate) fn new(width: u32, height: u32, clear: [f32; 4]) -> anyhow::Result<Self> {
        let size = validated_surface_size(width, height, MAX_SURFACE_DIMENSION)
            .ok_or_else(|| anyhow::anyhow!("unsafe software frame size {width}x{height}"))?;
        let mut frame =
            Self { width: size.width, height: size.height, pixels: vec![0; size.bytes] };
        frame.clear(clear);
        Ok(frame)
    }

    pub(crate) fn prepare(
        &mut self,
        width: u32,
        height: u32,
        clear: [f32; 4],
    ) -> anyhow::Result<()> {
        let size = validated_surface_size(width, height, MAX_SURFACE_DIMENSION)
            .ok_or_else(|| anyhow::anyhow!("unsafe software frame size {width}x{height}"))?;
        if self.width != size.width || self.height != size.height {
            let old_capacity = self.pixels.capacity();
            self.width = size.width;
            self.height = size.height;
            if size.bytes > self.pixels.capacity() || size.bytes < self.pixels.capacity() / 2 {
                self.pixels = vec![0; size.bytes];
            } else {
                // When: the request still fits capacity and is at least half of it, resize
                // reuses the allocation so a window drag does not reallocate every frame.
                self.pixels.resize(size.bytes, 0);
            }
            tracing::debug!(
                target: "memory",
                width = size.width,
                height = size.height,
                bytes = size.bytes,
                old_capacity,
                new_capacity = self.pixels.capacity(),
                "Windows software frame resized"
            );
        }
        self.clear(clear);
        Ok(())
    }

    /// Bytes this frame is holding, counting reserved capacity.
    ///
    /// Capacity rather than length: `prepare` keeps the allocation across
    /// resizes that shrink by less than half, so a frame that was once large
    /// still owns the larger buffer.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.pixels.capacity()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_layers(
        &mut self,
        glyph_atlas: &GlyphAtlas,
        image_atlas: &GlyphAtlas,
        quads: &[QuadInstance],
        images: &[GlyphInstance],
        glyphs: &[GlyphInstance],
        overlay_quads: &[QuadInstance],
        overlay_glyphs: &[GlyphInstance],
    ) {
        self.draw_quads(quads);
        self.draw_glyphs(image_atlas, images);
        self.draw_glyphs(glyph_atlas, glyphs);
        self.draw_quads(overlay_quads);
        self.draw_glyphs(glyph_atlas, overlay_glyphs);
    }

    pub(crate) fn present(&self, window: &Window) -> anyhow::Result<()> {
        let hwnd = hwnd_for_window(window)?;
        // SAFETY: hwnd is the live Win32 handle of the window being presented, and
        // pixels holds width * height * 4 bytes, the exact extent the blit describes.
        unsafe { blit_bgra_to_hwnd(hwnd, self.width, self.height, &self.pixels) }
    }

    pub(crate) fn pixel_bgra_at(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            // When: x or y exceeds width or height the flat offset would silently land on
            // the next row's pixel rather than fail, so the probe reports no pixel.
            return None;
        }
        let off = ((y * self.width + x) * 4) as usize;
        Some([self.pixels[off], self.pixels[off + 1], self.pixels[off + 2], self.pixels[off + 3]])
    }

    #[cfg(test)]
    fn pixel_bgra(&self, x: u32, y: u32) -> [u8; 4] {
        self.pixel_bgra_at(x, y).expect("test pixel in bounds")
    }

    fn clear(&mut self, color: [f32; 4]) {
        let px = linear_rgba_to_bgra(color);
        for p in self.pixels.as_chunks_mut::<4>().0 {
            p.copy_from_slice(&px);
        }
    }

    fn draw_quads(&mut self, quads: &[QuadInstance]) {
        let sw = self.width as f32;
        let sh = self.height as f32;
        for q in quads {
            let Some((x, y, w, h)) = ndc_rect_to_pixels(q.rect, sw, sh) else {
                // When: ndc_rect_to_pixels returns None the frame has zero extent, so this
                // quad is dropped rather than scaled against a zero-sized surface.
                continue;
            };
            if q.line_thickness_px > 0.0 {
                self.draw_line_quad(q, x, y, w, h);
            } else if q.radius_px > 0.0 {
                // When: radius_px is positive the fill derives per-pixel coverage from a
                // corner distance field, which a rectangular span cannot express.
                self.fill_rounded_rect(x, y, w, h, q.color, q.radius_px);
            } else {
                // When: neither line_thickness_px nor radius_px is set the quad is a plain
                // rectangle, so the span is written directly with no coverage term.
                self.fill_rect(x, y, w, h, q.color);
            }
        }
    }

    fn draw_glyphs(&mut self, atlas: &GlyphAtlas, glyphs: &[GlyphInstance]) {
        let sw = self.width as f32;
        let sh = self.height as f32;
        for g in glyphs {
            let Some((x, y, w, h)) = ndc_rect_to_pixels(g.rect, sw, sh) else {
                // When: ndc_rect_to_pixels returns None the frame has zero extent, so this
                // glyph is dropped rather than scaled against a zero-sized surface.
                continue;
            };
            if w <= 0.0 || h <= 0.0 {
                // When: w or h is not positive the glyph covers no pixels, and blit_glyph
                // divides by both to build its atlas sample coordinates.
                continue;
            }
            self.blit_glyph(atlas, g, x, y, w, h);
        }
    }

    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        let x0 = (x - 0.5).ceil().max(0.0) as i32;
        let y0 = (y - 0.5).ceil().max(0.0) as i32;
        let x1 = (x + w - 0.5).ceil().min(self.width as f32) as i32;
        let y1 = (y + h - 0.5).ceil().min(self.height as f32) as i32;
        if x1 <= x0 || y1 <= y0 {
            // When: x1 or y1 collapsed past its origin the clamp to frame bounds left an
            // empty span, so an off-surface quad is a no-op rather than a stray write.
            return;
        }
        let src = premul_linear_rgba_to_premul_bgra_f32(color);
        for yy in y0..y1 {
            let row = yy as usize * self.width as usize * 4;
            for xx in x0..x1 {
                let off = row + xx as usize * 4;
                blend_premul_bgra(&mut self.pixels[off..off + 4], src);
            }
        }
    }

    fn fill_rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4], radius: f32) {
        let x0 = x.floor().max(0.0) as i32;
        let y0 = y.floor().max(0.0) as i32;
        let x1 = (x + w).ceil().min(self.width as f32) as i32;
        let y1 = (y + h).ceil().min(self.height as f32) as i32;
        if x1 <= x0 || y1 <= y0 {
            // When: x1 or y1 collapsed past its origin the rounded rect is entirely
            // off-surface, so no pixel can receive a corner coverage term.
            return;
        }
        let src = premul_linear_rgba_to_premul_bgra_f32(color);
        let half_w = w * 0.5;
        let half_h = h * 0.5;
        let r = radius.min(half_w).min(half_h).max(0.0);
        for yy in y0..y1 {
            let row = yy as usize * self.width as usize * 4;
            for xx in x0..x1 {
                let local_x = (xx as f32 + 0.5) - (x + half_w);
                let local_y = (yy as f32 + 0.5) - (y + half_h);
                let qx = local_x.abs() - (half_w - r);
                let qy = local_y.abs() - (half_h - r);
                let outside_x = qx.max(0.0);
                let outside_y = qy.max(0.0);
                let outside = (outside_x * outside_x + outside_y * outside_y).sqrt();
                let inside = qx.max(qy).min(0.0);
                let d = outside + inside - r;
                let coverage = (0.5 - d).clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    // When: coverage is zero the pixel lies outside the corner radius, so
                    // blending would square off the corner this path exists to round.
                    continue;
                }
                let mut c = src;
                c[0] *= coverage;
                c[1] *= coverage;
                c[2] *= coverage;
                c[3] *= coverage;
                let off = row + xx as usize * 4;
                blend_premul_bgra(&mut self.pixels[off..off + 4], c);
            }
        }
    }

    fn draw_line_quad(&mut self, q: &QuadInstance, x: f32, y: f32, w: f32, h: f32) {
        let size = if q.size_px[0] > 0.0 && q.size_px[1] > 0.0 {
            q.size_px
        } else {
            // When: size_px is unset the pixel extent w and h stand in, keeping the segment
            // distance in the same units the endpoints were offset in.
            [w, h]
        };
        let center_x = x + w * 0.5;
        let center_y = y + h * 0.5;
        let ax = center_x + q.line_a[0];
        let ay = center_y + q.line_a[1];
        let bx = center_x + q.line_b[0];
        let by = center_y + q.line_b[1];
        let x0 = x.floor().max(0.0) as i32;
        let y0 = y.floor().max(0.0) as i32;
        let x1 = (x + size[0].max(w)).ceil().min(self.width as f32) as i32;
        let y1 = (y + size[1].max(h)).ceil().min(self.height as f32) as i32;
        if x1 <= x0 || y1 <= y0 {
            // When: x1 or y1 collapsed past its origin the line's padded bounds fell
            // outside the frame, so no pixel is near enough the segment to shade.
            return;
        }
        let src = premul_linear_rgba_to_premul_bgra_f32(q.color);
        let half = (q.line_thickness_px * 0.5).max(0.5);
        for yy in y0..y1 {
            let row = yy as usize * self.width as usize * 4;
            for xx in x0..x1 {
                let px = xx as f32 + 0.5;
                let py = yy as f32 + 0.5;
                let d = distance_to_segment(px, py, ax, ay, bx, by) - half;
                let coverage = (1.0 - d.clamp(0.0, 1.0)).clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    // When: coverage is zero the pixel sits farther than half the stroke
                    // from the segment, so blending would widen the drawn line.
                    continue;
                }
                let mut c = src;
                c[0] *= coverage;
                c[1] *= coverage;
                c[2] *= coverage;
                c[3] *= coverage;
                let off = row + xx as usize * 4;
                blend_premul_bgra(&mut self.pixels[off..off + 4], c);
            }
        }
    }

    fn blit_glyph(
        &mut self,
        atlas: &GlyphAtlas,
        glyph: &GlyphInstance,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) {
        let [u0, v0, u1, v1] = glyph.uv;
        if u1 <= u0 || v1 <= v0 {
            // When: u1 or v1 does not exceed its origin the glyph has no atlas rectangle
            // to sample, so drawing it would read a zero-width region and blend nothing.
            return;
        }
        let atlas_w = atlas.width().max(1);
        let atlas_h = atlas.height().max(1);
        let ax0 = (u0 * atlas_w as f32).round().clamp(0.0, atlas_w as f32) as u32;
        let ay0 = (v0 * atlas_h as f32).round().clamp(0.0, atlas_h as f32) as u32;
        let ax1 = (u1 * atlas_w as f32).round().clamp(0.0, atlas_w as f32) as u32;
        let ay1 = (v1 * atlas_h as f32).round().clamp(0.0, atlas_h as f32) as u32;
        if ax1 <= ax0 || ay1 <= ay0 {
            // When: ax1 or ay1 rounds onto its origin the tile spans no atlas texel, and
            // the later (ax1 - 1) clamp would underflow on unsigned atlas coordinates.
            return;
        }
        let src_w_u = (ax1 - ax0).max(1);
        let src_h_u = (ay1 - ay0).max(1);
        let src_w = src_w_u as f32;
        let src_h = src_h_u as f32;
        let one_to_one_x = (w - src_w).abs() < 0.01;
        let one_to_one_y = (h - src_h).abs() < 0.01;
        let one_to_one = one_to_one_x && one_to_one_y;
        // Images retain fractional placement only on axes that are resampled. Every text axis uses
        // one stabilized destination origin regardless of the source tile dimensions.
        let image = glyph.flags[2] >= 0.5;
        let draw_x = if image && !one_to_one_x {
            x
        } else {
            // When: `image && !one_to_one_x` is false, X uses pixel-aligned nearest sampling.
            stabilize_half_pixel_origin(x).round()
        };
        let draw_y = if image && !one_to_one_y {
            y
        } else {
            // When: `image && !one_to_one_y` is false, Y uses pixel-aligned nearest sampling.
            stabilize_half_pixel_origin(y).round()
        };
        // Keep the unclipped bounds: native sampling must advance past source texels hidden above
        // or left of the frame rather than restarting from the tile's first row or column.
        let unclipped_x0 = (draw_x - 0.5).ceil() as i32;
        let unclipped_y0 = (draw_y - 0.5).ceil() as i32;
        let x0 = unclipped_x0.max(0);
        let y0 = unclipped_y0.max(0);
        let x1 = (draw_x + w - 0.5).ceil().min(self.width as f32) as i32;
        let y1 = (draw_y + h - 0.5).ceil().min(self.height as f32) as i32;
        if x1 <= x0 || y1 <= y0 {
            // When: x1 or y1 collapsed past its origin the glyph's destination span is
            // empty, so the atlas is never sampled for a row that cannot appear.
            return;
        }
        let atlas_pixels = atlas.pixels_bgra();
        let fg_srgb = premul_linear_rgba_to_straight_srgb(glyph.color);
        let fg_alpha = glyph.color[3].clamp(0.0, 1.0);
        let color_glyph = glyph.flags[0] >= 0.5;
        let subpixel_glyph = glyph.flags[1] >= 0.5;
        // Inline images set flags[2]; glyphs leave it clear. Only images want
        // bilinear scaling — a glyph sampled bilinearly reads its atlas
        // neighbours and blends them into its own edges.
        for yy in y0..y1 {
            let ty = ((yy as f32 + 0.5 - draw_y) / h).clamp(0.0, 0.999_999);
            let sy = ay0 as f32 + src_h * ty;
            let row = yy as usize * self.width as usize * 4;
            for xx in x0..x1 {
                let tx = ((xx as f32 + 0.5 - draw_x) / w).clamp(0.0, 0.999_999);
                let sx = ax0 as f32 + src_w * tx;
                // Match the GPU pipeline: glyphs use nearest sampling while
                // inline images retain intentional bilinear scaling.
                let sample = if one_to_one {
                    let sx = ax0 + (xx - unclipped_x0) as u32;
                    let sy = ay0 + (yy - unclipped_y0) as u32;
                    bgra_pixel_at(atlas_pixels, atlas_w, sx.min(atlas_w - 1), sy.min(atlas_h - 1))
                } else if image {
                    // When: image is set the source is inline media being scaled, where
                    // bilinear taps smooth the result instead of blocking it up.
                    sample_atlas_bilinear_in_rect(
                        atlas_pixels,
                        atlas_w,
                        atlas_h,
                        sx,
                        sy,
                        (ax0, ay0, ax1, ay1),
                    )
                } else {
                    // When: neither one_to_one nor image holds the glyph is scaled text,
                    // which takes nearest sampling so no neighbour bleeds into its edges.

                    // Nearest, clamped to this glyph's own tile. The clamp is
                    // what makes neighbour bleed impossible rather than
                    // merely unlikely.
                    let sx = sx.floor().clamp(ax0 as f32, (ax1 - 1) as f32) as u32;
                    let sy = sy.floor().clamp(ay0 as f32, (ay1 - 1) as f32) as u32;
                    bgra_pixel_at(atlas_pixels, atlas_w, sx, sy)
                };
                let dst_off = row + xx as usize * 4;
                if color_glyph {
                    // When: color_glyph is set the atlas already holds premultiplied
                    // colour, so the sample is blended as-is and glyph.color cannot tint.
                    if sample[3] <= 0.0 {
                        // When: sample alpha is zero the texel is fully transparent, so
                        // blending would cost work and leave the destination unchanged.
                        continue;
                    }
                    blend_premul_bgra(&mut self.pixels[dst_off..dst_off + 4], sample);
                } else if subpixel_glyph {
                    // When: subpixel_glyph is set the sample carries per-channel coverage
                    // rather than one alpha, so R, G and B are weighted separately.
                    if sample[3] <= 0.0 {
                        // When: sample alpha is zero no channel of this texel is covered,
                        // so every subpixel weight would be zero.
                        continue;
                    }
                    blend_subpixel_bgra(
                        &mut self.pixels[dst_off..dst_off + 4],
                        sample,
                        fg_srgb,
                        fg_alpha,
                    );
                } else {
                    // When: neither color_glyph nor subpixel_glyph is set the atlas holds
                    // grayscale coverage, so glyph.color is scaled by it and blended.
                    let cov = grayscale_coverage(sample);
                    if cov <= 0.0 {
                        // When: cov is zero the texel contributes nothing, so the
                        // premultiplied source would be fully transparent.
                        continue;
                    }
                    let src = [
                        glyph.color[0] * cov,
                        glyph.color[1] * cov,
                        glyph.color[2] * cov,
                        glyph.color[3] * cov,
                    ];
                    blend_premul_linear_over_srgb_bgra(&mut self.pixels[dst_off..dst_off + 4], src);
                }
            }
        }
    }
}

fn hwnd_for_window(window: &Window) -> anyhow::Result<HWND> {
    let handle = window.window_handle()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut _)),
        _ => anyhow::bail!("window is not a Win32 HWND"),
    }
}

// SAFETY: callers pass a live HWND for the window being presented and a pixels slice
// holding width * height * 4 bytes, the exact extent the blit below describes to GDI.
unsafe fn blit_bgra_to_hwnd(
    hwnd: HWND,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> anyhow::Result<()> {
    let hdc =
        // SAFETY: GetDC only reads hwnd, and reports failure as a null HDC rather than
        // by trapping, which the null check below rejects before any drawing use.
        unsafe { GetDC(Some(hwnd)) };
    if hdc.0.is_null() {
        anyhow::bail!("GetDC failed");
    }
    let result =
        // SAFETY: hdc is non-null on this path, and width, height and pixels carry this
        // function's own precondition forward unchanged.
        unsafe { blit_bgra_to_hdc(hdc, width, height, pixels) };
    let _ =
        // SAFETY: releases exactly the hdc GetDC returned, paired with the same hwnd, on
        // every path that reached the acquisition.
        unsafe { ReleaseDC(Some(hwnd), hdc) };
    result
}

// SAFETY: callers pass a valid HDC and a pixels slice holding width * height * 4 bytes
// in BGRA order, since GDI reads that extent directly from the pointer below.
unsafe fn blit_bgra_to_hdc(hdc: HDC, width: u32, height: u32, pixels: &[u8]) -> anyhow::Result<()> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        bmiColors: [RGBQUAD::default(); 1],
    };
    let rows =
        // SAFETY: bmi describes height rows of width BGRA texels, the same extent the
        // caller guaranteed in pixels; biHeight is negated to read the slice top-down.
        unsafe {
        SetDIBitsToDevice(
            hdc,
            0,
            0,
            width,
            height,
            0,
            0,
            0,
            height,
            pixels.as_ptr() as *const std::ffi::c_void,
            &bmi,
            DIB_RGB_COLORS,
        )
    };
    if rows == 0 {
        anyhow::bail!("SetDIBitsToDevice failed");
    }
    Ok(())
}

fn linear_rgba_to_bgra(color: [f32; 4]) -> [u8; 4] {
    let a = color[3].clamp(0.0, 1.0);
    [
        to_u8(linear_to_srgb(color[2].clamp(0.0, 1.0))),
        to_u8(linear_to_srgb(color[1].clamp(0.0, 1.0))),
        to_u8(linear_to_srgb(color[0].clamp(0.0, 1.0))),
        to_u8(a),
    ]
}

fn premul_linear_rgba_to_premul_bgra_f32(color: [f32; 4]) -> [f32; 4] {
    let a = color[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        // When: a is zero the premultiplied colour carries no recoverable hue, and the
        // unpremultiply divides by it, so a transparent texel is returned instead.
        return [0.0; 4];
    }
    let r = (color[0] / a).clamp(0.0, 1.0);
    let g = (color[1] / a).clamp(0.0, 1.0);
    let b = (color[2] / a).clamp(0.0, 1.0);
    [linear_to_srgb(b) * a, linear_to_srgb(g) * a, linear_to_srgb(r) * a, a]
}

fn premul_linear_rgba_to_straight_srgb(color: [f32; 4]) -> [f32; 3] {
    let a = color[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        // When: a is zero there is no straight colour to recover, since every channel
        // below is divided by it to undo premultiplication.
        return [0.0; 3];
    }
    [
        linear_to_srgb((color[0] / a).clamp(0.0, 1.0)),
        linear_to_srgb((color[1] / a).clamp(0.0, 1.0)),
        linear_to_srgb((color[2] / a).clamp(0.0, 1.0)),
    ]
}

fn bgra8_to_premul_f32(px: &[u8]) -> [f32; 4] {
    [px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0, px[3] as f32 / 255.0]
}

fn stabilize_half_pixel_origin(value: f32) -> f32 {
    // NDC reconstruction can place one shared half-pixel edge on opposite sides
    // of `round` for different glyph heights; recover only that numerical noise.
    let nearest_half = (value * 2.0).round() * 0.5;
    if (value - nearest_half).abs() <= 0.001 {
        nearest_half
    } else {
        // When: value differs materially from nearest_half, preserve the intended fraction.
        value
    }
}

#[cfg(test)]
fn sample_atlas_bilinear(pixels: &[u8], width: u32, height: u32, x: f32, y: f32) -> [f32; 4] {
    sample_atlas_bilinear_in_rect(pixels, width, height, x, y, (0, 0, width, height))
}

fn sample_atlas_bilinear_in_rect(
    pixels: &[u8],
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    rect: (u32, u32, u32, u32),
) -> [f32; 4] {
    let (min_x, min_y, max_x, max_y) = rect;
    let max_x = max_x.min(width);
    let max_y = max_y.min(height);
    if width == 0 || height == 0 || pixels.is_empty() || min_x >= max_x || min_y >= max_y {
        // When: the rect collapsed or pixels is empty there is no texel inside this
        // glyph's own tile, and the (max_x - 1) clamp below would underflow.
        return [0.0; 4];
    }
    // Atlas tiles are packed without padding, so both bilinear taps must stay
    // inside this glyph's rectangle instead of borrowing a neighboring tile.
    let x = (x - 0.5).clamp(min_x as f32, (max_x - 1) as f32);
    let y = (y - 0.5).clamp(min_y as f32, (max_y - 1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(max_x - 1);
    let y1 = (y0 + 1).min(max_y - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let c00 = bgra_pixel_at(pixels, width, x0, y0);
    let c10 = bgra_pixel_at(pixels, width, x1, y0);
    let c01 = bgra_pixel_at(pixels, width, x0, y1);
    let c11 = bgra_pixel_at(pixels, width, x1, y1);
    let mut out = [0.0; 4];
    for i in 0..4 {
        let top = c00[i] * (1.0 - fx) + c10[i] * fx;
        let bottom = c01[i] * (1.0 - fx) + c11[i] * fx;
        out[i] = top * (1.0 - fy) + bottom * fy;
    }
    out
}

fn bgra_pixel_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [f32; 4] {
    let off = ((y * width + x) * 4) as usize;
    if off + 4 > pixels.len() {
        // When: off runs past pixels the coordinate lies outside the atlas upload, so a
        // transparent texel is returned rather than panicking on the slice.
        return [0.0; 4];
    }
    bgra8_to_premul_f32(&pixels[off..off + 4])
}

fn blend_premul_bgra(dst: &mut [u8], src: [f32; 4]) {
    let da = dst[3] as f32 / 255.0;
    let db = dst[0] as f32 / 255.0;
    let dg = dst[1] as f32 / 255.0;
    let dr = dst[2] as f32 / 255.0;
    let inv = 1.0 - src[3].clamp(0.0, 1.0);
    let b = src[0] + db * inv;
    let g = src[1] + dg * inv;
    let r = src[2] + dr * inv;
    let a = src[3] + da * inv;
    dst[0] = to_u8(b);
    dst[1] = to_u8(g);
    dst[2] = to_u8(r);
    dst[3] = to_u8(a);
}

fn blend_subpixel_bgra(dst: &mut [u8], coverage_bgra: [f32; 4], fg_srgb: [f32; 3], fg_alpha: f32) {
    let fg_alpha = fg_alpha.clamp(0.0, 1.0);
    let db = dst[0] as f32 / 255.0;
    let dg = dst[1] as f32 / 255.0;
    let dr = dst[2] as f32 / 255.0;
    let wb = (coverage_bgra[0] * fg_alpha).clamp(0.0, 1.0);
    let wg = (coverage_bgra[1] * fg_alpha).clamp(0.0, 1.0);
    let wr = (coverage_bgra[2] * fg_alpha).clamp(0.0, 1.0);
    dst[0] = to_u8(db + (fg_srgb[2] - db) * wb);
    dst[1] = to_u8(dg + (fg_srgb[1] - dg) * wg);
    dst[2] = to_u8(dr + (fg_srgb[0] - dr) * wr);
    dst[3] = 255;
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        // When: v is above the linear toe the sRGB transfer curve switches to its gamma
        // segment, which the two pieces are chosen to meet continuously at that value.
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn distance_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let vx = bx - ax;
    let vy = by - ay;
    let wx = px - ax;
    let wy = py - ay;
    let denom = vx * vx + vy * vy;
    let t = if denom <= f32::EPSILON {
        0.0
    } else {
        // When: denom exceeds EPSILON the segment has length, so the projection is
        // clamped to it and the distance measures the segment, not its infinite line.
        ((wx * vx + wy * vy) / denom).clamp(0.0, 1.0)
    };
    let cx = ax + t * vx;
    let cy = ay + t * vy;
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

#[cfg(test)]
#[path = "software_windows_tests.rs"]
mod software_windows_tests;
