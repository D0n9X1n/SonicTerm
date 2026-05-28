//! Platform-agnostic value types shared across Sonic crates.
//!
//! This crate carries only **data types** (cells, colors, positions, actions,
//! glyph keys, hyperlink ids). It deliberately has **no** runtime dependencies
//! on rendering, ptys, or windowing — any crate that just needs the value
//! shapes can depend on `sonic-types` without pulling in the engine.
//!
//! All previous import paths in `sonic-core` etc. continue to work via
//! `pub use` re-exports, so this is a zero-behavior-change move.

#![deny(missing_docs)]

pub mod action;
pub mod cell;
pub mod geom;
pub mod glyph_key;
pub mod hyperlink_id;

pub use action::{Action, Direction, ScrollAction};
pub use cell::{Cell, CellFlags, Color};
pub use geom::Pos;
pub use glyph_key::GlyphKey;
pub use hyperlink_id::HyperlinkId;
