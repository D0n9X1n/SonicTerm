#![cfg(target_os = "windows")]

use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use crossbeam_channel::Receiver;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::Theme,
};
use sonicterm_io::pty::{PtyHandle, PtyOutputChunk, PtyReplySender};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{GetKeyboardState, SetKeyboardState},
        WindowsAndMessaging::{PostMessageW, SendMessageW, WM_CHAR, WM_KEYDOWN, WM_KEYUP},
    },
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    platform::windows::{
        EventLoopBuilderExtWindows, KeyEventExtWindows, WindowAttributesExtWindows,
    },
    window::{Window, WindowId},
};

#[derive(Clone, Copy, Debug)]
struct Stroke {
    vk: u16,
    scan: u16,
    extended: bool,
    down: bool,
    repeat: u16,
    previous: bool,
    controlled_shift: bool,
    controlled_ctrl: bool,
    unicode: &'static [u16],
}

impl Stroke {
    fn new(vk: u16, scan: u16, extended: bool, down: bool) -> Self {
        Self {
            vk,
            scan,
            extended,
            down,
            repeat: 1,
            previous: !down,
            controlled_shift: false,
            controlled_ctrl: false,
            unicode: &[],
        }
    }

    fn lparam(self) -> LPARAM {
        LPARAM(
            (u32::from(self.repeat)
                | (u32::from(self.scan) << 16)
                | (u32::from(self.extended) << 24)
                | (u32::from(self.previous) << 30)
                | (u32::from(!self.down) << 31)) as isize,
        )
    }
}

fn strokes() -> VecDeque<Stroke> {
    let mut enter = Stroke::new(13, 0x1c, false, true);
    enter.unicode = &[13];
    let mut shift_enter = enter;
    shift_enter.controlled_shift = true;
    let mut shift_release = Stroke::new(13, 0x1c, false, false);
    shift_release.controlled_shift = true;
    let mut ctrl_q = Stroke::new(0x51, 0x10, false, true);
    ctrl_q.controlled_ctrl = true;
    ctrl_q.unicode = &[0x11];
    let mut ctrl_release = Stroke::new(0x51, 0x10, false, false);
    ctrl_release.controlled_ctrl = true;
    let mut repeated = Stroke::new(0x27, 0x4d, true, true);
    repeated.repeat = 7;
    repeated.previous = true;
    let mut unicode = Stroke::new(0x41, 0x1e, false, true);
    unicode.unicode = &[0x03bb];
    VecDeque::from([
        enter,
        Stroke::new(13, 0x1c, false, false),
        shift_enter,
        shift_release,
        ctrl_q,
        ctrl_release,
        Stroke::new(0x26, 0x48, true, true),
        Stroke::new(0x26, 0x48, true, false),
        Stroke::new(0x70, 0x3b, false, true),
        Stroke::new(0x70, 0x3b, false, false),
        Stroke::new(0x27, 0x4d, true, true),
        repeated,
        Stroke::new(0x27, 0x4d, true, false),
        unicode,
        Stroke::new(0x41, 0x1e, false, false),
        Stroke::new(0x7b, 0x58, false, true),
        Stroke::new(0x7b, 0x58, false, false),
    ])
}

struct ThreadKeyboardState([u8; 256]);

impl ThreadKeyboardState {
    fn controlled_modifiers(shift: bool, ctrl: bool) -> Result<Self, String> {
        let mut original = [0; 256];
        // SAFETY: both snapshots are complete arrays; SetKeyboardState affects only this calling test thread.
        unsafe { GetKeyboardState(&mut original) }.map_err(|error| error.to_string())?;
        let restore = Self(original);
        let mut controlled = original;
        for vk in [0x10, 0xa0, 0xa1, 0x11, 0xa2, 0xa3, 0x12, 0xa4, 0xa5] {
            controlled[vk] &= 0x7f;
        }
        controlled[0x10] |= u8::from(shift) << 7;
        controlled[0xa0] |= u8::from(shift) << 7;
        controlled[0x11] |= u8::from(ctrl) << 7;
        controlled[0xa2] |= u8::from(ctrl) << 7;
        // SAFETY: restore owns the exact original thread snapshot before the controlled state is installed.
        unsafe { SetKeyboardState(&controlled) }.map_err(|error| error.to_string())?;
        Ok(restore)
    }

