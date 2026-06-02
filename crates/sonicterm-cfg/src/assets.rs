//! Bundled-asset directory probe.
//!
//! Moved from `sonicterm_shared::asset_dir()` in PR-C of issue #469 so the
//! app crate no longer needs to depend on the deprecated `sonicterm-shared`
//! façade just to locate `assets/`.

/// Locate the bundled `assets/` directory: prefers
/// `<binary>/../Resources/assets` (macOS .app layout) and falls back to
/// the workspace-root `assets/` next to the source tree.
///
/// This lives here so that both the platform binary (one-shot at
/// startup) and the live-reload path (re-loading themes/keymaps on
/// `sonicterm.toml` change) compute the same path.
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
