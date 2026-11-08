//! A tab's pane tree with a layout revision. Every change to the tree goes through a method here
//! that draws a fresh revision from one process-wide sequence, so a layout computed for one
//! revision is never reused for a different tree, a changed tree, or a clone changed apart.
//!
//! Frame collection reuses the last validated layout while the revision, the outer rectangle and
//! the active pane are unchanged and every cached leaf is still a live pane, which skips walking
//! the tree and allocating its leaf list and layout on every frame of an unchanged tab.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use sonicterm_cfg::keymap::Direction;
use sonicterm_ui::pane::{PaneId, PaneTree, Rect, SplitterId};

/// The next layout revision; only grows, so no two trees or changes share one.
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

/// A fresh revision no earlier tree or change had.
// Ordering: NEXT_REVISION Relaxed; the counter only has to hand out distinct values.
fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}

/// A validated visible layout and what it was computed for.
#[derive(Debug, Clone)]
pub(crate) struct CachedLayout {
    /// The tree revision it was computed from.
    revision: u64,
    /// The outer rectangle it laid out.
    outer: Rect,
    /// The active pane it was validated for.
    active: PaneId,
    /// Every leaf, hidden ones included, so a removed hidden pane still invalidates it.
    pub(crate) leaves: Vec<PaneId>,
    /// The visible panes' rectangles, in tree order.
    pub(crate) rects: Vec<(PaneId, Rect)>,
    /// The active pane's position in `rects`.
    pub(crate) active_pos: usize,
}

/// A tab's pane tree, readable as a [`PaneTree`]; changed only through the methods here. It
/// dereferences to `&PaneTree` and never to `&mut PaneTree`, so the compiler refuses any change
/// that would skip the revision; replacing the whole tree goes through `From`, which draws one.
#[derive(Debug)]
pub struct TabTree {
    /// The tree.
    tree: PaneTree,
    /// The revision of the tree's current shape.
    revision: u64,
    /// The last validated layout; only the event loop reads or fills it.
    cache: RefCell<Option<CachedLayout>>,
    /// The last plain layout and the revision and outer rectangle it was computed for, which the
    /// pointer, splitter and resize paths read instead of walking the tree on every event.
    rects: RefCell<Option<(u64, Rect, Vec<(PaneId, Rect)>)>>,
}

impl Clone for TabTree {
    /// A clone keeps the revision: the trees are equal until either changes, which draws a fresh
    /// one. Its cache starts empty.
    fn clone(&self) -> Self {
        Self {
            tree: self.tree.clone(),
            revision: self.revision,
            cache: RefCell::new(None),
            rects: RefCell::new(None),
        }
    }
}

impl std::ops::Deref for TabTree {
    type Target = PaneTree;

    /// Every read of the tree: leaves, layout, zoom, containment.
    fn deref(&self) -> &PaneTree {
        &self.tree
    }
}

impl From<PaneTree> for TabTree {
    /// A tree with a fresh revision.
    fn from(tree: PaneTree) -> Self {
        Self {
            tree,
            revision: next_revision(),
            cache: RefCell::new(None),
            rects: RefCell::new(None),
        }
    }
}

impl TabTree {
    /// The revision of the tree's current shape, for tests that check a change drew a fresh one.
    #[cfg(test)]
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Run `change` on the tree; a change that reports success draws a fresh revision.
    fn change(&mut self, change: impl FnOnce(&mut PaneTree) -> bool) -> bool {
        let changed = change(&mut self.tree);
        if changed {
            // Every layout computed for the old shape is stale now.
            self.revision = next_revision();
        }
        changed
    }

    /// [`PaneTree::split`].
    pub fn split(&mut self, focus: PaneId, dir: Direction, new_id: PaneId) -> bool {
        self.change(|tree| tree.split(focus, dir, new_id))
    }

    /// [`PaneTree::close`].
    pub fn close(&mut self, id: PaneId) -> bool {
        self.change(|tree| tree.close(id))
    }

    /// [`PaneTree::toggle_zoom`].
    pub fn toggle_zoom(&mut self, active_pane: PaneId) -> bool {
        self.change(|tree| tree.toggle_zoom(active_pane))
    }

    /// [`PaneTree::resize_split`].
    pub fn resize_split(&mut self, active_pane: PaneId, dir: Direction, delta: f32) -> bool {
        self.change(|tree| tree.resize_split(active_pane, dir, delta))
    }

    /// [`PaneTree::resize_splitter_by_delta`].
    pub fn resize_splitter_by_delta(
        &mut self,
        id: &SplitterId,
        outer: Rect,
        delta_x: f32,
        delta_y: f32,
    ) -> bool {
        self.change(|tree| tree.resize_splitter_by_delta(id, outer, delta_x, delta_y))
    }

