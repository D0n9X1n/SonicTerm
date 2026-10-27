//! Atlas growth policy, resident-tile copying, and working-set measurements.

use super::*;

impl GlyphAtlas {
    /// New square atlas of `start` pixels that doubles up to `max` before it evicts.
    ///
    /// This is the only growing constructor; the renderer's glyph atlas is its only caller.
    ///
    /// # Panics
    ///
    /// When `start` or `max` is not a power of two, or `start > max`.
    #[must_use]
    pub fn growable(start: u32, max: u32) -> Self {
        assert!(
            start.is_power_of_two() && max.is_power_of_two(),
            "growable sizes are powers of two"
        );
        assert!(start <= max, "a growable atlas starts at or below its maximum");
        let mut atlas = Self::new(start, start);
        atlas.growth = GrowthPolicy::Growable { max };
        atlas
    }

    /// Whether this atlas grows on packing failure, and to what maximum.
    #[must_use]
    pub fn growth_policy(&self) -> GrowthPolicy {
        self.growth
    }

    /// Monotonic count of size doublings since construction, including across resets.
    #[must_use]
    pub fn growths(&self) -> u64 {
        self.growths
    }

    /// Largest tile width and height a lookup may place: the growth maximum when growable,
    /// else the current size. A larger tile is cached as a sentinel instead.
    pub(super) fn placement_limit(&self) -> (u32, u32) {
        match self.growth {
            GrowthPolicy::Growable { max } => (max, max),
            GrowthPolicy::Fixed => (self.width, self.height),
        }
    }

    /// The next doubled dimension when this atlas may still grow, else `None`.
    fn next_growth_dim(&self) -> Option<u32> {
        let GrowthPolicy::Growable { max } = self.growth else {
            // When: growth is Fixed the atlas keeps its constructed size, so packing failure evicts.
            return None;
        };
        // At the maximum there is nothing left to double into, so packing failure evicts there too.
        (self.width < max).then(|| self.width.saturating_mul(2).min(max))
    }

    /// Enlarge the atlas to `dim × dim` without re-rasterizing anything, when its growth
    /// policy allows that size; returns whether it grew.
    ///
    /// Only the next doubling of a `Growable` atlas is allowed: `dim` must equal twice the
    /// current size, clamped to the maximum. A `Fixed` atlas, an atlas at its maximum, and
    /// every other `dim` are refused, and a refused call changes nothing.
    ///
    /// On growth, every resident tile keeps its pixel position, so its pixels are copied row
    /// by row and its normalized UVs are recomputed from its own tile size. The identity
    /// advances so every UV-bearing cache rebuilds, and the dirty list is replaced by one rect
    /// per resident tile, typed by that tile's pixel kind, so the recreated GPU texture
    /// receives each tile exactly once.
    #[must_use = "a refused growth leaves the atlas at its current size"]
    pub fn grow_to(&mut self, dim: u32) -> bool {
        if self.next_growth_dim() != Some(dim) {
            // When: the policy's next doubling is not `dim`, refuse before touching any state.
            return false;
        }
        self.grow_unchecked(dim);
        true
    }

    /// The copy behind [`Self::grow_to`]; `dim` is the policy's next doubling.
    fn grow_unchecked(&mut self, dim: u32) {
        debug_assert!(self.width == self.height && dim > self.width, "growth enlarges a square");
        let bpp = BYTES_PER_PIXEL as usize;
        let old_row_bytes = self.width as usize * bpp;
        let new_row_bytes = dim as usize * bpp;
        let mut grown = vec![0u8; new_row_bytes * dim as usize];
        for (row_index, old_row) in self.pixels.chunks_exact(old_row_bytes).enumerate() {
            let start = row_index * new_row_bytes;
            grown[start..start + old_row_bytes].copy_from_slice(old_row);
        }
        // The old buffer drops here, so peak memory is both buffers for this call only.
        self.pixels = grown;
        self.packer.grow(dim, dim);
        self.width = dim;
        self.height = dim;
        let dim_f = dim as f32;
        let mut uploads: Vec<DirtyRect> = Vec::new();
        for entry in self.map.values_mut() {
            let Some((x, y, _, _)) = entry.rect else {
                // When: rect is None the entry owns no pixels, so its zero UV stays as is.
                continue;
            };
            let [tile_w, tile_h] = entry.info.px_size;
            // UVs use the tile size, never the slot size: a reused slot may be larger than
            // its tile, and the slot margin can hold an evicted tile's stale pixels.
            entry.info.uv = [
                x as f32 / dim_f,
                y as f32 / dim_f,
                (x + tile_w) as f32 / dim_f,
                (y + tile_h) as f32 / dim_f,
            ];
            let kind =
                if entry.info.is_color { AtlasPixelKind::Color } else { AtlasPixelKind::Coverage };
            uploads.push(DirtyRect { x, y, w: tile_w, h: tile_h, kind });
        }
        // HashMap order is arbitrary; sorting keeps the re-upload order deterministic.
        uploads.sort_by_key(|rect| (rect.y, rect.x));
        self.dirty.clear();
        self.dirty.extend(uploads);
        self.identity = self.identity.wrapping_add(1);
        self.growths += 1;
        tracing::debug!(
            target: "sonic::glyph_atlas",
            dim,
            growths = self.growths,
            resident = self.map.len(),
            "glyph atlas grew"
        );
    }

