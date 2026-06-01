//! Regression guard for #375: `QuadPipeline` must use premultiplied-alpha
//! blend factors (`src=One, dst=OneMinusSrcAlpha`), NOT the straight-alpha
//! `wgpu::BlendState::ALPHA_BLENDING`.
//!
//! `QuadInstance::color` is documented as premultiplied and the chrome layer
//! constructs colors that way; pairing premultiplied source with
//! straight-alpha factors double-multiplies the alpha and on transparent
//! Win11 Mica surfaces makes dark tab chrome blend nearly into the clear
//! backdrop — the invisible-tab-bar bug.

use sonicterm_gpu::quad::premultiplied_alpha_blend;

#[test]
fn quad_blend_is_premultiplied_not_straight_alpha() {
    let blend = premultiplied_alpha_blend();

    // Must NOT be the straight-alpha preset.
    assert_ne!(
        blend,
        wgpu::BlendState::ALPHA_BLENDING,
        "QuadPipeline must use premultiplied-alpha blending (see #375)"
    );

    // Color channel: src=One, dst=OneMinusSrcAlpha, op=Add.
    assert_eq!(blend.color.src_factor, wgpu::BlendFactor::One);
    assert_eq!(blend.color.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
    assert_eq!(blend.color.operation, wgpu::BlendOperation::Add);

    // Alpha channel: src=One, dst=OneMinusSrcAlpha, op=Add.
    assert_eq!(blend.alpha.src_factor, wgpu::BlendFactor::One);
    assert_eq!(blend.alpha.dst_factor, wgpu::BlendFactor::OneMinusSrcAlpha);
    assert_eq!(blend.alpha.operation, wgpu::BlendOperation::Add);
}
