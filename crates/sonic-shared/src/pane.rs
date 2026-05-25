//! Pane tree — recursive horizontal/vertical splits inside a tab.

use sonic_core::keymap::Direction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    Horizontal, // children stacked top↔bottom
    Vertical,   // children stacked left↔right
}

#[derive(Debug)]
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

    fn split_recursive(&mut self, focus: u64, axis: SplitAxis, new_first: bool, new_id: u64) -> bool {
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
        // Top-level leaf cannot be removed without leaving an empty tree.
        if let PaneTree::Leaf { id: leaf } = self {
            return *leaf == id; // signal "I am that leaf"
        }
        let mut taken: Option<PaneTree> = None;
        if let PaneTree::Split { first, second, .. } = self {
            let first_is = matches!(first.as_ref(), PaneTree::Leaf { id: l } if *l == id);
            let second_is = matches!(second.as_ref(), PaneTree::Leaf { id: l } if *l == id);
            if first_is {
                taken = Some(std::mem::replace(second.as_mut(), PaneTree::leaf(0)));
            } else if second_is {
                taken = Some(std::mem::replace(first.as_mut(), PaneTree::leaf(0)));
            } else if first.close(id) {
                taken = Some(std::mem::replace(second.as_mut(), PaneTree::leaf(0)));
            } else if second.close(id) {
                taken = Some(std::mem::replace(first.as_mut(), PaneTree::leaf(0)));
            }
        }
        if let Some(t) = taken {
            *self = t;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_right_then_down() {
        let mut t = PaneTree::leaf(1);
        assert!(t.split(1, Direction::Right, 2));
        assert!(t.split(2, Direction::Down, 3));
        assert_eq!(t.leaves(), vec![1, 2, 3]);
    }

    #[test]
    fn close_collapses_split() {
        let mut t = PaneTree::leaf(1);
        t.split(1, Direction::Right, 2);
        t.close(2);
        assert_eq!(t.leaves(), vec![1]);
        assert!(matches!(t, PaneTree::Leaf { id: 1 }));
    }
}
