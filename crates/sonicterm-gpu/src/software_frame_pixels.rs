//! Software-presenter color conversion, subpixel blending and bounded atlas sampling.

use super::*;

pub(super) fn linear_rgba_to_bgra(color: [f32; 4]) -> [u8; 4] {
    let a = color[3].clamp(0.0, 1.0);
    [
        to_u8(linear_to_srgb(color[2].clamp(0.0, 1.0))),
        to_u8(linear_to_srgb(color[1].clamp(0.0, 1.0))),
        to_u8(linear_to_srgb(color[0].clamp(0.0, 1.0))),
        to_u8(a),
    ]
}

pub(super) fn bgra8_to_unorm(px: [u8; 4]) -> [f32; 4] {
    [px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0, px[3] as f32 / 255.0]
}

pub(super) fn premultiplied_srgb_bgra_to_linear_rgba(px: [u8; 4]) -> [f32; 4] {
    let alpha = px[3];
    if alpha == 0 {
        // When: alpha is zero, match GPU upload by discarding invalid encoded RGB before filtering.
        return [0.0; 4];
    }

    [
        premultiplied_srgb_channel_to_linear(px[2], alpha),
        premultiplied_srgb_channel_to_linear(px[1], alpha),
        premultiplied_srgb_channel_to_linear(px[0], alpha),
        alpha as f32 / 255.0,
    ]
}

#[inline]
pub(super) fn premultiplied_srgb_channel_to_linear(encoded_premul: u8, alpha: u8) -> f32 {
    // The fixed 64 KiB table avoids transfer-curve evaluation for every bilinear tap.
    static STAGED_CHANNELS: std::sync::OnceLock<[u8; 256 * 256]> = std::sync::OnceLock::new();
    let staged = STAGED_CHANNELS.get_or_init(|| {
        std::array::from_fn(|index| {
            let alpha = (index >> 8) as u8;
            if alpha == 0 {
                // When: alpha is zero, the staged channel must use canonical transparent black.
                return 0;
            }
            let encoded_premul = index as u8;
            let straight_srgb = (encoded_premul as f64 / alpha as f64).clamp(0.0, 1.0);
            let alpha_linear = alpha as f64 / 255.0;
            let premul_linear = srgb_channel_to_linear(straight_srgb) * alpha_linear;
            linear_channel_to_srgb_u8(premul_linear as f32)
        })
    })[(usize::from(alpha) << 8) | usize::from(encoded_premul)];
    srgb_u8_to_linear_lut()[staged as usize]
}

pub(super) fn stabilize_half_pixel_origin(value: f32) -> f32 {
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
pub(super) fn sample_atlas_bilinear(
    pixels: &[u8],
    width: u32,
    height: u32,
    x: f32,
    y: f32,
) -> [f32; 4] {
    sample_color_atlas_bilinear_in_rect(pixels, width, height, x, y, (0, 0, width, height))
}

pub(super) fn sample_color_atlas_bilinear_in_rect(
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
    let c00 = premultiplied_srgb_bgra_to_linear_rgba(bgra_pixel_at(pixels, width, x0, y0));
    let c10 = premultiplied_srgb_bgra_to_linear_rgba(bgra_pixel_at(pixels, width, x1, y0));
    let c01 = premultiplied_srgb_bgra_to_linear_rgba(bgra_pixel_at(pixels, width, x0, y1));
    let c11 = premultiplied_srgb_bgra_to_linear_rgba(bgra_pixel_at(pixels, width, x1, y1));
    let mut out = [0.0; 4];
    for i in 0..4 {
        let top = c00[i] * (1.0 - fx) + c10[i] * fx;
        let bottom = c01[i] * (1.0 - fx) + c11[i] * fx;
        out[i] = top * (1.0 - fy) + bottom * fy;
    }
    out
}

pub(super) fn bgra_pixel_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let off = ((y * width + x) * 4) as usize;
    if off + 4 > pixels.len() {
        // When: off runs past pixels the coordinate lies outside the atlas upload, so a
        // transparent texel is returned rather than panicking on the slice.
        return [0; 4];
    }
    [pixels[off], pixels[off + 1], pixels[off + 2], pixels[off + 3]]
}

pub(super) fn blend_subpixel_bgra(dst: &mut [u8], coverage_rgba: [f32; 4], foreground: [f32; 4]) {
    let alpha = foreground[3].clamp(0.0, 1.0);
    let weights = [
        (coverage_rgba[0] * alpha).clamp(0.0, 1.0),
        (coverage_rgba[1] * alpha).clamp(0.0, 1.0),
        (coverage_rgba[2] * alpha).clamp(0.0, 1.0),
    ];
    let decode = crate::color::srgb_u8_to_linear_lut();
    let out_r = foreground[0] * coverage_rgba[0] + decode[dst[2] as usize] * (1.0 - weights[0]);
    let out_g = foreground[1] * coverage_rgba[1] + decode[dst[1] as usize] * (1.0 - weights[1]);
    let out_b = foreground[2] * coverage_rgba[2] + decode[dst[0] as usize] * (1.0 - weights[2]);
    dst[0] = crate::color::linear_channel_to_srgb_u8(out_b);
    dst[1] = crate::color::linear_channel_to_srgb_u8(out_g);
    dst[2] = crate::color::linear_channel_to_srgb_u8(out_r);
    let source_alpha = weights.into_iter().fold(0.0_f32, f32::max);
    let destination_alpha = dst[3] as f32 / 255.0;
    dst[3] = to_u8(source_alpha + destination_alpha * (1.0 - source_alpha));
}

pub(super) fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        // When: v is above the linear toe the sRGB transfer curve switches to its gamma
        // segment, which the two pieces are chosen to meet continuously at that value.
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

pub(super) fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

pub(super) fn distance_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
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
#[path = "software_frame_pixels_tests.rs"]
mod software_frame_pixels_tests;
