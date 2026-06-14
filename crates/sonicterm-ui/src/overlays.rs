//! Pure layout helpers for the three state-only overlays drawn over the
//! terminal grid:
//!
//! 1. Command palette — centered modal (~520×460) with a query input row
//!    and a filtered action list. State lives in
//!    [`crate::command_palette::CommandPalette`].
//! 2. Search bar — bottom-right single-line status with `N/M` match
//!    counter. State lives in [`crate::search::SearchState`] (one per
//!    tab).
//! 3. IME preedit — below the cursor, with an underline. State lives in
//!    [`crate::ime::ImeState`].
//!
//! No GPU types here — these helpers compute coordinates from a viewport
//! size + the state and return rectangles + label strings. The renderer
//! turns them into `QuadInstance`s and the legacy chrome layer `TextArea`s. Pure logic
//! keeps the path covered by unit tests without a wgpu device.
//!
//! Coordinate system: physical pixels, origin top-left (the same system
//! [`crate::tabbar_view`] uses).

use crate::command_label::label as action_label;
use crate::command_palette::{CommandPalette, CommandPaletteMode};
use crate::ime::ImeState;
use crate::search::SearchState;
use crate::tabbar_view::Rect;

// TODO: switch to ui_tokens after #115 merges. Until then the design
// tokens from issue #112 Round 1 live here as named constants so that
// `render.rs` and the integration tests can reference them by name and
// stay self-documenting.

/// Ideal modal width in physical pixels (Raycast-style redesign).
pub const PALETTE_WIDTH: f32 = 520.0;

/// Ideal modal height in physical pixels.
pub const PALETTE_HEIGHT: f32 = 400.0;

/// Hard upper bound on the modal width — the layout never grows past this
/// even on very wide windows. The viewport-relative clamp is
/// `viewport_w - 48`, whichever is smaller (see [`PaletteLayout::compute`]).
pub const PALETTE_MAX_WIDTH: f32 = 560.0;

/// Hard upper bound on the modal height. Viewport-relative clamp is
/// `viewport_h - 96`.
pub const PALETTE_MAX_HEIGHT: f32 = 460.0;

/// Top margin: the modal's top edge sits at `max(72, viewport_h * 0.18)`.
pub const PALETTE_TOP_RATIO: f32 = 0.18;

/// Minimum distance from the top of the viewport to the modal top edge.
pub const PALETTE_TOP_MIN: f32 = 72.0;

/// 1px chrome border around the modal.
pub const PALETTE_BORDER: f32 = 1.0;

/// Height of the query input field.
pub const PALETTE_QUERY_HEIGHT: f32 = 42.0;

/// Horizontal padding inside the query field.
pub const PALETTE_QUERY_PAD_X: f32 = 16.0;

/// Vertical padding inside the query field.
pub const PALETTE_QUERY_PAD_Y: f32 = 6.0;

/// Search icon size + offset inside the query field.
pub const PALETTE_QUERY_ICON_SIZE: f32 = 16.0;
pub const PALETTE_QUERY_ICON_X: f32 = 16.0;

/// Row height inside the action list.
pub const PALETTE_ROW_HEIGHT: f32 = 28.0;

/// Vertical gap between consecutive rows.
pub const PALETTE_ROW_GAP: f32 = 2.0;

/// Horizontal padding inside each row.
pub const PALETTE_ROW_PAD_X: f32 = 14.0;

/// Minimum gap between the command label and shortcut hint columns.
pub const PALETTE_ROW_COLUMN_GAP: f32 = 28.0;

/// Footer height (count + nav hint strip at the bottom of the modal).
pub const PALETTE_FOOTER_HEIGHT: f32 = 30.0;

/// Default inset between the modal edge and the inner content (rows, query,
/// footer). Users can override this via `appearance.panel_padding`.
pub const PALETTE_INNER_PAD: f32 = 2.0;

