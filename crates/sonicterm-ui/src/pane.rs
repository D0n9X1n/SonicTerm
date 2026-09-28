//! Pane tree — recursive horizontal/vertical splits inside a tab.

use sonicterm_cfg::keymap::Direction;

pub type PaneId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    Horizontal, // children stacked top↔bottom
    Vertical,   // children stacked left↔right
}

#[derive(Debug, Clone)]
pub enum PaneTree {
    Leaf {
        id: PaneId,
        zoomed_pane_id: Option<PaneId>,
    },
    Split {
        axis: SplitAxis,
        ratio: f32, // 0..1, share for the first child
        first: Box<PaneTree>,
        second: Box<PaneTree>,
        zoomed_pane_id: Option<PaneId>,
    },
}

/// A rectangle in arbitrary units. Used by `PaneTree::layout` and the
/// renderer to position each leaf inside the window.
#[derive(Debug, Clone, Copy, PartialEq)]
// Named by callers outside this crate.
#[allow(clippy::min_ident_chars)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Visual splitter seam between two adjacent panes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplitterRect {
    pub axis: SplitAxis,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SplitterId(Vec<bool>);

#[derive(Debug, Clone, PartialEq)]
pub struct SplitterHit {
    pub id: SplitterId,
    pub axis: SplitAxis,
    pub rect: Rect,
}

impl Rect {
    /// Build a rectangle from its top-left origin and size.
    pub fn new(left: f32, top: f32, width: f32, height: f32) -> Self {
        Self { x: left, y: top, w: width, h: height }
    }
    /// Return the midpoint, which `focus_neighbor` uses to rank spatial neighbours.
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
    /// Report whether a point lies inside, treating right and bottom edges as outside
    /// so abutting panes never both claim the same pixel.
    pub fn contains(&self, point_x: f32, point_y: f32) -> bool {
        point_x >= self.x
            && point_x < self.x + self.w
            && point_y >= self.y
            && point_y < self.y + self.h
    }
}

