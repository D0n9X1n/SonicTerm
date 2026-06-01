//! Regression tests for #425 (real cause behind #414) — when a
//! LF/IND/NEL is dispatched at the bottom of a scroll region that
//! happens to span the entire visible grid, the ejected line MUST
//! be pushed into scrollback. Pre-fix, the VT layer routed such
//! scrolls through `Grid::scroll_region_up` which never touched
//! scrollback, dropping the line on the floor and leaving stale
//! cells behind interactive TUIs like Claude Code.
//!
//! The fix lives in `Grid::scroll_region_up`: when the region
//! covers the full visible grid, it delegates to `scroll_up`,
//! which is the canonical scrollback-pushing scroll. This covers
//! BOTH the historical `ESC[r` (omitted-params) routing AND any
//! shell that explicitly sets `ESC[1;Nr` margins equal to the
//! full screen.

use sonic_grid::grid::Grid;
use sonic_types::cell::{CellFlags, Color};

fn fill_unique(g: &mut Grid) {
    let rows = g.rows;
    let cols = g.cols;
    for r in 0..rows {
        let tag = char::from(b'A' + (r as u8 % 26));
        g.goto(r, 0);
        for _ in 0..cols.min(4) {
            g.put_char(tag, Color::Default, Color::Default, CellFlags::empty());
        }
    }
}

/// Case 1: full-screen scroll region established via the historical
/// `ESC[r`-omitted-params codepath (which lowered to
/// `scroll_top=Some(0)..scroll_bottom=Some(rows-1)`). The VT layer
/// then routes LF/IND at the bottom through `scroll_region_up`. The
/// ejected top row MUST end up in scrollback.
#[test]
fn full_region_scroll_pushes_to_scrollback_zero_to_rows_minus_one() {
    let mut g = Grid::new(20, 10);
    fill_unique(&mut g);
    assert_eq!(g.scrollback_len(), 0);

    let rows = g.rows;
    g.scroll_region_up(0, rows - 1, 1);

    assert_eq!(
        g.scrollback_len(),
        1,
        "full-screen scroll_region_up MUST push to scrollback (regression for #425)"
    );
}

/// Case 2: same expectation when a shell sets explicit full-screen
/// margins. Spec-wise the region IS the full screen, so the scroll
/// is indistinguishable from a normal terminal scroll and must
/// push to scrollback.
#[test]
fn full_region_scroll_pushes_to_scrollback_explicit_full_screen() {
    let mut g = Grid::new(20, 24);
    fill_unique(&mut g);

    // Equivalent to `CSI 1;24 r` followed by LF at bottom row.
    g.scroll_region_up(0, 23, 1);

    assert_eq!(
        g.scrollback_len(),
        1,
        "explicit full-screen margins (ESC[1;Nr) must still push on bottom LF"
    );
}

/// Case 3: real middle-screen DECSTBM margins must NOT push to
/// scrollback. This is the canonical scroll-region behavior that
/// htop/vim/less rely on — the region scrolls in place; nothing
/// leaves the visible grid.
#[test]
fn middle_region_scroll_does_not_push_to_scrollback() {
    let mut g = Grid::new(20, 20);
    fill_unique(&mut g);

    // Equivalent to `CSI 5;15 r` followed by LF at row 14.
    g.scroll_region_up(4, 14, 1);

    assert_eq!(g.scrollback_len(), 0, "middle-screen DECSTBM scrolls must NOT touch scrollback");
}

/// Sanity: multi-line full-screen scroll pushes N lines.
#[test]
fn full_region_scroll_pushes_n_lines() {
    let mut g = Grid::new(20, 10);
    fill_unique(&mut g);

    g.scroll_region_up(0, 9, 3);

    assert_eq!(g.scrollback_len(), 3);
}

/// Sanity: top-only partial region (top=0, bottom < rows-1) is
/// still a real region scroll, not a full-screen scroll, so it
/// must NOT push.
#[test]
fn top_anchored_partial_region_does_not_push() {
    let mut g = Grid::new(20, 20);
    fill_unique(&mut g);

    g.scroll_region_up(0, 10, 1);

    assert_eq!(g.scrollback_len(), 0);
}
