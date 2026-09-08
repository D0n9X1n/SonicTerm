#![cfg(target_os = "windows")]

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::{Config, SoftwareRenderMode},
    keymap::{Action, ActionWrapper, Binding, Keymap, Meta},
    theme::Theme,
};
use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{DeviceId, ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    platform::windows::EventLoopBuilderExtWindows,
    window::{Window, WindowId},
};

struct Probe {
    outcome: Option<Result<(), String>>,
    native_keys: Option<NativeKeys>,
}

#[derive(Clone, Copy)]
enum KeyStage {
    NormalEnter,
    ReadOnlyDown,
    ReadOnlyEnter,
    CloseSelector,
    BlockedCommand,
    ExitReadOnly,
}

struct NativeKeys {
    app: App,
    child_window: Arc<Window>,
    child: WindowId,
    overflow: sonicterm_ui::tabbar_view::Rect,
    first_pane: u64,
    second_pane: u64,
    stage: KeyStage,
    deadline: Instant,
}

impl NativeKeys {
    fn post_key(&self, virtual_key: usize, scan: u32, extended: bool) -> Result<(), String> {
        use windows::Win32::{
            Foundation::{LPARAM, WPARAM},
            UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP},
        };
        let handle = self.child_window.window_handle().map_err(|error| error.to_string())?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return Err("child has no Win32 handle".into());
        };
        let hwnd = HWND(handle.hwnd.get() as *mut _);
        let flags = 1 | (scan << 16) | (u32::from(extended) << 24);
        // SAFETY: The child Arc retains this HWND; both messages carry only scalar key data to this test window.
        unsafe {
            PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(virtual_key), LPARAM(flags as isize))
                .map_err(|error| error.to_string())?;
            PostMessageW(
                Some(hwnd),
                WM_KEYUP,
                WPARAM(virtual_key),
                LPARAM((flags | 0xc000_0000) as isize),
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn expected_key(&self) -> NamedKey {
        match self.stage {
            KeyStage::NormalEnter | KeyStage::ReadOnlyEnter => NamedKey::Enter,
            KeyStage::ReadOnlyDown => NamedKey::ArrowDown,
            KeyStage::CloseSelector | KeyStage::ExitReadOnly => NamedKey::Escape,
            KeyStage::BlockedCommand => NamedKey::F12,
        }
    }

    fn open_selector(&mut self, active: &ActiveEventLoop) {
        let point =
            (self.overflow.x + self.overflow.w * 0.5, self.overflow.y + self.overflow.h * 0.5);
        click(&mut self.app, active, self.child, point);
        assert!(self.app.__test_palette_open());
        assert_eq!(self.app.__test_palette_attached_window(), Some(self.child));
        render(&mut self.app, active, self.child);
    }

    fn advance(&mut self, active: &ActiveEventLoop) -> Result<bool, String> {
        match self.stage {
            KeyStage::NormalEnter => {
                assert!(
                    !self.app.__test_palette_open(),
                    "native non-READONLY Enter closes the selector"
                );
                assert_eq!(self.app.__test_child_active_pane(self.child), Some(self.first_pane));
                assert!(self.app.run_action_for_window(&Action::EnterCopyMode, self.child));
                assert_eq!(self.app.__test_child_read_only(self.child), Some(true));
                assert!(self.app.run_action_for_window(&Action::ActivateLastTab, self.child));
                self.open_selector(active);
                self.stage = KeyStage::ReadOnlyDown;
                self.post_key(0x28, 0x50, true)?;
            }
            KeyStage::ReadOnlyDown => {
                assert!(self.app.__test_palette_open());
                assert_eq!(self.app.__test_child_read_only(self.child), Some(true));
                self.stage = KeyStage::ReadOnlyEnter;
                self.post_key(0x0d, 0x1c, false)?;
            }
            KeyStage::ReadOnlyEnter => {
                assert!(
                    !self.app.__test_palette_open(),
                    "native Enter must reach the READONLY child's selector"
                );
                assert_eq!(self.app.__test_child_active_pane(self.child), Some(self.second_pane));
                assert_eq!(self.app.__test_child_read_only(self.child), Some(true));
                self.open_selector(active);
                self.stage = KeyStage::CloseSelector;
                self.post_key(0x1b, 0x01, false)?;
            }
            KeyStage::CloseSelector => {
                assert!(!self.app.__test_palette_open());
                assert_eq!(self.app.__test_child_read_only(self.child), Some(true));
                self.stage = KeyStage::BlockedCommand;
                self.post_key(0x7b, 0x58, false)?;
            }
            KeyStage::BlockedCommand => {
                assert_eq!(self.app.__test_child_tab_count(self.child), Some(18));
                assert_eq!(self.app.__test_child_read_only(self.child), Some(true));
                assert!(self.app.__test_pty_write_log().is_empty());
                self.stage = KeyStage::ExitReadOnly;
                self.post_key(0x1b, 0x01, false)?;
            }
            KeyStage::ExitReadOnly => {
                assert!(!self.app.__test_palette_open());
                assert_eq!(self.app.__test_child_read_only(self.child), Some(false));
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, active: &ActiveEventLoop) {
        if self.native_keys.is_some() || self.outcome.is_some() {
            return;
        }
        match run_probe(active) {
            Ok(mut native) => {
                native.app.__test_enable_pty_write_log();
                native.open_selector(active);
                if let Err(error) = native.post_key(0x0d, 0x1c, false) {
                    self.outcome = Some(Err(error));
                    active.exit();
                }
                self.native_keys = Some(native);
            }
            Err(error) => {
                self.outcome = Some(Err(error));
                active.exit();
            }
        }
    }

    fn window_event(&mut self, active: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(native) = self.native_keys.as_mut() else {
            return;
        };
        if id != native.child_window.id() {
            return;
        }
        let WindowEvent::KeyboardInput { event: key, .. } = &event else {
            return;
        };
        if key.logical_key != Key::Named(native.expected_key()) {
            return;
        }
        let released = key.state == ElementState::Released;
        ApplicationHandler::window_event(&mut native.app, active, native.child, event);
        if released {
            match native.advance(active) {
                Ok(false) => {}
                result => {
                    self.outcome = Some(result.map(|_| ()));
                    active.exit();
                }
            }
        }
    }

    fn about_to_wait(&mut self, active: &ActiveEventLoop) {
        if let Some(native) = self.native_keys.as_ref() {
            if Instant::now() >= native.deadline {
                self.outcome = Some(Err("native keyboard events exceeded their deadline".into()));
                active.exit();
            } else {
                active.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(native.deadline));
            }
        }
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
            position: winit::dpi::PhysicalPosition::new(f64::from(point.0), f64::from(point.1)),
        },
    );
}

