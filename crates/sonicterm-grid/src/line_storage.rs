use super::*;

impl LineStorage {
    /// Build a `Cluster` storage from a flat slice, collapsing runs of equal
    /// cells. Always succeeds; for an all-distinct slice the result is the
    /// same length as the input and offers no saving (callers can check that
    /// via [`Self::approx_byte_size`]).
    pub fn cluster_from_flat(cells: &[Cell]) -> Self {
        let mut clusters: Vec<Cluster> = Vec::new();
        for cell in cells {
            match clusters.last_mut() {
                Some(last) if &last.cell == cell => last.count += 1,
                _ => clusters.push(Cluster { cell: cell.clone(), count: 1 }),
            }
        }
        LineStorage::Cluster(clusters)
    }

    /// Logical length (number of cells the line presents to its consumer).
    pub fn len(&self) -> usize {
        match self {
            LineStorage::Flat(cells) => cells.len(),
            LineStorage::Cluster(clusters) => clusters.iter().map(|cluster| cluster.count).sum(),
            LineStorage::Trimmed { len, .. } => *len,
        }
    }

    /// Return whether the storage contains no logical cells.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Approximate byte footprint of the storage payload (excluding the
    /// enum discriminant): the stored items, whatever the form.
    pub fn approx_byte_size(&self) -> usize {
        match self {
            LineStorage::Flat(cells) | LineStorage::Trimmed { cells, .. } => {
                cells.len() * std::mem::size_of::<Cell>()
            }
            LineStorage::Cluster(clusters) => clusters.len() * std::mem::size_of::<Cluster>(),
        }
    }

    /// Approximate reserved heap payload bytes for the inner storage Vec.
    /// Unlike [`Self::approx_byte_size`], this uses `Vec::capacity()` so
    /// reporting code counts bytes actually reserved from the allocator.
    pub fn approx_capacity_byte_size(&self) -> usize {
        match self {
            LineStorage::Flat(cells) | LineStorage::Trimmed { cells, .. } => {
                cells.capacity() * std::mem::size_of::<Cell>()
            }
            LineStorage::Cluster(clusters) => clusters.capacity() * std::mem::size_of::<Cluster>(),
        }
    }

    /// Heap bytes held by cells' rare-attribute boxes.
    ///
    /// [`Cell`] keeps hyperlink ids, grapheme extras, and non-default
    /// underline metadata behind an `Option<Box<FatAttributes>>`, so a linked
    /// cell costs its inline 24 bytes *plus* a 40-byte heap allocation.
    /// [`Self::approx_capacity_byte_size`] multiplies capacity by the inline
    /// size and therefore counts only the pointer, never what it points at —
    /// a row of linked cells under-reports by 40 bytes per cell, which is
    /// larger than the figure it does report.
    ///
    /// The box is not the whole cost. `FatAttributes::extras` is itself an
    /// `Option<Box<str>>` holding a cell's trailing zero-width codepoints, up
    /// to [`MAX_CELL_EXTRAS_BYTES`](crate::grid::MAX_CELL_EXTRAS_BYTES) of
    /// them, in a second allocation the box's own size does not describe.
    /// Ordinary output reaches it: any combining mark or ZWJ emoji appends
    /// there. Counting only `size_of::<FatAttributes>()` under-reports a grid
    /// of accented or emoji text by up to **1.99x**, so the payload is
    /// measured per cell rather than assumed small.
    ///
    /// Counted per *stored* cell rather than per logical column: in cluster
    /// form a run of N identical linked cells holds one `Cell`, hence one box.
    /// Multiplying by the logical length would over-report a long link span by
    /// the run length.
    ///
    /// Walks the row, so it is O(stored cells). Callers on a per-frame path
    /// should not use it; it exists for the retention sampling pass, which
    /// runs on an interval rather than per wake or per frame. That cadence is
    /// enforced by the caller — this figure is uncached, so each call pays the
    /// full walk.
    pub fn fat_attribute_bytes(&self) -> usize {
        let cell_bytes = |cell: &Cell| {
            if !cell.has_fat() {
                // When: `cell.has_fat()` is false, the cell owns no rare-attribute allocation.
                return 0;
            }
            // The box itself, plus the separate allocation behind its
            // `extras` pointer. `size_of::<FatAttributes>()` counts the
            // `Box<str>` fat pointer, never the bytes it points at.
            std::mem::size_of::<FatAttributes>() + cell.extras().map_or(0, str::len)
        };
        match self {
            // A trimmed row stores its fill once, so a rare-attribute fill counts once.
            LineStorage::Flat(cells) | LineStorage::Trimmed { cells, .. } => {
                cells.iter().map(cell_bytes).sum()
            }
            LineStorage::Cluster(clusters) => {
                clusters.iter().map(|cluster| cell_bytes(&cluster.cell)).sum()
            }
        }
    }