    fn restore(&self) -> Result<(), String> {
        // SAFETY: the saved array is the full thread-local keyboard snapshot captured by this guard.
        unsafe { SetKeyboardState(&self.0) }.map_err(|error| error.to_string())
    }
}

// Lifecycle: ThreadKeyboardState restores the test thread on unwind; normal dispatch checks restoration explicitly.
impl Drop for ThreadKeyboardState {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("failed to restore test thread keyboard state: {error}");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Record([u32; 6]);

impl Record {
    fn parse(line: &str) -> Result<Self, String> {
        let values = line
            .trim()
            .split(';')
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("invalid console record {line:?}: {error}"))?;
        let fields =
            values.try_into().map_err(|_| format!("console record field count: {line:?}"))?;
        Ok(Self(fields))
    }
}

struct Session {
    app: App,
    window: Arc<Window>,
    logical_id: WindowId,
    pane: u64,
    output: Receiver<PtyOutputChunk>,
    replies: Receiver<Vec<u8>>,
    reply_sender: PtyReplySender,
    pending_text: String,
    transcript: String,
    steps: VecDeque<Stroke>,
    in_flight: Option<Stroke>,
    expected: VecDeque<Record>,
    ready: bool,
    native_done: bool,
    deadline: Instant,
    step_deadline: Instant,
    validated: usize,
}

impl Session {
    fn new(active: &ActiveEventLoop) -> Result<Self, String> {
        let window = Arc::new(
            active
                .create_window(
                    Window::default_attributes()
                        .with_title("SonicTerm native console input regression")
                        .with_inner_size(LogicalSize::new(240.0, 80.0))
                        .with_visible(false)
                        .with_active(false)
                        .with_skip_taskbar(true),
                )
                .map_err(|error| error.to_string())?,
        );
        let mut app = App::new(
            Theme::default(),
            Config::default(),
            Keymap {
                meta: Meta { name: "native-console-test".into(), version: "1.0".into() },
                bindings: Vec::new(),
            },
        );
        let (pane, replies) = app.__test_seed_tab_with_reply("console-child");
        let logical_id = app.__test_main_window_id().ok_or("missing seeded main window")?;
        let pty = PtyHandle::spawn_with_args(env!("CARGO_BIN_EXE_win32_input_helper"), &[], 80, 24)
            .map_err(|error| format!("ConPTY helper spawn failed: {error}"))?;
        let output = pty.out_rx.clone();
        let reply_sender = pty.reply_sender();
        if !app.__test_set_pane_pty(pane, Some(pty)) {
            return Err("failed to attach real PTY to seeded pane".into());
        }
        let now = Instant::now();
        Ok(Self {
            app,
            window,
            logical_id,
            pane,
            output,
            replies,
            reply_sender,
            pending_text: String::new(),
            transcript: String::new(),
            steps: strokes(),
            in_flight: None,
            expected: VecDeque::new(),
            ready: false,
            native_done: false,
            deadline: now + Duration::from_secs(30),
            step_deadline: now + Duration::from_secs(5),
            validated: 0,
        })
    }

