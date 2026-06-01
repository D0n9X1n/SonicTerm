//! Renderer module — split for readability (issue #143).
//!
//! `core` holds the `GpuRenderer` struct and all its `impl` blocks
//! (which must live in the same module as the struct to share private
//! fields). The sibling modules host the free-function helpers and
//! plain-data structs extracted from the original 3,600-LOC
//! `render.rs`. All public symbols are re-exported below so existing
//! `use sonicterm_shared::render::*` call sites keep working unchanged.

#![deny(missing_docs)]

mod core;

#[deprecated(since = "0.9.0", note = "import from sonicterm_gpu::color directly")]
pub use sonicterm_gpu::color;
pub mod cursor;
pub mod drag_chip;
#[deprecated(since = "0.9.0", note = "import from sonicterm_render_model::geometry directly")]
pub use sonicterm_render_model::geometry;
#[deprecated(since = "0.9.0", note = "use sonicterm_text::metrics directly")]
pub use sonicterm_text::metrics;
#[deprecated(since = "0.9.0", note = "use sonicterm_ui::tabbar_view directly")]
pub use sonicterm_ui::tabbar_view::{tab_bar_top_inset, tab_bar_top_inset_with_titlebar};
pub mod row_quad_cache;
pub mod tab_spans;

pub use color::*;
pub use core::*;
pub use cursor::*;
pub use drag_chip::*;
pub use geometry::*;
#[allow(deprecated)]
pub use metrics::*;
pub use tab_spans::*;
