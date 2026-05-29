//! Line cluster compression for same-attribute runs (Epic #300, P5).
//!
//! A terminal line frequently consists of long runs of consecutive cells that
//! share the exact same attributes (theme background, default fg, no
//! bold/italic, no hyperlink) — e.g. trailing blanks after a short prompt, or
//! an empty alt-screen page. Storing those as `Vec<Cell>` wastes memory and
//! cache: each `Cell` is dozens of bytes.
//!
//! `LineStorage` is a two-form representation:
//!
//! * `Cluster(Vec<Cluster>)` — RLE-style runs of identical cells. Built when
//!   we know a line was written as a stream of same-attr cells (most pty
//!   output of the form "echo something").
//! * `Flat(Vec<Cell>)` — the classic dense form. Any in-place edit (write a
//!   single cell, change a single attr) **degrades** the storage to `Flat`
//!   immediately. Flat is also the form used while the parser is actively
//!   mutating a line; clustering is a post-hoc compaction.
//!
//! `Line` exposes a transparent `iter`/`get`/`len`/`set` API so callers don't
//! have to know which form a given line is in. The compaction policy
//! (`compact_if_beneficial`) only switches Flat → Cluster when the saving is
//! ≥ 2× — otherwise the bookkeeping costs more than it saves.
//!
//! NOTE: this module currently lives as an additive primitive. Wiring it into
//! `Grid::scrollback` is a follow-up (#300-P5b) because every Row consumer in
//! the codebase indexes through `Vec<Cell>` directly. Landing the data
//! structure + invariants + tests first lets the integration PR focus purely
//! on the call-site refactor.

use sonic_types::cell::Cell;

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
}

impl LineStorage {
    /// Build a `Cluster` storage from a flat slice, collapsing runs of equal
    /// cells. Always succeeds; for an all-distinct slice the result is the
    /// same length as the input and offers no saving (callers can check that
    /// via [`Self::approx_byte_size`]).
    pub fn cluster_from_flat(cells: &[Cell]) -> Self {
        let mut clusters: Vec<Cluster> = Vec::new();
        for c in cells {
            match clusters.last_mut() {
                Some(last) if &last.cell == c => last.count += 1,
                _ => clusters.push(Cluster { cell: c.clone(), count: 1 }),
            }
        }
        LineStorage::Cluster(clusters)
    }

