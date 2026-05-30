//! Extracted from `app/mod.rs` in refactor PR 8b (expose-then-extract).
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use sonic_core::{
    config::Config,
    grid::Grid,
    keymap::{Action, Direction, Keymap, ScrollAction},
    pty::PtyHandle,
    theme::Theme,
    vt::{Parser, VtEvent},
};
use sonic_shared::render::GpuRenderer;
use sonic_ui::pane::PaneTree;
use sonic_ui::selection::Selection;
use sonic_ui::tabbar_view::{TabBarLayout, TabHit};
use sonic_ui::tabs::{Tab, TabBar};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_panes_to_rects,
    shell_quote_posix, to_logical_pos, with_integrated_titlebar, wrap_paste, App, PaneState,
    TabState, UserEvent, WindowState,
};
use crate::app::integrated_titlebar_inset;

fn propagate_theme_to_pane_parsers(panes: &HashMap<u64, PaneState>, theme: &Theme) {
    let fg = theme.colors.foreground.rgb();
    let bg = theme.colors.background.rgb();
    let cursor = theme.colors.cursor.rgb();

    for pane in panes.values() {
        // Config live-reload runs on the app thread, not the render hot path,
        // so lock() is intentional here. Dropping this update would leave OSC
        // 10/11/12 replies stale for shells already attached to the pane.
        let mut parser = pane.parser.lock();
        if let Some((r, g, b)) = fg {
            parser.set_theme_fg(r, g, b);
        }
        if let Some((r, g, b)) = bg {
            parser.set_theme_bg(r, g, b);
        }
        if let Some((r, g, b)) = cursor {
            parser.set_theme_cursor(r, g, b);
        }
    }
}

/// True iff any field in `new_cfg.font` differs from `old_cfg.font`
/// (family, size, or line_height) in a way that should drive a live
/// renderer re-apply.
///
/// Extracted as a free function so the file-watcher path can be
/// unit-tested without a live `GpuRenderer`.
pub fn config_diff_needs_font_apply(old_cfg: &Config, new_cfg: &Config) -> bool {
    new_cfg.font.family != old_cfg.font.family
        || (new_cfg.font.size - old_cfg.font.size).abs() > f32::EPSILON
        || (new_cfg.font.line_height - old_cfg.font.line_height).abs() > f32::EPSILON
}

impl App {
    pub(super) fn apply_new_config(&mut self, new_cfg: Config) {
        // PR #132: any live-reload (theme/font/keymap) is user-driven
        // and must render immediately, not at the next vsync deadline.
        self.input_dirty = true;
        let assets = sonic_shared::asset_dir();

        // Theme
        if new_cfg.theme != self.config.theme {
            let theme_path = assets.join("themes").join(format!("{}.toml", new_cfg.theme));
            match Theme::load(&theme_path) {
                Ok(mut t) => {
                    t.apply_accessibility(&new_cfg.accessibility);
                    tracing::info!("live-reload: theme -> {}", t.name);
                    if let Some(r) = self.main_renderer_mut() {
                        r.set_theme(&t);
                    }
                    for child in self.windows.values_mut() {
                        if let Some(r) = child.renderer.as_mut() {
                            r.set_theme(&t);
                        }
                    }
                    self.theme = t;
                    propagate_theme_to_pane_parsers(&self.panes, &self.theme);
                    for child in self.windows.values() {
                        propagate_theme_to_pane_parsers(&child.panes, &self.theme);
                    }
                    // Theme swap changes presentation (colors) without
                    // mutating cell contents — mark every pane dirty so
                    // the renderer re-shapes with the new palette.
                    mark_all_panes_dirty(&self.panes);
                    for child in self.windows.values() {
                        // Phase B2 PR-A: skip shadow main entry (renderer=None).
                        if child.renderer.is_none() {
                            continue;
                        }
                        mark_all_panes_dirty(&child.panes);
                    }
                }
                Err(e) => tracing::warn!("live-reload: theme {:?} failed: {e:#}", theme_path),
            }
        }

        if new_cfg.theme == self.config.theme
            && new_cfg.accessibility.high_contrast != self.config.accessibility.high_contrast
        {
            let theme_path = assets.join("themes").join(format!("{}.toml", new_cfg.theme));
            let mut t = match Theme::load(&theme_path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("live-reload: theme {:?} failed: {e:#}", theme_path);
                    self.theme.clone()
                }
            };
            t.apply_accessibility(&new_cfg.accessibility);
            if let Some(r) = self.main_renderer_mut() {
                r.set_theme(&t);
            }
            for child in self.windows.values_mut() {
                if let Some(r) = child.renderer.as_mut() {
                    r.set_theme(&t);
                }
            }
            self.theme = t;
            propagate_theme_to_pane_parsers(&self.panes, &self.theme);
            for child in self.windows.values() {
                propagate_theme_to_pane_parsers(&child.panes, &self.theme);
            }
            mark_all_panes_dirty(&self.panes);
            for child in self.windows.values() {
                // Phase B2 PR-A: skip shadow main entry (renderer=None).
                if child.renderer.is_none() {
                    continue;
                }
                mark_all_panes_dirty(&child.panes);
            }
            tracing::info!(
                "live-reload: accessibility.high_contrast -> {}",
                new_cfg.accessibility.high_contrast
            );
        }