    // Cluster-aware accessors. These cover every operation a `Line` caller
    // needs without forcing the storage back to Flat, which is what makes
    // compressed scrollback worth keeping compressed.

    /// `true` if storage is currently in cluster (RLE) form.
    pub fn is_cluster(&self) -> bool {
        matches!(self, LineStorage::Cluster(_))
    }

    /// `true` if storage is currently in flat (dense `Vec<Cell>`) form.
    pub fn is_flat(&self) -> bool {
        matches!(self, LineStorage::Flat(_))
    }

    /// `true` if storage is currently in trimmed (prefix plus fill) form.
    pub fn is_trimmed(&self) -> bool {
        matches!(self, LineStorage::Trimmed { .. })
    }

    /// Expand a trimmed row to `Flat` in its own buffer: one exact reserve, then the fill to the
    /// logical width. No-op for any other form.
    pub(super) fn expand_trimmed(&mut self) {
        let LineStorage::Trimmed { cells, len } = self else {
            // When: the storage is not `Trimmed`, there is nothing to expand.
            return;
        };
        let len = *len;
        let mut cells = std::mem::take(cells);
        if let Some(fill) = cells.last().cloned() {
            cells.reserve_exact(len - cells.len());
            cells.resize(len, fill);
        }
        *self = LineStorage::Flat(cells);
    }

    /// Shorten a trimmed row to `new_len`, below its length and above zero. It stays trimmed
    /// while two fill columns remain; otherwise its buffer becomes the `Flat` row, with no
    /// allocation.
    pub(super) fn truncate_trimmed(&mut self, new_len: usize) {
        let LineStorage::Trimmed { cells, len } = self else {
            // When: the storage is not `Trimmed`, the caller's own path truncates it.
            return;
        };
        let stored = cells.len() - 1;
        if new_len >= stored + MIN_TRIMMED_FILL_COLUMNS {
            // When: `new_len >= stored + MIN_TRIMMED_FILL_COLUMNS`, two fill columns remain; only `len` shrinks.
            *len = new_len;
            return;
        }
        let mut cells = std::mem::take(cells);
        // At `stored + 1` the buffer already holds exactly `new_len` cells, the fill last.
        cells.truncate(new_len);
        shrink_vec_if_excessive(&mut cells);
        *self = LineStorage::Flat(cells);
    }

    /// Lengthen a trimmed row to `new_len`, padding with `fill`. A pad equal to the stored fill
    /// only widens the row; any other pad expands it to `Flat` with one exact reserve.
    pub(super) fn grow_trimmed(&mut self, new_len: usize, fill: Cell) {
        let LineStorage::Trimmed { cells, len } = self else {
            // When: the storage is not `Trimmed`, the caller's own path grows it.
            return;
        };
        if cells.last() == Some(&fill) {
            // When: the pad is the stored fill, the row reads correctly with a wider width.
            *len = new_len;
            return;
        }
        let old_len = *len;
        let mut cells = std::mem::take(cells);
        if let Some(stored_fill) = cells.last().cloned() {
            cells.reserve_exact(new_len - cells.len());
            cells.resize(old_len, stored_fill);
        }
        cells.resize(new_len, fill);
        *self = LineStorage::Flat(cells);
    }

    /// Force the storage to `Flat`. No-op if already flat.
    #[allow(clippy::wrong_self_convention)]
    pub fn to_flat(&mut self) {
        self.expand_trimmed();
        if let LineStorage::Cluster(clusters) = self {
            let total: usize = clusters.iter().map(|cluster| cluster.count).sum();
            let mut flat = Vec::with_capacity(total);
            for cluster in clusters.iter() {
                for _ in 0..cluster.count {
                    flat.push(cluster.cell.clone());
                }
            }
            *self = LineStorage::Flat(flat);
        }
    }

