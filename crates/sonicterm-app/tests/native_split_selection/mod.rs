use std::{
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

use anyhow::{ensure, Context, Result};
use sonicterm_app::app::{App, WindowState};
use sonicterm_cfg::{
    config::{Config, ScrollbarMode},
    keymap::{Action, Direction, Keymap},
    theme::Theme,
};
use sonicterm_gpu::core::{
    build_snapped_cell_x, GpuRenderer, PaneLayoutSnapshot, RendererSettings, SurfaceAppearance,
};
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{DeviceId, ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::ModifiersState,
    window::{Window, WindowId},
};

const DEADLINE: Duration = Duration::from_secs(180);
const CASE_DEADLINE: Duration = Duration::from_secs(20);

struct Scratch(PathBuf);

impl Scratch {
    fn new(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute(), "scratch path must be absolute");
        let parent = path.parent().context("scratch path needs a parent")?.canonicalize()?;
        ensure!(
            parent.starts_with(std::env::temp_dir().canonicalize()?),
            "scratch must be under the OS temp directory"
        );
        std::fs::create_dir(path).context("scratch directory must be new")?;
        Ok(Self(path.to_path_buf()))
    }
}

// Lifecycle: Scratch removes only the exclusive directory this probe created, after its logging guard closes.
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("native selection scratch cleanup failed: {error}");
        }
    }
}

struct Watchdog {
    cancel: mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    fn start() -> Self {
        let (cancel, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            if matches!(receiver.recv_timeout(DEADLINE), Err(mpsc::RecvTimeoutError::Timeout)) {
                eprintln!("FAIL native split selection: process deadline expired");
                std::process::abort();
            }
        });
        Self { cancel, thread: Some(thread) }
    }
}

// Lifecycle: Watchdog disarms and joins even if a native assertion unwinds; it owns no child processes.
impl Drop for Watchdog {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Topology {
    Horizontal,
    Vertical,
    Nested,
}

#[derive(Clone, Copy, Debug)]
enum DragTarget {
    Foreign,
    Gap,
    Outside,
    Shift,
}

#[derive(Clone, Copy, Debug)]
enum Stage {
    Initial,
    Seeded,
    Press(DragTarget),
    Released(DragTarget),
}

struct DragGeometry {
    start: (f64, f64),
    foreign: (f64, f64),
    gap: (f64, f64),
    outside: (f64, f64),
    terminal: (f64, f64),
    expected: String,
    outside_expected: String,
}

struct Case {
    app: App,
    id: WindowId,
    native_id: WindowId,
    panes: Vec<u64>,
    child: bool,
    topology: Topology,
    deadline: Instant,
    stage: Stage,
    geometry: Option<DragGeometry>,
    native_redraws: u64,
    attempts: u64,
    last_occluded: Option<bool>,
}

impl Case {
    fn window(&self) -> &Window {
        state(&self.app).window.as_ref().unwrap()
    }

    fn frames(&self) -> u64 {
        state(&self.app).renderer.as_ref().unwrap().successful_frame_count()
    }

    fn trace(&self, event: &str) {
        eprintln!(
            "NATIVE_SELECTION {event} native={:?} mapped={:?} child={} topology={:?} stage={:?} frames={} native_redraws={} app_redraws={} size={:?} visible={:?} last_occlusion_event={:?}",
            self.native_id,
            self.id,
            self.child,
            self.topology,
            self.stage,
            self.frames(),
            self.native_redraws,
            self.attempts,
            self.window().inner_size(),
            self.window().is_visible(),
            self.last_occluded,
        );
    }

    fn check_deadline(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            self.trace("deadline");
            if self.native_redraws == 0 || self.attempts == 0 {
                anyhow::bail!("FAIL: fixture redraw routing did not execute at {:?}", self.stage);
            }
            let verdict = if self.frames() == 0 { "BLOCKED" } else { "FAIL" };
            anyhow::bail!("{verdict}: native presentation deadline expired at {:?}; last native occlusion event {:?} does not identify the acquisition outcome", self.stage, self.last_occluded);
        }
        Ok(())
    }

