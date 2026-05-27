//! Stable per-glyph identity used by the GPU glyph atlas.
//!
//! The type itself now lives in `sonic-types::glyph_key` so non-engine
//! crates can use it without depending on `sonic-core`. The full design
//! discussion (why color is NOT part of the key, font-slot fallback,
//! shaper-driven path, etc.) is retained on the type's rustdoc and in the
//! `sonic-types::glyph_key` module docs. This file is now a thin
//! re-export — every existing `use sonic_core::glyph_key::GlyphKey`
//! continues to compile unchanged.

pub use sonic_types::GlyphKey;
