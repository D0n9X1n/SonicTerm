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

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}

fn coalesce_splitter_rects(mut splitters: Vec<SplitterRect>) -> Vec<SplitterRect> {
    let eps = 0.01_f32;
    let mut changed = true;
    while changed {
        changed = false;
        'outer: for i in 0..splitters.len() {
            for j in (i + 1)..splitters.len() {
                if let Some(merged) = merge_splitter(splitters[i], splitters[j], eps) {
                    splitters[i] = merged;
                    splitters.remove(j);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    splitters
}

fn merge_splitter(a: SplitterRect, b: SplitterRect, eps: f32) -> Option<SplitterRect> {
    if a.axis != b.axis {
        return None;
    }

    match a.axis {
        SplitAxis::Vertical => {
            if (a.rect.x - b.rect.x).abs() > eps || (a.rect.w - b.rect.w).abs() > eps {
                return None;
            }
            let top = a.rect.y.min(b.rect.y);
            let bottom = (a.rect.y + a.rect.h).max(b.rect.y + b.rect.h);
            let combined_h = a.rect.h + b.rect.h;
            if bottom - top - combined_h > eps {
                return None;
            }
            Some(SplitterRect {
                axis: a.axis,
                rect: Rect::new(a.rect.x, top, a.rect.w, bottom - top),
            })
        }
        SplitAxis::Horizontal => {
            if (a.rect.y - b.rect.y).abs() > eps || (a.rect.h - b.rect.h).abs() > eps {
                return None;
            }
            let left = a.rect.x.min(b.rect.x);
            let right = (a.rect.x + a.rect.w).max(b.rect.x + b.rect.w);
            let combined_w = a.rect.w + b.rect.w;
            if right - left - combined_w > eps {
                return None;
            }
            Some(SplitterRect {
                axis: a.axis,
                rect: Rect::new(left, a.rect.y, right - left, a.rect.h),
            })
        }
    }
}

impl PaneTree {
    pub fn leaf(id: PaneId) -> Self {
        PaneTree::Leaf { id, zoomed_pane_id: None }
    }

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

    pub fn toggle_zoom(&mut self, active_pane: PaneId) -> bool {
        if self.zoomed_pane_id() == Some(active_pane) {
            self.set_zoomed_pane_id(None);
            return true;
        }

        if self.contains_leaf(active_pane) {
            self.set_zoomed_pane_id(Some(active_pane));
            true
        } else {
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

    /// Split the focused leaf in `dir`, returning the id of the new pane.
    pub fn split(&mut self, focus: PaneId, dir: Direction, new_id: PaneId) -> bool {
        let axis = match dir {
            Direction::Left | Direction::Right => SplitAxis::Vertical,
            Direction::Up | Direction::Down => SplitAxis::Horizontal,
        };
        let put_new_first = matches!(dir, Direction::Left | Direction::Up);
        let zoomed = self.zoomed_pane_id();
        self.split_recursive(focus, axis, put_new_first, new_id, zoomed)
    }

    fn split_recursive(
        &mut self,
        focus: PaneId,
        axis: SplitAxis,
        new_first: bool,
        new_id: PaneId,
        zoomed_pane_id: Option<PaneId>,
    ) -> bool {
        match self {
            PaneTree::Leaf { id, .. } if *id == focus => {
                let existing = PaneTree::leaf(*id);
                let new_leaf = PaneTree::leaf(new_id);
                let (first, second) =
                    if new_first { (new_leaf, existing) } else { (existing, new_leaf) };
                *self = PaneTree::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(second),
                    zoomed_pane_id,
                };
                true
            }
            PaneTree::Leaf { .. } => false,
            PaneTree::Split { first, second, .. } => {
                first.split_recursive(focus, axis, new_first, new_id, zoomed_pane_id)
                    || second.split_recursive(focus, axis, new_first, new_id, zoomed_pane_id)
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
                let directly_owns_active = matches!(first.as_ref(), PaneTree::Leaf { id, .. } if *id == active_pane)
                    || matches!(second.as_ref(), PaneTree::Leaf { id, .. } if *id == active_pane);
                if directly_owns_active {
                    let axis_matches = matches!(
                        (*axis, dir),
                        (SplitAxis::Vertical, Direction::Left | Direction::Right)
                            | (SplitAxis::Horizontal, Direction::Up | Direction::Down)
                    );
                    if axis_matches {
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
            return *leaf == id;
        }
        let zoomed = self.zoomed_pane_id().filter(|zoomed| *zoomed != id);
        let mut surviving: Option<PaneTree> = None;
        if let PaneTree::Split { first, second, .. } = self {
            let first_is = matches!(first.as_ref(), PaneTree::Leaf { id: l, .. } if *l == id);
            let second_is = matches!(second.as_ref(), PaneTree::Leaf { id: l, .. } if *l == id);
            if first_is {
                surviving = Some(std::mem::replace(second.as_mut(), PaneTree::leaf(0)));
            } else if second_is {
                surviving = Some(std::mem::replace(first.as_mut(), PaneTree::leaf(0)));
            } else if first.close(id) || second.close(id) {
                self.set_zoomed_pane_id(zoomed);
                return true;
            }
        }
        if let Some(mut t) = surviving {
            t.set_zoomed_pane_id(zoomed);
            *self = t;
            true
        } else {
            false
        }
    }

    /// Recursively compute each visible leaf's rectangle inside `outer`.
    pub fn layout(&self, outer: Rect) -> Vec<(PaneId, Rect)> {
        if let Some(id) = self.zoomed_pane_id() {
            if self.contains_leaf(id) {
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
                    let w1 = outer.w * *ratio;
                    let r1 = Rect::new(outer.x, outer.y, w1, outer.h);
                    let r2 = Rect::new(outer.x + w1, outer.y, outer.w - w1, outer.h);
                    first.layout_into(r1, out);
                    second.layout_into(r2, out);
                }
                SplitAxis::Horizontal => {
                    let h1 = outer.h * *ratio;
                    let r1 = Rect::new(outer.x, outer.y, outer.w, h1);
                    let r2 = Rect::new(outer.x, outer.y + h1, outer.w, outer.h - h1);
                    first.layout_into(r1, out);
                    second.layout_into(r2, out);
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
            return Vec::new();
        }

        let mut out = Vec::new();
        self.splitter_rects_into(outer, thickness.max(0.0), &mut out);
        coalesce_splitter_rects(out)
    }

    fn splitter_rects_into(&self, outer: Rect, thickness: f32, out: &mut Vec<SplitterRect>) {
        match self {
            PaneTree::Leaf { .. } => {}
            PaneTree::Split { axis, ratio, first, second, .. } => match axis {
                SplitAxis::Vertical => {
                    let w1 = outer.w * *ratio;
                    let r1 = Rect::new(outer.x, outer.y, w1, outer.h);
                    let r2 = Rect::new(outer.x + w1, outer.y, outer.w - w1, outer.h);
                    let x = outer.x + w1 - thickness * 0.5;
                    out.push(SplitterRect {
                        axis: *axis,
                        rect: Rect::new(x, outer.y, thickness, outer.h),
                    });
                    first.splitter_rects_into(r1, thickness, out);
                    second.splitter_rects_into(r2, thickness, out);
                }
                SplitAxis::Horizontal => {
                    let h1 = outer.h * *ratio;
                    let r1 = Rect::new(outer.x, outer.y, outer.w, h1);
                    let r2 = Rect::new(outer.x, outer.y + h1, outer.w, outer.h - h1);
                    let y = outer.y + h1 - thickness * 0.5;
                    out.push(SplitterRect {
                        axis: *axis,
                        rect: Rect::new(outer.x, y, outer.w, thickness),
                    });
                    first.splitter_rects_into(r1, thickness, out);
                    second.splitter_rects_into(r2, thickness, out);
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
        let me = panes.iter().find(|(id, _)| *id == focus)?.1;
        let (mx, my) = me.center();

        let mut best: Option<(f32, PaneId)> = None;
        for (id, r) in &panes {
            if *id == focus {
                continue;
            }
            let (cx, cy) = r.center();
            let candidate = match dir {
                Direction::Left => cx < mx - 1e-6 && r.y < me.y + me.h && r.y + r.h > me.y,
                Direction::Right => cx > mx + 1e-6 && r.y < me.y + me.h && r.y + r.h > me.y,
                Direction::Up => cy < my - 1e-6 && r.x < me.x + me.w && r.x + r.w > me.x,
                Direction::Down => cy > my + 1e-6 && r.x < me.x + me.w && r.x + r.w > me.x,
            };
            if !candidate {
                continue;
            }
            let dist = match dir {
                Direction::Left => (mx - cx).abs() + (my - cy).abs() * 0.01,
                Direction::Right => (cx - mx).abs() + (my - cy).abs() * 0.01,
                Direction::Up => (my - cy).abs() + (mx - cx).abs() * 0.01,
                Direction::Down => (cy - my).abs() + (mx - cx).abs() * 0.01,
            };
            match best {
                Some((d, _)) if d <= dist => {}
                _ => best = Some((dist, *id)),
            }
        }
        best.map(|(_, id)| id)
    }
}
