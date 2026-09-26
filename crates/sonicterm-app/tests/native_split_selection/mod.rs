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

struct Probe {
    config: Config,
    outcome: Option<Result<()>>,
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.outcome.is_some() {
            return;
        }
        self.outcome = Some((|| {
            for child in [false, true] {
                for topology in [Topology::Horizontal, Topology::Vertical, Topology::Nested] {
                    run_case(event_loop, &self.config, child, topology)?;
                }
            }
            Ok(())
        })());
        event_loop.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
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
    let mut probe = Probe { config, outcome: None };
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

fn run_case(
    event_loop: &ActiveEventLoop,
    config: &Config,
    child: bool,
    topology: Topology,
) -> Result<()> {
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
    ensure!(app.__test_attach_window_renderer(id, window, renderer), "attach native renderer");
    ensure!(app.run_action(&Action::ToggleTabBar) && !app.tab_bar_visible(), "hide tab bar");
    let size = state(&app).window.as_ref().unwrap().inner_size();
    dispatch(&mut app, event_loop, id, WindowEvent::Resized(size));
    present(&mut app, event_loop, id, deadline)?;
    for (index, pane) in panes.iter().enumerate() {
        let mut parser = state(&app).panes[pane].parser.lock();
        parser.advance(
            format!(
                "\x1b[2J\x1b[H{}",
                if index == 0 { "PRESS-0123456789" } else { "FOREIGN-abcdefgh" }
            )
            .as_bytes(),
        );
    }
    present(&mut app, event_loop, id, deadline)?;
    let press = panes[0];
    let foreign = panes[1];
    let press_layout =
        state(&app).renderer.as_ref().unwrap().pane_layout(press).context("press layout")?;
    let foreign_layout =
        state(&app).renderer.as_ref().unwrap().pane_layout(foreign).context("foreign layout")?;
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
    let expected = expected_text(&app, press, (0, 6), expected_end);
    drag(&mut app, event_loop, id, start, end, deadline)?;
    assert_copy(&mut app, event_loop, id, press, &expected, deadline)?;

    // Interior padding has no hit cell; the drag still resolves through the press pane.
    let gap = match topology {
        Topology::Horizontal => (f64::from(foreign_layout.origin_x_logical - 2.0), start.1),
        Topology::Vertical | Topology::Nested => {
            (end.0, f64::from(foreign_layout.origin_y_logical - 2.0))
        }
    };
    ensure!(
        state(&app)
            .renderer
            .as_ref()
            .unwrap()
            .pixel_to_pane_cell(gap.0 as f32, gap.1 as f32)
            .is_none(),
        "fixture gap resolved to a text cell"
    );
    state_mut(&mut app).last_click_time = None;
    drag(&mut app, event_loop, id, start, gap, deadline)?;
    assert_copy(&mut app, event_loop, id, press, &expected, deadline)?;

    // The same held gesture must clamp beyond all panes rather than reuse a foreign cell.
    state_mut(&mut app).last_click_time = None;
    let outside = (f64::from(size.width) + 100.0, f64::from(size.height) + 100.0);
    let expected =
        expected_text(&app, press, (0, 6), (press_layout.rows - 1, press_layout.cols - 1));
    drag(&mut app, event_loop, id, start, outside, deadline)?;
    assert_copy(&mut app, event_loop, id, press, &expected, deadline)?;

    // Real handlers must keep the terminal press route separate from local selection.
    state(&app).panes[&press].parser.lock().advance(b"\x1b[?1002h\x1b[?1006h");
    state_mut(&mut app).last_click_time = None;
    app.__test_drain_pty_writes();
    drag(&mut app, event_loop, id, start, cell_point(press_layout, 0, 8), deadline)?;
    ensure!(state(&app).selection.is_none(), "terminal-owned press created local selection");
    let writes = app.__test_drain_pty_writes();
    ensure!(
        writes.iter().any(|(pane, bytes)| *pane == press && bytes == b"\x1b[<0;7;1M"),
        "terminal press bytes missing: {writes:?}"
    );
    ensure!(writes.iter().all(|(pane, _)| *pane == press), "terminal input targeted another pane");

    state_mut(&mut app).modifiers = ModifiersState::SHIFT;
    state_mut(&mut app).last_click_time = None;
    drag(&mut app, event_loop, id, start, end, deadline)?;
    let expected = expected_text(&app, press, (0, 6), expected_end);
    assert_copy(&mut app, event_loop, id, press, &expected, deadline)?;
    ensure!(app.__test_drain_pty_writes().is_empty(), "Shift selection leaked terminal input");
    ensure!(Instant::now() < deadline, "case deadline expired");
    println!("PASS native selection child={child} topology={topology:?} press_pane={press} foreign_pane={foreign}");
    Ok(())
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

fn drag(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    id: WindowId,
    start: (f64, f64),
    end: (f64, f64),
    deadline: Instant,
) -> Result<()> {
    pointer(app, event_loop, id, start);
    button(app, event_loop, id, ElementState::Pressed);
    if state(app).selection.is_some() {
        // Present the press range so a repeated drag endpoint is still a changed frame.
        present(app, event_loop, id, deadline)?;
    }
    pointer(app, event_loop, id, end);
    button(app, event_loop, id, ElementState::Released);
    Ok(())
}

fn present(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    id: WindowId,
    deadline: Instant,
) -> Result<()> {
    let before = state(app).renderer.as_ref().unwrap().successful_frame_count();
    loop {
        ensure!(Instant::now() < deadline, "native presentation deadline expired");
        // Backdate pacing so this selection check cannot stall on the normal frame deadline.
        app.__test_set_window_last_render(id, Instant::now() - Duration::from_secs(1));
        dispatch(app, event_loop, id, WindowEvent::RedrawRequested);
        if state(app).renderer.as_ref().unwrap().successful_frame_count() > before {
            return Ok(());
        }
        std::thread::yield_now();
    }
}

fn assert_copy(
    app: &mut App,
    event_loop: &ActiveEventLoop,
    id: WindowId,
    pane: u64,
    expected: &str,
    deadline: Instant,
) -> Result<()> {
    let selection = state(app).selection.context("selection missing")?;
    ensure!(selection.pane_id == Some(pane), "selection rebound to {:?}", selection.pane_id);
    ensure!(state(app).tab_states[0].active_pane == pane, "press did not commit pane focus");
    present(app, event_loop, id, deadline)?;
    app.__test_set_memory_clipboard("clipboard sentinel");
    ensure!(app.run_action_for_window(&Action::CopyToClipboard, id), "copy refused");
    ensure!(
        app.__test_memory_clipboard().as_deref() == Some(expected),
        "copied {:?}, expected {expected:?}",
        app.__test_memory_clipboard()
    );
    Ok(())
}
