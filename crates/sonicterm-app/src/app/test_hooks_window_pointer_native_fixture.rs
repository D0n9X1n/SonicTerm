//! Native hover retry and modifier-selection probe state.

use super::*;

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
pub(super) enum HoverRetryPhase {
    Setup,
    BaselineActive,
    BaselineInactive,
    ContendedActive,
    ContendedInactive,
    PointerReady,
    PointerBaselineBlank,
    PointerBaselineActive,
    PointerContendedBlank,
    PointerContendedActive,
}

#[cfg(windows)]
pub(super) struct HoverRetryProbe {
    app: App,
    native_id: winit::window::WindowId,
    tracked_id: winit::window::WindowId,
    pane_id: u64,
    pub(super) case: &'static str,
    progress: std::sync::Arc<NativeProbeProgress>,
    phase: HoverRetryPhase,
    cycle: usize,
    deadline: std::time::Instant,
    phase_started: std::time::Instant,
    last_native_frame: std::time::Instant,
    native_frames: u64,
    phase_native_start: u64,
    phase_present_start: u64,
    active_pixels: Vec<[u8; 4]>,
    inactive_pixels: Vec<[u8; 4]>,
    blank_pixels: Vec<[u8; 4]>,
}

#[cfg(windows)]
impl HoverRetryProbe {
    pub(super) const URI: &'static str = "https://example.com/docs";
    pub(super) const QUIET: std::time::Duration = std::time::Duration::from_millis(200);

