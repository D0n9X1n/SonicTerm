//! Requested GPU bytes per renderer, by resource class.
//!
//! wgpu reports the size a resource was requested at, not the physical memory
//! the driver keeps for it: a driver may pad, round, compress, defer release or
//! hold a surface's drawables in the compositor. This ledger therefore counts
//! **requests**. A figure here answers "what did this renderer ask the device
//! for", never "what does the GPU hold"; the OS figure in
//! `sonicterm_logging::graphics_memory` is the other half of that comparison.
//!
//! Every resource the renderer owns has one slot. Installing a resource into a
//! slot releases whatever the slot held and creates the new request, so a
//! rebuild, a resize or a covered-window trim moves the live figure by exactly
//! the difference and counts one release and one creation. A renderer's drop
//! releases every slot, so a ledger that outlives it reads zero live bytes.
//!
//! The surface's drawable count is exact only where wgpu fixes it from the
//! configured latency (Metal, DX12). Vulkan clamps latency to a surface
//! capability range wgpu does not expose and may allocate more images than it
//! requests, and GL ignores latency, so on those backends the surface figure is
//! an estimate and is marked as one.
//!
//! Not counted: the legacy `TextPipeline` and `QuadPipeline` instance buffers,
//! which no renderer builds in production, and the test-only retained-frame
//! readback buffer, which belongs to the test that copied the frame out.

use std::sync::{Arc, Mutex, PoisonError};

/// One kind of GPU resource a renderer requests, as the ledger reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GraphicsClass {
    /// The window surface's drawables: width x height x bytes per pixel x drawable count.
    Surface,
    /// The retained frame texture the presenter draws into and blits from.
    FrameTexture,
    /// The GPU mirror of the glyph atlas.
    GlyphAtlas,
    /// The GPU mirror of the inline-image atlas.
    ImageAtlas,
    /// The present pipeline's vertex, index and uniform buffers.
    PresentBuffers,
}

impl GraphicsClass {
    /// Every class, in report order.
    pub const ALL: [Self; 5] = [
        Self::Surface,
        Self::FrameTexture,
        Self::GlyphAtlas,
        Self::ImageAtlas,
        Self::PresentBuffers,
    ];

    /// The stable name a log field or report row uses for this class.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Surface => "surface",
            Self::FrameTexture => "frame_texture",
            Self::GlyphAtlas => "glyph_atlas",
            Self::ImageAtlas => "image_atlas",
            Self::PresentBuffers => "present_buffers",
        }
    }

    /// Position of this class in [`Self::ALL`] and in a snapshot's class array.
    const fn index(self) -> usize {
        match self {
            Self::Surface => 0,
            Self::FrameTexture => 1,
            Self::GlyphAtlas => 2,
            Self::ImageAtlas => 3,
            Self::PresentBuffers => 4,
        }
    }
}

/// One resource slot a renderer installs requests into; each class has one or more slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphicsSlot {
    /// The configured surface.
    Surface,
    /// The retained frame texture.
    FrameTexture,
    /// The glyph atlas texture.
    GlyphAtlas,
    /// The image atlas texture.
    ImageAtlas,
    /// The present vertex buffer.
    PresentVertices,
    /// The present index buffer.
    PresentIndices,
    /// The present uniform buffer.
    PresentUniform,
}

impl GraphicsSlot {
    /// Number of slots, the length of the ledger's slot array.
    const COUNT: usize = 7;

    /// The class this slot reports under.
    const fn class(self) -> GraphicsClass {
        match self {
            Self::Surface => GraphicsClass::Surface,
            Self::FrameTexture => GraphicsClass::FrameTexture,
            Self::GlyphAtlas => GraphicsClass::GlyphAtlas,
            Self::ImageAtlas => GraphicsClass::ImageAtlas,
            Self::PresentVertices | Self::PresentIndices | Self::PresentUniform => {
                GraphicsClass::PresentBuffers
            }
        }
    }

    /// Position of this slot in the ledger's slot array.
    const fn index(self) -> usize {
        match self {
            Self::Surface => 0,
            Self::FrameTexture => 1,
            Self::GlyphAtlas => 2,
            Self::ImageAtlas => 3,
            Self::PresentVertices => 4,
            Self::PresentIndices => 5,
            Self::PresentUniform => 6,
        }
    }
}

/// One live request: its bytes, how many GPU resources it stands for, and whether the count is an
/// estimate rather than the request the backend makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct SlotRequest {
    bytes: u64,
    resources: u64,
    estimated: bool,
}

