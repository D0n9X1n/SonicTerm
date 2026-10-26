use super::{Cell, Cluster};

/// Transparent iterator over either storage form. Yields `&Cell` without
/// materialising the flat form for Cluster storage. Implements
/// `DoubleEndedIterator` + `ExactSizeIterator` so the existing
/// `iter().rev()` / `iter().len()` call sites keep working unchanged.
pub enum LineIter<'a> {
    Empty,
    Flat(std::slice::Iter<'a, Cell>),
    /// A trimmed row (or a window of one): part of its prefix, then its fill
    /// `fill_remaining` times.
    Trimmed {
        prefix: std::slice::Iter<'a, Cell>,
        fill: &'a Cell,
        fill_remaining: usize,
    },
    /// Bi-directional walk over a slice of clusters covering exactly
    /// `total` cells, with `head_*` tracking the next-from-front cluster
    /// position and `tail_*` tracking the next-from-back cluster
    /// position. Front and back may chase into the same cluster, at
    /// which point `total` reaching 0 terminates iteration.
    Cluster {
        clusters: &'a [Cluster],
        /// Index of the next cluster to consume from the front.
        head_idx: usize,
        /// Number of cells still to yield from the current head cluster.
        head_remaining: usize,
        /// Index of the next cluster to consume from the back
        /// (inclusive — i.e. `clusters[tail_idx]` still has cells to
        /// yield from the back side).
        tail_idx: usize,
        /// Number of cells still to yield from the current tail cluster
        /// (consumed in reverse from its tail).
        tail_remaining: usize,
        /// Total cells still to yield (front + back combined).
        total: usize,
    },
}

impl<'a> LineIter<'a> {
    pub(super) fn empty() -> Self {
        LineIter::Empty
    }

    /// Build a Cluster iterator covering the full cluster list.
    pub(super) fn new_cluster(clusters: &'a [Cluster], total: usize) -> Self {
        if clusters.is_empty() || total == 0 {
            // When: `clusters.is_empty()` or `total == 0`, the full iterator is empty.
            return LineIter::Empty;
        }
        LineIter::Cluster {
            clusters,
            head_idx: 0,
            head_remaining: clusters[0].count,
            tail_idx: clusters.len() - 1,
            tail_remaining: clusters[clusters.len() - 1].count,
            total,
        }
    }

    /// Build a Cluster iterator over a windowed range `[start, start+take)`.
    /// Walks `clusters` to find the cluster containing `start`, then
    /// configures head/tail bookkeeping so exactly `take` cells are
    /// yielded.
    pub(super) fn cluster_range(clusters: &'a [Cluster], start: usize, take: usize) -> Self {
        if take == 0 {
            // When: `take == 0`, the requested cluster window is empty.
            return LineIter::Empty;
        }
        // Find head: cluster containing `start`.
        let mut off = 0;
        let mut head_idx = 0;
        let mut head_remaining = 0;
        while head_idx < clusters.len() {
            let cluster = &clusters[head_idx];
            if start < off + cluster.count {
                // When: `start < off + cluster.count`, this cluster contains the range head.
                head_remaining = (off + cluster.count) - start;
                break;
            }
            off += cluster.count;
            head_idx += 1;
        }
        if head_idx >= clusters.len() {
            // When: `head_idx >= clusters.len()`, `start` lies beyond all clusters.
            return LineIter::Empty;
        }
        // Find tail: cluster containing `start + take - 1`.
        let end_inclusive = start + take - 1;
        let mut off2 = 0;
        let mut tail_idx = 0;
        let mut tail_remaining = 0;
        for (index, cluster) in clusters.iter().enumerate() {
            if end_inclusive < off2 + cluster.count {
                // When: `end_inclusive < off2 + cluster.count`, this cluster contains the range tail.
                tail_idx = index;
                tail_remaining = end_inclusive - off2 + 1;
                break;
            }
            off2 += cluster.count;
        }
        if tail_idx == head_idx {
            // Same cluster — the window lives entirely inside it. Reconcile.
            head_remaining = take;
            tail_remaining = take;
        }
        LineIter::Cluster {
            clusters,
            head_idx,
            head_remaining,
            tail_idx,
            tail_remaining,
            total: take,
        }
    }
}

