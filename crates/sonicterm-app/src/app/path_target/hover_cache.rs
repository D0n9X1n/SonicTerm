//! One window's memo of its last hover detection.
//!
//! Text detection depends only on the pointed cell, the frame's identity, the detection settings
//! and the rows it read. Each window keeps the last result with that key and the content stamp of
//! every row in the span detection read, so a lookup whose key and stamps all match reuses the
//! result without reading a cell or hashing a row. Stamps are only this fast path: a click always
//! computes its own fresh target, so authorization never rests on a cached result.

use sonicterm_grid::grid::Grid;

use super::CellTargetSnapshot;

/// Everything a detection depends on apart from the rows it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct DetectionContext {
    pub(in crate::app) pane_id: u64,
    /// The pointed cell in displayed coordinates, `(row, col)`.
    pub(in crate::app) pointed: (u16, u16),
    pub(in crate::app) view_top: u64,
    /// Whether a row exists below the frame, which the bottom edge's refusal depends on.
    pub(in crate::app) rows_below: bool,
    pub(in crate::app) scrollback_evicted: u64,
    pub(in crate::app) screen_epoch: u64,
    pub(in crate::app) size_generation: u64,
    pub(in crate::app) alt_screen: bool,
    pub(in crate::app) cwd_revision: u64,
    pub(in crate::app) clickable_local_targets: bool,
    pub(in crate::app) clickable_bare_names: bool,
    pub(in crate::app) clickable_path_max_chars: usize,
}

/// The last detection: its context, the first row of its read span with each row's content
/// stamp in order, and the result.
#[derive(Debug, Clone)]
struct CachedDetection {
    context: DetectionContext,
    first_row: u64,
    stamps: Vec<Option<u64>>,
    target: Option<CellTargetSnapshot>,
}

/// A window's detection memo; empty until the first detection and after every invalidation.
#[derive(Debug, Clone, Default)]
pub(in crate::app) struct HoverDetectionCache {
    entry: Option<CachedDetection>,
}

impl HoverDetectionCache {
    /// The cached result for `context` when every row in its read span still carries the stamp
    /// it had; `None` on any difference. Reads row stamps only, never a cell or a hash.
    pub(in crate::app) fn lookup(
        &self,
        context: &DetectionContext,
        grid: &Grid,
    ) -> Option<Option<CellTargetSnapshot>> {
        let entry = self.entry.as_ref()?;
        if entry.context != *context {
            // When: any part of the context differs, the cached result answers another question.
            return None;
        }
        let unchanged = entry.stamps.iter().enumerate().all(|(offset, stamp)| {
            grid.row_content_seq_at_abs(entry.first_row + offset as u64) == *stamp
        });
        unchanged.then(|| entry.target.clone())
    }

    /// Remember `target`, detected under `context` from the rows in `read_span`, with their
    /// current stamps. `read_span` is `None` when detection read no row.
    pub(in crate::app) fn store(
        &mut self,
        context: DetectionContext,
        grid: &Grid,
        read_span: Option<(u64, u64)>,
        target: Option<CellTargetSnapshot>,
    ) {
        let (first_row, stamps) = match read_span {
            Some((low, high)) => {
                (low, (low..=high).map(|row| grid.row_content_seq_at_abs(row)).collect())
            }
            None => (0, Vec::new()),
        };
        self.entry = Some(CachedDetection { context, first_row, stamps, target });
    }

    /// Forget the cached result, so the next lookup detects again.
    pub(in crate::app) fn clear(&mut self) {
        self.entry = None;
    }

    /// Whether a result is cached.
    #[cfg(test)]
    pub(in crate::app) fn is_empty(&self) -> bool {
        self.entry.is_none()
    }
}

#[cfg(test)]
#[path = "hover_cache_tests.rs"]
mod hover_cache_tests;
