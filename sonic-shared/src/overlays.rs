//! Pure layout helpers for the three state-only overlays drawn over the
//! terminal grid:
//!
//! 1. Command palette — centered modal (~480×280) with a query input row
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
//! turns them into `QuadInstance`s and glyphon `TextArea`s. Pure logic
//! keeps the path covered by unit tests without a wgpu device.
//!
//! Coordinate system: physical pixels, origin top-left (the same system
//! [`crate::tabbar_view`] uses).

use crate::command_label::label as action_label;
use crate::command_palette::CommandPalette;
use crate::ime::ImeState;
use crate::search::SearchState;
use crate::tabbar_view::Rect;

/// Width of the command-palette modal in physical pixels.
pub const PALETTE_WIDTH: f32 = 480.0;

/// Height of the command-palette modal in physical pixels.
pub const PALETTE_HEIGHT: f32 = 280.0;

/// 1px chrome border around the modal.
pub const PALETTE_BORDER: f32 = 1.0;

/// Row height inside the list / for the query input.
pub const PALETTE_ROW_HEIGHT: f32 = 22.0;

/// Inset between the modal edge and the inner content.
pub const PALETTE_INNER_PAD: f32 = 8.0;

/// Margin between the search bar and the right/bottom window edge.
pub const SEARCH_BAR_MARGIN: f32 = 8.0;

/// Width of the small bottom-right search bar.
pub const SEARCH_BAR_WIDTH: f32 = 260.0;

/// Height of the small bottom-right search bar.
pub const SEARCH_BAR_HEIGHT: f32 = 26.0;

/// Layout of the command-palette modal.
#[derive(Debug, Clone)]
pub struct PaletteLayout {
    /// 1px border rectangle (drawn under everything else).
    pub border: Rect,
    /// Modal background rectangle, inset by [`PALETTE_BORDER`] from
    /// `border`.
    pub bg: Rect,
    /// Query-input row at the top of the modal.
    pub query_row: Rect,
    /// One rect per visible action row. May be empty when the palette
    /// hides every action (e.g. a query with no matches).
    pub rows: Vec<PaletteRow>,
    /// Index of the highlighted row inside `rows`, if any. The highlight
    /// is only emitted when the selected index actually falls inside the
    /// visible window (see scroll clamping in [`PaletteLayout::compute`]).
    pub selected_row: Option<usize>,
    /// Query string the renderer should paint into `query_row`. A
    /// trailing block cursor is appended so the user can see the caret.
    pub query_label: String,
    /// Display labels for each row in `rows`, parallel order.
    pub row_labels: Vec<String>,
    /// When the filter produced zero matches, the layout still emits a
    /// modal + query row but `rows` is empty; the renderer should paint
    /// this centered placeholder string instead of the unfiltered list.
    /// `None` whenever `rows` is non-empty.
    pub empty_label: Option<String>,
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
    ) -> Option<PaletteLayout> {
        if !palette.is_open() {
            return None;
        }
        // Clamp modal size to the window so a tiny terminal still draws
        // something legible rather than nothing.
        let modal_w = PALETTE_WIDTH.min((window_w - 16.0).max(120.0));
        let modal_h = PALETTE_HEIGHT.min((window_h - 16.0).max(80.0));
        let border_x = ((window_w - modal_w) * 0.5).max(0.0);
        let border_y = ((window_h - modal_h) * 0.35).max(0.0);
        let border = Rect { x: border_x, y: border_y, w: modal_w, h: modal_h };
        let bg = Rect {
            x: border.x + PALETTE_BORDER,
            y: border.y + PALETTE_BORDER,
            w: (border.w - PALETTE_BORDER * 2.0).max(0.0),
            h: (border.h - PALETTE_BORDER * 2.0).max(0.0),
        };
        let query_row = Rect {
            x: bg.x + PALETTE_INNER_PAD,
            y: bg.y + PALETTE_INNER_PAD,
            w: (bg.w - PALETTE_INNER_PAD * 2.0).max(0.0),
            h: PALETTE_ROW_HEIGHT,
        };

        // Action list region (everything below the query row).
        let list_top = query_row.y + query_row.h + PALETTE_INNER_PAD;
        let list_bottom = bg.y + bg.h - PALETTE_INNER_PAD;
        let avail = (list_bottom - list_top).max(0.0);
        let max_rows = (avail / PALETTE_ROW_HEIGHT).floor() as usize;

        // Publish viewport size to the state so the next key press can
        // clamp scroll_offset correctly.
        palette.set_visible_rows(max_rows);

        let visible = palette.visible();
        let total = visible.len();

        // Use the palette's own scroll_offset (kept in sync by
        // ensure_selected_in_view inside the state) so the viewport
        // tracks the selection across every input path — keys, mouse,
        // backspace, refilter.
        let window_start = palette.scroll_offset().min(total.saturating_sub(max_rows));
        let window_end = (window_start + max_rows).min(total);
        let selected = palette.selected();

        let mut rows = Vec::with_capacity(window_end.saturating_sub(window_start));
        let mut row_labels = Vec::with_capacity(rows.capacity());
        for (i, item_index) in (window_start..window_end).enumerate() {
            let r = Rect {
                x: bg.x + PALETTE_INNER_PAD,
                y: list_top + (i as f32) * PALETTE_ROW_HEIGHT,
                w: (bg.w - PALETTE_INNER_PAD * 2.0).max(0.0),
                h: PALETTE_ROW_HEIGHT,
            };
            rows.push(PaletteRow { item_index, rect: r });
            if let Some(a) = visible.get(item_index) {
                row_labels.push(action_label(a));
            } else {
                row_labels.push(String::new());
            }
        }
        let selected_row = if total > 0 && selected >= window_start && selected < window_end {
            Some(selected - window_start)
        } else {
            None
        };

        let mut query_label = String::from("> ");
        query_label.push_str(palette.query());
        // Append a block cursor so the user sees where their next
        // keystroke lands.
        query_label.push('▏');

        // Zero-matches placeholder. Shown only when the user has typed
        // something AND every action was filtered out. With an empty
        // query we still surface the full action universe.
        let empty_label = if total == 0 && !palette.query().is_empty() {
            Some(NO_MATCHES.to_string())
        } else {
            None
        };

        Some(PaletteLayout {
            border,
            bg,
            query_row,
            rows,
            selected_row,
            query_label,
            row_labels,
            empty_label,
        })
    }
}

