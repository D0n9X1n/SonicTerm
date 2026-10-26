//! Monochrome weight coverage transforms and their checked bitmap geometry.

use super::*;

pub(super) fn sanitize_weight_scale(scale: f32) -> f32 {
    if scale.is_finite() && (0.5..=5.0).contains(&scale) {
        scale
    } else {
        // When: `scale` is nonfinite or outside the supported range, identity avoids pathological coverage math.
        1.0
    }
}

/// Glyph outline growth per unit of `weight_scale` above 1.0, expressed as a
/// fraction of the cell height. Tuned so `weight_scale = 2.0` adds roughly
/// half a pixel of radius at a 13pt Retina cell.
const EMBOLDEN_RADIUS_PER_CELL_H: f64 = 0.02;

/// Raster-space growth ceiling while the output stays inside the original tile.
///
/// Larger bitmap dilations flatten counters and saturate the tile edge after
/// crop-back. The ceiling is deliberately independent of the glyph's existing
/// margins: flat-sided glyphs often touch one bitmap edge, and consulting spare
/// margin would disable growth for those while still growing curved glyphs.
const MAX_EMBOLDEN_RADIUS_PX: f64 = 1.0;

/// Radius, in raster px, that monochrome text should grow at `scale`. Zero at or
/// below `1.0`, where [`thin_radius_px`] takes over instead.
pub(super) fn embolden_radius_px(scale: f32, cell_h: f64) -> f64 {
    if scale <= 1.0 || !cell_h.is_finite() || cell_h <= 0.0 {
        // When: `scale` does not request growth or `cell_h` is unusable, disable emboldening instead of guessing a radius.
        return 0.0;
    }
    f64::from(scale - 1.0) * cell_h * EMBOLDEN_RADIUS_PER_CELL_H
}

/// Outline shrink per unit of `weight_scale` below 1.0, as a fraction of cell
/// height. Slightly larger per unit than [`EMBOLDEN_RADIUS_PER_CELL_H`]
/// because thinning only has 0.5 of range to work with (0.5..1.0) against
/// emboldening's 4.0. Kept low enough that a small thin such as `0.9` stays a
/// nudge and leaves stem cores opaque rather than washing the glyph out.
const THIN_RADIUS_PER_CELL_H: f64 = 0.012;

/// Radius, in raster px, that monochrome text should shrink at `scale`. Zero at
/// or above `1.0`. Like emboldening, this exists because the coverage remap
/// cannot move a pixel that is already fully opaque — at HiDPI a stem core is
/// solid, so gamma alone leaves the stem exactly as wide as it started.
pub(super) fn thin_radius_px(scale: f32, cell_h: f64) -> f64 {
    if scale >= 1.0 || !cell_h.is_finite() || cell_h <= 0.0 {
        // When: `scale` does not request thinning or `cell_h` is unusable, disable erosion instead of guessing a radius.
        return 0.0;
    }
    f64::from(1.0 - scale) * cell_h * THIN_RADIUS_PER_CELL_H
}

pub(super) fn checked_coverage_len(width: usize, height: usize, channels: usize) -> Option<usize> {
    if width == 0
        || height == 0
        || width > MAX_RASTERIZED_GLYPH_DIMENSION
        || height > MAX_RASTERIZED_GLYPH_DIMENSION
    {
        // When: `width` or `height` is zero or exceeds the raster limit, no legal allocation exists.
        return None;
    }
    width.checked_mul(height)?.checked_mul(channels)
}

/// Shrink `coverage` inward by `radius` px in place. Unlike growth, erosion
/// never needs padding — the glyph only loses ink — so tile dimensions and
/// offsets are unchanged and the caller can swap the buffer straight in.
///
/// Returns `None` when there is nothing to do.
pub(super) fn erode_coverage(
    coverage: &[u8],
    width: usize,
    height: usize,
    radius: f64,
    is_subpixel: bool,
) -> Option<Vec<u8>> {
    if radius <= 0.0 || width == 0 || height == 0 {
        // When: `radius` or tile dimensions are nonpositive, erosion has no valid work to perform.
        return None;
    }
    let channels = if is_subpixel { 4 } else { 1 };
    let byte_len = checked_coverage_len(width, height, channels)?;
    if coverage.len() != byte_len {
        // When: `coverage.len()` differs from `byte_len`, morphology would index outside the supplied tile.
        return None;
    }
    let pixel_len = width.checked_mul(height)?;
    let mut out = vec![0u8; byte_len];
    for channel in 0..channels {
        let mut plane = vec![0u8; pixel_len];
        for pixel in 0..pixel_len {
            plane[pixel] = coverage[pixel * channels + channel];
        }
        let mut tmp = vec![0u8; pixel_len];
        morph_axis(&plane, &mut tmp, width, height, width, radius, true);
        let mut transposed = vec![0u8; pixel_len];
        for row in 0..height {
            for column in 0..width {
                transposed[column * height + row] = tmp[row * width + column];
            }
        }
        let mut tcol = vec![0u8; pixel_len];
        morph_axis(&transposed, &mut tcol, height, width, height, radius, true);
        for row in 0..height {
            for column in 0..width {
                out[(row * width + column) * channels + channel] = tcol[column * height + row];
            }
        }
    }
    if is_subpixel {
        for px in out.as_chunks_mut::<4>().0 {
            px[3] = px[0].max(px[1]).max(px[2]);
        }
    }
    Some(out)
}

