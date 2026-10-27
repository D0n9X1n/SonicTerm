use super::*;
pub(super) use sonicterm_cfg::config::Config;
pub(super) use sonicterm_cfg::keymap::Keymap;
pub(super) use sonicterm_cfg::theme::Theme;

/// A headless App, which has no windows, with `enabled` as the configured strip state.
pub(super) fn app_with_strip(enabled: bool) -> App {
    let mut config = Config::default();
    config.status_line.enabled = enabled;
    App::new(Theme::default(), config, Keymap::default())
}

/// The full-pass count the frame counters recorded so far.
pub(super) fn full_passes(app: &App) -> u64 {
    app.frame_counters.as_ref().map_or(0, |counters| counters.status_line.full_passes)
}

/// The platform's target open modifier: Cmd on macOS, Ctrl elsewhere.
pub(super) fn open_modifier() -> winit::keyboard::ModifiersState {
    if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    }
}

/// Seed `window_id` with a resolved hover over `https://example.com`.
pub(super) fn seed_hovered_url(app: &mut App, window_id: WindowId) {
    use sonicterm_render_model::inputs::{HoveredUrlCells, HoveredUrlSpan};
    let span = HoveredUrlSpan { row: 0, start_col: 0, end_col: 19 };
    let cells = HoveredUrlCells::new(1, [span], false).expect("one span");
    app.windows.get_mut(&window_id).unwrap().hovered_url =
        Some(super::super::hovered_url::HoveredUrl {
            cells,
            url: "https://example.com".to_owned(),
            label: None,
        });
}

/// A scratch config file holding `text`, in its own directory under the temp dir.
pub(super) fn scratch_config(case: &str, text: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("sonicterm-status-save-{case}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, text).unwrap();
    path
}

/// The ids of `resolved`'s `side` list, in order.
pub(super) fn side_ids(
    resolved: &sonicterm_cfg::status_line::ResolvedStatusLine,
    side: sonicterm_cfg::status_line::StatusSide,
) -> Vec<StatusSegmentId> {
    resolved
        .segments
        .iter()
        .filter(|segment| segment.side == side)
        .map(|segment| segment.id)
        .collect()
}

/// A strip band over raster rows 0..30 holding one inert segment and one that scrolls to the bottom.
pub(super) fn strip_over_top() -> sonicterm_gpu::status_strip::StripHit {
    use sonicterm_gpu::status_strip::PresentedStatusSegment;
    sonicterm_gpu::status_strip::StripHit {
        top: 0.0,
        height: 30.0,
        segments: vec![
            PresentedStatusSegment { id: StatusSegmentId::Mem, x: 0.0, width: 100.0, click: None },
            PresentedStatusSegment {
                id: StatusSegmentId::Scroll,
                x: 200.0,
                width: 100.0,
                click: Some(SegmentClick::Handler(SegmentHandler::ScrollToBottom)),
            },
        ],
    }
}

/// Any-motion SGR mouse reporting, so a wheel that reached the pane would write bytes to its PTY.
pub(super) fn publish_mouse_reporting(pane: &crate::app::PaneState) {
    use sonicterm_vt::vt::{MouseTracking, PointerModes};
    let modes = PointerModes::new(MouseTracking::AnyMotion, true, false, false);
    // Ordering: pointer_input is read only by this thread's handlers here.
    pane.pointer_input.store(modes.bits(), std::sync::atomic::Ordering::Relaxed);
}

/// A three-row grid holding `abc`, `def` and `ghi` on hard-broken rows.
pub(super) fn three_rows() -> Grid {
    use sonicterm_grid::grid::{CellFlags, Color};
    let mut grid = Grid::new(10, 3);
    for (row, text) in ["abc", "def", "ghi"].into_iter().enumerate() {
        grid.goto(row as u16, 0);
        for character in text.chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    grid
}

/// A macOS graphics reading whose charged figure is `charged`.
pub(super) fn task_vm_graphics(
    charged: MemoryMetric,
) -> sonicterm_logging::graphics_memory::GraphicsMemory {
    use sonicterm_logging::graphics_memory::{GraphicsMemory, GraphicsSource};
    let mut reading = GraphicsMemory::unavailable(GraphicsSource::TaskVmInfo);
    reading.footprint = charged;
    reading.nofootprint = MemoryMetric::Bytes(7 << 20);
    reading
}

/// Seed a main and a child window and give each a recording stand-in for its renderer's strip
/// setters; returns both ids, main first.
pub(super) fn seed_recorded_windows(app: &mut App) -> [WindowId; 2] {
    let child = app.__test_seed_child_window(&["work"]);
    let main = app.main_window_id.expect("the seed made a main window");
    for id in [main, child] {
        let window = app.windows.get_mut(&id).expect("a seeded window");
        window.test_strip_settings = Some(super::TestStripSettings::default());
    }
    [main, child]
}

/// What the sync handed window `id`'s strip.
pub(super) fn recorded_strip(app: &App, id: WindowId) -> super::TestStripSettings {
    app.windows[&id].test_strip_settings.clone().expect("a recorded window")
}
