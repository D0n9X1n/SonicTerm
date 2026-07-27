//! Extracted from `app/mod.rs` in refactor PR 8b (expose-then-extract).
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Direction, Keymap, ScrollAction};
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::PtyHandle;
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::selection::Selection;
use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use sonicterm_ui::tabs::{Tab, TabBar};
use sonicterm_vt::vt::{Parser, VtEvent};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_panes_to_rects,
    shell_quote_posix, with_integrated_titlebar, wrap_paste, App, PaneState, TabState, UserEvent,
    WindowState,
};

fn propagate_theme_to_pane_parsers(panes: &HashMap<u64, PaneState>, theme: &Theme) {
    for pane in panes.values() {
        // Config live-reload runs on the app thread, not the render hot path,
        // so lock() is intentional here. Dropping this update would leave OSC
        // 10/11/12 + OSC 4 palette replies stale for shells already attached to
        // the pane. Re-seeds the full set (fg/bg/cursor + 16-colour palette) so
        // a theme swap also refreshes the OSC 4 palette.
        let mut parser = pane.parser.lock();
        super::seed_parser_theme_colors(&mut parser, theme);
    }
}

/// Accepted `weight_scale` range, matching the clamp in `sonicterm-cfg`,
/// `sonicterm-gpu`, and `sonicterm-engine`. Stepping past either end is a
/// no-op rather than an error.
pub(super) const WEIGHT_SCALE_MIN: f32 = 0.5;
pub(super) const WEIGHT_SCALE_MAX: f32 = 5.0;

/// True iff any field in `new_cfg.font` differs from `old_cfg.font`
/// (family, size, or line_height) in a way that should drive a live
/// renderer re-apply.
///
/// Extracted as a free function so the file-watcher path can be
/// unit-tested without a live `GpuRenderer`.
pub fn config_diff_needs_font_apply(old_cfg: &Config, new_cfg: &Config) -> bool {
    config_diff_changes_font_metrics(old_cfg, new_cfg)
        || (new_cfg.font.effective_weight_scale() - old_cfg.font.effective_weight_scale()).abs()
            > f32::EPSILON
}

fn config_diff_changes_font_metrics(old_cfg: &Config, new_cfg: &Config) -> bool {
    new_cfg.font.family != old_cfg.font.family
        || (new_cfg.font.size - old_cfg.font.size).abs() > f32::EPSILON
        || (new_cfg.font.line_height - old_cfg.font.line_height).abs() > f32::EPSILON
}

/// True iff `appearance.scrollbar` changed and existing renderers need
/// their cached scrollbar policy updated.
///
/// Kept as a small public test seam because integration tests cannot
/// reliably construct a live `GpuRenderer` + wgpu surface headlessly;
/// `apply_new_config` uses the same comparison before calling
/// `GpuRenderer::set_scrollbar_mode` on every live window.
pub fn renderer_scrollbar_mode_differs(old_cfg: &Config, new_cfg: &Config) -> bool {
    old_cfg.appearance.scrollbar != new_cfg.appearance.scrollbar
}

/// True iff overlay panel padding changed and existing renderers need
/// their cached overlay layout invalidated.
pub fn renderer_panel_padding_differs(old_cfg: &Config, new_cfg: &Config) -> bool {
    (old_cfg.appearance.panel_padding - new_cfg.appearance.panel_padding).abs() > f32::EPSILON
}