    /// Publish setup milestones before native window and renderer construction can block.
    pub(super) fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        case_index: usize,
        progress: std::sync::Arc<NativeProbeProgress>,
    ) -> Self {
        use sonicterm_cfg::config::{BackdropKind, SoftwareRenderMode, SubpixelAaMode};
        use sonicterm_gpu::core::{GpuRenderer, RendererSettings, SurfaceAppearance};
        use std::{sync::Arc, time::Instant};
        use winit::{dpi::PhysicalSize, window::Window};
        let case = ["main/plain", "main/OSC8", "child/plain", "child/OSC8"][case_index];
        progress.set("hover", case, "Setup/create_window");
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_inner_size(PhysicalSize::new(640, 360))
                        .with_active(false)
                        .with_title("SonicTerm quiet hover regression"),
                )
                .unwrap(),
        );
        let native_id = window.id();
        let theme = Theme::default();
        let mut config = Config::default();
        config.font.size = 14.0;
        config.font.subpixel_aa = SubpixelAaMode::Rgb;
        config.appearance.backdrop = BackdropKind::Opaque;
        config.appearance.opacity = 1.0;
        config.appearance.software_render_mode = SoftwareRenderMode::Force;
        config.appearance.scrollbar = ScrollbarMode::Never;
        config.window.padding_left = 0.0;
        config.window.padding_right = 0.0;
        config.window.padding_top = 0.0;
        config.window.padding_bottom = 0.0;
        progress.set("hover", case, "Setup/create_renderer");
        let mut renderer = GpuRenderer::new(
            window.clone(),
            event_loop,
            &theme,
            RendererSettings {
                font_family: &config.font.family,
                font_dirs: &[],
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
                role: "quiet-hover-test",
                glyph_atlas_start: sonicterm_gpu::core::GlyphAtlasStart::Normal,
            },
        )
        .unwrap();
        renderer.set_tab_bar_visible(false);
        renderer.set_cursor_blink(false);
        progress.set("hover", case, "Setup/create_pane");
        let mut app = App::new(theme, config, Keymap::default());
        app.__test_set_software_render_degrade(true);
        let (tracked_id, pane_id) = if case_index < 2 {
            let pane = app.__test_seed_tab("hover-main");
            (app.main_window_id.unwrap(), pane)
        } else {
            let id = app.__test_seed_child_window(&["hover-child"]);
            (id, app.windows[&id].tab_states[0].active_pane)
        };
        assert!(app.__test_attach_window_renderer(tracked_id, window, renderer));
        app.windows.get_mut(&tracked_id).unwrap().cursor_pos = (-100.0, -100.0);
        let label = if case_index.is_multiple_of(2) {
            Self::URI.to_owned()
        } else {
            format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", Self::URI, Self::URI)
        };
        // Identical terminal cells isolate OSC 8 metadata; alternate-screen mouse reporting stays enabled without a PTY.
        {
            let pane = &app.windows[&tracked_id].panes[&pane_id];
            let mut parser = pane.parser.lock();
            parser.advance(
                format!("\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?25l\x1b[2;2H{label}\x1b[6;1H")
                    .as_bytes(),
            );
            // Pointer handlers read the published byte, as the VT worker would leave it.
            pane.__test_publish_input_modes(&parser);
        }
        assert!(app.path_workers.is_none());
        assert!(app.windows[&tracked_id].panes[&pane_id].pty.is_none());
        let now = Instant::now();
        let mut probe = Self {
            app,
            native_id,
            tracked_id,
            pane_id,
            case,
            progress,
            phase: HoverRetryPhase::Setup,
            cycle: 0,
            deadline: now + std::time::Duration::from_secs(20),
            phase_started: now,
            last_native_frame: now,
            native_frames: 0,
            phase_native_start: 0,
            phase_present_start: 0,
            active_pixels: Vec::new(),
            inactive_pixels: Vec::new(),
            blank_pixels: Vec::new(),
        };
        // Focus supplies initial layout independently of the modifier scheduling contract under test.
        probe.progress.set("hover", case, "Setup/focus");
        winit::application::ApplicationHandler::window_event(
            &mut probe.app,
            event_loop,
            tracked_id,
            winit::event::WindowEvent::Focused(true),
        );
        probe.progress.set("hover", case, "Setup");
        probe
    }

    pub(super) fn assert_unscheduled(&self) {
        assert!(
            !self.app.pending_redraw
                && self.app.pending_redraw_windows.is_empty()
                && self.app.windows[&self.tracked_id].retry_not_before.is_none(),
            "INVALID {} {:?}: pacing or parser retry contaminated the native observation",
            self.case,
            self.phase,
        );
    }

    pub(super) fn event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        if id != self.native_id || !matches!(event, winit::event::WindowEvent::RedrawRequested) {
            return;
        }
        self.progress.set(
            "hover",
            self.case,
            format!("{:?}/render cycle={}", self.phase, self.cycle),
        );
        assert!(
            self.app.windows[&self.tracked_id].panes[&self.pane_id].parser.try_lock().is_some(),
            "INVALID {} {:?}: native frame arrived while parser lock was held",
            self.case,
            self.phase,
        );
        self.assert_unscheduled();
        // Backdate pacing only after native delivery; frame admission cannot supply a missing native redraw request.
        assert!(self.app.__test_set_window_last_render(
            self.tracked_id,
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        ));
        self.native_frames += 1;
        winit::application::ApplicationHandler::window_event(
            &mut self.app,
            event_loop,
            self.tracked_id,
            event,
        );
        self.last_native_frame = std::time::Instant::now();
        self.assert_unscheduled();
    }

    pub(super) fn row_pixels(&self) -> Vec<[u8; 4]> {
        let renderer = self.app.windows[&self.tracked_id].renderer.as_ref().unwrap();
        let [origin_x, origin_y] =
            renderer.pane_grid_origin(self.pane_id).expect("native layout must exist");
        let (cell_w, cell_h) = renderer.cell_size();
        // Restrict readback to the URI row and prove the real tooltip geometry cannot cover its pixels.
        if let Some(preview) = self.app.windows[&self.tracked_id].link_preview.as_ref() {
            let font_size = renderer.font_size().max(1.0) * renderer.scale_factor();
            let layout = sonicterm_ui::overlays::LinkPreviewLayout::compute(
                &sonicterm_ui::overlays::link_preview_text(&preview.uri),
                preview.pointer,
                renderer.logical_size(),
                font_size * 1.4,
                renderer.scale_factor(),
                |text| renderer.measure_overlay_text_width(text, font_size),
            )
            .expect("baseline preview must fit the native window");
            assert!(
                layout.border.y >= (origin_y + 2.0 * cell_h).ceil()
                    || layout.border.y + layout.border.h <= (origin_y + cell_h).floor(),
                "INVALID {}: link preview overlaps target-row readback",
                self.case,
            );
        }
        ((origin_y + cell_h).floor() as u32..(origin_y + 2.0 * cell_h).ceil() as u32)
            .flat_map(|pixel_y| {
                ((origin_x + cell_w).floor() as u32
                    ..(origin_x + (1 + Self::URI.len()) as f32 * cell_w).ceil() as u32)
                    .map(move |pixel_x| (pixel_x, pixel_y))
            })
            .map(|(pixel_x, pixel_y)| {
                self.app
                    .__test_window_software_frame_pixel_bgra(self.tracked_id, pixel_x, pixel_y)
                    .expect("forced software frame must expose target-row pixels")
            })
            .collect()
    }

    pub(super) fn assert_hover(&self, active: bool) {
        let hover = self.app.windows[&self.tracked_id]
            .hovered_url
            .as_ref()
            .expect("stationary URI must retain its hover identity");
        assert_eq!(hover.url, Self::URI, "{} {:?}", self.case, self.phase);
        assert_eq!(hover.active(), active, "{} {:?}", self.case, self.phase);
        assert_eq!(self.app.frontmost_window, Some(self.tracked_id));
    }

    /// The watchdog sees every modifier and pointer phase, including its repeated cycle, before the stimulus runs.
    pub(super) fn begin_phase(&mut self, phase: HoverRetryPhase) {
        self.progress.set("hover", self.case, format!("{phase:?} cycle={}", self.cycle));
        self.assert_unscheduled();
        let period = crate::app::effective_frame_period(true, false, self.app.frame_period);
        assert!(self.app.windows[&self.tracked_id].last_render.elapsed() > period);
        self.phase = phase;
        self.phase_started = std::time::Instant::now();
        self.phase_native_start = self.native_frames;
        self.phase_present_start =
            self.app.windows[&self.tracked_id].renderer.as_ref().unwrap().successful_frame_count();
    }

    /// Keep the fixture's lock flag visible throughout the real modifier lookup, but clear it before rendering.
    pub(super) fn modifiers(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        active: bool,
        contended: bool,
        phase: HoverRetryPhase,
    ) {
        self.begin_phase(phase);
        let parser = self.app.windows[&self.tracked_id].panes[&self.pane_id].parser.clone();
        let previous = self.app.windows[&self.tracked_id].hovered_url.clone();
        self.progress.parser_lock(contended);
        let guard = contended.then(|| parser.lock());
        winit::application::ApplicationHandler::window_event(
            &mut self.app,
            event_loop,
            self.tracked_id,
            winit::event::WindowEvent::ModifiersChanged(
                if active { ModifiersState::CONTROL } else { ModifiersState::empty() }.into(),
            ),
        );
        if contended && active {
            assert_eq!(self.app.windows[&self.tracked_id].hovered_url, previous);
            self.assert_hover(false);
            assert!(self.app.windows[&self.tracked_id].link_preview.is_none());
        } else {
            self.assert_hover(active);
        }
        if contended && !active {
            assert!(!self.app.windows[&self.tracked_id].hover_link);
            assert!(self.app.windows[&self.tracked_id].link_preview.is_none());
        }
        // The lock covers modifier lookup only; rendering with it held would test a different retry mechanism.
        drop(guard);
        self.progress.parser_lock(false);
        self.assert_unscheduled();
    }

    /// Diagnose a held-lock pointer lookup without turning its subsequent redraw into a parser-contention test.
    pub(super) fn pointer_refresh(
        &mut self,
        on_uri: bool,
        contended: bool,
        phase: HoverRetryPhase,
    ) {
        self.begin_phase(phase);
        assert_eq!(self.app.windows[&self.tracked_id].modifiers, ModifiersState::CONTROL);
        let window = self.app.windows.get_mut(&self.tracked_id).unwrap();
        let renderer = window.renderer.as_ref().unwrap();
        let [origin_x, origin_y] = renderer.pane_grid_origin(self.pane_id).unwrap();
        let (cell_w, cell_h) = renderer.cell_size();
        window.cursor_pos = (
            (origin_x + 4.5 * cell_w) as f64,
            (origin_y + if on_uri { 1.5 } else { 5.5 } * cell_h) as f64,
        );
        let parser = window.panes[&self.pane_id].parser.clone();
        let previous = window.hovered_url.clone();
        self.progress.parser_lock(contended);
        let guard = contended.then(|| parser.lock());
        // This is the shared pointer-refresh seam, not CursorMoved: its later mouse-report lock would block this fixture.
        self.app.refresh_target_hover(self.tracked_id);
        if contended {
            assert_eq!(self.app.windows[&self.tracked_id].hovered_url, previous);
            assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
            assert!(self.app.windows[&self.tracked_id].link_preview.is_none());
        } else if on_uri {
            self.assert_hover(true);
        } else {
            assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
        }
        drop(guard);
        self.progress.parser_lock(false);
        self.assert_unscheduled();
    }

    pub(super) fn poll(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) -> bool {
        use HoverRetryPhase::{
            BaselineActive, BaselineInactive, ContendedActive, ContendedInactive,
            PointerBaselineActive, PointerBaselineBlank, PointerContendedActive,
            PointerContendedBlank, PointerReady, Setup,
        };
        self.progress.set(
            "hover",
            self.case,
            format!("{:?}/poll cycle={}", self.phase, self.cycle),
        );
        let now = std::time::Instant::now();
        assert!(
            now < self.deadline,
            "INVALID {} {:?}: hover watchdog expired",
            self.case,
            self.phase
        );
        if now.duration_since(self.phase_started.max(self.last_native_frame)) < Self::QUIET {
            return false;
        }
        self.assert_unscheduled();
        let frames = self.native_frames - self.phase_native_start;
        let presents =
            self.app.windows[&self.tracked_id].renderer.as_ref().unwrap().successful_frame_count()
                - self.phase_present_start;
        let hover = self.app.windows[&self.tracked_id].hovered_url.as_ref();
        eprintln!(
            "hover case={} phase={:?} cycle={} native_frames={frames} presents={presents} uri={:?} active={:?}",
            self.case,
            self.phase,
            self.cycle,
            hover.map(|hover| hover.url.as_str()),
            hover.map(HoveredUrl::active),
        );
        if matches!(self.phase, ContendedActive | ContendedInactive | PointerContendedActive) {
            assert!(
                frames > 0,
                "{} {:?} cycle={}: 0 native frames in the quiet window",
                self.case,
                self.phase,
                self.cycle
            );
        } else {
            assert!(
                frames > 0,
                "INVALID {} {:?}: baseline received 0 native frames",
                self.case,
                self.phase
            );
        }
        assert!(
            presents > 0,
            "INVALID {} {:?}: native redraw produced no presentation",
            self.case,
            self.phase
        );
        match self.phase {
            Setup => {
                let window = self.app.windows.get_mut(&self.tracked_id).unwrap();
                let renderer = window.renderer.as_ref().unwrap();
                assert!(
                    renderer.__test_pane_focus_flash_target().is_none(),
                    "INVALID: setup focus flash must not affect the baseline"
                );
                let [origin_x, origin_y] =
                    renderer.pane_grid_origin(self.pane_id).expect("setup layout");
                let (cell_w, cell_h) = renderer.cell_size();
                window.cursor_pos =
                    ((origin_x + 4.5 * cell_w) as f64, (origin_y + 1.5 * cell_h) as f64);
                self.modifiers(event_loop, true, false, BaselineActive);
            }
            BaselineActive => {
                self.assert_hover(true);
                self.active_pixels = self.row_pixels();
                self.modifiers(event_loop, false, false, BaselineInactive);
            }
            BaselineInactive => {
                self.assert_hover(false);
                self.inactive_pixels = self.row_pixels();
                assert_ne!(
                    self.active_pixels, self.inactive_pixels,
                    "INVALID {}: baseline target-row pixels do not distinguish Ctrl",
                    self.case
                );
                self.modifiers(event_loop, true, true, ContendedActive);
            }
            ContendedActive => {
                self.assert_hover(true);
                assert_eq!(
                    self.row_pixels(),
                    self.active_pixels,
                    "{} cycle={}: contended Ctrl must paint the free-lock active row",
                    self.case,
                    self.cycle
                );
                self.modifiers(event_loop, false, true, ContendedInactive);
            }
            ContendedInactive => {
                self.assert_hover(false);
                assert_eq!(
                    self.row_pixels(),
                    self.inactive_pixels,
                    "{} cycle={}: contended release must restore the free-lock inactive row",
                    self.case,
                    self.cycle
                );
                self.cycle += 1;
                if self.cycle == 3 {
                    self.cycle = 0;
                    self.modifiers(event_loop, true, false, PointerReady);
                } else {
                    self.modifiers(event_loop, true, true, ContendedActive);
                }
            }
            PointerReady => {
                self.assert_hover(true);
                self.pointer_refresh(false, false, PointerBaselineBlank);
            }
            PointerBaselineBlank => {
                assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
                self.blank_pixels = self.row_pixels();
                self.pointer_refresh(true, false, PointerBaselineActive);
            }
            PointerBaselineActive => {
                self.assert_hover(true);
                assert_eq!(
                    self.row_pixels(),
                    self.active_pixels,
                    "{}: free pointer refresh must match the modifier baseline",
                    self.case
                );
                assert_ne!(
                    self.row_pixels(),
                    self.blank_pixels,
                    "INVALID {}: blank and active pointer baselines must differ",
                    self.case
                );
                self.pointer_refresh(false, false, PointerContendedBlank);
            }
            PointerContendedBlank => {
                assert!(self.app.windows[&self.tracked_id].hovered_url.is_none());
                assert_eq!(
                    self.row_pixels(),
                    self.blank_pixels,
                    "{}: blank row must settle before the held-lock pointer refresh",
                    self.case
                );
                self.pointer_refresh(true, true, PointerContendedActive);
            }
            PointerContendedActive => {
                self.assert_hover(true);
                assert_eq!(self.row_pixels(), self.active_pixels, "{} cycle={}: contended pointer-refresh seam must paint the free-lock active row", self.case, self.cycle);
                self.cycle += 1;
                if self.cycle == 3 {
                    return true;
                }
                self.pointer_refresh(false, false, PointerContendedBlank);
            }
        }
        false
    }
}

