use sonic_gpu::atlas_upload::atlas_sampler_descriptor;

#[test]
fn atlas_sampler_uses_nearest_filtering() {
    let desc = atlas_sampler_descriptor();

    assert_eq!(desc.mag_filter, wgpu::FilterMode::Nearest);
    assert_eq!(desc.min_filter, wgpu::FilterMode::Nearest);
    assert_eq!(desc.mipmap_filter, wgpu::MipmapFilterMode::Nearest);
}