    fn request_frame(&self) {
        self.window().request_redraw();
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) -> Result<bool> {
        self.check_deadline()?;
        let before = self.frames();
        self.attempts += 1;
        // Backdating bypasses App pacing without blocking the native event loop on a retry.
        self.app.__test_set_window_last_render(self.id, Instant::now() - Duration::from_secs(1));
        dispatch(&mut self.app, event_loop, self.id, WindowEvent::RedrawRequested);
        self.check_deadline()?;
        if self.frames() == before {
            // Probe owns retries because it does not run App's deferred-redraw scheduler.
            self.request_frame();
            return Ok(false);
        }
        self.trace("presented");
        match self.stage {
            Stage::Initial => {
                for (index, pane) in self.panes.iter().enumerate() {
                    let mut parser = state(&self.app).panes[pane].parser.lock();
                    parser.advance(
                        format!(
                            "\x1b[2J\x1b[H{}",
                            if index == 0 { "PRESS-0123456789" } else { "FOREIGN-abcdefgh" }
                        )
                        .as_bytes(),
                    );
                }
                self.stage = Stage::Seeded;
            }
            Stage::Seeded => {
                self.geometry = Some(drag_geometry(&self.app, &self.panes, self.topology)?);
                self.press(event_loop, DragTarget::Foreign)?;
            }
            Stage::Press(target) => {
                let geometry = self.geometry.as_ref().unwrap();
                let end = match target {
                    DragTarget::Foreign | DragTarget::Shift => geometry.foreign,
                    DragTarget::Gap => geometry.gap,
                    DragTarget::Outside => geometry.outside,
                };
                pointer(&mut self.app, event_loop, self.id, end);
                button(&mut self.app, event_loop, self.id, ElementState::Released);
                self.stage = Stage::Released(target);
            }
            Stage::Released(target) => {
                let geometry = self.geometry.as_ref().unwrap();
                let expected = match target {
                    DragTarget::Outside => &geometry.outside_expected,
                    _ => &geometry.expected,
                };
                assert_copy(&mut self.app, self.id, self.panes[0], expected)?;
                match target {
                    DragTarget::Foreign => self.press(event_loop, DragTarget::Gap)?,
                    DragTarget::Gap => self.press(event_loop, DragTarget::Outside)?,
                    DragTarget::Outside => {
                        self.check_terminal_route(event_loop)?;
                        state_mut(&mut self.app).modifiers = ModifiersState::SHIFT;
                        self.press(event_loop, DragTarget::Shift)?;
                    }
                    DragTarget::Shift => {
                        ensure!(
                            self.app.__test_drain_pty_writes().is_empty(),
                            "Shift selection leaked terminal input"
                        );
                        self.check_deadline()?;
                        println!("PASS native selection child={} topology={:?} press_pane={} foreign_pane={}", self.child, self.topology, self.panes[0], self.panes[1]);
                        return Ok(true);
                    }
                }
            }
        }
        self.request_frame();
        Ok(false)
    }

    fn press(&mut self, event_loop: &ActiveEventLoop, target: DragTarget) -> Result<()> {
        let start = self.geometry.as_ref().unwrap().start;
        state_mut(&mut self.app).last_click_time = None;
        pointer(&mut self.app, event_loop, self.id, start);
        button(&mut self.app, event_loop, self.id, ElementState::Pressed);
        ensure!(state(&self.app).selection.is_some(), "local press did not create selection");
        // Every local gesture waits for its press frame before sending motion and release.
        self.stage = Stage::Press(target);
        Ok(())
    }

    fn check_terminal_route(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let press = self.panes[0];
        let geometry = self.geometry.as_ref().unwrap();
        let (start, end) = (geometry.start, geometry.terminal);
        state(&self.app).panes[&press].parser.lock().advance(b"\x1b[?1002h\x1b[?1006h");
        state_mut(&mut self.app).last_click_time = None;
        self.app.__test_drain_pty_writes();
        pointer(&mut self.app, event_loop, self.id, start);
        button(&mut self.app, event_loop, self.id, ElementState::Pressed);
        ensure!(
            state(&self.app).selection.is_none(),
            "terminal-owned press created local selection"
        );
        pointer(&mut self.app, event_loop, self.id, end);
        button(&mut self.app, event_loop, self.id, ElementState::Released);
        let writes = self.app.__test_drain_pty_writes();
        ensure!(
            writes.iter().any(|(pane, bytes)| *pane == press && bytes == b"\x1b[<0;7;1M"),
            "terminal press bytes missing: {writes:?}"
        );
        ensure!(
            writes.iter().all(|(pane, _)| *pane == press),
            "terminal input targeted another pane"
        );
        Ok(())
    }
}

