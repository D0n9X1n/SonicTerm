//! Presentation vertex geometry and glyph color flag contracts.

use super::*;

/// Line geometry must not overwrite the color transform carried by each vertex.
#[test]
pub(super) fn line_vertices_keep_geometry_out_of_hsv() {
    let color = [0.0, 1.0, 1.0, 1.0];
    let line_a = [-5.0, -2.0];
    let line_b = [7.0, 3.0];
    let thickness = 2.5;
    let line = QuadInstance::line(
        crate::quad::px_to_ndc(2.0, 3.0, 16.0, 10.0, 32.0, 24.0),
        color,
        [16.0, 10.0],
        line_a,
        line_b,
        thickness,
    );
    let mut vertices = Vec::new();

    push_quad_instances(&mut vertices, &[line], 32.0, 24.0);

    assert_eq!(vertices.len(), VERTICES_PER_QUAD);
    for vertex in vertices {
        assert_eq!(vertex.has_color, IS_LINE);
        assert_eq!(vertex.alt_color, [line_a[0], line_a[1], line_b[0], line_b[1]]);
        assert_eq!(vertex.mix_value, thickness);
        assert_eq!(vertex.hsv, [1.0, 1.0, 1.0]);
        assert_eq!(vertex.fg_color, color);
    }
}

/// Moving line geometry out of HSV must leave the ordinary glyph transform contract intact.
#[test]
pub(super) fn glyph_vertices_retain_foreground_hsv_contract() {
    let color = [0.2, 0.3, 0.4, 1.0];
    let glyph = GlyphInstance {
        rect: crate::quad::px_to_ndc(1.0, 2.0, 4.0, 6.0, 16.0, 12.0),
        uv: [0.0, 0.0, 1.0, 1.0],
        color,
        flags: [0.0; 4],
    };
    let mut vertices = Vec::new();

    push_glyph_instances(
        &mut vertices,
        &[glyph],
        16.0,
        12.0,
        sonicterm_render_model::boundary::cfg::config::SubpixelAaMode::Off,
    );

    assert_eq!(vertices.len(), VERTICES_PER_QUAD);
    for vertex in vertices {
        assert_eq!(vertex.has_color, IS_GLYPH);
        assert_eq!(vertex.hsv, [1.0, 1.0, 1.0]);
        assert_eq!(vertex.fg_color, color);
    }
    assert!(SHADER.contains("hsv *= uniforms.foreground_text_hsb;"));
}

/// LCD policy reclassifies only ordinary subpixel glyphs and preserves higher-priority kinds.
#[test]
pub(super) fn subpixel_vertex_kind_respects_mode_and_primitive_precedence() {
    use sonicterm_render_model::boundary::cfg::config::SubpixelAaMode::{Bgr, Off, Rgb};

    let instance = |flags| GlyphInstance {
        rect: crate::quad::px_to_ndc(1.0, 2.0, 4.0, 6.0, 16.0, 12.0),
        uv: [0.0, 0.0, 1.0, 1.0],
        color: [0.2, 0.3, 0.4, 1.0],
        flags,
    };
    let kind = |glyph: GlyphInstance, mode| {
        let mut vertices = Vec::new();
        push_glyph_instances(&mut vertices, &[glyph], 16.0, 12.0, mode);
        vertices[0].has_color
    };

    assert_eq!(kind(instance([0.0, 1.0, 0.0, 0.0]), Off), IS_GLYPH);
    assert_eq!(kind(instance([0.0, 1.0, 0.0, 0.0]), Rgb), IS_SUBPIXEL_RGB);
    assert_eq!(kind(instance([0.0, 1.0, 0.0, 0.0]), Bgr), IS_SUBPIXEL_BGR);
    assert_eq!(kind(instance([1.0, 1.0, 0.0, 0.0]), Rgb), IS_COLOR_EMOJI);
    let image = ImageInstance {
        rect_px: [1.0, 2.0, 4.0, 6.0],
        uv: [0.25, 0.25, 0.5, 0.75],
        sample_uv: [0.0, 0.0, 1.0, 1.0],
    };
    let mut vertices = Vec::new();
    push_image_instances(&mut vertices, &[image], 16.0, 12.0);
    assert!(vertices.iter().all(|vertex| vertex.has_color == IS_IMAGE));
    assert!(vertices.iter().all(|vertex| vertex.alt_color == image.sample_uv));
    assert_eq!(vertices[0].tex, [0.25, 0.25]);
    assert_eq!(vertices[3].tex, [0.5, 0.75]);
}

/// The dual-source shader carries independent source color and destination attenuation factors.
#[test]
pub(super) fn dual_source_shader_declares_two_blend_sources_and_subpixel_weights() {
    let shader = shader_source(true);

    assert!(shader.contains("enable dual_source_blending;"));
    assert!(shader.contains("@location(0) @blend_src(0) color"));
    assert!(shader.contains("@location(0) @blend_src(1) factor"));
    assert!(shader.contains("coverage * transformed_foreground.a"));
    assert!(shader.contains("transformed_foreground.rgb * coverage"));
}

/// Primitive kind is categorical state and must never be perspective-interpolated.
#[test]
pub(super) fn primitive_kind_uses_flat_interpolation() {
    assert!(SHADER.contains("@location(4) @interpolate(flat) has_color: f32,"));
}

/// Line antialiasing must keep smoothstep edges ordered when derivatives are zero.
#[test]
pub(super) fn line_antialiasing_has_a_positive_derivative_floor() {
    assert!(SHADER.contains("let w = max(fwidth(d), 1.0e-4);"));
}
