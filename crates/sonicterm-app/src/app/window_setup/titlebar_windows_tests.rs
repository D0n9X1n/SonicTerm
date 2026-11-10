//! Native contracts for the snap subclass on real Win32 windows: the hit test answers
//! `HTMAXBUTTON` only over the maximise button, non-client presses reach the window below as
//! client presses, the system never runs its own maximise, a window closed mid-forward gets
//! nothing more, and winit itself turns the forwarded press into one press and release.

use std::cell::{Cell, RefCell};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, IsWindow, SendMessageW, WINDOW_EX_STYLE, WM_LBUTTONDOWN,
    WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSEMOVE, WS_POPUP,
};

use super::super::snap_route::{lparam_of, SnapTarget, HTCLIENT, HTMAXBUTTON, WM_MOUSEMOVE};
use super::*;

/// `HTCAPTION`: a non-client hit code that is not the maximise button.
const HTCAPTION: usize = 2;

thread_local! {
    /// Messages the recorder (the procedure below the snap subclass) received: id, wparam, lparam.
    static LOG: RefCell<Vec<(u32, usize, isize)>> = const { RefCell::new(Vec::new()) };
    /// The recorder closes its window on the next client move, as an App closing during a callback.
    static CLOSE_ON_MOVE: Cell<bool> = const { Cell::new(false) };
    /// What the recorder does during the next hit test: close its window, or clear its snap
    /// target as full screen would, both while the subclass waits for the default answer.
    static ON_HIT: Cell<HitSideEffect> = const { Cell::new(HitSideEffect::None) };
}

/// A side effect the recorder runs inside a hit test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HitSideEffect {
    /// Nothing.
    None,
    /// Close the window.
    Close,
    /// Clear the snap target.
    ClearTarget,
}

/// The procedure below the snap subclass, standing in for winit: it answers `HTCLIENT` to every
/// hit test (an undecorated window's client area covers it) and records every mouse message.
// SAFETY: Windows calls this only as the test window's subclass procedure on its own thread.
unsafe extern "system" fn recorder(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    _data: usize,
) -> LRESULT {
    match message {
        snap_route::WM_NCHITTEST => {
            match ON_HIT.with(|effect| effect.replace(HitSideEffect::None)) {
                HitSideEffect::Close => {
                    let _ =
                        // SAFETY: the test owns this window on this thread.
                        unsafe { DestroyWindow(hwnd) };
                }
                HitSideEffect::ClearTarget => sync_hwnd(hwnd, None),
                HitSideEffect::None => {}
            }
            LRESULT(HTCLIENT)
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_NCMOUSEMOVE | WM_NCLBUTTONDOWN | WM_NCLBUTTONUP => {
            LOG.with(|log| log.borrow_mut().push((message, wparam.0, lparam.0)));
            if message == WM_MOUSEMOVE && CLOSE_ON_MOVE.with(Cell::take) {
                let _ =
                    // SAFETY: the test owns this window on this thread.
                    unsafe { DestroyWindow(hwnd) };
            }
            LRESULT(0)
        }
        _ =>
        // SAFETY: hwnd is the recorder's own subclassed window.
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
    }
}

/// A hidden 800x400 popup at screen (-1000, 100), left of the primary monitor so screen points are
/// negative, with the recorder below the snap subclass and a 32 px caption band at scale 1:
/// maximise is client 708..754 x 0..32, its centre screen (-269, 116).
fn test_window(target: Option<SnapTarget>) -> HWND {
    LOG.with(|log| log.borrow_mut().clear());
    let hwnd =
        // SAFETY: creates a hidden window owned by this test thread; STATIC needs no class setup.
        unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("snap test"),
            WS_POPUP,
            -1000,
            100,
            800,
            400,
            None,
            None,
            None,
            None,
        )
    }
    .expect("hidden test window");
    assert!(
        // SAFETY: the window is live on this thread and recorder lives for the process.
        unsafe { SetWindowSubclass(hwnd, Some(recorder), 1, 0) }.as_bool()
    );
    sync_hwnd(hwnd, target);
    hwnd
}

