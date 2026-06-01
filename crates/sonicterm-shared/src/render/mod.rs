//! Renderer module — thin re-export shim (M7f).
//!
//! As of M7f, the entire `core` module (the `GpuRenderer` struct + all
//! its `impl` blocks, plus its free-function helpers) lives in
//! [`sonicterm_gpu::core`]. This module exists only so legacy import
//! paths of the form `sonicterm_shared::render::*` keep compiling
//! against the deprecated `sonicterm-shared` façade; new code should
//! import from `sonicterm_gpu` (and its sibling crates) directly.

#![allow(missing_docs)]
#![allow(deprecated)]

#[deprecated(since = "0.9.0", note = "import from sonicterm_gpu::color directly")]
pub use sonicterm_gpu::color;
#[deprecated(since = "0.9.0", note = "import from sonicterm_gpu::core directly")]
pub use sonicterm_gpu::core;
#[deprecated(since = "0.9.0", note = "import from sonicterm_gpu::cursor directly")]
pub use sonicterm_gpu::cursor;
#[deprecated(since = "0.9.0", note = "import from sonicterm_gpu::row_quad_cache directly")]
pub use sonicterm_gpu::row_quad_cache;
#[deprecated(since = "0.9.0", note = "import from sonicterm_render_model::geometry directly")]
pub use sonicterm_render_model::geometry;
#[deprecated(since = "0.9.0", note = "use sonicterm_text::metrics directly")]
pub use sonicterm_text::metrics;
#[deprecated(since = "0.9.0", note = "import from sonicterm_ui::drag_chip directly")]
pub use sonicterm_ui::drag_chip;
#[deprecated(since = "0.9.0", note = "import from sonicterm_ui::tab_spans directly")]
pub use sonicterm_ui::tab_spans;
#[deprecated(since = "0.9.0", note = "use sonicterm_ui::tabbar_view directly")]
pub use sonicterm_ui::tabbar_view::{tab_bar_top_inset, tab_bar_top_inset_with_titlebar};

pub use color::*;
pub use core::*;
pub use cursor::*;
pub use drag_chip::*;
pub use geometry::*;
pub use metrics::*;
pub use tab_spans::*;
