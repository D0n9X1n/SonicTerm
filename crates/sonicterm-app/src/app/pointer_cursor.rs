//! The native pointer cursor of each window, decided in one place: the UI owners first (an
//! overlay, a splitter, the scrollbar, the tab bar, the titlebar band, a modifier link hover, a
//! local selection), else the OSC 22 shape of the terminal pane under the pointer; a report
//! window's pane shows the arrow.
//!
//! The decision runs once per event-loop turn for the window under the pointer, after input,
//! output and owner changes, and writes to the OS only when its result changed. Only that window
//! writes: Windows sets the cursor for the whole thread, so another window's write would replace
//! the icon of the window the pointer is over. A window whose hidden owners may have gone stale
//! (its overlay closed, the pointer left, a resize, a scale or focus change) is reclassified from
//! the pointer's current position and geometry before its decision.

use sonicterm_ui::pane::SplitAxis;
use sonicterm_vt::vt::PointerShape;
use winit::window::{CursorIcon, WindowId};

use super::{App, PointerGestureOwner};

/// What claims one window's pointer at the moment of a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct CursorOwners {
    /// A modal overlay (the command palette) is open over this window.
    pub(in crate::app) overlay: bool,
    /// A splitter is dragged or hovered, along this axis.
    pub(in crate::app) splitter: Option<SplitAxis>,
    /// The scrollbar is dragged or owns the pointer in its gutter.
    pub(in crate::app) scrollbar: bool,
    /// A tab is pressed or dragged.
    pub(in crate::app) tab_bar: bool,
    /// The pointer is on a titlebar-band resize edge, which shows this icon.
    pub(in crate::app) band_edge: Option<CursorIcon>,
    /// A modifier hover shows an openable link or path.
    pub(in crate::app) link: bool,
    /// A local text selection is being dragged.
    pub(in crate::app) selection: bool,
    /// The effective shape of the pane under the pointer; `None` over no pane.
    pub(in crate::app) pane_shape: Option<PointerShape>,
}

/// The cursor `owners` select, highest owner first; the arrow over no pane.
pub(in crate::app) fn decide_cursor(owners: CursorOwners) -> CursorIcon {
    if owners.overlay {
        // When: an overlay is open, it owns the pointer and shows the arrow.
        return CursorIcon::Default;
    }
    if let Some(axis) = owners.splitter {
        // When: a splitter is dragged or hovered, its resize cursor shows along `axis`.
        return match axis {
            SplitAxis::Vertical => CursorIcon::ColResize,
            SplitAxis::Horizontal => CursorIcon::RowResize,
        };
    }
    if owners.scrollbar || owners.tab_bar {
        // When: the scrollbar or a held tab owns the pointer, the arrow shows.
        return CursorIcon::Default;
    }
    if let Some(icon) = owners.band_edge {
        // When: band_edge names the resize edge under the pointer, that edge's cursor shows.
        return icon;
    }
    if owners.link {
        // When: link is set, a modifier hover shows an openable target, so the hand shows.
        return CursorIcon::Pointer;
    }
    if owners.selection {
        // When: a local selection is dragged, the I-beam shows over every pane.
        return CursorIcon::Text;
    }
    owners.pane_shape.map_or(CursorIcon::Default, cursor_icon_for)
}

