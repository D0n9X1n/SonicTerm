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

use crate::{quad::QuadInstance, wezterm_pipeline::ndc_rect_to_pixels};

pub(crate) struct WindowsSoftwareFrame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl WindowsSoftwareFrame {
    pub(crate) fn new(width: u32, height: u32, clear: [f32; 4]) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let mut frame =
            Self { width, height, pixels: vec![0; width as usize * height as usize * 4] };
        frame.clear(clear);
        frame
    }

    pub(crate) fn reset(&mut self, width: u32, height: u32, clear: [f32; 4]) {
        let width = width.max(1);
        let height = height.max(1);
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            self.pixels.resize(width as usize * height as usize * 4, 0);
        }
        self.clear(clear);
    }

    pub(crate) fn draw_layers(
        &mut self,
        atlas: &GlyphAtlas,
        quads: &[QuadInstance],
        glyphs: &[GlyphInstance],
        overlay_quads: &[QuadInstance],
        overlay_glyphs: &[GlyphInstance],
    ) {
        self.draw_quads(quads);
        self.draw_glyphs(atlas, glyphs);
        self.draw_quads(overlay_quads);
        self.draw_glyphs(atlas, overlay_glyphs);
    }

    pub(crate) fn present(&self, window: &Window) -> anyhow::Result<()> {
        let hwnd = hwnd_for_window(window)?;
        unsafe { blit_bgra_to_hwnd(hwnd, self.width, self.height, &self.pixels) }
    }

    fn clear(&mut self, color: [f32; 4]) {
        let px = linear_rgba_to_bgra(color);
        for p in self.pixels.chunks_exact_mut(4) {
            p.copy_from_slice(&px);
        }
    }

    fn draw_quads(&mut self, quads: &[QuadInstance]) {
        let sw = self.width as f32;
        let sh = self.height as f32;
        for q in quads {
            let Some((x, y, w, h)) = ndc_rect_to_pixels(q.rect, sw, sh) else { continue };
            if q.line_thickness_px > 0.0 {
                self.draw_line_quad(q, x, y, w, h);
            } else if q.radius_px > 0.0 {
                self.fill_rounded_rect(x, y, w, h, q.color, q.radius_px);
            } else {
                self.fill_rect(x, y, w, h, q.color);
            }
        }
    }

    fn draw_glyphs(&mut self, atlas: &GlyphAtlas, glyphs: &[GlyphInstance]) {
        let sw = self.width as f32;
        let sh = self.height as f32;
        for g in glyphs {
            let Some((x, y, w, h)) = ndc_rect_to_pixels(g.rect, sw, sh) else { continue };
            if w <= 0.0 || h <= 0.0 {
                continue;
            }
            self.blit_glyph(atlas, g, x, y, w, h);
        }
    }

    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        let x0 = x.floor().max(0.0) as i32;
        let y0 = y.floor().max(0.0) as i32;
        let x1 = (x + w).ceil().min(self.width as f32) as i32;
        let y1 = (y + h).ceil().min(self.height as f32) as i32;
        if x1 <= x0 || y1 <= y0 {
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
        let size = if q.size_px[0] > 0.0 && q.size_px[1] > 0.0 { q.size_px } else { [w, h] };
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
            return;
        }
        let atlas_w = atlas.width().max(1);
        let atlas_h = atlas.height().max(1);
        let ax0 = (u0 * atlas_w as f32).round().clamp(0.0, atlas_w as f32) as u32;
        let ay0 = (v0 * atlas_h as f32).round().clamp(0.0, atlas_h as f32) as u32;
        let ax1 = (u1 * atlas_w as f32).round().clamp(0.0, atlas_w as f32) as u32;
        let ay1 = (v1 * atlas_h as f32).round().clamp(0.0, atlas_h as f32) as u32;
        if ax1 <= ax0 || ay1 <= ay0 {
            return;
        }
        let x0 = x.floor().max(0.0) as i32;
        let y0 = y.floor().max(0.0) as i32;
        let x1 = (x + w).ceil().min(self.width as f32) as i32;
        let y1 = (y + h).ceil().min(self.height as f32) as i32;
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let atlas_pixels = atlas.pixels_bgra();
        let fg = straight_linear_rgba_to_premul_bgra_f32(glyph.color);
        let color_glyph = glyph.flags[0] >= 0.5;
        for yy in y0..y1 {
            let ty = ((yy as f32 + 0.5 - y) / h).clamp(0.0, 0.999_999);
            let sy = ay0 + ((ay1 - ay0) as f32 * ty).floor() as u32;
            let row = yy as usize * self.width as usize * 4;
            for xx in x0..x1 {
                let tx = ((xx as f32 + 0.5 - x) / w).clamp(0.0, 0.999_999);
                let sx = ax0 + ((ax1 - ax0) as f32 * tx).floor() as u32;
                let src_off = ((sy * atlas_w + sx) * 4) as usize;
                if src_off + 4 > atlas_pixels.len() {
                    continue;
                }
                let src = if color_glyph {
                    bgra8_to_premul_f32(&atlas_pixels[src_off..src_off + 4])
                } else {
                    let cov = atlas_pixels[src_off + 3] as f32 / 255.0;
                    [fg[0] * cov, fg[1] * cov, fg[2] * cov, fg[3] * cov]
                };
                if src[3] <= 0.0 {
                    continue;
                }
                let dst_off = row + xx as usize * 4;
                blend_premul_bgra(&mut self.pixels[dst_off..dst_off + 4], src);
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

unsafe fn blit_bgra_to_hwnd(
    hwnd: HWND,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> anyhow::Result<()> {
    let hdc = unsafe { GetDC(Some(hwnd)) };
    if hdc.0.is_null() {
        anyhow::bail!("GetDC failed");
    }
    let result = unsafe { blit_bgra_to_hdc(hdc, width, height, pixels) };
    let _ = unsafe { ReleaseDC(Some(hwnd), hdc) };
    result
}

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
    let rows = unsafe {
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
        return [0.0; 4];
    }
    let r = (color[0] / a).clamp(0.0, 1.0);
    let g = (color[1] / a).clamp(0.0, 1.0);
    let b = (color[2] / a).clamp(0.0, 1.0);
    [linear_to_srgb(b) * a, linear_to_srgb(g) * a, linear_to_srgb(r) * a, a]
}

fn straight_linear_rgba_to_premul_bgra_f32(color: [f32; 4]) -> [f32; 4] {
    let a = color[3].clamp(0.0, 1.0);
    [
        linear_to_srgb(color[2].clamp(0.0, 1.0)) * a,
        linear_to_srgb(color[1].clamp(0.0, 1.0)) * a,
        linear_to_srgb(color[0].clamp(0.0, 1.0)) * a,
        a,
    ]
}

fn bgra8_to_premul_f32(px: &[u8]) -> [f32; 4] {
    [px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0, px[3] as f32 / 255.0]
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

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
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
    let t = if denom <= f32::EPSILON { 0.0 } else { ((wx * vx + wy * vy) / denom).clamp(0.0, 1.0) };
    let cx = ax + t * vx;
    let cy = ay + t * vy;
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}
