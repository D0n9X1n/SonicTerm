//! Pins the tab tree's layout revision and cache: every successful change draws a fresh revision,
//! a refused one does not; a clone keeps its revision until either side changes; the cache is hit
//! only for the same revision, outer rectangle and active pane with every leaf, hidden ones
//! included, still live; and a cached layout always equals a fresh one.

use sonicterm_cfg::keymap::Direction;
use sonicterm_ui::pane::{PaneTree, Rect};

use super::*;

/// A fresh layout of `tree` for `outer` and `active`, every leaf treated as live.
fn fresh(tree: &PaneTree, outer: Rect, active: PaneId) -> Result<CachedLayout, ()> {
    let rects = tree.layout(outer);
    let active_pos = rects.iter().position(|(id, _)| *id == active).ok_or(())?;
    Ok(CachedLayout::new(tree.leaves(), rects, active_pos))
}

/// The cache hits recorded on this thread so far.
fn hits() -> usize {
    HITS.with(std::cell::Cell::get)
}

const OUTER: Rect = Rect { x: 0.0, y: 0.0, w: 100.0, h: 40.0 };

/// Split, close, zoom and both resizes draw a fresh revision when they change the tree; a refused
/// change keeps it.
#[test]
fn each_change_draws_a_fresh_revision() {
    let mut tree = TabTree::from(PaneTree::leaf(1));
    let mut seen = vec![tree.revision()];
    assert!(tree.split(1, Direction::Right, 2));
    seen.push(tree.revision());
    assert!(tree.resize_split(1, Direction::Right, 0.1));
    seen.push(tree.revision());
    // The vertical divider sits at the split ratio's x; a hit there names it.
    let seam = tree.splitter_rects(OUTER, 1.0)[0].rect;
    let hit =
        tree.hit_splitter(OUTER, 1.0, seam.x + seam.w / 2.0, OUTER.h / 2.0).expect("on the seam");
    let splitter = hit.id;
    assert!(tree.resize_splitter_by_delta(&splitter, OUTER, 5.0, 0.0));
    seen.push(tree.revision());
    assert!(tree.toggle_zoom(2));
    seen.push(tree.revision());
    assert!(tree.toggle_zoom(2));
    seen.push(tree.revision());
    assert!(tree.close(2));
    seen.push(tree.revision());
    let distinct: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(distinct.len(), seen.len(), "{seen:?}");
    let before = tree.revision();
    assert!(!tree.close(9), "no such pane");
    assert_eq!(tree.revision(), before, "a refused change keeps the revision");
}

/// An unchanged tree hits the cache; a new outer rectangle, a new active pane, or a change misses,
/// and every hit equals a fresh layout.
#[test]
fn the_cache_hits_only_an_unchanged_layout() {
    let mut tree = TabTree::from(PaneTree::leaf(1));
    assert!(tree.split(1, Direction::Right, 2));
    let live = |_| true;
    let start = hits();
    let first = tree.layout_for(OUTER, 1, live, |tree| fresh(tree, OUTER, 1)).unwrap();
    assert_eq!(hits(), start, "the first layout is computed");
    let again = tree.layout_for(OUTER, 1, live, |_| Err(())).expect("a hit never computes");
    assert_eq!(hits(), start + 1);
    assert_eq!(again.rects, first.rects);
    let wider = Rect { w: 120.0, ..OUTER };
    tree.layout_for(wider, 1, live, |tree| fresh(tree, wider, 1)).unwrap();
    assert_eq!(hits(), start + 1, "a new outer rectangle misses");
    tree.layout_for(wider, 2, live, |tree| fresh(tree, wider, 2)).unwrap();
    assert_eq!(hits(), start + 1, "a new active pane misses");
    assert!(tree.resize_split(1, Direction::Right, 0.1));
    let resized = tree.layout_for(wider, 2, live, |tree| fresh(tree, wider, 2)).unwrap();
    assert_eq!(hits(), start + 1, "a change misses");
    assert_eq!(resized.rects, fresh(&tree, wider, 2).unwrap().rects);
}

/// While zoomed only the active pane is visible, yet a hidden leaf that is no longer live still
/// invalidates the cache.
#[test]
fn a_removed_hidden_leaf_invalidates_while_zoomed() {
    let mut tree = TabTree::from(PaneTree::leaf(1));
    assert!(tree.split(1, Direction::Right, 2));
    assert!(tree.toggle_zoom(1));
    tree.layout_for(OUTER, 1, |_| true, |tree| fresh(tree, OUTER, 1)).unwrap();
    let start = hits();
    let recomputed = std::cell::Cell::new(false);
    let _ = tree.layout_for(
        OUTER,
        1,
        |id| id != 2,
        |tree| {
            recomputed.set(true);
            fresh(tree, OUTER, 1)
        },
    );
    assert_eq!(hits(), start, "the hidden leaf's removal was noticed");
    assert!(recomputed.get());
}

/// A clone shares its revision only until one side changes: an independently changed clone, or a
/// replacement tree, never reuses the other's layout.
#[test]
fn clones_and_replacements_never_share_a_stale_layout() {
    let mut original = TabTree::from(PaneTree::leaf(1));
    assert!(original.split(1, Direction::Right, 2));
    let mut copy = original.clone();
    assert_eq!(copy.revision(), original.revision());
    assert!(copy.resize_split(1, Direction::Right, 0.2));
    assert_ne!(copy.revision(), original.revision());
    let replacement = TabTree::from(original.tree().clone());
    assert_ne!(replacement.revision(), original.revision(), "a replacement draws its own revision");
    original.layout_for(OUTER, 1, |_| true, |tree| fresh(tree, OUTER, 1)).unwrap();
    let start = hits();
    let rects = copy.layout_for(OUTER, 1, |_| true, |tree| fresh(tree, OUTER, 1)).unwrap().rects;
    assert_eq!(hits(), start, "the changed clone computes its own layout");
    assert_eq!(rects, fresh(copy.tree(), OUTER, 1).unwrap().rects);
    assert_ne!(rects, original.layout_for(OUTER, 1, |_| true, |_| Err(())).unwrap().rects);
}

