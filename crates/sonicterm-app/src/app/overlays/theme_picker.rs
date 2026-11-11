//! The theme picker palette page: every bundled theme and every theme in the user's `themes`
//! directory, light ones first. Moving the highlight applies a theme live to every window;
//! Escape restores the theme the picker opened with, and Enter keeps the highlighted one for the
//! session.

use std::path::PathBuf;

use sonicterm_cfg::theme::{Appearance, Theme};
use sonicterm_ui::command_palette::{PickerKind, PickerRow};

use crate::app::{App, FrontmostKind};

/// Every message key the picker reads, for the locale coverage test.
#[cfg(test)]
pub(super) const LABEL_KEYS: [&str; 5] = [
    "command-choose-theme",
    "theme-picker-title",
    "theme-picker-footer",
    "theme-picker-light",
    "theme-picker-dark",
];

/// The open theme picker's original theme and the themes it lists, loaded once when it opens.
pub(in crate::app) struct ThemePickerState {
    /// The `config.theme` name live when the picker opened.
    original_name: String,
    /// The live theme when the picker opened, accessibility already applied; restored as is, so
    /// a theme file edited or deleted while the picker is open cannot strand the preview.
    original: Theme,
    /// Each listed theme by name, as loaded from its file without accessibility overrides.
    candidates: Vec<(String, Theme)>,
}

/// Load every `*.toml` theme in `dirs`, keyed by file stem. An earlier directory wins a name
/// clash, as the user directory does over the bundled one; a file that fails to load is logged
/// and left out.
pub(super) fn load_theme_candidates(dirs: &[PathBuf]) -> Vec<(String, Theme)> {
    let mut candidates: Vec<(String, Theme)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            // When: `dir` cannot be read, as when the user has no themes directory, it lists none.
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|extension| extension == "toml"))
            .collect();
        paths.sort();
        for path in paths {
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()).map(str::to_owned)
            else {
                // When: the stem is not UTF-8, no theme name can select the file.
                continue;
            };
            if candidates.iter().any(|(listed, _)| *listed == name) {
                // When: an earlier directory already lists `name`, that file is the one it loads.
                continue;
            }
            match Theme::load_strict(&path) {
                Ok(theme) => candidates.push((name, theme)),
                Err(error) => {
                    // A file that does not parse is left out of the list, and the log names it.
                    tracing::warn!(path = %path.display(), "theme picker skips a theme: {error:#}");
                }
            }
        }
    }
    candidates
}

impl App {
    /// The directories the picker lists, in lookup order: the user `themes` directory beside the
    /// config file, then the bundled one.
    fn theme_picker_dirs(&self) -> Vec<PathBuf> {
        let user_dir = match self.configs.runtime_config_path.as_deref() {
            Some(config_path) => config_path.parent().map(|dir| dir.join("themes")),
            None => sonicterm_cfg::config::default_config_dir().map(|dir| dir.join("themes")),
        };
        user_dir
            .into_iter()
            .chain(std::iter::once(sonicterm_cfg::assets::asset_dir().join("themes")))
            .collect()
    }

