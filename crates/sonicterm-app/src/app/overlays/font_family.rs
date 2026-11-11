//! The font family picker palette page: installed monospace families from the font catalog.
//! Moving the highlight applies a family live to every window; Escape restores the font the
//! picker opened with, and Enter keeps the highlighted one for the session.

use sonicterm_ui::command_palette::{PickerKind, PickerRow};

use crate::app::font_catalog::FontCatalog;
use crate::app::{App, FrontmostKind};

/// Every message key the picker reads, for the locale coverage test.
#[cfg(test)]
pub(super) const LABEL_KEYS: [&str; 4] = [
    "command-choose-font-family",
    "font-picker-title",
    "font-picker-footer",
    "font-picker-loading",
];

impl App {
    /// Open the picker on the frontmost window, listing the catalog with the live family marked
    /// and highlighted. Before the catalog is ready, only the live family is listed.
    pub(in crate::app) fn open_font_family_picker(&mut self) {
        // An open picker of either kind is cancelled first, so its original value comes back,
        // and one another page replaced is settled, so the original below is a confirmed font.
        self.close_picker(false);
        self.settle_abandoned_pickers();
        self.interaction.palette_pointer_capture = None;
        // Installed families are enumerated the first time the picker opens, never at startup.
        self.start_font_catalog();
        let attached = match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        };
        // The palette names its main-window attachment `None`, as every other page does.
        self.interaction.palette_attached_window =
            attached.filter(|id| Some(*id) != self.registry.main_window_id);
        let current = self.config.font.family.clone();
        let (title, rows) = self.font_family_page(&current);
        let footer = self.i18n.translate("font-picker-footer");
        self.interaction.command_palette.start_picker(
            PickerKind::FontFamily,
            title,
            footer,
            rows,
            &current,
        );
        self.fonts.font_picker_original = Some(current);
        self.request_redraw_for_overlay(self.interaction.palette_attached_window);
    }

    /// The picker's title and rows from the catalog, with `current` marked. Before the catalog is
    /// ready only `current` is listed, under the loading title.
    fn font_family_page(&self, current: &str) -> (String, Vec<PickerRow>) {
        let (mut families, title_key) = match &self.fonts.font_catalog {
            FontCatalog::Ready(families) => (families.clone(), "font-picker-title"),
            FontCatalog::Idle | FontCatalog::Loading => (Vec::new(), "font-picker-loading"),
        };
        if !families.iter().any(|family| family.eq_ignore_ascii_case(current)) {
            // The live family is listed even when the catalog lacks it, so it can be kept.
            families.insert(0, current.to_owned());
        }
        let rows = families
            .into_iter()
            .map(|family| {
                let mark = if family.eq_ignore_ascii_case(current) { "✓ " } else { "  " };
                PickerRow { label: format!("{mark}{family}"), value: family }
            })
            .collect();
        (self.i18n.translate(title_key), rows)
    }

    /// Rebuild an open picker's title and rows from the catalog in place, keeping the typed filter
    /// and the highlighted family; the family it opened with stays marked.
    pub(in crate::app) fn refresh_font_family_picker(&mut self) {
        let Some(current) = self.fonts.font_picker_original.clone() else {
            // When: `font_picker_original` is None, no picker is open, so nothing is refreshed.
            return;
        };
        let (title, rows) = self.font_family_page(&current);
        self.interaction.command_palette.update_picker(PickerKind::FontFamily, title, rows);
        self.request_redraw_for_overlay(self.interaction.palette_attached_window);
    }

    /// Apply the highlighted family live to every window, if it is not the live one already.
    pub(in crate::app) fn preview_font_family(&mut self) {
        let Some(family) =
            self.interaction.command_palette.selected_picker_value().map(str::to_owned)
        else {
            // When: `selected_picker_value()` is None, as when the filter matches nothing, nothing applies.
            return;
        };
        if family != self.config.font.family {
            self.set_font_family(&family);
            self.request_redraw_for_overlay(self.interaction.palette_attached_window);
        }
    }

    /// Bring back the family a picker opened with when its page is gone without its own close.
    pub(in crate::app) fn restore_abandoned_font_picker(&mut self) {
        let Some(original) = self.fonts.font_picker_original.take() else {
            // When: font_picker_original is None, no font picker was left behind.
            return;
        };
        if original != self.config.font.family {
            // A family that differs from the live one is the abandoned preview's original; it comes back.
            self.set_font_family(&original);
        }
    }

    /// Close the picker. `keep` applies the highlighted family; otherwise the font the picker
    /// opened with comes back.
    pub(in crate::app) fn close_font_family_picker(&mut self, keep: bool) {
        let source = self.interaction.palette_attached_window.or(self.registry.main_window_id);
        let original = self.fonts.font_picker_original.take();
        if keep {
            self.preview_font_family();
        } else if let Some(original) = original.filter(|family| *family != self.config.font.family)
        {
            // When: `original` differs from the live family, Escape brings that font back.
            self.set_font_family(&original);
        }
        self.interaction.command_palette.close();
        self.interaction.palette_attached_window = None;
        self.request_redraw_for_overlay(source);
    }
}

#[cfg(test)]
#[path = "font_family_tests.rs"]
mod font_family_tests;
