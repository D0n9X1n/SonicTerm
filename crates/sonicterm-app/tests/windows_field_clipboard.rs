//! Native Windows proof for selection, clipboard, pointer routing, IME anchoring
//! and presented-geometry lifecycle of the four app text fields.
//!
//! Real visible HWNDs present through the GDI software path with bundled fonts.
//! Keys reach winit as `WM_KEYDOWN` and `WM_KEYUP` messages posted to one test HWND;
//! winit's own `TranslateMessage` derives `WM_CHAR`. Ctrl and Shift come from a
//! thread-local `SetKeyboardState` snapshot held from key-down to release, which is
//! controlled modifier state, not physical keyboard evidence. Pointer, IME,
//! resize and modifier events are winit `WindowEvent`s delivered through
//! `ApplicationHandler::window_event`; a synthetic IME event proves routing only,
//! never OS IME behavior. The clipboard is the App's in-memory test buffer.
#![cfg(target_os = "windows")]

use std::{
    collections::VecDeque,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Arc,
    time::{Duration, Instant},
};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, SoftwareRenderMode},
    keymap::{Action, ActionWrapper, Binding, Keymap, Meta},
    theme::Theme,
};
use sonicterm_gpu::{
    core::{GpuRenderer, RendererSettings, SurfaceAppearance},
    device_errors::GpuFaultKind,
    field_geometry::{FieldHit, FieldHitMode, FieldRect},
};
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID},
    UI::{
        Input::KeyboardAndMouse::{GetKeyboardState, SetKeyboardState},
        WindowsAndMessaging::{
            PostMessageW, SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_SHOWWINDOW, WM_KEYDOWN, WM_KEYUP,
        },
    },
};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{DeviceId, ElementState, Ime, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    platform::windows::{EventLoopBuilderExtWindows, KeyEventExtWindows},
    window::{Window, WindowId},
};

/// Clipboard text pasted into every field; the `▏` must survive as literal text.
const PASTE: &str = "alpha▏beta";
/// Terminal-area point of either window, away from both query fields.
const TERMINAL_POINT: (f32, f32) = (360.0, 300.0);
/// Inner size of both test windows, in physical pixels.
const WINDOW_SIZE: (u32, u32) = (760, 520);

/// Return an error carrying a formatted message when a condition fails.
macro_rules! ensure {
    ($condition:expr, $($message:tt)+) => {
        if !$condition {
            return Err(format!($($message)+));
        }
    };
}

/// One native key press-and-release sent to the current key window.
#[derive(Clone, Copy, Debug)]
struct Stroke {
    virtual_key: u16,
    scan: u16,
    extended: bool,
    ctrl: bool,
    shift: bool,
}

impl Stroke {
    /// Key-message lParam: repeat count, scan code, extended bit, previous and transition state.
    fn lparam(self, down: bool) -> LPARAM {
        LPARAM(
            (1 | (u32::from(self.scan) << 16)
                | (u32::from(self.extended) << 24)
                | (u32::from(!down) << 30)
                | (u32::from(!down) << 31)) as isize,
        )
    }
}

const CTRL_A: Stroke =
    Stroke { virtual_key: 0x41, scan: 0x1e, extended: false, ctrl: true, shift: false };
const COPY: Stroke =
    Stroke { virtual_key: 0x43, scan: 0x2e, extended: false, ctrl: true, shift: true };
const PASTE_KEY: Stroke =
    Stroke { virtual_key: 0x56, scan: 0x2f, extended: false, ctrl: true, shift: true };
const SHIFT_LEFT: Stroke =
    Stroke { virtual_key: 0x25, scan: 0x4b, extended: true, ctrl: false, shift: true };
const TYPE_X: Stroke =
    Stroke { virtual_key: 0x58, scan: 0x2d, extended: false, ctrl: false, shift: false };
const END: Stroke =
    Stroke { virtual_key: 0x23, scan: 0x4f, extended: true, ctrl: false, shift: false };
const ENTER: Stroke =
    Stroke { virtual_key: 0x0d, scan: 0x1c, extended: false, ctrl: false, shift: false };