fn pointer_button(app: &mut App, active: &ActiveEventLoop, id: WindowId, state: ElementState) {
    ApplicationHandler::window_event(
        app,
        active,
        id,
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left },
    );
}

fn click(app: &mut App, active: &ActiveEventLoop, id: WindowId, point: (f32, f32)) {
    pointer_at(app, active, id, point);
    pointer_button(app, active, id, ElementState::Pressed);
    pointer_button(app, active, id, ElementState::Released);
}

fn run_probe(active: &ActiveEventLoop) -> Result<NativeKeys, String> {
    let window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_visible(true)
                    .with_inner_size(PhysicalSize::new(900, 600))
                    .with_title("SonicTerm native palette shortcut regression"),
            )
            .map_err(|error| error.to_string())?,
    );
    let theme = Theme::default();
    let mut config = Config::default();
    config.appearance.software_render_mode = SoftwareRenderMode::Force;
    config.locale = "en".into();
    // Native text assertions require the shipped face, not a font installed only on the developer's machine.
    let font_dirs =
        [std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
    let keymap = Keymap {
        meta: Meta { name: "native-hint".into(), version: "1.0".into() },
        bindings: vec![
            Binding { keys: "ctrl+alt+y".into(), action: ActionWrapper(Action::ToggleTabBar) },
            Binding { keys: "f12".into(), action: ActionWrapper(Action::CloseTab) },
        ],
    };
    let mut renderer = GpuRenderer::new(
        window.clone(),
        active,
        &theme,
        RendererSettings {
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
            role: "native-palette-hint-test",
        },
    )
    .map_err(|error| error.to_string())?;
    renderer.set_tab_bar_visible(true);
    renderer.set_cursor_blink(false);
    let mut app = App::new(theme, config, keymap);
    app.__test_seed_tab("main");
    app.__test_set_software_render_degrade(true);
    let id = app.__test_main_window_id().unwrap();
    assert!(app.__test_attach_window_renderer(id, window.clone(), renderer));
    render(&mut app, active, id);
    assert!(app.main_renderer().unwrap().tab_bar_visible());
    let baseline = app.__test_window_software_frame_pixel_bgra(id, 450, 90).unwrap();
    let count = app.main_renderer().unwrap().successful_frame_count();

    assert!(app.run_action(&Action::OpenCommandPalette));
    for ch in "Ctrl+Alt+Y".chars() {
        assert!(app.__test_command_palette_handle_key(&Key::Character(ch.to_string().into())));
    }
    render(&mut app, active, id);
    assert!(app.__test_palette_open());
    assert_eq!(app.__test_palette_query(), "Ctrl+Alt+Y");
    assert!(app.main_renderer().unwrap().successful_frame_count() > count);
    let overlay = app.__test_window_software_frame_pixel_bgra(id, 450, 90).unwrap();
    if overlay == baseline {
        return Err(String::from("palette did not change the presented overlay pixel"));
    }
    let size = window.inner_size();
    let mut english = Vec::new();
    for y in 0..size.height {
        for x in 0..size.width {
            english.push(app.__test_window_software_frame_pixel_bgra(id, x, y).unwrap());
        }
    }
    let count = app.main_renderer().unwrap().successful_frame_count();
    app.set_locale("ja");
    assert_eq!(app.locale(), "ja");
    render(&mut app, active, id);
    assert!(
        app.main_renderer().unwrap().successful_frame_count() > count,
        "locale-only change must invalidate the retained palette frame"
    );
    assert_eq!(app.__test_palette_query(), "Ctrl+Alt+Y");
    let mut changed = None;
    for y in 0..size.height {
        for x in 0..size.width {
            let before = english[(y * size.width + x) as usize];
            let after = app.__test_window_software_frame_pixel_bgra(id, x, y).unwrap();
            if before != after {
                changed = Some((x, y, before, after));
                break;
            }
        }
        if changed.is_some() {
            break;
        }
    }
    let (x, y, before, after) =
        changed.ok_or_else(|| String::from("locale-only change did not update palette pixels"))?;
    let handle = window.window_handle().map_err(|error| error.to_string())?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err(String::from("window did not expose a Win32 handle"));
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let hdc =
        // SAFETY: `hwnd` belongs to the live test window and its borrowed DC is released below.
        unsafe { GetDC(Some(hwnd)) };
    if hdc.0.is_null() {
        return Err(String::from("GetDC returned null"));
    }
    let native =
        // SAFETY: `hdc` is live and the sampled coordinates came from inside the window surface.
        unsafe { GetPixel(hdc, x as i32, y as i32) }.0;
    let _ =
        // SAFETY: `hdc` was borrowed from `hwnd` above and is returned exactly once.
        unsafe { ReleaseDC(Some(hwnd), hdc) };
    assert_ne!(native, CLR_INVALID);
    let colorref =
        |bgra: [u8; 4]| u32::from(bgra[2]) | (u32::from(bgra[1]) << 8) | (u32::from(bgra[0]) << 16);
    assert_eq!(native, colorref(after), "translated pixels reached the HWND");
    assert_ne!(native, colorref(before));
    let count = app.main_renderer().unwrap().successful_frame_count();
    app.set_locale("ja");
    render(&mut app, active, id);
    assert_eq!(
        app.main_renderer().unwrap().successful_frame_count(),
        count,
        "identical locale refresh retains the frame-key fast path"
    );
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    render(&mut app, active, id);
    if app.main_renderer().unwrap().tab_bar_visible() {
        return Err(String::from("native shortcut query did not execute its live bound action"));
    }
    assert!(!app.__test_palette_open());
    app.__test_set_memory_clipboard("unchanged disabled copy");
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_palette_query("Copy to Clipboard");
    render(&mut app, active, id);
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    assert!(
        app.__test_palette_open(),
        "disabled command remains visible instead of dispatching and closing"
    );
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("unchanged disabled copy"));
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Escape)));
    let target = app.main_tabs().unwrap().tabs()[0].id;
    app.main_tabs_mut().unwrap().set_active_custom_title("stable target");
    app.__test_seed_tab("peer");
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_palette_query("stable target");
    render(&mut app, active, id);
    assert!(app.__test_palette_open());
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    render(&mut app, active, id);
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, target);
    assert!(!app.__test_palette_open());
    assert!(app.run_action(&Action::ToggleTabBar));
    for index in 0..16 {
        app.__test_seed_tab(&format!("overflow {index}"));
    }
    render(&mut app, active, id);
    let layout = sonicterm_ui::tabbar_view::TabBarLayout::compute_with_height(
        app.main_tabs().unwrap(),
        window.inner_size().width as f32,
        app.main_renderer().unwrap().tab_bar_logical_height(),
    )
    .with_top_offset(app.main_renderer().unwrap().tab_bar_y_offset());
    let control = layout.overflow.expect("many tabs expose the selector control");
    let hidden_target = app.main_tabs().unwrap().tabs()[0].id;
    assert!(!layout.tabs.iter().any(|tab| tab.idx == 0));
    for event in [
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(
                f64::from(control.x + control.w * 0.5),
                f64::from(control.y + control.h * 0.5),
            ),
        },
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    ] {
        ApplicationHandler::window_event(&mut app, active, id, event);
    }
    assert!(app.__test_palette_open());
    app.__test_enable_pty_write_log();
    ApplicationHandler::window_event(
        &mut app,
        active,
        id,
        WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, -1.0),
            phase: TouchPhase::Moved,
        },
    );
    assert!(app.__test_pty_write_log().is_empty(), "modal wheel never reaches the terminal");
    render(&mut app, active, id);
    let mut model = sonicterm_ui::command_palette::CommandPalette::new();
    let i18n = sonicterm_ui::i18n::I18n::new(Some("ja"));
    model.set_tabs(app.main_tabs().unwrap(), &i18n);
    model.open_tabs();
    let size = window.inner_size();
    let palette_layout = sonicterm_ui::overlays::PaletteLayout::compute(
        &mut model,
        size.width as f32,
        size.height as f32,
        0.0,
        app.main_renderer().unwrap().scale_factor(),
    )
    .unwrap();
    let row = palette_layout.rows[0].rect;
    for event in [
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(
                f64::from(row.x + row.w * 0.5),
                f64::from(row.y + row.h * 0.5),
            ),
        },
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
    ] {
        ApplicationHandler::window_event(&mut app, active, id, event);
    }
    render(&mut app, active, id);
    assert_eq!(
        app.main_tabs().unwrap().active().unwrap().id,
        hidden_target,
        "pointer selection activates the hidden tab's runtime identity"
    );
    assert!(!app.__test_palette_open());
    let revealed = sonicterm_ui::tabbar_view::TabBarLayout::compute_with_height(
        app.main_tabs().unwrap(),
        window.inner_size().width as f32,
        app.main_renderer().unwrap().tab_bar_logical_height(),
    );
    assert!(revealed.tabs.iter().any(|tab| tab.idx == 0));
    let center =
        |rect: sonicterm_ui::tabbar_view::Rect| (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
    let targets: Vec<_> =
        app.main_tabs().unwrap().tabs().iter().map(|tab| (tab.id, tab.title.clone())).collect();
    // Every current tab is reachable through native pointer events and the existing Enter activation path.
    for (target_id, title) in &targets {
        click(&mut app, active, id, center(control));
        assert!(app.__test_palette_open());
        app.__test_set_palette_query(title);
        let mut matches = sonicterm_ui::command_palette::CommandPalette::new();
        matches.set_tabs(app.main_tabs().unwrap(), &i18n);
        matches.set_locale(&i18n);
        matches.open_tabs();
        matches.set_query(title);
        let index = matches.visible().iter().position(|entry| matches!(entry, sonicterm_ui::command_palette::PaletteEntry::Tab { id, .. } if id == target_id)).unwrap();
        let mut selected_row = None;
        for _ in 0..=matches.len() {
            let layout = sonicterm_ui::overlays::PaletteLayout::compute(
                &mut matches,
                size.width as f32,
                size.height as f32,
                0.0,
                app.main_renderer().unwrap().scale_factor(),
            )
            .unwrap();
            if let Some(row) = layout.rows.iter().find(|row| row.item_index == index) {
                selected_row = Some(row.rect);
                break;
            }
            ApplicationHandler::window_event(
                &mut app,
                active,
                id,
                WindowEvent::MouseWheel {
                    device_id: DeviceId::dummy(),
                    delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                    phase: TouchPhase::Moved,
                },
            );
            matches.move_selection_down();
        }
        render(&mut app, active, id);
        click(&mut app, active, id, center(selected_row.expect("wheel exposes the target row")));
        render(&mut app, active, id);
        assert_eq!(app.main_tabs().unwrap().active().unwrap().id, *target_id);
        assert!(!app.__test_palette_open());
    }
    for (index, (target_id, _)) in targets.iter().enumerate() {
        click(&mut app, active, id, center(control));
        for _ in 0..index {
            app.__test_command_palette_handle_key(&Key::Named(NamedKey::ArrowDown));
        }
        assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
        assert_eq!(app.main_tabs().unwrap().active().unwrap().id, *target_id);
    }
    // A parser with mouse tracking is the negative control for modal clicks and wheel leakage.
    let active_index = app.main_tabs().unwrap().active_index();
    let pane = app.__test_active_pane_in_tab(active_index).unwrap();
    assert!(app.__test_advance_pane_parser(pane, b"\x1b[?1003h\x1b[?1006h"));
    render(&mut app, active, id);
    app.__test_enable_pty_write_log();
    click(&mut app, active, id, (450.0, 500.0));
    assert!(
        !app.__test_pty_write_log().is_empty(),
        "unowned terminal pointer must produce reports"
    );
    click(&mut app, active, id, center(control));
    assert!(app.__test_palette_open());
    app.__test_enable_pty_write_log();
    click(&mut app, active, id, center(palette_layout.query_row));
    assert!(app.__test_palette_open());
    ApplicationHandler::window_event(
        &mut app,
        active,
        id,
        WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -f64::MAX)),
            phase: TouchPhase::Moved,
        },
    );
    pointer_at(&mut app, active, id, (20.0, 500.0));
    pointer_button(&mut app, active, id, ElementState::Pressed);
    assert!(app.__test_palette_open(), "outside dismissal waits for release");
    pointer_button(&mut app, active, id, ElementState::Released);
    assert!(!app.__test_palette_open());
    assert!(app.__test_pty_write_log().is_empty(), "modal input cannot reach a mouse-aware pane");
    // A redraw between press and release cannot transfer a held click to a reordered row.
    click(&mut app, active, id, center(control));
    render(&mut app, active, id);
    pointer_at(&mut app, active, id, center(row));
    pointer_button(&mut app, active, id, ElementState::Pressed);
    let before = app.main_tabs().unwrap().active().unwrap().id;
    app.main_tabs_mut().unwrap().reorder(0, 1);
    app.main_tab_states_mut().unwrap().swap(0, 1);
    render(&mut app, active, id);
    pointer_button(&mut app, active, id, ElementState::Released);
    assert!(app.__test_palette_open());
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, before);
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Escape));
    // A held modal row is revoked by IME preedit, and its release is still consumed.
    click(&mut app, active, id, center(control));
    render(&mut app, active, id);
    pointer_at(&mut app, active, id, center(row));
    pointer_button(&mut app, active, id, ElementState::Pressed);
    ApplicationHandler::window_event(
        &mut app,
        active,
        id,
        WindowEvent::Ime(winit::event::Ime::Preedit("中".into(), None)),
    );
    pointer_button(&mut app, active, id, ElementState::Released);
    assert!(app.__test_palette_open());
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, before);
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Escape));
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Escape));
    // Normal wgpu presentation consumes the same selector geometry and tab identity after leaving degrade mode.
    app.main_renderer_mut().unwrap().set_software_render_degrade(false);
    app.__test_set_software_render_degrade(false);
    render(&mut app, active, id);
    assert!(!app.main_renderer().unwrap().is_software_render_degraded());
    click(&mut app, active, id, center(control));
    app.__test_set_palette_query("stable target");
    render(&mut app, active, id);
    let presented = app.main_renderer().unwrap().successful_frame_count();
    click(&mut app, active, id, center(row));
    render(&mut app, active, id);
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, target);
    assert!(!app.__test_palette_open());
    assert!(app.main_renderer().unwrap().successful_frame_count() > presented);
    // Width policy changes must repaint the same idle tab strip before its new hit rectangles are used.
    let width_before = sonicterm_ui::tabbar_view::TabBarLayout::compute_with_height(
        app.main_tabs().unwrap(),
        size.width as f32,
        app.main_renderer().unwrap().tab_bar_logical_height(),
    )
    .active_indicator_rect()
    .unwrap()
    .w;
    let frame_before = app.main_renderer().unwrap().successful_frame_count();
    let max_before = sonicterm_ui::tabbar_view::max_tab_width();
    sonicterm_ui::tabbar_view::set_max_tab_width(1.0);
    let width_after = sonicterm_ui::tabbar_view::TabBarLayout::compute_with_height(
        app.main_tabs().unwrap(),
        size.width as f32,
        app.main_renderer().unwrap().tab_bar_logical_height(),
    )
    .active_indicator_rect()
    .unwrap()
    .w;
    assert!(width_after < width_before, "the width-only stimulus must change tab geometry");
    render(&mut app, active, id);
    let repainted = app.main_renderer().unwrap().successful_frame_count() > frame_before;
    sonicterm_ui::tabbar_view::set_max_tab_width(max_before);
    assert!(repainted, "a width-only change must invalidate the retained tab strip");
    render(&mut app, active, id);
    let stable_count = app.main_renderer().unwrap().successful_frame_count();
    sonicterm_ui::tabbar_view::set_max_tab_width(max_before);
    render(&mut app, active, id);
    assert_eq!(app.main_renderer().unwrap().successful_frame_count(), stable_count);
    // Font, DPI, and native-window resizing keep renderer and input geometry in the same physical coordinates.
    for (scale, font_size, width) in [(1.0, 18.0, 420_u32), (1.5, 13.0, 900), (2.0, 13.0, 600)] {
        let config = Config::default();
        let renderer = app.main_renderer_mut().unwrap();
        renderer.set_scale_factor(scale);
        renderer.set_font(
            &config.font.family,
            font_size,
            config.font.line_height,
            config.font.effective_weight_scale(),
        );
        let _ = window.request_inner_size(PhysicalSize::new(width, 600));
        let resized = window.inner_size();
        ApplicationHandler::window_event(&mut app, active, id, WindowEvent::Resized(resized));
        let width = resized.width;
        render(&mut app, active, id);
        let renderer = app.main_renderer().unwrap();
        let layout = sonicterm_ui::tabbar_view::TabBarLayout::compute_with_height(
            app.main_tabs().unwrap(),
            width as f32,
            renderer.tab_bar_logical_height(),
        )
        .with_top_offset(renderer.tab_bar_y_offset());
        let overflow = layout.overflow.unwrap();
        let active_rect = layout.active_indicator_rect().unwrap();
        assert!(active_rect.w > 0.0 && active_rect.x + active_rect.w <= overflow.x);
        click(&mut app, active, id, center(overflow));
        assert!(
            app.__test_palette_open(),
            "overflow opens at scale={scale} font={font_size} size={resized:?}"
        );
        app.__test_set_palette_query("stable target");
        render(&mut app, active, id);
        let mut model = sonicterm_ui::command_palette::CommandPalette::new();
        model.set_tabs(app.main_tabs().unwrap(), &i18n);
        model.open_tabs();
        model.set_query("stable target");
        let layout = sonicterm_ui::overlays::PaletteLayout::compute(
            &mut model,
            width as f32,
            600.0,
            0.0,
            scale,
        )
        .unwrap();
        click(&mut app, active, id, center(layout.rows[0].rect));
        assert!(!app.__test_palette_open());
        assert_eq!(app.main_tabs().unwrap().active().unwrap().id, target);
    }
    // A child overflow selector resolves its own pane identity without borrowing main or requiring foreground ownership.
    let titles: Vec<_> = (0..18).map(|index| format!("child {index}")).collect();
    let child =
        app.__test_seed_child_window(&titles.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(app.__test_invoke_activate_tab_in_child(child, 0));
    let first_child_pane = app.__test_child_active_pane(child).unwrap();
    assert!(app.__test_invoke_activate_tab_in_child(child, 1));
    let second_child_pane = app.__test_child_active_pane(child).unwrap();
    assert!(app.__test_invoke_activate_tab_in_child(child, 17));
    let child_window = Arc::new(
        active
            .create_window(
                Window::default_attributes()
                    .with_visible(true)
                    .with_inner_size(PhysicalSize::new(900, 600)),
            )
            .map_err(|error| error.to_string())?,
    );
    let config = Config::default();
    let mut child_renderer = GpuRenderer::new_with_shared_context(
        child_window.clone(),
        active,
        &Theme::default(),
        RendererSettings {
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
            role: "native-palette-child-test",
        },
        app.main_renderer().unwrap().shared_context(),
    )
    .map_err(|error| error.to_string())?;
    child_renderer.set_tab_bar_visible(true);
    child_renderer.set_cursor_blink(false);
    let mut child_tabs = sonicterm_ui::tabs::TabBar::new();
    for title in &titles {
        child_tabs.push(sonicterm_ui::tabs::Tab::new(title));
    }
    let child_bar = sonicterm_ui::tabbar_view::TabBarLayout::compute_with_height(
        &child_tabs,
        900.0,
        child_renderer.tab_bar_logical_height(),
    )
    .with_top_offset(child_renderer.tab_bar_y_offset());
    let child_scale = child_renderer.scale_factor();
    assert!(app.__test_attach_window_renderer(child, child_window.clone(), child_renderer));
    render(&mut app, active, child);
    app.__test_set_frontmost_window(Some(id));
    click(&mut app, active, child, center(child_bar.overflow.unwrap()));
    assert_eq!(app.__test_palette_attached_window(), Some(child));
    assert!(app.__test_palette_open());
    let mut child_model = sonicterm_ui::command_palette::CommandPalette::new();
    child_model.set_tabs(&child_tabs, &i18n);
    child_model.open_tabs();
    let child_layout = sonicterm_ui::overlays::PaletteLayout::compute(
        &mut child_model,
        900.0,
        600.0,
        0.0,
        child_scale,
    )
    .unwrap();
    render(&mut app, active, child);
    click(&mut app, active, child, center(child_layout.rows[0].rect));
    assert_eq!(app.__test_child_active_pane(child), Some(first_child_pane));
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, target);
    assert!(!app.__test_palette_open());
    render(&mut app, active, child);
    assert!(app.__test_invoke_activate_tab_in_child(child, 17));
    Ok(NativeKeys {
        app,
        child_window,
        child,
        overflow: child_bar.overflow.unwrap(),
        first_pane: first_child_pane,
        second_pane: second_child_pane,
        stage: KeyStage::NormalEnter,
        deadline: Instant::now() + Duration::from_secs(30),
    })
}

/// Native palette text, overflow selection, modal routing, and geometry remain coherent across windows and presenters.
#[test]
fn windows_palette_native_hint_remains_searchable_and_executable() {
    let event_loop =
        EventLoop::builder().with_any_thread(true).build().expect("Windows event loop");
    let mut probe = Probe { outcome: None, native_keys: None };
    event_loop.run_app(&mut probe).expect("native palette event loop");
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}
