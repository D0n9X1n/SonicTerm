//! sonic-text — headless text layer.
//!
//! Houses everything needed to turn `(Cell, font, size) → rasterized glyph
//! masks in an atlas`: text shaping (cosmic-text), per-glyph rasterization
//! (swash), the GPU-shaped atlas + LRU, and the per-row glyph cache.
//!
//! This crate is pure CPU — **no wgpu, no winit**. Downstream `sonic-gpu`
//! consumes [`GlyphInstance`] records produced here and uploads them.
//!
//! Imports of the form `sonic_shared::shape::*`, `sonic_shared::glyph_atlas::*`,
//! etc. continue to work via `pub use` re-exports in `sonic-shared`.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod glyph_atlas;
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
/// Sonic fonts whose OS/2 italic bit marks every generated face as Italic.
///
/// The override is intentionally narrow: only the Rec Mono St.Helens family we
/// ship is patched, and the desired style is inferred from its PostScript name.
pub fn load_font_data_with_sonic_overrides(font_system: &mut FontSystem, bytes: Vec<u8>) {
    let ids =
        font_system.db_mut().load_font_source(fontdb::Source::Binary(std::sync::Arc::new(bytes)));
    let fixes: Vec<(fontdb::ID, fontdb::Style)> = ids
        .iter()
        .filter_map(|id| {
            let face = font_system.db().face(*id)?;
            let style = sonic_bundled_font_style_override(face)?;
            Some((*id, style))
        })
        .collect();

    for (id, style) in fixes {
        let Some(face) = font_system.db().face(id).cloned() else { continue };
        let mut fixed = fontdb::FaceInfo { style, ..face };
        font_system.db_mut().remove_face(id);
        fixed.id = fontdb::ID::dummy();
        font_system.db_mut().push_face_info(fixed);
    }
}

fn sonic_bundled_font_style_override(face: &fontdb::FaceInfo) -> Option<fontdb::Style> {
    let family_match = face.families.iter().any(|(name, _)| name == "Rec Mono St.Helens");
    if !family_match {
        return None;
    }

    match face.post_script_name.as_str() {
        "RecMonoSt.Helens" | "RecMonoSt.Helens-Bold" => Some(fontdb::Style::Normal),
        "RecMonoSt.Helens-Italic" | "RecMonoSt.Helens-BoldItalic" => Some(fontdb::Style::Italic),
        _ => None,
    }
}

/// One drawable glyph in NDC space with its atlas UV rect and color.
///
/// This is the hand-off record between the CPU text layer and the GPU
/// text pass. It lives here (not in `sonic-gpu`) because the row-glyph
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