const ESCAPE: Stroke =
    Stroke { virtual_key: 0x1b, scan: 0x01, extended: false, ctrl: false, shift: false };

/// Thread-local keyboard snapshot with controlled Ctrl and Shift, restored on drop.
struct ThreadKeyboardState([u8; 256]);

impl ThreadKeyboardState {
    fn controlled(shift: bool, ctrl: bool) -> Result<Self, String> {
        let mut original = [0; 256];
        // SAFETY: both snapshots are complete arrays; SetKeyboardState affects only this test thread.
        unsafe { GetKeyboardState(&mut original) }.map_err(|error| error.to_string())?;
        let restore = Self(original);
        let mut controlled = original;
        for virtual_key in [0x10, 0xa0, 0xa1, 0x11, 0xa2, 0xa3, 0x12, 0xa4, 0xa5] {
            controlled[virtual_key] &= 0x7f;
        }
        controlled[0x10] |= u8::from(shift) << 7;
        controlled[0xa0] |= u8::from(shift) << 7;
        controlled[0x11] |= u8::from(ctrl) << 7;
        controlled[0xa2] |= u8::from(ctrl) << 7;
        // SAFETY: `restore` owns the exact original thread snapshot before the controlled one is installed.
        unsafe { SetKeyboardState(&controlled) }.map_err(|error| error.to_string())?;
        Ok(restore)
    }
}

// Lifecycle: ThreadKeyboardState restores the test thread's keyboard snapshot on every exit, including unwind.
impl Drop for ThreadKeyboardState {
    fn drop(&mut self) {
        if let Err(error) =
            // SAFETY: the saved array is the full thread keyboard snapshot captured by this guard.
            unsafe { SetKeyboardState(&self.0) }
        {
            eprintln!("failed to restore test thread keyboard state: {error}");
        }
    }
}

type Check = fn(&mut Session, &ActiveEventLoop) -> Result<(), String>;

/// One real renderer call that must invalidate presented field geometry.
type Transition = Box<dyn Fn(&mut GpuRenderer)>;

enum Step {
    Key(Stroke),
    Check(&'static str, Check),
}

struct Session {
    app: App,
    main_window: Arc<Window>,
    child_window: Arc<Window>,
    main: WindowId,
    child: WindowId,
    /// Whether keys and field checks target the child window.
    key_child: bool,
    font_family: String,
    font_size: f32,
    line_height: f32,
    weight_scale: f32,
    steps: VecDeque<Step>,
    in_flight: Option<Stroke>,
    /// Controlled modifier snapshot of the in-flight key, held until its release is forwarded.
    held_modifiers: Option<ThreadKeyboardState>,
    /// Whether the in-flight key's `WM_KEYUP` was posted.
    released_sent: bool,
    step_deadline: Instant,
    deadline: Instant,
}

fn hwnd(window: &Window) -> Result<HWND, String> {
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("window has no Win32 handle".into());
    };
    Ok(HWND(handle.hwnd.get() as *mut _))
}

fn long_query() -> String {
    format!("{}▏{}", "w".repeat(60), "q".repeat(59))
}

impl Session {
    fn field_id(&self) -> WindowId {
        if self.key_child {
            self.child
        } else {
            self.main
        }
    }

    fn field_window(&self) -> &Arc<Window> {
        if self.key_child {
            &self.child_window
        } else {
            &self.main_window
        }
    }

    /// Post one key-down with controlled modifiers; the snapshot stays installed until release.
    fn post_down(&mut self, stroke: Stroke) -> Result<(), String> {
        let target = hwnd(self.field_window())?;
        self.held_modifiers = Some(ThreadKeyboardState::controlled(stroke.shift, stroke.ctrl)?);
        self.released_sent = false;
        // SAFETY: the session retains the destination HWND; the message carries only scalar key data.
        unsafe {
            PostMessageW(
                Some(target),
                WM_KEYDOWN,
                WPARAM(usize::from(stroke.virtual_key)),
                stroke.lparam(true),
            )
        }
        .map_err(|error| error.to_string())
    }

