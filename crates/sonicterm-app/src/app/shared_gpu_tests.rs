use super::*;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

#[test]
fn main_renderer_wins_over_every_window_and_warm_renderer() {
    // The main device is the one warm-pool and tear-out windows share, so it must win even over
    // a window with a lower id.
    let (main, window, warm) = ("main", "window-1", "warm");
    assert_eq!(select_shared_renderer(Some(main), [(1_u64, window)], [warm]), Some(main));
}

#[test]
fn lowest_window_key_wins_without_a_main_renderer() {
    // HashMap iteration order must not choose the device: both orders pick the lowest key, and
    // a window renderer still beats a warm one.
    let (low, high, warm) = ("window-3", "window-7", "warm");
    for windows in [[(7_u64, high), (3, low)], [(3, low), (7, high)]] {
        assert_eq!(select_shared_renderer(None, windows, [warm]), Some(low));
    }
}

#[test]
fn first_warm_renderer_wins_without_main_or_window_renderers() {
    // With no main or window renderer, the first warm entry is the only live device left.
    let no_windows: [(u64, &str); 0] = [];
    let (first, second) = ("warm-first", "warm-second");
    assert_eq!(select_shared_renderer(None, no_windows, [first, second]), Some(first));
}

#[test]
fn no_renderer_anywhere_selects_nothing() {
    // With nothing to share, New Window must fall back to opening the first device itself.
    let no_windows: [(u64, &str); 0] = [];
    let no_warm: [&str; 0] = [];
    assert!(select_shared_renderer(None, no_windows, no_warm).is_none());
}

#[test]
fn headless_windows_offer_no_shared_context() {
    // Synthetic windows carry no renderer; the App adapter must skip them rather than pick one.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.main_renderer().is_none() && app.windows[&child].renderer.is_none());
    assert!(app.warm_window_pool.is_empty());
    assert!(app.shared_gpu_context().is_none());
}

/// A fallback waker called on another thread reaches a real event loop as exactly one
/// `FontFallbackReady` carrying its window and notice. Windows-only: there an event loop may run
/// off the main thread, and `isolated()` keeps it the only loop in its process.
#[cfg(windows)]
#[test]
fn font_fallback_waker_posts_through_a_real_windows_event_loop() {
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use winit::platform::windows::EventLoopBuilderExtWindows;
    use winit::window::WindowId;

    /// Records every user event the loop delivers.
    #[derive(Default)]
    struct Receiver {
        received: Vec<UserEvent>,
    }

    impl ApplicationHandler<UserEvent> for Receiver {
        fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

        fn window_event(
            &mut self,
            _event_loop: &ActiveEventLoop,
            _id: WindowId,
            _event: WindowEvent,
        ) {
        }

        fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
            self.received.push(event);
        }
    }

    if crate::app::pty_test_support::isolated() {
        return;
    }
    let mut event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .expect("Windows event loop");
    let window_id = WindowId::dummy();
    let waker = font_fallback_waker(event_loop.create_proxy(), window_id);
    std::thread::spawn(move || waker(42)).join().expect("the poster thread");
    // Bounded well inside the isolation envelope's 60 s.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut receiver = Receiver::default();
    while receiver.received.is_empty() && std::time::Instant::now() < deadline {
        let status =
            event_loop.pump_app_events(Some(std::time::Duration::from_millis(50)), &mut receiver);
        if let PumpStatus::Exit(_) = status {
            // When: the loop exited, no later pump can deliver the event.
            break;
        }
    }
    assert_eq!(receiver.received, vec![UserEvent::FontFallbackReady { window_id, notice_id: 42 }]);
}
