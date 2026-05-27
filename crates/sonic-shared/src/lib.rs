//! sonic-shared — windowing, app loop, GPU rendering.
//!
//! Pre-PR-5 this crate also held UI-shaped state (tabs, panes, palette,
//! prefs view, overlays, etc.); those modules now live in [`sonic_ui`]
//! and are re-exported here so legacy imports of the form
//! `use sonic_shared::tabs::TabBar;` continue to work unchanged.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod app;
pub mod atlas_upload;
pub mod config_watch;
pub mod menu;
pub mod menubar_bridge;
pub mod os_drag;
pub mod os_drag_bridge;
pub mod prefs_renderer;
pub mod quad;
pub mod render;
pub mod tab_drag;
pub mod text_pipeline;

// Re-exports from the extracted `sonic-ui` crate (PR-5 of the workspace
// refactor, issue #121). `sonic-ui` owns pure UI state + layout with no
// winit / wgpu / glyphon deps.
pub use sonic_ui::{
    command_label, command_palette, cursor, i18n, ime, overlays, pane, prefs, search, selection,
    tab_title, tabbar_view, tabs, ui_tokens,
};

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
