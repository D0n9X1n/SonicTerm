//! Pane geometry refresh: rectangles, resizing, dirty marking, selection invalidation,
//! IME cursor placement, and parser theme colors.

use super::*;

/// Seed a freshly-created parser with the active theme's query-reply colours:
/// default fg/bg/cursor (OSC 10/11/12 `?`) AND the full 16-colour ANSI palette
/// (OSC 4 `?`). Every pane-spawn site calls this one function, so the OSC 4
/// palette wiring cannot be added to one path and forgotten on another. Per-slot colours that don't resolve are simply left unseeded
/// (the parser then suppresses that slot's reply rather than lying).
pub fn seed_parser_theme_colors(parser: &mut sonicterm_vt::vt::Parser, theme: &Theme) {
    if let Some((red, green, blue)) = theme.colors.foreground.rgb() {
        parser.set_theme_fg(red, green, blue);
    }
    if let Some((red, green, blue)) = theme.colors.background.rgb() {
        parser.set_theme_bg(red, green, blue);
    }
    if let Some((red, green, blue)) = theme.colors.cursor.rgb() {
        parser.set_theme_cursor(red, green, blue);
    }
    // OSC 4 palette: indices 0..=7 from `ansi.*`, 8..=15 from `bright.*`,
    // in the standard xterm slot order.
    let normal = [
        &theme.colors.ansi.black,
        &theme.colors.ansi.red,
        &theme.colors.ansi.green,
        &theme.colors.ansi.yellow,
        &theme.colors.ansi.blue,
        &theme.colors.ansi.magenta,
        &theme.colors.ansi.cyan,
        &theme.colors.ansi.white,
    ];
    let bright = [
        &theme.colors.bright.black,
        &theme.colors.bright.red,
        &theme.colors.bright.green,
        &theme.colors.bright.yellow,
        &theme.colors.bright.blue,
        &theme.colors.bright.magenta,
        &theme.colors.bright.cyan,
        &theme.colors.bright.white,
    ];
    for (palette_index, hex) in normal.iter().chain(bright.iter()).enumerate() {
        if let Some((red, green, blue)) = hex.rgb() {
            parser.set_theme_palette_color(palette_index as u8, red, green, blue);
        }
    }
}

/// Resize every pane in `panes` to `(cols, rows)`: both the parser's
/// grid and (if the pane owns one) the PTY child. Used by the window
/// resize handler and by the font live-reload path, where changing
/// cell metrics shifts how many cells fit inside the current window.
///
/// `pub` + `#[doc(hidden)]` so integration tests can exercise the
/// invariant on a synthetic pane map without needing a live wgpu
/// surface or a real shell.
#[doc(hidden)]
pub fn resize_all_panes(panes: &HashMap<u64, PaneState>, cols: u16, rows: u16) {
    for (pane_id, pane) in panes {
        crate::app::frame_counters::lock_parser(&pane.parser).resize(cols, rows);
        pane.resize_pty(*pane_id, cols, rows);
    }
}

