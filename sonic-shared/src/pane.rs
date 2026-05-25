//! Pane tree — recursive horizontal/vertical splits inside a tab.

use sonic_core::keymap::Direction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    Horizontal, // children stacked top↔bottom
    Vertical,   // children stacked left↔right
}

#[derive(Debug, Clone)]
pub enum PaneTree {
    Leaf {
        id: u64,
    },
    Split {
        axis: SplitAxis,
        ratio: f32, // 0..1, share for the first child
        first: Box<PaneTree>,
        second: Box<PaneTree>,
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

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}

impl PaneTree {
    pub fn leaf(id: u64) -> Self {
        PaneTree::Leaf { id }
    }

    /// Split the focused leaf in `dir`, returning the id of the new pane.
    pub fn split(&mut self, focus: u64, dir: Direction, new_id: u64) -> bool {
        let axis = match dir {
            Direction::Left | Direction::Right => SplitAxis::Vertical,
            Direction::Up | Direction::Down => SplitAxis::Horizontal,
        };
        let put_new_first = matches!(dir, Direction::Left | Direction::Up);
        self.split_recursive(focus, axis, put_new_first, new_id)
    }

    fn split_recursive(
        &mut self,
        focus: u64,
        axis: SplitAxis,
        new_first: bool,
        new_id: u64,
    ) -> bool {
        match self {
            PaneTree::Leaf { id } if *id == focus => {
                let existing = PaneTree::leaf(*id);
                let new_leaf = PaneTree::leaf(new_id);
                let (first, second) =
                    if new_first { (new_leaf, existing) } else { (existing, new_leaf) };
                *self = PaneTree::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(second),
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

    /// Collect leaf ids in left-to-right, top-to-bottom order.
    pub fn leaves(&self) -> Vec<u64> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<u64>) {
        match self {
            PaneTree::Leaf { id } => out.push(*id),
            PaneTree::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    /// Remove the leaf with `id`. If a Split ends up with one child, it
    /// collapses to that child. Returns true if anything was removed.
    pub fn close(&mut self, id: u64) -> bool {
        if let PaneTree::Leaf { id: leaf } = self {
            return *leaf == id;
        }
        let mut surviving: Option<PaneTree> = None;
        if let PaneTree::Split { first, second, .. } = self {
            let first_is = matches!(first.as_ref(), PaneTree::Leaf { id: l } if *l == id);
            let second_is = matches!(second.as_ref(), PaneTree::Leaf { id: l } if *l == id);
            if first_is {
                surviving = Some(std::mem::replace(second.as_mut(), PaneTree::leaf(0)));
            } else if second_is {
                surviving = Some(std::mem::replace(first.as_mut(), PaneTree::leaf(0)));
            } else if first.close(id) || second.close(id) {
                return true;
            }
        }
        if let Some(t) = surviving {
            *self = t;
            true
        } else {
            false
        }
    }

    /// Recursively compute each leaf's rectangle inside `outer`.
    pub fn layout(&self, outer: Rect) -> Vec<(u64, Rect)> {
        let mut out = Vec::new();
        self.layout_into(outer, &mut out);
        out
    }

    fn layout_into(&self, outer: Rect, out: &mut Vec<(u64, Rect)>) {
        match self {
            PaneTree::Leaf { id } => out.push((*id, outer)),
            PaneTree::Split { axis, ratio, first, second } => match axis {
                SplitAxis::Vertical => {
                    let w1 = outer.w * ratio;
                    let r1 = Rect::new(outer.x, outer.y, w1, outer.h);
                    let r2 = Rect::new(outer.x + w1, outer.y, outer.w - w1, outer.h);
                    first.layout_into(r1, out);
                    second.layout_into(r2, out);
                }
                SplitAxis::Horizontal => {
                    let h1 = outer.h * ratio;
                    let r1 = Rect::new(outer.x, outer.y, outer.w, h1);
                    let r2 = Rect::new(outer.x, outer.y + h1, outer.w, outer.h - h1);
                    first.layout_into(r1, out);
                    second.layout_into(r2, out);
                }
            },
        }
    }

    /// Find the leaf whose rectangle is the closest spatial neighbour of
    /// `focus` in direction `dir`. Returns `None` when nothing lies in that
    /// direction (focus is on the edge).
    pub fn focus_neighbor(&self, focus: u64, dir: Direction) -> Option<u64> {
        // Unit reference frame — direction-independent of window size.
        let panes = self.layout(Rect::new(0.0, 0.0, 1.0, 1.0));
        let me = panes.iter().find(|(id, _)| *id == focus)?.1;
        let (mx, my) = me.center();

        let mut best: Option<(f32, u64)> = None;
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