/// Over random splits, closes, zooms and resizes, every cached layout equals a fresh one.
#[test]
fn a_cached_layout_always_equals_a_fresh_one() {
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = |bound: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    let mut tree = TabTree::from(PaneTree::leaf(1));
    let mut next_id = 2;
    for _ in 0..500 {
        let leaves = tree.leaves();
        let pick = leaves[next(leaves.len() as u64) as usize];
        let directions = [Direction::Left, Direction::Right, Direction::Up, Direction::Down];
        let direction = directions[next(4) as usize];
        match next(5) {
            0 => {
                tree.split(pick, direction, next_id);
                next_id += 1;
            }
            1 if leaves.len() > 1 => {
                tree.close(pick);
            }
            2 => {
                tree.toggle_zoom(pick);
            }
            _ => {
                tree.resize_split(pick, direction, 0.05);
            }
        }
        let active = tree.zoomed_pane_id().unwrap_or(tree.leaves()[0]);
        let outer = Rect { w: 80.0 + next(40) as f32, ..OUTER };
        for _ in 0..2 {
            let cached =
                tree.layout_for(outer, active, |_| true, |tree| fresh(tree, outer, active));
            assert_eq!(cached.unwrap().rects, fresh(&tree, outer, active).unwrap().rects);
        }
    }
}

/// A hit lends the stored layout itself: the rectangles a second visit reads are the same
/// allocation the first stored, so a frame of an unchanged tab copies and allocates nothing.
#[test]
fn a_hit_lends_the_stored_layout_without_a_copy() {
    let mut tree = TabTree::from(PaneTree::leaf(1));
    assert!(tree.split(1, Direction::Right, 2));
    let compute = |tree: &PaneTree| fresh(tree, OUTER, 1);
    tree.visit_layout(OUTER, 1, |_| true, compute, |_| ()).unwrap();
    let start = hits();
    // While the visit runs, the lent rectangles and the stored ones are both alive, so a copy
    // would sit at another address.
    let same = tree
        .visit_layout(
            OUTER,
            1,
            |_| true,
            |_| Err(()),
            |layout| {
                let stored = tree.cache.borrow().as_ref().map(|cached| cached.rects.as_ptr());
                stored == Some(layout.rects.as_ptr())
            },
        )
        .unwrap();
    assert_eq!(hits(), start + 1);
    assert!(same, "the hit lent the stored rectangles, not a copy");
}

/// The pointer, splitter and resize paths read `layout` through the tree's own cache: a repeat for
/// the same revision and outer rectangle lends the stored list, and a change or a new rectangle
/// recomputes it, always equal to the tree's fresh layout.
#[test]
fn the_plain_layout_is_computed_once_per_shape() {
    let rect_hits = || RECT_HITS.with(std::cell::Cell::get);
    let mut tree = TabTree::from(PaneTree::leaf(1));
    assert!(tree.split(1, Direction::Down, 2));
    let start = rect_hits();
    let first = tree.with_layout(OUTER, |rects| rects.as_ptr());
    assert_eq!(rect_hits(), start, "the first layout is computed");
    let again = tree.with_layout(OUTER, |rects| rects.as_ptr());
    assert_eq!((rect_hits(), again), (start + 1, first), "a repeat lends the stored list");
    assert_eq!(tree.layout(OUTER), tree.tree().layout(OUTER));
    let wider = Rect { w: 140.0, ..OUTER };
    assert_eq!(tree.layout(wider), tree.tree().layout(wider));
    assert!(tree.resize_split(1, Direction::Down, 0.1));
    let hits_before = rect_hits();
    assert_eq!(tree.layout(wider), tree.tree().layout(wider));
    assert_eq!(rect_hits(), hits_before, "a change recomputes");
}

/// A tree moved to another window keeps its revision but lays out a different area there, so the
/// first read in the new window recomputes, and a pointer read after a split, before any frame,
/// already sees the split.
#[test]
fn a_moved_tree_and_a_pointer_before_redraw_read_the_current_shape() {
    let mut tree = TabTree::from(PaneTree::leaf(1));
    assert_eq!(tree.layout(OUTER), tree.tree().layout(OUTER));
    let moved = std::mem::replace(&mut tree, TabTree::from(PaneTree::leaf(9)));
    let other_window = Rect { x: 10.0, w: 60.0, ..OUTER };
    assert_eq!(moved.layout(other_window), moved.tree().layout(other_window));
    let mut moved = moved;
    assert!(moved.split(1, Direction::Right, 2));
    // No frame ran since the split; the pointer path still reads two panes.
    assert_eq!(moved.layout(other_window).len(), 2);
}

/// A debug build checks every hit against a fresh layout, so a hit whose cached layout no longer
/// matches the tree panics instead of drawing a stale frame.
#[test]
#[should_panic(expected = "a cached layout differs from a fresh one")]
fn a_debug_hit_refuses_a_stale_layout() {
    let tree = TabTree::from(PaneTree::leaf(1));
    tree.layout_for(OUTER, 1, |_| true, |tree| fresh(tree, OUTER, 1)).unwrap();
    // A compute that disagrees with what was cached stands in for a change that kept the revision.
    let wrong = |_: &PaneTree| {
        Ok::<_, ()>(CachedLayout::new(vec![1], vec![(1, Rect { w: 1.0, ..OUTER })], 0))
    };
    let _ = tree.layout_for(OUTER, 1, |_| true, wrong);
}
