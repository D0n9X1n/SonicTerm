//! sonic-shared — GPU rendering scaffolding shared by the app loop and
//! the prefs window.
//!
//! Historically this crate held the entire app surface (winit loop, OS
//! drag, menubar bridge, tabs, panes, etc.). PR 8a of the workspace
//! refactor split the app loop out into [`sonic_app`]; this crate now
//! owns just the renderers (and the asset-directory probe shared by the
//! app and the live-reload path).
//!
//! Pre-PR-5 this crate also held UI-shaped state (tabs, panes, palette,
//! prefs view, overlays, etc.); those modules now live in [`sonic_ui`]
//! and are re-exported here so legacy imports of the form
//! `use sonic_shared::tabs::TabBar;` continue to work unchanged.

// TODO: add per-item docs and switch to #![deny(missing_docs)] in a follow-up PR.
#![allow(missing_docs)]
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod prefs_renderer;
pub mod render;

// Re-exports from the extracted `sonic-gpu` crate (PR 7a of the workspace
// refactor). `sonic-gpu` owns the wgpu/glyphon/cosmic-text-touching pipeline
// primitives. Legacy `use sonic_shared::{quad, text_pipeline, atlas_upload};`
// imports keep working through these re-exports.
pub use sonic_gpu::{atlas_upload, quad, text_pipeline};

// Re-exports from the extracted `sonic-ui` crate (PR-5 of the workspace
// refactor, issue #121). `sonic-ui` owns pure UI state + layout with no
// winit / wgpu / glyphon deps.
pub use sonic_ui::{
    cheatsheet, command_label, command_palette, copy_mode, cursor, i18n, ime, overlays, pane,
    prefs, search, selection, tab_title, tabbar_view, tabs, ui_tokens,
};

// Re-exports from the extracted `sonic-text` crate so legacy import paths
// (`sonic_shared::shape::*`, `sonic_shared::glyph_atlas::*`, etc.) keep
// compiling unchanged. New code should depend on `sonic-text` directly.
pub use sonic_text::{glyph_atlas, row_glyph_cache, shape, swash_rasterizer};

/// Locate the bundled `assets/` directory: prefers
/// `<binary>/../Resources/assets` (macOS .app layout) and falls back to
/// the workspace-root `assets/` next to the source tree.
///
/// This lives here so that both the platform binary (one-shot at
/// startup) and the live-reload path (re-loading themes/keymaps on
/// `sonic.toml` change) compute the same path.
pub fn asset_dir() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos) = exe.parent() {
            if let Some(contents) = macos.parent() {
                let bundled = contents.join("Resources").join("assets");
                if bundled.exists() {
                    return bundled;
                }
            }
        }
    }
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}