    /// A copy of [`Self::visit_layout`]'s layout, for tests that keep it.
    #[cfg(test)]
    pub(crate) fn layout_for<E>(
        &self,
        outer: Rect,
        active: PaneId,
        exists: impl Fn(PaneId) -> bool,
        compute: impl FnOnce(&PaneTree) -> Result<CachedLayout, E>,
    ) -> Result<CachedLayout, E> {
        self.visit_layout(outer, active, exists, compute, CachedLayout::clone)
    }

    /// Lend `visit` the cached layout for `outer` and `active` when it was computed from this
    /// revision and every cached leaf still `exists`; otherwise `compute` runs, its result, when
    /// valid, is stored, and `visit` reads that. A hit copies and allocates nothing.
    pub(crate) fn visit_layout<E, Value>(
        &self,
        outer: Rect,
        active: PaneId,
        exists: impl Fn(PaneId) -> bool,
        compute: impl FnOnce(&PaneTree) -> Result<CachedLayout, E>,
        visit: impl FnOnce(&CachedLayout) -> Value,
    ) -> Result<Value, E> {
        let current = self.cache.borrow().as_ref().is_some_and(|cached| {
            cached.revision == self.revision
                && cached.outer == outer
                && cached.active == active
                && cached.leaves.iter().all(|id| exists(*id))
        });
        if current {
            // A current cache matches the tree, geometry and live panes exactly.
            #[cfg(test)]
            HITS.with(|hits| hits.set(hits.get() + 1));
            // A debug build recomputes every hit and requires the same layout, so a change that
            // skipped the revision fails at once instead of drawing a stale frame.
            #[cfg(debug_assertions)]
            if let Ok(fresh) = compute(&self.tree) {
                let cache = self.cache.borrow();
                let cached = cache.as_ref().expect("a current layout is cached");
                debug_assert_eq!(
                    (&cached.rects, cached.active_pos),
                    (&fresh.rects, fresh.active_pos),
                    "a cached layout differs from a fresh one"
                );
            }
        } else {
            // When: nothing current is cached, the layout is computed and stored for later frames.
            let fresh = compute(&self.tree)?;
            *self.cache.borrow_mut() =
                Some(CachedLayout { revision: self.revision, outer, active, ..fresh });
        }
        let cache = self.cache.borrow();
        // The cache was filled or found current above, and nothing between changes it.
        let cached = cache.as_ref().expect("the layout was cached above");
        Ok(visit(cached))
    }

    /// Lend `visit` every visible pane's rectangle in `outer`, as [`PaneTree::layout`] gives them,
    /// computed once per revision and outer rectangle; a repeat lends the stored list.
    pub(crate) fn with_layout<Value>(
        &self,
        outer: Rect,
        visit: impl FnOnce(&[(PaneId, Rect)]) -> Value,
    ) -> Value {
        let current = self.rects.borrow().as_ref().is_some_and(|(revision, cached_outer, _)| {
            *revision == self.revision && *cached_outer == outer
        });
        if current {
            // A current list matches the tree's revision and the outer rectangle.
            #[cfg(test)]
            RECT_HITS.with(|hits| hits.set(hits.get() + 1));
        } else {
            // When: nothing current is stored, the tree is walked once for this shape.
            *self.rects.borrow_mut() = Some((self.revision, outer, self.tree.layout(outer)));
        }
        let rects = self.rects.borrow();
        // The list was filled or found current above, and nothing between changes it.
        let (_, _, rects) = rects.as_ref().expect("the rects were stored above");
        visit(rects)
    }

    /// [`Self::with_layout`]'s rectangles as an owned list, for a caller that keeps them.
    pub(crate) fn layout(&self, outer: Rect) -> Vec<(PaneId, Rect)> {
        self.with_layout(outer, <[(PaneId, Rect)]>::to_vec)
    }

    /// The tree itself, for code that hands it on as a [`PaneTree`].
    pub(crate) fn tree(&self) -> &PaneTree {
        &self.tree
    }
}

impl CachedLayout {
    /// A layout to store, computed from `leaves`, `rects` and the active position.
    pub(crate) fn new(leaves: Vec<PaneId>, rects: Vec<(PaneId, Rect)>, active_pos: usize) -> Self {
        Self {
            revision: 0,
            outer: Rect::new(0.0, 0.0, 0.0, 0.0),
            active: 0,
            leaves,
            rects,
            active_pos,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Cache hits on this thread, so a test can tell a hit from a recomputation.
    pub(crate) static HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Plain-layout hits on this thread, the same for [`TabTree::with_layout`].
    pub(crate) static RECT_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[path = "tab_tree_tests.rs"]
mod tab_tree_tests;
