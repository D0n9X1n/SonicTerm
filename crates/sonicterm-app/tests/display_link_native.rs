//! The native display link on a real `NSView`, run on the process main thread, which AppKit
//! requires; libtest would run it on a worker thread, so this test owns its `main`.
//!
//! It checks the link is created paused with the window's preferred rate, that pausing and
//! unpausing round-trip, that the tick target reads the shared generation when it fires and posts
//! that generation through the event-loop proxy, and that dropping the handle invalidates the link
//! and releases its target. Tick delivery from the display itself is not asserted: a hosted
//! virtual Mac may never fire one.

#![warn(clippy::min_ident_chars)]

#[cfg(target_os = "macos")]
fn main() {
    use sonicterm_app::app::{probe_native_display_link, UserEvent};
    use std::{
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::Duration,
    };
    use winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::{
            macos::{ActivationPolicy, EventLoopBuilderExtMacOS},
            pump_events::EventLoopExtPumpEvents,
        },
        window::WindowId,
    };

    /// Records every display-link tick the event loop delivers.
    #[derive(Default)]
    struct TickRecorder {
        /// Each delivered tick's window and generation, in order.
        ticks: Vec<(WindowId, u64)>,
    }

    impl ApplicationHandler<UserEvent> for TickRecorder {
        fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}

        fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
            if let UserEvent::DisplayLinkTick { window_id, generation, .. } = event {
                self.ticks.push((window_id, generation));
            }
        }
    }

    let mut event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_activation_policy(ActivationPolicy::Prohibited)
        .build()
        .expect("the event loop builds on the main thread");
    let window_id = WindowId::dummy();
    let generation = Arc::new(AtomicU64::new(0));
    let period = Duration::from_micros(16_666);
    let Some(report) =
        probe_native_display_link(event_loop.create_proxy(), window_id, &generation, 7, period)
    else {
        // When: the system predates NSView.displayLink (macOS 14), the window keeps the timer path.
        println!("NOT_EXERCISED: NSView.displayLink needs macOS 14");
        return;
    };
    assert!(report.created_paused, "the link is created paused");
    let rate = 1.0 / period.as_secs_f32();
    for bound in [report.range.0, report.range.1, report.range.2] {
        assert!((bound - rate).abs() < 0.01, "range {:?} is one rate, {rate}", report.range);
    }
    assert!(report.running_after_start, "set_running(true) unpauses");
    assert!(report.paused_after_stop, "set_running(false) pauses");
    assert!(report.target_released_on_drop, "dropping the handle invalidates and releases");
    let mut recorder = TickRecorder::default();
    for _ in 0..50 {
        // The proxy event wakes the loop; a pass with a short timeout delivers it.
        let _ = event_loop.pump_app_events(Some(Duration::from_millis(20)), &mut recorder);
        if !recorder.ticks.is_empty() {
            break;
        }
    }
    // Ordering: Acquire matches the probe's Release store of the generation the target read.
    assert_eq!(generation.load(Ordering::Acquire), 7);
    assert_eq!(recorder.ticks, vec![(window_id, 7)], "the fired target posts the live generation");
    println!("display_link_native: ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("NOT_EXERCISED: the native display link exists only on macOS");
}