/// Corner radius for the modal panel (and its 1px border ring) in physical
/// pixels. Quad pipeline draws this via an SDF-style rounded-rect path
/// (see `sonicterm-shared/src/quad.rs`).
pub const PALETTE_PANEL_RADIUS: f32 = 16.0;

/// Corner radius for the query input field. Slightly tighter than the
/// panel so it reads as nested chrome.
pub const PALETTE_QUERY_RADIUS: f32 = 8.0;

/// Corner radius for the selected-row highlight quad.
pub const PALETTE_ROW_RADIUS: f32 = 6.0;

/// Margin between the search bar and the right/top window edge.
pub const SEARCH_BAR_MARGIN: f32 = 12.0;

/// Maximum width of the small top-right search bar.
pub const SEARCH_BAR_WIDTH: f32 = 600.0;

/// Minimum width of the search bar before query text expands it.
pub const SEARCH_BAR_MIN_WIDTH: f32 = 200.0;

/// Left padding inside the search/read-only badges.
pub const SEARCH_BAR_PAD_LEFT: f32 = 20.0;

/// Right padding inside the search/read-only badges.
pub const SEARCH_BAR_PAD_RIGHT: f32 = 15.0;

/// Gap between the fixed leading Nerd Font icon and text.
pub const SEARCH_BAR_ICON_GAP: f32 = 10.0;

/// Height of the small top-right search bar.
pub const SEARCH_BAR_HEIGHT: f32 = 26.0;

/// Layout of the command-palette modal.
#[derive(Debug, Clone)]
pub struct PaletteLayout {
    /// Full-window scrim (dim layer under the modal). Covers the entire
    /// viewport behind `border`.
    pub scrim: Rect,
    /// 1px border rectangle (drawn under everything else).
    pub border: Rect,
    /// Modal background rectangle, inset by [`PALETTE_BORDER`] from
    /// `border`.
    pub bg: Rect,
    /// Query-input row at the top of the modal.
    pub query_row: Rect,
    /// Search-icon rectangle inside the query field.
    pub query_icon: Rect,
    /// One rect per visible action row. May be empty when the palette
    /// hides every action (e.g. a query with no matches).
    pub rows: Vec<PaletteRow>,
    /// Index of the highlighted row inside `rows`, if any. The highlight
    /// is only emitted when the selected index actually falls inside the
    /// visible window (see scroll clamping in [`PaletteLayout::compute`]).
    pub selected_row: Option<usize>,
    /// Query string the renderer should paint into `query_row`. The
    /// trailing block cursor is appended so the user can see the caret.
    /// No `> ` prefix any more — the search icon stands in for it.
    pub query_label: String,
    /// Placeholder shown inside the query field when `query_label` is
    /// effectively empty (just the cursor). Renderer paints this in the
    /// muted placeholder color and only when the user hasn't typed.
    pub query_placeholder: Option<String>,
    /// Display labels for each row in `rows`, parallel order.
    pub row_labels: Vec<String>,
    /// Display shortcut hints for each row in `rows`, parallel order.
    pub row_shortcuts: Vec<Option<String>>,
    /// Optional color swatches for each row in `rows`, parallel order.
    pub row_swatches: Vec<Option<String>>,
    /// When the filter produced zero matches, the layout still emits a
    /// modal + query row but `rows` is empty; the renderer should paint
    /// this centered placeholder string instead of the unfiltered list.
    /// `None` whenever `rows` is non-empty.
    pub empty_label: Option<String>,
    /// Secondary hint shown under the empty placeholder.
    pub empty_hint: Option<String>,
    /// Footer rectangle at the bottom of the modal (count + nav hints).
    pub footer: Rect,
    /// Footer label, e.g. `"42 commands · ↑↓ navigate · ↵ run · esc close"`.
    pub footer_label: String,
}

/// One row inside the palette action list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaletteRow {
    /// Index into [`CommandPalette::visible`] that this row paints.
    pub item_index: usize,
    pub rect: Rect,
}

