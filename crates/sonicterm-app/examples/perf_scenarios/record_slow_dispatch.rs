//! Bounded slow-dispatch retention and descending-duration order.

use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) struct ByDuration(pub(super) SlowDispatch);

impl PartialEq for ByDuration {
    fn eq(&self, other: &Self) -> bool {
        self.0.duration_ms.total_cmp(&other.0.duration_ms).is_eq()
    }
}

impl Eq for ByDuration {}

impl PartialOrd for ByDuration {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ByDuration {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.duration_ms.total_cmp(&other.0.duration_ms)
    }
}

impl SlowDispatches {
    /// Offer one dispatch: it is kept while the heap has room or when it outlasts the shortest one kept.
    pub(crate) fn push(&mut self, dispatch: SlowDispatch) {
        self.count += 1;
        if self.shortest_first.len() < SLOW_DISPATCH_LIMIT {
            self.shortest_first.push(std::cmp::Reverse(ByDuration(dispatch)));
        } else if self
            .shortest_first
            .peek()
            .is_some_and(|shortest| dispatch.duration_ms > shortest.0 .0.duration_ms)
        {
            // When: the heap is full and this dispatch outlasts its shortest, that one makes room.
            self.shortest_first.pop();
            self.shortest_first.push(std::cmp::Reverse(ByDuration(dispatch)));
        }
    }
}

impl SlowDispatches {
    /// The kept dispatches, longest first, and the number offered.
    pub(crate) fn finish(self) -> (Vec<SlowDispatch>, u64) {
        let mut kept: Vec<SlowDispatch> =
            self.shortest_first.into_iter().map(|entry| entry.0 .0).collect();
        kept.sort_by(|left, right| right.duration_ms.total_cmp(&left.duration_ms));
        (kept, self.count)
    }
}