    /// Get the cell at logical column `idx`, materializing from cluster if
    /// needed. Returns `None` if out of range.
    pub fn get(&self, idx: usize) -> Option<Cell> {
        match self {
            LineStorage::Flat(cells) => cells.get(idx).cloned(),
            LineStorage::Trimmed { cells, len } => {
                // storage is `Trimmed`, columns past the prefix read as its fill.
                (idx < *len).then(|| cells[idx.min(cells.len() - 1)].clone())
            }
            LineStorage::Cluster(clusters) => {
                // When: storage is `Cluster`, locate `idx` by accumulating run lengths.
                let mut off = 0;
                for cluster in clusters {
                    if idx < off + cluster.count {
                        // When: `idx < off + cluster.count`, this cluster contains the requested cell.
                        return Some(cluster.cell.clone());
                    }
                    off += cluster.count;
                }
                None
            }
        }
    }

    /// Iterate over cells in `[start, end)`, cloning cells for a uniform return
    /// type across storage forms. `end` is clamped to `len()`; empty or reversed
    /// ranges yield no cells.
    pub fn get_range(&self, start: u16, end: u16) -> impl Iterator<Item = Cell> + '_ {
        let start = usize::from(start);
        let end = usize::from(end).min(self.len());
        if start >= end {
            // When: `start >= end`, the clamped range contains no cells.
            return StorageRangeIter::Empty;
        }

