//! Retention responsibilities of the renderer, preserving its single owner.

use super::*;

/// What one covered-window trim released, as request sizes rather than residency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrimReport {
    /// The device refused GPU work, so nothing was released.
    pub refused: bool,
    /// The frame texture's extent before the trim; unchanged under the software presenter.
    pub frame_texture_before: (u32, u32),
    /// Bytes both present buffers requested before the trim.
    pub present_buffer_bytes_before: u64,
    /// Requested device bytes the trim gave back: the frame texture minus its 1x1 replacement,
    /// plus each present buffer minus its initial-size replacement.
    pub gpu_released_requested_bytes: u64,
}

impl TrimReport {
    /// The report for a trim the device refused: nothing released.
    #[must_use]
    pub fn refused() -> Self {
        Self { refused: true, ..Self::default() }
    }
}

/// CPU-side storage a renderer holds, split by owning class.
///
/// Deliberately not a single total. The parts have different lifetimes
/// and remedies: atlases grow with content, row caches follow viewport churn,
/// a software frame is sized by the window, and the vertex scratch follows the
/// largest recent frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RendererRetention {
    /// Rasterized glyph pixels mirrored on the CPU, and resident entries.
    pub glyph_atlas: ResourceAmount,
    /// Decoded inline-image pixels mirrored on the CPU, and resident entries.
    pub image_atlas: ResourceAmount,
    /// Cached per-row glyph instances and decoration metadata.
    pub row_glyph_cache: ResourceAmount,
    /// Cached per-row background and decoration quads.
    pub row_quad_cache: ResourceAmount,
    /// Windows software presentation buffer. Zero elsewhere.
    pub software_frame: ResourceAmount,
    /// Reused per-frame vertex assembly storage of the presentation pipeline, plus both atlas
    /// uploads' dirty and coalesced rect lists. CPU memory, so the GPU-buffer exclusion does not
    /// cover it; one item while the vertex scratch holds an allocation.
    pub vertex_scratch: ResourceAmount,
    /// Per-row ink records of the presented frame and one frame's staging; items are records.
    pub row_ink: ResourceAmount,
    /// The reused per-frame draw vectors held between frames; items are allocated vectors.
    pub frame_scratch: ResourceAmount,
    /// Kept tab titles and chrome runs plus the UI palette's colors; items are kept runs.
    pub chrome_cache: ResourceAmount,
}

impl RendererRetention {
    /// Class-tagged parts, for checking that every part is accounted for.
    ///
    /// **Nothing charges these.** `sonicterm-gpu` declares no dependency on
    /// `sonicterm-resource`, so this crate cannot reserve against a governor
    /// at all, and the renderer's memory reaches the app as a report rather
    /// than a ledger entry. The `ResourceClass` tags exist so a part cannot be
    /// added to this struct without deciding what it is.
    ///
    /// Wiring this to charging is not a small change, and the reason is here
    /// rather than in the caller that would attempt it: `image_atlas` maps to
    /// `InlineMediaRetained`, which a pane's decoded media already uses. They
    /// are different resident allocations — `capacity()` of one contiguous
    /// atlas buffer versus summed `len()` across separately-owned per-image
    /// `Vec`s — so charging both under one class would make the class mean two
    /// things and leave a reader unable to tell which allocation to act on.
    #[must_use]
    pub fn seam_classes(&self) -> [(ResourceClass, ResourceAmount); 9] {
        [
            (ResourceClass::GlyphAtlas, self.glyph_atlas),
            (ResourceClass::InlineMediaRetained, self.image_atlas),
            (ResourceClass::RowGlyphCache, self.row_glyph_cache),
            (ResourceClass::RowQuadCache, self.row_quad_cache),
            (ResourceClass::SoftwareFrame, self.software_frame),
            // The vertex scratch is CPU storage staged for the vertex-buffer upload.
            (ResourceClass::UploadStaging, self.vertex_scratch),
            (ResourceClass::RowInk, self.row_ink),
            (ResourceClass::FrameScratch, self.frame_scratch),
            (ResourceClass::ChromeCache, self.chrome_cache),
        ]
    }

    /// Sum of every part.
    #[must_use]
    pub fn total(&self) -> ResourceAmount {
        [
            self.glyph_atlas,
            self.image_atlas,
            self.row_glyph_cache,
            self.row_quad_cache,
            self.software_frame,
            self.vertex_scratch,
            self.row_ink,
            self.frame_scratch,
            self.chrome_cache,
        ]
        .into_iter()
        .fold(ResourceAmount::default(), |acc, part| ResourceAmount {
            bytes: acc.bytes.saturating_add(part.bytes),
            items: acc.items.saturating_add(part.items),
        })
    }
}

