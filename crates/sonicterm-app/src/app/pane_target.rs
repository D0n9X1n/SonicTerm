//! Pane targeting for agents: find a pane by id wherever it lives, open helper panes beside it
//! without moving the user's focus, and write agent input that READONLY refuses.
//!
//! Keyboard actions act on the frontmost window's active pane. A control request names a pane,
//! so nothing here ever falls back to the active or frontmost pane: a missing target fails and
//! changes nothing.

use std::ffi::OsString;
use std::path::PathBuf;

use sonicterm_cfg::keymap::Direction;
use sonicterm_types::CommandLaunch;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::tabs::Tab;

use super::*;

/// Where a pane lives: its window, its tab, and whether that window is the main one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneLocation {
    /// The window holding the pane.
    pub(crate) window_id: WindowId,
    /// The pane's tab index within that window.
    pub(crate) tab_index: usize,
    /// Whether that window is the main window.
    pub(crate) is_main: bool,
}

/// Where a new helper pane goes, relative to its anchor pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// A split to the anchor's right, in the anchor's tab.
    Right,
    /// A split below the anchor, in the anchor's tab.
    Down,
    /// A new tab in the anchor's window.
    Tab,
}

/// What a helper pane runs: a program with arguments, or the configured shell when `argv` is
/// empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AgentLaunch {
    /// The program and its arguments; empty runs the configured shell.
    pub(crate) argv: Vec<OsString>,
    /// The working directory; `None` inherits the anchor pane's trusted OSC 7 directory.
    pub(crate) cwd: Option<PathBuf>,
    /// The tab title for a new tab; ignored for a split.
    pub(crate) title: Option<String>,
}

/// Whether the helper's process started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchOutcome {
    /// The PTY and its process started.
    Started,
    /// The process did not start; the pane is installed and held, showing why.
    Failed,
}

/// A helper pane an agent created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedPane {
    /// The new pane's id.
    pub(crate) pane_id: u64,
    /// Whether its process started.
    pub(crate) launch: LaunchOutcome,
}

/// Why a targeted pane operation did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetError {
    /// No live pane has that id.
    PaneGone,
    /// The pane's window is in READONLY, which refuses agent writes and new helpers.
    ReadOnly,
    /// The pane's input queue refused the bytes.
    InputBusy,
    /// The window could not host a new pane (no renderer yet, or the split was refused).
    Unavailable,
}

impl App {
    /// The window and tab that hold `pane_id`, searching the main window and every child.
    pub(crate) fn locate_pane(&self, pane_id: u64) -> Option<PaneLocation> {
        self.registry.windows.iter().find_map(|(window_id, window)| {
            if !window.panes.contains_key(&pane_id) {
                // When: this window's `panes` lack `pane_id`, the pane lives elsewhere.
                return None;
            }
            let tab_index = window
                .tab_states
                .iter()
                .position(|tab_state| tab_state.tree.contains_pane(pane_id))?;
            Some(PaneLocation {
                window_id: *window_id,
                tab_index,
                is_main: Some(*window_id) == self.registry.main_window_id,
            })
        })
    }

