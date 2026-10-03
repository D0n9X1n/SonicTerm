//! Row storage in three forms, read through one transparent API.
//!
//! A terminal row frequently ends in a long run of identical cells (the
//! blanks after a short prompt), and is sometimes uniform throughout (an empty
//! alt-screen page). Storing every column as a 24-byte `Cell` wastes memory
//! once the row has left the screen.
//!
//! `LineStorage` has three forms:
//!
//! * `Flat(Vec<Cell>)` — the dense form. Every row is Flat while the parser
//!   writes it, and any in-place edit of another form expands it to Flat first.
//! * `Cluster(Vec<Cluster>)` — RLE runs of identical cells. `Grid` stores a
//!   uniform row ejected into scrollback as one cluster.
//! * `Trimmed { cells, len }` — the cells up to the last one that differs from
//!   the row's fill (its last cell), then the fill once, plus the logical
//!   width. `Grid` stores other ejected rows this way when that saves at least
//!   a quarter of the row and 256 bytes.
//!
//! `Line` exposes a transparent `iter`/`get`/`len`/`set` API, and equality,
//! hashing and debug output that ignore the form, so callers never see which
//! form a row is in.

use sonicterm_types::cell::{Cell, CellFlags, FatAttributes};

const MIN_EXACT_HALF_COMPACTION_ITEMS: usize = 1024;
/// Fill columns a trimmed row must leave; one would save nothing over storing it.
const MIN_TRIMMED_FILL_COLUMNS: usize = 2;
/// Bytes a trim must save; smaller rows keep their plain form.
const MIN_TRIM_SAVING_BYTES: usize = 256;

fn shrink_vec_if_excessive<T>(items: &mut Vec<T>) {
    let len = items.len();
    let capacity = items.capacity();
    let twice_len = len.saturating_mul(2);
    let exact_half_is_material =
        capacity == twice_len && capacity.saturating_sub(len) >= MIN_EXACT_HALF_COMPACTION_ITEMS;
    if capacity > len && (len == 0 || capacity > twice_len || exact_half_is_material) {
        items.shrink_to_fit();
    }
}

/// A run of `count` consecutive cells that are byte-identical to `cell`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub cell: Cell,
    pub count: usize,
}