impl GpuRenderer {
    /// Update the cached "is the OS window focused" flag. Hides the
    /// text cursor when `false`. Bumps the FrameKey via
    /// `Self::last_frame_key` so the next render is not skipped by
    /// the cache.
    /// Host-side storage this renderer holds, split by the class that owns it.
    ///
    /// Every figure here already existed and was unreachable from outside the
    /// crate: `GlyphAtlas::retained_amount` and
    /// `SoftwareFrame::retained_bytes` were both written, tested, and
    /// called by nothing. What was missing was a way for the owner of the
    /// governor to read them, which is what this provides.
    ///
    /// **CPU-side only.** GPU textures and buffers are not included: their
    /// memory belongs to the driver, `wgpu` exposes no size accounting for
    /// them, and a figure invented here would be a guess presented as a
    /// measurement. The atlases are the CPU mirrors that back those textures,
    /// so they track the same content without claiming to measure VRAM.
    #[must_use]
    pub fn retained_amounts(&self) -> RendererRetention {
        RendererRetention {
            glyph_atlas: self.glyph_atlas.retained_amount(),
            image_atlas: self.image_atlas.retained_amount(),
            row_glyph_cache: self.row_glyph_cache.retained_amount(),
            row_quad_cache: self.line_quad_cache.retained_amount(),
            software_frame: self.software_frame_retained_amount(),
            vertex_scratch: self.upload_staging_retained(),
            row_ink: self.row_ink.retained_amount(),
            frame_scratch: self.frame_scratch.retained_amount(),
            chrome_cache: ResourceAmount {
                bytes: self.chrome_caches.retained_bytes(),
                items: self.chrome_caches.items(),
            },
        }
    }
}

impl GpuRenderer {
    /// The glyph atlas's size, packing, growth, eviction and fit facts.
    #[must_use]
    pub fn glyph_atlas_facts(&self) -> GlyphAtlasFacts {
        GlyphAtlasFacts::of(&self.glyph_atlas)
    }
}

impl GpuRenderer {
    /// The `UploadStaging` part: the vertex scratch plus both atlas uploads' CPU storage.
    pub(super) fn upload_staging_retained(&self) -> ResourceAmount {
        upload_staging_amount(
            self.present_pipeline.vertex_scratch_retained(),
            &self.glyph_upload,
            &self.image_upload,
        )
    }
}

impl GpuRenderer {
    /// Release one permanently removed pane's cached rows without evicting peers.
    ///
    /// Both caches and retention snapshots are event-loop-owned through `&mut
    /// GpuRenderer`, so the fixed glyph-then-quad order is observed atomically by
    /// every app caller. Cross-window detach calls this for the source renderer;
    /// same-renderer reorder keeps its entries because no pane leaves that owner.
    pub fn invalidate_pane_caches(&mut self, pane_id: u64) {
        self.row_glyph_cache.invalidate_pane(pane_id);
        self.line_quad_cache.invalidate_pane(pane_id);
        self.row_ink.drop_pane(pane_id);
    }
}

impl GpuRenderer {
    #[cfg(windows)]
    pub(super) fn software_frame_retained_amount(&self) -> ResourceAmount {
        self.software_frame.as_ref().map_or_else(ResourceAmount::default, |frame| ResourceAmount {
            bytes: frame.retained_bytes(),
            items: usize::from(frame.retained_bytes() > 0),
        })
    }
}

impl GpuRenderer {
    /// Non-Windows builds have no software presentation path, so this is
    /// always zero rather than absent — a caller charging it should not need
    /// a platform branch to do so.
    #[cfg(not(windows))]
    pub(super) fn software_frame_retained_amount(&self) -> ResourceAmount {
        ResourceAmount::default()
    }
}

impl GpuRenderer {
    /// Release what a covered window does not need until it is shown again, inside the device
    /// gate. A refused admission changes nothing; once admitted, the releases are not rolled back.
    ///
    /// The glyph atlas, its GPU texture, the retry and eviction state, the software frame and the
    /// preedit cache are kept. The next frame is a full first frame.
    pub fn trim_for_occlusion(&mut self) -> TrimReport {
        let Some(_scope) = self.device_errors.enter_gpu_work("trim") else {
            // When: enter_gpu_work refuses, the stopped device keeps every resource untouched.
            return TrimReport::refused();
        };
        let frame_texture_before = self.frame_texture_extent();
        let present_buffer_bytes_before = self.present_pipeline.present_buffer_bytes();
        let mut released_bytes = 0_u64;
        if !self.uses_windows_software_presenter() {
            // The GPU presenter's texture is surface-sized; the software presenter's is already 1x1.
            let (frame_texture, frame_view) = build_frame_texture(
                &self.device,
                false,
                1,
                1,
                self.config.format,
                self.retained_frame_readback,
            );
            self.frame_texture = frame_texture;
            self.frame_view = frame_view;
            self.frame_texture_trimmed = true;
            self.record_frame_texture_request();
            released_bytes += frame_texture_payload_bytes(frame_texture_before)
                .saturating_sub(frame_texture_payload_bytes((1, 1)));
        }
        released_bytes += self.present_pipeline.reset_to_initial(&self.device);
        self.row_glyph_cache.release_all();
        self.line_quad_cache.release_all();
        self.row_ink.release_all();
        self.glyph_upload.release_scratch();
        self.image_upload.release_scratch();
        self.frame_scratch.release_held();
        self.chrome_caches.release_runs();
        self.release_image_atlas_for_trim();
        self.last_frame_key = None;
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            frame_texture_width = frame_texture_before.0,
            frame_texture_height = frame_texture_before.1,
            present_buffer_bytes_before,
            gpu_released_requested_bytes = released_bytes,
            reason = "occlusion_trim",
            "covered window trimmed"
        );
        TrimReport {
            refused: false,
            frame_texture_before,
            present_buffer_bytes_before,
            gpu_released_requested_bytes: released_bytes,
        }
    }
}

#[cfg(test)]
#[path = "core_retention_tests.rs"]
mod core_retention_tests;
