use num::{Integer, ToPrimitive};
use std::cmp::{max, min, Ordering};
use std::fmt::Debug;
use std::ops::Range;

/// Track a set of integers, collapsing adjacent integers into ranges.
/// Internally stores the set in an array of ranges.
/// Allows adding and subtracting ranges.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RangeSet<T: Integer + Copy> {
    ranges: Vec<Range<T>>,
    needs_sort: bool,
}

pub fn range_is_empty<T: Integer>(range: &Range<T>) -> bool {
    range.start == range.end
}

/// Returns true if `first` intersects `second`
pub fn intersects_range<T: Integer + Copy + Debug>(first: &Range<T>, second: &Range<T>) -> bool {
    let start = max(first.start, second.start);
    let end = min(first.end, second.end);

    end > start
}

/// Computes the intersection of `first` and `second`
pub fn range_intersection<T: Integer + Copy + Debug>(
    first: &Range<T>,
    second: &Range<T>,
) -> Option<Range<T>> {
    let start = max(first.start, second.start);
    let end = min(first.end, second.end);

    if end > start {
        Some(start..end)
    } else {
        // When: `end > start` is false, the ranges have no non-empty intersection.
        None
    }
}

/// Computes `range` minus `removed`, which may result in up to two non-overlapping ranges.
pub fn range_subtract<T: Integer + Copy + Debug>(
    range: &Range<T>,
    removed: &Range<T>,
) -> (Option<Range<T>>, Option<Range<T>>) {
    let i_start = max(range.start, removed.start);
    let i_end = min(range.end, removed.end);

    if i_end > i_start {
        let left = if i_start == range.start {
            // Intersection overlaps with the LHS
            None
        } else {
            // When: `i_start == range.start` is false, preserve the left remainder.
            // The LHS up to the intersection
            Some(range.start..range.end.min(i_start))
        };

        let right = if i_end == range.end {
            // Intersection overlaps with the RHS
            None
        } else {
            // When: `i_end == range.end` is false, preserve the right remainder.
            // The intersection up to the RHS
            Some(range.end.min(i_end)..range.end)
        };

        (left, right)
    } else {
        // When: `i_end > i_start` is false, subtraction leaves `range` unchanged.
        // No intersection, so `range` is left with nothing removed
        (Some(range.clone()), None)
    }
}

/// Merge two ranges to produce their union
pub fn range_union<T: Integer>(first: Range<T>, second: Range<T>) -> Range<T> {
    if range_is_empty(&first) {
        second
    } else if range_is_empty(&second) {
        // When: `first` is non-empty but `second` is empty, the union is `first`.
        first
    } else {
        // When: both `range_is_empty(&first)` and `range_is_empty(&second)` are false, span them.
        let start = first.start.min(second.start);
        let end = first.end.max(second.end);
        start..end
    }
}

impl<T: Integer + Copy + Debug + ToPrimitive> From<RangeSet<T>> for Vec<Range<T>> {
    fn from(set: RangeSet<T>) -> Vec<Range<T>> {
        set.ranges
    }
}

impl<T: Integer + Copy + Debug + ToPrimitive> RangeSet<T> {
    /// Create a new set
    pub fn new() -> Self {
        Self { ranges: vec![], needs_sort: false }
    }