impl PaletteLayout {
    /// Build the layout for a window of `window_w × window_h` physical
    /// pixels. Returns `None` when the palette is closed — callers should
    /// not draw anything in that case.
    ///
    /// Takes the palette by `&mut` so it can publish the current
    /// `visible_rows` count back into the state — subsequent arrow-key
    /// navigation uses that to keep the highlighted row inside the
    /// rendered viewport (the bug this PR fixes was that the selection
    /// could move past the visible window and the highlight quad would
    /// then be drawn offscreen below the modal).
    #[must_use]
    pub fn compute(
        palette: &mut CommandPalette,
        window_w: f32,
        window_h: f32,
        panel_padding: f32,
        scale: f32,
    ) -> Option<PaletteLayout> {
        if !palette.is_open() {
            return None;
        }
        // DPI scale for SIZE terms only. Window-relative POSITION terms
        // (centering, top ratio/min, the `window_w - 48` / `window_h - 96`
        // clamps) stay in window pixels and are NOT multiplied by `s`.
        let s = scale.max(0.01);
        // panel_padding is a caller-supplied inset that contributes to the
        // inner content size, so it scales with the other SIZE terms.
        let panel_padding = panel_padding.max(0.0) * s;
        let border_px = PALETTE_BORDER * s;
        // Spec: width is `min(560, viewport_w - 48)`, ideal 520.
        // Height: `min(520, viewport_h - 96)`, ideal 460. The ideal/max
        // SIZE bounds scale; the viewport-relative clamp does not.
        let modal_w =
            (PALETTE_WIDTH * s).min(PALETTE_MAX_WIDTH * s).min((window_w - 48.0).max(160.0));
        let modal_h =
            (PALETTE_HEIGHT * s).min(PALETTE_MAX_HEIGHT * s).min((window_h - 96.0).max(120.0));
        let border_x = ((window_w - modal_w) * 0.5).max(0.0);
        let border_y =
            (window_h * PALETTE_TOP_RATIO).max(PALETTE_TOP_MIN).min((window_h - modal_h).max(0.0));
        let scrim = Rect { x: 0.0, y: 0.0, w: window_w, h: window_h };
        let border = Rect { x: border_x, y: border_y, w: modal_w, h: modal_h };
        let bg = Rect {
            x: border.x + border_px,
            y: border.y + border_px,
            w: (border.w - border_px * 2.0).max(0.0),
            h: (border.h - border_px * 2.0).max(0.0),
        };
        let query_row = Rect {
            x: bg.x + panel_padding,
            y: bg.y + panel_padding,
            w: (bg.w - panel_padding * 2.0).max(0.0),
            h: PALETTE_QUERY_HEIGHT * s,
        };
        let query_icon = Rect {
            x: query_row.x + PALETTE_QUERY_ICON_X * s,
            y: query_row.y + (query_row.h - PALETTE_QUERY_ICON_SIZE * s) * 0.5,
            w: PALETTE_QUERY_ICON_SIZE * s,
            h: PALETTE_QUERY_ICON_SIZE * s,
        };
        let footer_h = PALETTE_FOOTER_HEIGHT * s;
        let footer = Rect {
            x: bg.x,
            y: (bg.y + bg.h - footer_h).max(query_row.y + query_row.h),
            w: bg.w,
            h: footer_h,
        };

        // Action list region (everything between the query row and the footer).
        let row_height = PALETTE_ROW_HEIGHT * s;
        let row_gap = PALETTE_ROW_GAP * s;
        let list_top = query_row.y + query_row.h + panel_padding;
        let list_bottom = footer.y - panel_padding;
        let avail = (list_bottom - list_top).max(0.0);
        let row_stride = row_height + row_gap;
        let max_rows =
            if row_stride > 0.0 { ((avail + row_gap) / row_stride).floor() as usize } else { 0 };

        // Publish viewport size to the state so the next key press can
        // clamp scroll_offset correctly.
        palette.set_visible_rows(max_rows);

        let visible = palette.visible();
        let color_choices = palette.tab_color_choices();
        let total = match palette.mode() {
            CommandPaletteMode::TabColor => color_choices.len(),
            _ => visible.len(),
        };

        let window_start = palette.scroll_offset().min(total.saturating_sub(max_rows));
        let window_end = (window_start + max_rows).min(total);
        let selected = palette.selected();

        let mut rows = Vec::with_capacity(window_end.saturating_sub(window_start));
        let mut row_labels = Vec::with_capacity(rows.capacity());
        let mut row_shortcuts = Vec::with_capacity(rows.capacity());
        let mut row_swatches = Vec::with_capacity(rows.capacity());
        for (i, item_index) in (window_start..window_end).enumerate() {
            let r = Rect {
                x: bg.x + panel_padding,
                y: list_top + (i as f32) * row_stride,
                w: (bg.w - panel_padding * 2.0).max(0.0),
                h: row_height,
            };
            rows.push(PaletteRow { item_index, rect: r });
            match palette.mode() {
                CommandPaletteMode::TabColor => {
                    if let Some(choice) = color_choices.get(item_index) {
                        row_labels.push(format!("{} — {}", choice.name, palette.tab_color_title()));
                        row_shortcuts.push(None);
                        row_swatches.push(Some(choice.hex.clone()));
                    } else {
                        row_labels.push(String::new());
                        row_shortcuts.push(None);
                        row_swatches.push(None);
                    }
                }
                _ => {
                    if let Some(a) = visible.get(item_index) {
                        row_labels.push(action_label(a));
                        row_shortcuts.push(
                            palette.shortcut_hint_for_visible_index(item_index).map(str::to_string),
                        );
                    } else {
                        row_labels.push(String::new());
                        row_shortcuts.push(None);
                    }
                    row_swatches.push(None);
                }
            }
        }
        let selected_row = if total > 0 && selected >= window_start && selected < window_end {
            Some(selected - window_start)
        } else {
            None
        };
        let query_label = if palette.mode() == CommandPaletteMode::TabColor {
            format!("Color for {}▏", palette.tab_color_title())
        } else {
            command_palette_query_label(palette, "")
        };
        let query_placeholder = if palette.query().is_empty() && palette.mode() != CommandPaletteMode::TabColor {
            Some(match palette.mode() {
                CommandPaletteMode::Commands => {
                    String::from("Search commands, settings, shortcuts…")
                }
                CommandPaletteMode::RenameTab => String::from("New tab title…"),
                CommandPaletteMode::TabColor => String::new(),
            })
        } else {
            None
        };

        let empty_label = if palette.mode() == CommandPaletteMode::RenameTab {
            None
        } else if total == 0 && !palette.query().is_empty() {
            Some(NO_MATCHES.to_string())
        } else {
            None
        };
        let empty_hint = if empty_label.is_some() {
            Some(String::from("Try settings, split, font, shortcut"))
        } else {
            None
        };

        let footer_label = match palette.mode() {
            CommandPaletteMode::Commands => format!(
                "{} command{} · ↑↓ navigate · ↵ run · esc close",
                total,
                if total == 1 { "" } else { "s" }
            ),
            CommandPaletteMode::RenameTab => "↵ rename · esc cancel".to_string(),
            CommandPaletteMode::TabColor => "↑↓ choose color · ↵ apply · esc cancel".to_string(),
        };

        Some(PaletteLayout {
            scrim,
            border,
            bg,
            query_row,
            query_icon,
            rows,
            selected_row,
            query_label,
            query_placeholder,
            row_labels,
            row_shortcuts,
            row_swatches,
            empty_label,
            empty_hint,
            footer,
            footer_label,
        })
    }
}