    /// Logical length (number of cells the line presents to its consumer).
    pub fn len(&self) -> usize {
        match self {
            LineStorage::Flat(v) => v.len(),
            LineStorage::Cluster(cs) => cs.iter().map(|c| c.count).sum(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Approximate byte footprint of the storage payload (excluding the
    /// enum discriminant). Used by [`Line::compact_if_beneficial`] to decide
    /// whether collapsing pays for itself.
    pub fn approx_byte_size(&self) -> usize {
        match self {
            LineStorage::Flat(v) => v.len() * std::mem::size_of::<Cell>(),
            LineStorage::Cluster(cs) => cs.len() * std::mem::size_of::<Cluster>(),
        }
    }
}

/// A line of cells with transparent cluster-or-flat storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    storage: LineStorage,
}

impl Line {
    /// Build a flat line of `len` clones of `fill`.
    pub fn flat_filled(len: usize, fill: Cell) -> Self {
        Self { storage: LineStorage::Flat(vec![fill; len]) }
    }

    /// Build directly from a `Vec<Cell>` in flat form.
    pub fn from_flat(cells: Vec<Cell>) -> Self {
        Self { storage: LineStorage::Flat(cells) }
    }

    /// Build directly from clusters. The caller is responsible for the
    /// "no adjacent equal cells" invariant; in debug builds we assert it.
    pub fn from_clusters(clusters: Vec<Cluster>) -> Self {
        debug_assert!(
            clusters.windows(2).all(|w| w[0].cell != w[1].cell),
            "adjacent clusters must differ"
        );
        debug_assert!(clusters.iter().all(|c| c.count > 0));
        Self { storage: LineStorage::Cluster(clusters) }
    }

    /// Logical cell count.
    pub fn len(&self) -> usize {
        self.storage.len()
    }

    pub fn is_empty(&self) -> bool {
        self.storage.is_empty()
    }

    /// Approximate payload byte size.
    pub fn approx_byte_size(&self) -> usize {
        self.storage.approx_byte_size()
    }

    /// Returns `true` if the line is currently in cluster form.
    pub fn is_clustered(&self) -> bool {
        matches!(self.storage, LineStorage::Cluster(_))
    }

    /// Get the cell at logical column `idx`, if in range.
    pub fn get(&self, idx: usize) -> Option<&Cell> {
        match &self.storage {
            LineStorage::Flat(v) => v.get(idx),
            LineStorage::Cluster(cs) => {
                let mut off = 0;
                for c in cs {
                    if idx < off + c.count {
                        return Some(&c.cell);
                    }
                    off += c.count;
                }
                None
            }
        }
    }

    /// Set the cell at logical column `idx`. Degrades the storage to `Flat`
    /// on the first call (and stays Flat). Returns `true` if the index was
    /// in range.
    pub fn set(&mut self, idx: usize, cell: Cell) -> bool {
        self.degrade_to_flat();
        match &mut self.storage {
            LineStorage::Flat(v) => {
                if let Some(slot) = v.get_mut(idx) {
                    *slot = cell;
                    true
                } else {
                    false
                }
            }
            LineStorage::Cluster(_) => unreachable!("just degraded"),
        }
    }

    /// Force the storage to `Flat`. No-op if already flat.
    pub fn degrade_to_flat(&mut self) {
        if let LineStorage::Cluster(cs) = &self.storage {
            let total: usize = cs.iter().map(|c| c.count).sum();
            let mut flat = Vec::with_capacity(total);
            for c in cs {
                for _ in 0..c.count {
                    flat.push(c.cell.clone());
                }
            }
            self.storage = LineStorage::Flat(flat);
        }
    }

    /// Try to collapse a Flat storage into Cluster form. Only switches when
    /// the cluster form would use **less than half** the bytes of the flat
    /// form — otherwise the win is too small to justify the indirection on
    /// later accesses. No-op if already clustered.
    ///
    /// Returns `true` if storage changed.
    pub fn compact_if_beneficial(&mut self) -> bool {
        let flat = match &self.storage {
            LineStorage::Flat(v) => v,
            LineStorage::Cluster(_) => return false,
        };
        if flat.is_empty() {
            return false;
        }
        let candidate = LineStorage::cluster_from_flat(flat);
        if candidate.approx_byte_size() * 2 <= self.storage.approx_byte_size() {
            self.storage = candidate;
            true
        } else {
            false
        }
    }

    /// Iterator over cells in logical order. Cheap regardless of storage.
    pub fn iter(&self) -> LineIter<'_> {
        match &self.storage {
            LineStorage::Flat(v) => LineIter::Flat(v.iter()),
            LineStorage::Cluster(cs) => {
                LineIter::Cluster { clusters: cs.iter(), current: None, remaining: 0 }
            }
        }
    }

    /// Materialise into a flat `Vec<Cell>` (cloning). Equivalent to
    /// `self.iter().cloned().collect()` but a hair faster for the cluster
    /// case because it pre-sizes.
    pub fn to_vec(&self) -> Vec<Cell> {
        let mut out = Vec::with_capacity(self.len());
        for c in self.iter() {
            out.push(c.clone());
        }
        out
    }

    /// Read-only access to the underlying storage form. Useful for tests
    /// and for the eventual `Grid` integration that wants to fast-path
    /// the cluster case.
    pub fn storage(&self) -> &LineStorage {
        &self.storage
    }
}

/// Transparent iterator over either storage form.
pub enum LineIter<'a> {
    Flat(std::slice::Iter<'a, Cell>),
    Cluster { clusters: std::slice::Iter<'a, Cluster>, current: Option<&'a Cell>, remaining: usize },
}

impl<'a> Iterator for LineIter<'a> {
    type Item = &'a Cell;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            LineIter::Flat(it) => it.next(),
            LineIter::Cluster { clusters, current, remaining } => {
                if *remaining == 0 {
                    let c = clusters.next()?;
                    *current = Some(&c.cell);
                    *remaining = c.count;
                }
                *remaining -= 1;
                *current
            }
        }
    }
}