/// Placeholder shown in the action list when the current query filters
/// every action out. Exposed for tests + so the renderer doesn't have to
/// duplicate the string.
pub const NO_MATCHES: &str = "No commands found";

/// Layout of the bottom-right search bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchBarLayout {
    pub bg: Rect,
    pub border: Rect,
}

impl SearchBarLayout {
    /// Place the bar in the bottom-right corner. The renderer is
    /// responsible for picking colors and drawing the label produced by
    /// [`search_bar_label`].
    #[must_use]
    pub fn compute(window_w: f32, window_h: f32) -> SearchBarLayout {
        let w = SEARCH_BAR_WIDTH.min((window_w - SEARCH_BAR_MARGIN * 2.0).max(40.0));
        let h = SEARCH_BAR_HEIGHT.min((window_h - SEARCH_BAR_MARGIN * 2.0).max(20.0));
        let x = (window_w - w - SEARCH_BAR_MARGIN).max(0.0);
        let y = (window_h - h - SEARCH_BAR_MARGIN).max(0.0);
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

/// Produce the text label for the bottom-right search bar.
///
/// `N/M` is `current/total` (1-based) when there are matches; otherwise
/// the bar shows `0/0`. An empty query renders as `/ ` so the user sees
/// the prompt.
#[must_use]
pub fn search_bar_label(search: &SearchState) -> String {
    let total = search.matches.len();
    let cur = search.current.map(|i| i + 1).unwrap_or(0);
    format!("/ {} — {}/{}", search.query, cur, total)
}

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
    ) -> Option<ImePreeditLayout> {
        let text = ime.preedit();
        if text.is_empty() {
            return None;
        }
        let char_count = text.chars().count().max(1) as f32;
        let w = (cell_w * char_count + 12.0).min(window_w.max(40.0));
        let h = cell_h + 6.0;
        let mut x = cursor_x;
        let y = (cursor_y + cell_h).min((window_h - h).max(0.0));
        if x + w > window_w {
            x = (window_w - w).max(0.0);
        }
        let bg = Rect { x, y, w, h };
        let underline =
            Rect { x: bg.x + 2.0, y: bg.y + bg.h - 2.0, w: (bg.w - 4.0).max(0.0), h: 2.0 };
        Some(ImePreeditLayout { bg, underline })
    }
}
