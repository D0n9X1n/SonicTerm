//! Tab-bar pointer routing shared by the main and child pointer handlers.
//!
//! `WindowState::route_tab_press`, `route_tab_motion` and `route_tab_release`
//! take the drawn bar layout, the pointer position and the window's gesture
//! state (`mouse_down`, `pressed_tab`, `drag_session`, `drag_target`), update
//! that state and return the action. `App::apply_tab_press`,
//! `apply_tab_motion` and `apply_tab_release` carry the action out. The
//! handlers supply only the native inputs: the bar layout, built from the
//! native window size for the main window and from the renderer for a child,
//! and the event loop a tear-out needs.

use sonicterm_ui::drag_chip::DragChipOverlay;
use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use winit::window::WindowId;

use super::child_window::resize_visible_panes_in_child;
use super::{App, WindowState};
use crate::tab_drag::{DragAction, DragSession};

/// What a left press on a tab bar does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TabPress {
    /// The press missed the bar; pane input handles it.
    Miss,
    /// Select the tab at this index; the press is recorded for a drag.
    Activate(usize),
    /// Close the tab at this index.
    Close(usize),
    /// Open the overflow selector; no drag starts.
    OpenSelector,
}

/// What a pointer move does to a window's tab gesture.
pub(super) enum TabMotion {
    /// No tab is held; the move goes on to pane input.
    Idle,
    /// The held tab closed; the gesture stops.
    Cancel,
    /// A tab is held: draw its chip, and follow the drop target while the button is down.
    Drag {
        /// The drag chip to draw, or `None` below the drag threshold.
        chip: Option<DragChipOverlay>,
        /// Whether the button is down on a pressed tab, so the move follows the drop target.
        seek_target: bool,
    },
    /// The button is down on a pressed tab without a drag session; only the drop target moves.
    Seek,
}

/// What a left release does to a window's tab gesture.
#[derive(Debug, Clone, Copy)]
pub(super) enum TabRelease {
    /// No tab was pressed and dragged, or no bar is drawn to resolve the drop against.
    Idle,
    /// The pressed tab closed during the gesture; cancel it rather than move a neighbour.
    Cancel,
    /// Finish the drag: return, reorder, merge or tear out the tab.
    Finish(DragSession<WindowId>, DragAction<WindowId>),
}

impl WindowState {
    /// Route a left press at `pointer` against the drawn bar `layout`. A press on a tab
    /// records it for a drag, and a press on the overflow control starts none.
    pub(super) fn route_tab_press(
        &mut self,
        window_id: WindowId,
        layout: &TabBarLayout,
        pointer: (f32, f32),
    ) -> TabPress {
        match layout.hit(pointer.0, pointer.1) {
            None => TabPress::Miss,
            Some(TabHit::Activate(tab_index)) => {
                self.begin_tab_press(window_id, tab_index, pointer);
                TabPress::Activate(tab_index)
            }
            Some(TabHit::Close(tab_index)) => TabPress::Close(tab_index),
            Some(TabHit::Overflow) => {
                self.mouse_down = false;
                self.pressed_tab = None;
                self.drag_session = None;
                TabPress::OpenSelector
            }
        }
    }

    /// Route a pointer move to `pointer`. A held tab's drag session follows the pointer and
    /// its chip is built against `layout`, the drawn bar, which the caller computes only
    /// while a session is held. A held tab that closed cancels the gesture.
    pub(super) fn route_tab_motion(
        &mut self,
        layout: Option<&TabBarLayout>,
        pointer: (f32, f32),
    ) -> TabMotion {
        let seek_target = self.mouse_down && self.pressed_tab.is_some();
        let Some(session) = self.drag_session.as_mut() else {
            // When: `drag_session` is None, only a pressed tab still follows a drop target.
            return if seek_target { TabMotion::Seek } else { TabMotion::Idle };
        };
        session.current_pos = pointer;
        let session = *session;
        let Some((source_index, tab)) =
            self.tabs.tabs().iter().enumerate().find(|(_, tab)| tab.id == session.source_tab)
        else {
            // When: no tab has `session.source_tab`, so the held tab closed; stop, never show a successor.
            return TabMotion::Cancel;
        };
        let chip = layout.and_then(|drawn| {
            crate::tab_drag::build_drag_chip_overlay(
                &session,
                drawn,
                source_index,
                tab.title.clone(),
            )
        });
        TabMotion::Drag { chip, seek_target }
    }

