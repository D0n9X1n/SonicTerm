//! Ordered quad, glyph and image vertex emission for the presentation pipeline.

use super::*;

/// Append one frame's vertices to `out` and return how many belong to the reset quad.
///
/// Degenerate primitives push nothing, so the returned count and `out.len()` are the
/// only sources for draw ranges.
pub(crate) fn assemble_vertices(
    out: &mut Vec<Vertex>,
    reset: Option<&QuadInstance>,
    layers: &PipelineLayers<'_>,
    surface_w: f32,
    surface_h: f32,
    subpixel_aa: SubpixelAaMode,
) -> usize {
    push_quad_instances(out, reset.map(std::slice::from_ref).unwrap_or(&[]), surface_w, surface_h);
    let reset_vertices = out.len();
    push_quad_instances(out, layers.quads, surface_w, surface_h);
    push_image_instances(out, layers.images, surface_w, surface_h);
    push_glyph_instances(out, layers.glyphs, surface_w, surface_h, subpixel_aa);
    push_quad_instances(out, layers.overlay_quads, surface_w, surface_h);
    push_glyph_instances(out, layers.overlay_glyphs, surface_w, surface_h, subpixel_aa);
    reset_vertices
}

/// Index ranges for the reset draw and the main draw, from emitted vertex counts only.
pub(crate) fn draw_index_ranges(
    reset_vertices: usize,
    total_vertices: usize,
) -> (std::ops::Range<u32>, std::ops::Range<u32>) {
    let reset_indices = (reset_vertices / VERTICES_PER_QUAD * INDICES_PER_QUAD) as u32;
    let total_indices = (total_vertices / VERTICES_PER_QUAD * INDICES_PER_QUAD) as u32;
    (0..reset_indices, reset_indices..total_indices)
}

pub(super) fn push_image_instances(
    out: &mut Vec<Vertex>,
    images: &[ImageInstance],
    sw: f32,
    sh: f32,
) {
    for image in images {
        let [x, y, w, h] = image.rect_px;
        if w <= 0.0 || h <= 0.0 {
            // When: `w` or `h` is empty, no image fragments can be produced.
            continue;
        }
        let [u0, v0, u1, v1] = image.uv;
        let tex = [[u0, v0], [u1, v0], [u0, v1], [u1, v1]];
        push_rect_vertices(
            out,
            x,
            y,
            w,
            h,
            sw,
            sh,
            [1.0; 4],
            IS_IMAGE,
            tex,
            [image.sample_uv; VERTICES_PER_QUAD],
        );
    }
}

pub(super) fn push_glyph_instances(
    out: &mut Vec<Vertex>,
    glyphs: &[GlyphInstance],
    sw: f32,
    sh: f32,
    subpixel_aa: SubpixelAaMode,
) {
    for g in glyphs {
        let Some((x, y, w, h)) = ndc_rect_to_pixels(g.rect, sw, sh) else {
            // When: ndc_rect_to_pixels returns None the surface has zero extent, so this
            // glyph is dropped instead of scaled against a zero-sized frame.
            continue;
        };
        if w <= 0.0 || h <= 0.0 {
            // When: w or h is not positive the quad is degenerate or inverted, adding no
            // fragments while still consuming vertices and indices in the batch.
            continue;
        }
        let color = g.color;
        let has_color = if g.flags[0] >= 0.5 {
            IS_COLOR_EMOJI
        } else if g.flags[1] >= 0.5 {
            // When: `g.flags[1]` marks a subpixel mask, `subpixel_aa` selects its presentation classification.
            match subpixel_aa {
                SubpixelAaMode::Off => IS_GLYPH,
                SubpixelAaMode::Rgb => IS_SUBPIXEL_RGB,
                SubpixelAaMode::Bgr => IS_SUBPIXEL_BGR,
            }
        } else {
            // When: no flags bit is set, so the glyph is monochrome: atlas alpha is coverage
            // and the shader scales fg_color by it, then applies foreground_text_hsb.
            IS_GLYPH
        };
        let [u0, v0, u1, v1] = g.uv;
        let tex = [[u0, v0], [u1, v0], [u0, v1], [u1, v1]];
        push_rect_vertices(
            out,
            x,
            y,
            w,
            h,
            sw,
            sh,
            color,
            has_color,
            tex,
            [[0.0; 4]; VERTICES_PER_QUAD],
        );
    }
}

