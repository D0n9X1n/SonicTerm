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

fn setup_commands(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    session.key_child = false;
    // The seeded logical main id differs from its HWND id, so `None` (main fallback) routes like production.
    session.app.__test_set_frontmost_window(None);
    session.app.__test_enable_pty_write_log();
    ensure!(session.app.run_action(&Action::OpenCommandPalette), "open palette");
    session.app.__test_set_memory_clipboard(PASTE);
    render(&mut session.app, active, session.main);
    ensure!(session.state()?.0.is_empty(), "commands query starts empty");
    Ok(())
}

fn setup_rename_tab(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    session.key_child = false;
    ensure!(session.app.run_action(&Action::RenameTab), "rename tab");
    session.app.__test_set_memory_clipboard(PASTE);
    render(&mut session.app, active, session.main);
    ensure!(!session.state()?.0.is_empty(), "rename tab seeds the active title body");
    Ok(())
}

fn setup_rename_window(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    session.key_child = false;
    ensure!(session.app.run_action(&Action::RenameWindow), "rename window");
    session.app.__test_set_memory_clipboard(PASTE);
    render(&mut session.app, active, session.main);
    ensure!(session.state()?.0.is_empty(), "rename window starts empty");
    Ok(())
}

fn setup_child_search(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    session.key_child = true;
    session.app.__test_enable_pty_write_log();
    ensure!(session.app.run_action_for_window(&Action::EnterCopyMode, session.child), "copy mode");
    ensure!(session.app.__test_child_read_only(session.child) == Some(true), "child is READONLY");
    session.app.run_action_for_window(&Action::OpenSearch, session.child);
    ensure!(
        session.app.__test_field_state(session.child).is_some(),
        "OpenSearch must open search in the READONLY child"
    );
    session.app.__test_set_memory_clipboard(PASTE);
    render(&mut session.app, active, session.child);
    ensure!(session.state()?.0.is_empty(), "child search starts empty");
    Ok(())
}

fn check_pasted(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    let state = session.state()?;
    ensure!(
        state == (PASTE.to_string(), None, PASTE.len()),
        "paste replaced the selection: {state:?}"
    );
    check_ime_follows_presented_caret(session, active)
}

fn check_extended(session: &mut Session, _: &ActiveEventLoop) -> Result<(), String> {
    let state = session.state()?;
    ensure!(state.1 == Some(5..PASTE.len()), "Shift+Left selects back over the ▏: {state:?}");
    Ok(())
}

fn check_copied(session: &mut Session, _: &ActiveEventLoop) -> Result<(), String> {
    let copied = session.app.__test_memory_clipboard();
    ensure!(copied.as_deref() == Some("▏beta"), "copy writes only the selection: {copied:?}");
    Ok(())
}

fn check_typed(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    let state = session.state()?;
    ensure!(state == ("alphax".to_string(), None, 6), "typing replaced the selection: {state:?}");
    check_ime_follows_presented_caret(session, active)?;
    session.app.__test_set_memory_clipboard(&long_query());
    Ok(())
}

