//! sonic-shared — windowing, tab/pane tree, app loop.
//!
//! This crate contains everything that is platform-agnostic but UI-shaped:
//! the [`tabs::TabBar`] model, [`pane::PaneTree`] for splits, and an
//! [`app::App`] that ties [`sonic_core`] (engine) to `winit` + `wgpu`.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod app;
pub mod atlas_upload;
pub mod command_label;
pub mod command_palette;
pub mod config_watch;
pub mod cursor;
pub mod i18n;
pub mod ime;
pub mod menu;
pub mod menubar_bridge;
pub mod os_drag;
pub mod os_drag_bridge;
pub mod overlays;
pub mod pane;
pub mod prefs;
pub mod prefs_renderer;
pub mod quad;
pub mod render;
pub mod search;
pub mod selection;
pub mod tab_drag;
pub mod tab_title;
pub mod tabbar_view;
pub mod tabs;
pub mod text_pipeline;
pub mod ui_tokens;

// Re-exports from the extracted `sonic-text` crate so legacy import paths
// (`sonic_shared::shape::*`, `sonic_shared::glyph_atlas::*`, etc.) keep
// compiling unchanged. New code should depend on `sonic-text` directly.
pub use sonic_text::{glyph_atlas, row_glyph_cache, shape, swash_rasterizer};

/// Re-exports for binary crates.
pub use app::run;
pub use app::{run_with, KeymapLoader, ThemeLoader};

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