/// One separable morphology pass with fractional radius. `radius` is split
/// into an integer core, taken at full strength, and a fractional outer ring
/// that is blended in proportionally so the change is smooth rather than
/// snapping a whole pixel at a time.
///
/// `erode` selects the operator: max-filter (grow) when false, min-filter
/// (shrink) when true. The two differ at the boundary — a max-filter ignores
/// out-of-bounds samples, while a min-filter must treat them as empty so the
/// glyph erodes inward from its own edge.
fn morph_axis(
    src: &[u8],
    dst: &mut [u8],
    len: usize,
    count: usize,
    stride: usize,
    radius: f64,
    erode: bool,
) {
    let whole = radius.floor() as usize;
    let frac = radius - radius.floor();
    for line in 0..count {
        let base = line * stride;
        for position in 0..len {
            let low = position.saturating_sub(whole);
            let high = (position + whole).min(len - 1);
            if erode {
                // A window that overhangs the tile edge sees empty space
                // there, so the glyph erodes inward from its own rim. Only
                // genuinely out-of-bounds samples count as empty — treating
                // in-bounds neighbours as empty would erode the whole glyph
                // rather than its edge.
                let mut best =
                    if position < whole || position + whole >= len { 0u8 } else { u8::MAX };
                for neighbor in low..=high {
                    best = best.min(src[base + neighbor]);
                }
                if frac > 0.0 && best > 0 {
                    // Outer ring one step beyond the integer core, on both
                    // sides. Out-of-bounds reads as empty.
                    let left = if position > whole {
                        src[base + position - whole - 1]
                    } else {
                        // When: `position` has no sample beyond the left core, erosion sees empty space at the tile edge.
                        0
                    };
                    let right = if position + whole + 1 < len {
                        src[base + position + whole + 1]
                    } else {
                        // When: the right outer sample exceeds `len`, erosion sees empty space at the tile edge.
                        0
                    };
                    let ring = left.min(right);
                    if ring < best {
                        // Pull toward the ring minimum in proportion to frac.
                        let blended = f64::from(best) - (f64::from(best) - f64::from(ring)) * frac;
                        best = blended.round().clamp(0.0, 255.0) as u8;
                    }
                }
                dst[base + position] = best;
            } else {
                // When: `erode` is false, use a max filter to grow coverage without empty edge samples.
                let mut best = 0u8;
                for neighbor in low..=high {
                    best = best.max(src[base + neighbor]);
                }
                if frac > 0.0 {
                    let mut ring = 0u8;
                    if position > whole {
                        ring = ring.max(src[base + position - whole - 1]);
                    }
                    if position + whole + 1 < len {
                        ring = ring.max(src[base + position + whole + 1]);
                    }
                    let blended = f64::from(ring) * frac;
                    best = best.max(blended.round().clamp(0.0, 255.0) as u8);
                }
                dst[base + position] = best;
            }
        }
    }
}

