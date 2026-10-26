//! Renderer device policy contracts, sharing one root fixture owner.

use super::*;

/// Software adapters select low reserve while hardware keeps the performance policy.
///
/// Exercising both classification values pins the policy seam before descriptor construction.
#[test]
fn device_memory_policy_selects_usage_for_software_and_performance_for_hardware() {
    assert_eq!(device_memory_policy_from(true), DeviceMemoryPolicy::MemoryUsage);
    assert_eq!(device_memory_policy_from(false), DeviceMemoryPolicy::Performance);
}

/// Device descriptors carry the policy's exact wgpu memory hint.
///
/// Inspecting both descriptors prevents device creation from silently reverting to the default.
#[test]
fn device_descriptor_uses_the_selected_memory_hint() {
    assert!(matches!(
        device_descriptor_for(true, wgpu::Features::empty()).memory_hints,
        wgpu::MemoryHints::MemoryUsage
    ));
    assert!(matches!(
        device_descriptor_for(false, wgpu::Features::empty()).memory_hints,
        wgpu::MemoryHints::Performance
    ));
}

/// Windows requests dual-source blending only when the adapter advertises it.
#[test]
fn optional_device_features_select_dual_source_only_on_supported_windows() {
    let dual = wgpu::Features::DUAL_SOURCE_BLENDING;

    assert_eq!(selected_optional_device_features(dual, true), dual);
    assert!(selected_optional_device_features(wgpu::Features::empty(), true).is_empty());
    assert!(selected_optional_device_features(dual, false).is_empty());
}

/// The descriptor requests exactly the selected optional feature set.
#[test]
fn device_descriptor_carries_selected_optional_features() {
    let dual = wgpu::Features::DUAL_SOURCE_BLENDING;

    assert_eq!(device_descriptor_for(false, dual).required_features, dual);
    assert_eq!(
        device_descriptor_for(true, wgpu::Features::empty()).required_features,
        wgpu::Features::empty()
    );
}

/// Effective LCD mode requires Windows, an opaque target, and a viable presenter path.
#[test]
fn effective_subpixel_aa_policy_falls_back_deterministically() {
    use sonicterm_render_model::boundary::cfg::config::SubpixelAaMode::{Bgr, Off, Rgb};

    assert_eq!(effective_subpixel_aa_mode(Rgb, true, true, false, true), Rgb);
    assert_eq!(effective_subpixel_aa_mode(Bgr, true, true, false, true), Bgr);
    assert_eq!(effective_subpixel_aa_mode(Rgb, true, true, true, false), Rgb);
    assert_eq!(effective_subpixel_aa_mode(Off, true, true, false, true), Off);
    assert_eq!(effective_subpixel_aa_mode(Rgb, false, true, false, true), Off);
    assert_eq!(effective_subpixel_aa_mode(Rgb, true, false, false, true), Off);
    assert_eq!(effective_subpixel_aa_mode(Rgb, true, true, false, false), Off);
}

/// Allocator projection retains totals, counts, and the largest block without labels.
///
/// Distinct totals and uneven block sizes expose field swaps, bad counts, and a wrong maximum.
#[test]
fn allocator_snapshot_maps_report_totals_counts_and_largest_block() {
    let report = wgpu::AllocatorReport {
        allocations: vec![
            wgpu::wgt::AllocationReport {
                name: String::from("must-not-be-read"),
                offset: 0,
                size: 19,
            },
            wgpu::wgt::AllocationReport {
                name: String::from("also-must-not-be-read"),
                offset: 64,
                size: 23,
            },
        ],
        blocks: vec![
            wgpu::wgt::MemoryBlockReport { size: 128, allocations: 0..1 },
            wgpu::wgt::MemoryBlockReport { size: 512, allocations: 1..2 },
            wgpu::wgt::MemoryBlockReport { size: 256, allocations: 2..2 },
        ],
        total_allocated_bytes: 42,
        total_reserved_bytes: 896,
    };

    assert_eq!(
        allocator_snapshot_from(&report),
        AllocatorSnapshot {
            allocated_bytes: 42,
            reserved_bytes: 896,
            allocations: 2,
            blocks: 3,
            largest_block_bytes: 512,
        }
    );
}

/// An unavailable backend report remains absence rather than a zero-valued snapshot.
///
/// Passing `None` through the production adapter preserves capability state for callers.
#[test]
fn allocator_snapshot_preserves_an_unavailable_report_as_none() {
    assert_eq!(allocator_snapshot_from_report(None), None);
}

#[test]
fn detects_cpu_device_type_as_software() {
    // Even a "GPU-sounding" name is software if the device type is CPU.
    assert!(software_rendering_from("Some Virtual GPU", wgpu::DeviceType::Cpu));
}

#[test]
fn detects_known_software_rasterizers_by_name() {
    assert!(software_rendering_from(
        "Microsoft Basic Render Driver",
        wgpu::DeviceType::DiscreteGpu
    ));
    assert!(software_rendering_from("llvmpipe (LLVM 15.0.7, 256 bits)", wgpu::DeviceType::Other));
    assert!(software_rendering_from("Google SwiftShader", wgpu::DeviceType::Other));
}

#[test]
fn does_not_flag_real_gpus() {
    assert!(!software_rendering_from("NVIDIA GeForce RTX 4090", wgpu::DeviceType::DiscreteGpu));
    assert!(!software_rendering_from("Apple M3 Max", wgpu::DeviceType::IntegratedGpu));
    assert!(!software_rendering_from("Intel(R) Iris(R) Xe", wgpu::DeviceType::IntegratedGpu));
}