impl<'a> Iterator for LineIter<'a> {
    type Item = &'a Cell;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            LineIter::Empty => None,
            LineIter::Flat(it) => it.next(),
            LineIter::Trimmed { prefix, fill, fill_remaining } => {
                // walking `Trimmed` storage forward, the prefix comes before the fill.
                prefix.next().or_else(|| {
                    (*fill_remaining > 0).then(|| {
                        *fill_remaining -= 1;
                        *fill
                    })
                })
            }
            LineIter::Cluster {
                clusters,
                head_idx,
                head_remaining,
                tail_idx,
                tail_remaining,
                total,
            } => {
                // When: iterating `Cluster` storage, advance using the front-side run state.
                if *total == 0 {
                    // When: `*total == 0`, no cells remain to yield.
                    return None;
                }
                // Advance head if exhausted in current cluster.
                while *head_remaining == 0 {
                    *head_idx += 1;
                    if *head_idx > *tail_idx {
                        // When: `*head_idx > *tail_idx`, front iteration has crossed the tail.
                        return None;
                    }
                    *head_remaining = if *head_idx == *tail_idx {
                        *tail_remaining
                    } else {
                        // When: `*head_idx != *tail_idx`, load the full next cluster count.
                        clusters[*head_idx].count
                    };
                }
                let cell = &clusters[*head_idx].cell;
                *head_remaining -= 1;
                *total -= 1;
                // Keep tail bookkeeping consistent when head and tail
                // share a cluster.
                if *head_idx == *tail_idx {
                    *tail_remaining = (*tail_remaining).saturating_sub(1);
                }
                Some(cell)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl<'a> DoubleEndedIterator for LineIter<'a> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            LineIter::Empty => None,
            LineIter::Flat(it) => it.next_back(),
            LineIter::Trimmed { prefix, fill, fill_remaining } => {
                // Walking `Trimmed` storage backward, the fill comes before the prefix.
                if *fill_remaining > 0 {
                    *fill_remaining -= 1;
                    Some(*fill)
                } else {
                    // When: `*fill_remaining == 0`, the prefix supplies the rest from its end.
                    prefix.next_back()
                }
            }
            LineIter::Cluster {
                clusters,
                head_idx,
                head_remaining,
                tail_idx,
                tail_remaining,
                total,
            } => {
                // When: iterating `Cluster` storage backward, use the tail-side run state.
                if *total == 0 {
                    // When: `*total == 0`, no cells remain to yield.
                    return None;
                }
                while *tail_remaining == 0 {
                    if *tail_idx == 0 || *tail_idx <= *head_idx {
                        // When: `*tail_idx == 0` or `*tail_idx <= *head_idx`, the tail cannot retreat.
                        return None;
                    }
                    *tail_idx -= 1;
                    *tail_remaining = if *tail_idx == *head_idx {
                        *head_remaining
                    } else {
                        // When: `*tail_idx != *head_idx`, load the full preceding cluster count.
                        clusters[*tail_idx].count
                    };
                }
                let cell = &clusters[*tail_idx].cell;
                *tail_remaining -= 1;
                *total -= 1;
                if *head_idx == *tail_idx {
                    *head_remaining = (*head_remaining).saturating_sub(1);
                }
                Some(cell)
            }
        }
    }
}

impl<'a> ExactSizeIterator for LineIter<'a> {
    fn len(&self) -> usize {
        match self {
            LineIter::Empty => 0,
            LineIter::Flat(it) => it.len(),
            LineIter::Trimmed { prefix, fill_remaining, .. } => prefix.len() + fill_remaining,
            LineIter::Cluster { total, .. } => *total,
        }
    }
}

#[cfg(test)]
#[path = "line_iter_tests.rs"]
mod line_iter_tests;
