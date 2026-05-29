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
use sonic_ui::prefs::PrefsHit;

/// True iff any field in `new_cfg.font` differs from `old_cfg.font`
/// (family, size, or line_height) in a way that should drive a live
/// renderer re-apply.
///
/// Extracted as a free function so the prefs commit path
/// (`App::commit_prefs_and_apply_live`) and the file-watcher path
/// (`App::apply_new_config`) share one source of truth and so this
/// classification can be unit-tested without a live `GpuRenderer`.
///
/// Regression cover: issue #167 — changing `font.size` in prefs had
/// no live effect because the commit path only handled theme/keymap
/// and the file-watcher's later diff saw the live `self.config`
/// already updated (no-op).
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
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_theme(&t);
                    }
                    for child in self.windows.values_mut() {
                        child.renderer.set_theme(&t);
                    }
                    self.theme = t;
                    // Theme swap changes presentation (colors) without
                    // mutating cell contents — mark every pane dirty so
                    // the renderer re-shapes with the new palette.
                    mark_all_panes_dirty(&self.panes);
                    for child in self.windows.values() {
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
            if let Some(r) = self.renderer.as_mut() {
                r.set_theme(&t);
            }
            for child in self.windows.values_mut() {
                child.renderer.set_theme(&t);
            }
            self.theme = t;
            mark_all_panes_dirty(&self.panes);
            for child in self.windows.values() {
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
            if let Some(r) = self.renderer.as_mut() {
                r.set_font(&new_cfg.font.family, new_cfg.font.size, new_cfg.font.line_height);
            }
            // Cell metrics changed → resize each pane to its own PaneRect.
            // See docs/specs/per-pane-grids.md for why this is per-pane,
            // not whole-window.
            let rects = self.compute_active_pane_rects();
            if let Some(r) = self.renderer.as_ref() {
                let (cw, ch) = r.cell_size();
                resize_panes_to_rects(&self.panes, &rects, cw, ch);
            }
            // Apply the same swap to every torn-out child window. Each
            // child owns its own GpuRenderer, so it needs the font
            // change AND the matching pane resize against its own cell
            // metrics (its window can be a different size from main).
            for child in self.windows.values_mut() {
                child.renderer.set_font(
                    &new_cfg.font.family,
                    new_cfg.font.size,
                    new_cfg.font.line_height,
                );
                let rects = App::compute_pane_rects_for(child);
                let (cw, ch) = child.renderer.cell_size();
                resize_panes_to_rects(&child.panes, &rects, cw, ch);
            }
            tracing::info!(
                "live-reload: font -> {} @ {}px x{}",
                new_cfg.font.family,
                new_cfg.font.size,
                new_cfg.font.line_height,
            );
        }

        // Cursor visuals — cheap to apply; the setters short-circuit
        // when nothing changed, so an unrelated config edit (e.g. a
        // theme swap) doesn't reset the blink phase.
        if let Some(r) = self.renderer.as_mut() {
            r.set_cursor_shape(new_cfg.terminal.cursor_shape);
            r.set_cursor_blink(new_cfg.terminal.cursor_blink);
        }
        for child in self.windows.values_mut() {
            child.renderer.set_cursor_shape(new_cfg.terminal.cursor_shape);
            child.renderer.set_cursor_blink(new_cfg.terminal.cursor_blink);
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
            if let Some(r) = self.renderer.as_mut() {
                r.set_padding(pad);
            }
            let rects = self.compute_active_pane_rects();
            if let Some(r) = self.renderer.as_ref() {
                let (cw, ch) = r.cell_size();
                resize_panes_to_rects(&self.panes, &rects, cw, ch);
            }
            for child in self.windows.values_mut() {
                child.renderer.set_padding(pad);
                let rects = App::compute_pane_rects_for(child);
                let (cw, ch) = child.renderer.cell_size();
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
            if let Some(r) = self.renderer.as_mut() {
                r.set_theme_with_opacity(&self.theme, new_cfg.appearance.opacity);
            }
            for child in self.windows.values_mut() {
                child.renderer.set_theme_with_opacity(&self.theme, new_cfg.appearance.opacity);
            }
            tracing::info!(opacity = new_cfg.appearance.opacity, "live-reload: appearance opacity");
        }

        // Tab close-button override (wezterm `tab_close_button_color`).
        // Diff against the live config so an edit that adds, changes,
        // or clears the value propagates to the main + every child
        // renderer without a restart.
        if new_cfg.tab_close_button_color != self.config.tab_close_button_color {
            if let Some(r) = self.renderer.as_mut() {
                r.set_tab_close_override(new_cfg.tab_close_button_color.as_deref());
            }
            for child in self.windows.values_mut() {
                child.renderer.set_tab_close_override(new_cfg.tab_close_button_color.as_deref());
            }
            tracing::info!(
                "live-reload: tab_close_button_color -> {:?}",
                new_cfg.tab_close_button_color
            );
        }

        if new_cfg.accessibility.reduced_motion != self.config.accessibility.reduced_motion
            || new_cfg.accessibility.strong_focus != self.config.accessibility.strong_focus
        {
            if let Some(w) = self.prefs_window.as_ref() {
                w.request_redraw();
            }
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
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        for child in self.windows.values() {
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
        if let Some(r) = self.renderer.as_mut() {
            r.set_theme(&theme);
        }
        for child in self.windows.values_mut() {
            child.renderer.set_theme(&theme);
        }
        self.theme = theme;
        self.config.theme = name.to_string();
        // Theme swap changes presentation (colors) without mutating
        // cell contents — mark every pane dirty so the renderer
        // re-shapes with the new palette.
        mark_all_panes_dirty(&self.panes);
        for child in self.windows.values() {
            mark_all_panes_dirty(&child.panes);
        }
        // Keep the prefs surface (if open) in sync — the Appearance
        // Accent swatch and theme-derived chrome must follow live.
        if let Some(prefs) = self.prefs_state.as_mut() {
            prefs.set_theme(self.theme.clone());
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        for child in self.windows.values() {
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
        if let Some(r) = self.renderer.as_mut() {
            r.set_font(&family, size, line_h);
        }
        let rects = self.compute_active_pane_rects();
        if let Some(r) = self.renderer.as_ref() {
            let (cw, ch) = r.cell_size();
            resize_panes_to_rects(&self.panes, &rects, cw, ch);
        }
        for child in self.windows.values_mut() {
            child.renderer.set_font(&family, size, line_h);
            let rects = App::compute_pane_rects_for(child);
            let (cw, ch) = child.renderer.cell_size();
            resize_panes_to_rects(&child.panes, &rects, cw, ch);
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        for child in self.windows.values() {
            child.window.request_redraw();
        }
        tracing::info!("font size -> {size}pt");
    }
    pub(super) fn toggle_tab_bar(&mut self) {
        self.tab_bar_visible = !self.tab_bar_visible;
        let visible = self.tab_bar_visible;
        tracing::info!("tab bar visible -> {visible}");
        let main_changed = if let Some(r) = self.renderer.as_mut() {
            r.set_tab_bar_visible(visible)
        } else {
            false
        };
        if main_changed {
            let rects = self.compute_active_pane_rects();
            if let Some(r) = self.renderer.as_ref() {
                let (cw, ch) = r.cell_size();
                resize_panes_to_rects(&self.panes, &rects, cw, ch);
            }
        }
        for child in self.windows.values_mut() {
            if child.renderer.set_tab_bar_visible(visible) {
                let rects = App::compute_pane_rects_for(child);
                let (cw, ch) = child.renderer.cell_size();
                resize_panes_to_rects(&child.panes, &rects, cw, ch);
            }
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        for child in self.windows.values() {
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
}