/// Two-form storage for a row of cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineStorage {
    /// RLE form. Invariant: every `Cluster.count > 0`, no two adjacent
    /// clusters are equal-cell (otherwise they'd be merged). Sum of counts
    /// equals the logical line length.
    Cluster(Vec<Cluster>),
    /// Dense form. Length equals the logical line length.
    Flat(Vec<Cell>),
    /// History form. `cells` holds the stored prefix, then the row's fill cell
    /// as its last element; every column from `cells.len() - 1` to `len` reads
    /// as that fill. Invariant: at least two fill columns, a fill that is
    /// neither `WIDE` nor `WIDE_CONT`, and a prefix whose last cell differs
    /// from the fill. Zero length is always `Flat`.
    Trimmed {
        /// The stored prefix, then the fill once.
        cells: Vec<Cell>,
        /// The logical width.
        len: usize,
    },
}

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
    fn expand_trimmed(&mut self) {
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
    fn truncate_trimmed(&mut self, new_len: usize) {
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
    fn grow_trimmed(&mut self, new_len: usize, fill: Cell) {
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

/// The part of a trimmed row's stored `cells` that `[start, end)` covers, its fill, and how many
/// fill columns of the window lie past the prefix. `end` must not exceed the logical width.
/// Whether `cell` equals `fill`, deciding on the plain fields when neither carries a rare-attribute box.
///
/// Equal to `cell == fill`, but the eject scans call it on every trailing blank of a history row, and
/// the derived comparison is not inlined across crates because it may compare the boxed attributes.
#[inline]
pub(crate) fn same_as_fill(cell: &Cell, fill: &Cell) -> bool {
    if !cell.has_fat() && !fill.has_fat() {
        // When: neither cell has rare attributes, the plain fields decide equality.
        return cell.ch == fill.ch
            && cell.fg == fill.fg
            && cell.bg == fill.bg
            && cell.flags == fill.flags;
    }
    cell == fill
}

fn trimmed_window(cells: &[Cell], start: usize, end: usize) -> (&[Cell], &Cell, usize) {
    let stored = cells.len() - 1;
    let prefix = &cells[start.min(stored)..end.min(stored)];
    (prefix, &cells[stored], end.saturating_sub(start.max(stored)))
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

/// A line of cells with transparent flat, cluster or trimmed storage.
#[derive(Clone)]
pub struct Line {
    storage: LineStorage,
    /// Last content sequence plus the automatic-wrap provenance bit.
    ///
    /// Keeping both with the row lets change and logical-line identity survive
    /// moves between visible storage and scrollback without enlarging `Line`.
    content_seq_and_flags: u64,
}

const SOFT_WRAPPED_FROM_PREVIOUS: u64 = 1 << 63;
pub(crate) const MAX_LINE_CONTENT_SEQ: u64 = SOFT_WRAPPED_FROM_PREVIOUS - 1;

/// Equality reads cells through the transparent iterator, so a compressed or trimmed row equals
/// its flat original, as its `Hash` already does.
impl PartialEq for Line {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self.soft_wrapped_from_previous() == other.soft_wrapped_from_previous()
            && self.iter().eq(other.iter())
    }
}

/// Debug output lists the logical cells, whatever form stores them.
impl std::fmt::Debug for Line {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Line")
            .field("cells", &self.iter().collect::<Vec<_>>())
            .field("content_seq", &self.content_seq())
            .field("soft_wrapped_from_previous", &self.soft_wrapped_from_previous())
            .finish()
    }
}

impl Eq for Line {}

impl Line {
    /// Build a flat line of `len` clones of `fill`.
    pub fn flat_filled(len: usize, fill: Cell) -> Self {
        Self { storage: LineStorage::Flat(vec![fill; len]), content_seq_and_flags: 0 }
    }

    /// Build directly from a `Vec<Cell>` in flat form.
    pub fn from_flat(cells: Vec<Cell>) -> Self {
        Self { storage: LineStorage::Flat(cells), content_seq_and_flags: 0 }
    }

    /// Build directly from clusters. The caller is responsible for the
    /// "no adjacent equal cells" invariant; in debug builds we assert it.
    pub fn from_clusters(clusters: Vec<Cluster>) -> Self {
        debug_assert!(
            clusters.windows(2).all(|pair| pair[0].cell != pair[1].cell),
            "adjacent clusters must differ"
        );
        debug_assert!(clusters.iter().all(|cluster| cluster.count > 0));
        Self { storage: LineStorage::Cluster(clusters), content_seq_and_flags: 0 }
    }

    pub(crate) fn content_seq(&self) -> u64 {
        self.content_seq_and_flags & MAX_LINE_CONTENT_SEQ
    }

    pub(crate) fn set_content_seq(&mut self, seq: u64) {
        let flags = self.content_seq_and_flags & SOFT_WRAPPED_FROM_PREVIOUS;
        self.content_seq_and_flags = flags | seq.min(MAX_LINE_CONTENT_SEQ);
    }

    /// Whether automatic terminal wrapping made this row continue its predecessor.
    #[must_use]
    pub fn soft_wrapped_from_previous(&self) -> bool {
        self.content_seq_and_flags & SOFT_WRAPPED_FROM_PREVIOUS != 0
    }

    pub(crate) fn set_soft_wrapped_from_previous(&mut self, wrapped: bool) -> bool {
        if self.soft_wrapped_from_previous() == wrapped {
            // When: the retained wrap bit already equals `wrapped`, preserve the sequence word unchanged.
            return false;
        }
        if wrapped {
            self.content_seq_and_flags |= SOFT_WRAPPED_FROM_PREVIOUS;
        } else {
            // When: `wrapped` is false, clear only the high provenance bit and preserve the content sequence.
            self.content_seq_and_flags &= MAX_LINE_CONTENT_SEQ;
        }
        true
    }

    /// Logical cell count.
    pub fn len(&self) -> usize {
        self.storage.len()
    }

    /// Return whether the line contains no logical cells.
    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    /// Approximate payload byte size.
    pub fn approx_byte_size(&self) -> usize {
        self.storage.approx_byte_size()
    }

    /// Approximate reserved heap payload bytes for this line's storage.
    pub fn approx_capacity_byte_size(&self) -> usize {
        self.storage.approx_capacity_byte_size()
    }

    /// Heap bytes held by this row's rare-attribute boxes.
    ///
    /// See [`LineStorage::fat_attribute_bytes`]. Separate from
    /// [`Self::approx_capacity_byte_size`] rather than folded into it because
    /// that function is `capacity`-based and O(1); this one walks the row.
    pub fn fat_attribute_bytes(&self) -> usize {
        self.storage.fat_attribute_bytes()
    }

    /// Release all unused inner storage capacity.
    pub(crate) fn shrink_capacity_to_fit(&mut self) {
        match &mut self.storage {
            LineStorage::Flat(cells) | LineStorage::Trimmed { cells, .. } => cells.shrink_to_fit(),
            LineStorage::Cluster(clusters) => clusters.shrink_to_fit(),
        }
    }

    /// Returns `true` if the line is currently in cluster form.
    pub fn is_clustered(&self) -> bool {
        matches!(self.storage, LineStorage::Cluster(_))
    }

    /// Returns `true` if the line is currently in trimmed (prefix plus fill) form.
    pub fn is_trimmed(&self) -> bool {
        self.storage.is_trimmed()
    }

    /// Store this flat row as its cells up to the last one that differs from its fill (its last
    /// cell), then the fill once.
    ///
    /// Trims only when that leaves at least two fill columns, saves at least a quarter of the row
    /// and 256 bytes, and the fill is not half of a wide pair. The prefix and the fill move into a
    /// new buffer of exact capacity, so no rare-attribute box is cloned. Returns the released
    /// buffer, which now holds only fill copies, for the caller to reuse; `None` leaves the row
    /// unchanged.
    pub fn try_trim(&mut self) -> Option<Vec<Cell>> {
        let LineStorage::Flat(cells) = &mut self.storage else {
            // When: the row is not `Flat`, it is already in a compact form.
            return None;
        };
        let row_len = cells.len();
        let stored = {
            let fill = cells.last()?;
            if fill.flags.intersects(CellFlags::WIDE | CellFlags::WIDE_CONT) {
                // When: the fill is half of a wide pair, repeating it would split the pair.
                return None;
            }
            cells.iter().rposition(|cell| !same_as_fill(cell, fill)).map_or(0, |index| index + 1)
        };
        let fill_columns = row_len - stored;
        let cell_bytes = std::mem::size_of::<Cell>();
        let saving = fill_columns.saturating_sub(1) * cell_bytes;
        if fill_columns < MIN_TRIMMED_FILL_COLUMNS
            || saving * 4 < row_len * cell_bytes
            || saving < MIN_TRIM_SAVING_BYTES
        {
            // When: `fill_columns` is under two or `saving` is under a quarter of the row or 256 bytes, keep `Flat`.
            return None;
        }
        let mut trimmed = Vec::with_capacity(stored + 1);
        trimmed.extend(cells.drain(..stored));
        trimmed.push(cells.pop()?);
        let released = std::mem::take(cells);
        self.storage = LineStorage::Trimmed { cells: trimmed, len: row_len };
        Some(released)
    }

    /// Get the cell at logical column `idx`, if in range.
    pub fn get(&self, idx: usize) -> Option<&Cell> {
        match &self.storage {
            LineStorage::Flat(cells) => cells.get(idx),
            LineStorage::Trimmed { cells, len } => {
                // storage is `Trimmed`, columns past the prefix read as its fill.
                (idx < *len).then(|| &cells[idx.min(cells.len() - 1)])
            }
            LineStorage::Cluster(clusters) => {
                // When: storage is `Cluster`, locate `idx` by accumulating run lengths.
                let mut off = 0;
                for cluster in clusters {
                    if idx < off + cluster.count {
                        // When: `idx < off + cluster.count`, this cluster contains the requested cell.
                        return Some(&cluster.cell);
                    }
                    off += cluster.count;
                }
                None
            }
        }
    }

    /// Set the cell at logical column `idx`.
    ///
    /// Smart-degrade: when the storage is a single uniform
    /// Cluster and the new cell is byte-identical to the cluster's
    /// representative, this is a no-op and storage stays in Cluster form.
    /// Otherwise the storage is degraded to Flat before the write. This
    /// preserves the RAM win for the very common pattern of pty-output
    /// rewriting already-blank cells (cursor repositioning, repeated
    /// prompts, clearing a region that's already cleared).
    ///
    /// Multi-cluster split (the "punch a hole" optimisation) is
    /// intentionally not attempted here — the full-Flat fallback is
    /// always correct and is simpler. Returns `true` if `idx` was in
    /// range.
    pub fn set(&mut self, idx: usize, cell: Cell) -> bool {
        if idx >= self.len() {
            // When: `idx >= self.len()`, the requested logical column is out of range.
            return false;
        }
        // Smart-degrade fast path: same-cell write on a uniform Cluster
        // stays Cluster.
        if let Some(rep) = self.cluster_representative() {
            // When: `self.cluster_representative()` returns `Some(rep)`, compare before degrading.
            if rep == cell {
                // When: `rep == cell`, the write is already represented and remains clustered.
                return true;
            }
        }
        self.degrade_to_flat();
        match &mut self.storage {
            LineStorage::Flat(cells) => {
                cells[idx] = cell;
                true
            }
            _ => unreachable!("just degraded"),
        }
    }

    /// If the line is currently a single uniform Cluster (one entry),
    /// return a clone of its representative cell. `None` for Flat
    /// storage, empty storage, or multi-Cluster storage.
    pub fn cluster_representative(&self) -> Option<Cell> {
        match &self.storage {
            LineStorage::Cluster(clusters) if clusters.len() == 1 => Some(clusters[0].cell.clone()),
            _ => None,
        }
    }

    /// Fill cells in `[start, end)` with `cell`. `end` is clamped to
    /// `len()`; empty/reversed ranges are no-ops.
    ///
    /// Smart-degrade: matches [`Self::set`]'s policy. If the line
    /// is a single uniform Cluster whose representative equals `cell`,
    /// the write is a no-op and storage stays Cluster. Otherwise
    /// degrade to Flat then bulk-fill the range.
    pub fn fill_range(&mut self, start: usize, end: usize, cell: Cell) {
        let len = self.len();
        let end = end.min(len);
        if start >= end {
            // When: `start >= end`, the clamped fill range contains no cells.
            return;
        }
        if let Some(rep) = self.cluster_representative() {
            // When: `self.cluster_representative()` returns `Some(rep)`, compare before degrading.
            if rep == cell {
                // When: `rep == cell`, the fill is already represented and remains clustered.
                return;
            }
        }
        self.degrade_to_flat();
        match &mut self.storage {
            LineStorage::Flat(cells) => {
                for slot in &mut cells[start..end] {
                    *slot = cell.clone();
                }
            }
            _ => unreachable!("just degraded"),
        }
    }

    /// Force the storage to `Flat`. No-op if already flat. A trimmed row expands in its own
    /// buffer; a clustered row is rebuilt.
    #[inline]
    pub fn degrade_to_flat(&mut self) {
        if matches!(self.storage, LineStorage::Flat(_)) {
            // When: `matches!(self.storage, LineStorage::Flat(_))`, the write path needs no conversion.
            return;
        }
        self.storage.expand_trimmed();
        if let LineStorage::Cluster(clusters) = &self.storage {
            let total: usize = clusters.iter().map(|cluster| cluster.count).sum();
            let mut flat = Vec::with_capacity(total);
            for cluster in clusters {
                for _ in 0..cluster.count {
                    flat.push(cluster.cell.clone());
                }
            }
            self.storage = LineStorage::Flat(flat);
        }
    }

    /// Iterator over cells in logical order. Cluster-transparent: yields
    /// `&Cell` regardless of storage form without materializing the flat
    /// representation.
    ///
    /// This used to route through `as_flat_slice()`, which panicked on
    /// Cluster lines via `as_vec()` — so producing Cluster lines from
    /// scrollback eject was impossible without rewriting every downstream
    /// call site. The iterator now lazily walks either form
    /// and implements `DoubleEndedIterator` + `ExactSizeIterator` so the
    /// existing `iter().rev()` / `iter().len()` call sites (copy_mode,
    /// search, etc.) keep working unchanged.
    pub fn iter(&self) -> LineIter<'_> {
        self.iter_storage()
    }

    /// Transparent cluster-or-flat iterator. Same as [`Self::iter`] —
    /// kept as an explicit name for the hot paths that want
    /// to call it for clarity.
    pub fn iter_storage(&self) -> LineIter<'_> {
        match &self.storage {
            LineStorage::Flat(cells) => LineIter::Flat(cells.iter()),
            LineStorage::Cluster(clusters) => {
                let total: usize = clusters.iter().map(|cluster| cluster.count).sum();
                LineIter::new_cluster(clusters, total)
            }
            LineStorage::Trimmed { cells, len } => {
                let (prefix, fill, fill_remaining) = trimmed_window(cells, 0, *len);
                LineIter::Trimmed { prefix: prefix.iter(), fill, fill_remaining }
            }
        }
    }

    /// Iterator over cells in `[start, end)` returning `&Cell` references
    /// without cloning. `end` is clamped to `len()`. Empty / reversed
    /// ranges yield no cells. Replaces the removed `Index<Range<usize>>`
    /// impl: that one couldn't return a real `&[Cell]` slice from a
    /// Cluster line without materialising, so the trait surface was
    /// fundamentally incompatible with Cluster storage. Callers that
    /// truly need a `&[Cell]` slice should `as_flat_slice_after_materialise()`
    /// instead.
    pub fn get_range(&self, start: usize, end: usize) -> LineIter<'_> {
        let len = self.len();
        let end = end.min(len);
        if start >= end {
            // When: `start >= end`, the clamped range contains no cells.
            return LineIter::empty();
        }
        let take = end - start;
        match &self.storage {
            LineStorage::Flat(cells) => LineIter::Flat(cells[start..end].iter()),
            LineStorage::Cluster(clusters) => LineIter::cluster_range(clusters, start, take),
            LineStorage::Trimmed { cells, .. } => {
                let (prefix, fill, fill_remaining) = trimmed_window(cells, start, end);
                LineIter::Trimmed { prefix: prefix.iter(), fill, fill_remaining }
            }
        }
    }

    /// Materialise into a flat `Vec<Cell>` (cloning). Equivalent to
    /// `self.iter().cloned().collect()` but a hair faster for the cluster
    /// case because it pre-sizes.
    pub fn to_vec(&self) -> Vec<Cell> {
        let mut out = Vec::with_capacity(self.len());
        for cell in self.iter() {
            out.push(cell.clone());
        }
        out
    }

    /// Read-only access to the underlying storage form. Useful for tests
    /// and for the eventual `Grid` integration that wants to fast-path
    /// the cluster case.
    pub fn storage(&self) -> &LineStorage {
        &self.storage
    }

    // ----- Shim accessors for callers that still pass &Vec<Cell> / &[Cell]
    // around. These force the storage to Flat and expose the inner Vec
    // directly, so the lifetime chain
    // `&Grid → &VecDeque<Line> → &Line → &Vec<Cell>` works without copies.
    //
    // Forcing is not free: `Grid` compresses lines on scrollback eject
    // (`grid.rs`, `row.try_compress()`), so any line in history may be
    // Cluster and calling one of these decompresses it in place. Prefer the
    // cluster-aware accessors above on any path that walks scrollback.

    /// Borrow the underlying flat `Vec<Cell>`. **PANICS** on Cluster storage:
    /// there is no `Vec<Cell>` to lend without first materialising into
    /// Flat, which requires `&mut self`. Use [`Self::iter`] / [`Self::get`] /
    /// [`Self::get_range`] for read-only access that works for either form,
    /// or call [`Self::as_vec_mut`] / [`Self::degrade_to_flat`] first if a
    /// borrowed `Vec` reference is truly needed.
    ///
    /// `iter` / `Hash` / range-index used to route through this method and
    /// panic on Cluster lines. They now use cluster-transparent paths; this
    /// method remains for the few legitimately-flat-only callers (e.g.
    /// `as_flat_slice_mut` for mutation), but reading it on a Cluster line
    /// is a programming error.
    pub fn as_vec(&self) -> &Vec<Cell> {
        match &self.storage {
            LineStorage::Flat(cells) => cells,
            LineStorage::Cluster(_) | LineStorage::Trimmed { .. } => {
                unreachable!(
                    "Line::as_vec()/as_flat_slice() requires Flat storage, not Cluster or Trimmed; \
                     call iter()/get()/get_range() for transparent access"
                )
            }
        }
    }

    /// Mutably borrow the underlying flat `Vec<Cell>`. Degrades any Cluster
    /// storage to Flat first.
    pub fn as_vec_mut(&mut self) -> &mut Vec<Cell> {
        self.degrade_to_flat();
        match &mut self.storage {
            LineStorage::Flat(cells) => cells,
            _ => unreachable!("just degraded"),
        }
    }

    /// Borrow the cells as a slice (read-only).
    pub fn as_flat_slice(&self) -> &[Cell] {
        self.as_vec().as_slice()
    }

    /// Borrow the cells as a mutable slice. Degrades to Flat first.
    pub fn as_flat_slice_mut(&mut self) -> &mut [Cell] {
        self.as_vec_mut().as_mut_slice()
    }

    /// Resize without reflow, using `fill` for padding or a clipped wide lead while preserving clusters.
    pub fn resize(&mut self, new_len: usize, fill: Cell) {
        let cur = self.len();
        if new_len == cur {
            // When: `new_len == cur`, resizing would leave the logical length unchanged.
            return;
        }
        if new_len < cur {
            // When: `new_len < cur`, truncation may remove the continuation of the new trailing cell.
            self.truncate(new_len);
            match &mut self.storage {
                LineStorage::Trimmed { .. } => {
                    // When: storage is `Trimmed`, its last logical cell is the fill, never a `WIDE` lead to repair.
                }
                LineStorage::Flat(cells) => {
                    if let Some(edge) =
                        cells.last_mut().filter(|cell| cell.flags.contains(CellFlags::WIDE))
                    {
                        *edge = fill;
                    }
                }
                LineStorage::Cluster(clusters) => {
                    // When: storage is `Cluster`, repair only the final column so unaffected runs stay compressed.
                    let Some(edge) =
                        clusters.last_mut().filter(|run| run.cell.flags.contains(CellFlags::WIDE))
                    else {
                        // When: the last run has no WIDE lead, the truncated boundary needs no repair or materialization.
                        return;
                    };
                    edge.count -= 1;
                    if edge.count == 0 {
                        clusters.pop();
                    }
                    match clusters.last_mut() {
                        Some(last) if last.cell == fill => last.count += 1,
                        _ => clusters.push(Cluster { cell: fill, count: 1 }),
                    }
                }
            }
            return;
        }
        match &mut self.storage {
            LineStorage::Flat(cells) => cells.resize(new_len, fill),
            LineStorage::Cluster(clusters) => {
                let extra = new_len - cur;
                match clusters.last_mut() {
                    Some(last) if last.cell == fill => last.count += extra,
                    _ => clusters.push(Cluster { cell: fill, count: extra }),
                }
            }
            LineStorage::Trimmed { .. } => self.storage.grow_trimmed(new_len, fill),
        }
    }

    /// Truncate the line to `new_len`. No-op if already shorter or
    /// equal. Cluster-preserving: reduces the trailing cluster's run-len
    /// and drops any clusters past the new boundary.
    pub fn truncate(&mut self, new_len: usize) {
        let cur = self.len();
        if new_len >= cur {
            // When: `new_len >= cur`, truncation would not shorten the line.
            return;
        }
        if new_len == 0 {
            // When: `new_len == 0`, replace all storage with an empty flat line.
            self.storage = LineStorage::Flat(Vec::new());
            return;
        }
        match &mut self.storage {
            LineStorage::Flat(cells) => {
                cells.truncate(new_len);
                shrink_vec_if_excessive(cells);
            }
            LineStorage::Trimmed { .. } => self.storage.truncate_trimmed(new_len),
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

    /// Mutable iterator over cells. Degrades to Flat first.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Cell> {
        self.as_flat_slice_mut().iter_mut()
    }

    /// try to compress this line into a single Cluster when
    /// the entire row is uniform (every cell byte-identical). Called by
    /// `Grid::scroll_up` as a Line is ejected from visible into
    /// scrollback. Multi-Cluster compression of partially-uniform lines
    /// is intentionally not implemented.
    ///
    /// Returns `true` if storage changed.
    ///
    /// * No-op if already Cluster.
    /// * No-op on empty Flat (nothing to compress).
    /// * No-op if any two cells differ — keeps Flat to avoid degrading
    ///   on the first edit.
    pub fn try_compress(&mut self) -> bool {
        self.compress_releasing().is_some()
    }

    /// Compress a uniform flat row to one `Cluster`, as [`Line::try_compress`] does, and return
    /// its old cell buffer for the caller to reuse. `None` leaves the row unchanged.
    pub fn compress_releasing(&mut self) -> Option<Vec<Cell>> {
        let LineStorage::Flat(flat) = &mut self.storage else {
            // When: storage is already `Cluster` or `Trimmed`, compression does not apply.
            return None;
        };
        let first = flat.first()?;
        if !flat.iter().all(|cell| same_as_fill(cell, first)) {
            // When: not every flat cell equals `first`, single-cluster compression is invalid.
            return None;
        }
        let count = flat.len();
        let cell = first.clone();
        let released = std::mem::take(flat);
        self.storage = LineStorage::Cluster(vec![Cluster { cell, count }]);
        Some(released)
    }

    /// Empty this row into a flat buffer that can hold `cols` cells, without reading its old
    /// cells, and reset its stamp and soft wrap. Returns `true` when that took an allocation.
    ///
    /// A flat buffer already large enough is cleared in place; a trimmed row's buffer is cleared
    /// and grown once by exactly `cols`; a clustered row gets a new buffer of `cols`.
    pub fn clear_for_reuse(&mut self, cols: usize) -> bool {
        self.content_seq_and_flags = 0;
        match &mut self.storage {
            LineStorage::Flat(cells) if cells.capacity() >= cols => {
                // the flat buffer already holds `cols` cells, clearing it needs no allocation.
                cells.clear();
                false
            }
            LineStorage::Flat(cells) | LineStorage::Trimmed { cells, .. } => {
                let mut cells = std::mem::take(cells);
                cells.clear();
                cells.reserve_exact(cols);
                self.storage = LineStorage::Flat(cells);
                true
            }
            LineStorage::Cluster(_) => {
                self.storage = LineStorage::Flat(Vec::with_capacity(cols));
                true
            }
        }
    }

    /// force the storage to Flat. Use at any mutation site
    /// that may operate on a scrollback Line that could now be in
    /// Cluster form (the Grid produces Cluster lines on eject). All
    /// existing `as_vec_mut` / `set` / `iter_mut` paths already degrade,
    /// but call sites that hold a `&mut Line` and intend to do bulk
    /// in-place edits can call this once up front for clarity.
    pub fn ensure_flat(&mut self) {
        self.degrade_to_flat();
    }
}