/// Resize each pane in `panes` to the cells that fit inside its own
/// `sonicterm_ui::pane::Rect` (window-pixel logical rect produced by
/// `PaneTree::layout`). `cell_w` / `cell_h` are the logical cell metrics
/// from the renderer (`Renderer::cell_size()`).
///
/// This is the per-pane sizing counterpart to [`resize_all_panes`]: the
/// older helper sized every pane to the same whole-window `(cols, rows)`,
/// which is wrong as soon as a tab has more than one pane (an inactive
/// pane's grid then thinks it has more columns than it actually shows,
/// so TUIs like vim/htop draw past their visible border and the wrap
/// column is wrong on resize).
///
/// CLAUDE.md §4: uses `parser.lock()` (NOT `try_lock`) — same as
/// `resize_all_panes`. Call sites are app-thread (WindowEvent::Resized
/// and config-live-reload), not the render hot path, so the lock is
/// safe and a dropped resize would leave the grid wrong-sized for the
/// next burst of pty output.
///
/// `rects` whose `id` is missing from `panes` are silently skipped
/// (covers the brief window during tab close where the layout list
/// includes a pane that was just removed).
///
pub fn resize_panes_to_rects(
    panes: &HashMap<u64, PaneState>,
    rects: &[(u64, sonicterm_ui::pane::Rect)],
    cell_w: f32,
    cell_h: f32,
    content_inset: [f32; 4],
) {
    let [left, right, top, bottom] = content_inset;
    for (id, rect) in rects {
        let Some(pane) = panes.get(id) else {
            // When: `id` names a pane already removed from `panes` — the layout
            // list still carries it mid tab-close — so skip rather than resize it.
            continue;
        };
        let content_w = (rect.w - left - right).max(cell_w);
        let content_h = (rect.h - top - bottom).max(cell_h);
        let (cols, rows) = sonicterm_grid::grid::bounded_grid_size(
            (content_w / cell_w).floor() as u64,
            (content_h / cell_h).floor() as u64,
        );
        let mut parser = crate::app::frame_counters::lock_parser(&pane.parser);
        // The clock is read only while the counters are on, to time the resize's parser hold.
        let locked_at = pane.frame_counters.as_ref().map(|_| std::time::Instant::now());
        parser.resize(cols, rows);
        // The cell size sizes a kitty image placed without `c` or `r`, and so its cursor move.
        parser.set_cell_pixel_size(cell_w, cell_h);
        drop(parser);
        if let (Some(counters), Some(locked_at)) = (&pane.frame_counters, locked_at) {
            let held_us = u64::try_from(locked_at.elapsed().as_micros()).unwrap_or(u64::MAX);
            counters.vt.resize_hold.record_us(held_us);
        }
        pane.resize_pty(*id, cols, rows);
    }
}

pub(super) fn update_terminal_ime_cursor_area(
    throttle: &mut sonicterm_ui::ime::ImeCursorThrottle,
    pane: (u64, sonicterm_ui::pane::Rect),
    cursor: (u16, u16),
    cell_size: (f32, f32),
    padding: (f32, f32),
    publish: impl FnOnce(winit::dpi::PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>),
) {
    // Pane layout and metrics are physical already; scaling or adding the window top inset again would double the offset.
    let area = sonicterm_ui::ime::ImeCursorArea {
        pane_id: pane.0,
        position: (
            (pane.1.x + padding.0 + f32::from(cursor.1) * cell_size.0) as i32,
            (pane.1.y + padding.1 + f32::from(cursor.0) * cell_size.1) as i32,
        ),
        size: (cell_size.0.ceil() as u32, cell_size.1.ceil() as u32),
    };
    if throttle.should_update(area) {
        publish(area.position.into(), area.size.into());
    }
}

/// Mark every pane's grid fully dirty. Used by triggers that change
/// the renderer's *presentation* invariant without mutating any cell
/// content (theme swap, font swap, focus transition, selection change).
/// This is the foundation hook the upcoming RowCache will use to know
/// when its cached row data is stale even though grid revision did not
/// bump.
///
/// `pub` + `#[doc(hidden)]` so integration tests can exercise the
/// invariant on a synthetic pane map.
#[doc(hidden)]
pub fn mark_all_panes_dirty(panes: &HashMap<u64, PaneState>) {
    for pane in panes.values() {
        crate::app::frame_counters::lock_parser(&pane.parser).grid_mut().mark_all_dirty();
    }
}

/// Revalidate the authoritative selection and rebase its active drag anchor.
///
/// Callers already hold the active pane's parser guard during frame assembly;
/// accepting `&Grid` avoids re-locking that parser and the AB-BA deadlock such a
/// re-lock would cause. Search and copy-mode overlays own separate state and are
/// deliberately untouched.
#[doc(hidden)]
pub fn invalidate_selection_for_content(
    selection: &mut Option<Selection>,
    select_anchor: &mut (u64, u16),
    pane_id: u64,
    grid: &Grid,
) -> bool {
    let (anchor_belongs_to_selection, previous_evicted) =
        selection.as_ref().map_or((false, grid.scrollback_evicted()), |selection| {
            (selection.contains(select_anchor.0, select_anchor.1), selection.scrollback_evicted)
        });
    let should_clear = selection.as_mut().is_some_and(|selection| {
        sonicterm_ui::selection::revalidate_selection(selection, pane_id, grid)
    });
    if should_clear {
        *selection = None;
    } else if anchor_belongs_to_selection {
        // When: `anchor_belongs_to_selection` is true, apply the selection endpoints' scrollback rebase to its drag anchor.
        let rebased_rows = selection
            .as_ref()
            .map_or(0, |selection| selection.scrollback_evicted.saturating_sub(previous_evicted));
        select_anchor.0 = select_anchor.0.saturating_sub(rebased_rows);
    }
    should_clear
}