    /// Allocate a `width × height` slot, growing the atlas before giving up. Never evicts.
    pub(super) fn alloc_rect_growing(
        &mut self,
        width: u32,
        height: u32,
    ) -> Option<AtlasAllocation> {
        loop {
            if let Some(allocation) = self.alloc_rect(width, height) {
                // When: alloc_rect placed the tile at the current size, no growth is needed.
                return Some(allocation);
            }
            // Each pass doubles toward the maximum, so the loop ends after at most three
            // growths from the 256 floor.
            let next_dim = self.next_growth_dim()?;
            self.grow_unchecked(next_dim);
        }
    }

    /// Area in pixels of every resident tile at its tile size.
    #[must_use]
    pub fn packed_pixels(&self) -> u64 {
        self.map
            .values()
            .filter(|entry| entry.rect.is_some())
            .map(|entry| u64::from(entry.info.px_size[0]) * u64::from(entry.info.px_size[1]))
            .sum()
    }

    /// Largest resident tile width and largest resident tile height, independently.
    #[must_use]
    pub fn max_tile_dims(&self) -> [u32; 2] {
        self.map.values().filter(|entry| entry.rect.is_some()).fold(
            [0, 0],
            |[max_w, max_h], entry| {
                [max_w.max(entry.info.px_size[0]), max_h.max(entry.info.px_size[1])]
            },
        )
    }

    /// Keys of every resident tile that owns atlas pixels; sentinels are excluded.
    #[must_use]
    pub fn resident_tile_keys(&self) -> std::collections::HashSet<GlyphKey> {
        self.map.iter().filter(|(_, entry)| entry.rect.is_some()).map(|(key, _)| *key).collect()
    }

    /// Reserved capacity of the CPU pixel buffer alone.
    #[must_use]
    pub fn retained_pixel_bytes(&self) -> usize {
        self.pixels.capacity()
    }

    /// Reserved capacity of the pending dirty list, in rects.
    #[must_use]
    pub fn dirty_capacity(&self) -> usize {
        self.dirty.capacity()
    }

    /// Smallest candidate start size the resident tiles would have fitted.
    ///
    /// Replays the resident tiles in `(y, x)` order at their tile size through a fresh
    /// packer, per [`fit_outcome_of_tiles`]. An atlas that has ever evicted reports
    /// [`FitOutcome::Evicted`], since its resident set is no longer its working set.
    #[must_use]
    pub fn fit_outcome(&self) -> FitOutcome {
        if self.ever_evicted {
            // When: ever_evicted is set the resident set understates the working set.
            return FitOutcome::Evicted;
        }
        let mut placed: Vec<(u32, u32, [u32; 2])> = self
            .map
            .values()
            .filter_map(|entry| entry.rect.map(|(x, y, _, _)| (y, x, entry.info.px_size)))
            .collect();
        placed.sort_unstable();
        let tiles: Vec<[u32; 2]> = placed.into_iter().map(|(_, _, size)| size).collect();
        fit_outcome_of_tiles(&tiles)
    }
}

#[cfg(test)]
#[path = "glyph_atlas_growth_tests.rs"]
mod glyph_atlas_growth_tests;
