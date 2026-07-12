//! Bundled-asset directory probe.
//!
//! Moved from the deprecated `sonicterm-shared` façade `asset_dir` in PR-C so the
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
    let manifest_fallback =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    resolve_asset_dir(std::env::current_exe().ok().as_deref(), manifest_fallback, |p| p.exists())
}

/// Precedence core shared by [`asset_dir`], with the executable path,
/// workspace fallback, and existence probe injected so the macOS
/// `Resources/assets` branch and the fallback can be exercised without a
/// real bundle layout on disk.
fn resolve_asset_dir(
    current_exe: Option<&std::path::Path>,
    manifest_fallback: std::path::PathBuf,
    exists: impl Fn(&std::path::Path) -> bool,
) -> std::path::PathBuf {
    if let Some(exe) = current_exe {
        if let Some(macos) = exe.parent() {
            if let Some(contents) = macos.parent() {
                let bundled = contents.join("Resources").join("assets");
                if exists(&bundled) {
                    return bundled;
                }
            }
        }
    }
    manifest_fallback
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod assets_tests;
