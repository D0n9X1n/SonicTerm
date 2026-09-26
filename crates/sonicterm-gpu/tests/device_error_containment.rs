//! Headless device-error containment on the host's native backend: Metal on
//! macOS, WARP on Windows, and Vulkan (lavapipe in CI) on Linux.
//!
//! A host without an adapter fails these tests rather than skipping them.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sonicterm_gpu::device_errors::{
    destroy_and_await_loss, install_device_error_handlers, run_isolated_validation,
    DeviceErrorKind, DeviceErrorState, DeviceState,
};

struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    state: Arc<DeviceErrorState>,
    wakes: Arc<AtomicUsize>,
}

fn harness() -> Harness {
    #[cfg(target_os = "windows")]
    let (descriptor, force_fallback_adapter) = (
        wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        },
        true,
    );
    #[cfg(not(target_os = "windows"))]
    let (descriptor, force_fallback_adapter) =
        (wgpu::InstanceDescriptor::new_without_display_handle(), false);
    let instance = wgpu::Instance::new(descriptor);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter,
        apply_limit_buckets: false,
    }))
    .expect("native test adapter: containment is verified on every desktop host");
    let software = adapter.get_info().device_type == wgpu::DeviceType::Cpu;
    let descriptor = sonicterm_gpu::core::device_descriptor_for(software, wgpu::Features::empty());
    let (device, queue) =
        pollster::block_on(adapter.request_device(&descriptor)).expect("native test device");
    let state = install_device_error_handlers(&device);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    assert!(state.set_waker(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));
    Harness { device, queue, state, wakes }
}

fn invalid_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("containment-invalid-buffer"),
        size: 4,
        usage: wgpu::BufferUsages::empty(),
        mapped_at_creation: false,
    })
}

/// An unscoped invalid texture reaches the installed handler instead of
/// wgpu's panicking default, stops the device, and wakes the app once.
#[test]
fn unscoped_invalid_texture_stops_the_device() {
    let h = harness();
    let _texture = h.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("containment-zero-texture"),
        size: wgpu::Extent3d { width: 0, height: 0, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let snapshot = h.state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Unusable);
    assert!(snapshot.counts.validation >= 1);
    assert_eq!(snapshot.records_logged, 1);
    assert_eq!(h.wakes.load(Ordering::SeqCst), 1);
    assert!(h.state.enter_gpu_work("after").is_none());
}

/// A submission that references a destroyed buffer fails validation at submit
/// time, and the handler stops the device.
#[test]
fn unscoped_invalid_submit_stops_the_device() {
    let h = harness();
    let buffer = h.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("containment-destroyed-buffer"),
        size: 16,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = h.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.clear_buffer(&buffer, 0, None);
    let commands = encoder.finish();
    assert_eq!(h.state.state(), DeviceState::Usable, "recording the clear is valid");
    buffer.destroy();
    h.queue.submit(Some(commands));
    let snapshot = h.state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Unusable);
    assert_eq!(snapshot.unusable.map(|record| record.kind), Some(DeviceErrorKind::Validation));
    assert_eq!(h.wakes.load(Ordering::SeqCst), 1);
}

/// An error inside the isolated-operation scope is recorded as isolated and
/// leaves the device usable, without waking the app.
#[test]
fn isolated_error_leaves_the_device_usable() {
    let h = harness();
    assert!(run_isolated_validation(&h.device, &h.state, || {
        let _buffer = invalid_buffer(&h.device);
    }));
    let snapshot = h.state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Usable);
    assert_eq!(snapshot.counts.isolated, 1);
    assert_eq!(snapshot.counts.validation, 0);
    assert_eq!(h.wakes.load(Ordering::SeqCst), 0);
    assert!(h.state.accepts_gpu_work());
}

/// An intentional destroy followed by the hook's bounded poll records the loss
/// once through the lost callback, without a panic.
#[test]
fn destroy_then_poll_records_lost_once() {
    let h = harness();
    destroy_and_await_loss(&h.device, &h.state);
    let snapshot = h.state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Lost);
    assert!(snapshot.destroy_requested);
    assert_eq!(snapshot.counts.lost, 1);
    assert_eq!(snapshot.lost.map(|record| record.description), Some("Destroyed".to_owned()));
    assert_eq!(h.wakes.load(Ordering::SeqCst), 1);
    assert!(h.state.enter_gpu_work("after").is_none());
}

/// A burst of 1000 isolated errors produces one record, a count of 1000, and
/// no wake.
#[test]
fn isolated_burst_is_coalesced() {
    let h = harness();
    for _ in 0..1000 {
        run_isolated_validation(&h.device, &h.state, || {
            let _buffer = invalid_buffer(&h.device);
        });
    }
    let snapshot = h.state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Usable);
    assert_eq!(snapshot.counts.isolated, 1000);
    assert_eq!(snapshot.records_logged, 1);
    assert_eq!(h.wakes.load(Ordering::SeqCst), 0);
}

/// A burst of 1000 unscoped errors produces one transition and one wake, with
/// every error counted.
#[test]
fn unscoped_burst_is_coalesced() {
    let h = harness();
    for _ in 0..1000 {
        let _buffer = invalid_buffer(&h.device);
    }
    let snapshot = h.state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Unusable);
    assert_eq!(snapshot.counts.validation, 1000);
    assert_eq!(snapshot.records_logged, 1);
    assert_eq!(h.wakes.load(Ordering::SeqCst), 1);
}
