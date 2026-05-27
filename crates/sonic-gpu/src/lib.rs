//! sonic-gpu — GPU pipeline primitives split out of `sonic-shared` in PR 7a
//! of the workspace refactor (see `docs/specs/2026-04-22-workspace-refactor.md`).
//!
//! This crate owns the wgpu/glyphon/cosmic-text-touching primitives that the
//! terminal renderer composes per frame:
//!
//!   * [`quad`] — the WGSL quad pipeline + `QuadInstance` (cursor, selection,
//!     rounded-rect chrome, panel backgrounds, underlines).
//!   * [`text_pipeline`] — the instanced text pipeline that consumes
//!     `sonic_text::GlyphInstance` and samples the GPU glyph atlas.
//!   * [`atlas_upload`] — wgpu-side wrapper around `sonic_text::glyph_atlas`
//!     that owns the texture/view/sampler/bind-group and syncs dirty tiles.
//!
//! The composite renderer (`sonic-shared::render`) and the prefs window
//! renderer still live in `sonic-shared` for now; PR 7b will split `render.rs`
//! into sub-files and PR 7c may move the prefs renderer once its `crate::prefs`
//! dependency is decoupled.
//!
//! Dependency rule: `sonic-gpu` may depend on `sonic-types`, `sonic-text`, and
//! `sonic-render-model` only. It must NOT depend on `sonic-ui` or `sonic-shared`
//! — those depend on `sonic-gpu`, so a back-edge would create a cycle.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod atlas_upload;
pub mod quad;
pub mod text_pipeline;
