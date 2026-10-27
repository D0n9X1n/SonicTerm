//! Atlas policy responsibilities of the renderer, preserving its single owner.

use super::*;

#[must_use]
pub(super) fn atlas_changed_during_frame(
    before: GlyphContentStamp,
    after: GlyphContentStamp,
) -> bool {
    before != after
}

#[must_use]
pub(super) fn row_cache_atlas_identity(atlas: &GlyphAtlas) -> u64 {
    atlas.identity()
}

pub(super) fn atlas_texture_rebuild_required(current: (u32, u32), next: (u32, u32)) -> bool {
    current != next
}

pub(super) fn scale_factor_rebuild_required(current: f32, next: f32) -> bool {
    (current - next.max(0.1)).abs() >= f32::EPSILON
}

pub(super) const PLACEHOLDER_ATLAS_DIM: u32 = 1;

#[must_use]
pub(super) fn atlas_payload_bytes(width: u32, height: u32) -> u64 {
    u64::from(width).saturating_mul(u64::from(height)).saturating_mul(4)
}

#[must_use]
pub(super) fn desired_gpu_atlas_dimensions(
    software_presenter: bool,
    atlas: &GlyphAtlas,
) -> (u32, u32) {
    if software_presenter {
        (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
    } else {
        // When: `!software_presenter` — the GPU samples the atlas texture, so
        // it must match the CPU atlas or the UVs address the wrong tiles.
        (atlas.width(), atlas.height())
    }
}

#[must_use]
pub(super) fn image_atlas_promotion_required(atlas: &GlyphAtlas, has_inline_media: bool) -> bool {
    has_inline_media
        && (atlas.width(), atlas.height()) == (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
}

/// Frames a window must draw with no renderable inline media before its image
/// atlas is released.
///
/// At 60fps this is about four seconds. Long enough that scrolling an image
/// out of view and back does not free and reallocate 16 MiB — which would also
/// force every visible image to re-decode — and short enough that a window
/// that has genuinely finished with images does not hold the allocation for
/// the rest of its life.
pub(super) const IMAGE_ATLAS_IDLE_FRAMES: u32 = 240;

/// How long a window may go without renderable inline media before its promoted image atlas is
/// released, whether or not it assembles frames meanwhile. A window that stops drawing after an image
/// scrolls away never reaches [`IMAGE_ATLAS_IDLE_FRAMES`], so this interval releases it without a frame.
pub(super) const IMAGE_ATLAS_IDLE_INTERVAL: Duration = Duration::from_secs(30);

/// Whether the image atlas holds a full allocation rather than the 1x1 placeholder.
#[must_use]
pub(super) fn image_atlas_promoted(atlas: &GlyphAtlas) -> bool {
    (atlas.width(), atlas.height()) != (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
}

/// Whether the interval trigger releases the image atlas at `now`: it is promoted and no renderable
/// media has been visible since `absent_since`, at least [`IMAGE_ATLAS_IDLE_INTERVAL`] ago.
#[must_use]
pub(super) fn image_atlas_release_due(
    promoted: bool,
    absent_since: Option<Instant>,
    now: Instant,
) -> bool {
    promoted
        && absent_since
            .is_some_and(|since| now.saturating_duration_since(since) >= IMAGE_ATLAS_IDLE_INTERVAL)
}

/// When the interval trigger is due; `None` for a placeholder atlas or while media is visible.
#[must_use]
pub(super) fn image_atlas_release_deadline_for(
    promoted: bool,
    absent_since: Option<Instant>,
) -> Option<Instant> {
    absent_since.filter(|_| promoted).map(|since| since + IMAGE_ATLAS_IDLE_INTERVAL)
}

/// The absence instant after one assembly: set by the first media-free frame and kept by later ones,
/// so repeated idle frames never move the deadline; cleared by a frame with renderable media.
#[must_use]
pub(super) fn next_inline_media_absent_since(
    current: Option<Instant>,
    has_renderable_inline_media: bool,
    now: Instant,
) -> Option<Instant> {
    if has_renderable_inline_media {
        None
    } else {
        // When: !has_renderable_inline_media, keep the current absence instant or start one at now.
        current.or(Some(now))
    }
}

/// Whether an idle window should release its full-size image atlas.
///
/// Promotion is otherwise one-way: `reset_in_place` clears the map and
/// repacker but never touches the pixel buffer or the dimensions, so a window
/// that displays one inline image keeps the allocation until it closes.
#[must_use]
pub(super) fn image_atlas_demotion_ready(
    atlas: &GlyphAtlas,
    has_inline_media: bool,
    frames_without_inline_media: u32,
) -> bool {
    !has_inline_media
        && (atlas.width(), atlas.height()) != (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
        && frames_without_inline_media >= IMAGE_ATLAS_IDLE_FRAMES
}

/// Whether clearing the image atlas would actually do anything.
///
/// The frame-assembly caller resets whenever inline media "changed", but that
/// signal is `true` on any frame whose predecessor's key was absent — which is
/// every frame following the many state changes that clear it. On a window
/// that has never shown an image the media hash cannot change, so the absent
/// key accounts for every reset, once per rendered frame.
///
/// An untouched placeholder atlas holds no entries and no packing state, so
/// resetting it changes nothing while still rebuilding the packer and bumping
/// the atlas identity — which invalidates every cache keyed to it. A promoted
/// atlas carries that state even while its entry map is momentarily empty and
/// must still be reset, or the packer would keep handing out coordinates from
/// a layout the caller believes it discarded.
#[must_use]
pub(super) fn image_atlas_reset_warranted(atlas: &GlyphAtlas) -> bool {
    !atlas.is_empty()
        || (atlas.width(), atlas.height()) != (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
}

/// Atlas-local UV identity qualified by its device and renderer-owned allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GlyphContentStamp {
    pub(super) device_generation: u64,
    pub(super) allocation_generation: u64,
    pub(super) content_identity: u64,
    /// The atlas's monotonic growth count, so a growth is told apart from a reset or eviction.
    pub(super) growths: u64,
}

impl GlyphContentStamp {
    pub(super) fn capture(
        device_generation: u64,
        allocation_generation: u64,
        atlas: &GlyphAtlas,
    ) -> Self {
        Self {
            device_generation,
            allocation_generation,
            content_identity: atlas.identity(),
            growths: atlas.growths(),
        }
    }
}

/// Whether the atlas changed during a frame only by growing: same device, same allocation, no
/// eviction since `frame_evictions`, and a higher growth count. Growth moves no tile, so the
/// frame retries without resetting the atlas or disabling eviction.
pub(super) fn growth_only_change(
    before: GlyphContentStamp,
    after: GlyphContentStamp,
    frame_evictions: u64,
    current_evictions: u64,
) -> bool {
    before.device_generation == after.device_generation
        && before.allocation_generation == after.allocation_generation
        && frame_evictions == current_evictions
        && after.growths > before.growths
}

#[cfg(test)]
#[path = "core_atlas_policy_tests.rs"]
mod core_atlas_policy_tests;