/// Long-query clipping, reverse/forward pointer drags, Shift-extend, and highlight pixels.
fn check_pointer(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    // Reuse one screen position: hosted desktops need not fit two windows side by side.
    // Raise only this test HWND without stealing keyboard focus before reading visible GDI pixels.
    let target = hwnd(session.field_window())?;
    // SAFETY: target is owned by the live test session; flags preserve its bounds and activation.
    unsafe {
        SetWindowPos(
            target,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    }
    .map_err(|error| format!("show field for native pixel verification: {error}"))?;
    let id = session.field_id();
    // The last key held Ctrl+Shift; pointer presses below must not read it as Shift-extend.
    modifiers(&mut session.app, active, id, ModifiersState::empty());
    let long = long_query();
    ensure!(session.state()?.0 == long, "long paste: {:?}", session.state()?);
    render(&mut session.app, active, id);
    let end = session.caret()?;
    let width = WINDOW_SIZE.0 as f32;
    ensure!(end.x >= 0.0 && end.x + end.w <= width, "long query caret is clipped inside: {end:?}");
    let row_y = caret_center(end);
    // This sample stays strictly between the drag point and the anchor on every field.
    let reverse_sample = (end.x - 60.0) as u32;
    let before_reverse = session
        .app
        .__test_window_software_frame_pixel_bgra(id, reverse_sample, row_y as u32)
        .ok_or("reverse sample pixel")?;

    // Reverse drag at a fixed visible point: redraw must not move text under it.
    let press = (end.x - 1.0, row_y);
    let Some(FieldHit::Offset(press_offset)) =
        session.app.__test_field_hit(id, press, FieldHitMode::Press)
    else {
        return Err(format!("press at {press:?} misses the presented field"));
    };
    pointer_at(&mut session.app, active, id, press);
    button(&mut session.app, active, id, ElementState::Pressed);
    ensure!(session.app.__test_field_pointer_capture() == Some((id, true)), "press starts a drag");
    let fixed_point = (end.x - 120.0, row_y);
    let fixed_hit = session.app.__test_field_hit(id, fixed_point, FieldHitMode::Drag);
    pointer_at(&mut session.app, active, id, fixed_point);
    let (_, reverse, caret) = session.state()?;
    let Some(reverse) = reverse else {
        return Err("reverse drag selected nothing".into());
    };
    ensure!(
        reverse.end == press_offset && reverse.start == caret,
        "reverse keeps the press anchor"
    );
    ensure!(reverse.start > 0, "a long query's left edge is clipped, not offset 0: {reverse:?}");
    ensure!(long.is_char_boundary(reverse.start), "drag offsets stay on char boundaries");
    for _ in 0..4 {
        render(&mut session.app, active, id);
        let hit = session.app.__test_field_hit(id, fixed_point, FieldHitMode::Drag);
        ensure!(hit == fixed_hit, "reverse redraw moves the fixed hit: {fixed_hit:?} -> {hit:?}");
        pointer_at(&mut session.app, active, id, fixed_point);
        ensure!(session.state()?.1 == Some(reverse.clone()), "stationary drag changes selection");
    }
    let reverse_pixel = session
        .app
        .__test_window_software_frame_pixel_bgra(id, reverse_sample, row_y as u32)
        .ok_or("selected reverse sample pixel")?;
    let reverse_native = gdi_pixel(session.field_window(), reverse_sample, row_y as u32)?;
    ensure!(reverse_pixel != before_reverse && reverse_native == colorref(reverse_pixel),
        "fixed reverse highlight not visible: before={before_reverse:?}, selected={reverse_pixel:?}, HWND={reverse_native:#08x}");
    button(&mut session.app, active, id, ElementState::Released);
    ensure!(session.app.__test_field_pointer_capture().is_none(), "release ends the drag");
    println!("reverse fixed[{id:?}]: hit={fixed_hit:?} selection={reverse:?} sample=({reverse_sample},{row_y}) HWND={reverse_native:#08x}");
    // Highlight proof at one query, caret and scroll: select leftward of the caret, which
    // stays on screen, then compare with the same caret collapsed. Only the selection differs.
    render(&mut session.app, active, id);
    let anchor_caret = session.caret()?;
    let caret_offset = session.state()?.2;
    let mut tried = Vec::new();
    let mut highlight_press = None;
    for delta in [200.0, 160.0, 120.0, 80.0] {
        let point = (anchor_caret.x - delta, row_y);
        let hit = session.app.__test_field_hit(id, point, FieldHitMode::Press);
        tried.push((delta, hit));
        if let Some(FieldHit::Offset(offset)) = hit {
            if offset + 4 <= caret_offset {
                highlight_press = Some((point, offset));
                break;
            }
        }
    }
    let Some((highlight_point, highlight_start)) = highlight_press else {
        return Err(format!("no on-screen point left of caret {anchor_caret:?}: {tried:?}"));
    };
    pointer_at(&mut session.app, active, id, highlight_point);
    button(&mut session.app, active, id, ElementState::Pressed);
    pointer_at(&mut session.app, active, id, (anchor_caret.x + 0.5, row_y));
    button(&mut session.app, active, id, ElementState::Released);
    let selected_state = session.state()?;
    ensure!(
        selected_state.1 == Some(highlight_start..caret_offset) && selected_state.2 == caret_offset,
        "on-screen selection {highlight_start}..{caret_offset}: {selected_state:?}"
    );
    render(&mut session.app, active, id);
    let selected_caret = session.caret()?;
    let selected = frame_row(&session.app, id, row_y as u32)?;
    let native_selected: Vec<u32> = (0..WINDOW_SIZE.0)
        .map(|pixel_x| gdi_pixel(session.field_window(), pixel_x, row_y as u32))
        .collect::<Result<_, _>>()?;
    ensure!(session.app.__test_collapse_field_selection(id), "collapse keeps the caret");
    let collapsed_state = session.state()?;
    ensure!(
        collapsed_state.1.is_none() && collapsed_state.2 == caret_offset,
        "collapsed at the caret: {collapsed_state:?}"
    );
    render(&mut session.app, active, id);
    ensure!(session.caret()? == selected_caret, "collapse kept caret and scroll unchanged");
    let collapsed_row = frame_row(&session.app, id, row_y as u32)?;
    let changed: Vec<u32> = (0..WINDOW_SIZE.0)
        .filter(|&pixel_x| selected[pixel_x as usize] != collapsed_row[pixel_x as usize])
        .collect();
    ensure!(changed.len() >= 40, "selection highlight changed only {} pixels", changed.len());
    let (span_left, span_right) =
        (highlight_point.0 - 20.0, selected_caret.x + selected_caret.w + 2.0);
    ensure!(
        changed.iter().all(|&pixel_x| (span_left..=span_right).contains(&(pixel_x as f32))),
        "highlight pixels outside {span_left}..{span_right}: {changed:?}"
    );
    let sample = changed[changed.len() / 2];
    let native_collapsed = gdi_pixel(session.field_window(), sample, row_y as u32)?;
    let native = native_selected[sample as usize];
    ensure!(
        native == colorref(selected[sample as usize])
            && native_collapsed == colorref(collapsed_row[sample as usize])
            && native != native_collapsed,
        "HWND ({sample}, {row_y}) selected={native:#08x} collapsed={native_collapsed:#08x}"
    );
    let native_matches = changed
        .iter()
        .filter(|&&pixel_x| {
            native_selected[pixel_x as usize] == colorref(selected[pixel_x as usize])
        })
        .count();
    ensure!(
        native_matches == changed.len(),
        "HWND shows {native_matches}/{} highlight px",
        changed.len()
    );

    // Outside-edge motion remains stable through redraws instead of auto-scrolling.
    let start = session.caret()?;
    pointer_at(&mut session.app, active, id, (start.x + 1.0, row_y));
    button(&mut session.app, active, id, ElementState::Pressed);
    for outside_x in [-100.0, width + 100.0] {
        let point = (outside_x, row_y);
        let expected = session.app.__test_field_hit(id, point, FieldHitMode::Drag);
        pointer_at(&mut session.app, active, id, point);
        let state = session.state()?;
        for _ in 0..4 {
            render(&mut session.app, active, id);
            ensure!(
                session.app.__test_field_hit(id, point, FieldHitMode::Drag) == expected,
                "outside edge {outside_x} moved after redraw"
            );
            pointer_at(&mut session.app, active, id, point);
            ensure!(session.state()? == state, "outside edge {outside_x} changed the selection");
        }
    }
    button(&mut session.app, active, id, ElementState::Released);

    // Forward drag: return to the visible interior anchor, then drag past the right edge.
    let press = (start.x + 1.0, caret_center(start));
    let Some(FieldHit::Offset(forward_anchor)) =
        session.app.__test_field_hit(id, press, FieldHitMode::Press)
    else {
        return Err(format!("forward press at {press:?} misses the field"));
    };
    pointer_at(&mut session.app, active, id, press);
    button(&mut session.app, active, id, ElementState::Pressed);
    pointer_at(&mut session.app, active, id, (width - 2.0, press.1));
    let forward = session.state()?.1;
    ensure!(
        forward
            .as_ref()
            .is_some_and(|range| range.start == forward_anchor && range.end > forward_anchor),
        "forward drag selects from its press: {forward:?} anchor={forward_anchor}"
    );
    button(&mut session.app, active, id, ElementState::Released);

    // Shift+press extends from the caret instead of collapsing it.
    pointer_at(&mut session.app, active, id, press);
    button(&mut session.app, active, id, ElementState::Pressed);
    button(&mut session.app, active, id, ElementState::Released);
    render(&mut session.app, active, id);
    let collapsed = session.state()?;
    ensure!(collapsed.1.is_none(), "a plain click collapses: {collapsed:?}");
    let caret = session.caret()?;
    // Pick a presented point on either side of the caret that maps to a different offset.
    let mut tried = Vec::new();
    let mut found = None;
    for delta in [60.0, -60.0, 30.0, -30.0, 120.0, -120.0] {
        let point = (caret.x + delta, caret_center(caret));
        let hit = session.app.__test_field_hit(id, point, FieldHitMode::Press);
        tried.push((delta, hit));
        if let Some(FieldHit::Offset(offset)) = hit {
            if offset != collapsed.2 {
                found = Some((point, offset));
                break;
            }
        }
    }
    let Some((extend_point, extend_to)) = found else {
        return Err(format!("no Shift press point near caret {caret:?}: {tried:?}"));
    };
    modifiers(&mut session.app, active, id, ModifiersState::SHIFT);
    pointer_at(&mut session.app, active, id, extend_point);
    button(&mut session.app, active, id, ElementState::Pressed);
    button(&mut session.app, active, id, ElementState::Released);
    modifiers(&mut session.app, active, id, ModifiersState::empty());
    let extended = session.state()?.1;
    ensure!(
        extended == Some(collapsed.2.min(extend_to)..collapsed.2.max(extend_to))
            && extend_to != collapsed.2,
        "Shift press extends from caret {} to {extend_to}: {extended:?}",
        collapsed.2
    );
    println!(
        "pointer[{id:?}]: reverse={reverse:?} forward={forward:?} shift={extended:?} \
         highlight_px={} gdi_sample=({sample},{row_y})={native:#08x}/{native_collapsed:#08x}",
        changed.len()
    );
    Ok(())
}

fn teardown_commands(session: &mut Session, _: &ActiveEventLoop) -> Result<(), String> {
    ensure!(!session.app.__test_palette_open(), "native Escape closes the palette");
    ensure!(session.app.__test_pty_write_log().is_empty(), "field keys never reach the PTY");
    Ok(())
}

fn teardown_rename_tab(session: &mut Session, _: &ActiveEventLoop) -> Result<(), String> {
    ensure!(!session.app.__test_palette_open(), "Enter commits the rename");
    let title = session
        .app
        .main_tabs()
        .and_then(|tabs| tabs.active())
        .and_then(|tab| tab.custom_title.clone());
    ensure!(title == Some(long_query()), "tab title keeps the literal ▏: {title:?}");
    ensure!(session.app.__test_pty_write_log().is_empty(), "field keys never reach the PTY");
    Ok(())
}

fn teardown_rename_window(session: &mut Session, _: &ActiveEventLoop) -> Result<(), String> {
    ensure!(!session.app.__test_palette_open(), "Enter commits the window name");
    let name = session.app.__test_window_custom_name(session.main).map(str::to_owned);
    ensure!(name == Some(long_query()), "window name keeps the literal ▏: {name:?}");
    ensure!(session.app.__test_pty_write_log().is_empty(), "field keys never reach the PTY");
    Ok(())
}

fn teardown_child_search(session: &mut Session, _: &ActiveEventLoop) -> Result<(), String> {
    ensure!(session.app.__test_field_state(session.child).is_none(), "native Escape closes search");
    ensure!(session.app.__test_child_read_only(session.child) == Some(true), "READONLY is kept");
    ensure!(
        session.app.__test_pty_write_log().is_empty(),
        "the READONLY child attempted no PTY write"
    );
    Ok(())
}

/// Main search over a pane with all-motion mouse reporting, with a positive control.
fn cancel_setup(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    session.key_child = false;
    ensure!(session.app.__test_set_main_search_query("needle"), "main search opens");
    let index = session.app.main_tabs().ok_or("main tabs")?.active_index();
    let pane = session.app.__test_active_pane_in_tab(index).ok_or("main pane")?;
    ensure!(
        session.app.__test_advance_pane_parser(pane, b"\x1b[?1003h\x1b[?1006h"),
        "mouse tracking"
    );
    render(&mut session.app, active, session.main);
    session.app.__test_enable_pty_write_log();
    pointer_at(&mut session.app, active, session.main, TERMINAL_POINT);
    ensure!(
        !session.app.__test_pty_write_log().is_empty(),
        "positive control: unowned motion reports to the mouse-aware pane"
    );
    Ok(())
}

fn start_search_drag(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    let main = session.main;
    render(&mut session.app, active, main);
    let caret = session.caret()?;
    pointer_at(&mut session.app, active, main, (caret.x - 1.0, caret_center(caret)));
    // Hover before the press is ordinary unowned motion; only the press onward is the field's.
    session.app.__test_enable_pty_write_log();
    button(&mut session.app, active, main, ElementState::Pressed);
    ensure!(session.app.__test_field_pointer_capture() == Some((main, true)), "search drag starts");
    let log = session.app.__test_pty_write_log();
    ensure!(log.is_empty(), "the field press is not reported: {log:?}");
    Ok(())
}

fn assert_swallowed(
    session: &mut Session,
    active: &ActiveEventLoop,
    cause: &str,
) -> Result<(), String> {
    let main = session.main;
    ensure!(
        session.app.__test_field_pointer_capture() == Some((main, false)),
        "{cause} cancels the drag"
    );
    pointer_at(&mut session.app, active, main, TERMINAL_POINT);
    ApplicationHandler::window_event(
        &mut session.app,
        active,
        main,
        WindowEvent::CursorLeft { device_id: DeviceId::dummy() },
    );
    ensure!(session.state()?.1.is_none(), "after {cause}, motion never selects");
    ensure!(
        session.app.__test_pty_write_log().is_empty(),
        "after {cause}, motion and leave are swallowed"
    );
    button(&mut session.app, active, main, ElementState::Released);
    ensure!(
        session.app.__test_field_pointer_capture().is_none(),
        "the paired release ends the swallow"
    );
    ensure!(
        session.app.__test_pty_write_log().is_empty(),
        "after {cause}, the release is swallowed"
    );
    pointer_at(&mut session.app, active, main, (TERMINAL_POINT.0 + 8.0, TERMINAL_POINT.1));
    ensure!(!session.app.__test_pty_write_log().is_empty(), "after {cause}, reporting resumes");
    Ok(())
}

fn cancel_by_resize(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    start_search_drag(session, active)?;
    let size = PhysicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1);
    ApplicationHandler::window_event(
        &mut session.app,
        active,
        session.main,
        WindowEvent::Resized(size),
    );
    assert_swallowed(session, active, "resize")
}

