//! Device policy responsibilities of the renderer, preserving its single owner.

use super::*;

/// The display name of graphics API `backend`, as the About window shows it.
#[must_use]
pub fn backend_display_name(backend: wgpu::Backend) -> &'static str {
    match backend {
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Dx12 => "DirectX 12",
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Gl => "OpenGL",
        wgpu::Backend::BrowserWebGpu => "WebGPU",
        wgpu::Backend::Noop => "none",
    }
}

/// Classify a wgpu adapter as a software (CPU) rasterizer.
///
/// True when the adapter is a CPU device, or its name matches a known
/// software rasterizer (Microsoft WARP, Mesa llvmpipe, Google SwiftShader).
/// Used to drive the no-GPU degrade path. Pure fn over the
/// adapter info so it is unit-testable without a live GPU.
#[must_use]
pub fn detect_software_rendering(info: &wgpu::AdapterInfo) -> bool {
    software_rendering_from(&info.name, info.device_type)
}

/// Inner predicate over just the adapter name + device type, so it can be
/// unit-tested without building a full `wgpu::AdapterInfo` (which has no
/// `Default`).
#[must_use]
pub(super) fn software_rendering_from(name: &str, device_type: wgpu::DeviceType) -> bool {
    if device_type == wgpu::DeviceType::Cpu {
        // When: `device_type` is `Cpu` — authoritative, so no name match is
        // needed. The string tests below cover rasterizers reporting otherwise.
        return true;
    }
    let name = name.to_ascii_lowercase();
    name.contains("microsoft basic render driver")
        || name.contains("llvmpipe")
        || name.contains("swiftshader")
        || name.contains("software adapter")
}

pub(super) fn software_render_degrade_from(mode: SoftwareRenderMode, detected: bool) -> bool {
    match mode {
        SoftwareRenderMode::Auto => detected,
        SoftwareRenderMode::Force => true,
        SoftwareRenderMode::Off => false,
    }
}

/// Memory-allocation strategy selected for a wgpu device.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMemoryPolicy {
    /// Favor lower allocator reserve on software adapters.
    MemoryUsage,
    /// Favor rendering performance on hardware adapters.
    Performance,
}

/// Select the device memory policy for an adapter classification.
///
/// Software adapters minimize allocator reserve; hardware adapters retain
/// wgpu's performance-oriented policy.
#[doc(hidden)]
#[must_use]
pub fn device_memory_policy_from(software_rendering: bool) -> DeviceMemoryPolicy {
    match software_rendering {
        true => DeviceMemoryPolicy::MemoryUsage,
        false => DeviceMemoryPolicy::Performance,
    }
}

/// Select optional device features that SonicTerm can use without losing fallback support.
#[doc(hidden)]
#[must_use]
pub fn selected_optional_device_features(
    adapter_features: wgpu::Features,
    windows_host: bool,
) -> wgpu::Features {
    if windows_host {
        adapter_features & wgpu::Features::DUAL_SOURCE_BLENDING
    } else {
        // When: `windows_host` is false, LCD presentation stays disabled and requests no feature.
        wgpu::Features::empty()
    }
}

/// Resolve the requested LCD mode against platform, target opacity, and presenter capability.
#[doc(hidden)]
#[must_use]
pub const fn effective_subpixel_aa_mode(
    requested: SubpixelAaMode,
    windows_host: bool,
    opaque_target: bool,
    software_presenter: bool,
    dual_source_supported: bool,
) -> SubpixelAaMode {
    if !windows_host || !opaque_target {
        // When: `windows_host` or `opaque_target` is false, LCD coverage cannot reach an opaque Windows target.
        return SubpixelAaMode::Off;
    }
    if software_presenter || dual_source_supported {
        requested
    } else {
        // When: both `software_presenter` and `dual_source_supported` are false, use grayscale destination blending.
        SubpixelAaMode::Off
    }
}

/// Build the sole wgpu device descriptor used by the renderer.
///
/// The caller selects only features advertised by the adapter, while memory
/// policy remains independently derived from software-render classification.
#[doc(hidden)]
#[must_use]
pub fn device_descriptor_for(
    software_rendering: bool,
    required_features: wgpu::Features,
) -> DeviceDescriptor<'static> {
    let memory_hints = match device_memory_policy_from(software_rendering) {
        DeviceMemoryPolicy::MemoryUsage => wgpu::MemoryHints::MemoryUsage,
        DeviceMemoryPolicy::Performance => wgpu::MemoryHints::Performance,
    };
    DeviceDescriptor { memory_hints, required_features, ..DeviceDescriptor::default() }
}

/// Aggregate allocator usage without retaining allocation labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocatorSnapshot {
    /// Bytes occupied by live GPU allocations.
    pub allocated_bytes: u64,
    /// Bytes reserved by GPU memory blocks.
    pub reserved_bytes: u64,
    /// Number of live GPU allocations.
    pub allocations: u32,
    /// Number of reserved GPU memory blocks.
    pub blocks: u32,
    /// Size of the largest reserved GPU memory block.
    pub largest_block_bytes: u64,
}

/// Summarize a wgpu allocator report without reading allocation names.
#[doc(hidden)]
#[must_use]
pub fn allocator_snapshot_from(report: &wgpu::AllocatorReport) -> AllocatorSnapshot {
    AllocatorSnapshot {
        allocated_bytes: report.total_allocated_bytes,
        reserved_bytes: report.total_reserved_bytes,
        allocations: u32::try_from(report.allocations.len()).unwrap_or(u32::MAX),
        blocks: u32::try_from(report.blocks.len()).unwrap_or(u32::MAX),
        largest_block_bytes: report.blocks.iter().map(|block| block.size).max().unwrap_or(0),
    }
}

/// Preserve report unavailability while projecting an available report to scalar counters.
pub(super) fn allocator_snapshot_from_report(
    report: Option<wgpu::AllocatorReport>,
) -> Option<AllocatorSnapshot> {
    report.as_ref().map(allocator_snapshot_from)
}

#[cfg(test)]
#[path = "core_device_policy_tests.rs"]
mod core_device_policy_tests;
