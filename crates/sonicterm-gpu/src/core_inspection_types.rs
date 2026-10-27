//! Inspection types responsibilities of the renderer, preserving its single owner.

use super::*;

/// Bytes per row of a readback buffer for a `width_px`-wide 4-byte frame, padded to wgpu's copy
/// alignment.
#[doc(hidden)]
#[must_use]
pub fn padded_readback_row_bytes(width_px: u32) -> u32 {
    let tight_bytes = width_px.max(1) * 4;
    tight_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

/// The tightly packed 4-byte pixels of a mapped readback whose `height_px` rows are each
/// `padded_row_bytes` long, the padding dropped.
#[doc(hidden)]
#[must_use]
pub fn unpad_readback_rows(
    mapped: &[u8],
    width_px: u32,
    height_px: u32,
    padded_row_bytes: u32,
) -> Vec<u8> {
    let tight_bytes = width_px as usize * 4;
    mapped
        .chunks(padded_row_bytes as usize)
        .take(height_px as usize)
        .flat_map(|row| &row[..tight_bytes])
        .copied()
        .collect()
}

/// The damage a presented frame carried to the presenter, read back by real-renderer tests.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentedDamage {
    /// Whether the frame was a first frame, which repaints the whole surface.
    pub first_frame: bool,
    /// The damage rectangle the presenter drew under its scissor.
    pub damage: PixelRect,
    /// The whole surface.
    pub surface: PixelRect,
}

impl PresentedDamage {
    /// Whether the frame redrew less than the whole surface: not a first frame, and its damage
    /// covers a smaller area than the surface, so a full repaint can never count as narrow.
    #[must_use]
    pub fn is_narrow(&self) -> bool {
        let area = |rect: PixelRect| u64::from(rect.w) * u64::from(rect.h);
        !self.first_frame && area(self.damage) < area(self.surface)
    }
}

/// Keeps the last presented frame's damage for a real-renderer test, only once enabled.
///
/// Disabled by default, as in production: `record` then tests one bool and builds nothing.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct PresentedDamageRecorder {
    pub(super) enabled: bool,
    pub(super) last: Option<PresentedDamage>,
}

impl PresentedDamageRecorder {
    /// Start keeping each presented frame's damage.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Keep the snapshot `build` makes, only when enabled; a disabled recorder never calls it.
    #[inline]
    pub fn record(&mut self, build: impl FnOnce() -> PresentedDamage) {
        if self.enabled {
            // Only a test hook sets `enabled`; production skips the snapshot entirely.
            self.last = Some(build());
        }
    }

    /// The last kept snapshot, cleared by this read.
    pub fn take(&mut self) -> Option<PresentedDamage> {
        self.last.take()
    }
}

/// A copy of the retained frame in a buffer the test maps itself; production code under `src/`
/// never maps or polls.
#[doc(hidden)]
pub struct RetainedFrameReadback {
    /// The device that owns `buffer`, which the test polls until the map completes.
    pub device: wgpu::Device,
    /// The `MAP_READ` buffer the frame was copied into.
    pub buffer: wgpu::Buffer,
    /// Frame width in pixels.
    pub width_px: u32,
    /// Frame height in pixels.
    pub height_px: u32,
    /// Bytes per buffer row, padded to wgpu's copy alignment.
    pub padded_row_bytes: u32,
}

#[cfg(test)]
#[path = "core_inspection_types_tests.rs"]
mod core_inspection_types_tests;