    /// Whether `pane_id`'s window is in READONLY, frontmost or not.
    pub(crate) fn pane_window_read_only(&self, pane_id: u64) -> bool {
        self.registry.windows.values().any(|window| {
            window.panes.contains_key(&pane_id)
                && window.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only)
        })
    }

    /// Open a helper pane beside `anchor` without moving the user's focus: a split in the
    /// anchor's tab, or a new tab in its window. The pane is agent-owned; a program it runs is
    /// held after it exits so its output and status stay readable. A missing anchor or a
    /// READONLY window changes nothing.
    pub(crate) fn create_pane_at(
        &mut self,
        anchor: u64,
        placement: Placement,
        agent_launch: AgentLaunch,
    ) -> Result<CreatedPane, TargetError> {
        let location = self.locate_pane(anchor).ok_or(TargetError::PaneGone)?;
        if self.pane_window_read_only(anchor) {
            // When: the anchor's window is in READONLY, no helper is opened in it.
            return Err(TargetError::ReadOnly);
        }
        let window = self.registry.windows.get(&location.window_id).ok_or(TargetError::PaneGone)?;
        // The cwd is cloned under the parser lock, which drops before any spawn. It is inherited
        // before a program is set, because `inherit_cwd` leaves command launches alone.
        let anchor_cwd = window.panes.get(&anchor).and_then(|pane| {
            crate::app::frame_counters::lock_parser(&pane.parser).osc7_cwd().cloned()
        });
        let mut launch = super::pane_launch::PaneLaunch {
            cwd: agent_launch.cwd.clone(),
            title: agent_launch.title.clone(),
            ..Default::default()
        }
        .inherit_cwd(
            anchor_cwd.as_ref(),
            sonicterm_cfg::url_scan::PathStyle::native(),
            &self.local_hostname,
        );
        // A named program runs directly and is held after it exits, so its status stays readable.
        if let Some((program, args)) = agent_launch.argv.split_first() {
            launch.command = Some(CommandLaunch { program: program.clone(), args: args.to_vec() });
            launch.hold = true;
        }
        let pane_id = next_pane_id();
        // The process starts at the size it will have once installed, so it never sees a
        // whole-window size first.
        let size = self.target_cells(location, anchor, placement, pane_id);
        let mut pane = self.spawn_target_pane(location, pane_id, &launch, size)?;
        pane.agent_owned = true;
        let outcome =
            if pane.pty.is_some() { LaunchOutcome::Started } else { LaunchOutcome::Failed };
        self.install_target_pane(location, anchor, placement, pane_id, pane, agent_launch.title)?;
        tracing::info!(
            target: "mcp",
            pane = pane_id,
            anchor,
            ?placement,
            started = outcome == LaunchOutcome::Started,
            "agent pane created"
        );
        Ok(CreatedPane { pane_id, launch: outcome })
    }

    /// Spawn a pane for `location`'s window: the main window's spawn path, or a child's with its
    /// own cell size (a headless test child gets a pane without a PTY).
    fn spawn_target_pane(
        &self,
        location: PaneLocation,
        pane_id: u64,
        launch: &super::pane_launch::PaneLaunch,
        size: Option<(u16, u16)>,
    ) -> Result<PaneState, TargetError> {
        if location.is_main {
            // When: `is_main` holds for the anchor's `location`, the main spawn path runs at `size`.
            return Ok(self.spawn_pane_sized(pane_id, launch, size));
        }
        let child = self.registry.windows.get(&location.window_id).ok_or(TargetError::PaneGone)?;
        match (child.renderer.as_ref(), child.window.as_ref()) {
            (Some(renderer), Some(window)) => {
                let (cols, rows) = size.unwrap_or_else(|| renderer.cells());
                Ok(self.spawn_pane_state_for_child(pane_id, cols, rows, window.clone(), launch))
            }
            // A headless test child has neither; it still owns a pane without a PTY.
            (None, None) => {
                let (cols, rows) = size.unwrap_or((80, 24));
                let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
                    Grid::new(cols, rows),
                    None,
                    Arc::clone(&self.capture_staging_pool),
                )));
                Ok(PaneState::new_with_media_pool(parser, None, &self.inline_media_pool))
            }
            _ => Err(TargetError::Unavailable),
        }
    }

    /// Insert `pane` beside `anchor` without changing the active tab or pane, then size every pane
    /// of the tab it joined from that tab's own layout, visible or not. A refused insert retires
    /// the pane.
    fn install_target_pane(
        &mut self,
        location: PaneLocation,
        anchor: u64,
        placement: Placement,
        pane_id: u64,
        pane: PaneState,
        title: Option<String>,
    ) -> Result<(), TargetError> {
        let Some(window) = self.registry.windows.get_mut(&location.window_id) else {
            // When: the window closed while the pane spawned, its PTY still needs retirement.
            self.retire_pane(pane);
            return Err(TargetError::PaneGone);
        };
        let tab_index = match placement {
            Placement::Right | Placement::Down => {
                // When: `placement` is a split, the anchor's tab gains the pane or refuses it.
                let direction =
                    if placement == Placement::Right { Direction::Right } else { Direction::Down };
                let Some(tab_state) = window.tab_states.get_mut(location.tab_index) else {
                    // When: `tab_states` has no `tab_index` for the anchor, the pane has no place to go.
                    self.retire_pane(pane);
                    return Err(TargetError::PaneGone);
                };
                if !tab_state.tree.split(anchor, direction, pane_id) {
                    // When: the tree refuses the split, the layout stays and the pane is retired.
                    self.retire_pane(pane);
                    return Err(TargetError::Unavailable);
                }
                location.tab_index
            }
            Placement::Tab => {
                let index = window.tab_states.len();
                window.tabs.insert_inactive(index, Tab::new(title.unwrap_or_default()));
                window.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
                index
            }
        };
        window.panes.insert(pane_id, pane);
        self.size_tab_panes(location.window_id, tab_index);
        Ok(())
    }

    /// Resize every pane of tab `tab_index` in `window_id` to its rect in that tab's own layout,
    /// so a pane added to a hidden tab wraps at its real width before anyone reads it. The
    /// visible tab also takes the window's usual topology refresh.
    fn size_tab_panes(&mut self, window_id: WindowId, tab_index: usize) {
        let is_main = Some(window_id) == self.registry.main_window_id;
        let Some(window) = self.registry.windows.get(&window_id) else {
            // When: `windows` no longer holds `window_id`, there is nothing to size.
            return;
        };
        if let Some(geometry) = self.window_geometry(window_id) {
            let rects = Self::compute_tab_pane_rects(
                window,
                tab_index,
                geometry.viewport,
                geometry.renderer,
            );
            resize_panes_to_rects(
                &window.panes,
                &rects,
                geometry.cell_w,
                geometry.cell_h,
                geometry.inset,
            );
        }
        let visible = window.tabs.active_index() == tab_index;
        if is_main {
            // Own the new pane now, as a keyboard split does, so its memory has an owner.
            self.reconcile_pane_owners();
            if visible {
                self.resize_visible_panes();
            }
        } else if let Some(window) = self.registry.windows.get_mut(&window_id) {
            // When: `windows` still holds the child, it owns the new pane now, visible or not.
            window.reconcile_pane_owners();
            if visible {
                super::child_window::resize_visible_panes_in_child(window);
                window.request_window_redraw();
            }
        }
    }

    /// The cells `pane_id` will have once installed beside `anchor`: its rect in a copy of the
    /// target tab's layout with the split or new tab applied. `None` when the window has no
    /// geometry yet, so the spawn falls back to its usual size.
    fn target_cells(
        &self,
        location: PaneLocation,
        anchor: u64,
        placement: Placement,
        pane_id: u64,
    ) -> Option<(u16, u16)> {
        let window = self.registry.windows.get(&location.window_id)?;
        let geometry = self.window_geometry(location.window_id)?;
        // A split is measured in a copy of the anchor's tab tree; a new tab is the pane alone.
        let tree = match placement {
            Placement::Right | Placement::Down => {
                let mut tree = window.tab_states.get(location.tab_index)?.tree.clone();
                let direction =
                    if placement == Placement::Right { Direction::Right } else { Direction::Down };
                tree.split(anchor, direction, pane_id).then_some(())?;
                tree
            }
            Placement::Tab => PaneTree::leaf(pane_id),
        };
        let outer = match geometry.viewport {
            Some(outer) => outer,
            None => full_pane_rect(geometry.renderer?),
        };
        let rect = tree.layout(outer).into_iter().find(|(id, _)| *id == pane_id)?.1;
        let [left, right, top, bottom] = geometry.inset;
        let content_w = (rect.w - left - right).max(geometry.cell_w);
        let content_h = (rect.h - top - bottom).max(geometry.cell_h);
        Some(sonicterm_grid::grid::bounded_grid_size(
            (content_w / geometry.cell_w).floor() as u64,
            (content_h / geometry.cell_h).floor() as u64,
        ))
    }

    /// A window's layout inputs: the headless test seam, or its renderer's cell size and padding.
    fn window_geometry(&self, window_id: WindowId) -> Option<WindowGeometry<'_>> {
        let window = self.registry.windows.get(&window_id)?;
        let (seam, renderer) = if Some(window_id) == self.registry.main_window_id {
            (self.testing.test_viewport_override, self.main_renderer())
        } else {
            // When: `window_id` is a child, its own `test_pane_viewport` and `renderer` apply.
            (window.test_pane_viewport, window.renderer.as_ref())
        };
        if let Some((outer, cell_w, cell_h)) = seam {
            // When: the `seam` is set, layout runs without a renderer and with no padding.
            return Some(WindowGeometry {
                viewport: Some(outer),
                renderer,
                cell_w,
                cell_h,
                inset: [0.0; 4],
            });
        }
        let (cell_w, cell_h, inset) = renderer_metrics(renderer?);
        Some(WindowGeometry { viewport: None, renderer, cell_w, cell_h, inset })
    }

    /// Queue `bytes` for `pane_id` as agent input. READONLY in the pane's window refuses it,
    /// whether that window is frontmost or not; nothing ever falls back to another pane.
    pub(crate) fn write_agent_input(
        &mut self,
        pane_id: u64,
        bytes: Vec<u8>,
        source: PtyInputSource,
    ) -> Result<usize, TargetError> {
        if self.locate_pane(pane_id).is_none() {
            // When: no live pane has `pane_id`, nothing is written anywhere.
            return Err(TargetError::PaneGone);
        }
        if self.pane_window_read_only(pane_id) {
            // When: `pane_window_read_only` holds for `pane_id`, READONLY refuses agent input.
            return Err(TargetError::ReadOnly);
        }
        let len = bytes.len();
        if self.write_to_pane(pane_id, bytes, source) {
            Ok(len)
        } else {
            // When: the input queue refused the bytes, report it rather than claim a write.
            Err(TargetError::InputBusy)
        }
    }
}

