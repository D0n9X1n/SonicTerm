//! Does software-adapter detection agree with the adapter the host actually has?
//!
//! `detect_software_rendering` decides whether SonicTerm engages the no-GPU
//! degrade path: the 40 fps frame cap, `Fifo` present mode, opaque alpha, and
//! suppressed scrollbar animation. Every existing test for it passes a
//! hand-written name and device type — `"Microsoft Basic Render Driver"`,
//! `"llvmpipe (LLVM 15.0.7, 256 bits)"`, `DeviceType::Cpu`. Those strings were
//! chosen by whoever wrote the test.
//!
//! That is the shape of defect this milestone kept finding: a claim checked
//! against the thing it was derived from. What was never checked is whether the
//! predicate agrees with a *real* `AdapterInfo` from a *real* wgpu enumeration,
//! and the reason is that the production request passes
//! `compatible_surface: Some(&surface)`, which needs a window.
//!
//! It does not have to. `RequestAdapterOptions::compatible_surface` accepts
//! `None`, so an adapter can be enumerated headlessly — on any host, in CI,
//! including the Windows runner, which has no GPU and therefore exercises the
//! software path this decision exists for.
//!
//! **What this asserts is agreement, not a verdict.** Asserting "the adapter is
//! software" would fail on a developer's machine and pass on CI for the wrong
//! reason. Asserting that the classification matches what the adapter says
//! about itself is true on both, and is the property that actually matters.

use sonicterm_gpu::core::detect_software_rendering;

/// Enumerate the host's real adapter, headlessly.
///
/// `None` for `compatible_surface`: the production path passes a surface
/// because it has one, not because the adapter query needs it.
fn host_adapter_info() -> Option<wgpu::AdapterInfo> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .ok()?;
    Some(adapter.get_info())
}

/// The predicate agrees with the adapter the host reports.
///
/// Prints what it found, because that is the evidence: a CI log showing
/// `Microsoft Basic Render Driver / Cpu / software=true` is the verification
/// that the Windows software path is reachable, and no assertion can carry
/// that as well as the name itself.
#[test]
fn detection_agrees_with_the_hosts_real_adapter() {
    let Some(info) = host_adapter_info() else {
        // No adapter at all is a legitimate CI configuration and not a
        // detection defect. Say so rather than passing silently, because a
        // silent skip and a real pass look identical in a summary line.
        println!("no wgpu adapter on this host; detection not exercised");
        return;
    };

    let detected = detect_software_rendering(&info);
    println!(
        "adapter: name={:?} backend={:?} device_type={:?} driver={:?} -> software={}",
        info.name, info.backend, info.device_type, info.driver, detected
    );

    // A CPU device type is software by definition, whatever its name.
    if info.device_type == wgpu::DeviceType::Cpu {
        assert!(
            detected,
            "adapter {:?} reports DeviceType::Cpu but detection says hardware; \
             the degrade path would never engage on a host with no GPU",
            info.name
        );
        return;
    }

    // A discrete or integrated GPU is hardware unless its name is one of the
    // known software rasterizers shipped under a non-Cpu device type — which
    // is exactly why the name check exists alongside the device type.
    let name = info.name.to_ascii_lowercase();
    let named_software = name.contains("microsoft basic render driver")
        || name.contains("llvmpipe")
        || name.contains("swiftshader")
        || name.contains("software adapter");

    assert_eq!(
        detected,
        named_software || info.device_type == wgpu::DeviceType::Cpu,
        "detection disagrees with the adapter's own report: name={:?} device_type={:?}",
        info.name,
        info.device_type
    );

    if matches!(info.device_type, wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu)
        && !named_software
    {
        assert!(
            !detected,
            "adapter {:?} is a {:?} and is not a known software rasterizer, but detection \
             says software; the frame cap would engage on a machine with a working GPU",
            info.name, info.device_type
        );
    }
}

/// The fallback adapter is classified as software.
///
/// `force_fallback_adapter: true` asks wgpu for its software rasterizer
/// specifically. If one exists on this host and detection calls it hardware,
/// the degrade path is broken for exactly the configuration it was built for —
/// and that is not reachable through the test above, which asks for
/// `HighPerformance` and gets whatever the host prefers.
#[test]
fn the_fallback_adapter_is_recognised_as_software() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: true,
        apply_limit_buckets: false,
    })) else {
        println!("no fallback adapter on this host; software path not exercised");
        return;
    };

    let info = adapter.get_info();
    println!(
        "fallback adapter: name={:?} backend={:?} device_type={:?} -> software={}",
        info.name,
        info.backend,
        info.device_type,
        detect_software_rendering(&info)
    );

    assert!(
        detect_software_rendering(&info),
        "wgpu's own fallback adapter ({:?}, {:?}) is not recognised as software; \
         the degrade path cannot engage on the configuration it exists for",
        info.name,
        info.device_type
    );
}
