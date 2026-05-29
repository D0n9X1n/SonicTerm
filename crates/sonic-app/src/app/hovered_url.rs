//! Pure-logic helpers for the "Cmd+hover auto-detected URL" affordance.
//!
//! Behavior contract (v1.0):
//!
//! - Modifier required: macOS = Super (Cmd); Windows/Linux = Control.
//! - Underline appears only when the modifier is held AND the cursor
//!   sits on a cell whose row+col falls inside a `UrlMatch` returned
//!   by [`sonic_core::url_scan::url_at_char_col`].
//! - Underline is a 2px-thick quad at the row's baseline, spanning
//!   every char of the URL on that row. (Multi-row / wrapped URLs are
//!   out of scope for v1 — they yield `None` because `url_at_char_col`
//!   operates on a single reconstructed row string.)
//! - OSC 8 hyperlinks are NOT handled here; they already have their
//!   own hover-underline path in the renderer.
//!
//! Tests live in `crates/sonic-app/tests/cmd_hover_url_underline.rs`
//! and `…/cmd_hover_no_url_no_underline.rs`. They exercise this
//! module directly so the contract can be verified without spinning
//! up a real winit / wgpu context.

use sonic_render_model::inputs::UnderlineRect;

/// State of the "Cmd is held" modifier on the current platform.
///
/// Cross-platform abstraction so callers don't sprinkle
/// `cfg!(target_os = "...")` at every site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModifierState {
    /// True when the platform open-URL modifier is currently held
    /// (Cmd on macOS, Ctrl on Windows / Linux).
    pub open_url_modifier_held: bool,
}

/// Snapshot of a hover hit used to drive cursor-icon transitions and
/// the underline overlay. Identity-equal across frames so callers can
/// detect a real state change cheaply with `PartialEq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoveredUrl {
    /// Row in the viewport (0 = top visible row).
    pub row: u16,
    /// Inclusive start column of the URL on this row.
    pub start_col: u16,
    /// Exclusive end column of the URL on this row.
    pub end_col: u16,
    /// The matched URL string — kept so a subsequent click on the
    /// same cell while the modifier is still held can avoid re-scanning.
    pub url: String,
}

/// Pixel metrics needed to project a `HoveredUrl` into an
/// [`UnderlineRect`]. All values are in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    /// Width of one terminal cell.
    pub cell_w: f32,
    /// Height of one terminal cell.
    pub cell_h: f32,
    /// Underline thickness in pixels (spec: 2.0).
    pub underline_h: f32,
}

impl CellMetrics {
    /// Standard 2px-thick underline at the cell's baseline.
    #[must_use]
    pub fn new(cell_w: f32, cell_h: f32) -> Self {
        Self { cell_w, cell_h, underline_h: 2.0 }
    }
}

/// Compute the underline rect for the currently-hovered URL, if any.
///
/// Returns `Some(rect)` only when **both** are true:
/// - `modifier.open_url_modifier_held == true`
/// - `hovered.is_some()`
///
/// The rect is pane-relative; the renderer is responsible for
/// translating to window coordinates and clipping to the pane via
/// `clip_rect_to_pane`. Width is `(end_col - start_col) * cell_w`
/// so it spans every cell of the URL on that row.
#[must_use]
pub fn compute_hovered_url_underline(
    hovered: Option<&HoveredUrl>,
    modifier: ModifierState,
    metrics: CellMetrics,
) -> Option<UnderlineRect> {
    if !modifier.open_url_modifier_held {
        return None;
    }
    let h = hovered?;
    if h.end_col <= h.start_col {
        return None;
    }
    let x = f32::from(h.start_col) * metrics.cell_w;
    let y = f32::from(h.row) * metrics.cell_h + (metrics.cell_h - metrics.underline_h);
    let w = f32::from(h.end_col - h.start_col) * metrics.cell_w;
    Some(UnderlineRect { x, y, w, h: metrics.underline_h })
}

/// Build a `HoveredUrl` from a reconstructed row string + a column
/// index, returning `None` when the column does not fall inside any
/// detected URL. Thin wrapper over [`sonic_core::url_scan::url_at_char_col`]
/// that fills in the row coordinate and converts byte offsets to char
/// columns (the URL row reconstruction in `App::hyperlink_uri_at`
/// pushes one char per cell, so the byte→char mapping is the natural
/// `char_indices().position(...)`).
#[must_use]
pub fn hovered_from_row(row_text: &str, row: u16, col: u16) -> Option<HoveredUrl> {
    let m = sonic_core::url_scan::url_at_char_col(row_text, col as usize)?;
    // Convert byte offsets back to char columns. The grid lays one
    // char per visual cell, so chars_count(prefix) == column index.
    let start_chars = row_text.get(..m.start).map(|s| s.chars().count()).unwrap_or(0);
    let end_chars = row_text.get(..m.end).map(|s| s.chars().count()).unwrap_or(start_chars);
    // Clamp end_col to u16; row widths are well under u16::MAX in practice.
    let start_col = u16::try_from(start_chars).unwrap_or(u16::MAX);
    let end_col = u16::try_from(end_chars).unwrap_or(u16::MAX);
    if end_col <= start_col {
        return None;
    }
    Some(HoveredUrl { row, start_col, end_col, url: m.url })
}