/// The caption band target the tests use.
fn caption() -> Option<SnapTarget> {
    SnapTarget::of(WindowBand::caption(), 32.0, 1.0)
}

/// Send `message` to `hwnd` through the whole procedure chain, snap subclass first.
fn send(hwnd: HWND, message: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: hwnd is this thread's test window.
    unsafe { SendMessageW(hwnd, message, Some(WPARAM(wparam)), Some(LPARAM(lparam))) }.0
}

/// The recorded messages since the last clear.
fn take_log() -> Vec<(u32, usize, isize)> {
    LOG.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

/// Close `hwnd` and check its hook went with it.
fn close(hwnd: HWND) {
    // SAFETY: hwnd is this thread's test window.
    unsafe { DestroyWindow(hwnd) }.expect("destroy test window");
    assert!(!hooked(hwnd), "WM_NCDESTROY forgets the hook");
}

/// The hit test answers `HTMAXBUTTON` over the maximise button only, at negative screen points;
/// the other buttons and the terminal stay client area, so their in-app handling is unchanged.
#[test]
fn the_hit_test_names_only_the_maximise_button() {
    let hwnd = test_window(caption());
    assert!(hooked(hwnd));
    assert_eq!(send(hwnd, snap_route::WM_NCHITTEST, 0, lparam_of(-269, 116)), HTMAXBUTTON);
    assert_eq!(send(hwnd, snap_route::WM_NCHITTEST, 0, lparam_of(-315, 116)), HTCLIENT, "min");
    assert_eq!(send(hwnd, snap_route::WM_NCHITTEST, 0, lparam_of(-223, 116)), HTCLIENT, "close");
    assert_eq!(send(hwnd, snap_route::WM_NCHITTEST, 0, lparam_of(-600, 300)), HTCLIENT, "pane");
    // Full screen (no target) passes every hit test through.
    sync_hwnd(hwnd, None);
    assert_eq!(send(hwnd, snap_route::WM_NCHITTEST, 0, lparam_of(-269, 116)), HTCLIENT);
    close(hwnd);
}

/// A non-client press on the button reaches the window below as a client move and left press at
/// client coordinates, and is not passed on; the non-client release is swallowed, so the system's
/// own maximise never runs beside the App's. A hover sends the client move and passes the
/// original on for the flyout. Other hit codes pass through untouched.
#[test]
fn non_client_button_messages_reach_the_window_as_client_ones() {
    let hwnd = test_window(caption());
    let screen = lparam_of(-269, 116);
    let client = lparam_of(731, 16);
    assert_eq!(send(hwnd, WM_NCLBUTTONDOWN, HTMAXBUTTON as usize, screen), 0);
    assert_eq!(take_log(), vec![(WM_MOUSEMOVE, 0, client), (WM_LBUTTONDOWN, MK_LBUTTON, client)]);
    assert_eq!(send(hwnd, WM_NCLBUTTONUP, HTMAXBUTTON as usize, screen), 0);
    assert_eq!(take_log(), vec![], "the release goes nowhere");
    send(hwnd, WM_NCMOUSEMOVE, HTMAXBUTTON as usize, screen);
    let hover = vec![(WM_MOUSEMOVE, 0, client), (WM_NCMOUSEMOVE, HTMAXBUTTON as usize, screen)];
    assert_eq!(take_log(), hover);
    send(hwnd, WM_NCLBUTTONDOWN, HTCAPTION, screen);
    assert_eq!(take_log(), vec![(WM_NCLBUTTONDOWN, HTCAPTION, screen)]);
    close(hwnd);
}

/// A window without a target is not hooked, and its non-client press passes through unchanged.
#[test]
fn a_window_without_caption_buttons_is_left_alone() {
    let hwnd = test_window(None);
    assert!(!hooked(hwnd), "no target, no subclass");
    send(hwnd, WM_NCLBUTTONDOWN, HTMAXBUTTON as usize, lparam_of(-269, 116));
    assert_eq!(take_log(), vec![(WM_NCLBUTTONDOWN, HTMAXBUTTON as usize, lparam_of(-269, 116))]);
    // SAFETY: hwnd is this thread's test window.
    unsafe { DestroyWindow(hwnd) }.unwrap();
}

/// A window the App closes during the forwarded move gets no press afterwards and leaves no hook;
/// a new window, which may reuse the HWND value, gets a fresh hook of its own.
#[test]
fn a_window_closed_mid_forward_gets_nothing_more() {
    let hwnd = test_window(caption());
    CLOSE_ON_MOVE.with(|close| close.set(true));
    send(hwnd, WM_NCLBUTTONDOWN, HTMAXBUTTON as usize, lparam_of(-269, 116));
    assert_eq!(take_log(), vec![(WM_MOUSEMOVE, 0, lparam_of(731, 16))], "no press after close");
    assert!(!
        // SAFETY: IsWindow only reads whether the handle names a window.
        unsafe { IsWindow(Some(hwnd)) }.as_bool());
    assert!(!hooked(hwnd));
    let next = test_window(caption());
    assert!(hooked(next));
    assert_eq!(send(next, snap_route::WM_NCHITTEST, 0, lparam_of(-269, 116)), HTMAXBUTTON);
    close(next);
}

/// winit itself turns a forwarded non-client press and the client release into one left press and
/// release at the button, after a move there, so the App's caption logic runs exactly one click.
#[test]
fn winit_reports_one_press_and_release_for_a_forwarded_click() {
    use crate::app::pty_test_support::isolated;
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_LBUTTONUP};
    use winit::application::ApplicationHandler;
    use winit::event::{ElementState, MouseButton, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::windows::EventLoopBuilderExtWindows;
    use winit::window::Window;
    if isolated() {
        return;
    }
    /// The probe: its window, the pointer events seen, and a deadline.
    struct Probe {
        window: Option<Window>,
        seen: Vec<String>,
        deadline: std::time::Instant,
    }
    impl ApplicationHandler for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let attributes = Window::default_attributes()
                .with_visible(false)
                .with_active(false)
                .with_decorations(false)
                .with_inner_size(winit::dpi::PhysicalSize::new(800, 400));
            let window = event_loop.create_window(attributes).unwrap();
            sync(&window, WindowBand::caption(), 32.0, 1.0);
            let RawWindowHandle::Win32(handle) = window.window_handle().unwrap().as_raw() else {
                panic!("Windows handle")
            };
            let hwnd = HWND(handle.hwnd.get() as *mut _);
            let mut point = POINT { x: 731, y: 16 };
            assert!(
                // SAFETY: hwnd is the probe's window and point a local the call rewrites.
                unsafe { ClientToScreen(hwnd, &mut point) }.as_bool()
            );
            let screen = lparam_of(point.x, point.y);
            assert_eq!(send(hwnd, snap_route::WM_NCHITTEST, 0, screen), HTMAXBUTTON);
            // SAFETY: posting to the probe's own window injects no input into another application.
            unsafe {
                PostMessageW(
                    Some(hwnd),
                    WM_NCLBUTTONDOWN,
                    WPARAM(HTMAXBUTTON as usize),
                    LPARAM(screen),
                )
                .unwrap();
                PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(lparam_of(731, 16)))
                    .unwrap();
            }
            self.window = Some(window);
        }
        fn window_event(
            &mut self,
            event_loop: &ActiveEventLoop,
            _id: winit::window::WindowId,
            event: WindowEvent,
        ) {
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    self.seen.push(format!("move {},{}", position.x, position.y));
                }
                WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                    let released = state == ElementState::Released;
                    self.seen.push(if released { "release" } else { "press" }.to_string());
                    if released {
                        // When: the release arrived, the click is complete.
                        event_loop.exit();
                    }
                }
                _ => {
                    // When: any other event (focus, the real pointer entering), it is not counted.
                }
            }
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            assert!(std::time::Instant::now() < self.deadline, "forwarded click timed out");
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + std::time::Duration::from_millis(10),
            ));
        }
    }
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let mut probe = Probe {
        window: None,
        seen: Vec::new(),
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(20),
    };
    event_loop.run_app(&mut probe).unwrap();
    let clicks: Vec<&str> = probe
        .seen
        .iter()
        .map(String::as_str)
        .filter(|seen| !seen.starts_with("move") || *seen == "move 731,16")
        .collect();
    assert_eq!(clicks, vec!["move 731,16", "press", "release"], "all seen: {:?}", probe.seen);
}

