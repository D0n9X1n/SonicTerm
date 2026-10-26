//! Histogram bucket, cumulative arithmetic and atomic snapshot operations.

use super::*;

impl HistogramUnit {
    pub(super) fn bounds(self) -> &'static [u64] {
        match self {
            Self::Millis => &MILLIS_BOUNDS,
            Self::Micros => &MICROS_BOUNDS,
        }
    }

    pub(super) fn micros_per_unit(self) -> u64 {
        match self {
            Self::Millis => 1_000,
            Self::Micros => 1,
        }
    }

    pub(super) fn suffix(self) -> &'static str {
        match self {
            Self::Millis => "ms",
            Self::Micros => "us",
        }
    }

    /// The bucket a value falls in; a value equal to a bound belongs to that bound's bucket.
    pub(super) fn bucket(self, value_us: u64) -> usize {
        let per_unit = self.micros_per_unit();
        let bounds = self.bounds();
        bounds.iter().position(|bound| value_us <= bound * per_unit).unwrap_or(bounds.len())
    }

    pub(super) fn bound_of(self, bucket: usize) -> Bound {
        let bounds = self.bounds();
        match bounds.get(bucket) {
            Some(bound) => Bound::AtMost(*bound),
            None => Bound::Above(bounds[bounds.len() - 1]),
        }
    }
}

impl Bound {
    /// `le_<unit>=N` or `gt_<unit>=N`, the field form a line prints.
    pub(super) fn field(self, suffix: &str) -> String {
        match self {
            Self::AtMost(bound) => format!("le_{suffix}={bound}"),
            Self::Above(bound) => format!("gt_{suffix}={bound}"),
        }
    }
}

impl Histogram {
    /// An empty histogram with `unit` buckets.
    pub(crate) const fn new(unit: HistogramUnit) -> Self {
        Self { unit, buckets: [0; BUCKETS], sum_us: 0 }
    }

    /// Record one duration, in microseconds.
    pub(crate) fn record_us(&mut self, value_us: u64) {
        self.buckets[self.unit.bucket(value_us)] += 1;
        self.sum_us = self.sum_us.saturating_add(value_us);
    }

    /// Recorded durations.
    pub(crate) fn count(&self) -> u64 {
        self.buckets.iter().sum()
    }

    /// The exact sum of the recorded microseconds.
    pub(crate) fn sum_us(&self) -> u64 {
        self.sum_us
    }

    /// A microsecond histogram from a renderer's bucket `counts` (overflow last) and exact sum.
    pub(crate) fn from_micros(counts: &[u64], sum_us: u64) -> Self {
        let mut histogram = Self::new(HistogramUnit::Micros);
        for (slot, count) in histogram.buckets.iter_mut().zip(counts) {
            *slot = *count;
        }
        histogram.sum_us = sum_us;
        histogram
    }

    /// A millisecond histogram from a renderer's bucket `counts` (overflow last) and exact sum.
    pub(crate) fn from_millis(counts: &[u64], sum_us: u64) -> Self {
        let mut histogram = Self::new(HistogramUnit::Millis);
        for (slot, count) in histogram.buckets.iter_mut().zip(counts) {
            *slot = *count;
        }
        histogram.sum_us = sum_us;
        histogram
    }

    /// The unit, bounds, used bucket counts (overflow last) and exact sum, borrowed.
    pub(super) fn buckets(&self) -> HistogramBuckets<'_> {
        let bounds = self.unit.bounds();
        HistogramBuckets {
            unit: self.unit.suffix(),
            bounds,
            counts: &self.buckets[..=bounds.len()],
            sum_us: self.sum_us,
        }
    }

    /// The bound of the highest bucket holding a value; `None` when empty.
    pub(crate) fn max(&self) -> Option<Bound> {
        self.buckets.iter().rposition(|count| *count > 0).map(|bucket| self.unit.bound_of(bucket))
    }

    /// The bound of the bucket holding the 95th percentile; `None` when empty.
    pub(crate) fn p95(&self) -> Option<Bound> {
        let rank = self.count().saturating_mul(95).div_ceil(100);
        if rank == 0 {
            // When: `rank` is 0, the histogram is empty, so it has no p95.
            return None;
        }
        let mut cumulative = 0;
        self.buckets
            .iter()
            .position(|count| {
                cumulative += count;
                cumulative >= rank
            })
            .map(|bucket| self.unit.bound_of(bucket))
    }

    /// The histogram's fields on a line: buckets, sum, p95 and max bounds; empty when no events.
    pub(crate) fn line_fields(&self, name: &str) -> String {
        let (Some(p95), Some(max)) = (self.p95(), self.max()) else {
            // When: `p95` or `max` is None, the histogram is empty and contributes no fields.
            return String::new();
        };
        let suffix = self.unit.suffix();
        let used = &self.buckets[..=self.unit.bounds().len()];
        let buckets: Vec<String> = used.iter().map(ToString::to_string).collect();
        format!(
            "{name}_{suffix}=[{}] {name}_sum_us={} {name}_p95_{} {name}_max_{}",
            buckets.join(","),
            self.sum_us,
            p95.field(suffix),
            max.field(suffix)
        )
    }

    /// Add `other`'s counts, as a closed window's totals join `closed_windows`.
    pub(super) fn add(&mut self, other: &Self) {
        for (slot, count) in self.buckets.iter_mut().zip(other.buckets) {
            *slot += count;
        }
        self.sum_us += other.sum_us;
    }

    /// What was recorded since `earlier`, a snapshot of this same cumulative histogram.
    pub(crate) fn delta_since(&self, earlier: &Self) -> Self {
        let mut buckets = [0; BUCKETS];
        for (slot, (later, before)) in
            buckets.iter_mut().zip(self.buckets.iter().zip(&earlier.buckets))
        {
            *slot = later.saturating_sub(*before);
        }
        Self { unit: self.unit, buckets, sum_us: self.sum_us.saturating_sub(earlier.sum_us) }
    }
}

impl AtomicHistogram {
    /// An empty shared histogram with `unit` buckets.
    pub(crate) fn new(unit: HistogramUnit) -> Self {
        Self {
            unit,
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_us: AtomicU64::new(0),
        }
    }

    /// Record one duration, in microseconds.
    // Ordering: buckets and sum_us use Relaxed; they are statistics and order nothing.
    pub(crate) fn record_us(&self, value_us: u64) {
        self.buckets[self.unit.bucket(value_us)].fetch_add(1, Ordering::Relaxed);
        self.sum_us.fetch_add(value_us, Ordering::Relaxed);
    }

    /// Add a thread-local histogram's counts, as a dispatch scope drains its tally.
    // Ordering: shared buckets and sum_us use Relaxed; they are statistics and order nothing.
    pub(super) fn add(&self, local: &Histogram) {
        for (shared, count) in self.buckets.iter().zip(local.buckets) {
            shared.fetch_add(count, Ordering::Relaxed);
        }
        self.sum_us.fetch_add(local.sum_us, Ordering::Relaxed);
    }

    /// The cumulative counts, read field by field; a concurrent record may land in either read.
    // Ordering: buckets and sum_us load Relaxed; a snapshot is observational and orders nothing.
    pub(crate) fn snapshot(&self) -> Histogram {
        Histogram {
            unit: self.unit,
            buckets: std::array::from_fn(|bucket| self.buckets[bucket].load(Ordering::Relaxed)),
            sum_us: self.sum_us.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
#[path = "frame_counters_histogram_tests.rs"]
mod frame_counters_histogram_tests;
