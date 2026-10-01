//! Mouse selection inside the command-palette query and the search query.
//!
//! One handler owns a left-button gesture that starts inside either query
//! field. It runs before the palette modal and the terminal handlers, maps the
//! pointer through the renderer's presented field geometry, and keeps the
//! gesture bound to the source window and field until the paired release. The
//! release of a consumed press is always consumed too, so neither the palette
//! rows nor the PTY ever see half of a click.

use sonicterm_gpu::field_geometry::{FieldHit, FieldHitMode};
use sonicterm_ui::command_palette::CommandPaletteMode;
use sonicterm_ui::tabs::TabId;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::window::WindowId;

use super::App;

#[cfg(test)]
#[path = "field_pointer_tests.rs"]
mod field_pointer_tests;

/// Which query field a gesture belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FieldTarget {
    /// The command-palette query of one editor session, in the mode the press saw.
    ///
    /// `session` changes on every open, so close-and-reopen ends the gesture. A
    /// rename editor is also live only while its captured tab or window still is.
    Palette {
        /// Palette mode the press landed in.
        mode: CommandPaletteMode,
        /// Editor identity from [`sonicterm_ui::command_palette::CommandPalette::session`].
        session: u64,
    },
    /// The search query of one tab.
    Search(TabId),
}

/// A held query-field gesture, or the release still owed to one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum FieldPointerCapture {
    /// A press inside a field, extending the selection until release.
    Dragging {
        /// Window that received the press.
        window_id: WindowId,
        /// Field the press landed in.
        target: FieldTarget,
        /// Press position in physical pixels.
        press: (f32, f32),
        /// Whether Shift extended from the existing caret.
        extend: bool,
        /// Whether the press has been mapped to a caret yet.
        anchored: bool,
    },
    /// A cancelled or unmappable press whose left release must still be consumed.
    Swallow {
        /// Window whose release is owed.
        window_id: WindowId,
    },
}

/// Maps a physical-pixel point onto a presented field for one window.
pub(super) type FieldHitResolver =
    fn(&App, WindowId, FieldTarget, (f32, f32), FieldHitMode) -> FieldHit;

impl App {
    /// Route one window event through the query-field pointer handler.
    ///
    /// Returns true when the event belongs to a field gesture and must not
    /// reach the palette modal or the terminal.
    pub(super) fn field_pointer_event(&mut self, window_id: WindowId, event: &WindowEvent) -> bool {
        self.field_pointer_event_with(window_id, event, App::renderer_field_hit)
    }