        // Font
        let font_changed = config_diff_needs_font_apply(&self.config, &new_cfg);
        if font_changed {
            if let Some(r) = self.main_renderer_mut() {
                r.set_font(&new_cfg.font.family, new_cfg.font.size, new_cfg.font.line_height);
            }
            // Cell metrics changed → resize each pane to its own PaneRect.
            // See docs/specs/per-pane-grids.md for why this is per-pane,
            // not whole-window.
            let rects = self.compute_active_pane_rects();
            if let Some(r) = self.main_renderer() {
                let (cw, ch) = r.cell_size();
                resize_panes_to_rects(&self.panes, &rects, cw, ch);
            }
            // Apply the same swap to every torn-out child window. Each
            // child owns its own GpuRenderer, so it needs the font
            // change AND the matching pane resize against its own cell
            // metrics (its window can be a different size from main).
            for child in self.windows.values_mut() {
                let Some(r) = child.renderer.as_mut() else { continue };
                r.set_font(&new_cfg.font.family, new_cfg.font.size, new_cfg.font.line_height);
                let (cw, ch) = r.cell_size();
                let rects = App::compute_pane_rects_for(child);
                resize_panes_to_rects(&child.panes, &rects, cw, ch);
            }
            tracing::info!(
                "live-reload: font -> {} @ {}px x{}",
                new_cfg.font.family,
                new_cfg.font.size,
                new_cfg.font.line_height,
            );
        }

        // Language / i18n. Rebuild the app-level bundle so translated
        // strings are re-derived on the next frame instead of requiring a
        // restart.
        if new_cfg.locale != self.config.locale {
            let requested =
                if new_cfg.locale.is_empty() { None } else { Some(new_cfg.locale.as_str()) };
            self.i18n.reload_locale(requested);
            tracing::info!(locale = %self.i18n.locale(), "live-reload: locale");
        }

        // Cursor visuals — cheap to apply; the setters short-circuit
        // when nothing changed, so an unrelated config edit (e.g. a
        // theme swap) doesn't reset the blink phase.
        if let Some(r) = self.main_renderer_mut() {
            r.set_cursor_shape(new_cfg.terminal.cursor_shape);
            r.set_cursor_blink(new_cfg.terminal.cursor_blink);
        }
        for child in self.windows.values_mut() {
            if let Some(r) = child.renderer.as_mut() {
                r.set_cursor_shape(new_cfg.terminal.cursor_shape);
                r.set_cursor_blink(new_cfg.terminal.cursor_blink);
            }
        }

        // Padding (per-side). A change to any of the four window-padding
        // values shrinks/grows the inner cell area, so after pushing the
        // new padding into each live renderer we must resize every pane's
        // grid + PTY to match the renderer's new (cols, rows). Without the
        // resize the shell keeps reporting stale `stty size` and the grid
        // draws clipped against the old inner rect until a manual window
        // resize. Mirrors the font-live-reload path above (PR #53).
        let padding_changed = (new_cfg.window.padding_left - self.config.window.padding_left).abs()
            > f32::EPSILON
            || (new_cfg.window.padding_right - self.config.window.padding_right).abs()
                > f32::EPSILON
            || (new_cfg.window.padding_top - self.config.window.padding_top).abs() > f32::EPSILON
            || (new_cfg.window.padding_bottom - self.config.window.padding_bottom).abs()
                > f32::EPSILON;
        if padding_changed {
            let pad = [
                new_cfg.window.padding_left,
                new_cfg.window.padding_right,
                new_cfg.window.padding_top,
                new_cfg.window.padding_bottom,
            ];
            if let Some(r) = self.main_renderer_mut() {
                r.set_padding(pad);
            }
            let rects = self.compute_active_pane_rects();
            if let Some(r) = self.main_renderer() {
                let (cw, ch) = r.cell_size();
                resize_panes_to_rects(&self.panes, &rects, cw, ch);
            }
            for child in self.windows.values_mut() {
                let Some(r) = child.renderer.as_mut() else { continue };
                r.set_padding(pad);
                let (cw, ch) = r.cell_size();
                let rects = App::compute_pane_rects_for(child);
                resize_panes_to_rects(&child.panes, &rects, cw, ch);
            }
            tracing::info!(
                "live-reload: padding -> l={} r={} t={} b={}",
                pad[0],
                pad[1],
                pad[2],
                pad[3],
            );
        }

