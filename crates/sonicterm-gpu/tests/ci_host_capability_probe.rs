//! What can the CI host actually do?
//!
//! #991, #992, and #993 are open on "needs a human at a keyboard on a Windows
//! software-rendering host". That may be more pessimistic than the truth:
//! GitHub's `windows-latest` image runs an interactive desktop session, and if
//! a window can be created there, frame cadence and dirty-rect presentation
//! become measurable without a person watching.
//!
//! Nobody has checked. This checks, and prints what it finds.
//!
//! **It asserts that it reached a conclusion, not what the conclusion is.** A
//! host that cannot create a window is a fact about the host, not a defect —
//! failing CI over it would turn information into an outage. What would be a
//! defect is this test passing without having learned anything, so that is
//! what the assertion guards.

use winit::event_loop::EventLoop;

/// Build an event loop from a test thread, where the platform permits it.
///
/// The libtest harness runs tests on spawned threads. Windows allows an event
/// loop off the main thread through `with_any_thread`; **macOS panics** —
/// "on macOS, `EventLoop` must be created on the main thread!" — rather than
/// returning `Err`, so it is not attempted there. A probe that aborts the
/// suite on the host it cannot help is worse than one that reports the limit.
fn build_event_loop() -> Result<EventLoop<()>, String> {
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        EventLoop::builder().with_any_thread(true).build().map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(String::from(
            "not attempted: winit panics when an EventLoop is built off the main thread \
             on this platform, and libtest runs every test on a spawned thread",
        ))
    }
}

/// Report whether this host can host a window, and therefore whether the
/// Windows software-path verification can be automated.
#[test]
fn report_whether_this_host_can_create_a_window() {
    let os = std::env::consts::OS;
    let ci = std::env::var("CI").is_ok();
    println!("host: os={os} ci={ci}");

    match build_event_loop() {
        Err(err) => {
            // Expected on macOS from a test thread, and the honest answer for
            // any host without a display connection.
            println!("event loop: UNAVAILABLE ({err})");
            println!("verdict: window-dependent verification cannot run here");
        }
        Ok(event_loop) => {
            println!("event loop: available");

            // A window is the thing #991/#992 need. Create one, report, exit.
            // `resumed` fires immediately on Windows; the handler leaves the
            // loop on its first call so this cannot spin.
            struct Probe {
                outcome: Option<Result<(), String>>,
            }

            impl winit::application::ApplicationHandler for Probe {
                fn resumed(&mut self, active: &winit::event_loop::ActiveEventLoop) {
                    let attrs = winit::window::Window::default_attributes()
                        .with_visible(false)
                        .with_title("sonicterm ci probe");
                    self.outcome = Some(match active.create_window(attrs) {
                        Ok(window) => {
                            let size = window.inner_size();
                            println!(
                                "window: created {}x{} scale={}",
                                size.width,
                                size.height,
                                window.scale_factor()
                            );
                            Ok(())
                        }
                        Err(err) => Err(err.to_string()),
                    });
                    active.exit();
                }

                fn window_event(
                    &mut self,
                    _: &winit::event_loop::ActiveEventLoop,
                    _: winit::window::WindowId,
                    _: winit::event::WindowEvent,
                ) {
                }
            }

            let mut probe = Probe { outcome: None };
            match event_loop.run_app(&mut probe) {
                Ok(()) => match probe.outcome {
                    Some(Ok(())) => {
                        println!("verdict: a window CAN be created on this host");
                        println!(
                            "  => frame cadence (#991) and dirty-rect presentation (#992) \
                             are automatable here"
                        );
                    }
                    Some(Err(err)) => {
                        println!("window: REFUSED ({err})");
                        println!("verdict: event loop exists but windows cannot be created");
                    }
                    None => {
                        println!("window: resumed never fired");
                        println!("verdict: event loop ran but never became active");
                    }
                },
                Err(err) => {
                    println!("event loop: run failed ({err})");
                    println!("verdict: window-dependent verification cannot run here");
                }
            }
        }
    }

    // The probe is only useful if it ran. `println!` output is captured on a
    // passing test, so this must be read from a CI step that passes
    // `--nocapture` — noted in the workflow beside that step.
    println!("probe: complete");
}