/// The winit icon of `shape`: each W3C shape has its own, and a platform without one draws
/// winit's fallback.
pub(in crate::app) fn cursor_icon_for(shape: PointerShape) -> CursorIcon {
    match shape {
        PointerShape::Default => CursorIcon::Default,
        PointerShape::ContextMenu => CursorIcon::ContextMenu,
        PointerShape::Help => CursorIcon::Help,
        PointerShape::Pointer => CursorIcon::Pointer,
        PointerShape::Progress => CursorIcon::Progress,
        PointerShape::Wait => CursorIcon::Wait,
        PointerShape::Cell => CursorIcon::Cell,
        PointerShape::Crosshair => CursorIcon::Crosshair,
        PointerShape::Text => CursorIcon::Text,
        PointerShape::VerticalText => CursorIcon::VerticalText,
        PointerShape::Alias => CursorIcon::Alias,
        PointerShape::Copy => CursorIcon::Copy,
        PointerShape::Move => CursorIcon::Move,
        PointerShape::NoDrop => CursorIcon::NoDrop,
        PointerShape::NotAllowed => CursorIcon::NotAllowed,
        PointerShape::Grab => CursorIcon::Grab,
        PointerShape::Grabbing => CursorIcon::Grabbing,
        PointerShape::AllScroll => CursorIcon::AllScroll,
        PointerShape::ColResize => CursorIcon::ColResize,
        PointerShape::RowResize => CursorIcon::RowResize,
        PointerShape::NResize => CursorIcon::NResize,
        PointerShape::EResize => CursorIcon::EResize,
        PointerShape::SResize => CursorIcon::SResize,
        PointerShape::WResize => CursorIcon::WResize,
        PointerShape::NeResize => CursorIcon::NeResize,
        PointerShape::NwResize => CursorIcon::NwResize,
        PointerShape::SeResize => CursorIcon::SeResize,
        PointerShape::SwResize => CursorIcon::SwResize,
        PointerShape::EwResize => CursorIcon::EwResize,
        PointerShape::NsResize => CursorIcon::NsResize,
        PointerShape::NeswResize => CursorIcon::NeswResize,
        PointerShape::NwseResize => CursorIcon::NwseResize,
        PointerShape::ZoomIn => CursorIcon::ZoomIn,
        PointerShape::ZoomOut => CursorIcon::ZoomOut,
    }
}

/// One window's cursor inputs that no other state records, and the icon last written.
#[derive(Debug, Default)]
pub(crate) struct NativeCursorState {
    /// The titlebar band's resize-edge icon under the pointer, if any.
    pub(in crate::app) band_edge: Option<CursorIcon>,
    /// The last no-button motion found the pointer in a pane's scrollbar gutter.
    pub(in crate::app) on_scrollbar: bool,
    /// The icon last written to the window; `None` before the first decision.
    pub(in crate::app) applied: Option<CursorIcon>,
    /// The splitter, scrollbar and band-edge owners are reclassified at the next decision.
    pub(in crate::app) reclassify: bool,
    /// The palette was open over this window at the last decision; its closing reclassifies.
    pub(in crate::app) overlay_shown: bool,
    /// Decisions that changed `applied`, each standing for one OS write.
    #[cfg(test)]
    pub(in crate::app) applied_writes: u32,
}

impl App {
    /// Follow which window owns the pointer: the one it entered or moved in inside its client area,
    /// until it leaves that window. A leave, a resize, a scale or a focus change marks
    /// `window_id`'s owners stale.
    pub(in crate::app) fn track_pointer_owner(
        &mut self,
        window_id: WindowId,
        event: &winit::event::WindowEvent,
    ) {
        use winit::event::WindowEvent;
        match event {
            WindowEvent::CursorEntered { .. } => {
                self.interaction.pointer_window = Some(window_id);
            }
            WindowEvent::CursorMoved { position, .. } => {
                // A captured drag keeps reporting moves to its source from outside the client area
                // after `CursorLeft`; such a move keeps its routing but claims no cursor.
                if self.pointer_in_client(window_id, (position.x, position.y)) {
                    self.interaction.pointer_window = Some(window_id);
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.interaction.pointer_window == Some(window_id) {
                    // The owner itself is left; a leave reported after another window's enter
                    // keeps that window the owner.
                    self.interaction.pointer_window = None;
                }
                self.mark_cursor_stale(window_id);
            }
            WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. }
            | WindowEvent::Focused(_) => self.mark_cursor_stale(window_id),
            _ => {
                // When: any other event, the pointer's owner and the hover owners stand.
            }
        }
    }