        if (new_cfg.appearance.opacity - self.config.appearance.opacity).abs() > f32::EPSILON {
            // B1b borrow-split: clone theme before borrowing renderer
            // (theme + renderer used to be disjoint App fields).
            let theme_snapshot = self.theme.clone();
            if let Some(r) = self.main_renderer_mut() {
                r.set_theme_with_opacity(&theme_snapshot, new_cfg.appearance.opacity);
            }
            for child in self.windows.values_mut() {
                if let Some(r) = child.renderer.as_mut() {
                    r.set_theme_with_opacity(&theme_snapshot, new_cfg.appearance.opacity);
                }
            }
            tracing::info!(opacity = new_cfg.appearance.opacity, "live-reload: appearance opacity");
        }

        // Tab close-button override (wezterm `tab_close_button_color`).
        // Diff against the live config so an edit that adds, changes,
        // or clears the value propagates to the main + every child
        // renderer without a restart.
        if new_cfg.tab_close_button_color != self.config.tab_close_button_color {
            if let Some(r) = self.main_renderer_mut() {
                r.set_tab_close_override(new_cfg.tab_close_button_color.as_deref());
            }
            for child in self.windows.values_mut() {
                if let Some(r) = child.renderer.as_mut() {
                    r.set_tab_close_override(new_cfg.tab_close_button_color.as_deref());
                }
            }
            tracing::info!(
                "live-reload: tab_close_button_color -> {:?}",
                new_cfg.tab_close_button_color
            );
        }

        if new_cfg.accessibility.reduced_motion != self.config.accessibility.reduced_motion
            || new_cfg.accessibility.strong_focus != self.config.accessibility.strong_focus
        {
            tracing::info!(
                "live-reload: accessibility reduced_motion={} strong_focus={}",
                new_cfg.accessibility.reduced_motion,
                new_cfg.accessibility.strong_focus
            );
        }

        // Keymap
        if new_cfg.keymap != self.config.keymap {
            let km_path = assets.join("keymaps").join(format!("{}.toml", new_cfg.keymap));
            match self
                .keymap_loader
                .as_ref()
                .map_or_else(|| Keymap::load(&km_path), |loader| loader(&new_cfg.keymap))
            {
                Ok(km) => {
                    tracing::info!(
                        "live-reload: keymap -> {} ({} bindings)",
                        km.meta.name,
                        km.bindings.len()
                    );
                    self.keymap = km;
                }
                Err(e) => tracing::warn!("live-reload: keymap {:?} failed: {e:#}", km_path),
            }
        }

        self.config = new_cfg;
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
        for child in self.windows.values() {
            // Phase B2 PR-A: skip shadow main entry (renderer=None).
            if child.renderer.is_none() {
                continue;
            }
            child.window.request_redraw();
        }
    }
}