    /// Route a left release against the drawn bar `layout`, or `None` when no bar is drawn.
    /// The gesture ends, and a dragged tab's drop is decided at the pointer position its
    /// last move recorded, which is where the release lands.
    pub(super) fn route_tab_release(&mut self, layout: Option<&TabBarLayout>) -> TabRelease {
        let (session, foreign, pressed) = self.end_tab_press();
        let (Some(session), Some(_)) = (session, pressed) else {
            // When: `session` or `pressed` is None, no tab was pressed and dragged, so nothing moves.
            return TabRelease::Idle;
        };
        let Some(source_index) =
            self.tabs.tabs().iter().position(|tab| tab.id == session.source_tab)
        else {
            // When: no tab has `session.source_tab`, so the pressed tab closed; cancel, never move a neighbour.
            return TabRelease::Cancel;
        };
        let Some(drawn) = layout else {
            // When: `layout` is None, no bar is drawn, so no slot resolves the drop.
            return TabRelease::Idle;
        };
        let action = crate::tab_drag::compute_action(&session, foreign, drawn, source_index);
        TabRelease::Finish(session, action)
    }
}

impl App {
    /// Carry out a routed press on `window`'s bar: select the tab, close it or open the
    /// overflow selector. The main window and a child select and close through their own paths.
    pub(super) fn apply_tab_press(&mut self, window: WindowId, press: TabPress) {
        let is_main = Some(window) == self.main_window_id;
        match press {
            TabPress::Miss => {
                // When: `press` is Miss, the pointer missed the bar, so nothing on it changes.
            }
            TabPress::Activate(tab_index) => {
                if is_main {
                    self.activate_main_tab(tab_index);
                } else if let Some(state) = self.windows.get_mut(&window) {
                    // When: `window` is a child still in `windows`; it selects its own tab and resizes its panes.
                    state.tabs.activate(tab_index);
                    resize_visible_panes_in_child(state);
                }
            }
            TabPress::Close(tab_index) => {
                if is_main {
                    self.close_tab_at(tab_index);
                } else {
                    // When: `is_main` is false, so the child `window` closes its own tab.
                    self.close_tab_at_in_child(window, tab_index);
                }
            }
            TabPress::OpenSelector => self.open_tab_selector(window),
        }
    }

    /// Carry out a routed pointer move on `window`, at `position` in its window pixels: draw
    /// the held tab's chip, follow the drop target across windows while the button is down,
    /// and on the main window hand the drag to the OS once it passes the threshold. Returns
    /// whether the move belongs to the tab gesture, so pane input must not see it.
    pub(super) fn apply_tab_motion(
        &mut self,
        window: WindowId,
        motion: TabMotion,
        position: (f64, f64),
    ) -> bool {
        let seek_target = match motion {
            TabMotion::Idle => {
                // When: `motion` is Idle, no tab is held, so the move is pane input.
                return false;
            }
            TabMotion::Cancel => {
                // When: `motion` is Cancel, the held tab closed, so the gesture stops before pane input.
                self.cancel_drag_session();
                return true;
            }
            TabMotion::Drag { chip, seek_target } => {
                if let Some(renderer) =
                    self.windows.get_mut(&window).and_then(|state| state.renderer.as_mut())
                {
                    renderer.set_drag_chip(chip);
                }
                seek_target
            }
            TabMotion::Seek => true,
        };
        if !seek_target {
            // When: `seek_target` is false, the button is up, so pane input runs.
            return false;
        }
        let is_main = Some(window) == self.main_window_id;
        let target = if is_main {
            self.compute_main_drag_target(position)
        } else {
            // When: `is_main` is false, so drop targets are searched from the child `window`.
            self.compute_child_drag_target(window, position)
        };
        if let Some(state) = self.windows.get_mut(&window) {
            state.drag_target = target;
        }
        if is_main && !self.os_drag_handoff_started {
            // When: `is_main` and `os_drag_handoff_started` is false, test once whether the drag crossed the OS threshold.
            let started_index = self.windows.get(&window).and_then(|state| {
                state
                    .drag_session
                    .as_ref()
                    .filter(|session| crate::tab_drag::drag_moved_enough(session))
                    .and_then(|session| {
                        self.tab_index_of_id(session.source_window, session.source_tab)
                    })
            });
            if let Some(index) = started_index {
                // When: `started_index` is Some, hand this tab gesture to the OS backend once.
                self.os_drag_handoff_started = true;
                let _ = self.try_os_drag_handoff(index);
            }
        }
        if let Some(state) = self.windows.get(&window) {
            state.request_redraw();
        }
        true
    }

    /// Carry out a routed release: cancel the gesture, or finish the drag by returning,
    /// reordering, merging or tearing out the tab. `tear_out` opens the new window and needs
    /// the handler's event loop.
    pub(super) fn apply_tab_release(
        &mut self,
        release: TabRelease,
        tear_out: impl FnOnce(&mut Self, WindowId, usize),
    ) {
        match release {
            TabRelease::Idle => {
                // When: `release` is Idle, no tab was pressed and dragged, so nothing moves.
            }
            TabRelease::Cancel => {
                self.cancel_drag_session();
            }
            TabRelease::Finish(session, action) => {
                self.finish_tab_drag(session, action, tear_out);
            }
        }
    }
}