    /// Whether `position` lies in `window_id`'s client area: not negative, and inside its native
    /// inner size when it has a native window (a headless window has no far edge to check).
    fn pointer_in_client(&self, window_id: WindowId, position: (f64, f64)) -> bool {
        let (pointer_x, pointer_y) = position;
        if pointer_x < 0.0 || pointer_y < 0.0 {
            // When: pointer_x or pointer_y is negative, the pointer is left of or above the client area.
            return false;
        }
        let native =
            self.registry.windows.get(&window_id).and_then(|window| window.window.as_ref());
        native.is_none_or(|native| {
            let size = native.inner_size();
            pointer_x < f64::from(size.width) && pointer_y < f64::from(size.height)
        })
    }

    /// Reclassify `window_id`'s splitter, scrollbar and band-edge owners at its next decision.
    pub(in crate::app) fn mark_cursor_stale(&mut self, window_id: WindowId) {
        if let Some(window) = self.registry.windows.get_mut(&window_id) {
            window.native_cursor.reclassify = true;
        }
    }

    /// Recompute `window_id`'s splitter hover, scrollbar gutter and band edge from the pointer's
    /// recorded position and the current geometry, so an owner a modal, a leave or a geometry
    /// change hid never outlives the position that made it.
    fn reclassify_pointer_owners(&mut self, window_id: WindowId) {
        let Some(window) = self.registry.windows.get_mut(&window_id) else {
            // When: window_id closed, it has no owners left to reclassify.
            return;
        };
        window.native_cursor.reclassify = false;
        let (pointer_x, pointer_y) = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
        if pointer_x < 0.0 || pointer_y < 0.0 {
            // When: pointer_x or pointer_y is negative, the pointer is off the window and on no owner.
            window.splitter_hover = None;
            window.native_cursor.on_scrollbar = false;
            window.native_cursor.band_edge = None;
            return;
        }
        window.native_cursor.band_edge =
            match super::titlebar_band::classify_band_press(window, pointer_x, pointer_y, false) {
                super::titlebar_band::BandPress::Edge(edge) => {
                    Some(super::titlebar_band::edge_cursor(edge))
                }
                _ => None,
            };
        if Some(window_id) == self.registry.main_window_id {
            self.refresh_splitter_hover(pointer_x, pointer_y);
        } else {
            // When: window_id is a child, its own layout places the splitters.
            self.refresh_child_splitter_hover(window_id, pointer_x, pointer_y);
        }
        let on_scrollbar = self.pointer_on_scrollbar(window_id, pointer_x, pointer_y);
        if let Some(window) = self.registry.windows.get_mut(&window_id) {
            window.native_cursor.on_scrollbar = on_scrollbar;
        }
    }

    /// Whether the pointer at (`pointer_x`, `pointer_y`) in `window_id` is in the scrollbar gutter
    /// of the pane under it, by the one calculation main and child motion use.
    pub(in crate::app) fn pointer_on_scrollbar(
        &self,
        window_id: WindowId,
        pointer_x: f32,
        pointer_y: f32,
    ) -> bool {
        let Some(window) = self.registry.windows.get(&window_id) else {
            // When: window_id closed, no gutter is under the pointer.
            return false;
        };
        let pane = if Some(window_id) == self.registry.main_window_id {
            self.pane_at_cursor(pointer_x, pointer_y).and_then(|pane_id| {
                let rect = self
                    .compute_active_pane_rects()
                    .into_iter()
                    .find_map(|(id, rect)| (id == pane_id).then_some(rect))?;
                Some((pane_id, rect))
            })
        } else {
            // When: window_id is a child, its own layout places the pane.
            super::child_window_pointer::child_pane_cell_at(window, pointer_x, pointer_y).and_then(
                |(pane_id, _, _)| App::pane_rect_for(window, pane_id).map(|rect| (pane_id, rect)),
            )
        };
        let Some((pane_id, rect)) = pane else {
            // When: no pane is under the pointer, no gutter is either.
            return false;
        };
        let (edge_active, visible) =
            window.scrollbar_vis.get(&pane_id).map_or((false, false), |state| {
                (
                    state.mouse_near_right_edge,
                    state.alpha > crate::app::scrollbar_visibility::ALPHA_EMIT_FLOOR,
                )
            });
        super::window_event::pane_scrollbar_owns_pointer(
            self.config.appearance.scrollbar,
            rect,
            window.renderer.as_ref().map(super::window_event::ScrollbarMetrics::of),
            (pointer_x, pointer_y),
            edge_active,
            visible,
        )
    }

