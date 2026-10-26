//! The font family picker palette page: installed monospace families from the font catalog.
//! Moving the highlight applies a family live to every window; Escape restores the font the
//! picker opened with, and Enter keeps the highlighted one for the session.

use sonicterm_ui::command_palette::FontFamilyRow;

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
        self.palette_pointer_capture = None;
        let attached = match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        };
        // The palette names its main-window attachment `None`, as every other page does.
        self.palette_attached_window = attached.filter(|id| Some(*id) != self.main_window_id);
        let current = self.config.font.family.clone();
        let (mut families, title_key) = match &self.font_catalog {
            FontCatalog::Ready(families) => (families.clone(), "font-picker-title"),
            FontCatalog::Idle | FontCatalog::Loading => (Vec::new(), "font-picker-loading"),
        };
        if !families.iter().any(|family| family.eq_ignore_ascii_case(&current)) {
            // The live family is listed even when the catalog lacks it, so it can be kept.
            families.insert(0, current.clone());
        }
        let rows = families
            .into_iter()
            .map(|family| {
                let mark = if family.eq_ignore_ascii_case(&current) { "✓ " } else { "  " };
                FontFamilyRow { label: format!("{mark}{family}"), family }
            })
            .collect();
        let title = self.i18n.translate(title_key);
        let footer = self.i18n.translate("font-picker-footer");
        self.command_palette.start_font_family(title, footer, rows, &current);
        self.font_picker_original = Some(current);
        self.request_redraw_for_overlay(self.palette_attached_window);
    }

    /// Apply the highlighted family live to every window, if it is not the live one already.
    pub(in crate::app) fn preview_font_family(&mut self) {
        let Some(family) = self.command_palette.selected_font_family().map(str::to_owned) else {
            // When: `selected_font_family()` is None, as when the filter matches nothing, nothing applies.
            return;
        };
        if family != self.config.font.family {
            self.set_font_family(&family);
            self.request_redraw_for_overlay(self.palette_attached_window);
        }
    }

    /// Close the picker. `keep` applies the highlighted family; otherwise the font the picker
    /// opened with comes back.
    pub(in crate::app) fn close_font_family_picker(&mut self, keep: bool) {
        let source = self.palette_attached_window.or(self.main_window_id);
        let original = self.font_picker_original.take();
        if keep {
            self.preview_font_family();
        } else if let Some(original) = original.filter(|family| *family != self.config.font.family)
        {
            // When: `original` differs from the live family, Escape brings that font back.
            self.set_font_family(&original);
        }
        self.command_palette.close();
        self.palette_attached_window = None;
        self.request_redraw_for_overlay(source);
    }
}

#[cfg(test)]
#[path = "font_family_tests.rs"]
mod font_family_tests;