/// A window closed, or put into full screen, while the subclass waits for the default hit test
/// gets that default answer: the maximise answer is decided on the window's state after the call.
#[test]
fn a_hit_test_decides_on_the_window_as_it_is_after_the_default() {
    let hwnd = test_window(caption());
    ON_HIT.with(|effect| effect.set(HitSideEffect::ClearTarget));
    assert_eq!(send(hwnd, snap_route::WM_NCHITTEST, 0, lparam_of(-269, 116)), HTCLIENT);
    assert!(hooked(hwnd), "a cleared target keeps the hook, passing everything through");
    sync_hwnd(hwnd, caption());
    assert_eq!(send(hwnd, snap_route::WM_NCHITTEST, 0, lparam_of(-269, 116)), HTMAXBUTTON);
    ON_HIT.with(|effect| effect.set(HitSideEffect::Close));
    assert_eq!(send(hwnd, snap_route::WM_NCHITTEST, 0, lparam_of(-269, 116)), HTCLIENT);
    assert!(!hooked(hwnd), "the closed window's hook is gone");
}

/// With the real pointer (moved, never clicked; the click is posted to the test window), the
/// caption logic sees what a user's hover and click produce: resting on maximise keeps it hovered
/// with no leave flicker, a press there holds the mouse capture and survives to its release, which
/// runs maximise once, and the hover clears on the pane and outside the window. Ignored by default
/// because it moves the pointer; the pointer is put back before any assertion fails.
#[test]
#[ignore = "moves the real pointer over a test window"]
fn real_pointer_hover_click_and_leave_reach_the_caption_logic() {
    use crate::app::pty_test_support::{isolated_with_output, IsolationConfig};
    use crate::app::titlebar_band::release_action;
    use sonicterm_ui::window_band::{caption_button_at, CaptionButton};
    use windows::Win32::UI::Input::KeyboardAndMouse::GetCapture;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetCursorPos, PostMessageW, SetCursorPos, WindowFromPoint, WM_LBUTTONUP,
    };
    use winit::application::ApplicationHandler;
    use winit::event::{ElementState, MouseButton, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::windows::EventLoopBuilderExtWindows;
    use winit::window::{Window, WindowLevel};
    if isolated_with_output(IsolationConfig::ORDINARY) {
        return;
    }
    /// The probe: the window, the caption state the App would hold, and the phase it is in.
    struct Probe {
        window: Option<(Window, HWND)>,
        geometry: (f32, f32, f32),
        hovered: Option<CaptionButton>,
        pressed: Option<CaptionButton>,
        leaves: usize,
        captured_at_press: bool,
        action: Option<Option<CaptionButton>>,
        phase: usize,
        next_at: std::time::Instant,
        saved: POINT,
        failures: Vec<String>,
        deadline: std::time::Instant,
    }
    impl Probe {
        /// The caption button at client `(x_px, y_px)`, by the App's own geometry.
        fn button_at(&self, x_px: f32, y_px: f32) -> Option<CaptionButton> {
            let (width, band_height, scale) = self.geometry;
            caption_button_at(&WindowBand::caption(), width, band_height, scale, x_px, y_px)
        }
        /// Client `(x_px, y_px)` as a screen point.
        fn screen(&self, x_px: i32, y_px: i32) -> POINT {
            let mut point = POINT { x: x_px, y: y_px };
            if let Some((_, hwnd)) = self.window.as_ref() {
                let _ =
                    // SAFETY: hwnd is the probe's window and point a local the call rewrites.
                    unsafe { ClientToScreen(*hwnd, &mut point) };
            }
            point
        }
        /// The maximise button's centre, client px.
        fn maximise(&self) -> (i32, i32) {
            let (width, band_height, scale) = self.geometry;
            let button = (46.0 * scale).round();
            ((width - 1.5 * button) as i32, (band_height / 2.0) as i32)
        }
        /// Record `failure` when `ok` is false.
        fn check(&mut self, ok: bool, failure: String) {
            if !ok {
                self.failures.push(failure);
            }
        }
        /// Post `message` to the probe's own window.
        fn post(&self, message: u32, wparam: usize, lparam: isize) {
            if let Some((_, hwnd)) = self.window.as_ref() {
                let _ =
                    // SAFETY: posting to the probe's own window injects no input elsewhere.
                    unsafe { PostMessageW(Some(*hwnd), message, WPARAM(wparam), LPARAM(lparam)) };
            }
        }
        /// The class name of the window under screen `point`.
        fn class_under(point: POINT) -> String {
            let mut class = [0u16; 128];
            let length =
                // SAFETY: WindowFromPoint and GetClassNameW only read window identity.
                unsafe { GetClassNameW(WindowFromPoint(point), &mut class) };
            String::from_utf16_lossy(&class[..length.max(0) as usize])
        }
    }
    /// Put the pointer at screen `point`; nothing is clicked.
    fn point_at(point: POINT) {
        let _ =
            // SAFETY: moves the pointer only.
            unsafe { SetCursorPos(point.x, point.y) };
    }
    impl ApplicationHandler for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let attributes = Window::default_attributes()
                .with_active(false)
                .with_decorations(false)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_position(winit::dpi::PhysicalPosition::new(400, 300))
                .with_inner_size(winit::dpi::PhysicalSize::new(900, 400));
            let window = event_loop.create_window(attributes).unwrap();
            let scale = window.scale_factor() as f32;
            let band_height = (32.0 * scale).round();
            self.geometry = (window.inner_size().width as f32, band_height, scale);
            sync(&window, WindowBand::caption(), band_height, scale);
            let RawWindowHandle::Win32(handle) = window.window_handle().unwrap().as_raw() else {
                panic!("Windows handle")
            };
            self.window = Some((window, HWND(handle.hwnd.get() as *mut _)));
            let _ =
                // SAFETY: saved is a field the call writes.
                unsafe { GetCursorPos(&mut self.saved) };
            self.next_at = std::time::Instant::now() + std::time::Duration::from_millis(500);
        }
        fn window_event(
            &mut self,
            _event_loop: &ActiveEventLoop,
            _id: winit::window::WindowId,
            event: WindowEvent,
        ) {
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    // As hover_window_band: hover follows the pointer; leaving a held button
                    // cancels it.
                    let under = self.button_at(position.x as f32, position.y as f32);
                    if self.pressed.is_some() && self.pressed != under {
                        self.pressed = None;
                    }
                    self.hovered = under;
                }
                WindowEvent::CursorLeft { .. } => {
                    // As cancel_window_band: a leave clears hover and press.
                    self.leaves += 1;
                    self.hovered = None;
                    self.pressed = None;
                }
                WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                    let hwnd = self.window.as_ref().map(|(_, hwnd)| *hwnd);
                    if state == ElementState::Pressed {
                        let holder =
                            // SAFETY: GetCapture only reads which window holds the capture.
                            unsafe { GetCapture() };
                        self.captured_at_press = Some(holder) == hwnd;
                        self.pressed = self.hovered;
                    } else {
                        // When: Released, release_window_band decides the action.
                        let action = release_action(self.pressed.take(), self.hovered, false);
                        self.action = Some(action);
                    }
                }
                _ => {
                    // When: any other event, the caption logic ignores it.
                }
            }
        }
        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            let now = std::time::Instant::now();
            if now >= self.deadline {
                // When: the deadline passed, the pointer goes back before the test fails.
                point_at(self.saved);
                panic!("real pointer probe timed out in phase {}", self.phase);
            }
            if now >= self.next_at {
                let Some(wait_ms) = self.step() else {
                    // When: step returns None, every phase ran: the pointer is back.
                    event_loop.exit();
                    return;
                };
                self.phase += 1;
                self.next_at = now + std::time::Duration::from_millis(wait_ms);
            }
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                now + std::time::Duration::from_millis(10),
            ));
        }
    }
    impl Probe {
        /// Run the current phase; returns how long to wait before the next, or `None` when done.
        fn step(&mut self) -> Option<u64> {
            let (max_x, max_y) = self.maximise();
            let on_maximise = self.screen(max_x, max_y);
            match self.phase {
                0 => {
                    // Rest on maximise long enough for the system to offer the flyout.
                    self.leaves = 0;
                    point_at(on_maximise);
                    Some(1500)
                }
                1 => {
                    let (hovered, leaves) = (self.hovered, self.leaves);
                    self.check(
                        hovered == Some(CaptionButton::Maximize),
                        format!("rest {hovered:?}"),
                    );
                    self.check(leaves == 0, format!("rest: {leaves} leaves on the button"));
                    let below = self.screen(max_x, max_y * 2 + 40);
                    println!("under the button after the hover: {}", Probe::class_under(below));
                    let screen = lparam_of(on_maximise.x, on_maximise.y);
                    self.post(snap_route::WM_NCLBUTTONDOWN, HTMAXBUTTON as usize, screen);
                    Some(400)
                }
                2 => {
                    let (captured, pressed) = (self.captured_at_press, self.pressed);
                    self.check(captured, "press: the window did not hold the capture".into());
                    self.check(
                        pressed == Some(CaptionButton::Maximize),
                        format!("held {pressed:?}"),
                    );
                    self.post(WM_LBUTTONUP, 0, lparam_of(max_x, max_y));
                    Some(400)
                }
                3 => {
                    let action = self.action;
                    let once = action == Some(Some(CaptionButton::Maximize));
                    self.check(once, format!("click: {action:?}"));
                    point_at(self.screen(200, 300));
                    Some(600)
                }
                4 => {
                    let hovered = self.hovered;
                    self.check(hovered.is_none(), format!("pane: still hovered {hovered:?}"));
                    point_at(on_maximise);
                    Some(800)
                }
                5 => {
                    let hovered = self.hovered;
                    self.check(
                        hovered == Some(CaptionButton::Maximize),
                        format!("back {hovered:?}"),
                    );
                    point_at(self.screen(-200, -150));
                    Some(800)
                }
                _ => {
                    let (hovered, pressed) = (self.hovered, self.pressed);
                    let clear = hovered.is_none() && pressed.is_none();
                    self.check(clear, format!("outside: {hovered:?} {pressed:?}"));
                    point_at(self.saved);
                    None
                }
            }
        }
    }
    let event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
    let now = std::time::Instant::now();
    let mut probe = Probe {
        window: None,
        geometry: (0.0, 0.0, 1.0),
        hovered: None,
        pressed: None,
        leaves: 0,
        captured_at_press: false,
        action: None,
        phase: 0,
        next_at: now,
        saved: POINT::default(),
        failures: Vec::new(),
        deadline: now + std::time::Duration::from_secs(30),
    };
    event_loop.run_app(&mut probe).unwrap();
    assert!(probe.failures.is_empty(), "{:#?}", probe.failures);
}