impl App {
    /// Window-pixel rects for every pane in the active tab.
    ///
    /// Derived from the main renderer's logical size, insets, and padding, so
    /// resize and config-reload sites share one geometry source. Empty before a
    /// renderer exists or when no tab is active.
    pub(crate) fn compute_active_pane_rects(&self) -> Vec<(u64, sonicterm_ui::pane::Rect)> {
        let Some(main) = self.main() else {
            // When: `main` has no window yet, so no surface exists to derive a
            // layout from and there is nothing to size panes against.
            return Vec::new();
        };
        let viewport = self.testing.test_viewport_override.map(|(outer, _, _)| outer);
        Self::compute_tab_pane_rects(main, main.tabs.active_index(), viewport, self.main_renderer())
    }

    /// Same as [`Self::compute_active_pane_rects`] but for a torn-out
    /// child window (its own renderer + tab_states).
    pub(crate) fn compute_pane_rects_for(
        child: &WindowState,
    ) -> Vec<(u64, sonicterm_ui::pane::Rect)> {
        let viewport = child.test_pane_viewport.map(|(outer, _, _)| outer);
        Self::compute_tab_pane_rects(
            child,
            child.tabs.active_index(),
            viewport,
            child.renderer.as_ref(),
        )
    }

    /// Window-pixel rects for every pane of tab `tab_index` in `window`, visible or not, laid
    /// out in the window's current surface. `viewport` is the headless test seam, used instead of
    /// `renderer`; empty when neither exists or the tab does not.
    pub(crate) fn compute_tab_pane_rects(
        window: &WindowState,
        tab_index: usize,
        viewport: Option<sonicterm_ui::pane::Rect>,
        renderer: Option<&GpuRenderer>,
    ) -> Vec<(u64, sonicterm_ui::pane::Rect)> {
        let Some(tab_state) = window.tab_states.get(tab_index) else {
            // When: `tab_index` names no entry in `tab_states`, so no pane tree exists to lay out.
            return Vec::new();
        };
        if let Some(outer) = viewport {
            // When: `viewport` supplies the outer rect, layout runs without a live renderer.
            return tab_state.tree.layout(outer);
        }
        let Some(renderer) = renderer else {
            // When: `renderer` is absent, logical size and insets are unavailable.
            return Vec::new();
        };
        tab_state.tree.layout(Self::pane_area(renderer))
    }

    /// The window-pixel area a tab's panes tile: the surface less the tab bar and title band.
    fn pane_area(renderer: &GpuRenderer) -> sonicterm_ui::pane::Rect {
        let (width_px, height_px) = renderer.logical_size();
        let top = (renderer.top_inset() - renderer.padding_top_px()).max(0.0);
        let bottom = renderer.bottom_inset();
        sonicterm_ui::pane::Rect::new(
            0.0,
            top,
            width_px.max(0.0),
            (height_px - top - bottom).max(0.0),
        )
    }

    /// The rect [`Self::compute_pane_rects_for`] gives `pane_id` in `child`'s active tab, laying out
    /// only the splits that contain it; `None` when it is not visible there.
    pub(crate) fn pane_rect_for(
        child: &WindowState,
        pane_id: u64,
    ) -> Option<sonicterm_ui::pane::Rect> {
        let tab_state = child.tab_states.get(child.tabs.active_index())?;
        let outer = match child.test_pane_viewport.map(|(outer, _, _)| outer) {
            Some(outer) => outer,
            None => Self::pane_area(child.renderer.as_ref()?),
        };
        tab_state.tree.pane_rect(outer, pane_id)
    }
}

#[cfg(test)]
#[path = "pane_refresh_tests.rs"]
mod pane_refresh_tests;