pub(super) fn push_quad_instances(out: &mut Vec<Vertex>, quads: &[QuadInstance], sw: f32, sh: f32) {
    for q in quads {
        let Some((x, y, w, h)) = ndc_rect_to_pixels(q.rect, sw, sh) else {
            // When: ndc_rect_to_pixels returns None the surface has zero extent, so this
            // quad is dropped instead of scaled against a zero-sized frame.
            continue;
        };
        if w <= 0.0 || h <= 0.0 {
            // When: w or h is not positive the quad is degenerate or inverted, adding no
            // fragments while still consuming vertices and indices in the batch.
            continue;
        }
        let kind = if q.line_thickness_px > 0.0 {
            IS_LINE
        } else if q.radius_px > 0.0 {
            // When: radius_px is positive the shader evaluates a rounded-rect distance
            // field, so the corners stay antialiased instead of squared off.
            IS_ROUNDED_RECT
        } else {
            // When: neither line_thickness_px nor radius_px is set the quad is a plain
            // fill, so the shader emits fg_color with no distance-field pass.
            IS_SOLID_COLOR
        };
        let size = if q.size_px[0] > 0.0 && q.size_px[1] > 0.0 {
            q.size_px
        } else {
            // When: size_px is unset the pixel extent w and h stand in, keeping the
            // distance field in the same units the vertices were built from.
            [w, h]
        };
        let local = [
            [-size[0] * 0.5, -size[1] * 0.5],
            [size[0] * 0.5, -size[1] * 0.5],
            [-size[0] * 0.5, size[1] * 0.5],
            [size[0] * 0.5, size[1] * 0.5],
        ];
        let params = [
            [size[0], size[1], q.radius_px, q.line_thickness_px],
            [size[0], size[1], q.radius_px, q.line_thickness_px],
            [size[0], size[1], q.radius_px, q.line_thickness_px],
            [size[0], size[1], q.radius_px, q.line_thickness_px],
        ];
        push_rect_vertices(out, x, y, w, h, sw, sh, q.color, kind, local, params);
        if kind == IS_LINE {
            let n = out.len();
            for v in &mut out[n - VERTICES_PER_QUAD..n] {
                v.alt_color = [q.line_a[0], q.line_a[1], q.line_b[0], q.line_b[1]];
                v.mix_value = q.line_thickness_px;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_rect_vertices(
    out: &mut Vec<Vertex>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
    has_color: f32,
    tex: [[f32; 2]; 4],
    params: [[f32; 4]; 4],
) {
    let left = x - sw * 0.5;
    let right = x + w - sw * 0.5;
    let top = y - sh * 0.5;
    let bottom = y + h - sh * 0.5;
    let positions = [[left, top], [right, top], [left, bottom], [right, bottom]];
    for i in 0..VERTICES_PER_QUAD {
        out.push(Vertex {
            position: positions[i],
            tex: tex[i],
            fg_color: color,
            alt_color: params[i],
            hsv: [1.0, 1.0, 1.0],
            has_color,
            mix_value: 0.0,
        });
    }
}

pub(crate) fn ndc_rect_to_pixels(rect: [f32; 4], sw: f32, sh: f32) -> Option<(f32, f32, f32, f32)> {
    if sw <= 0.0 || sh <= 0.0 {
        // When: sw or sh is not positive the surface has no pixel extent and no NDC
        // mapping exists, so every caller skips the primitive instead of scaling by zero.
        return None;
    }
    let x = (rect[0] + 1.0) * 0.5 * sw;
    let w = rect[2] * 0.5 * sw;
    let h = rect[3] * 0.5 * sh;
    let top_ndc = rect[1] + rect[3];
    let y = (1.0 - top_ndc) * 0.5 * sh;
    Some((x, y, w, h))
}

pub(super) fn build_indices(quad_count: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(quad_count * INDICES_PER_QUAD);
    for q in 0..quad_count {
        let base = (q * VERTICES_PER_QUAD) as u32;
        out.extend_from_slice(&[
            base + V_TOP_LEFT,
            base + V_TOP_RIGHT,
            base + V_BOT_LEFT,
            base + V_TOP_RIGHT,
            base + V_BOT_LEFT,
            base + V_BOT_RIGHT,
        ]);
    }
    out
}

pub(super) fn orthographic_projection(sw: f32, sh: f32) -> [[f32; 4]; 4] {
    // Matches WezTerm's pixel-space projection:
    // left=-w/2, right=w/2, bottom=h/2, top=-h/2.
    [
        [2.0 / sw, 0.0, 0.0, 0.0],
        [0.0, -2.0 / sh, 0.0, 0.0],
        [0.0, 0.0, -1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

#[cfg(test)]
#[path = "wezterm_pipeline_vertices_tests.rs"]
mod wezterm_pipeline_vertices_tests;
