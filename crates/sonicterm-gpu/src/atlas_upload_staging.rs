//! Reusable dirty-rectangle staging, coalescing and upload scratch accounting.

use super::*;

impl UploadLists {
    /// Drain `atlas`'s dirty rects, coalesce same-kind neighbours, and hand each staged rect to
    /// `write`. The stats are computed before the lists are released, so they report the work
    /// done; both lists are then cleared and shrunk when oversized, on the zero-dirty path too.
    /// The staging buffer is cleared after the last write returns (the queue has copied each
    /// slice) and released by the shared scratch rule against this sync's largest write, so a
    /// full-atlas re-upload holds its staging for one sync only.
    pub(crate) fn sync_with(
        &mut self,
        atlas: &mut GlyphAtlas,
        mut write: impl FnMut(DirtyRect, &[u8]),
    ) -> AtlasUploadStats {
        atlas.drain_dirty_rects_into(&mut self.dirty_rects);
        let dirty_rects = self.dirty_rects.len();
        if dirty_rects == 0 {
            // When: `dirty_rects` is zero, skip queue writes, release any oversized list and
            // staging buffer, and report an idle synchronization.
            self.release_rect_lists();
            self.release_staging(0);
            return AtlasUploadStats::default();
        }
        coalesce_dirty_rects(&self.dirty_rects, &mut self.coalesced_rects);
        let atlas_w = atlas.width();
        let pixels = atlas.pixels();
        let mut uploaded_bytes = 0usize;
        let mut largest = 0usize;
        for &rect in &self.coalesced_rects {
            copy_rect_into_scratch(pixels, atlas_w, rect, &mut self.scratch);
            uploaded_bytes = uploaded_bytes.saturating_add(self.scratch.len());
            largest = largest.max(self.scratch.len());
            write(rect, &self.scratch);
        }
        let stats = AtlasUploadStats {
            dirty_rects,
            upload_calls: self.coalesced_rects.len(),
            uploaded_bytes,
        };
        self.release_rect_lists();
        self.release_staging(largest);
        stats
    }
}

impl UploadLists {
    /// Empty the staging buffer, then release its excess against `largest`, the biggest write of
    /// the sync that just ended. Clearing first matters: the shared rule cannot shrink below the
    /// buffer's length, so a buffer still holding its last write would never be released.
    pub(super) fn release_staging(&mut self, largest: usize) {
        self.scratch.clear();
        crate::wezterm_pipeline::release_excess(&mut self.scratch, largest);
    }
}

impl UploadLists {
    /// Release the staging buffer and both rect lists entirely, for a covered window's trim. The
    /// next sync allocates what it needs again.
    pub(crate) fn release_scratch(&mut self) {
        self.scratch = Vec::new();
        self.dirty_rects = Vec::new();
        self.coalesced_rects = Vec::new();
    }
}

impl UploadLists {
    /// Clear both rect lists and shrink any whose capacity exceeds 1,024 rects back to 64.
    pub(super) fn release_rect_lists(&mut self) {
        for list in [&mut self.dirty_rects, &mut self.coalesced_rects] {
            list.clear();
            if list.capacity() > UPLOAD_LIST_SHRINK_ABOVE {
                list.shrink_to(UPLOAD_LIST_RETAINED);
            }
        }
    }
}

impl UploadLists {
    /// Bytes reserved by both rect lists.
    pub(crate) fn retained_list_bytes(&self) -> usize {
        (self.dirty_rects.capacity() + self.coalesced_rects.capacity())
            * std::mem::size_of::<DirtyRect>()
    }
}

impl UploadLists {
    /// Bytes this storage keeps between syncs, reported under `UploadStaging` with the vertex
    /// scratch: both rect lists plus the staging buffer, which keeps its largest write's capacity
    /// (at most one whole atlas, since a staged rect never exceeds the atlas it is copied from).
    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained_list_bytes() + self.scratch.capacity()
    }
}

pub(super) fn coalesce_dirty_rects(input: &[DirtyRect], out: &mut Vec<DirtyRect>) {
    out.clear();
    out.extend(input.iter().copied().filter(|rect| rect.w > 0 && rect.h > 0));
    out.sort_unstable_by_key(|rect| (rect.kind, rect.y, rect.h, rect.x, rect.w));
    merge_sorted_rects(
        out,
        |left, right| {
            left.kind == right.kind
                && left.y == right.y
                && left.h == right.h
                && right.x <= left.x.saturating_add(left.w)
        },
        |left, right| {
            left.w = left.x.saturating_add(left.w).max(right.x.saturating_add(right.w)) - left.x;
        },
    );
    out.sort_unstable_by_key(|rect| (rect.kind, rect.x, rect.w, rect.y, rect.h));
    merge_sorted_rects(
        out,
        |top, bottom| {
            top.kind == bottom.kind
                && top.x == bottom.x
                && top.w == bottom.w
                && bottom.y <= top.y.saturating_add(top.h)
        },
        |top, bottom| {
            top.h = top.y.saturating_add(top.h).max(bottom.y.saturating_add(bottom.h)) - top.y;
        },
    );
}

pub(super) fn merge_sorted_rects(
    rects: &mut Vec<DirtyRect>,
    compatible: impl Fn(DirtyRect, DirtyRect) -> bool,
    merge: impl Fn(&mut DirtyRect, DirtyRect),
) {
    let mut write = 0usize;
    for read in 0..rects.len() {
        let next = rects[read];
        if write > 0 && compatible(rects[write - 1], next) {
            merge(&mut rects[write - 1], next);
        } else {
            // When: `next` is incompatible with the previous output rectangle, retain it as a new merge run.
            rects[write] = next;
            write += 1;
        }
    }
    rects.truncate(write);
}

pub(super) fn copy_rect_into_scratch(
    pixels: &[u8],
    atlas_width: u32,
    rect: DirtyRect,
    scratch: &mut Vec<u8>,
) {
    let bpp = BYTES_PER_PIXEL as usize;
    let row_bytes = rect.w as usize * bpp;
    let required = rect.h as usize * row_bytes;
    scratch.clear();
    // Exact growth prevents a larger dirty rect from retaining Vec's geometric
    // spare capacity beyond one full-atlas staging payload.
    if required > scratch.capacity() {
        scratch.reserve_exact(required);
    }
    for row in 0..rect.h {
        let offset = ((rect.y + row) * atlas_width + rect.x) as usize * bpp;
        let source = &pixels[offset..offset + row_bytes];
        match rect.kind {
            AtlasPixelKind::Coverage => scratch.extend_from_slice(source),
            AtlasPixelKind::Color => {
                // When: Color is selected, decoded linear RGB must be premultiplied before sRGB texture storage.
                for pixel in source.as_chunks::<4>().0 {
                    let alpha = pixel[3];
                    if alpha == 0 {
                        // When: alpha is zero, canonicalize arbitrary encoded RGB to the unique transparent premultiplied value.
                        scratch.extend_from_slice(&[0, 0, 0, 0]);
                        continue;
                    }
                    let alpha_linear = alpha as f64 / 255.0;
                    for &encoded_premul in &pixel[..3] {
                        let straight_srgb = (encoded_premul as f64 / alpha as f64).clamp(0.0, 1.0);
                        let premul_linear =
                            crate::color::srgb_channel_to_linear(straight_srgb) * alpha_linear;
                        scratch.push(crate::color::linear_channel_to_srgb_u8(premul_linear as f32));
                    }
                    scratch.push(alpha);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "atlas_upload_staging_tests.rs"]
mod atlas_upload_staging_tests;
