//! sonicterm-shared — GPU rendering scaffolding shared by the app loop.
//!
//! Historically this crate held the entire app surface (winit loop, OS
//! drag, menubar bridge, tabs, panes, etc.). PR 8a of the workspace
//! refactor split the app loop out into [`sonicterm_app`]; this crate now
//! owns just the renderers (and the asset-directory probe shared by the
//! app and the live-reload path).
//!
//! Pre-PR-5 this crate also held UI-shaped state (tabs, panes, palette,
//! overlays, etc.); those modules now live in [`sonicterm_ui`]
//! and are re-exported here so legacy imports of the form
//! `use sonicterm_shared::tabs::TabBar;` continue to work unchanged.

// TODO: add per-item docs and switch to #![deny(missing_docs)] in a follow-up PR.
#![allow(missing_docs)]
#![allow(deprecated)]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod render;

// Re-exports from the extracted `sonicterm-gpu` crate (PR 7a of the workspace
// refactor). `sonicterm-gpu` owns the wgpu/glyphon/cosmic-text-touching pipeline
// primitives. Legacy `use sonicterm_shared::{quad, text_pipeline, atlas_upload};`
// imports keep working through these re-exports.
#[deprecated(
    since = "0.9.0",
    note = "use `sonicterm_gpu::{atlas_upload,quad,text_pipeline}` directly"
)]
pub use sonicterm_gpu::{atlas_upload, quad, text_pipeline};

// Re-exports from the extracted `sonicterm-ui` crate (PR-5 of the workspace
// refactor, issue #121). `sonicterm-ui` owns pure UI state + layout with no
// winit / wgpu / glyphon deps.
#[deprecated(since = "0.9.0", note = "use `sonicterm_ui::*` directly")]
pub use sonicterm_ui::{
    cheatsheet, command_label, command_palette, copy_mode, cursor, i18n, ime, overlays, pane,
    search, selection, tab_title, tabbar_view, tabs, ui_tokens,
};

// Re-exports from the extracted `sonicterm-text` crate so legacy import paths
// (`sonicterm_shared::shape::*`, `sonicterm_shared::glyph_atlas::*`, etc.) keep
// compiling unchanged. New code should depend on `sonicterm-text` directly.
#[deprecated(since = "0.9.0", note = "use `sonicterm_text::*` directly")]
pub use sonicterm_text::{glyph_atlas, row_glyph_cache, shape, swash_rasterizer};

/// Locate the bundled `assets/` directory.
///
/// As of issue #469 PR-C-residual this lives in
/// [`sonicterm_cfg::assets::asset_dir`]; this façade just re-exports it so that
/// legacy `sonicterm_shared::asset_dir()` callers keep compiling.
#[deprecated(since = "0.9.0", note = "use `sonicterm_cfg::assets::asset_dir` directly")]
pub fn asset_dir() -> std::path::PathBuf {
    sonicterm_cfg::assets::asset_dir()
}