    /// Post the key-up of the in-flight key after its press was forwarded.
    fn post_up(&mut self, stroke: Stroke) -> Result<(), String> {
        let target = hwnd(self.field_window())?;
        self.released_sent = true;
        // SAFETY: the session retains the destination HWND; the message carries only scalar key data.
        unsafe {
            PostMessageW(
                Some(target),
                WM_KEYUP,
                WPARAM(usize::from(stroke.virtual_key)),
                stroke.lparam(false),
            )
        }
        .map_err(|error| error.to_string())
    }

    fn state(&self) -> Result<(String, Option<std::ops::Range<usize>>, usize), String> {
        self.app
            .__test_field_state(self.field_id())
            .ok_or_else(|| "no field owns the key window".into())
    }

    fn caret(&self) -> Result<FieldRect, String> {
        self.app
            .__test_field_caret_rect(self.field_id())
            .ok_or_else(|| "the field caret is not presented".into())
    }
}

fn render(app: &mut App, active: &ActiveEventLoop, id: WindowId) {
    assert!(app.__test_set_window_last_render(id, Instant::now() - Duration::from_secs(1)));
    ApplicationHandler::window_event(app, active, id, WindowEvent::RedrawRequested);
}

fn pointer_at(app: &mut App, active: &ActiveEventLoop, id: WindowId, point: (f32, f32)) {
    ApplicationHandler::window_event(
        app,
        active,
        id,
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(f64::from(point.0), f64::from(point.1)),
        },
    );
}

fn button(app: &mut App, active: &ActiveEventLoop, id: WindowId, state: ElementState) {
    ApplicationHandler::window_event(
        app,
        active,
        id,
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left },
    );
}

fn modifiers(app: &mut App, active: &ActiveEventLoop, id: WindowId, state: ModifiersState) {
    ApplicationHandler::window_event(app, active, id, WindowEvent::ModifiersChanged(state.into()));
}

fn caret_center(caret: FieldRect) -> f32 {
    caret.y + caret.h * 0.5
}

/// IME area the redraw paths must publish for a presented caret, in whole physical pixels.
fn ime_area(caret: FieldRect) -> ((i32, i32), (u32, u32)) {
    (
        (caret.x.round() as i32, caret.y.round() as i32),
        (caret.w.ceil().max(1.0) as u32, caret.h.ceil().max(1.0) as u32),
    )
}

fn frame_row(app: &App, id: WindowId, row_y: u32) -> Result<Vec<[u8; 4]>, String> {
    (0..WINDOW_SIZE.0)
        .map(|pixel_x| {
            app.__test_window_software_frame_pixel_bgra(id, pixel_x, row_y)
                .ok_or_else(|| format!("no software pixel at ({pixel_x}, {row_y})"))
        })
        .collect()
}

/// Read one pixel the HWND shows through GDI, as COLORREF.
fn gdi_pixel(window: &Window, pixel_x: u32, pixel_y: u32) -> Result<u32, String> {
    let target = hwnd(window)?;
    let hdc =
        // SAFETY: `target` belongs to the live test window and its borrowed DC is released below.
        unsafe { GetDC(Some(target)) };
    if hdc.0.is_null() {
        return Err("GetDC returned null".into());
    }
    let native =
        // SAFETY: `hdc` is live and the coordinates are inside the window surface.
        unsafe { GetPixel(hdc, pixel_x as i32, pixel_y as i32) }.0;
    let _ =
        // SAFETY: `hdc` was borrowed from `target` above and is returned exactly once.
        unsafe { ReleaseDC(Some(target), hdc) };
    ensure!(native != CLR_INVALID, "GetPixel returned CLR_INVALID");
    Ok(native)
}

fn colorref(bgra: [u8; 4]) -> u32 {
    u32::from(bgra[2]) | (u32::from(bgra[1]) << 8) | (u32::from(bgra[0]) << 16)
}