    /// [`Self::field_pointer_event`] with an injected hit resolver.
    pub(super) fn field_pointer_event_with(
        &mut self,
        window_id: WindowId,
        event: &WindowEvent,
        resolve: FieldHitResolver,
    ) -> bool {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.field_pointer_press(window_id, resolve),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.field_pointer_release(window_id, resolve),
            WindowEvent::CursorMoved { .. } => self.field_pointer_move(window_id, resolve),
            WindowEvent::CursorLeft { .. } => {
                // A held or cancelled gesture keeps its leave away from the terminal; the next
                // move clamps a live drag, and only the source window's leave is owed.
                self.field_capture_window() == Some(window_id)
                    || self.field_owed_releases.contains(&window_id)
            }
            WindowEvent::Focused(false)
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. } => {
                // Input or layout changes end the drag; the event itself still reaches its handler.
                self.cancel_field_pointer(window_id);
                false
            }
            WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                // A closing window will never deliver the owed release.
                self.drop_field_pointer_for_window(window_id);
                false
            }
            _ => false,
        }
    }

    /// Forget every field gesture and release debt of `window_id`, which is closing.
    ///
    /// Window events and child registry release both call this, so a window reaped
    /// without a `Destroyed` event cannot leave a capture that blocks other windows.
    pub(super) fn drop_field_pointer_for_window(&mut self, window_id: WindowId) {
        if self.field_capture_window() == Some(window_id) {
            // `window_id` holds the capture, so no paired release can arrive any more.
            self.field_pointer_capture = None;
        }
        self.field_owed_releases.retain(|owed| *owed != window_id);
    }

    /// End a field drag in `window_id`; its paired release is still consumed.
    pub(super) fn cancel_field_pointer(&mut self, window_id: WindowId) {
        if self.field_drag_source() == Some(window_id) {
            // The drag stops selecting, but its button is still down.
            self.field_pointer_capture = Some(FieldPointerCapture::Swallow { window_id });
        }
    }

    /// Window of a live drag, if any.
    fn field_drag_source(&self) -> Option<WindowId> {
        match self.field_pointer_capture {
            Some(FieldPointerCapture::Dragging { window_id, .. }) => Some(window_id),
            _ => None,
        }
    }

    /// Window that owns the current capture, dragging or owed a release.
    fn field_capture_window(&self) -> Option<WindowId> {
        match self.field_pointer_capture {
            Some(
                FieldPointerCapture::Dragging { window_id, .. }
                | FieldPointerCapture::Swallow { window_id },
            ) => Some(window_id),
            None => None,
        }
    }

    /// Editable query field under this window's pointer ownership, if any.
    ///
    /// The palette is modal over its host window's search; the colour picker's
    /// title row is not an editable field.
    pub(super) fn field_pointer_target(&self, window_id: WindowId) -> Option<FieldTarget> {
        let window = self.windows.get(&window_id).filter(|window| !window.hidden)?;
        if self.command_palette.is_open()
            && self.palette_attached_window.or(self.main_window_id) == Some(window_id)
        {
            // When: the palette is open on `window_id`, only its query may own a field gesture.
            let mode = self.command_palette.mode();
            let live = match mode {
                CommandPaletteMode::TabColor => false,
                // A rename edits one captured tab or window; once it closed or moved, the
                // editor still shows but its query belongs to nothing and must not be edited.
                CommandPaletteMode::RenameTab => self.tab_edit_target_window() == Some(window_id),
                CommandPaletteMode::RenameWindow => {
                    self.window_rename_target.and_then(|key| self.window_keys.resolve(key))
                        == Some(window_id)
                }
                CommandPaletteMode::Commands => true,
            };
            let session = self.command_palette.session();
            return live.then_some(FieldTarget::Palette { mode, session });
        }
        let active = window.tabs.active_index();
        window.tab_states.get(active)?.search.as_ref()?;
        Some(FieldTarget::Search(window.tabs.active()?.id))
    }

    /// Place the caret, or extend the selection, at a query-relative offset.
    fn apply_field_offset(
        &mut self,
        window_id: WindowId,
        target: FieldTarget,
        offset: usize,
        extend: bool,
    ) {
        match target {
            FieldTarget::Palette { .. } if extend => self.command_palette.extend_to(offset),
            FieldTarget::Palette { .. } => self.command_palette.set_cursor(offset),
            FieldTarget::Search(_) => {
                // When: `target` is the search field, the offset edits that window's active search.
                let Some(window) = self.windows.get_mut(&window_id) else {
                    // When: the source window closed, there is no search to edit.
                    return;
                };
                let active = window.tabs.active_index();
                if let Some(search) =
                    window.tab_states.get_mut(active).and_then(|state| state.search.as_mut())
                {
                    // Selection-only edits never re-run the match scan.
                    if extend {
                        search.extend_to(offset);
                    } else {
                        // When: `extend` is false, the press collapses the selection to a caret.
                        search.set_cursor(offset);
                    }
                }
            }
        }
        self.request_redraw_for_overlay(Some(window_id));
    }

    /// A left press: start a field gesture when it lands inside a presented field.
    fn field_pointer_press(&mut self, window_id: WindowId, resolve: FieldHitResolver) -> bool {
        if let Some(source) = self
            .field_capture_window()
            .filter(|source| self.windows.get(source).is_none_or(|window| window.hidden))
        {
            // The capture's window is gone or hidden and can never deliver its release, so its
            // gesture must not block this press.
            self.drop_field_pointer_for_window(source);
        }
        if self.field_drag_source().is_some_and(|source| source != window_id) {
            // When: `field_drag_source` is a window other than `window_id`, that window keeps its
            // motion and paired release; this press is left to its own window's handlers.
            return false;
        }
        // A press in this window settles any release it still owed.
        self.field_owed_releases.retain(|owed| *owed != window_id);
        match self.field_pointer_capture.take() {
            Some(FieldPointerCapture::Swallow { window_id: owed }) if owed != window_id => {
                // Another window's cancelled gesture never delivered its release (focus moved
                // away), so keep that debt per window and still consume its late release.
                self.field_owed_releases.push(owed);
            }
            _ => {
                // When: `field_pointer_capture` held no other window's debt, an earlier owed
                // release of `window_id` is moot once its button goes down again.
            }
        }
        // Debts of windows that closed without a close event can never be paid.
        let windows = &self.windows;
        self.field_owed_releases.retain(|owed| windows.contains_key(owed));
        let Some(target) = self.field_pointer_target(window_id) else {
            // When: `field_pointer_target` finds no editable field in `window_id`, the press is not ours.
            return false;
        };
        let Some(window) = self.windows.get(&window_id) else {
            // When: the window vanished between lookups, there is nothing to press.
            return false;
        };
        if window.mouse_down
            || window.pointer_gesture.is_some()
            || window.pressed_tab.is_some()
            || window.drag_session.is_some()
            || window.scrollbar_drag.is_some()
            || window.splitter_drag.is_some()
        {
            // When: `window` already holds a terminal or chrome gesture, it keeps its events.
            return false;
        }
        let press = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
        let extend = window.modifiers.shift_key();
        let capture = match resolve(self, window_id, target, press, FieldHitMode::Press) {
            FieldHit::Outside => {
                // When: the press missed the field, the modal or terminal handles it.
                return false;
            }
            FieldHit::Offset(offset) => {
                self.apply_field_offset(window_id, target, offset, extend);
                FieldPointerCapture::Dragging { window_id, target, press, extend, anchored: true }
            }
            FieldHit::Stale => {
                // The field is on screen but its geometry is not current, so hold the press.
                self.request_redraw_for_overlay(Some(window_id));
                FieldPointerCapture::Dragging { window_id, target, press, extend, anchored: false }
            }
            FieldHit::Unavailable => {
                // No exact mapping exists (live preedit), so consume the click without editing.
                FieldPointerCapture::Swallow { window_id }
            }
        };
        self.field_pointer_capture = Some(capture);
        self.palette_pointer_capture = None;
        true
    }

    /// Pointer motion: extend the held field selection, clamped to the query.
    fn field_pointer_move(&mut self, window_id: WindowId, resolve: FieldHitResolver) -> bool {
        if self.field_owed_releases.contains(&window_id) {
            // When: `window_id` still owes the release of a cancelled gesture, its button is
            // held, so its motion stays away from the terminal and mouse reporting.
            return true;
        }
        let (source, target, press, extend, anchored) = match self.field_pointer_capture {
            Some(FieldPointerCapture::Dragging {
                window_id: source,
                target,
                press,
                extend,
                anchored,
            }) => (source, target, press, extend, anchored),
            Some(FieldPointerCapture::Swallow { window_id: owed }) => {
                // When: `owed` holds a cancelled gesture's button down, its motion must not
                // reach the terminal or mouse reporting; other windows' motion is untouched.
                return owed == window_id;
            }
            None => {
                // When: `field_pointer_capture` holds no gesture, motion belongs to the other handlers.
                return false;
            }
        };
        if source != window_id {
            // When: motion comes from another window, the source keeps its gesture.
            return false;
        }
        if self.field_pointer_target(window_id) != Some(target) {
            // When: `target` closed, changed mode, or its tab lost focus, stop selecting
            // but keep the held button's motion and release away from the terminal.
            self.field_pointer_capture = Some(FieldPointerCapture::Swallow { window_id });
            return true;
        }
        if !anchored {
            // When: the press is not `anchored` to a presented offset yet, resolve it before following.
            match resolve(self, window_id, target, press, FieldHitMode::Drag) {
                FieldHit::Offset(offset) => {
                    // The press point maps now; anchor there before following the pointer.
                    self.apply_field_offset(window_id, target, offset, extend);
                    self.field_pointer_capture = Some(FieldPointerCapture::Dragging {
                        window_id,
                        target,
                        press,
                        extend,
                        anchored: true,
                    });
                }
                FieldHit::Unavailable => {
                    // When: `target` became unmappable (preedit), only the release is owed.
                    self.field_pointer_capture = Some(FieldPointerCapture::Swallow { window_id });
                    return true;
                }
                FieldHit::Stale | FieldHit::Outside => {
                    // When: `target` geometry is still not presented, wait for the next frame.
                    self.request_redraw_for_overlay(Some(window_id));
                    return true;
                }
            }
        }
        let Some(window) = self.windows.get(&window_id) else {
            // When: the window closed mid-drag, nothing more can be selected.
            return true;
        };
        let point = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
        if let FieldHit::Offset(offset) =
            resolve(self, window_id, target, point, FieldHitMode::Drag)
        {
            // A stale or unmappable frame leaves the selection as the last presented one.
            self.apply_field_offset(window_id, target, offset, true);
        }
        true
    }

    /// Left release: end the gesture and consume the release of every consumed press.
    fn field_pointer_release(&mut self, window_id: WindowId, resolve: FieldHitResolver) -> bool {
        if let Some(index) = self.field_owed_releases.iter().position(|owed| *owed == window_id) {
            // When: `window_id` owes a parked cancelled gesture's late release, consume it exactly once.
            self.field_owed_releases.swap_remove(index);
            return true;
        }
        match self.field_pointer_capture {
            Some(FieldPointerCapture::Swallow { window_id: owed }) if owed == window_id => {
                self.field_pointer_capture = None;
                true
            }
            Some(FieldPointerCapture::Dragging {
                window_id: source,
                target,
                press,
                extend,
                anchored,
            }) if source == window_id => {
                self.field_pointer_capture = None;
                if !anchored && self.field_pointer_target(window_id) == Some(target) {
                    // A click whose geometry arrived only now still places the caret at its press.
                    if let FieldHit::Offset(offset) =
                        resolve(self, window_id, target, press, FieldHitMode::Drag)
                    {
                        self.apply_field_offset(window_id, target, offset, extend);
                    }
                }
                self.request_redraw_for_overlay(Some(window_id));
                true
            }
            _ => false,
        }
    }

    /// Hit the renderer's presented geometry of `target` in `window_id`.
    pub(super) fn renderer_field_hit(
        &self,
        window_id: WindowId,
        target: FieldTarget,
        point: (f32, f32),
        mode: FieldHitMode,
    ) -> FieldHit {
        let Some(window) = self.windows.get(&window_id) else {
            // When: the window closed, there is no presented field to hit.
            return FieldHit::Outside;
        };
        let Some(renderer) = window.renderer.as_ref() else {
            // When: the window has no renderer yet, nothing was presented to press.
            return FieldHit::Outside;
        };
        let preedit = window.ime.preedit();
        match target {
            FieldTarget::Palette { .. } => {
                renderer.palette_field_hit(&self.command_palette, preedit, point, mode)
            }
            FieldTarget::Search(_) => {
                let active = window.tabs.active_index();
                match window.tab_states.get(active).and_then(|state| state.search.as_ref()) {
                    Some(search) => renderer.search_field_hit(search, preedit, point, mode),
                    None => FieldHit::Outside,
                }
            }
        }
    }
}
