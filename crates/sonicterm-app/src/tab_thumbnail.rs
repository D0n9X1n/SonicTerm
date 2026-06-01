//! Issue #296: render a small PNG preview for an OS-level tab drag.
//!
//! Pre-#296, `try_os_drag_handoff` passed `Vec::new()` as the
//! `drag_image_png` to every `OsTabDragBackend::begin_session` call —
//! so when a user dragged a tab out of the window via NSDraggingSession
//! / OLE `DoDragDrop`, the operating system rendered an empty (or
//! placeholder) drag preview with no visual connection to the tab the
//! user was actually moving.
//!
//! This module produces a small RGBA PNG (~200x40 px scaled to the
//! current DPI) that the platform backends pass to the OS as the drag
//! image. We deliberately rasterize a stylized tab shape rather than
//! attempting a full offscreen wgpu render — see the rationale below.
//!
//! ## Why not offscreen wgpu
//!
//! The original spec for this fix proposed creating a small wgpu
//! `RenderTarget`, re-rendering the dragged tab's quad + text into it,
//! reading pixels back via `Texture::copy_to_buffer`, and PNG-encoding
//! the result. That approach was rejected for three reasons:
//!
//! 1. **Threading.** `begin_session` runs on the winit main thread.
//!    Acquiring the wgpu `Device` + `Queue` + grabbing the shape /
//!    atlas locks during a drag start risks the AB-BA pattern that
//!    bit us in PR #36 and was caught by the §4 land-mine list. The
//!    renderer uses `try_lock` precisely so a drag burst does NOT
//!    deadlock the main thread — a synchronous offscreen render would
//!    re-introduce that hazard.
//!
//! 2. **Fontless thumbnail is fine.** The OS drag preview is ~200 px
//!    wide at 1x; the tab title rasterized at that size is largely
//!    decorative and reads as "a tab-shaped chip following the
//!    cursor." Solid color blocks + a color stripe convey the same
//!    "this is the tab you grabbed" affordance without dragging the
//!    text shaper into the drag start path.
//!
//! 3. **Cross-platform parity.** The Windows backend (`os_drag_win`)
//!    runs `DoDragDrop` synchronously inside `begin_session`; the
//!    macOS backend writes the pasteboard and immediately returns.
//!    Neither has a clean place to await an async GPU readback. A
//!    pure-CPU PNG generator returns bytes in microseconds and works
//!    identically on both.
//!
//! When/if a future PR lifts the threading constraint (e.g. by
//! pre-rendering the thumbnail at every tab activation and caching
//! the PNG on the tab itself), it can swap the body of
//! [`render_tab_thumbnail_png`] without touching any caller.

use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};

/// Logical (pre-DPI) width of the thumbnail in pixels.
pub const THUMB_LOGICAL_WIDTH: u32 = 200;
/// Logical (pre-DPI) height of the thumbnail in pixels.
pub const THUMB_LOGICAL_HEIGHT: u32 = 40;

/// Inputs the renderer needs to draw a recognisable tab chip.
///
/// All colors are sRGB `(r, g, b, a)` in `0..=255`. The caller is
/// expected to source these from the active theme + tab state — see
/// [`tab_thumbnail_inputs_from_payload`] for the default mapping used
/// when no theme info is plumbed through.
#[derive(Debug, Clone)]
pub struct TabThumbnailInputs {
    /// Tab title (currently unused for rendering — kept on the struct
    /// so a future text-capable renderer can pick it up without a
    /// caller-side change).
    pub title: String,
    /// Background color of the tab body.
    pub bg: (u8, u8, u8, u8),
    /// Accent / active-marker color drawn as a stripe along the top
    /// edge so the chip reads as a tab even without text.
    pub accent: (u8, u8, u8, u8),
    /// Border color for a 1-px outline around the chip.
    pub border: (u8, u8, u8, u8),
    /// HiDPI scale factor (1.0 on standard displays, 2.0 on Retina,
    /// etc.). Pixel dimensions of the resulting PNG are
    /// `THUMB_LOGICAL_* * scale_factor`, rounded.
    pub scale_factor: f32,
}

impl Default for TabThumbnailInputs {
    fn default() -> Self {
        // Tokyo Night-ish defaults so the chip never renders as a
        // solid black square if a caller forgets to fill the struct.
        Self {
            title: String::new(),
            bg: (0x1a, 0x1b, 0x26, 0xff),
            accent: (0x7a, 0xa2, 0xf7, 0xff),
            border: (0x41, 0x48, 0x68, 0xff),
            scale_factor: 1.0,
        }
    }
}

/// Convenience: produce a [`TabThumbnailInputs`] from just a payload
/// title using the default palette. Used by `tear_out.rs` until a
/// future PR plumbs theme colors through.
pub fn tab_thumbnail_inputs_from_payload(title: &str, scale_factor: f32) -> TabThumbnailInputs {
    TabThumbnailInputs {
        title: title.to_string(),
        scale_factor: scale_factor.clamp(0.5, 8.0),
        ..TabThumbnailInputs::default()
    }
}

/// Render the tab chip into a PNG byte vector.
///
/// The PNG always carries the standard 8-byte PNG signature
/// (`89 50 4E 47 0D 0A 1A 0A`) so the platform backends can sanity-check
/// it without parsing.
///
/// Returns an empty `Vec<u8>` on encoder failure rather than panicking
/// — the caller (`try_os_drag_handoff`) treats an empty buffer as
/// "no preview, proceed without one," matching the pre-#296 behavior.
pub fn render_tab_thumbnail_png(input: &TabThumbnailInputs) -> Vec<u8> {
    let width = (THUMB_LOGICAL_WIDTH as f32 * input.scale_factor).round().max(8.0) as u32;
    let height = (THUMB_LOGICAL_HEIGHT as f32 * input.scale_factor).round().max(8.0) as u32;

    // Stripe height scales with DPI so the visual proportion stays
    // constant.
    let stripe_h = ((3.0 * input.scale_factor).round() as u32).max(1);
    let border_w = ((1.0 * input.scale_factor).round() as u32).max(1);

    let mut buf: Vec<u8> = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let is_top_stripe = y < stripe_h;
            let is_border = x < border_w
                || x >= width.saturating_sub(border_w)
                || y < border_w
                || y >= height.saturating_sub(border_w);
            let (r, g, b, a) = if is_top_stripe {
                input.accent
            } else if is_border {
                input.border
            } else {
                input.bg
            };
            buf.push(r);
            buf.push(g);
            buf.push(b);
            buf.push(a);
        }
    }

    let mut out: Vec<u8> = Vec::new();
    let encoder = PngEncoder::new(&mut out);
    match encoder.write_image(&buf, width, height, ColorType::Rgba8.into()) {
        Ok(()) => out,
        Err(e) => {
            tracing::warn!(?e, "tab_thumbnail: PNG encode failed; returning empty preview");
            Vec::new()
        }
    }
}

/// PNG magic-number signature. Exposed for tests and for backends
/// that want a cheap sanity-check before handing the bytes to NSImage
/// / `CreateStreamOnHGlobal`.
pub const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// `true` if `bytes` starts with the standard PNG signature.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= PNG_SIGNATURE.len() && bytes[..PNG_SIGNATURE.len()] == PNG_SIGNATURE
}