/// After a key edit the IME anchor waits for a presented caret, then follows exactly that caret.
fn check_ime_follows_presented_caret(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let id = session.field_id();
    let pending = session.app.__test_field_ime_anchor(id);
    ensure!(
        pending == ("pending", None),
        "unpresented edit must suppress the terminal anchor: {pending:?}"
    );
    render(&mut session.app, active, id);
    let caret = session.caret()?;
    let anchor = session.app.__test_field_ime_anchor(id);
    ensure!(anchor == ("field", Some(ime_area(caret))), "IME anchor {anchor:?} vs caret {caret:?}");
    Ok(())
}

fn field_steps(
    steps: &mut VecDeque<Step>,
    setup: (&'static str, Check),
    teardown: (Stroke, &'static str, Check),
) {
    steps.push_back(Step::Check(setup.0, setup.1));
    steps.push_back(Step::Key(CTRL_A));
    steps.push_back(Step::Key(PASTE_KEY));
    steps.push_back(Step::Check("pasted", check_pasted));
    for _ in 0..5 {
        steps.push_back(Step::Key(SHIFT_LEFT));
    }
    steps.push_back(Step::Check("extended", check_extended));
    steps.push_back(Step::Key(COPY));
    steps.push_back(Step::Check("copied", check_copied));
    steps.push_back(Step::Key(TYPE_X));
    steps.push_back(Step::Check("typed", check_typed));
    steps.push_back(Step::Key(CTRL_A));
    steps.push_back(Step::Key(PASTE_KEY));
    steps.push_back(Step::Check("pointer", check_pointer));
    steps.push_back(Step::Key(teardown.0));
    steps.push_back(Step::Check(teardown.1, teardown.2));
}

fn plan() -> VecDeque<Step> {
    let mut steps = VecDeque::new();
    field_steps(
        &mut steps,
        ("setup commands", setup_commands),
        (ESCAPE, "teardown commands", teardown_commands),
    );
    field_steps(
        &mut steps,
        ("setup rename tab", setup_rename_tab),
        (ENTER, "teardown rename tab", teardown_rename_tab),
    );
    field_steps(
        &mut steps,
        ("setup rename window", setup_rename_window),
        (ENTER, "teardown rename window", teardown_rename_window),
    );
    steps.push_back(Step::Check("cancel setup", cancel_setup));
    steps.push_back(Step::Check("cancel by resize", cancel_by_resize));
    steps.push_back(Step::Check("cancel by IME", cancel_by_ime));
    steps.push_back(Step::Check("start drag before key", start_search_drag));
    steps.push_back(Step::Key(END));
    steps.push_back(Step::Check("cancel by key", cancel_by_key));
    steps.push_back(Step::Check("other window press", other_window_press));
    steps.push_back(Step::Check("rename tab closed", rename_tab_closed));
    field_steps(
        &mut steps,
        ("setup child search", setup_child_search),
        (ESCAPE, "teardown child search", teardown_child_search),
    );
    steps.push_back(Step::Check("lost release recovery", lost_release_recovery));
    steps.push_back(Step::Key(ESCAPE));
    steps.push_back(Step::Check("child search closed", child_search_closed));
    steps.push_back(Step::Check("renderer lifecycle", renderer_lifecycle));
    steps
}

fn renderer(
    window: Arc<Window>,
    active: &ActiveEventLoop,
    config: &Config,
    shared: Option<&App>,
    role: &'static str,
) -> Result<GpuRenderer, String> {
    let font_dirs = [std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../sonicterm-font/test-fonts/rec-mono")];
    let settings = RendererSettings {
        font_family: &config.font.family,
        font_dirs: &font_dirs,
        font_size: config.font.size,
        line_height_mult: config.font.line_height,
        font_weight_scale: config.font.effective_weight_scale(),
        subpixel_aa: config.font.subpixel_aa,
        padding: [0.0; 4],
        appearance: SurfaceAppearance {
            backdrop: config.appearance.backdrop,
            opacity: 1.0,
            scrollbar: config.appearance.scrollbar,
            panel_padding: 0.0,
            software_render_mode: SoftwareRenderMode::Force,
        },
        role,
        glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
    };
    let mut renderer = match shared {
        Some(app) => GpuRenderer::new_with_shared_context(
            window,
            active,
            &Theme::default(),
            settings,
            app.main_renderer().ok_or("main renderer")?.shared_context(),
        ),
        None => GpuRenderer::new(window, active, &Theme::default(), settings),
    }
    .map_err(|error| error.to_string())?;
    renderer.set_tab_bar_visible(true);
    renderer.set_cursor_blink(false);
    Ok(renderer)
}

fn open_window(active: &ActiveEventLoop, title: &str, left: i32) -> Result<Arc<Window>, String> {
    Ok(Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_visible(true)
                    .with_inner_size(PhysicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1))
                    .with_position(PhysicalPosition::new(left, 40))
                    .with_title(title),
            )
            .map_err(|error| error.to_string())?,
    ))
}

