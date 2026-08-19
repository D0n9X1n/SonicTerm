use super::*;
use std::path::{Path, PathBuf};

#[test]
fn macos_bundle_assets_have_highest_precedence() {
    // Protect installed app bundles from accidentally selecting a neighboring portable tree.
    let exe = Path::new("/Applications/SonicTerm.app/Contents/MacOS/sonicterm");
    let bundled = PathBuf::from("/Applications/SonicTerm.app/Contents/Resources/assets");
    let adjacent = PathBuf::from("/Applications/SonicTerm.app/Contents/MacOS/assets");
    let result = resolve_asset_dir(
        Some(exe),
        Some(Path::new("/usr/share/sonicterm/assets")),
        PathBuf::from("/workspace/assets"),
        |path| path == bundled || path == adjacent,
    );
    assert_eq!(result, bundled);
}

#[test]
fn executable_adjacent_assets_precede_linux_fhs_assets() {
    // Protect relocatable tarball installs even when a system package is installed too.
    let exe = Path::new("/opt/SonicTerm/sonicterm");
    let adjacent = PathBuf::from("/opt/SonicTerm/assets");
    let fhs = Path::new("/usr/share/sonicterm/assets");
    let result =
        resolve_asset_dir(Some(exe), Some(fhs), PathBuf::from("/workspace/assets"), |path| {
            path == adjacent || path == fhs
        });
    assert_eq!(result, adjacent);
}

#[test]
fn linux_fhs_assets_precede_source_tree_fallback() {
    // Protect Debian installs whose executable and assets live in separate FHS prefixes.
    let fhs = Path::new("/usr/share/sonicterm/assets");
    let result = resolve_asset_dir(
        Some(Path::new("/usr/bin/sonicterm")),
        Some(fhs),
        PathBuf::from("/workspace/assets"),
        |path| path == fhs,
    );
    assert_eq!(result, fhs);
}

#[test]
fn development_assets_are_discovered_from_runtime_working_directory_ancestors() {
    // Protect workspace and per-crate runs without retaining the build host path in release binaries.
    let workspace_assets = PathBuf::from("/workspace/assets");
    assert_eq!(
        development_asset_dir(Some(Path::new("/workspace")), |path| path == workspace_assets),
        workspace_assets
    );
    assert_eq!(
        development_asset_dir(Some(Path::new("/workspace/crates/sonicterm-linux")), |path| {
            path == workspace_assets
        }),
        workspace_assets
    );
    let crate_assets = PathBuf::from("/workspace/crates/sonicterm-linux/assets");
    assert_eq!(
        development_asset_dir(Some(Path::new("/workspace/crates/sonicterm-linux")), |path| {
            path == crate_assets || path == workspace_assets
        }),
        crate_assets
    );
    assert_eq!(development_asset_dir(None, |_| false), PathBuf::from("assets"));
}

#[test]
fn missing_packaged_assets_fall_back_to_development_assets() {
    // Protect development runs and hosts where the executable path cannot be inspected.
    let fallback = PathBuf::from("/workspace/assets");
    assert_eq!(
        resolve_asset_dir(
            Some(Path::new("/tmp/sonicterm")),
            Some(Path::new("/usr/share/sonicterm/assets")),
            fallback.clone(),
            |_| false,
        ),
        fallback
    );
    assert_eq!(resolve_asset_dir(None, None, fallback.clone(), |_| true), fallback);
}