    /// Open the picker on the frontmost window, listing the themes with the live one marked and
    /// highlighted. The live theme is listed even when no file provides it, so it can be kept.
    pub(in crate::app) fn open_theme_picker(&mut self) {
        // An open picker of either kind is cancelled first, so its original value comes back,
        // and one another page replaced is settled, so the snapshot below is a confirmed theme.
        self.close_picker(false);
        self.settle_abandoned_pickers();
        self.interaction.palette_pointer_capture = None;
        let attached = match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        };
        // The palette names its main-window attachment `None`, as every other page does.
        self.interaction.palette_attached_window =
            attached.filter(|id| Some(*id) != self.registry.main_window_id);
        let current = self.config.theme.clone();
        let candidates = load_theme_candidates(&self.theme_picker_dirs());
        let rows = self.theme_rows(&current, &candidates);
        let title = self.i18n.translate("theme-picker-title");
        let footer = self.i18n.translate("theme-picker-footer");
        self.interaction.command_palette.start_picker(
            PickerKind::Theme,
            title,
            footer,
            rows,
            &current,
        );
        self.configs.theme_picker = Some(ThemePickerState {
            original_name: current,
            original: self.theme.clone(),
            candidates,
        });
        self.request_redraw_for_overlay(self.interaction.palette_attached_window);
    }

    /// The picker rows: light themes first, then dark, each by name, with `current` marked and
    /// every row tagged light or dark so typing either word filters by it.
    fn theme_rows(&self, current: &str, candidates: &[(String, Theme)]) -> Vec<PickerRow> {
        let mut listed: Vec<(String, Appearance)> =
            candidates.iter().map(|(name, theme)| (name.clone(), theme.appearance)).collect();
        if !listed.iter().any(|(name, _)| name == current) {
            // No listed file provides the live theme, so it is listed from the live colors.
            listed.push((current.to_owned(), self.theme.appearance));
        }
        listed.sort_by(|(left_name, left), (right_name, right)| {
            (*left != Appearance::Light, left_name).cmp(&(*right != Appearance::Light, right_name))
        });
        let light = self.i18n.translate("theme-picker-light");
        let dark = self.i18n.translate("theme-picker-dark");
        listed
            .into_iter()
            .map(|(name, appearance)| {
                let mark = if name == current { "✓ " } else { "  " };
                let tone = if appearance == Appearance::Light { &light } else { &dark };
                PickerRow { label: format!("{mark}{name} · {tone}"), value: name }
            })
            .collect()
    }

    /// Apply the highlighted theme live to every window, if it is not the live one already. The
    /// theme the picker opened with comes back from its snapshot, never from its file.
    pub(in crate::app) fn preview_theme(&mut self) {
        let Some(name) =
            self.interaction.command_palette.selected_picker_value().map(str::to_owned)
        else {
            // When: `selected_picker_value()` is None, as when the filter matches nothing, nothing applies.
            return;
        };
        if name == self.config.theme {
            // When: the highlighted theme is already live, re-installing it would only redraw.
            return;
        }
        let Some(state) = self.configs.theme_picker.as_ref() else {
            // When: no theme picker state exists, the picker is not open and nothing applies.
            return;
        };
        let theme = if name == state.original_name {
            state.original.clone()
        } else {
            // When: `name` is another listed theme, its loaded file gets the session's accessibility.
            let Some((_, loaded)) = state.candidates.iter().find(|(listed, _)| *listed == name)
            else {
                // When: `name` is not a loaded candidate, there is no theme to apply.
                return;
            };
            let mut theme = loaded.clone();
            theme.apply_accessibility(&self.config.accessibility);
            theme
        };
        self.install_theme(theme, &name);
        tracing::info!("theme picker preview -> {name}");
        self.request_redraw_for_overlay(self.interaction.palette_attached_window);
    }

    /// Close the picker. `keep` applies the highlighted theme; otherwise the theme the picker
    /// opened with comes back.
    pub(in crate::app) fn close_theme_picker(&mut self, keep: bool) {
        let source = self.interaction.palette_attached_window.or(self.registry.main_window_id);
        if keep {
            self.preview_theme();
        }
        let state = self.configs.theme_picker.take();
        if let Some(state) = state.filter(|state| !keep && state.original_name != self.config.theme)
        {
            // A cancel after a preview brings back the snapshot the picker opened with.
            self.install_theme(state.original, &state.original_name);
        }
        self.interaction.command_palette.close();
        self.interaction.palette_attached_window = None;
        self.request_redraw_for_overlay(source);
    }

    /// Bring back the theme a picker opened with when its page is gone without its own close, and
    /// drop the picker state, so a later picker never captures an unconfirmed preview.
    pub(in crate::app) fn restore_abandoned_theme_picker(&mut self) {
        let Some(state) = self.configs.theme_picker.take() else {
            // When: theme_picker is None, no theme picker was left behind.
            return;
        };
        if state.original_name != self.config.theme {
            // A live theme other than the original is the abandoned preview; the snapshot comes back.
            self.install_theme(state.original, &state.original_name);
        }
    }

    /// End an open theme preview without restoring anything: a config reload calls this before
    /// it applies the configured theme, which then stands.
    pub(in crate::app) fn end_theme_preview(&mut self) {
        if self.configs.theme_picker.take().is_none() {
            // When: theme_picker.take() is None, no theme picker is open and no preview ends.
            return;
        }
        if self.interaction.command_palette.picker_kind() == Some(PickerKind::Theme) {
            // The theme page still showing closes with its preview state gone.
            let source = self.interaction.palette_attached_window.or(self.registry.main_window_id);
            self.interaction.command_palette.close();
            self.interaction.palette_attached_window = None;
            self.request_redraw_for_overlay(source);
        }
    }
}

#[cfg(test)]
#[path = "theme_picker_tests.rs"]
mod theme_picker_tests;