/// Grow `coverage` outward by `radius` raster pixels without changing the
/// returned tile dimensions or origin.
///
/// The operation pads scratch space so max-filter samples can cross the old
/// bitmap edge, then crops back to the original bounds. Radius is capped by
/// [`MAX_EMBOLDEN_RADIUS_PX`] rather than by spare bitmap margin: a flat-sided
/// glyph commonly touches an edge, and margin-dependent growth would make
/// weight behavior vary by glyph shape.
///
/// Returns `None` when there is nothing to do, the declared final dimensions
/// exceed the atlas limit, the input buffer does not match them exactly, or
/// checked scratch arithmetic overflows. The bounded scratch padding may exceed
/// the final atlas dimensions because it is cropped before return.
pub(super) fn embolden_coverage(
    coverage: &[u8],
    width: usize,
    height: usize,
    radius: f64,
    is_subpixel: bool,
) -> Option<(Vec<u8>, usize, usize, usize)> {
    if radius <= 0.0 || width == 0 || height == 0 {
        // When: `radius` or tile dimensions are nonpositive, growth has no valid work to perform.
        return None;
    }
    let channels = if is_subpixel { 4 } else { 1 };
    let byte_len = checked_coverage_len(width, height, channels)?;
    if coverage.len() != byte_len {
        // When: `coverage.len()` differs from `byte_len`, dilation would index outside the supplied tile.
        return None;
    }
    // A shape-independent ceiling prevents high-weight crop saturation without
    // making flat glyphs (which commonly touch one bitmap edge) grow less than
    // curved glyphs. Fractional values below the ceiling still blend smoothly.
    let radius = radius.min(MAX_EMBOLDEN_RADIUS_PX);
    let pad = radius.ceil() as usize;
    let doubled_pad = pad.checked_mul(2)?;
    let new_w = width.checked_add(doubled_pad)?;
    let new_h = height.checked_add(doubled_pad)?;
    let scratch_pixels = new_w.checked_mul(new_h)?;
    let scratch_bytes = scratch_pixels.checked_mul(channels)?;
    let mut padded = vec![0u8; scratch_bytes];
    for row in 0..height {
        let src = row * width * channels;
        let dst = ((row + pad) * new_w + pad) * channels;
        padded[dst..dst + width * channels].copy_from_slice(&coverage[src..src + width * channels]);
    }

    // Dilate each channel independently. Interleaved BGRA is handled by
    // deinterleaving into a scratch plane, since the separable passes need a
    // contiguous stride per axis. Every byte is written below, so the buffer
    // starts zeroed rather than copied.
    let mut out = vec![0u8; scratch_bytes];
    for channel in 0..channels {
        let mut plane = vec![0u8; scratch_pixels];
        for pixel in 0..scratch_pixels {
            plane[pixel] = padded[pixel * channels + channel];
        }
        let mut tmp = vec![0u8; scratch_pixels];
        // Horizontal: new_h lines of new_w samples, stride new_w.
        morph_axis(&plane, &mut tmp, new_w, new_h, new_w, radius, false);
        // Vertical: transpose, reuse the same row-wise pass, transpose back.
        let mut transposed = vec![0u8; scratch_pixels];
        for row in 0..new_h {
            for column in 0..new_w {
                transposed[column * new_h + row] = tmp[row * new_w + column];
            }
        }
        let mut tcol = vec![0u8; scratch_pixels];
        morph_axis(&transposed, &mut tcol, new_h, new_w, new_h, radius, false);
        for row in 0..new_h {
            for column in 0..new_w {
                out[(row * new_w + column) * channels + channel] = tcol[column * new_h + row];
            }
        }
    }

    if is_subpixel {
        // Alpha is the envelope of the dilated RGB coverage.
        for px in out.as_chunks_mut::<4>().0 {
            px[3] = px[0].max(px[1]).max(px[2]);
        }
    }

    // Crop back to the original bounds. The padding exists so the dilation has
    // somewhere to expand into; keeping it would grow the tile, and a tile that
    // grows makes a *weight* control read as a *size* control. Every glyph then
    // changes size when the user asks for more ink, and glyphs from different
    // fonts change by different amounts — which is what makes two adjacent
    // markers drift apart.
    //
    // Ink that lands outside the original bounds is discarded. That is the
    // right trade: a rasterized glyph carries margin around its outline, so
    // there is room to thicken into, and where there is not, losing a fraction
    // of a pixel at the edge is less visible than every glyph resizing.
    let mut cropped = vec![0u8; byte_len];
    for row in 0..height {
        let src = ((row + pad) * new_w + pad) * channels;
        let dst = row * width * channels;
        cropped[dst..dst + width * channels].copy_from_slice(&out[src..src + width * channels]);
    }
    // `pad` is reported as zero: the caller shifts the tile offset by it, and
    // the tile no longer moved.
    Some((cropped, width, height, 0))
}

pub(super) fn scale_coverage(coverage: u8, scale: f32) -> u8 {
    if coverage == 0 || coverage == u8::MAX || (scale - 1.0).abs() < f32::EPSILON {
        // When: `coverage` is an endpoint or `scale` is identity, gamma remapping cannot change the byte.
        return coverage;
    }
    let normalized = f32::from(coverage) / 255.0;
    let exponent = 1.0 / scale;
    (normalized.powf(exponent) * 255.0).round().clamp(0.0, 255.0) as u8
}

pub(super) fn apply_weight_scale(coverage: &mut [u8], scale: f32, is_subpixel: bool) {
    let scale = sanitize_weight_scale(scale);
    if (scale - 1.0).abs() < f32::EPSILON {
        // When: sanitized `scale` is identity, leave every coverage byte and subpixel alpha untouched.
        return;
    }
    if is_subpixel {
        for pixel in coverage.as_chunks_mut::<4>().0 {
            pixel[0] = scale_coverage(pixel[0], scale);
            pixel[1] = scale_coverage(pixel[1], scale);
            pixel[2] = scale_coverage(pixel[2], scale);
            pixel[3] = pixel[0].max(pixel[1]).max(pixel[2]);
        }
    } else {
        // When: `is_subpixel` is false, each byte is a complete scalar coverage sample.
        for value in coverage {
            *value = scale_coverage(*value, scale);
        }
    }
}

#[cfg(test)]
#[path = "fontstack_weight_tests.rs"]
mod fontstack_weight_tests;