fn cancel_by_ime(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    start_search_drag(session, active)?;
    ApplicationHandler::window_event(
        &mut session.app,
        active,
        session.main,
        WindowEvent::Ime(Ime::Enabled),
    );
    assert_swallowed(session, active, "synthetic IME event")
}

fn cancel_by_key(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    assert_swallowed(session, active, "native End key")
}

/// A press in the child does not clear or steal main's field drag.
fn other_window_press(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    let (main, child) = (session.main, session.child);
    start_search_drag(session, active)?;
    pointer_at(&mut session.app, active, child, TERMINAL_POINT);
    button(&mut session.app, active, child, ElementState::Pressed);
    ensure!(
        session.app.__test_field_pointer_capture() == Some((main, true)),
        "child press keeps main's drag"
    );
    let caret = session.caret()?;
    pointer_at(&mut session.app, active, main, (2.0, caret_center(caret)));
    ensure!(session.state()?.1.is_some(), "main motion still extends its selection");
    button(&mut session.app, active, main, ElementState::Released);
    ensure!(session.app.__test_field_pointer_capture().is_none(), "main keeps its paired release");
    button(&mut session.app, active, child, ElementState::Released);
    ensure!(session.app.__test_pty_write_log().is_empty(), "no pointer half reached main's PTY");
    Ok(())
}