/// Placeholder shown in the action list when the current query filters
/// every action out. Exposed for tests + so the renderer doesn't have to
/// duplicate the string.
pub const NO_MATCHES: &str = "No commands found";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationBubble {
    pub level: NotificationLevel,
    pub message: String,
    pub expires_at: Option<std::time::Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NotificationBubbleLayout {
    pub bg: Rect,
    pub border: Rect,
    pub close: Rect,
}

impl NotificationBubbleLayout {
    #[must_use]
    pub fn compute(
        window_w: f32,
        window_h: f32,
        content_w: f32,
        row: u8,
        scale: f32,
    ) -> NotificationBubbleLayout {
        let s = scale.max(0.01);
        let close_w = SEARCH_BAR_HEIGHT * s;
        let layout = SearchBarLayout::compute_at_row(window_w, window_h, content_w + close_w, row, scale);
        let close = Rect {
            x: (layout.border.x + layout.border.w - close_w).max(layout.border.x),
            y: layout.border.y,
            w: close_w.min(layout.border.w),
            h: layout.border.h,
        };
        NotificationBubbleLayout { bg: layout.bg, border: layout.border, close }
    }
}

/// Layout of the bottom-right search bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchBarLayout {
    pub bg: Rect,
    pub border: Rect,
}

impl SearchBarLayout {
    /// Place the bar in the top-right corner. The renderer is
    /// responsible for picking colors and drawing the label produced by
    /// [`search_bar_label`].
    #[must_use]
    pub fn compute(window_w: f32, window_h: f32, content_w: f32, scale: f32) -> SearchBarLayout {
        Self::compute_at_row(window_w, window_h, content_w, 0, scale)
    }

