//! sonic-shared — windowing, tab/pane tree, app loop.
//!
//! This crate contains everything that is platform-agnostic but UI-shaped:
//! the [`tabs::TabBar`] model, [`pane::PaneTree`] for splits, and an
//! [`app::App`] that ties [`sonic_core`] (engine) to `winit` + `wgpu`.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod app;
pub mod command_palette;
pub mod config_watch;
pub mod glyph_atlas;
pub mod i18n;
pub mod ime;
pub mod menubar_bridge;
pub mod os_drag;
pub mod overlays;
pub mod pane;
pub mod prefs;
pub mod quad;
pub mod render;
pub mod search;
pub mod selection;
pub mod shape;
pub mod swash_rasterizer;
pub mod tab_drag;
pub mod tab_title;
pub mod tabbar_view;
pub mod tabs;
pub mod text_pipeline;

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
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets")
}