/// A cancelled main drag whose release never arrives does not block the child's field;
/// main's late release is still consumed once and then reporting resumes.
fn lost_release_recovery(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    let (main, child) = (session.main, session.child);
    session.key_child = false;
    start_search_drag(session, active)?;
    ApplicationHandler::window_event(&mut session.app, active, main, WindowEvent::Focused(false));
    ensure!(session.app.__test_field_owes_release(main), "focus loss leaves main a release debt");
    ensure!(session.app.__test_set_child_search_query(child, "needle"), "child search");
    session.key_child = true;
    render(&mut session.app, active, child);
    let caret = session.caret()?;
    pointer_at(&mut session.app, active, child, (caret.x - 1.0, caret_center(caret)));
    button(&mut session.app, active, child, ElementState::Pressed);
    ensure!(
        session.app.__test_field_pointer_capture() == Some((child, true)),
        "the child field gesture starts despite main's debt"
    );
    pointer_at(&mut session.app, active, child, (2.0, caret_center(caret)));
    ensure!(session.state()?.1.is_some(), "the child drag selects");
    button(&mut session.app, active, child, ElementState::Released);
    ensure!(session.app.__test_field_pointer_capture().is_none(), "the child release ends it");
    ensure!(session.app.__test_field_owes_release(main), "main's debt survives the child gesture");
    session.key_child = false;
    pointer_at(&mut session.app, active, main, TERMINAL_POINT);
    ensure!(session.app.__test_pty_write_log().is_empty(), "main motion while owed is swallowed");
    button(&mut session.app, active, main, ElementState::Released);
    ensure!(!session.app.__test_field_owes_release(main), "the late release pays the debt");
    ensure!(session.app.__test_pty_write_log().is_empty(), "the late release is consumed");
    pointer_at(&mut session.app, active, main, (TERMINAL_POINT.0 + 8.0, TERMINAL_POINT.1));
    ensure!(!session.app.__test_pty_write_log().is_empty(), "main reporting resumes");
    // The next native Escape closes the child search.
    session.key_child = true;
    Ok(())
}

