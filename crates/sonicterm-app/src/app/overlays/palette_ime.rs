use sonicterm_gpu::core::GpuRenderer;
use sonicterm_gpu::field_geometry::FieldRect;
use sonicterm_ui::command_palette::CommandPalette;
use sonicterm_ui::search::SearchState;
use winit::window::WindowId;

use crate::app::App;

#[cfg(test)]
#[path = "palette_ime_tests.rs"]
mod palette_ime_tests;

/// Where one window's OS IME candidate box goes after a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) enum FieldImeAnchor {
    /// A field owns IME and the renderer presented its caret at this rectangle.
    Field(FieldRect),
    /// A field owns IME but its caret is not presented yet; the terminal anchor must not be used.
    Pending,
    /// No field owns IME, so the terminal cursor anchors it.
    Terminal,
}

/// Map field ownership to an anchor: `None` means no field owns IME, and
/// `Some(None)` means a field owns it without a presented caret.
pub(in crate::app) fn field_ime_anchor_for(owned: Option<Option<FieldRect>>) -> FieldImeAnchor {
    match owned {
        Some(Some(caret)) => FieldImeAnchor::Field(caret),
        Some(None) => FieldImeAnchor::Pending,
        None => FieldImeAnchor::Terminal,
    }
}

/// IME anchor of a window whose renderer just presented: the palette when it is
/// attached to this window, otherwise the active tab's open search.
///
/// Main redraw, child redraw and the test inspector all call this, so the
/// candidate box always follows the caret geometry the renderer presented.
pub(in crate::app) fn field_ime_anchor(
    renderer: &GpuRenderer,
    palette: Option<&CommandPalette>,
    search: Option<&SearchState>,
    preedit: &str,
) -> FieldImeAnchor {
    let owned = match (palette.filter(|palette| palette.is_open()), search) {
        (Some(palette), _) => Some(renderer.palette_field_caret_rect(palette, preedit)),
        (None, Some(search)) => Some(renderer.search_field_caret_rect(search, preedit)),
        (None, None) => None,
    };
    field_ime_anchor_for(owned)
}

/// OS IME candidate area for a field caret the renderer presented, in physical pixels.
pub(in crate::app) fn field_ime_area(
    caret: FieldRect,
) -> (winit::dpi::PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>) {
    (
        winit::dpi::PhysicalPosition::new(caret.x.round() as i32, caret.y.round() as i32),
        winit::dpi::PhysicalSize::new(
            caret.w.ceil().max(1.0) as u32,
            caret.h.ceil().max(1.0) as u32,
        ),
    )
}

impl App {
    fn palette_ime_preedit(&self) -> &str {
        match self.palette_attached_window {
            Some(id) => self.windows.get(&id).map(|window| window.ime.preedit()).unwrap_or(""),
            None => self.main().map(|window| window.ime.preedit()).unwrap_or(""),
        }
    }

    fn update_palette_ime_state(&mut self, ime_event: &winit::event::Ime) {
        let target = self.palette_attached_window;
        let Some(window) = (match target {
            Some(id) => self.windows.get_mut(&id),
            None => self.main_mut(),
        }) else {
            // When: target names a window already removed, or target is None and
            // there is no main window, drop the IME update — nothing records it.
            return;
        };
        match ime_event {
            winit::event::Ime::Enabled => window.ime.handle_enabled(),
            winit::event::Ime::Disabled => window.ime.handle_disabled(),
            winit::event::Ime::Preedit(text, cursor) => window.ime.handle_preedit(text, *cursor),
            winit::event::Ime::Commit(text) => {
                // When: a Commit arrives the palette consumes text itself, so
                // take_commits drains the buffer and no bytes reach the PTY later.
                window.ime.handle_commit(text);
                let _ = window.ime.take_commits();
            }
        }
    }

    pub(in crate::app) fn palette_ime_is_composing(&self) -> bool {
        match self.palette_attached_window {
            Some(id) => {
                self.windows.get(&id).map(|window| window.ime.is_composing()).unwrap_or(false)
            }
            None => self.main().map(|window| window.ime.is_composing()).unwrap_or(false),
        }
    }

    /// The palette caret the attached window last presented, as an OS IME area.
    ///
    /// `None` until that renderer presents exactly this query, caret, selection,
    /// and preedit; the post-render IME update then supplies the area.
    pub(in crate::app) fn command_palette_ime_cursor_area(
        &self,
    ) -> Option<(winit::dpi::PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>)> {
        if !self.command_palette.is_open() {
            // When: command_palette is closed there is no query row to anchor
            // the IME candidate box to; None leaves the cursor area unchanged.
            return None;
        }
        let renderer = match self.palette_attached_window {
            Some(id) => self.windows.get(&id)?.renderer.as_ref()?,
            None => self.main_renderer()?,
        };
        renderer
            .palette_field_caret_rect(&self.command_palette, self.palette_ime_preedit())
            .map(field_ime_area)
    }

    /// Move the attached window's OS IME candidate box to the presented palette caret.
    pub(in crate::app) fn update_command_palette_ime_cursor_area(&self) {
        let window = match self.palette_attached_window {
            Some(id) => self.windows.get(&id).and_then(|child| child.window.clone()),
            None => self.main_window().cloned(),
        };
        let Some(window) = window else {
            // When: the attached window is gone or not created yet, there is no surface to anchor.
            return;
        };
        if let Some((pos, size)) = self.command_palette_ime_cursor_area() {
            window.set_ime_cursor_area(pos, size);
        }
    }

    pub(in crate::app) fn command_palette_handle_ime_in_window(
        &mut self,
        window_id: WindowId,
        ime_event: &winit::event::Ime,
    ) -> bool {
        self.command_palette_owns_input(window_id) && self.command_palette_handle_ime(ime_event)
    }

    pub(in crate::app) fn command_palette_handle_ime(
        &mut self,
        ime_event: &winit::event::Ime,
    ) -> bool {
        if !self.command_palette.is_open() {
            // When: command_palette is closed the IME event belongs to the
            // terminal; returning false lets window_event run its commit path.
            return false;
        }
        self.palette_pointer_capture = None;
        self.refresh_command_palette_context();
        self.update_palette_ime_state(ime_event);
        match ime_event {
            winit::event::Ime::Commit(text) => {
                // One replacement per commit: RenameWindow validates atomically, other modes strip controls.
                self.command_palette.input_str(text);
                self.update_command_palette_ime_cursor_area();
                self.request_redraw_for_overlay(self.palette_attached_window);
            }
            winit::event::Ime::Preedit(_, _)
            | winit::event::Ime::Enabled
            | winit::event::Ime::Disabled => {
                self.update_command_palette_ime_cursor_area();
                self.request_redraw_for_overlay(self.palette_attached_window);
            }
        }
        true
    }
}
