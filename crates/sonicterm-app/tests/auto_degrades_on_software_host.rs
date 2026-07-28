//! Does `Auto` degrade on a host that really is a software rasterizer?
//!
//! The unit tests around the degrade decision supply `detected` by hand, so
//! they pin the truth table without ever running detection. The gap that
//! leaves is the wiring: real adapter -> `detect_software_rendering` ->
//! `should_degrade_for_software_render`. A detection call that was never
//! reached, or reached with a hardcoded argument, satisfies every hand-fed
//! test while shipping a renderer that never degrades.
//!
//! Windows CI closes that gap. The runner presents "Microsoft Basic Render
//! Driver" with `device_type=Cpu`, so `detected` is genuinely true there and
//! `Auto` — the mode almost every user runs — is exercisable end to end.
//!
//! Gated to Windows because it asserts a *conclusion*, not merely that one
//! was reached: on a macOS host every adapter classifies as hardware and
//! `Auto` correctly resolves to false, which would fail an assertion written
//! for a software host. `sonicterm-gpu`'s `ci_adapter_classification_probe`
//! is the unasserted, every-host half that reports what each runner offers.

#![cfg(target_os = "windows")]

use sonicterm_app::app::should_degrade_for_software_render;
use sonicterm_cfg::config::SoftwareRenderMode;
use sonicterm_gpu::core::detect_software_rendering;

/// Enumerate the host's real adapters and classify them with the production
/// predicate. Returns whether any is a software rasterizer.
///
/// Uses `detect_software_rendering` rather than re-deciding what "software"
/// means: a test with its own classifier agrees with the shipping one only by
/// coincidence, and stops agreeing the moment the shipping one changes.
fn host_has_software_adapter() -> bool {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    assert!(
        !adapters.is_empty(),
        "no adapters enumerated: detection cannot be exercised against nothing"
    );
    adapters.iter().any(|adapter| {
        let info = adapter.get_info();
        let software = detect_software_rendering(&info);
        println!(
            "adapter: name={:?} backend={:?} device_type={:?} detect_software_rendering={}",
            info.name, info.backend, info.device_type, software
        );
        software
    })
}

/// `Auto` degrades on a real software-rasterizer host, without being told to.
///
/// This is the criterion the hand-fed tests cannot reach: `detected` here is
/// produced by running the shipping predicate over an adapter the host really
/// offers, rather than supplied as a literal.
#[test]
fn auto_degrades_on_a_real_software_rasterizer_host() {
    let detected = host_has_software_adapter();
    assert!(
        detected,
        "expected this host to present a software adapter (Microsoft Basic Render Driver, \
         device_type=Cpu). A host with a real GPU means this test is running somewhere it was \
         not designed for, rather than reporting a defect."
    );

    assert!(
        should_degrade_for_software_render(SoftwareRenderMode::Auto, detected),
        "Auto must degrade when detection finds a software rasterizer"
    );
    println!("verdict: Auto degraded on a real software adapter");
}

/// `Off` does not degrade even though this host's adapter *is* software.
///
/// The user asked for hardware; they get hardware or a clear failure, never a
/// silent downgrade. Distinct from the hand-fed truth table in that `detected`
/// is real here, so it also catches an `Off` path that ignores the configured
/// mode and follows detection instead.
#[test]
fn off_does_not_degrade_even_on_a_software_host() {
    let detected = host_has_software_adapter();
    assert!(detected, "precondition: this host must be a software rasterizer");

    assert!(
        !should_degrade_for_software_render(SoftwareRenderMode::Off, detected),
        "Off must not degrade, even when the adapter really is software"
    );
    println!("verdict: Off held hardware on a real software adapter");
}

/// `Force` degrades regardless of what detection says.
///
/// On this host detection agrees, so what this pins is that `Force` does not
/// become dependent on detection: it must degrade on the same real input that
/// `Off` refuses to degrade on, and on a hardware reading too.
#[test]
fn force_degrades_independently_of_detection() {
    let detected = host_has_software_adapter();

    assert!(
        should_degrade_for_software_render(SoftwareRenderMode::Force, detected),
        "Force must degrade"
    );
    assert!(
        should_degrade_for_software_render(SoftwareRenderMode::Force, false),
        "Force must degrade even when detection says hardware"
    );
    println!("verdict: Force degraded independently of detection");
}