#[cfg(windows)]
pub(super) struct ModifierSelectionProbe {
    app: App,
    progress: std::sync::Arc<NativeProbeProgress>,
    window: winit::window::Window,
    targets: Vec<(winit::window::WindowId, u64, sonicterm_io::pty::PtyInputSender)>,
    steps: std::collections::VecDeque<(bool, u16, u16, bool, bool)>,
    in_flight: Option<(bool, u16, u16, bool, bool)>,
    completed: u64,
    deadline: std::time::Instant,
}

#[cfg(windows)]
impl ModifierSelectionProbe {
    /// Name native construction and per-window PTY setup separately from the subsequent key-delivery phases.
    pub(super) fn new(
        event_loop: &winit::event_loop::ActiveEventLoop,
        progress: std::sync::Arc<NativeProbeProgress>,
    ) -> Self {
        use winit::window::Window;
        progress.set("selection", "main/child", "create_window");
        let window = event_loop
            .create_window(Window::default_attributes().with_visible(false).with_active(false))
            .unwrap();
        let mut app = App::new(Default::default(), Default::default(), Default::default());
        app.clipboard = None;
        app.test_clipboard_text = Some("clipboard sentinel".into());
        app.keymap = Keymap::parse_resilient("[meta]\nname = \"selection\"\nversion = \"1.0\"\n[[binding]]\nkeys = \"ctrl+shift+c\"\naction = \"copy_to_clipboard\"\n", "native selection fixture").unwrap();
        app.__test_enable_pty_write_log();
        let main_pane = app.__test_seed_tab("modifier-main");
        let main = app.main_window_id.unwrap();
        let child = app.__test_seed_child_window(&["modifier-child"]);
        let child_pane = app.__test_child_pane_ids(child).unwrap()[0];
        let mut targets = Vec::new();
        for (window_id, pane_id) in [(main, main_pane), (child, child_pane)] {
            let case = if window_id == main { "main" } else { "child" };
            progress.set("selection", case, "spawn_pty");
            let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(
                "cmd.exe",
                &["/D".into(), "/Q".into()],
                80,
                24,
            )
            .unwrap();
            let input = pty.input_sender();
            let pane = app.windows.get_mut(&window_id).unwrap().panes.get_mut(&pane_id).unwrap();
            pane.pty = Some(pty);
            progress.set("selection", case, "prepare_parser");
            let mut parser = pane.parser.lock();
            parser.advance(b"selected text\x1b[?9001h");
            pane.keyboard_input
                .store(parser.keyboard_input_snapshot(), std::sync::atomic::Ordering::Relaxed);
            targets.push((window_id, pane_id, input));
        }
        let mut steps = std::collections::VecDeque::new();
        for kitty in [false, true] {
            for (virtual_key, scan) in
                [(0x11, 0x1d), (0x10, 0x2a), (0x43, 0x2e), (0x58, 0x2d), (0x59, 0x15)]
            {
                steps.extend([
                    (kitty, virtual_key, scan, true, false),
                    (kitty, virtual_key, scan, true, true),
                    (kitty, virtual_key, scan, false, false),
                ]);
            }
        }
        progress.set("selection", "main/child", "ready");
        Self {
            app,
            progress,
            window,
            targets,
            steps,
            in_flight: None,
            completed: 0,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(15),
        }
    }

