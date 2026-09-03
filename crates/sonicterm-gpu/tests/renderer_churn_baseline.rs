//! Does renderer memory return to baseline across window churn?
//!
//! Opening and closing windows on the software path must leave the process
//! total where it started. A per-window buffer that is never released shows as
//! a staircase.
//!
//! **What this covers, and what it does not.**
//!
//! It creates and drops real `GpuRenderer`s against a real window, and checks
//! that the process-wide live-renderer count returns to where it started. That
//! is the reading a leak moves: a renderer's own `retained_amounts()` reports
//! the instance being asked, and every instance reports the same atlas
//! capacity, so comparing those across cycles compares a constant to itself
//! and holds whether or not anything leaked. The per-cycle retention readings
//! are still taken and printed, because a change in them would mean the atlas
//! sizing itself moved — but the count is what fails on a leak.
//!
//! It does **not** exercise the software frame. `WindowsSoftwareFrame` is
//! allocated lazily inside `render()`, which takes thirteen arguments
//! including `&mut [PaneRender]` and a `TabBar`; `set_software_render_degrade`
//! only reconfigures the surface and releases any frame already present. A
//! harness built on the toggle alone would report success having freed
//! nothing, which is worse than no harness. The frame half needs a render
//! harness and is tracked separately.
//!
//! Runs where a window can be created. macOS is not attempted: winit panics
//! when an `EventLoop` is built off the main thread, and libtest runs every
//! test on a spawned thread.

#![cfg(target_os = "windows")]

use std::sync::Arc;

use sonicterm_gpu::core::{live_renderer_count, GpuRenderer, RendererSettings, SurfaceAppearance};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::windows::EventLoopBuilderExtWindows;

/// How many open/close cycles. A single leaked renderer is one glyph atlas,
/// so eight cycles put ~128 MiB between a clean run and a leaking one — far
/// outside any noise the measurement could carry.
const CYCLES: usize = 8;

struct Churn {
    readings: Vec<(usize, usize, usize)>,
    /// Live renderers observed after each cycle's renderer was dropped.
    live_after_drop: Vec<usize>,
    /// Live renderers before the first cycle, so the comparison does not
    /// assume the process started at zero.
    live_baseline: usize,
    failure: Option<String>,
}

impl ApplicationHandler for Churn {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        let theme = sonicterm_render_model::boundary::cfg::theme::Theme::default();

        self.live_baseline = live_renderer_count();

        for cycle in 0..CYCLES {
            let attrs = winit::window::Window::default_attributes()
                .with_visible(false)
                .with_title("sonicterm churn probe");
            let window = match active.create_window(attrs) {
                Ok(w) => Arc::new(w),
                Err(err) => {
                    self.failure = Some(format!("cycle {cycle}: window: {err}"));
                    break;
                }
            };

            let settings = RendererSettings {
                font_family: "monospace",
                font_dirs: &[],
                font_size: 14.0,
                line_height_mult: 1.2,
                font_weight_scale: 1.0,
                subpixel_aa: Default::default(),
                padding: [0.0; 4],
                appearance: SurfaceAppearance {
                    backdrop: Default::default(),
                    opacity: 1.0,
                    scrollbar: Default::default(),
                    panel_padding: 0.0,
                    software_render_mode: Default::default(),
                },
                role: "churn-probe",
            };

            match GpuRenderer::new(window.clone(), active, &theme, settings) {
                Ok(mut renderer) => {
                    // Engage the degrade path: the software configuration is
                    // the one this measures.
                    renderer.set_software_render_degrade(true);
                    let held = renderer.retained_amounts();
                    self.readings.push((
                        held.glyph_atlas.bytes,
                        held.image_atlas.bytes,
                        held.software_frame.bytes,
                    ));
                    // Renderer and window both drop here.
                    drop(renderer);
                    // Read after the drop, not before: this is the reading a
                    // leaked renderer moves.
                    self.live_after_drop.push(live_renderer_count());
                }
                Err(err) => {
                    self.failure = Some(format!("cycle {cycle}: renderer: {err}"));
                    break;
                }
            }
        }

        active.exit();
    }

    fn window_event(
        &mut self,
        _: &ActiveEventLoop,
        _: winit::window::WindowId,
        _: winit::event::WindowEvent,
    ) {
    }
}

/// Renderers do not accumulate across open/close cycles.
///
/// The load-bearing assertion is on the live-renderer count, which returns to
/// its starting value only if every renderer built was also dropped. The
/// retention readings are reported alongside it: they are per-instance
/// constants, so they detect a change in atlas sizing, not a leak.
#[test]
fn renderer_memory_returns_to_baseline_across_window_churn() {
    let event_loop = match EventLoop::builder().with_any_thread(true).build() {
        Ok(el) => el,
        Err(err) => {
            // A host without a display is a fact about the host. Reported
            // rather than failed, and the probe in ci_host_capability_probe
            // is what establishes whether this should have been possible.
            println!("event loop unavailable ({err}); churn not exercised");
            return;
        }
    };

    let mut churn = Churn {
        readings: Vec::new(),
        live_after_drop: Vec::new(),
        live_baseline: 0,
        failure: None,
    };
    event_loop.run_app(&mut churn).expect("event loop runs");

    if let Some(failure) = churn.failure {
        // A renderer that cannot be built on this host is not a leak. Say so
        // plainly instead of asserting on an empty reading set.
        println!("churn stopped early: {failure}");
        println!("readings taken: {}", churn.readings.len());
    }

    assert!(
        churn.readings.len() >= 2,
        "fewer than two cycles completed, so nothing was compared; a pass here would \
         mean the harness did not run rather than that memory is stable"
    );

    let (first_glyph, first_image, first_frame) = churn.readings[0];
    let (last_glyph, last_image, last_frame) = *churn.readings.last().expect("non-empty");

    println!(
        "cycles={} first=(glyph {first_glyph}, image {first_image}, frame {first_frame}) \
         last=(glyph {last_glyph}, image {last_image}, frame {last_frame})",
        churn.readings.len()
    );
    println!(
        "live renderers: baseline {} after-each-drop {:?}",
        churn.live_baseline, churn.live_after_drop
    );

    // The one that fails on a leak. Every cycle must return the count to the
    // baseline, not merely end there: a run that leaked one renderer and freed
    // an unrelated one would balance out in a first-vs-last comparison.
    for (cycle, live) in churn.live_after_drop.iter().enumerate() {
        assert_eq!(
            *live, churn.live_baseline,
            "after cycle {cycle} the process held {live} live renderers against a \
             baseline of {}; a renderer built in this loop was not dropped, which is \
             the staircase this exists to catch",
            churn.live_baseline
        );
    }

    assert_eq!(
        churn.live_after_drop.len(),
        churn.readings.len(),
        "every cycle that produced a retention reading must also have produced a \
         live-renderer reading; unequal counts mean the leak check skipped a cycle"
    );

    assert_eq!(
        (last_glyph, last_image, last_frame),
        (first_glyph, first_image, first_frame),
        "per-renderer retention changed across {} cycles; this is a constant by \
         construction, so a change means atlas sizing moved between instances",
        churn.readings.len()
    );
}