    #[must_use]
    pub fn compute_at_row(
        window_w: f32,
        window_h: f32,
        content_w: f32,
        row: u8,
        scale: f32,
    ) -> SearchBarLayout {
        // SIZE terms scale; window-relative POSITION terms (the
        // SEARCH_BAR_MARGIN edge offset, x, row_y) stay in window pixels.
        let s = scale.max(0.01);
        let row = row.min(3);
        let desired_w = (content_w.max(0.0) + SEARCH_BAR_PAD_LEFT * s + SEARCH_BAR_PAD_RIGHT * s)
            .clamp(SEARCH_BAR_MIN_WIDTH * s, SEARCH_BAR_WIDTH * s);
        let w = desired_w.min((window_w - SEARCH_BAR_MARGIN * 2.0).max(40.0));
        let h = (SEARCH_BAR_HEIGHT * s).min((window_h - SEARCH_BAR_MARGIN * 2.0).max(20.0));
        let x = (window_w - w - SEARCH_BAR_MARGIN).max(0.0);
        // Row stacking: the per-row advance (bar height + gap) is a SIZE term
        // and must scale with DPI — otherwise row 1 (search bar under the
        // read-only badge) overlaps the DPI-scaled badge at scale > 1. Only the
        // initial top margin stays a window-anchored offset.
        let row_y =
            SEARCH_BAR_MARGIN + f32::from(row) * (SEARCH_BAR_HEIGHT + SEARCH_BAR_MARGIN) * s;
        let y = row_y.min((window_h - h).max(0.0));
        let border = Rect { x, y, w, h };
        let bg = Rect {
            x: border.x + 1.0,
            y: border.y + 1.0,
            w: (border.w - 2.0).max(0.0),
            h: (border.h - 2.0).max(0.0),
        };
        SearchBarLayout { bg, border }
    }
}

#[must_use]
pub fn command_palette_query_label(palette: &CommandPalette, preedit: &str) -> String {
    let query = palette.query();
    let mut cursor = palette.cursor().min(query.len());
    if !query.is_char_boundary(cursor) {
        cursor = query.len();
    }
    let mut label = String::new();
    label.push_str(&query[..cursor]);
    label.push_str(preedit);
    label.push('▏');
    label.push_str(&query[cursor..]);
    label
}

