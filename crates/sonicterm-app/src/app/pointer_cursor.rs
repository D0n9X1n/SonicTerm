//! The native pointer cursor of each window, decided in one place: the UI owners first (an
//! overlay, a splitter, the scrollbar, the tab bar, the titlebar band, a modifier link hover, a
//! local selection), else the OSC 22 shape of the terminal pane under the pointer; a report
//! window's pane shows the arrow.
//!
//! The decision runs once per event-loop turn for every window, after input, output and owner
//! changes, and writes to the OS only when its result changed.

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
    /// Decisions that changed `applied`, each standing for one OS write.
    #[cfg(test)]
    pub(in crate::app) applied_writes: u32,
}

impl App {
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
        let palette_window =
            self.interaction.palette_attached_window.or(self.registry.main_window_id);
        Some(decide_cursor(CursorOwners {
            overlay: self.interaction.command_palette.is_open()
                && palette_window == Some(window_id),
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

    /// Decide every window's cursor and write each one whose decision changed; the one place
    /// that sets a native cursor.
    pub(in crate::app) fn sync_window_cursors(&mut self) {
        // Empty in the common unchanged case, so a turn with nothing to write allocates nothing.
        let changed: Vec<(WindowId, CursorIcon)> = self
            .registry
            .windows
            .iter()
            .filter_map(|(window_id, window)| {
                let icon = self.window_cursor(*window_id)?;
                (window.native_cursor.applied != Some(icon)).then_some((*window_id, icon))
            })
            .collect();
        for (window_id, icon) in changed {
            let Some(window) = self.registry.windows.get_mut(&window_id) else {
                // When: window_id closed since the decision, nothing is left to write.
                continue;
            };
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
}

#[cfg(test)]
#[path = "pointer_cursor_tests.rs"]
mod pointer_cursor_tests;