        match self {
            LineStorage::Flat(cells) => StorageRangeIter::Flat(cells[start..end].iter()),
            LineStorage::Cluster(clusters) => StorageRangeIter::cluster(clusters, start, end),
            LineStorage::Trimmed { cells, .. } => {
                let (prefix, fill, fill_remaining) = trimmed_window(cells, start, end);
                StorageRangeIter::Trimmed { prefix: prefix.iter(), fill, fill_remaining }
            }
        }
    }

    /// Set the cell at `idx`. Degrades to `Flat` on first write. Returns
    /// `true` if the index was in range.
    pub fn set(&mut self, idx: usize, cell: Cell) -> bool {
        self.to_flat();
        match self {
            LineStorage::Flat(cells) => {
                if let Some(slot) = cells.get_mut(idx) {
                    *slot = cell;
                    true
                } else {
                    // When: `cells.get_mut(idx)` is `None`, the requested index is out of range.
                    false
                }
            }
            _ => unreachable!("just flattened"),
        }
    }

    /// Append a cell to the right end. Degrades to Flat first.
    pub fn push(&mut self, cell: Cell) {
        self.to_flat();
        match self {
            LineStorage::Flat(cells) => cells.push(cell),
            _ => unreachable!("just flattened"),
        }
    }

    /// Truncate to `new_len`. No-op if already shorter or equal. Preserves
    /// the current storage form, except that a trimmed row left with fewer
    /// than two fill columns becomes `Flat`.
    pub fn truncate(&mut self, new_len: usize) {
        if new_len >= self.len() {
            // When: `new_len >= self.len()`, truncation would not shorten the storage.
            return;
        }
        if new_len == 0 {
            // When: `new_len == 0`, every form becomes an empty flat row.
            *self = LineStorage::Flat(Vec::new());
            return;
        }
        match self {
            LineStorage::Trimmed { .. } => self.truncate_trimmed(new_len),
            LineStorage::Flat(cells) => {
                cells.truncate(new_len);
                shrink_vec_if_excessive(cells);
            }
            LineStorage::Cluster(clusters) => {
                // When: storage is `Cluster`, trim whole runs and then the boundary run.
                let mut remaining = new_len;
                let mut keep = 0;
                for cluster in clusters.iter_mut() {
                    if remaining == 0 {
                        // When: `remaining == 0`, no later cluster contributes to the new length.
                        break;
                    }
                    if cluster.count <= remaining {
                        remaining -= cluster.count;
                        keep += 1;
                    } else {
                        // When: `cluster.count > remaining`, shorten this boundary cluster.
                        cluster.count = remaining;
                        remaining = 0;
                        keep += 1;
                    }
                }
                clusters.truncate(keep);
                shrink_vec_if_excessive(clusters);
            }
        }
    }

    /// Resize to `new_len`, padding the right with `fill` if growing.
    /// Preserves cluster form when the trailing cluster matches `fill`.
    pub fn resize(&mut self, new_len: usize, fill: Cell) {
        let cur = self.len();
        if new_len == cur {
            // When: `new_len == cur`, resizing would leave the logical length unchanged.
            return;
        }
        if new_len < cur {
            // When: `new_len < cur`, delegate the shrink to cluster-aware truncation.
            self.truncate(new_len);
            return;
        }
        let extra = new_len - cur;
        match self {
            LineStorage::Flat(cells) => cells.resize(cur + extra, fill),
            LineStorage::Cluster(clusters) => match clusters.last_mut() {
                Some(last) if last.cell == fill => last.count += extra,
                _ => clusters.push(Cluster { cell: fill, count: extra }),
            },
            LineStorage::Trimmed { .. } => self.grow_trimmed(new_len, fill),
        }
    }

    /// Drop all cells, leaving an empty Flat storage.
    pub fn clear(&mut self) {
        *self = LineStorage::Flat(Vec::new());
    }

    /// Iterate over all cells (cloned for uniform return type across forms).
    pub fn iter(&self) -> StorageIter<'_> {
        match self {
            LineStorage::Flat(cells) => StorageIter::Flat(cells.iter()),
            LineStorage::Cluster(clusters) => {
                StorageIter::Cluster { clusters: clusters.iter(), current: None, remaining: 0 }
            }
            LineStorage::Trimmed { cells, len } => {
                let (prefix, fill, fill_remaining) = trimmed_window(cells, 0, *len);
                StorageIter::Trimmed { prefix: prefix.iter(), fill, fill_remaining }
            }
        }
    }

    /// Iterate mutably. Forces Flat first so callers always get `&mut Cell`.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Cell> {
        self.to_flat();
        match self {
            LineStorage::Flat(cells) => cells.iter_mut(),
            _ => unreachable!("just flattened"),
        }
    }

    /// Fill cells in `[start, end)` with `cell`. Degrades to Flat. `end` is
    /// clamped to `len()`; an empty range is a no-op.
    pub fn fill_range(&mut self, start: usize, end: usize, cell: Cell) {
        let len = self.len();
        let end = end.min(len);
        if start >= end {
            // When: `start >= end`, the clamped fill range contains no cells.
            return;
        }
        self.to_flat();
        match self {
            LineStorage::Flat(cells) => {
                for slot in &mut cells[start..end] {
                    *slot = cell.clone();
                }
            }
            _ => unreachable!("just flattened"),
        }
    }

    /// Clone the range `src` to the position starting at `dst`. Same
    /// semantics as `slice::copy_within` (overlap-safe) but uses `Clone`
    /// because `Cell` is not `Copy`. Degrades to Flat.
    pub fn copy_within(&mut self, src: std::ops::Range<usize>, dst: usize) {
        self.to_flat();
        match self {
            LineStorage::Flat(cells) => {
                // When: storage is `Flat`, clone the source so overlapping writes are safe.
                let snapshot: Vec<Cell> = cells[src.clone()].to_vec();
                let len = snapshot.len();
                for (offset, cell) in snapshot.into_iter().enumerate() {
                    cells[dst + offset] = cell;
                }
                let _ = len; // silence unused warning if optimizer drops it
            }
            _ => unreachable!("just flattened"),
        }
    }

    /// Try to re-compress into Cluster form.
    ///
    /// Switches only when Cluster storage is smaller than the current Flat
    /// storage. Returns `true` if storage changed.
    pub fn try_compress(&mut self) -> bool {
        let flat = match self {
            LineStorage::Flat(cells) => cells,
            LineStorage::Cluster(_) | LineStorage::Trimmed { .. } => {
                // When: storage is already `Cluster` or `Trimmed`, compression does not apply.
                return false;
            }
        };
        if flat.is_empty() {
            // When: `flat.is_empty()`, there are no cells to compress.
            return false;
        }
        let candidate = Self::cluster_from_flat(flat);
        if candidate.approx_byte_size() < self.approx_byte_size() {
            *self = candidate;
            true
        } else {
            // When: `candidate` is not smaller than `self`, retain flat storage.
            false
        }
    }
}