impl App {
    pub(super) fn apply_new_config(&mut self, new_cfg: Config) {
        // Config is only applied on an explicit user reload, so it must
        // render immediately rather than at the next vsync deadline.
        self.input_dirty = true;
        self.warm_window_pool.clear();
        let assets = sonicterm_cfg::assets::asset_dir();

        // Theme. Re-read unconditionally: the theme lives in its own file, so
        // its contents can change while `[theme]` in sonicterm.toml still
        // names the same theme. Comparing names would make an explicit reload
        // silently skip an edited theme file.
        {
            let theme_path = Theme::resolve_path(&new_cfg.theme, &assets);
            match Theme::load_strict(&theme_path) {
                Ok(mut t) => {
                    t.apply_accessibility(&new_cfg.accessibility);
                    tracing::info!("reload: theme -> {}", t.name);
                    if let Some(r) = self.main_renderer_mut() {
                        r.set_theme(&t);
                    }
                    for child in self.windows.values_mut() {
                        if let Some(r) = child.renderer.as_mut() {
                            r.set_theme(&t);
                        }
                    }
                    self.theme = t;
                    for child in self.windows.values() {
                        propagate_theme_to_pane_parsers(&child.panes, &self.theme);
                    }
                    // Theme swap changes presentation (colors) without
                    // mutating cell contents — mark every pane dirty so
                    // the renderer re-shapes with the new palette.
                    for child in self.windows.values() {
                        // Phase B2 PR-A: skip shadow main entry (renderer=None).
                        if child.renderer.is_none() {
                            continue;
                        }
                        mark_all_panes_dirty(&child.panes);
                    }
                }
                Err(e) => tracing::warn!("reload: theme {:?} failed: {e:#}", theme_path),
            }
        }

        // Font
        let font_changed = config_diff_needs_font_apply(&self.config, &new_cfg);
        if font_changed {
            let metrics_changed = config_diff_changes_font_metrics(&self.config, &new_cfg);
            let weight_scale = new_cfg.font.effective_weight_scale();
            if let Some(r) = self.main_renderer_mut() {
                r.set_font(
                    &new_cfg.font.family,
                    new_cfg.font.size,
                    new_cfg.font.line_height,
                    weight_scale,
                );
            }
            // Cell metrics changed → resize each pane to its own PaneRect,
            // never to the whole window's dimensions.
            // PR-B2c: main is in self.windows so the loop below
            // covers main + every torn-out child. Each owns its own
            // GpuRenderer + pane rects.
            for child in self.windows.values_mut() {
                {
                    let Some(r) = child.renderer.as_mut() else { continue };
                    r.set_font(
                        &new_cfg.font.family,
                        new_cfg.font.size,
                        new_cfg.font.line_height,
                        weight_scale,
                    );
                }
                if metrics_changed {
                    let Some(r) = child.renderer.as_ref() else { continue };
                    let rects = App::compute_pane_rects_for(child);
                    let (cw, ch) = r.cell_size();
                    let inset = [
                        r.padding_left_px(),
                        r.padding_right_px(),
                        r.padding_top_px(),
                        r.padding_bottom_px(),
                    ];
                    resize_panes_to_rects(&child.panes, &rects, cw, ch, inset);
                }
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
        // resize. Mirrors the font-live-reload path above.
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
            // PR-B2c: the loop below covers main + every child.
            for child in self.windows.values_mut() {
                {
                    let Some(r) = child.renderer.as_mut() else { continue };
                    r.set_padding(pad);
                }
                let Some(r) = child.renderer.as_ref() else { continue };
                let rects = App::compute_pane_rects_for(child);
                let (cw, ch) = r.cell_size();
                let inset = [
                    r.padding_left_px(),
                    r.padding_right_px(),
                    r.padding_top_px(),
                    r.padding_bottom_px(),
                ];
                resize_panes_to_rects(&child.panes, &rects, cw, ch, inset);
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

        if renderer_scrollbar_mode_differs(&self.config, &new_cfg) {
            if let Some(r) = self.main_renderer_mut() {
                r.set_scrollbar_mode(new_cfg.appearance.scrollbar);
            }
            for child in self.windows.values_mut() {
                if let Some(r) = child.renderer.as_mut() {
                    r.set_scrollbar_mode(new_cfg.appearance.scrollbar);
                }
            }
            tracing::info!(?new_cfg.appearance.scrollbar, "live-reload: appearance scrollbar");
        }

        if renderer_panel_padding_differs(&self.config, &new_cfg) {
            if let Some(r) = self.main_renderer_mut() {
                r.set_panel_padding(new_cfg.appearance.panel_padding);
            }
            for child in self.windows.values_mut() {
                if let Some(r) = child.renderer.as_mut() {
                    r.set_panel_padding(new_cfg.appearance.panel_padding);
                }
            }
            tracing::info!(
                panel_padding = new_cfg.appearance.panel_padding,
                "live-reload: appearance panel padding"
            );
        }

        if new_cfg.appearance.software_render_mode != self.config.appearance.software_render_mode {
            self.software_render_degrade = self.main_renderer().is_some_and(|r| {
                super::should_degrade_for_software_render(
                    new_cfg.appearance.software_render_mode,
                    r.is_software_rendering(),
                )
            });
            if let Some(r) = self.main_renderer_mut() {
                let degrade = super::should_degrade_for_software_render(
                    new_cfg.appearance.software_render_mode,
                    r.is_software_rendering(),
                );
                r.set_software_render_degrade(degrade);
            }
            for child in self.windows.values_mut() {
                if let Some(r) = child.renderer.as_mut() {
                    let degrade = super::should_degrade_for_software_render(
                        new_cfg.appearance.software_render_mode,
                        r.is_software_rendering(),
                    );
                    r.set_software_render_degrade(degrade);
                }
            }
            // Resolved from the monitor's own period, never from
            // `frame_period`: that field already holds the software cap when
            // degrade was previously engaged, so resolving from it would make
            // the decision one-way and leave the window capped after the user
            // turns software rendering off.
            self.frame_period = super::software_render_frame_period(
                self.software_render_degrade,
                self.monitor_frame_period,
            );
            tracing::info!(
                ?new_cfg.appearance.software_render_mode,
                degrade = self.software_render_degrade,
                "live-reload: appearance software_render_mode"
            );
        }

        // Tab maximum width (logical px). Held process-globally in
        // `tabbar_view`, so updating it once reaches every window's layout
        // and hit-testing on the next frame — no per-renderer push needed.
        if (new_cfg.tab_max_width - self.config.tab_max_width).abs() > f32::EPSILON {
            sonicterm_ui::tabbar_view::set_max_tab_width(new_cfg.tab_max_width);
            tracing::info!("live-reload: tab_max_width -> {}", new_cfg.tab_max_width);
        }

        // Deprecated tab-close compatibility key. Propagate changes so every
        // renderer drops its cached frame consistently; the close button is no
        // longer drawn and the renderer setter intentionally has no visual effect.
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

        // Keymap. Unconditional for the same reason as the theme: keymap.toml
        // is a separate file whose bindings can change while `[keymap]` still
        // names it.
        {
            let km_path = Keymap::resolve_path(&new_cfg.keymap, &assets);
            match self
                .keymap_loader
                .as_ref()
                .map_or_else(|| Keymap::load_strict(&km_path), |loader| loader(&new_cfg.keymap))
            {
                Ok(km) => {
                    tracing::info!(
                        "reload: keymap -> {} ({} bindings)",
                        km.meta.name,
                        km.bindings.len()
                    );
                    self.command_palette.set_keymap(&km);
                    self.keymap = km;
                }
                Err(e) => tracing::warn!("reload: keymap {:?} failed: {e:#}", km_path),
            }
        }

        // Scrollback depth: re-apply to every live pane across all windows
        // (and all tabs — every pane lives in `ws.panes`, inactive tabs
        // included). Lowering the cap drains excess history immediately
        // . App-thread, not render hot path, so lock is fine.
        if new_cfg.terminal.scrollback != self.config.terminal.scrollback {
            let limit = new_cfg.terminal.scrollback;
            tracing::info!("live-reload: scrollback -> {limit} rows");
            for child in self.windows.values() {
                for pane in child.panes.values() {
                    pane.parser.lock().grid_mut().set_scrollback_limit(limit);
                }
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
            child.request_redraw();
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
        for child in self.windows.values() {
            propagate_theme_to_pane_parsers(&child.panes, &self.theme);
        }
        // Theme swap changes presentation (colors) without mutating
        // cell contents — mark every pane dirty so the renderer
        // re-shapes with the new palette.
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
            child.request_redraw();
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
        // Return to the configured size, not `FontConfig::default()` — the
        // compile-time default is a size the user never asked for.
        let target = self.configured_font_size;
        if (self.config.font.size - target).abs() < f32::EPSILON {
            return;
        }
        self.set_font_size(target);
    }

    /// Step regular-text weight by `delta`, clamped to the accepted
    /// `weight_scale` range. Weight does not affect cell metrics, so unlike a
    /// size change this never resizes a grid or PTY.
    pub(super) fn change_font_weight(&mut self, delta: f32) {
        let cur = self.config.font.effective_weight_scale();
        let next = (cur + delta).clamp(WEIGHT_SCALE_MIN, WEIGHT_SCALE_MAX);
        if (next - cur).abs() < f32::EPSILON {
            return;
        }
        self.set_font_weight(next);
    }

    /// Return regular-text weight to the configured `weight_scale`, discarding
    /// transient palette adjustments.
    pub(super) fn reset_font_weight(&mut self) {
        let target = self.configured_weight_scale;
        if (self.config.font.effective_weight_scale() - target).abs() < f32::EPSILON {
            return;
        }
        self.set_font_weight(target);
    }

    pub(super) fn set_font_weight(&mut self, weight_scale: f32) {
        self.config.font.weight_scale = weight_scale;
        let family = self.config.font.family.clone();
        let size = self.config.font.size;
        let line_h = self.config.font.line_height;
        let weight_scale = self.config.font.effective_weight_scale();
        if let Some(r) = self.main_renderer_mut() {
            r.set_font(&family, size, line_h, weight_scale);
        }
        for child in self.windows.values_mut() {
            if let Some(r) = child.renderer.as_mut() {
                r.set_font(&family, size, line_h, weight_scale);
            }
        }
        self.input_dirty = true;
        for child in self.windows.values() {
            if child.renderer.is_none() {
                continue;
            }
            mark_all_panes_dirty(&child.panes);
            child.request_redraw();
        }
        tracing::info!("font weight_scale -> {weight_scale}");
    }
    pub(super) fn set_font_size(&mut self, size: f32) {
        self.config.font.size = size;
        let family = self.config.font.family.clone();
        let line_h = self.config.font.line_height;
        let weight_scale = self.config.font.effective_weight_scale();
        if let Some(r) = self.main_renderer_mut() {
            r.set_font(&family, size, line_h, weight_scale);
        }
        // PR-B2c: the loop below covers main + every child.
        for child in self.windows.values_mut() {
            {
                let Some(r) = child.renderer.as_mut() else { continue };
                r.set_font(&family, size, line_h, weight_scale);
            }
            let Some(r) = child.renderer.as_ref() else { continue };
            let rects = App::compute_pane_rects_for(child);
            let (cw, ch) = r.cell_size();
            let inset = [
                r.padding_left_px(),
                r.padding_right_px(),
                r.padding_top_px(),
                r.padding_bottom_px(),
            ];
            resize_panes_to_rects(&child.panes, &rects, cw, ch, inset);
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
        for child in self.windows.values() {
            // Phase B2 PR-A: skip shadow main entry (renderer=None).
            if child.renderer.is_none() {
                continue;
            }
            child.request_redraw();
        }
        tracing::info!("font size -> {size}pt");
    }
    pub(super) fn toggle_tab_bar(&mut self) {
        self.tab_bar_visible = !self.tab_bar_visible;
        let visible = self.tab_bar_visible;
        tracing::info!("tab bar visible -> {visible}");
        // PR-B2c: the loop below covers main + every child.
        for child in self.windows.values_mut() {
            let changed = {
                let Some(r) = child.renderer.as_mut() else { continue };
                r.set_tab_bar_visible(visible)
            };
            if changed {
                let Some(r) = child.renderer.as_ref() else { continue };
                let rects = App::compute_pane_rects_for(child);
                let (cw, ch) = r.cell_size();
                let inset = [
                    r.padding_left_px(),
                    r.padding_right_px(),
                    r.padding_top_px(),
                    r.padding_bottom_px(),
                ];
                resize_panes_to_rects(&child.panes, &rects, cw, ch, inset);
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
            child.request_redraw();
        }
    }
    /// Re-read `sonicterm.toml` from disk and apply it, along with the theme
    /// and keymap files it names. This is the only path that reads config
    /// after startup — there is no background watcher, so an edit takes effect
    /// when the user asks for it and not before.
    pub(super) fn force_reload_config(&mut self) {
        let Some(path) = sonicterm_cfg::config::Config::default_path() else { return };
        match Config::load_strict(&path) {
            Ok(cfg) => {
                // The reset targets follow the config the session has loaded.
                self.configured_font_size = cfg.font.size;
                self.configured_weight_scale = cfg.font.effective_weight_scale();
                self.apply_new_config(cfg);
            }
            Err(e) => tracing::warn!("reload: config parse failed: {e:#}"),
        }
    }

    pub(super) fn open_config_file(&mut self) {
        match sonicterm_cfg::config::Config::open_user_config_file() {
            Ok(path) => tracing::info!("opened config file {path:?}"),
            Err(e) => tracing::warn!("open config file failed: {e:#}"),
        }
    }

    pub(super) fn open_keymap_file(&mut self) {
        match sonicterm_cfg::keymap::open_user_keymap_file() {
            Ok(path) => tracing::info!("opened keymap file {path:?}"),
            Err(e) => tracing::warn!("open keymap file failed: {e:#}"),
        }
    }
}

#[cfg(test)]
#[path = "config_apply_tests.rs"]
mod config_apply_tests;