fn start(active: &ActiveEventLoop) -> Result<Session, String> {
    let mut config = Config::default();
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.locale = "en".into();
    // The fixture directory alone does not select it: the default family is the host's own face.
    config.font.family = "Rec Mono St.Helens".into();
    let keymap = Keymap {
        meta: Meta { name: "native-field-clipboard".into(), version: "1.0".into() },
        bindings: vec![
            Binding { keys: "ctrl+shift+c".into(), action: ActionWrapper(Action::CopyToClipboard) },
            Binding {
                keys: "ctrl+shift+v".into(),
                action: ActionWrapper(Action::PasteFromClipboard),
            },
        ],
    };
    let main_window = open_window(active, "SonicTerm native field clipboard (main)", 40)?;
    let main_renderer = renderer(main_window.clone(), active, &config, None, "native-field-main")?;
    let font = (
        config.font.family.clone(),
        config.font.size,
        config.font.line_height,
        config.font.effective_weight_scale(),
    );
    let mut app = App::new(Theme::default(), config.clone(), keymap);
    app.__test_seed_tab("main");
    app.__test_set_software_render_degrade(true);
    let main = app.__test_main_window_id().ok_or("seeded main window")?;
    ensure!(
        app.__test_attach_window_renderer(main, main_window.clone(), main_renderer),
        "attach main"
    );
    render(&mut app, active, main);
    let child = app.__test_seed_child_window(&["child"]);
    let child_window = open_window(active, "SonicTerm native field clipboard (child)", 40)?;
    let child_renderer =
        renderer(child_window.clone(), active, &config, Some(&app), "native-field-child")?;
    ensure!(
        app.__test_attach_window_renderer(child, child_window.clone(), child_renderer),
        "attach child"
    );
    render(&mut app, active, child);
    let now = Instant::now();
    Ok(Session {
        app,
        main_window,
        child_window,
        main,
        child,
        key_child: false,
        font_family: font.0,
        font_size: font.1,
        line_height: font.2,
        weight_scale: font.3,
        steps: plan(),
        in_flight: None,
        held_modifiers: None,
        released_sent: false,
        step_deadline: now,
        deadline: now + Duration::from_secs(120),
    })
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|text| (*text).to_string()))
        .unwrap_or_else(|| "non-string panic".into())
}

#[derive(Default)]
struct Probe {
    session: Option<Session>,
    outcome: Option<Result<(), String>>,
    completed: Vec<&'static str>,
}

impl Probe {
    fn finish(&mut self, active: &ActiveEventLoop, result: Result<(), String>) {
        self.outcome = Some(result);
        self.session.take();
        active.exit();
    }

