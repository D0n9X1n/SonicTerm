use sonicterm_ui::overlays::{
    command_palette_query_caret_prefix, PaletteLayout, PALETTE_ROW_PAD_X,
};
use winit::window::WindowId;

use crate::app::App;

fn estimate_palette_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().map(|character| if character.is_ascii() { 0.58 } else { 1.0 }).sum::<f32>()
        * font_size
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

    pub(in crate::app) fn command_palette_ime_cursor_area(
        &self,
        window_w: f32,
        window_h: f32,
        panel_padding: f32,
        scale: f32,
        font_size: f32,
        cell_w: f32,
    ) -> Option<(winit::dpi::PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>)> {
        if !self.command_palette.is_open() {
            // When: command_palette is closed there is no query row to anchor
            // the IME candidate box to; None leaves the cursor area unchanged.
            return None;
        }
        let mut palette = self.command_palette.clone();
        let layout =
            PaletteLayout::compute(&mut palette, window_w, window_h, panel_padding, scale)?;
        let preedit = self.palette_ime_preedit();
        let prefix = command_palette_query_caret_prefix(&palette, preedit);
        let text_x = layout.query_row.x + PALETTE_ROW_PAD_X * scale;
        let caret_x = text_x + estimate_palette_text_width(&prefix, font_size);
        Some((
            winit::dpi::PhysicalPosition::new(caret_x as i32, layout.query_row.y as i32),
            winit::dpi::PhysicalSize::new(cell_w.ceil() as u32, layout.query_row.h.ceil() as u32),
        ))
    }

    /// Move the attached window's OS IME candidate box to the palette caret.
    pub(in crate::app) fn update_command_palette_ime_cursor_area(&self) {
        if !self.command_palette.is_open() {
            // When: command_palette is closed there is no palette caret to
            // follow; the IME cursor area stays where the terminal set it.
            return;
        }
        let target = self.palette_attached_window;
        let (window, width, height, scale, font_size, cell_w) = if let Some(id) = target {
            // When: target names a child window the palette is attached to it,
            // so measure that child's surface for the IME box.
            let Some(child) = self.windows.get(&id) else {
                // When: id is no longer in windows the child closed since the
                // palette attached; abandon the reposition instead of a dead window.
                return;
            };
            let (Some(window), Some(renderer)) = (child.window.as_ref(), child.renderer.as_ref())
            else {
                // When: the child has no window or renderer yet there is no
                // surface to measure scale and cell width from; skip until ready.
                return;
            };
            let size = window.inner_size();
            (
                window.clone(),
                size.width as f32,
                size.height as f32,
                renderer.scale_factor(),
                renderer.font_size() * renderer.scale_factor(),
                renderer.cell_w,
            )
        } else {
            // When: target is None the palette is attached to no child window,
            // so measure the main window's surface instead.
            let (Some(window), Some(renderer)) = (self.main_window(), self.main_renderer()) else {
                // When: main_window or main_renderer is absent before the first
                // frame there is no surface to place the IME box on; skip.
                return;
            };
            let size = window.inner_size();
            (
                window.clone(),
                size.width as f32,
                size.height as f32,
                renderer.scale_factor(),
                renderer.font_size() * renderer.scale_factor(),
                renderer.cell_w,
            )
        };
        if let Some((pos, size)) = self.command_palette_ime_cursor_area(
            width,
            height,
            self.config.appearance.panel_padding,
            scale,
            font_size,
            cell_w,
        ) {
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
