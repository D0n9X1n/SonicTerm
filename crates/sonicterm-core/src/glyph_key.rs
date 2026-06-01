//! Stable per-glyph identity used by the GPU glyph atlas.
//!
//! The type itself now lives in `sonicterm-types::glyph_key` so non-engine
//! crates can use it without depending on `sonicterm-core`. The full design
//! discussion (why color is NOT part of the key, font-slot fallback,
//! shaper-driven path, etc.) is retained on the type's rustdoc and in the
//! `sonicterm-types::glyph_key` module docs. This file is now a thin
//! re-export — every existing `use sonicterm_core::glyph_key::GlyphKey`
//! continues to compile unchanged.

pub use sonicterm_types::GlyphKey;
