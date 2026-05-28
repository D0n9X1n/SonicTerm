//! Renderer module — split for readability (issue #143).
//!
//! `core` holds the `GpuRenderer` struct and all its `impl` blocks
//! (which must live in the same module as the struct to share private
//! fields). The sibling modules host the free-function helpers and
//! plain-data structs extracted from the original 3,600-LOC
//! `render.rs`. All public symbols are re-exported below so existing
//! `use sonic_shared::render::*` call sites keep working unchanged.

#![deny(missing_docs)]

mod core;

pub mod color;
pub mod cursor;
pub mod drag_chip;
pub mod metrics;
pub mod tab_spans;

pub use color::*;
pub use core::*;
pub use cursor::*;
pub use drag_chip::*;
pub use metrics::*;
pub use tab_spans::*;
