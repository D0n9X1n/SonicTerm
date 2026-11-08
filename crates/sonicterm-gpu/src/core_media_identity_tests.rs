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

/// The figures a fresh walk gives, the oracle the cache must always equal.
fn uncached(panes: &[PaneRender<'_>]) -> (usize, u64) {
    let mut seen = std::collections::HashSet::new();
    let bytes = panes
        .iter()
        .flat_map(|pane| pane.inline_images.iter())
        .filter(|image| seen.insert(image.id))
        .map(|image| image.bgra.len())
        .sum();
    let hash = crate::core::kitty_placeholders::inline_media_hash(
        panes.iter().map(|pane| (pane.id, pane.inline_images.as_slice())),
    );
    (bytes, hash)
}

/// A seeded model of stores and frames: each step mutates some panes' stores (drawing a new
/// revision, as a store write does), reorders, adds or removes panes, or repeats a frame (a failed
/// presentation plans the same panes again). Every frame's cached figures equal a fresh walk, and a
/// frame whose `(id, revision)` list is unchanged walks nothing.
#[test]
fn cached_figures_always_equal_a_fresh_walk() {
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut below = |bound: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % bound
    };
    let mut grids: Vec<Grid> = (0..6).map(|_| Grid::new(4, 2)).collect();
    // Each pane's store: its images and its revision, drawn from one counter like the store's.
    let mut stores: Vec<(Vec<InlineImage>, u64)> = (0..6).map(|_| (Vec::new(), 0)).collect();
    let mut next_revision = 1;
    let mut order: Vec<usize> = vec![0, 1, 2];
    let mut cache = MediaIdentityCache::default();
    let mut previous: Option<Vec<(u64, u64)>> = None;
    for step in 0..2_000 {
        match below(5) {
            0 => {
                // A store write: a new image, sometimes sharing another pane's buffer id.
                let pane = order[below(order.len() as u64) as usize];
                let id = if below(3) == 0 { below(4) } else { 100 + step };
                stores[pane].0.push(image(id, 4 * (1 + below(4) as usize)));
                stores[pane].1 = next_revision;
                next_revision += 1;
            }
            1 if !order.is_empty() => {
                // A trim or delete: the oldest image goes.
                let pane = order[below(order.len() as u64) as usize];
                if !stores[pane].0.is_empty() {
                    stores[pane].0.remove(0);
                    stores[pane].1 = next_revision;
                    next_revision += 1;
                }
            }
            2 if order.len() > 1 => {
                let last = order.len() - 1;
                order.swap(0, last);
            }
            3 => {
                // A pane opens or closes.
                if order.len() < 6 && below(2) == 0 {
                    let free = (0..6).find(|pane| !order.contains(pane)).unwrap();
                    order.push(free);
                } else if order.len() > 1 {
                    order.remove(below(order.len() as u64) as usize);
                }
            }
            _ => {}
        }
        // A pane never written still draws a real revision, as a new store does.
        for pane in &order {
            if stores[*pane].1 == 0 {
                stores[*pane].1 = next_revision;
                next_revision += 1;
            }
        }
        let start = recomputes();
        let mut grid_refs: Vec<Option<&mut Grid>> = grids.iter_mut().map(Some).collect();
        let panes: Vec<PaneRender<'_>> = order
            .iter()
            .map(|slot| {
                let grid = grid_refs[*slot].take().unwrap();
                let (images, revision) = &stores[*slot];
                pane(*slot as u64, grid, images.clone(), *revision)
            })
            .collect();
        let keys: Vec<(u64, u64)> =
            panes.iter().map(|pane| (pane.id, pane.inline_media_revision)).collect();
        assert_eq!(cache.figures(&panes), uncached(&panes), "step {step}");
        let walked = recomputes() - start;
        assert_eq!(walked, usize::from(previous.as_ref() != Some(&keys)), "step {step}: reuse");
        previous = Some(keys);
    }
}

/// A window with more panes than the list keeps recomputes every frame and keeps no list, so the
/// cache never reserves more than its cap; a trim drops the list.
#[test]
fn the_revision_list_stays_within_its_cap() {
    let mut grids: Vec<Grid> = (0..=MAX_TRACKED_PANES).map(|_| Grid::new(4, 2)).collect();
    let mut cache = MediaIdentityCache::default();
    let mut panes: Vec<PaneRender<'_>> = grids
        .iter_mut()
        .enumerate()
        .map(|(index, grid)| pane(index as u64, grid, vec![image(index as u64, 4)], 7))
        .collect();
    let start = recomputes();
    cache.figures(&panes);
    cache.figures(&panes);
    assert_eq!(recomputes(), start + 2, "over the cap, every frame recomputes");
    assert_eq!(cache.retained_bytes(), 0, "and keeps no list");
    panes.pop();
    cache.figures(&panes);
    assert!(cache.retained_bytes() <= crate::core::frame_scratch::MEDIA_REVISIONS_CAP_BYTES);
    assert!(cache.retained_bytes() > 0);
    cache.release();
    assert_eq!(cache.retained_bytes(), 0, "a trim drops the list");
    let start = recomputes();
    cache.figures(&panes);
    assert_eq!(recomputes(), start + 1, "and the next frame recomputes");
}

/// A list already allocated for fewer panes grows to exactly the panes it holds, so growing one
/// pane at a time from 40 up to the 64-pane limit never reserves more than the cap; doubling growth
/// would reserve 80 pairs (1,280 bytes) from 41 panes on.
#[test]
fn growing_from_an_allocated_list_stays_within_the_cap() {
    let mut grids: Vec<Grid> = (0..MAX_TRACKED_PANES).map(|_| Grid::new(4, 2)).collect();
    let mut cache = MediaIdentityCache::default();
    let mut grid_iter = grids.iter_mut();
    let mut panes: Vec<PaneRender<'_>> = Vec::new();
    for count in 1..=MAX_TRACKED_PANES {
        let grid = grid_iter.next().unwrap();
        panes.push(pane(count as u64, grid, vec![], count as u64));
        if count < 40 {
            // When: count is below 40, the list is still being filled to the starting size.
            continue;
        }
        cache.figures(&panes);
        assert_eq!(
            cache.retained_bytes(),
            count * std::mem::size_of::<(u64, u64)>(),
            "{count} panes reserve exactly {count} pairs"
        );
        assert!(cache.retained_bytes() <= crate::core::frame_scratch::MEDIA_REVISIONS_CAP_BYTES);
    }
}