/// Requested bytes and resource counts of one class: live now, and created and released so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphicsTally {
    /// Requested bytes of the resources this class holds now.
    pub live_bytes: u64,
    /// Resources this class holds now; a surface counts each drawable.
    pub live_resources: u64,
    /// Requested bytes of every resource this class created since the renderer was built.
    pub created_bytes: u64,
    /// Resources this class created since the renderer was built.
    pub created_resources: u64,
    /// Requested bytes of every resource this class released since the renderer was built.
    pub released_bytes: u64,
    /// Resources this class released since the renderer was built.
    pub released_resources: u64,
}

/// A renderer's requested GPU bytes by class, at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphicsLedgerSnapshot {
    /// One tally per class, in [`GraphicsClass::ALL`] order.
    pub classes: [GraphicsTally; GraphicsClass::ALL.len()],
    /// The live surface's drawable count is an estimate (Vulkan, GL), not the backend's request.
    pub surface_estimated: bool,
}

impl GraphicsLedgerSnapshot {
    /// The tally of `class`.
    #[must_use]
    pub fn class(&self, class: GraphicsClass) -> GraphicsTally {
        self.classes[class.index()]
    }

    /// Requested bytes every class holds now.
    #[must_use]
    pub fn live_bytes(&self) -> u64 {
        self.classes.iter().fold(0_u64, |total, tally| total.saturating_add(tally.live_bytes))
    }

    /// Resources every class holds now.
    #[must_use]
    pub fn live_resources(&self) -> u64 {
        self.classes.iter().fold(0_u64, |total, tally| total.saturating_add(tally.live_resources))
    }

    /// `class=bytes/resources` per class, from the tally field `pick` selects, space-separated.
    #[must_use]
    pub fn render_per_class(&self, pick: impl Fn(&GraphicsTally) -> (u64, u64)) -> String {
        GraphicsClass::ALL
            .iter()
            .map(|class| {
                let (bytes, resources) = pick(&self.class(*class));
                format!("{}={bytes}/{resources}", class.as_str())
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `exact` or `estimated`: how the live surface's drawable count was established.
    #[must_use]
    pub const fn surface_count_kind(&self) -> &'static str {
        if self.surface_estimated {
            "estimated"
        } else {
            // When: `surface_estimated` is false, the count is the backend's own request.
            "exact"
        }
    }
}

/// The ledger behind a [`GraphicsLedgerHandle`]: the live request per slot and the tally per class.
#[derive(Debug, Default)]
struct GraphicsLedger {
    slots: [Option<SlotRequest>; GraphicsSlot::COUNT],
    snapshot: GraphicsLedgerSnapshot,
}

impl GraphicsLedger {
    /// Release what `slot` holds, if anything, and leave it empty.
    fn release(&mut self, slot: GraphicsSlot) {
        let Some(request) = self.slots[slot.index()].take() else {
            // When: the slot is already empty, there is nothing to release.
            return;
        };
        let tally = &mut self.snapshot.classes[slot.class().index()];
        tally.live_bytes = tally.live_bytes.saturating_sub(request.bytes);
        tally.live_resources = tally.live_resources.saturating_sub(request.resources);
        tally.released_bytes = tally.released_bytes.saturating_add(request.bytes);
        tally.released_resources = tally.released_resources.saturating_add(request.resources);
        if slot == GraphicsSlot::Surface {
            // The surface slot alone can hold an estimate; an empty slot holds none.
            self.snapshot.surface_estimated = false;
        }
    }

    /// Release what `slot` holds and create `request` in its place.
    fn replace(&mut self, slot: GraphicsSlot, request: SlotRequest) {
        self.release(slot);
        let tally = &mut self.snapshot.classes[slot.class().index()];
        tally.live_bytes = tally.live_bytes.saturating_add(request.bytes);
        tally.live_resources = tally.live_resources.saturating_add(request.resources);
        tally.created_bytes = tally.created_bytes.saturating_add(request.bytes);
        tally.created_resources = tally.created_resources.saturating_add(request.resources);
        if slot == GraphicsSlot::Surface {
            // The surface slot's request decides whether its live count is an estimate.
            self.snapshot.surface_estimated = request.estimated;
        }
        self.slots[slot.index()] = Some(request);
    }
}

/// A renderer's graphics ledger, shared with the present pipeline that grows its own buffers.
///
/// Cloning shares the same ledger. Every operation takes the one lock briefly and never calls
/// out while holding it, so it nests under no other lock.
#[derive(Debug, Clone, Default)]
pub struct GraphicsLedgerHandle {
    inner: Arc<Mutex<GraphicsLedger>>,
}

impl GraphicsLedgerHandle {
    /// Install a request of `bytes` standing for `resources` GPU resources into `slot`, releasing
    /// what the slot held.
    pub(crate) fn replace(&self, slot: GraphicsSlot, bytes: u64, resources: u64) {
        self.replace_request(slot, bytes, resources, false);
    }

    /// [`Self::replace`] for a request whose resource count may be an estimate.
    pub(crate) fn replace_request(
        &self,
        slot: GraphicsSlot,
        bytes: u64,
        resources: u64,
        estimated: bool,
    ) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(slot, SlotRequest { bytes, resources, estimated });
    }

    /// Release every slot, as a renderer's drop does.
    pub(crate) fn release_all(&self) {
        let mut ledger = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        for slot in [
            GraphicsSlot::Surface,
            GraphicsSlot::FrameTexture,
            GraphicsSlot::GlyphAtlas,
            GraphicsSlot::ImageAtlas,
            GraphicsSlot::PresentVertices,
            GraphicsSlot::PresentIndices,
            GraphicsSlot::PresentUniform,
        ] {
            ledger.release(slot);
        }
    }

    /// The ledger's tallies now.
    #[must_use]
    pub fn snapshot(&self) -> GraphicsLedgerSnapshot {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).snapshot
    }
}

