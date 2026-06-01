//! Regression test for the P0 vim-scroll crash:
//!
//! ```text
//! wgpu error: Validation Error
//! In Queue::write_buffer
//!   Copy at offset 0 for 4032 bytes would end up overrunning the bounds
//!   of the Destination buffer of size 3888
//! ```
//!
//! Root cause: `QuadPipeline::draw`'s grow path used
//! `create_buffer_init(contents: instances)`, which sized the buffer to
//! `instances.len() * stride`. Capacity, however, was tracked as
//! `instances.len().next_power_of_two()`. The next draw with an instance
//! count `> instances.len()` but `<= capacity` slipped past the bounds
//! check and `write_buffer` overran the actual buffer end.
//!
//! Reproduction: grow once with N instances (so the buffer is sized for
//! N but capacity claims 2N), then draw with N+k instances. Pre-fix this
//! panics. Post-fix the buffer is sized to the full capacity on grow, so
//! the second draw fits.

use pollster::FutureExt as _;
use sonicterm_gpu::quad::{QuadInstance, QuadPipeline};
use wgpu::{
    Color, CommandEncoderDescriptor, DeviceDescriptor, Extent3d, InstanceDescriptor, LoadOp,
    Operations, PowerPreference, RenderPassColorAttachment, RenderPassDescriptor,
    RequestAdapterOptions, StoreOp, TextureDescriptor, TextureDimension, TextureFormat,
    TextureUsages, TextureViewDescriptor,
};

fn make_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&RequestAdapterOptions {
            power_preference: PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        })
        .block_on()
        .ok()?;
    let (device, queue) = adapter.request_device(&DeviceDescriptor::default()).block_on().ok()?;
    Some((device, queue))
}

fn render_once(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &mut QuadPipeline,
    instances: &[QuadInstance],
) {
    let format = TextureFormat::Rgba8UnormSrgb;
    let target = device.create_texture(&TextureDescriptor {
        label: Some("test-color"),
        size: Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format,
        usage: TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&TextureViewDescriptor::default());
    let mut enc = device.create_command_encoder(&CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations { load: LoadOp::Clear(Color::BLACK), store: StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pipeline.draw(device, queue, &mut pass, instances);
    }
    queue.submit(std::iter::once(enc.finish()));
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
}

#[test]
fn quad_pipeline_buffer_grows_without_overrun() {
    let Some((device, queue)) = make_device() else {
        eprintln!("no wgpu adapter — skipping (CI/headless)");
        return;
    };
    let format = TextureFormat::Rgba8UnormSrgb;
    let mut pipeline = QuadPipeline::new(&device, format);

    // Initial capacity is 64. First draw with 81 instances forces a grow.
    // Pre-fix: buffer is sized to exactly 81 * stride (3888 bytes for
    // QuadInstance), capacity field is bumped to 128.
    let small: Vec<QuadInstance> =
        (0..81).map(|_| QuadInstance::sharp([0.0; 4], [1.0, 0.0, 0.0, 1.0])).collect();
    render_once(&device, &queue, &mut pipeline, &small);

    // Second draw with 84 instances. 84 <= 128 (the claimed capacity), so
    // pre-fix this took the write_buffer branch on the still-3888-byte
    // buffer and panicked with the canonical 4032-vs-3888 message.
    let larger: Vec<QuadInstance> =
        (0..84).map(|_| QuadInstance::sharp([0.0; 4], [0.0, 1.0, 0.0, 1.0])).collect();
    render_once(&device, &queue, &mut pipeline, &larger);

    // Walk up through many more grows to make sure each new allocation
    // matches the recorded capacity (catches any future regression that
    // re-introduces a "size to live data only" path).
    for n in [200_usize, 500, 1024, 2048] {
        let v: Vec<QuadInstance> =
            (0..n).map(|_| QuadInstance::sharp([0.0; 4], [0.5; 4])).collect();
        render_once(&device, &queue, &mut pipeline, &v);
        // Now draw with a slightly larger count but still under the
        // next-power-of-two capacity — the regression scenario.
        let v2: Vec<QuadInstance> =
            (0..n + 3).map(|_| QuadInstance::sharp([0.0; 4], [0.5; 4])).collect();
        render_once(&device, &queue, &mut pipeline, &v2);
    }
}