    pub(super) fn poll(&mut self, failures: &mut Vec<String>) -> bool {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::{
            Foundation::{HWND, LPARAM, WPARAM},
            UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP},
        };
        self.progress.set(
            "selection",
            "main/child",
            format!("poll in_flight={:?}", self.in_flight),
        );
        if std::time::Instant::now() >= self.deadline {
            failures.push(format!("modifier input deadline: {:?}", self.in_flight));
            return true;
        }
        if self.in_flight.is_some()
            || self
                .targets
                .iter()
                .any(|(_, _, input)| input.diagnostics().completed_messages < self.completed)
        {
            return false;
        }
        let Some(stroke @ (kitty, virtual_key, scan, down, repeat)) = self.steps.pop_front() else {
            return true;
        };
        if down && !repeat {
            for (window_id, pane_id, _) in &self.targets {
                let case =
                    if Some(*window_id) == self.app.main_window_id { "main" } else { "child" };
                self.progress.set(
                    "selection",
                    case,
                    format!("prepare kitty={kitty} vk={virtual_key} down={down} repeat={repeat}"),
                );
                let pane = &self.app.windows[window_id].panes[pane_id];
                let mut parser = pane.parser.lock();
                parser.advance(if kitty { b"\x1b[=10u" } else { b"\x1b[=0u" });
                pane.keyboard_input
                    .store(parser.keyboard_input_snapshot(), std::sync::atomic::Ordering::Relaxed);
                drop(parser);
                if virtual_key == 0x43 {
                    // Copy must use the selection preserved across the preceding Shift lifecycle, not a fresh fixture range.
                    continue;
                }
                let mut selection = Selection::new(0, 0);
                selection.end = (0, 8);
                if Some(*window_id) == self.app.main_window_id {
                    self.app.__test_set_main_selection(Some(selection));
                } else {
                    self.app.__test_set_child_selection(*window_id, Some(selection));
                }
            }
        }
        let RawWindowHandle::Win32(handle) = self.window.window_handle().unwrap().as_raw() else {
            panic!("Windows handle");
        };
        let bits = 1
            | (u32::from(scan) << 16)
            | (u32::from(repeat || !down) << 30)
            | (u32::from(!down) << 31);
        self.in_flight = Some(stroke);
        self.progress.set(
            "selection",
            "main/child",
            format!("post kitty={kitty} vk={virtual_key} down={down} repeat={repeat}"),
        );
        // SAFETY: this test owns the destination HWND; only scalar key metadata is posted to its message queue.
        unsafe {
            PostMessageW(
                Some(HWND(handle.hwnd.get() as *mut _)),
                if down { WM_KEYDOWN } else { WM_KEYUP },
                WPARAM(usize::from(virtual_key)),
                LPARAM(bits as isize),
            )
        }
        .unwrap();
        false
    }

    pub(super) fn event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        id: winit::window::WindowId,
        event: winit::event::WindowEvent,
        failures: &mut Vec<String>,
    ) {
        use winit::{event::WindowEvent, platform::windows::KeyEventExtWindows};
        if id != self.window.id() {
            return;
        }
        let WindowEvent::KeyboardInput { event: key, is_synthetic: false, .. } = &event else {
            return;
        };
        let Some((kitty, virtual_key, scan, down, repeat)) = self.in_flight.take() else {
            failures.push("unrequested native key".into());
            return;
        };
        self.progress.set(
            "selection",
            "main/child",
            format!("native_key kitty={kitty} vk={virtual_key} down={down} repeat={repeat}"),
        );
        let native = key.native_key_event().expect("posted key must carry native metadata");
        assert_eq!(
            (native.virtual_key, native.scan_code, native.key_down),
            (virtual_key, scan, down)
        );
        assert_eq!(key.repeat, repeat);
        let physical = key.physical_key;
        let modifier = virtual_key == 0x11 || virtual_key == 0x10;
        if !modifier {
            assert!(matches!(key.logical_key, winit::keyboard::Key::Character(_)));
        }
        let copy_chord = virtual_key == 0x43;
        for (window_id, pane_id, _) in &self.targets {
            let case = if Some(*window_id) == self.app.main_window_id { "main" } else { "child" };
            self.progress.set(
                "selection",
                case,
                format!("dispatch kitty={kitty} vk={virtual_key} down={down} repeat={repeat}"),
            );
            // The native event is retained; only aggregate modifiers are controlled for copy and AltGr policy checks.
            let mods = if modifier {
                ModifiersState::empty()
            } else if copy_chord {
                ModifiersState::CONTROL | ModifiersState::SHIFT
            } else if virtual_key == 0x58 {
                ModifiersState::CONTROL | ModifiersState::ALT
            } else {
                ModifiersState::empty()
            };
            self.app.windows.get_mut(window_id).unwrap().modifiers = mods;
            let writes_before = self.app.__test_pty_write_log().len();
            winit::application::ApplicationHandler::window_event(
                &mut self.app,
                event_loop,
                *window_id,
                event.clone(),
            );
            self.progress.set(
                "selection",
                case,
                format!("check kitty={kitty} vk={virtual_key} down={down} repeat={repeat}"),
            );
            let state = &self.app.windows[window_id];
            if state.selection.is_some() != (modifier || copy_chord) {
                failures.push(format!("window={window_id:?} kitty={kitty} vk={virtual_key} down={down} repeat={repeat}: selection_present={}", state.selection.is_some()));
            }
            let held = state.pty_pressed_keys.get(&physical).and_then(|routes| routes.get(pane_id));
            assert_eq!(
                held.is_some(),
                down && !copy_chord,
                "only admitted press/repeat owns a matching release"
            );
            if down && !copy_chord {
                assert_eq!(matches!(held, Some(crate::app::HeldKey::Legacy)), kitty);
            }
            let writes = self.app.__test_pty_write_log();
            if copy_chord {
                assert_eq!(writes.len(), writes_before, "copy shortcut never leaks terminal input");
                if down {
                    assert_eq!(self.app.test_clipboard_text.as_deref(), Some("selected"));
                    self.app.test_clipboard_text = Some("clipboard sentinel".into());
                }
            } else {
                assert_eq!(writes.len(), writes_before + 1);
                let bytes = &writes.last().unwrap().1;
                if kitty {
                    assert!(
                        bytes.starts_with(b"\x1b[") && bytes.ends_with(b"u"),
                        "Kitty report-all record: {bytes:?}"
                    );
                } else {
                    assert_eq!(
                        *bytes,
                        crate::app::key_encoding::encode_win32_key(
                            crate::app::key_encoding::Win32KeyEvent {
                                virtual_key: native.virtual_key,
                                scan_code: native.scan_code,
                                unicode: &native.unicode,
                                key_down: native.key_down,
                                control_key_state: native.control_key_state,
                                repeat_count: native.repeat_count
                            }
                        )
                    );
                }
            }
        }
        if !copy_chord {
            self.completed += 1;
        }
        assert_eq!(self.app.test_clipboard_text.as_deref(), Some("clipboard sentinel"));
    }
}
