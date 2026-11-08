//! Pins #1859's media figure reuse: a frame whose panes all keep their media revisions reuses the
//! previous figures without walking any image, and any change of revision, pane set, pane order or
//! an untracked (0) revision recomputes them, giving the same figures a fresh walk gives.

use std::sync::Arc;

use sonicterm_render_model::boundary::grid::grid::Grid;
use sonicterm_render_model::{CursorStyle, InlineImage, PaneRender, PixelRect};

use super::*;

/// A pane `id` over `grid` holding `images` read at `revision`.
fn pane(id: u64, grid: &mut Grid, images: Vec<InlineImage>, revision: u64) -> PaneRender<'_> {
    PaneRender {
        id,
        rect_px: PixelRect { x: 0, y: 0, w: 80, h: 40 },
        grid,
        viewport_top_abs: None,
        is_active: true,
        cursor_style: CursorStyle::default(),
        is_broadcast_participant: false,
        scrollbar_alpha: 0.0,
        inline_images: Arc::new(images),
        inline_media_revision: revision,
    }
}

/// An image with buffer `id` of `bytes` pixels.
fn image(id: u64, bytes: usize) -> InlineImage {
    InlineImage {
        id,
        row: 0,
        col: 0,
        scroll_mark: 0,
        width: 1,
        height: 1,
        bgra: Arc::from(vec![0u8; bytes]),
        kitty: None,
    }
}

/// The recomputations counted so far on this thread.
fn recomputes() -> usize {
    RECOMPUTES.with(std::cell::Cell::get)
}

/// Equal revisions reuse; a new revision, a changed pane set, a reorder or revision 0 recompute,
/// and every recomputation equals what the uncached walk gives.
#[test]
fn equal_revisions_reuse_and_any_change_recomputes() {
    let (mut first, mut second) = (Grid::new(8, 2), Grid::new(8, 2));
    let mut cache = MediaIdentityCache::default();
    let shared = image(1, 16);
    let start = recomputes();
    let panes = [
        pane(1, &mut first, vec![shared.clone(), shared.clone()], 10),
        pane(2, &mut second, vec![image(2, 8)], 20),
    ];
    let computed = cache.figures(&panes);
    assert_eq!(computed.0, 24, "a shared buffer is counted once");
    assert_eq!(recomputes(), start + 1);
    assert_eq!(cache.figures(&panes), computed, "equal revisions reuse");
    assert_eq!(recomputes(), start + 1, "and walk nothing");
    drop(panes);
    // A new revision on one pane recomputes, here with a changed list.
    let panes = [
        pane(1, &mut first, vec![shared.clone()], 11),
        pane(2, &mut second, vec![image(2, 8)], 20),
    ];
    assert_eq!(cache.figures(&panes).0, 24);
    assert_eq!(recomputes(), start + 2);
    drop(panes);
    // Reordered panes recompute: the hash includes pane order.
    let panes = [
        pane(2, &mut second, vec![image(2, 8)], 20),
        pane(1, &mut first, vec![shared.clone()], 11),
    ];
    cache.figures(&panes);
    assert_eq!(recomputes(), start + 3);
    drop(panes);
    // A removed pane recomputes.
    let panes = [pane(1, &mut first, vec![shared.clone()], 11)];
    assert_eq!(cache.figures(&panes).0, 16);
    assert_eq!(recomputes(), start + 4);
    // Revision 0 is untracked and always recomputes, even twice in a row.
    drop(panes);
    let panes = [pane(1, &mut first, vec![shared], 0)];
    cache.figures(&panes);
    cache.figures(&panes);
    assert_eq!(recomputes(), start + 6);
}

/// Two panes swapped in order with their own revisions kept recompute: the revisions alone are the
/// same multiset, but the hash covers pane order and ids, so only the pane-id check catches it.
#[test]
fn swapped_panes_with_their_revisions_recompute() {
    let (mut first, mut second) = (Grid::new(8, 2), Grid::new(8, 2));
    let mut cache = MediaIdentityCache::default();
    let panes = [pane(1, &mut first, vec![image(1, 4)], 30), pane(2, &mut second, vec![], 30)];
    let before = cache.figures(&panes);
    drop(panes);
    let start = recomputes();
    // The same revision under each position, but the panes are swapped.
    let panes = [pane(2, &mut second, vec![], 30), pane(1, &mut first, vec![image(1, 4)], 30)];
    let after = cache.figures(&panes);
    assert_eq!(recomputes(), start + 1, "a different pane at a position recomputes");
    assert_ne!(after.1, before.1, "the hash covers pane order");
}