fn child_search_closed(session: &mut Session, _: &ActiveEventLoop) -> Result<(), String> {
    ensure!(session.app.__test_field_state(session.child).is_none(), "native Escape closes search");
    session.key_child = false;
    Ok(())
}

/// Closing the captured rename tab mid-drag leaves the sibling tab unchanged.
fn rename_tab_closed(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    let main = session.main;
    session.app.__test_seed_tab("peer");
    session.app.__test_set_frontmost_window(None);
    render(&mut session.app, active, main);
    ensure!(session.app.run_action(&Action::RenameTab), "rename peer");
    ensure!(!session.app.__test_palette_query().is_empty(), "rename edits the active peer tab");
    render(&mut session.app, active, main);
    let caret = session.caret()?;
    let row_y = caret_center(caret);
    pointer_at(&mut session.app, active, main, (caret.x - 1.0, row_y));
    button(&mut session.app, active, main, ElementState::Pressed);
    ensure!(session.app.__test_field_pointer_capture() == Some((main, true)), "rename drag starts");
    let sibling = session.app.main_tabs().ok_or("tabs")?.tabs()[0].clone();
    ensure!(session.app.run_action(&Action::CloseTab), "close the captured tab");
    ensure!(session.app.main_tabs().ok_or("tabs")?.len() == 1, "the sibling tab remains");
    pointer_at(&mut session.app, active, main, (2.0, row_y));
    ensure!(
        session.app.__test_field_pointer_capture() == Some((main, false)),
        "the orphan drag is swallowed"
    );
    button(&mut session.app, active, main, ElementState::Released);
    ensure!(session.app.__test_field_pointer_capture().is_none(), "release consumed");
    if session.app.__test_palette_open() {
        // An orphaned editor submits nothing.
        session.app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    }
    let remaining = session.app.main_tabs().ok_or("tabs")?.tabs()[0].clone();
    ensure!(
        (remaining.id, &remaining.custom_title, &remaining.title)
            == (sibling.id, &sibling.custom_title, &sibling.title),
        "the sibling was not renamed: {:?} -> {:?}",
        sibling.custom_title,
        remaining.custom_title
    );
    ensure!(session.app.__test_pty_write_log().is_empty(), "no rename gesture reached the PTY");
    Ok(())
}

