//! What does the production software-adapter predicate say about this host?
//!
//! #990 asks whether `Auto` detects a software rasterizer and degrades. Its
//! premise is that `Auto` is "the branch CI cannot exercise", because it
//! depends on `detected`, which needs a real software rasterizer present.
//!
//! That premise was assumed, never checked. GitHub's `windows-latest` image
//! has no discrete GPU, so the adapter it presents is plausibly WARP — in
//! which case `detected` is true there and `Auto` is exercisable after all.
//!
//! This enumerates the host's real adapters and runs the real production
//! predicate over each, printing what it finds. It does not reimplement the
//! classification: a probe that decides for itself what counts as software
//! would agree with the shipping code only by coincidence.
//!
//! **It asserts that it reached a conclusion, not what the conclusion is.**
//! A host with a hardware GPU is a fact about the host, not a defect. What
//! would be a defect is this test passing without having enumerated anything,
//! so that is what the assertion guards.

use sonicterm_gpu::core::detect_software_rendering;

/// Enumerate every adapter this host offers and classify each one.
#[test]
fn report_how_this_host_classifies_its_adapters() {
    let os = std::env::consts::OS;
    let ci = std::env::var("CI").is_ok();
    println!("host: os={os} ci={ci}");

    // `_from_env` so the probe honours `WGPU_BACKEND` the way the renderer
    // does; `without_display_handle` because enumeration needs no surface and
    // this must work on a headless runner.
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    // `pollster::block_on` for the same reason the renderer's own constructor
    // uses it: enumeration is async in wgpu 30 and there is no runtime here.
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));

    println!("adapters: count={}", adapters.len());

    let mut any_software = false;
    for adapter in &adapters {
        let info = adapter.get_info();
        // The production predicate, not a local reimplementation of it.
        let software = detect_software_rendering(&info);
        any_software |= software;
        println!(
            "adapter: name={:?} backend={:?} device_type={:?} detect_software_rendering={}",
            info.name, info.backend, info.device_type, software
        );
    }

    println!("verdict: any_software_adapter={any_software}");
    if any_software {
        println!(
            "verdict: Auto is exercisable on this host — `detected` is true, so \
             should_degrade_for_software_render(Auto, detected) engages the degrade path"
        );
    } else {
        println!(
            "verdict: Auto degrades to false on this host — every adapter classifies as \
             hardware, so the Auto-detects-software case needs a software-rasterizer host"
        );
    }

    // The conclusion is whatever it is; having reached one is the requirement.
    // An empty adapter list means the enumeration itself failed to tell us
    // anything, which is the one outcome that would make this test theatre.
    assert!(
        !adapters.is_empty(),
        "no adapters enumerated: this probe learned nothing about the host, which is the \
         one result that makes it worthless"
    );
}