fn coalesce_splitter_rects(mut splitters: Vec<SplitterRect>) -> Vec<SplitterRect> {
    let eps = 0.01_f32;
    let mut changed = true;
    while changed {
        changed = false;
        'outer: for first_index in 0..splitters.len() {
            for second_index in (first_index + 1)..splitters.len() {
                if let Some(merged) =
                    merge_splitter(splitters[first_index], splitters[second_index], eps)
                {
                    // When: `merge_splitter` returned `Some`, restart the scan because removal shifts indices.
                    splitters[first_index] = merged;
                    splitters.remove(second_index);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    splitters
}

fn merge_splitter(first: SplitterRect, second: SplitterRect, eps: f32) -> Option<SplitterRect> {
    if first.axis != second.axis {
        // When: `first.axis` differs from `second.axis`, the seams cross rather than continue one line.
        return None;
    }

    match first.axis {
        SplitAxis::Vertical => {
            // When: `first.axis` is Vertical, the seams may only join along a shared x and width.
            if (first.rect.x - second.rect.x).abs() > eps
                || (first.rect.w - second.rect.w).abs() > eps
            {
                // When: `first.rect.x` or `first.rect.w` differs beyond `eps`, the seams sit on separate columns.
                return None;
            }
            let top = first.rect.y.min(second.rect.y);
            let bottom = (first.rect.y + first.rect.h).max(second.rect.y + second.rect.h);
            let combined_h = first.rect.h + second.rect.h;
            if bottom - top - combined_h > eps {
                // When: the span exceeds `combined_h`, a gap separates the seams, so they are not one run.
                return None;
            }
            Some(SplitterRect {
                axis: first.axis,
                rect: Rect::new(first.rect.x, top, first.rect.w, bottom - top),
            })
        }
        SplitAxis::Horizontal => {
            // When: `first.axis` is Horizontal, the seams may only join along a shared y and height.
            if (first.rect.y - second.rect.y).abs() > eps
                || (first.rect.h - second.rect.h).abs() > eps
            {
                // When: `first.rect.y` or `first.rect.h` differs beyond `eps`, the seams sit on separate rows.
                return None;
            }
            let left = first.rect.x.min(second.rect.x);
            let right = (first.rect.x + first.rect.w).max(second.rect.x + second.rect.w);
            let combined_w = first.rect.w + second.rect.w;
            if right - left - combined_w > eps {
                // When: the span exceeds `combined_w`, a gap separates the seams, so they are not one run.
                return None;
            }
            Some(SplitterRect {
                axis: first.axis,
                rect: Rect::new(left, first.rect.y, right - left, first.rect.h),
            })
        }
    }
}

impl PaneTree {
    /// Build a single-pane tree that starts unzoomed.
    pub fn leaf(id: PaneId) -> Self {
        PaneTree::Leaf { id, zoomed_pane_id: None }
    }

    /// Return the pane currently zoomed to fill the tab; successful splitting clears zoom.
    pub fn zoomed_pane_id(&self) -> Option<PaneId> {
        match self {
            PaneTree::Leaf { zoomed_pane_id, .. } | PaneTree::Split { zoomed_pane_id, .. } => {
                *zoomed_pane_id
            }
        }
    }

    fn set_zoomed_pane_id(&mut self, next: Option<PaneId>) {
        match self {
            PaneTree::Leaf { zoomed_pane_id, .. } | PaneTree::Split { zoomed_pane_id, .. } => {
                *zoomed_pane_id = next;
            }
        }
    }

    /// Zoom `active_pane` to fill the tab, or unzoom when it is already zoomed.
    ///
    /// Returns whether the zoom state changed.
    pub fn toggle_zoom(&mut self, active_pane: PaneId) -> bool {
        if self.zoomed_pane_id() == Some(active_pane) {
            // When: `zoomed_pane_id` already names `active_pane`, the repeat toggle restores the split view.
            self.set_zoomed_pane_id(None);
            return true;
        }

        if self.contains_leaf(active_pane) {
            self.set_zoomed_pane_id(Some(active_pane));
            true
        } else {
            // When: `contains_leaf` is false, `active_pane` lives in another tab, so nothing zooms.
            false
        }
    }

    fn contains_leaf(&self, needle: PaneId) -> bool {
        match self {
            PaneTree::Leaf { id, .. } => *id == needle,
            PaneTree::Split { first, second, .. } => {
                first.contains_leaf(needle) || second.contains_leaf(needle)
            }
        }
    }

    /// Split the focused leaf and exit zoom on success; return false without mutation for missing focus.
    pub fn split(&mut self, focus: PaneId, dir: Direction, new_id: PaneId) -> bool {
        let axis = match dir {
            Direction::Left | Direction::Right => SplitAxis::Vertical,
            Direction::Up | Direction::Down => SplitAxis::Horizontal,
        };
        let put_new_first = matches!(dir, Direction::Left | Direction::Up);
        let split = self.split_recursive(focus, axis, put_new_first, new_id);
        if split {
            // Clear root zoom so layout includes the newly focused pane after a successful split.
            self.set_zoomed_pane_id(None);
        }
        split
    }

    fn split_recursive(
        &mut self,
        focus: PaneId,
        axis: SplitAxis,
        new_first: bool,
        new_id: PaneId,
    ) -> bool {
        match self {
            PaneTree::Leaf { id, .. } if *id == focus => {
                let existing = PaneTree::leaf(*id);
                let new_leaf = PaneTree::leaf(new_id);
                let (first, second) = if new_first {
                    (new_leaf, existing)
                } else {
                    // When: `new_first` is false, the existing pane keeps the leading position.
                    (existing, new_leaf)
                };
                *self = PaneTree::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(second),
                    zoomed_pane_id: None,
                };
                true
            }
            PaneTree::Leaf { .. } => false,
            PaneTree::Split { first, second, .. } => {
                first.split_recursive(focus, axis, new_first, new_id)
                    || second.split_recursive(focus, axis, new_first, new_id)
            }
        }
    }

    /// Resize the split divider that directly owns `active_pane`.
    ///
    /// Vertical splits respond to left/right directions; horizontal splits
    /// respond to up/down directions. The divider ratio is clamped to keep both
    /// children visible.
    pub fn resize_split(
        &mut self,
        active_pane: PaneId,
        dir: Direction,
        delta_fraction: f32,
    ) -> bool {
        let delta = match dir {
            Direction::Left | Direction::Up => -delta_fraction,
            Direction::Right | Direction::Down => delta_fraction,
        };
        self.resize_split_recursive(active_pane, dir, delta)
    }

    fn resize_split_recursive(&mut self, active_pane: PaneId, dir: Direction, delta: f32) -> bool {
        match self {
            PaneTree::Leaf { .. } => false,
            PaneTree::Split { axis, ratio, first, second, .. } => {
                // When: `self` is a `Split`, only the divider directly above `active_pane` may move.
                let directly_owns_active = matches!(first.as_ref(), PaneTree::Leaf { id, .. } if *id == active_pane)
                    || matches!(second.as_ref(), PaneTree::Leaf { id, .. } if *id == active_pane);
                if directly_owns_active {
                    // When: `directly_owns_active` is true, this node owns the divider the user is resizing.
                    let axis_matches = matches!(
                        (*axis, dir),
                        (SplitAxis::Vertical, Direction::Left | Direction::Right)
                            | (SplitAxis::Horizontal, Direction::Up | Direction::Down)
                    );
                    if axis_matches {
                        // When: `axis_matches` is true, the drag direction moves this divider's ratio.
                        *ratio = (*ratio + delta).clamp(0.1, 0.9);
                        return true;
                    }
                    return false;
                }

                first.resize_split_recursive(active_pane, dir, delta)
                    || second.resize_split_recursive(active_pane, dir, delta)
            }
        }
    }

    /// Collect leaf ids in left-to-right, top-to-bottom order.
    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            PaneTree::Leaf { id, .. } => out.push(*id),
            PaneTree::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    /// Remove the leaf with `id`. If a Split ends up with one child, it
    /// collapses to that child. Returns true if anything was removed.
    pub fn close(&mut self, id: PaneId) -> bool {
        if let PaneTree::Leaf { id: leaf, .. } = self {
            // When: `self` is a `Leaf`, a root pane has no parent split to collapse into.
            return *leaf == id;
        }
        let zoomed = self.zoomed_pane_id().filter(|zoomed| *zoomed != id);
        let mut surviving: Option<PaneTree> = None;
        if let PaneTree::Split { first, second, .. } = self {
            // When: `self` is a `Split`, either child may be the target or contain it deeper.
            let first_is =
                matches!(first.as_ref(), PaneTree::Leaf { id: leaf_id, .. } if *leaf_id == id);
            let second_is =
                matches!(second.as_ref(), PaneTree::Leaf { id: leaf_id, .. } if *leaf_id == id);
            if first_is {
                surviving = Some(std::mem::replace(second.as_mut(), PaneTree::leaf(0)));
            } else if second_is {
                // When: `second_is` is true, the first child survives and replaces this split.
                surviving = Some(std::mem::replace(first.as_mut(), PaneTree::leaf(0)));
            } else if first.close(id) || second.close(id) {
                // When: `first.close` or `second.close` succeeded, a deeper split already collapsed.
                self.set_zoomed_pane_id(zoomed);
                return true;
            }
        }
        if let Some(mut survivor) = surviving {
            survivor.set_zoomed_pane_id(zoomed);
            *self = survivor;
            true
        } else {
            // When: `surviving` is `None`, no child matched `id`, so the tree is unchanged.
            false
        }
    }

    /// Recursively compute each visible leaf's rectangle inside `outer`.
    pub fn layout(&self, outer: Rect) -> Vec<(PaneId, Rect)> {
        if let Some(id) = self.zoomed_pane_id() {
            // When: `zoomed_pane_id` is `Some`, one pane may claim the whole rectangle.
            if self.contains_leaf(id) {
                // When: `contains_leaf` is true, the zoomed pane is live here and hides its siblings.
                return vec![(id, outer)];
            }
        }

        let mut out = Vec::new();
        self.layout_into(outer, &mut out);
        out
    }

    fn layout_into(&self, outer: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            PaneTree::Leaf { id, .. } => out.push((*id, outer)),
            PaneTree::Split { axis, ratio, first, second, .. } => match axis {
                SplitAxis::Vertical => {
                    let first_width = outer.w * *ratio;
                    let first_rect = Rect::new(outer.x, outer.y, first_width, outer.h);
                    let second_rect =
                        Rect::new(outer.x + first_width, outer.y, outer.w - first_width, outer.h);
                    first.layout_into(first_rect, out);
                    second.layout_into(second_rect, out);
                }
                SplitAxis::Horizontal => {
                    let first_height = outer.h * *ratio;
                    let first_rect = Rect::new(outer.x, outer.y, outer.w, first_height);
                    let second_rect =
                        Rect::new(outer.x, outer.y + first_height, outer.w, outer.h - first_height);
                    first.layout_into(first_rect, out);
                    second.layout_into(second_rect, out);
                }
            },
        }
    }

    /// Recursively compute 1px splitter seams between adjacent leaves.
    ///
    /// The returned rects are interior seams only: no perimeter edges are
    /// emitted. Pane rectangles still tile `outer` with no gaps; callers draw
    /// these seams on top at the shared outer boundary, before applying any
    /// per-pane cell padding inside each pane.
    pub fn splitter_rects(&self, outer: Rect, thickness: f32) -> Vec<SplitterRect> {
        if self.zoomed_pane_id().is_some_and(|id| self.contains_leaf(id)) {
            // When: `zoomed_pane_id` names a leaf here, one pane covers the tab and hides every seam.
            return Vec::new();
        }

        let mut out = Vec::new();
        self.splitter_rects_into(outer, thickness.max(0.0), &mut out);
        coalesce_splitter_rects(out)
    }

    /// Find the splitter seam under a point, identified for a later drag.
    ///
    /// The returned [`SplitterId`] records the child path taken, so a drag can
    /// address the same divider after the tree is re-laid out.
    pub fn hit_splitter(
        &self,
        outer: Rect,
        thickness: f32,
        point_x: f32,
        point_y: f32,
    ) -> Option<SplitterHit> {
        if self.zoomed_pane_id().is_some_and(|id| self.contains_leaf(id)) {
            // When: `zoomed_pane_id` names a leaf here, no seam is drawn, so none can be hit.
            return None;
        }
        let mut path = Vec::new();
        self.hit_splitter_into(outer, thickness.max(0.0), point_x, point_y, &mut path)
    }

    fn hit_splitter_into(
        &self,
        outer: Rect,
        thickness: f32,
        point_x: f32,
        point_y: f32,
        path: &mut Vec<bool>,
    ) -> Option<SplitterHit> {
        match self {
            PaneTree::Leaf { .. } => None,
            PaneTree::Split { axis, ratio, first, second, .. } => {
                // When: `self` is a `Split`, its own seam is tested before descending into children.
                match axis {
                    SplitAxis::Vertical => {
                        // When: `axis` is Vertical, the seam is a vertical strip at the child boundary.
                        let first_width = outer.w * *ratio;
                        let first_rect = Rect::new(outer.x, outer.y, first_width, outer.h);
                        let second_rect = Rect::new(
                            outer.x + first_width,
                            outer.y,
                            outer.w - first_width,
                            outer.h,
                        );
                        let seam = Rect::new(
                            outer.x + first_width - thickness * 0.5,
                            outer.y,
                            thickness,
                            outer.h,
                        );
                        if seam.contains(point_x, point_y) {
                            // When: `seam` contains the point, this divider wins over any child seam below it.
                            return Some(SplitterHit {
                                id: SplitterId(path.clone()),
                                axis: *axis,
                                rect: seam,
                            });
                        }
                        path.push(false);
                        let hit =
                            first.hit_splitter_into(first_rect, thickness, point_x, point_y, path);
                        path.pop();
                        if hit.is_some() {
                            // When: `hit` is `Some`, the first child claimed the point, so stop descending.
                            return hit;
                        }
                        path.push(true);
                        let hit = second.hit_splitter_into(
                            second_rect,
                            thickness,
                            point_x,
                            point_y,
                            path,
                        );
                        path.pop();
                        hit
                    }
                    SplitAxis::Horizontal => {
                        // When: `axis` is Horizontal, the seam is a horizontal strip at the child boundary.
                        let first_height = outer.h * *ratio;
                        let first_rect = Rect::new(outer.x, outer.y, outer.w, first_height);
                        let second_rect = Rect::new(
                            outer.x,
                            outer.y + first_height,
                            outer.w,
                            outer.h - first_height,
                        );
                        let seam = Rect::new(
                            outer.x,
                            outer.y + first_height - thickness * 0.5,
                            outer.w,
                            thickness,
                        );
                        if seam.contains(point_x, point_y) {
                            // When: `seam` contains the point, this divider wins over any child seam inside it.
                            return Some(SplitterHit {
                                id: SplitterId(path.clone()),
                                axis: *axis,
                                rect: seam,
                            });
                        }
                        path.push(false);
                        let hit =
                            first.hit_splitter_into(first_rect, thickness, point_x, point_y, path);
                        path.pop();
                        if hit.is_some() {
                            // When: `hit` is `Some`, the first child claimed the point, so stop descending.
                            return hit;
                        }
                        path.push(true);
                        let hit = second.hit_splitter_into(
                            second_rect,
                            thickness,
                            point_x,
                            point_y,
                            path,
                        );
                        path.pop();
                        hit
                    }
                }
            }
        }
    }

    /// Move the divider named by `id` in response to a pointer drag.
    ///
    /// `delta_x` and `delta_y` are in the same units as `outer`; the component
    /// matching the divider's axis is converted to a ratio against that
    /// divider's own rectangle, so nested splits track the cursor at any depth.
    pub fn resize_splitter_by_delta(
        &mut self,
        id: &SplitterId,
        outer: Rect,
        delta_x: f32,
        delta_y: f32,
    ) -> bool {
        self.resize_splitter_by_delta_inner(&id.0, outer, delta_x, delta_y)
    }

    fn resize_splitter_by_delta_inner(
        &mut self,
        path: &[bool],
        outer: Rect,
        delta_x: f32,
        delta_y: f32,
    ) -> bool {
        match self {
            PaneTree::Leaf { .. } => false,
            PaneTree::Split { axis, ratio, first, second, .. } => {
                // When: `self` is a `Split`, `path` decides whether this divider moves or a child's does.
                if path.is_empty() {
                    // When: `path` is empty, this is the addressed divider, so apply the drag here.
                    let denom = match axis {
                        SplitAxis::Vertical => outer.w,
                        SplitAxis::Horizontal => outer.h,
                    };
                    if denom <= 0.0 {
                        // When: `denom` is nonpositive, the split has no extent to convert pixels into a ratio.
                        return false;
                    }
                    let delta = match axis {
                        SplitAxis::Vertical => delta_x / denom,
                        SplitAxis::Horizontal => delta_y / denom,
                    };
                    *ratio = (*ratio + delta).clamp(0.1, 0.9);
                    return true;
                }

                match axis {
                    SplitAxis::Vertical => {
                        let first_width = outer.w * *ratio;
                        let child_outer = if !path[0] {
                            Rect::new(outer.x, outer.y, first_width, outer.h)
                        } else {
                            // When: `path` selects the second child, its rectangle starts after `first_width`.
                            Rect::new(
                                outer.x + first_width,
                                outer.y,
                                outer.w - first_width,
                                outer.h,
                            )
                        };
                        if !path[0] {
                            first.resize_splitter_by_delta_inner(
                                &path[1..],
                                child_outer,
                                delta_x,
                                delta_y,
                            )
                        } else {
                            // When: `path` selects the second child, recurse there with the remaining path.
                            second.resize_splitter_by_delta_inner(
                                &path[1..],
                                child_outer,
                                delta_x,
                                delta_y,
                            )
                        }
                    }
                    SplitAxis::Horizontal => {
                        let first_height = outer.h * *ratio;
                        let child_outer = if !path[0] {
                            Rect::new(outer.x, outer.y, outer.w, first_height)
                        } else {
                            // When: `path` selects the second child, its rectangle starts below `first_height`.
                            Rect::new(
                                outer.x,
                                outer.y + first_height,
                                outer.w,
                                outer.h - first_height,
                            )
                        };
                        if !path[0] {
                            first.resize_splitter_by_delta_inner(
                                &path[1..],
                                child_outer,
                                delta_x,
                                delta_y,
                            )
                        } else {
                            // When: `path` selects the second child, recurse there with the remaining path.
                            second.resize_splitter_by_delta_inner(
                                &path[1..],
                                child_outer,
                                delta_x,
                                delta_y,
                            )
                        }
                    }
                }
            }
        }
    }

    fn splitter_rects_into(&self, outer: Rect, thickness: f32, out: &mut Vec<SplitterRect>) {
        match self {
            PaneTree::Leaf { .. } => {
                // When: `self` is a `Leaf`, it has no interior boundary, so it contributes no seam.
            }
            PaneTree::Split { axis, ratio, first, second, .. } => match axis {
                SplitAxis::Vertical => {
                    let first_width = outer.w * *ratio;
                    let first_rect = Rect::new(outer.x, outer.y, first_width, outer.h);
                    let second_rect =
                        Rect::new(outer.x + first_width, outer.y, outer.w - first_width, outer.h);
                    let seam_x = outer.x + first_width - thickness * 0.5;
                    out.push(SplitterRect {
                        axis: *axis,
                        rect: Rect::new(seam_x, outer.y, thickness, outer.h),
                    });
                    first.splitter_rects_into(first_rect, thickness, out);
                    second.splitter_rects_into(second_rect, thickness, out);
                }
                SplitAxis::Horizontal => {
                    let first_height = outer.h * *ratio;
                    let first_rect = Rect::new(outer.x, outer.y, outer.w, first_height);
                    let second_rect =
                        Rect::new(outer.x, outer.y + first_height, outer.w, outer.h - first_height);
                    let seam_y = outer.y + first_height - thickness * 0.5;
                    out.push(SplitterRect {
                        axis: *axis,
                        rect: Rect::new(outer.x, seam_y, outer.w, thickness),
                    });
                    first.splitter_rects_into(first_rect, thickness, out);
                    second.splitter_rects_into(second_rect, thickness, out);
                }
            },
        }
    }

    /// Find the leaf whose rectangle is the closest spatial neighbour of
    /// `focus` in direction `dir`. Returns `None` when nothing lies in that
    /// direction (focus is on the edge).
    pub fn focus_neighbor(&self, focus: PaneId, dir: Direction) -> Option<PaneId> {
        // Unit reference frame — direction-independent of window size.
        let panes = self.layout(Rect::new(0.0, 0.0, 1.0, 1.0));
        Self::focus_neighbor_in_layout(&panes, focus, dir)
    }

    /// Resolve left, right, up, and down neighbours from one normalized pane layout.
    pub fn focus_neighbors(&self, focus: PaneId) -> [Option<PaneId>; 4] {
        let panes = self.layout(Rect::new(0.0, 0.0, 1.0, 1.0));
        [Direction::Left, Direction::Right, Direction::Up, Direction::Down]
            .map(|direction| Self::focus_neighbor_in_layout(&panes, focus, direction))
    }

    fn focus_neighbor_in_layout(
        panes: &[(PaneId, Rect)],
        focus: PaneId,
        dir: Direction,
    ) -> Option<PaneId> {
        let origin = panes.iter().find(|(id, _)| *id == focus)?.1;
        let (origin_x, origin_y) = origin.center();

        let mut best: Option<(f32, PaneId)> = None;
        for (id, rect) in panes {
            if *id == focus {
                // When: `id` equals `focus`, the origin pane cannot be its own neighbour.
                continue;
            }
            let (center_x, center_y) = rect.center();
            let candidate = match dir {
                Direction::Left => {
                    center_x < origin_x - 1e-6
                        && rect.y < origin.y + origin.h
                        && rect.y + rect.h > origin.y
                }
                Direction::Right => {
                    center_x > origin_x + 1e-6
                        && rect.y < origin.y + origin.h
                        && rect.y + rect.h > origin.y
                }
                Direction::Up => {
                    center_y < origin_y - 1e-6
                        && rect.x < origin.x + origin.w
                        && rect.x + rect.w > origin.x
                }
                Direction::Down => {
                    center_y > origin_y + 1e-6
                        && rect.x < origin.x + origin.w
                        && rect.x + rect.w > origin.x
                }
            };
            if !candidate {
                // When: `candidate` is false, this pane is not in `dir` or misses the focus band entirely.
                continue;
            }
            let dist = match dir {
                Direction::Left => (origin_x - center_x).abs() + (origin_y - center_y).abs() * 0.01,
                Direction::Right => {
                    (center_x - origin_x).abs() + (origin_y - center_y).abs() * 0.01
                }
                Direction::Up => (origin_y - center_y).abs() + (origin_x - center_x).abs() * 0.01,
                Direction::Down => (center_y - origin_y).abs() + (origin_x - center_x).abs() * 0.01,
            };
            match best {
                Some((best_distance, _)) if best_distance <= dist => {
                    // When: `best_distance` is at most `dist`, an earlier pane is nearer, so `best` is kept.
                }
                _ => best = Some((dist, *id)),
            }
        }
        best.map(|(_, id)| id)
    }
}

#[cfg(test)]
#[path = "pane_tests.rs"]
mod pane_tests;