struct Probe {
    config: Config,
    cases: std::vec::IntoIter<(bool, Topology)>,
    active: Option<Case>,
    outcome: Option<Result<()>>,
}

impl Probe {
    fn finish(&mut self, event_loop: &ActiveEventLoop, outcome: Result<()>) {
        self.outcome = Some(outcome);
        self.active = None;
        event_loop.exit();
    }

    fn start_next(&mut self, event_loop: &ActiveEventLoop) {
        let Some((child, topology)) = self.cases.next() else {
            self.finish(event_loop, Ok(()));
            return;
        };
        match start_case(event_loop, &self.config, child, topology) {
            Ok(case) => {
                case.trace("created");
                case.request_frame();
                self.active = Some(case);
            }
            Err(error) => self.finish(event_loop, Err(error)),
        }
    }
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.active.is_none() && self.outcome.is_none() {
            // Return to the native event loop before waiting for the first completed frame.
            self.start_next(event_loop);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        native_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(case) = self.active.as_mut() else { return };
        if native_id != case.native_id {
            return;
        }
        if let Err(error) = case.check_deadline() {
            self.finish(event_loop, Err(error));
            return;
        }
        match event {
            WindowEvent::RedrawRequested => {
                case.native_redraws += 1;
                match case.redraw(event_loop) {
                    Ok(true) => self.active = None,
                    Ok(false) => {}
                    Err(error) => self.finish(event_loop, Err(error)),
                }
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                // The fixture App is keyed by a synthetic id, not the native window id.
                dispatch(&mut case.app, event_loop, case.id, event);
            }
            WindowEvent::Occluded(occluded) => {
                case.last_occluded = Some(occluded);
            }
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                self.finish(
                    event_loop,
                    Err(anyhow::anyhow!("native fixture window closed before completion")),
                );
            }
            // Physical input must not alter the scripted pointer sequence or memory clipboard.
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.outcome.is_some() {
            return;
        }
        if self.active.is_none() {
            self.start_next(event_loop);
        }
        if let Some(case) = self.active.as_ref() {
            if let Err(error) = case.check_deadline() {
                self.finish(event_loop, Err(error));
            } else {
                // A lost redraw still wakes at the original case deadline rather than hanging.
                event_loop
                    .set_control_flow(winit::event_loop::ControlFlow::WaitUntil(case.deadline));
            }
        }
    }
}

fn drag_geometry(app: &App, panes: &[u64], topology: Topology) -> Result<DragGeometry> {
    let press = panes[0];
    let foreign = panes[1];
    let press_layout =
        state(app).renderer.as_ref().unwrap().pane_layout(press).context("press layout")?;
    let foreign_layout =
        state(app).renderer.as_ref().unwrap().pane_layout(foreign).context("foreign layout")?;
    ensure!(press_layout.cols > 20 && press_layout.rows > 2, "fixture grid too small");
    let start = cell_point(press_layout, 0, 6);
    let end = match topology {
        Topology::Horizontal => cell_point(foreign_layout, 0, 1),
        Topology::Vertical | Topology::Nested => {
            (cell_point(press_layout, 0, 20).0, cell_point(foreign_layout, 0, 1).1)
        }
    };
    let expected_end = match topology {
        Topology::Horizontal => (0, press_layout.cols - 1),
        Topology::Vertical | Topology::Nested => (press_layout.rows - 1, 20),
    };
    // Padding has no hit cell; the gesture must still clamp through the press pane.
    let gap = match topology {
        Topology::Horizontal => (f64::from(foreign_layout.origin_x_logical - 2.0), start.1),
        Topology::Vertical | Topology::Nested => {
            (end.0, f64::from(foreign_layout.origin_y_logical - 2.0))
        }
    };
    ensure!(
        state(app)
            .renderer
            .as_ref()
            .unwrap()
            .pixel_to_pane_cell(gap.0 as f32, gap.1 as f32)
            .is_none(),
        "fixture gap resolved to a text cell"
    );
    let size = state(app).window.as_ref().unwrap().inner_size();
    Ok(DragGeometry {
        start,
        foreign: end,
        gap,
        outside: (f64::from(size.width) + 100.0, f64::from(size.height) + 100.0),
        terminal: cell_point(press_layout, 0, 8),
        expected: expected_text(app, press, (0, 6), expected_end),
        outside_expected: expected_text(
            app,
            press,
            (0, 6),
            (press_layout.rows - 1, press_layout.cols - 1),
        ),
    })
}