/// A window's layout inputs for sizing panes.
struct WindowGeometry<'a> {
    /// The headless test seam's outer rect, used instead of the renderer's surface.
    viewport: Option<sonicterm_ui::pane::Rect>,
    /// The window's renderer, when it has one.
    renderer: Option<&'a GpuRenderer>,
    /// Cell width in pixels.
    cell_w: f32,
    /// Cell height in pixels.
    cell_h: f32,
    /// Content padding: left, right, top, bottom.
    inset: [f32; 4],
}

/// The outer rect panes are laid out in for `renderer`'s surface, as `compute_tab_pane_rects`
/// derives it.
fn full_pane_rect(renderer: &GpuRenderer) -> sonicterm_ui::pane::Rect {
    let (width_px, height_px) = renderer.logical_size();
    let top = (renderer.top_inset() - renderer.padding_top_px()).max(0.0);
    let bottom = renderer.bottom_inset();
    sonicterm_ui::pane::Rect::new(0.0, top, width_px.max(0.0), (height_px - top - bottom).max(0.0))
}

/// A renderer's cell size and content padding, in the units `resize_panes_to_rects` takes.
fn renderer_metrics(renderer: &GpuRenderer) -> (f32, f32, [f32; 4]) {
    let (cell_w, cell_h) = renderer.cell_size();
    let inset = [
        renderer.padding_left_px(),
        renderer.padding_right_px(),
        renderer.padding_top_px(),
        renderer.padding_bottom_px(),
    ];
    (cell_w, cell_h, inset)
}

#[cfg(test)]
#[path = "pane_target_tests.rs"]
mod pane_target_tests;
