//! Dispatch for the palette's live-preview pickers: keys and clicks on a picker page reach the
//! font family or theme picker by the kind the palette holds.

use sonicterm_ui::command_palette::PickerKind;
use winit::window::WindowId;

use crate::app::App;

impl App {
    /// Apply the open picker's highlighted row live, by the picker's kind.
    pub(in crate::app) fn preview_picker(&mut self) {
        match self.interaction.command_palette.picker_kind() {
            Some(PickerKind::FontFamily) => self.preview_font_family(),
            Some(PickerKind::Theme) => self.preview_theme(),
            None => {
                // When: picker_kind() is None, no picker is open and no highlighted row applies.
            }
        }
    }

    /// Restore the original value of a picker the palette left by a route other than the picker's
    /// own close: toggling the palette, opening another page over it, or closing its window.
    /// Runs after every event batch and before a picker opens, so no preview outlives its page.
    pub(in crate::app) fn settle_abandoned_pickers(&mut self) {
        let kind = self.interaction.command_palette.picker_kind();
        if kind != Some(PickerKind::Theme) {
            // With no theme page showing, theme picker state left behind is a cancelled preview.
            self.restore_abandoned_theme_picker();
        }
        if kind != Some(PickerKind::FontFamily) {
            // With no font page showing, a font picker original left behind is a cancelled preview.
            self.restore_abandoned_font_picker();
        }
    }

    /// Cancel an open picker shown on `window_id`, restoring its original value, because that
    /// window is closing or hiding and the page would otherwise outlive it.
    pub(in crate::app) fn cancel_picker_for_window(&mut self, window_id: WindowId) {
        let host = self.interaction.palette_attached_window.or(self.registry.main_window_id);
        if self.interaction.command_palette.picker_kind().is_some() && host == Some(window_id) {
            // The picker on the closing or hiding window is cancelled, so its original comes back.
            self.close_picker(false);
        }
    }

    /// Close the open picker by its kind. `keep` keeps the highlighted row; otherwise the value
    /// the picker opened with comes back. Does nothing when no picker is open.
    pub(in crate::app) fn close_picker(&mut self, keep: bool) {
        match self.interaction.command_palette.picker_kind() {
            Some(PickerKind::FontFamily) => self.close_font_family_picker(keep),
            Some(PickerKind::Theme) => self.close_theme_picker(keep),
            None => {
                // When: picker_kind() is None, no picker is open, so nothing is kept or restored.
            }
        }
    }
}