pub(crate) fn run(scratch_path: &Path) -> Result<()> {
    ensure!(
        std::env::var_os("NO_COLOR").is_none(),
        "remove inherited NO_COLOR before this native probe"
    );
    let scratch = Scratch::new(scratch_path)?;
    let config_dir = scratch.0.join("config");
    std::fs::create_dir(&config_dir)?;
    let config_path = config_dir.join("sonicterm.toml");
    std::fs::write(&config_path, "[window]\npadding_left = 6.0\npadding_right = 6.0\npadding_top = 6.0\npadding_bottom = 6.0\n")?;
    let mut config = Config::load_strict(&config_path)?;
    config.appearance.scrollbar = ScrollbarMode::Never;
    let _logging = sonicterm_logging::init_in(&config.logging, &scratch.0.join("logs"))?;
    let _watchdog = Watchdog::start();
    let mut builder = EventLoop::builder();
    #[cfg(windows)]
    {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        builder.with_any_thread(true);
    }
    let event_loop = builder.build()?;
    let cases = [false, true]
        .into_iter()
        .flat_map(|child| {
            [Topology::Horizontal, Topology::Vertical, Topology::Nested]
                .into_iter()
                .map(move |topology| (child, topology))
        })
        .collect::<Vec<_>>()
        .into_iter();
    let mut probe = Probe { config, cases, active: None, outcome: None };
    event_loop.run_app(&mut probe)?;
    probe.outcome.context("native event loop never resumed")??;
    println!("PASS native split selection: native windows/renderers, synthetic App pointer events, memory clipboard; no physical gesture or pixel-readback claim");
    Ok(())
}

fn state(app: &App) -> &WindowState {
    app.frontmost().expect("probe window remains tracked")
}

fn state_mut(app: &mut App) -> &mut WindowState {
    app.frontmost_mut().expect("probe window remains tracked")
}

fn start_case(
    event_loop: &ActiveEventLoop,
    config: &Config,
    child: bool,
    topology: Topology,
) -> Result<Case> {
    let deadline = Instant::now() + CASE_DEADLINE;
    let window = Arc::new(
        event_loop.create_window(
            Window::default_attributes()
                .with_inner_size(PhysicalSize::new(800, 480))
                .with_active(false)
                .with_title(format!("SonicTerm native split selection {topology:?} child={child}")),
        )?,
    );
    let theme = Theme::default();
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
            padding: [
                config.window.padding_left,
                config.window.padding_right,
                config.window.padding_top,
                config.window.padding_bottom,
            ],
            appearance: SurfaceAppearance {
                backdrop: config.appearance.backdrop,
                opacity: config.appearance.opacity,
                scrollbar: ScrollbarMode::Never,
                panel_padding: 0.0,
                software_render_mode: config.appearance.software_render_mode,
            },
            role: "native-split-selection",
        },
    )?;
    renderer.set_cursor_blink(false);
    let mut app = App::new(theme, config.clone(), Keymap::default());
    let (id, panes) = seed_tree(&mut app, child, topology)?;
    app.__test_set_frontmost_window(Some(id));
    app.__test_set_memory_clipboard("clipboard sentinel");
    app.__test_enable_pty_write_log();
    let native_id = window.id();
    ensure!(app.__test_attach_window_renderer(id, window, renderer), "attach native renderer");
    ensure!(app.run_action(&Action::ToggleTabBar) && !app.tab_bar_visible(), "hide tab bar");
    let size = state(&app).window.as_ref().unwrap().inner_size();
    dispatch(&mut app, event_loop, id, WindowEvent::Resized(size));
    Ok(Case {
        app,
        id,
        native_id,
        panes,
        child,
        topology,
        deadline,
        stage: Stage::Initial,
        geometry: None,
        native_redraws: 0,
        attempts: 0,
        last_occluded: None,
    })
}