impl<'a> IntoIterator for &'a Line {
    type Item = &'a Cell;
    type IntoIter = LineIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut Line {
    type Item = &'a mut Cell;
    type IntoIter = std::slice::IterMut<'a, Cell>;
    fn into_iter(self) -> Self::IntoIter {
        self.as_flat_slice_mut().iter_mut()
    }
}

impl std::ops::Index<usize> for Line {
    type Output = Cell;
    fn index(&self, idx: usize) -> &Cell {
        self.get(idx).expect("Line index out of bounds")
    }
}

impl std::ops::IndexMut<usize> for Line {
    fn index_mut(&mut self, idx: usize) -> &mut Cell {
        &mut self.as_vec_mut()[idx]
    }
}

// NOTE: `Index<Range<usize>>` and friends were
// removed because they fundamentally cannot return a real `&[Cell]` slice
// from a Cluster line without materialising. Callers that want a windowed
// view must use `Line::get_range(start, end)` which returns a
// cluster-transparent iterator. Call sites updated: selection.rs,
// window_event.rs, child_window.rs, render_line_direct_smoke.rs.

impl std::hash::Hash for Line {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Hash the materialised cell sequence so cluster vs flat storage
        // produces the same hash for the same logical content. We
        // emulate `<[Cell]>::hash`: length prefix + each element, so a
        // Flat line and an equivalent Cluster line hash identically.
        self.len().hash(state);
        self.soft_wrapped_from_previous().hash(state);
        for cell in self.iter() {
            cell.hash(state);
        }
    }
}

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
    fn empty() -> Self {
        LineIter::Empty
    }

    /// Build a Cluster iterator covering the full cluster list.
    fn new_cluster(clusters: &'a [Cluster], total: usize) -> Self {
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
    fn cluster_range(clusters: &'a [Cluster], start: usize, take: usize) -> Self {
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
#[path = "line_tests.rs"]
mod line_tests;
