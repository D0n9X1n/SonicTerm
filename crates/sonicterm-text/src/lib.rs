//! sonicterm-text — headless text layer.
//!
//! Houses everything needed to turn `(Cell, font, size) → rasterized glyph
//! masks in an atlas`: text shaping (cosmic-text), per-glyph rasterization
//! (swash), the GPU-shaped atlas + LRU, and the per-row glyph cache.
//!
//! This crate is pure CPU — **no wgpu, no winit**. Downstream `sonicterm-gpu`
//! consumes [`GlyphInstance`] records produced here and uploads them.
//!
//! Imports of the form `sonicterm_text::shape::*`, `sonicterm_text::glyph_atlas::*`,
//! etc. continue to work via `pub use` re-exports in `sonicterm-shared`.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod async_fallback;
pub mod block_element_geometry;
pub mod box_drawing_geometry;
pub mod glyph_atlas;
pub mod metrics;
pub mod prewarm;
pub mod row_glyph_cache;
pub mod shape;
pub mod swash_rasterizer;

use cosmic_text::{Attrs, Family, FontSystem};

/// Single source of truth for the [`Attrs`] used by every text-rendering
/// site (terminal grid, tab titles, command palette, search status bar,
/// IME pre-edit). Pass the user-configured `font.family` here so all UI
/// chrome shares the EXACT same `Family::Name(...)` as grid cells —
/// avoiding the historical bug where tab titles silently fell through
/// to `Family::Monospace` and rendered with a different installed face.
#[must_use]
pub fn terminal_font_attrs(family: &str) -> Attrs<'_> {
    Attrs::new().family(Family::Name(family))
}

/// Load a TTF/OTF payload into `font_system`, correcting metadata for bundled
/// SonicTerm fonts whose OS/2 fields lie about both italic flag AND weight.
///
/// The St.Helens TTF set ships with every face's OS/2 `fsSelection` flagged
/// Italic and weights of (400, 400, 600, 600) regardless of actual variant —
/// so a naive `fontdb` query keyed by `(style, weight)` returns either the
/// wrong face or nothing. WezTerm dodges this by routing by filename; we patch
/// the metadata at load-time so the rest of the rasterizer (and any code that
/// asks fontdb for "Rec Mono St.Helens Bold") gets the right answer.
///
/// The override is intentionally narrow: only the Rec Mono St.Helens family we
/// ship is patched, and the desired (style, weight) is inferred from its
/// PostScript name (which IS correct in the upstream TTFs).
///
/// Context: https://github.com/D0n9X1n/sonic/issues/419
pub fn load_font_data_with_sonic_overrides(font_system: &mut FontSystem, bytes: Vec<u8>) {
    let ids =
        font_system.db_mut().load_font_source(fontdb::Source::Binary(std::sync::Arc::new(bytes)));
    let fixes: Vec<(fontdb::ID, fontdb::Style, fontdb::Weight)> = ids
        .iter()
        .filter_map(|id| {
            let face = font_system.db().face(*id)?;
            let (style, weight) = sonic_bundled_font_metadata_override(face)?;
            Some((*id, style, weight))
        })
        .collect();

    for (id, style, weight) in fixes {
        let Some(face) = font_system.db().face(id).cloned() else { continue };
        let mut fixed = fontdb::FaceInfo { style, weight, ..face };
        font_system.db_mut().remove_face(id);
        fixed.id = fontdb::ID::dummy();
        font_system.db_mut().push_face_info(fixed);
    }

    // Drop any *non-bundled* (system-installed) St.Helens faces so fontdb
    // queries can't accidentally resolve to a face whose OS/2 metadata is
    // still wrong. Without this, a user with St.Helens installed system-wide
    // would see fontdb pick the File-source copy (broken style/weight)
    // ahead of our patched Binary-source copies. This is the runtime
    // equivalent of WezTerm's "route by filename" trick — we just keep only
    // the filenames we control.
    let stale_helens_ids: Vec<fontdb::ID> = font_system
        .db()
        .faces()
        .filter(|f| {
            f.families.iter().any(|(name, _)| name == "Rec Mono St.Helens")
                && !matches!(f.source, fontdb::Source::Binary(_))
        })
        .map(|f| f.id)
        .collect();
    for id in stale_helens_ids {
        font_system.db_mut().remove_face(id);
    }
}

fn sonic_bundled_font_metadata_override(
    face: &fontdb::FaceInfo,
) -> Option<(fontdb::Style, fontdb::Weight)> {
    let family_match = face.families.iter().any(|(name, _)| name == "Rec Mono St.Helens");
    if !family_match {
        return None;
    }

    // Route by PostScript name (set correctly in the upstream TTFs even
    // though the OS/2 italic bit + usWeightClass are wrong). This is the
    // same approach WezTerm uses for these faces.
    match face.post_script_name.as_str() {
        "RecMonoSt.Helens" => Some((fontdb::Style::Normal, fontdb::Weight::NORMAL)),
        "RecMonoSt.Helens-Bold" => Some((fontdb::Style::Normal, fontdb::Weight::BOLD)),
        "RecMonoSt.Helens-Italic" => Some((fontdb::Style::Italic, fontdb::Weight::NORMAL)),
        "RecMonoSt.Helens-BoldItalic" => Some((fontdb::Style::Italic, fontdb::Weight::BOLD)),
        _ => None,
    }
}

/// One drawable glyph in NDC space with its atlas UV rect and color.
///
/// This is the hand-off record between the CPU text layer and the GPU
/// text pass. It lives here (not in `sonicterm-gpu`) because the row-glyph
/// cache pre-builds vectors of these from shaping output, well before
/// any GPU work happens. The struct carries only `[f32; 4]` arrays so
/// it has no wgpu dependency.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlyphInstance {
    /// `[x, y, w, h]` in NDC (–1..1). `w`/`h` are signed because the
    /// Y axis flips between screen and NDC.
    pub rect: [f32; 4],
    /// `[u0, v0, u1, v1]` normalized atlas coordinates from
    /// `GlyphInfo::uv`.
    pub uv: [f32; 4],
    /// `[r, g, b, a]` foreground color the alpha is modulated by.
    /// For color glyphs (`flags.x >= 0.5`) this is ignored — the
    /// fragment shader returns the premultiplied texture sample
    /// directly so the emoji's own colors come through.
    pub color: [f32; 4],
    /// Per-instance flags packed into a vec4 to keep WGSL vertex
    /// attribute slots simple. `flags.x` is the is-color toggle
    /// (>= 0.5 → color glyph). The remaining components are reserved
    /// for future use (e.g. signed-distance-field weight, oblique
    /// shear) and currently always zero.
    pub flags: [f32; 4],
}
