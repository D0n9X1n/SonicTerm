use super::*;
use std::path::{Path, PathBuf};

#[test]
fn bundled_assets_win_when_resources_directory_exists() {
    let exe = Path::new("/Applications/SonicTerm.app/Contents/MacOS/sonicterm");
    let expected = PathBuf::from("/Applications/SonicTerm.app/Contents/Resources/assets");
    let result =
        resolve_asset_dir(Some(exe), PathBuf::from("/workspace/assets"), |path| path == expected);
    assert_eq!(result, expected);
}

#[test]
fn missing_bundle_falls_back_to_manifest_assets() {
    let fallback = PathBuf::from("/workspace/assets");
    let exe = Path::new("/tmp/sonicterm");
    assert_eq!(resolve_asset_dir(Some(exe), fallback.clone(), |_| false), fallback);
    assert_eq!(resolve_asset_dir(None, fallback.clone(), |_| true), fallback);
}