    fn hwnd(&self) -> Result<HWND, String> {
        let handle = self.window.window_handle().map_err(|error| error.to_string())?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return Err("native window has no Win32 handle".into());
        };
        Ok(HWND(handle.hwnd.get() as *mut _))
    }

    fn post_stroke(&self, stroke: Stroke) -> Result<(), String> {
        let hwnd = self.hwnd()?;
        let message = if stroke.down { WM_KEYDOWN } else { WM_KEYUP };
        let controlled = stroke.controlled_shift || stroke.controlled_ctrl;
        let controlled_state = if controlled {
            println!("controlled modifier thread snapshot; not physical keyboard evidence");
            Some(ThreadKeyboardState::controlled_modifiers(
                stroke.controlled_shift,
                stroke.controlled_ctrl,
            )?)
        } else {
            None
        };
        // When: text or controlled modifiers need synchronous capture, queue CHAR before sending the key transition.
        if (stroke.down && stroke.vk == 0x41) || controlled {
            for unit in stroke.unicode {
                // SAFETY: window retains the destination HWND; messages carry only scalar fixture data.
                unsafe {
                    PostMessageW(Some(hwnd), WM_CHAR, WPARAM(usize::from(*unit)), stroke.lparam())
                }
                .map_err(|error| error.to_string())?;
            }
            // SAFETY: only this retained test HWND receives synchronous input; no state borrow crosses dispatch.
            unsafe {
                SendMessageW(
                    hwnd,
                    message,
                    Some(WPARAM(usize::from(stroke.vk))),
                    Some(stroke.lparam()),
                );
            }
        } else {
            // SAFETY: only the retained test HWND receives the scalar key-message payload.
            unsafe {
                PostMessageW(Some(hwnd), message, WPARAM(usize::from(stroke.vk)), stroke.lparam())
            }
            .map_err(|error| error.to_string())?;
        }
        if let Some(state) = controlled_state {
            state.restore()?;
        }
        Ok(())
    }

    fn read_output(&mut self) -> Result<(), String> {
        while let Ok(bytes) = self.output.try_recv() {
            // When: captured output exceeds the diagnostic bound, fail rather than retain unbounded child output.
            if self.transcript.len() + bytes.len() > 65_536 {
                return Err("ConPTY output exceeded capture bound".into());
            }
            if !self.app.__test_advance_pane_parser(self.pane, &bytes) {
                return Err("child output lost its pane parser".into());
            }
            // ConPTY startup waits for the real parser's cursor reply before the console child can report readiness.
            while let Ok(reply) = self.replies.try_recv() {
                self.reply_sender
                    .send(reply)
                    .map_err(|error| format!("PTY reply failed: {error}"))?;
            }
            let text = String::from_utf8_lossy(&bytes);
            self.transcript.push_str(&text);
            self.pending_text.push_str(&text);
        }
        // ConPTY may replace line endings with cursor movement; bracketed frames retain an explicit boundary.
        while let Some(end) = self.pending_text.find(']') {
            let frame: String = self.pending_text.drain(..=end).collect();
            let Some(start) = frame.rfind('[') else {
                continue;
            };
            let line = &frame[start + 1..frame.len() - 1];
            if line == "WIN32_READY" {
                if !self.transcript.contains("\x1b[?9001h") {
                    return Err(
                        "ConPTY readiness arrived without an observed Win32 negotiation".into()
                    );
                }
                self.ready = true;
            }
            if line == "NATIVE_DONE" {
                self.native_done = true;
            }
            if let Some((_, payload)) = line.split_once("WIN32_RECORD ") {
                let actual = Record::parse(payload)?;
                let expected = self
                    .expected
                    .pop_front()
                    .ok_or_else(|| format!("unexpected native record {actual:?}"))?;
                if actual != expected {
                    return Err(format!(
                        "ConPTY record mismatch: actual={actual:?} expected={expected:?}"
                    ));
                }
                self.validated += 1;
                println!("child record matched {actual:?}");
                if self.expected.is_empty() {
                    self.in_flight = None;
                }
            }
        }
        Ok(())
    }

    fn poll(&mut self) -> Result<bool, String> {
        self.read_output()?;
        let now = Instant::now();
        if now >= self.deadline || now >= self.step_deadline {
            return Err(format!(
                "native console deadline: in_flight={:?}, transcript={:?}",
                self.in_flight, self.transcript
            ));
        }
        // When: a child-confirmed record completes the step, only then send the next key through the production queue.
        if self.ready && self.in_flight.is_none() && self.expected.is_empty() {
            if let Some(stroke) = self.steps.pop_front() {
                self.in_flight = Some(stroke);
                self.step_deadline = now + Duration::from_secs(3);
                self.post_stroke(stroke)?;
            }
        }
        if self.native_done {
            if !self.steps.is_empty()
                || self.in_flight.is_some()
                || !self.expected.is_empty()
                || self.validated == 0
            {
                return Err("child finished before all native records were validated".into());
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn key_event(&mut self, active: &ActiveEventLoop, event: WindowEvent) -> Result<(), String> {
        let WindowEvent::KeyboardInput { event: key, is_synthetic, .. } = &event else {
            return Ok(());
        };
        // When: focus-generated input has no physical source, it must not create native ownership for this test.
        if *is_synthetic {
            if key.native_key_event().is_some() {
                return Err("synthetic key carried native metadata".into());
            }
            return Ok(());
        }
        {
            let stroke = self.in_flight.ok_or("native key arrived without an active test step")?;
            let native = key.native_key_event().ok_or("native key metadata missing")?;
            if (native.virtual_key, native.scan_code, native.key_down, native.repeat_count)
                != (stroke.vk, stroke.scan, stroke.down, stroke.repeat)
                || native.unicode != stroke.unicode
                || (native.control_key_state & 256 != 0) != stroke.extended
                || (stroke.controlled_shift && native.control_key_state & 31 != 16)
                || (stroke.controlled_ctrl && native.control_key_state & 31 != 8)
                || (key.state == ElementState::Pressed) != stroke.down
            {
                return Err(format!("winit metadata mismatch: {native:?}, stroke={stroke:?}"));
            }
            let units =
                if native.unicode.is_empty() { &[0][..] } else { native.unicode.as_slice() };
            for unit in units {
                self.expected.push_back(Record([
                    u32::from(native.virtual_key),
                    u32::from(native.scan_code),
                    u32::from(*unit),
                    u32::from(native.key_down),
                    native.control_key_state,
                    u32::from(native.repeat_count),
                ]));
            }
        }
        // This preserves the actual winit KeyEvent while selecting the seeded App main window's live pane.
        ApplicationHandler::window_event(&mut self.app, active, self.logical_id, event);
        Ok(())
    }
}

#[derive(Default)]
struct Probe {
    session: Option<Session>,
    outcome: Option<Result<(), String>>,
}

impl Probe {
    fn finish(&mut self, active: &ActiveEventLoop, result: Result<(), String>) {
        if result.is_err() {
            if let Some(session) = self.session.as_ref() {
                eprintln!("child transcript: {:?}", session.transcript);
            }
        }
        self.outcome = Some(result);
        // Dropping App owns PTY cancellation and child teardown before the native window is released.
        self.session.take();
        active.exit();
    }
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        if self.session.is_some() || self.outcome.is_some() {
            return;
        }
        match Session::new(active) {
            Ok(session) => self.session = Some(session),
            Err(error) => self.finish(active, Err(error)),
        }
    }

    fn window_event(&mut self, active: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if id != session.window.id() {
            return;
        }
        if let Err(error) = session.key_event(active, event) {
            self.finish(active, Err(error));
        }
    }

    fn about_to_wait(&mut self, active: &ActiveEventLoop) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.poll() {
            Ok(false) => active.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(5),
            )),
            Ok(true) => self.finish(active, Ok(())),
            Err(error) => self.finish(active, Err(error)),
        }
    }
}

#[test]
fn conpty_requested_native_records_route_from_window_through_app() {
    // ConPTY's own request enables posted native input; this proves child records, not physical keyboard or IME acceptance.
    let event_loop = EventLoop::builder().with_any_thread(true).build().expect("native event loop");
    let mut probe = Probe::default();
    event_loop.run_app(&mut probe).expect("native event loop execution");
    probe
        .outcome
        .expect("native probe did not report an outcome")
        .expect("native ConPTY input regression");
}
