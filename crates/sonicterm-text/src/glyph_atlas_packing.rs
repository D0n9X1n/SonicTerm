//! Shelf placement and replay-based atlas fit classification.

use super::{ATLAS_DIM, FIT_DIMS};

/// Smallest candidate start size the resident tiles would have fitted, replayed from empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitOutcome {
    /// Every tile packs at this dimension with the used height at most 75% of it.
    Fits(u32),
    /// Every tile packs at the maximum, but no candidate leaves 25% of its height free.
    FitsWithoutHeadroom,
    /// Some tile cannot be placed even at the maximum.
    DoesNotFit,
    /// The atlas has evicted since creation, so its resident set is not the working set.
    Evicted,
}

impl FitOutcome {
    /// Snapshot label: the dimension for a fit, else `no_headroom`, `does_not_fit` or `evicted`.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Fits(dim) => dim.to_string(),
            Self::FitsWithoutHeadroom => "no_headroom".to_owned(),
            Self::DoesNotFit => "does_not_fit".to_owned(),
            Self::Evicted => "evicted".to_owned(),
        }
    }
}

/// Replay `tiles` (each a `[width, height]`, in placement order) through a fresh shelf
/// packer at each of [`FIT_DIMS`] and classify the smallest dimension that holds them.
///
/// A dimension qualifies only when every tile is placed and the used height
/// (`shelf_y + shelf_h`) is at most three quarters of it, leaving room for the session to
/// keep drawing new glyphs before the first growth.
#[must_use]
pub fn fit_outcome_of_tiles(tiles: &[[u32; 2]]) -> FitOutcome {
    let mut placed_at_max = false;
    for dim in FIT_DIMS {
        let mut packer = ShelfPacker::new(dim, dim);
        let all_placed =
            tiles.iter().all(|[width, height]| packer.alloc(*width, *height).is_some());
        if !all_placed {
            // When: all_placed is false at this dim a larger candidate may still hold the set.
            continue;
        }
        if packer.used_height().saturating_mul(4) <= dim.saturating_mul(3) {
            // When: used_height leaves a quarter of dim free this is the smallest qualifying fit.
            return FitOutcome::Fits(dim);
        }
        placed_at_max = dim == ATLAS_DIM;
    }
    if placed_at_max {
        FitOutcome::FitsWithoutHeadroom
    } else {
        // When: placed_at_max is false some tile could not be placed even at ATLAS_DIM.
        FitOutcome::DoesNotFit
    }
}

/// Shelf packer: simple left-to-right strip allocator that opens a new
/// strip when the current one overflows in width.
///
/// Trade-off vs a guillotine/skyline packer: shelf wastes more vertical
/// space when tile heights vary a lot, but monospace glyphs are nearly
/// uniform in height so the waste is small (<20% in practice). The
/// simplicity makes the code easier to audit; we can swap algorithms
/// later without touching the public API.
#[derive(Debug)]
#[doc(hidden)]
pub struct ShelfPacker {
    width: u32,
    height: u32,
    /// X cursor on the current shelf.
    cursor_x: u32,
    /// Y top of the current shelf.
    shelf_y: u32,
    /// Height of the current shelf — set by the first tile placed on it.
    shelf_h: u32,
}

impl ShelfPacker {
    /// New packer for a `width × height` atlas, starting with an empty shelf.
    ///
    /// The packer owns placement only: `cursor_x`, `shelf_y` and `shelf_h`
    /// describe the open shelf, and `shelf_h` is fixed by the first tile
    /// placed on each one.
    #[doc(hidden)]
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, cursor_x: 0, shelf_y: 0, shelf_h: 0 }
    }

    /// Allocate a `(w, h)` rect on the atlas. Returns `(x, y)` of the
    /// top-left or `None` if the atlas is full. On failure the packer
    /// state is unchanged so subsequent smaller allocations that DO fit
    /// the current shelf still succeed.
    #[doc(hidden)]
    pub fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > self.width || h > self.height {
            // When: w or h exceeds the atlas no placement exists, so alloc returns before
            // mutating cursor_x or shelf_h and a later smaller tile still fits this shelf.
            return None; // tile bigger than entire atlas
        }
        // Compute a candidate (cursor_x, shelf_y, shelf_h) without
        // mutating self until the candidate is proven valid.
        let mut cand_cursor_x = self.cursor_x;
        let mut cand_shelf_y = self.shelf_y;
        let mut cand_shelf_h = self.shelf_h;

        // First-tile-ever — bootstrap the shelf height.
        if cand_shelf_h == 0 {
            cand_shelf_h = h;
        }
        // Doesn't fit horizontally → advance to a fresh shelf.
        if cand_cursor_x + w > self.width {
            cand_shelf_y = cand_shelf_y.saturating_add(cand_shelf_h);
            cand_cursor_x = 0;
            cand_shelf_h = h;
        }
        // Grow shelf height if this tile is taller than what's there.
        if h > cand_shelf_h {
            cand_shelf_h = h;
        }
        // Bounds check BEFORE committing — if vertical capacity is
        // exhausted, return None and leave packer untouched.
        if cand_shelf_y + cand_shelf_h > self.height {
            // When: cand_shelf_y plus cand_shelf_h passes the atlas height no shelf room
            // is left, and returning before the commit keeps the packer state reusable.
            return None;
        }
        // Commit.
        let x = cand_cursor_x;
        let y = cand_shelf_y;
        self.cursor_x = cand_cursor_x + w;
        self.shelf_y = cand_shelf_y;
        self.shelf_h = cand_shelf_h;
        Some((x, y))
    }

    /// Height consumed so far: the open shelf's top plus its height.
    #[doc(hidden)]
    pub fn used_height(&self) -> u32 {
        self.shelf_y.saturating_add(self.shelf_h)
    }

    /// Enlarge the packing area to `width × height` without moving any placement.
    ///
    /// The open shelf keeps its cursor and height and gains the new width; everything below
    /// the old height becomes free for new shelves. Closed shelves do not reclaim their new
    /// right-hand space, which is what keeps every placed tile at its pixel position.
    #[doc(hidden)]
    pub fn grow(&mut self, width: u32, height: u32) {
        debug_assert!(width >= self.width && height >= self.height, "a packer never shrinks");
        self.width = width;
        self.height = height;
    }
}

#[cfg(test)]
#[path = "glyph_atlas_packing_tests.rs"]
mod glyph_atlas_packing_tests;
