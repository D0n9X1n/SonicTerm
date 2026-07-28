//! What a renderer retains on the CPU, reported per window.
//!
//! The renderer computes three real, measured host-side figures —
//! [`GpuRenderer::retained_amounts`] reads the capacity of the glyph atlas, the
//! image atlas, and (on Windows) the software presentation frame. Those figures
//! were computed and read by nothing: the crate that owns them cannot depend on
//! the governor, so the numbers stopped at the crate boundary and no report
//! carried them.
//!
//! On the Windows software path the frame buffer is the largest single
//! host-side buffer in the process, and it was invisible in exactly the report
//! a user opens to find it.
//!
//! This module closes that by *reading* rather than *charging*. The app already
//! owns the renderer and already depends on both crates, so the figures are
//! fetched here and logged; nothing is reserved against the governor, because
//! the renderer cannot charge and giving it that ability would invert the
//! dependency direction the crate boundary exists to hold.
//!
//! ## Why a separate line
//!
//! These figures are deliberately *not* folded into `session retention`:
//!
//! - The renderer's image atlas and a pane's decoded inline media are both
//!   inline-image memory, but they are **different allocations** — one is the
//!   CPU mirror backing a GPU texture, the other is the decoded source the
//!   pane holds. Summing them into one field would report the same picture
//!   twice under a single name.
//! - A renderer belongs to a **window**; `session retention` sums **panes**. A
//!   per-window figure inside a per-pane aggregate makes one line mean two
//!   things.
//! - The seam totals in that line are what memory-triage procedures parse. A
//!   new term in `total_bytes` silently changes what every one of them reports.
//!
//! ## Cost
//!
//! Reached only from the sampling path, behind both the `memory` debug-level
//! gate and the thirty-second interval. A default session pays nothing: the
//! level check short-circuits before this is reached. An investigating session
//! pays three capacity reads per window every thirty seconds.
//!
//! Nothing here may move onto the per-wake path. `retained_amounts` is cheap
//! today, but the sampling path is also what governs idle CPU, and an
//! accounting walk that looked cheap is exactly how that path became expensive
//! before.

use sonicterm_gpu::core::RendererRetention;

/// Emit one window's renderer retention to the memory log.
///
/// Fields are named literally rather than iterated from a class list. The
/// literal list is what makes these lines greppable and stable for the triage
/// procedures that read them, and iterating classes would additionally reuse
/// the inline-media class name for atlas memory that is not a pane's media.
///
/// `items` accompany the atlas byte figures because an atlas grows by resident
/// entries: bytes alone say an atlas is large, while the entry count
/// distinguishes a large glyph set from a small one held in an oversized
/// allocation. The software frame carries no useful entry count — it is one
/// buffer sized by the window — so only its bytes are reported.
///
/// `role` distinguishes a renderer the user can see from one held ready in the
/// warm pool. Both hold a full-size glyph atlas, but the remedy differs and a
/// line that did not say which it was would point a user at a window they
/// cannot close.
pub fn emit_renderer_retention(label: &str, role: &str, retention: &RendererRetention) {
    let total = retention.total();
    tracing::debug!(
        target: "memory",
        window = label,
        role,
        total_bytes = total.bytes,
        glyph_atlas_bytes = retention.glyph_atlas.bytes,
        glyph_atlas_items = retention.glyph_atlas.items,
        image_atlas_bytes = retention.image_atlas.bytes,
        image_atlas_items = retention.image_atlas.items,
        software_frame_bytes = retention.software_frame.bytes,
        "renderer retention"
    );
}

impl super::App {
    /// Report every renderer's retained CPU storage — live and warm alike.
    ///
    /// One line per renderer rather than a process total: a renderer is the
    /// unit that holds the memory, so a total that hid which one was holding it
    /// would name a number without naming the remedy.
    ///
    /// **Both collections are walked, and that is the point.** A warm renderer
    /// is fully constructed and holds the same full-size glyph atlas as a
    /// visible one — the pool exists so a new window opens without paying for
    /// it — so a report covering only visible windows would omit a live
    /// multi-megabyte buffer per pooled entry. It would also mislead: a user
    /// summing the lines would get a figure below what the process holds, and
    /// the remedy the visible lines imply (close a window) cannot reach a warm
    /// renderer at all. The lever for those is the warm-pool size.
    ///
    /// Windows without a renderer are skipped. A window exists briefly before
    /// its renderer does, and reporting a zero for it would be
    /// indistinguishable from a renderer that genuinely holds nothing. Warm
    /// entries own their renderer outright, so none is ever absent.
    pub(super) fn log_renderer_retention(&self) {
        for (window_id, window) in &self.windows {
            let Some(renderer) = window.renderer.as_ref() else { continue };
            emit_renderer_retention(
                &format!("{window_id:?}"),
                "visible",
                &renderer.retained_amounts(),
            );
        }

        for (index, warm) in self.warm_window_pool.iter().enumerate() {
            emit_renderer_retention(
                &format!("warm[{index}]"),
                "warm",
                &warm.renderer.retained_amounts(),
            );
        }
    }
}

#[cfg(test)]
#[path = "renderer_retention_tests.rs"]
mod renderer_retention_tests;