    /// Run checks until the next key is sent, the plan ends, or a step fails.
    fn advance(&mut self, active: &ActiveEventLoop) -> Result<bool, String> {
        let session = self.session.as_mut().ok_or("no session")?;
        let now = Instant::now();
        if now >= session.deadline {
            return Err(format!("overall deadline; completed={:?}", self.completed));
        }
        if let Some(stroke) = session.in_flight {
            if now >= session.step_deadline {
                return Err(format!(
                    "key {stroke:?} never completed; completed={:?}",
                    self.completed
                ));
            }
            return Ok(false);
        }
        while let Some(step) = session.steps.pop_front() {
            match step {
                Step::Key(stroke) => {
                    session.in_flight = Some(stroke);
                    session.step_deadline = now + Duration::from_secs(5);
                    session.post_down(stroke)?;
                    return Ok(false);
                }
                Step::Check(name, check) => {
                    let result = catch_unwind(AssertUnwindSafe(|| check(session, active)))
                        .unwrap_or_else(|payload| Err(format!("panic: {}", panic_text(payload))));
                    result.map_err(|error| format!("{name}: {error}"))?;
                    println!("ok: {name}");
                    self.completed.push(name);
                }
            }
        }
        Ok(true)
    }
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        if self.session.is_some() || self.outcome.is_some() {
            return;
        }
        match catch_unwind(AssertUnwindSafe(|| start(active)))
            .unwrap_or_else(|payload| Err(format!("setup panic: {}", panic_text(payload))))
        {
            Ok(session) => self.session = Some(session),
            Err(error) => self.finish(active, Err(error)),
        }
    }

    fn window_event(&mut self, active: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(stroke) = session.in_flight else {
            return;
        };
        if id != session.field_window().id() {
            return;
        }
        let logical = session.field_id();
        let (press, release) = match &event {
            WindowEvent::KeyboardInput { event: key, is_synthetic: false, .. } => {
                let ours = key
                    .native_key_event()
                    .is_some_and(|native| native.virtual_key == stroke.virtual_key);
                (
                    ours && key.state == ElementState::Pressed,
                    ours && key.state == ElementState::Released,
                )
            }
            WindowEvent::ModifiersChanged(_) => (false, false),
            _ => return,
        };
        let forwarded = catch_unwind(AssertUnwindSafe(|| {
            ApplicationHandler::window_event(&mut session.app, active, logical, event);
        }));
        if let Err(payload) = forwarded {
            self.finish(active, Err(format!("key {stroke:?} panicked: {}", panic_text(payload))));
            return;
        }
        if press && !session.released_sent {
            // When: the press reached the App, release it; winit queued any WM_CHAR before this key-up.
            if let Err(error) = session.post_up(stroke) {
                self.finish(active, Err(error));
            }
        } else if release {
            // When: the release reached the App, restore the thread snapshot; winit reports the
            // modifier change itself on the next key, so the App mirrors winit's cached state.
            session.held_modifiers = None;
            session.in_flight = None;
        }
    }

    fn about_to_wait(&mut self, active: &ActiveEventLoop) {
        if self.session.is_none() {
            return;
        }
        match self.advance(active) {
            Ok(false) => active.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(5),
            )),
            Ok(true) => self.finish(active, Ok(())),
            Err(error) => self.finish(active, Err(error)),
        }
    }
}

/// All four fields edit, copy and paste through configured actions and native key
/// messages; pointer drags select with visible highlight pixels; cancelled drags
/// keep their events from the PTY; renderer getters follow only presented frames.
#[test]
fn windows_field_clipboard_pointer_and_presented_geometry() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe::default();
    event_loop.run_app(&mut probe).expect("native field event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}

#[path = "support/windows_field_clipboard_cases.rs"]
mod cases;
use cases::check_copied;
use cases::check_extended;
use cases::check_pasted;
use cases::check_pointer;
use cases::check_typed;
use cases::setup_child_search;
use cases::setup_commands;
use cases::setup_rename_tab;
use cases::setup_rename_window;
use cases::teardown_child_search;
use cases::teardown_commands;
use cases::teardown_rename_tab;
use cases::teardown_rename_window;
#[path = "support/windows_field_clipboard_lifecycle.rs"]
mod lifecycle;
use lifecycle::assert_swallowed;
use lifecycle::cancel_by_ime;
use lifecycle::cancel_by_key;
use lifecycle::cancel_by_resize;
use lifecycle::cancel_setup;
use lifecycle::child_search_closed;
use lifecycle::lost_release_recovery;
use lifecycle::other_window_press;
use lifecycle::rename_tab_closed;
use lifecycle::renderer_lifecycle;
use lifecycle::start_search_drag;