#[must_use]
pub fn command_palette_query_caret_prefix(palette: &CommandPalette, preedit: &str) -> String {
    let query = palette.query();
    let mut cursor = palette.cursor().min(query.len());
    if !query.is_char_boundary(cursor) {
        cursor = query.len();
    }
    let mut prefix = String::new();
    prefix.push_str(&query[..cursor]);
    prefix.push_str(preedit);
    prefix
}

/// Produce the text label for the bottom-right search bar.
///
/// `N/M` is `current/total` (1-based) when there are matches; otherwise
/// the bar shows `0/0`. An empty query renders as `/ ` so the user sees
/// the prompt.
#[must_use]
pub fn search_bar_label(search: &SearchState, preedit: &str) -> String {
    let total = search.matches.len();
    let cur = search.current.map(|i| i + 1).unwrap_or(0);
    // The in-flight IME composition is spliced in right after the committed
    // query (before the ▏ caret), so the whole bar renders as one continuous
    // string and the ` · N/M` counter flows to the RIGHT of the composition
    // instead of being overlapped by a separately-drawn preedit. (#B14)
    format!("/ {}{}▏ · {}/{}", search.query, preedit, cur, total)
}

/// The portion of [`search_bar_label`] that precedes the caret: the `/ `
/// prompt prefix plus the raw query. Measuring this string's width gives the
/// x-offset of the inline-composition caret (the `▏` in the full label) from
/// the start of the label text — i.e. where the IME preedit and the OS
/// candidate window should anchor, just past the typed query and *before* the
/// `▏ · N/M` match-counter suffix.
///
/// Both the inline preedit overlay ([`sonicterm-gpu`]) and the OS candidate
/// area ([`sonicterm-app`]) measure this same string so they agree on the
/// caret position regardless of how long the suffix grows.
#[must_use]
pub fn search_query_caret_prefix(search: &SearchState, preedit: &str) -> String {
    format!("/ {}{}", search.query, preedit)
}

#[cfg(test)]
#[path = "overlays/tests.rs"]
mod tests;
/// Layout of the IME preedit popover, placed just below the text cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImePreeditLayout {
    /// Background rectangle behind the preedit text.
    pub bg: Rect,
    /// Underline rectangle drawn at the bottom of `bg`.
    pub underline: Rect,
}

impl ImePreeditLayout {
    /// Place the preedit popover under the cursor cell. `cursor_x` and
    /// `cursor_y` are the **top-left** of the cursor cell in physical
    /// pixels; `cell_w` and `cell_h` are the cell size. Returns `None`
    /// when there is no in-flight preedit text.
    #[must_use]
    pub fn compute(
        ime: &ImeState,
        cursor_x: f32,
        cursor_y: f32,
        cell_w: f32,
        cell_h: f32,
        window_w: f32,
        window_h: f32,
        scale: f32,
    ) -> Option<ImePreeditLayout> {
        let text = ime.preedit();
        if text.is_empty() {
            return None;
        }
        // SIZE sub-pads scale; cursor_x/cursor_y and the window clamps are
        // POSITION terms and stay in window pixels.
        let s = scale.max(0.01);
        let char_count = text.chars().count().max(1) as f32;
        let w = (cell_w * char_count + 12.0 * s).min(window_w.max(40.0));
        let h = cell_h + 6.0 * s;
        let mut x = cursor_x;
        let y = (cursor_y + cell_h).min((window_h - h).max(0.0));
        if x + w > window_w {
            x = (window_w - w).max(0.0);
        }
        let bg = Rect { x, y, w, h };
        let underline = Rect {
            x: bg.x + 2.0 * s,
            y: bg.y + bg.h - 2.0 * s,
            w: (bg.w - 4.0 * s).max(0.0),
            h: 2.0 * s,
        };
        Some(ImePreeditLayout { bg, underline })
    }
}
