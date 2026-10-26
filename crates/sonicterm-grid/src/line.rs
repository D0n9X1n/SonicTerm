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

#[cfg(test)]
#[path = "line_tests.rs"]
mod line_tests;

#[path = "line_storage.rs"]
mod line_storage;

#[path = "line_iter.rs"]
mod line_iter;

pub use line_iter::LineIter;
pub use line_storage::{StorageIter, StorageRangeIter};

#[cfg(test)]
#[path = "line_test_support.rs"]
mod line_test_support;
