#![cfg(target_os = "windows")]

use sonicterm_gpu::core::{
    allocator_snapshot_from, detect_software_rendering, device_descriptor_for,
    device_memory_policy_from, DeviceMemoryPolicy,
};

const FOUR_MIB: u64 = 4 * 1024 * 1024;
const CANDIDATE_RESERVED_LIMIT: u64 = 64 * 1024 * 1024;
const CANDIDATE_LARGEST_BLOCK_LIMIT: u64 = 128 * 1024 * 1024;

struct RepresentativeAllocations {
    _vertex: wgpu::Buffer,
    _sampled: wgpu::Texture,
    _render_target: wgpu::Texture,
}

fn representative_allocations(device: &wgpu::Device) -> RepresentativeAllocations {
    let vertex = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("allocator baseline vertex"),
        size: FOUR_MIB,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let sampled = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("allocator baseline sampled texture"),
        size: wgpu::Extent3d { width: 1024, height: 1024, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let render_target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("allocator baseline render target"),
        size: wgpu::Extent3d { width: 1024, height: 1024, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    RepresentativeAllocations { _vertex: vertex, _sampled: sampled, _render_target: render_target }
}

#[test]
fn warp_memory_usage_policy_reduces_allocator_reserve() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
        apply_limit_buckets: false,
    }))
    .expect("Windows WARP must be available as the deterministic fallback adapter");
    let info = adapter.get_info();
    eprintln!("adapter={}", info.name);
    assert_eq!(info.backend, wgpu::Backend::Dx12, "fallback adapter must use DX12");
    assert_eq!(info.device_type, wgpu::DeviceType::Cpu, "fallback adapter must be WARP CPU");
    assert!(detect_software_rendering(&info), "WARP adapter must classify as software");

    let (control_device, _control_queue) =
        pollster::block_on(adapter.request_device(&device_descriptor_for(false)))
            .expect("open default-policy control device");
    assert_eq!(
        device_memory_policy_from(false),
        DeviceMemoryPolicy::Performance,
        "control helper input must preserve wgpu's default memory policy"
    );
    let (candidate_device, _candidate_queue) =
        pollster::block_on(adapter.request_device(&device_descriptor_for(true)))
            .expect("open production-policy candidate device");

    let _control_allocations = representative_allocations(&control_device);
    let _candidate_allocations = representative_allocations(&candidate_device);
    control_device.poll(wgpu::PollType::wait_indefinitely()).expect("poll control device");
    candidate_device.poll(wgpu::PollType::wait_indefinitely()).expect("poll candidate device");

    let control = allocator_snapshot_from(
        &control_device.generate_allocator_report().expect("control allocator report unavailable"),
    );
    let candidate = allocator_snapshot_from(
        &candidate_device
            .generate_allocator_report()
            .expect("candidate allocator report unavailable"),
    );

    assert!(
        candidate.reserved_bytes < CANDIDATE_RESERVED_LIMIT,
        "candidate reserved {} bytes, expected less than {}",
        candidate.reserved_bytes,
        CANDIDATE_RESERVED_LIMIT
    );
    assert!(
        candidate.largest_block_bytes < CANDIDATE_LARGEST_BLOCK_LIMIT,
        "candidate largest block {} bytes, expected less than {}",
        candidate.largest_block_bytes,
        CANDIDATE_LARGEST_BLOCK_LIMIT
    );
    assert!(
        candidate.reserved_bytes < control.reserved_bytes,
        "candidate reserve {} must be lower than control reserve {}",
        candidate.reserved_bytes,
        control.reserved_bytes
    );
    println!(
        "capability=EXERCISED control_reserved_bytes={} control_largest_block_bytes={} \
         candidate_reserved_bytes={} candidate_largest_block_bytes={}",
        control.reserved_bytes,
        control.largest_block_bytes,
        candidate.reserved_bytes,
        candidate.largest_block_bytes
    );
}