    /// Returns true if this set is empty
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Returns the total size of the range (the sum of the start..end
    /// distance of all contained ranges)
    pub fn len(&self) -> T {
        let mut total = num::zero();
        for range in &self.ranges {
            total = total + range.end - range.start;
        }
        total
    }

    /// Returns true if this set contains the specified integer
    pub fn contains(&self, value: T) -> bool {
        for range in &self.ranges {
            if range.contains(&value) {
                // When: `range.contains(&value)` is true, membership is established.
                return true;
            }
        }
        false
    }

    /// Returns a rangeset containing all of the integers that are present
    /// in self but not in other.
    /// The current implementation is `O(n**2)` but this should be "OK"
    /// as the likely scenario is that there will be a large contiguous
    /// range for the scrollback, and a smaller contiguous range for changes
    /// in the viewport.
    /// If that doesn't hold up, we can improve this.
    pub fn difference(&self, other: &Self) -> Self {
        let mut result = self.clone();

        for range in &other.ranges {
            result.remove_range(range.clone());
        }

        result
    }

    pub fn intersection(&self, other: &Self) -> Self {
        let mut result = Self::new();
        for range in &other.ranges {
            for own_range in &self.ranges {
                if let Some(overlap) = range_intersection(own_range, range) {
                    result.add_range(overlap);
                }
            }
        }

        result
    }

    pub fn intersection_with_range(&self, range: Range<T>) -> Self {
        let mut result = Self::new();

        for own_range in &self.ranges {
            if let Some(overlap) = range_intersection(own_range, &range) {
                result.add_range(overlap);
            }
        }

        result
    }

    /// Remove a single integer from the set
    pub fn remove(&mut self, value: T) {
        self.remove_range(value..value + num::one());
    }

    /// Remove a range of integers from the set
    pub fn remove_range(&mut self, range: Range<T>) {
        let mut to_add = vec![];
        let mut to_remove = vec![];

        for (idx, stored) in self.ranges.iter().enumerate() {
            match range_subtract(stored, &range) {
                (None, None) => to_remove.push(idx),
                (Some(left), Some(right)) => {
                    to_remove.push(idx);
                    to_add.push(left);
                    to_add.push(right);
                }
                (Some(remainder), None) | (None, Some(remainder)) if remainder != *stored => {
                    to_remove.push(idx);
                    to_add.push(remainder);
                }
                _ => {
                    // When: subtraction left this stored range unchanged, no edit is needed.
                }
            }
        }

        for idx in to_remove.into_iter().rev() {
            self.ranges.remove(idx);
        }

        for remainder in to_add {
            self.add_range(remainder);
        }
    }

    /// Remove a set of ranges from this set
    pub fn remove_set(&mut self, set: &Self) {
        for range in set.iter() {
            self.remove_range(range.clone());
        }
    }

    /// Add a single integer to the set
    pub fn add(&mut self, value: T) {
        self.add_range(value..value + num::one());
    }

    /// Add a range of integers to the set
    pub fn add_range(&mut self, range: Range<T>) {
        if range_is_empty(&range) {
            // When: `range_is_empty(&range)` is true, adding it cannot change the set.
            return;
        }

        if self.ranges.is_empty() {
            // When: `self.ranges.is_empty()` is true, this range becomes the sole entry.
            self.ranges.push(range);
            return;
        }

        self.sort_if_needed();

        match self.intersection_helper(&range) {
            (Some(first_index), Some(second_index)) if second_index == first_index + 1 => {
                // This range intersects with two or more adjacent ranges and will
                // therefore join them together

                let second = self.ranges[second_index].clone();
                let merged = range_union(range, second);

                self.ranges.remove(second_index);
                self.add_range(merged)
            }
            (Some(index), _) => self.merge_into_range(index, range),
            (None, Some(_)) => unreachable!(),
            (None, None) => {
                // No intersection, so find the insertion point
                let idx = self.insertion_point(&range);
                self.ranges.insert(idx, range.clone());
            }
        }
    }

    pub fn add_range_unchecked(&mut self, range: Range<T>) {
        self.ranges.push(range);
        self.needs_sort = true;
    }

    /// Add a set of ranges to this set
    pub fn add_set(&mut self, set: &Self) {
        for range in set.iter() {
            self.add_range(range.clone());
        }
    }

    fn merge_into_range(&mut self, idx: usize, range: Range<T>) {
        let existing = self.ranges[idx].clone();
        self.ranges[idx] = range_union(existing, range);
    }

    fn intersection_helper(&self, range: &Range<T>) -> (Option<usize>, Option<usize>) {
        if self.needs_sort {
            panic!("rangeset needs sorting");
        }

        let idx = match self.binary_search_ranges(range) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };

        let mut first = None;
        if let Some(stored) = self.ranges.get(idx) {
            if intersects_range(stored, range)
                || stored.end == range.start
                || range.end == stored.start
            {
                first = Some(idx);
            }
        }
        if let Some(next) = self.ranges.get(idx + 1) {
            // When: `self.ranges.get(idx + 1)` is `Some`, test a second adjacent candidate.
            if (intersects_range(next, range) || next.end == range.start || range.end == next.start)
                && first.is_some()
            {
                // When: the next range touches/intersects and `first.is_some()`, return both.
                return (first, Some(idx + 1));
            }
        }
        (first, None)
    }

    pub fn sort_if_needed(&mut self) {
        if self.needs_sort {
            self.ranges.sort_by_key(|range| range.start);
            self.needs_sort = false;
        }
    }

    fn binary_search_ranges(&self, range: &Range<T>) -> Result<usize, usize> {
        self.ranges.binary_search_by(|stored| {
            if range.start >= stored.start && range.end <= stored.end {
                Ordering::Equal
            } else if range.start < stored.start {
                // When: containment is false and `range.start < stored.start`, search
                // lower indices.
                Ordering::Greater
            } else if range.end > stored.end {
                // When: containment/start-before are false and `range.end > stored.end`,
                // search higher.
                Ordering::Less
            } else {
                // When: the ordered half-open range relations are inconsistent, the state is invalid.
                unreachable!()
            }
        })
    }

    fn insertion_point(&self, range: &Range<T>) -> usize {
        if self.needs_sort {
            panic!("rangeset needs sorting");
        }

        match self.binary_search_ranges(range) {
            Ok(idx) => idx,
            Err(idx) => idx,
        }
    }

    /// Returns an iterator over the ranges that comprise the set
    pub fn iter(&self) -> impl Iterator<Item = &Range<T>> {
        self.ranges.iter()
    }

    /// Returns an iterator over all of the contained values.
    /// Take care when the range is very large!
    pub fn iter_values<'a>(&'a self) -> impl Iterator<Item = T> + 'a {
        self.ranges.iter().flat_map(|stored| num::range(stored.start, stored.end))
    }
}

#[cfg(test)]
#[path = "rangeset_tests.rs"]
mod rangeset_tests;