/// A surface's drawable count, and whether it is the backend's own request or an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceDrawables {
    /// Drawables the surface holds or requests.
    pub count: u64,
    /// True when the backend does not fix the count from the configured latency.
    pub estimated: bool,
}

/// Drawables a surface configured with `desired_latency` holds on `backend`.
///
/// wgpu-core 30.0.1 clamps `desired_maximum_frame_latency` to the adapter's supported range
/// (`device/resource.rs`, `configure_surface`), then each backend sizes the swapchain from it:
/// - Metal: range `1..=2` on macOS 10.13.2+ (`wgpu-hal` 30.0.1 `metal/adapter.rs`), and
///   `setMaximumDrawableCount(maximum_frame_latency + 1)` (`metal/surface.rs`): exact, 2 or 3.
/// - DX12: range `1..=16`, and `BufferCount = (maximum_frame_latency + 1).min(16)`
///   (`dx12/adapter.rs`, `dx12/mod.rs`): exact.
/// - Vulkan: range `(min_image_count - 1)..=(max_image_count - 1)` of the surface's capabilities
///   (`vulkan/swapchain/native.rs`), then `min_image_count(maximum_frame_latency + 1)`, a minimum
///   the driver may exceed. wgpu's public `SurfaceCapabilities` omits that range, so production
///   passes `vulkan_image_range: None` and gets latency + 1 as an estimate; a known range clamps.
/// - GL and others: latency is ignored (`gles/adapter.rs` lists it as unused), so the count is
///   estimated as double buffering, 2, whatever the latency.
#[must_use]
pub fn surface_drawables(
    backend: wgpu::Backend,
    desired_latency: u32,
    vulkan_image_range: Option<(u32, u32)>,
) -> SurfaceDrawables {
    let exact = |count: u32| SurfaceDrawables { count: u64::from(count), estimated: false };
    let estimate = |count: u32| SurfaceDrawables { count: u64::from(count), estimated: true };
    match backend {
        wgpu::Backend::Metal => exact(desired_latency.clamp(1, 2) + 1),
        wgpu::Backend::Dx12 => exact((desired_latency.clamp(1, 16) + 1).min(16)),
        wgpu::Backend::Vulkan => match vulkan_image_range {
            Some((min_images, max_images)) => {
                let min_latency = min_images.max(1) - 1;
                let max_latency = max_images.max(min_images.max(1)) - 1;
                // The requested minimum is exact; the driver may still create more images.
                estimate(desired_latency.clamp(min_latency, max_latency) + 1)
            }
            None => estimate(desired_latency.max(1) + 1),
        },
        _ => estimate(2),
    }
}

/// Requested bytes of one drawable of a `width` x `height` surface in `format`; four bytes per
/// pixel when the format has no single block copy size.
#[must_use]
pub fn surface_drawable_bytes(width: u32, height: u32, format: wgpu::TextureFormat) -> u64 {
    let bytes_per_pixel = format.block_copy_size(None).unwrap_or(4);
    u64::from(width) * u64::from(height) * u64::from(bytes_per_pixel)
}

#[cfg(test)]
#[path = "graphics_ledger_tests.rs"]
mod graphics_ledger_tests;