fn seed_tree(app: &mut App, child: bool, topology: Topology) -> Result<(WindowId, Vec<u64>)> {
    let id = if child {
        app.__test_seed_child_window(&["press", "foreign", "outer"])
    } else {
        for name in ["press", "foreign", "outer"] {
            app.__test_seed_tab(name);
        }
        app.__test_main_window_id().context("main id")?
    };
    app.__test_set_frontmost_window(Some(id));
    let state = state_mut(app);
    let panes: Vec<_> = state.tab_states.iter().map(|tab| tab.active_pane).collect();
    let mut tree = sonicterm_ui::pane::PaneTree::leaf(panes[0]);
    match topology {
        Topology::Horizontal => {
            ensure!(tree.split(panes[0], Direction::Right, panes[1]), "horizontal split")
        }
        Topology::Vertical => {
            ensure!(tree.split(panes[0], Direction::Down, panes[1]), "vertical split")
        }
        Topology::Nested => {
            ensure!(tree.split(panes[0], Direction::Left, panes[2]), "outer split");
            ensure!(tree.split(panes[0], Direction::Down, panes[1]), "nested split");
        }
    }
    // Keep one tab/state pair while making the first press switch from the foreign pane.
    state.tabs.activate(0);
    state.tab_states[0].tree = tree;
    state.tab_states[0].active_pane = panes[1];
    state.tab_states.truncate(1);
    let extras: Vec<_> = state.tabs.tabs().iter().skip(1).map(|tab| tab.id).collect();
    for id in extras {
        state.tabs.close(id);
    }
    if !matches!(topology, Topology::Nested) {
        state.panes.remove(&panes[2]);
    }
    Ok((id, panes[..if matches!(topology, Topology::Nested) { 3 } else { 2 }].to_vec()))
}

fn cell_point(layout: PaneLayoutSnapshot, row: u16, col: u16) -> (f64, f64) {
    let edges = build_snapped_cell_x(layout.origin_x_logical, layout.cell_w_logical, layout.cols);
    (
        f64::from((edges[usize::from(col)] + edges[usize::from(col) + 1]) * 0.5),
        f64::from(layout.origin_y_logical + (f32::from(row) + 0.5) * layout.cell_h_logical),
    )
}

fn expected_text(app: &App, pane: u64, start: (u16, u16), end: (u16, u16)) -> String {
    let parser = state(app).panes[&pane].parser.lock();
    let grid = parser.grid();
    // Fresh panes received no scrolling output, so viewport rows are absolute rows here.
    (start.0..=end.0)
        .map(|row| {
            let first = if row == start.0 { start.1 } else { 0 };
            let last = if row == end.0 { end.1 } else { grid.cols - 1 };
            let cells = grid.row_at_abs(u64::from(row)).expect("fixture row");
            cells
                .iter()
                .skip(usize::from(first))
                .take(usize::from(last - first + 1))
                .map(|cell| cell.ch)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn dispatch(app: &mut App, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
    ApplicationHandler::window_event(app, event_loop, id, event);
}

fn pointer(app: &mut App, event_loop: &ActiveEventLoop, id: WindowId, point: (f64, f64)) {
    dispatch(
        app,
        event_loop,
        id,
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(point.0, point.1),
        },
    );
}

fn button(app: &mut App, event_loop: &ActiveEventLoop, id: WindowId, state: ElementState) {
    dispatch(
        app,
        event_loop,
        id,
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left },
    );
}

fn assert_copy(app: &mut App, id: WindowId, pane: u64, expected: &str) -> Result<()> {
    let selection = state(app).selection.context("selection missing")?;
    ensure!(selection.pane_id == Some(pane), "selection rebound to {:?}", selection.pane_id);
    ensure!(state(app).tab_states[0].active_pane == pane, "press did not commit pane focus");
    app.__test_set_memory_clipboard("clipboard sentinel");
    ensure!(app.run_action_for_window(&Action::CopyToClipboard, id), "copy refused");
    ensure!(
        app.__test_memory_clipboard().as_deref() == Some(expected),
        "copied {:?}, expected {expected:?}",
        app.__test_memory_clipboard()
    );
    Ok(())
}