    /// The cursor window `window_id` should show now, from its owners and the hovered pane's
    /// published shape (one atomic load, no parser lock); `None` for a closed window.
    fn window_cursor(&self, window_id: WindowId) -> Option<CursorIcon> {
        let window = self.registry.windows.get(&window_id)?;
        let (pointer_x, pointer_y) = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
        let hovered = if window.role.is_report() {
            // A report window's (About, Memory Usage) pane is no terminal, so no pane shape
            // applies and the arrow shows.
            None
        } else if Some(window_id) == self.registry.main_window_id {
            // When: window_id is the main window, its pane layout resolves the hovered cell.
            self.main_pane_cell_at(pointer_x, pointer_y)
        } else {
            // When: window_id is a child terminal window, its own renderer lays out the panes.
            super::child_window_pointer::child_pane_cell_at(window, pointer_x, pointer_y)
        };
        let pane_shape = hovered
            .and_then(|(pane_id, _, _)| window.panes.get(&pane_id))
            .map(|pane| pane.pointer_shape());
        Some(decide_cursor(CursorOwners {
            overlay: self.palette_window() == Some(window_id),
            splitter: window.splitter_drag.as_ref().map(|drag| drag.axis).or(window.splitter_hover),
            scrollbar: window.scrollbar_drag.is_some() || window.native_cursor.on_scrollbar,
            tab_bar: window.drag_session.is_some() || window.pressed_tab.is_some(),
            band_edge: window.native_cursor.band_edge,
            link: window.hover_link,
            selection: window.mouse_down
                && window
                    .pointer_gesture
                    .is_some_and(|gesture| gesture.owner == PointerGestureOwner::Local),
            pane_shape,
        }))
    }

    /// Reclassify every window whose owners went stale, then decide the cursor of the window under
    /// the pointer and write it when the decision changed; the one place that sets a native cursor.
    pub(in crate::app) fn sync_window_cursors(&mut self) {
        let palette_window = self.palette_window();
        // Empty in the common case, so a turn with nothing stale allocates nothing.
        let stale: Vec<WindowId> = self
            .registry
            .windows
            .iter()
            .filter(|(window_id, window)| {
                let overlay_closed =
                    window.native_cursor.overlay_shown && palette_window != Some(**window_id);
                window.native_cursor.reclassify || overlay_closed
            })
            .map(|(window_id, _)| *window_id)
            .collect();
        for window_id in stale {
            self.reclassify_pointer_owners(window_id);
        }
        for (window_id, window) in self.registry.windows.iter_mut() {
            window.native_cursor.overlay_shown = palette_window == Some(*window_id);
        }
        let Some(window_id) = self.interaction.pointer_window else {
            // When: pointer_window is None, the pointer is over no client window and the OS owns it.
            return;
        };
        let Some(icon) = self.window_cursor(window_id) else {
            // When: window_cursor finds no icon, the pointer's window closed and nothing is written.
            return;
        };
        let Some(window) = self.registry.windows.get_mut(&window_id) else {
            // When: window_id closed since the decision, nothing is left to write.
            return;
        };
        if window.native_cursor.applied == Some(icon) {
            // When: the window already shows this icon, the OS needs no write.
            return;
        }
        window.native_cursor.applied = Some(icon);
        #[cfg(test)]
        {
            window.native_cursor.applied_writes += 1;
        }
        if let Some(native) = window.window.as_ref() {
            native.set_cursor(icon);
        }
    }
}

#[cfg(test)]
#[path = "pointer_cursor_tests.rs"]
mod pointer_cursor_tests;