impl App {
    pub(super) fn apply_theme_by_name(&mut self, name: &str) {
        if self.config.theme == name {
            return;
        }
        let Some(loader) = self.theme_loader.as_ref() else {
            tracing::warn!("ApplyTheme({name}): no theme_loader installed; ignoring");
            return;
        };
        let mut theme = match loader(name) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("ApplyTheme({name}): load failed: {e:#}");
                return;
            }
        };
        theme.apply_accessibility(&self.config.accessibility);
        if let Some(r) = self.main_renderer_mut() {
            r.set_theme(&theme);
        }
        for child in self.windows.values_mut() {
            if let Some(r) = child.renderer.as_mut() {
                r.set_theme(&theme);
            }
        }
        self.theme = theme;
        self.config.theme = name.to_string();
        propagate_theme_to_pane_parsers(&self.panes, &self.theme);
        for child in self.windows.values() {
            propagate_theme_to_pane_parsers(&child.panes, &self.theme);
        }
        // Theme swap changes presentation (colors) without mutating
        // cell contents — mark every pane dirty so the renderer
        // re-shapes with the new palette.
        mark_all_panes_dirty(&self.panes);
        for child in self.windows.values() {
            // Phase B2 PR-A: skip shadow main entry (renderer=None).
            if child.renderer.is_none() {
                continue;
            }
            mark_all_panes_dirty(&child.panes);
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
        for child in self.windows.values() {
            // Phase B2 PR-A: skip shadow main entry (renderer=None).
            if child.renderer.is_none() {
                continue;
            }
            child.window.request_redraw();
        }
        tracing::info!("theme -> {name}");
    }
    pub(super) fn change_font_size(&mut self, delta: f32) {
        let cur = self.config.font.size;
        let next = (cur + delta).clamp(8.0, 48.0);
        if (next - cur).abs() < f32::EPSILON {
            return;
        }
        self.set_font_size(next);
    }
    pub(super) fn reset_font_size(&mut self) {
        let default = sonic_core::config::FontConfig::default().size;
        if (self.config.font.size - default).abs() < f32::EPSILON {
            return;
        }
        self.set_font_size(default);
    }
    pub(super) fn set_font_size(&mut self, size: f32) {
        self.config.font.size = size;
        let family = self.config.font.family.clone();
        let line_h = self.config.font.line_height;
        if let Some(r) = self.main_renderer_mut() {
            r.set_font(&family, size, line_h);
        }
        let rects = self.compute_active_pane_rects();
        if let Some(r) = self.main_renderer() {
            let (cw, ch) = r.cell_size();
            resize_panes_to_rects(&self.panes, &rects, cw, ch);
        }
        for child in self.windows.values_mut() {
            let Some(r) = child.renderer.as_mut() else { continue };
            r.set_font(&family, size, line_h);
            let (cw, ch) = r.cell_size();
            let rects = App::compute_pane_rects_for(child);
            resize_panes_to_rects(&child.panes, &rects, cw, ch);
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
        for child in self.windows.values() {
            // Phase B2 PR-A: skip shadow main entry (renderer=None).
            if child.renderer.is_none() {
                continue;
            }
            child.window.request_redraw();
        }
        tracing::info!("font size -> {size}pt");
    }
    pub(super) fn toggle_tab_bar(&mut self) {
        self.tab_bar_visible = !self.tab_bar_visible;
        let visible = self.tab_bar_visible;
        tracing::info!("tab bar visible -> {visible}");
        let main_changed = if let Some(r) = self.main_renderer_mut() {
            r.set_tab_bar_visible(visible)
        } else {
            false
        };
        if main_changed {
            let rects = self.compute_active_pane_rects();
            if let Some(r) = self.main_renderer() {
                let (cw, ch) = r.cell_size();
                resize_panes_to_rects(&self.panes, &rects, cw, ch);
            }
        }
        for child in self.windows.values_mut() {
            let Some(r) = child.renderer.as_mut() else { continue };
            if r.set_tab_bar_visible(visible) {
                let (cw, ch) = r.cell_size();
                let rects = App::compute_pane_rects_for(child);
                resize_panes_to_rects(&child.panes, &rects, cw, ch);
            }
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
        for child in self.windows.values() {
            // Phase B2 PR-A: skip shadow main entry (renderer=None).
            if child.renderer.is_none() {
                continue;
            }
            child.window.request_redraw();
        }
    }
    pub(super) fn force_reload_config(&mut self) {
        let Some(path) = sonic_core::config::Config::default_path() else { return };
        match Config::load_or_default(&path) {
            Ok(cfg) => self.apply_new_config(cfg),
            Err(e) => tracing::warn!("force_reload_config: parse failed: {e:#}"),
        }
    }

    pub(super) fn open_config_file(&mut self) {
        match sonic_core::config::Config::open_user_config_file() {
            Ok(path) => tracing::info!("opened config file {path:?}"),
            Err(e) => tracing::warn!("open config file failed: {e:#}"),
        }
    }

    pub(super) fn open_keymap_file(&mut self) {
        match sonic_core::keymap::open_user_keymap_file() {
            Ok(path) => tracing::info!("opened keymap file {path:?}"),
            Err(e) => tracing::warn!("open keymap file failed: {e:#}"),
        }
    }
}