/// Transparent iterator over either `LineStorage` form (clones cells for a
/// uniform return type).
pub enum StorageIter<'a> {
    Flat(std::slice::Iter<'a, Cell>),
    Cluster {
        clusters: std::slice::Iter<'a, Cluster>,
        current: Option<&'a Cell>,
        remaining: usize,
    },
    /// A trimmed row's prefix, then its fill `fill_remaining` times.
    Trimmed {
        prefix: std::slice::Iter<'a, Cell>,
        fill: &'a Cell,
        fill_remaining: usize,
    },
}

impl<'a> Iterator for StorageIter<'a> {
    type Item = Cell;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            StorageIter::Flat(it) => it.next().cloned(),
            StorageIter::Trimmed { prefix, fill, fill_remaining } => {
                // iterating `Trimmed` storage, the prefix comes first, then the fill.
                prefix.next().cloned().or_else(|| {
                    (*fill_remaining > 0).then(|| {
                        *fill_remaining -= 1;
                        (*fill).clone()
                    })
                })
            }
            StorageIter::Cluster { clusters, current, remaining } => {
                if *remaining == 0 {
                    let cluster = clusters.next()?;
                    *current = Some(&cluster.cell);
                    *remaining = cluster.count;
                }
                *remaining -= 1;
                current.cloned()
            }
        }
    }
}

/// Transparent range iterator over either `LineStorage` form (clones cells for
/// a uniform return type).
pub enum StorageRangeIter<'a> {
    Empty,
    Flat(std::slice::Iter<'a, Cell>),
    /// A window of a trimmed row: part of its prefix, then its fill `fill_remaining` times.
    Trimmed {
        prefix: std::slice::Iter<'a, Cell>,
        fill: &'a Cell,
        fill_remaining: usize,
    },
    Cluster {
        clusters: std::slice::Iter<'a, Cluster>,
        current: Option<&'a Cell>,
        remaining_in_cluster: usize,
        remaining_total: usize,
    },
}

impl<'a> StorageRangeIter<'a> {
    fn cluster(clusters: &'a [Cluster], start: usize, end: usize) -> Self {
        let total = end - start;
        let mut off = 0;
        let mut idx = 0;
        while let Some(cluster) = clusters.get(idx) {
            let next_off = off + cluster.count;
            if start < next_off {
                // When: `start < next_off`, this cluster contains the range's first cell.
                let skip_in_cluster = start - off;
                return StorageRangeIter::Cluster {
                    clusters: clusters[idx + 1..].iter(),
                    current: Some(&cluster.cell),
                    remaining_in_cluster: (cluster.count - skip_in_cluster).min(total),
                    remaining_total: total,
                };
            }
            off = next_off;
            idx += 1;
        }
        StorageRangeIter::Empty
    }
}

impl<'a> Iterator for StorageRangeIter<'a> {
    type Item = Cell;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            StorageRangeIter::Empty => None,
            StorageRangeIter::Flat(it) => it.next().cloned(),
            StorageRangeIter::Trimmed { prefix, fill, fill_remaining } => {
                // iterating a `Trimmed` window, the prefix comes first, then the fill.
                prefix.next().cloned().or_else(|| {
                    (*fill_remaining > 0).then(|| {
                        *fill_remaining -= 1;
                        (*fill).clone()
                    })
                })
            }
            StorageRangeIter::Cluster {
                clusters,
                current,
                remaining_in_cluster,
                remaining_total,
            } => {
                // When: iterating `Cluster` storage, honor both range and run boundaries.
                if *remaining_total == 0 {
                    // When: `*remaining_total == 0`, the requested range is exhausted.
                    return None;
                }
                if *remaining_in_cluster == 0 {
                    let cluster = clusters.next()?;
                    *current = Some(&cluster.cell);
                    *remaining_in_cluster = cluster.count.min(*remaining_total);
                }
                *remaining_in_cluster -= 1;
                *remaining_total -= 1;
                current.cloned()
            }
        }
    }
}

#[cfg(test)]
#[path = "line_storage_tests.rs"]
mod line_storage_tests;