/// Renderer getters answer only for the presented frame across every invalidating transition.
fn renderer_lifecycle(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    let main = session.main;
    session.key_child = false;
    ensure!(session.app.run_action(&Action::OpenCommandPalette), "open palette");
    session.app.__test_set_palette_query("lifecycle");
    render(&mut session.app, active, main);
    let caret = session.caret()?;
    let point = (caret.x - 1.0, caret_center(caret));
    let hit = session.app.__test_field_hit(main, point, FieldHitMode::Press);
    ensure!(matches!(hit, Some(FieldHit::Offset(_))), "presented hit: {hit:?}");
    session.app.__test_set_palette_query("lifecycle2");
    ensure!(
        session.app.__test_field_caret_rect(main).is_none(),
        "edited query: caret waits for a frame"
    );
    let stale = session.app.__test_field_hit(main, point, FieldHitMode::Press);
    ensure!(stale == Some(FieldHit::Stale), "edited query: hit is stale: {stale:?}");
    check_ime_follows_presented_caret(session, active)?;

    let (family, size, line_height, weight) =
        (session.font_family.clone(), session.font_size, session.line_height, session.weight_scale);
    let transitions: [(&str, Transition); 4] = [
        (
            "set_font",
            Box::new(move |renderer| renderer.set_font(&family, size + 2.0, line_height, weight)),
        ),
        (
            "try_resize",
            Box::new(|renderer| {
                assert!(
                    renderer.try_resize(WINDOW_SIZE.0 - 40, WINDOW_SIZE.1 - 40),
                    "resize applies"
                );
            }),
        ),
        ("set_scale_factor", Box::new(|renderer| renderer.set_scale_factor(1.25))),
        (
            "set_panel_padding",
            Box::new(|renderer| {
                assert!(renderer.set_panel_padding(12.0), "padding changes");
            }),
        ),
    ];
    for (name, transition) in &transitions {
        let renderer = session.app.__test_window_renderer_mut(main).ok_or("main renderer")?;
        transition(renderer);
        ensure!(
            session.app.__test_field_caret_rect(main).is_none(),
            "{name}: caret invalid until presented"
        );
        let anchor = session.app.__test_field_ime_anchor(main);
        ensure!(anchor == ("pending", None), "{name}: IME waits, never the terminal: {anchor:?}");
        render(&mut session.app, active, main);
        ensure!(
            session.app.__test_field_caret_rect(main).is_some(),
            "{name}: the next presented frame restores it"
        );
    }

    // A frame that does not present never commits its proposed geometry.
    let renderer = session.app.__test_window_renderer_mut(main).ok_or("main renderer")?;
    let presented = renderer.successful_frame_count();
    renderer.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    let after_fault = session.app.__test_field_caret_rect(main);
    session.app.__test_set_palette_query("lifecycle3");
    render(&mut session.app, active, main);
    let count = session
        .app
        .__test_window_renderer_mut(main)
        .ok_or("main renderer")?
        .successful_frame_count();
    ensure!(count == presented, "a stopped device presents no frame: {presented} -> {count}");
    let unpresented = session.app.__test_field_caret_rect(main);
    ensure!(unpresented.is_none(), "an unpresented frame committed geometry: {unpresented:?}");
    let anchor = session.app.__test_field_ime_anchor(main);
    ensure!(anchor == ("pending", None), "unpresented frame: IME waits: {anchor:?}");
    println!("lifecycle: caret right after DestroyDevice (before any edit) = {after_fault:?}");
    ensure!(
        after_fault.is_none(),
        "a stopped device must invalidate the presented caret: {after_fault:?}"
    );
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
    let font_dirs =
        [std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
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
