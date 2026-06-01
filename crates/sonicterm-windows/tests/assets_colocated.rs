//! Regression guard for #439: after `cargo build -p sonicterm-windows`,
//! the assets tree must be colocated under `target/<profile>/assets/`
//! so the runtime's first font-discovery candidate (`<exe>/assets/fonts`)
//! actually hits.
//!
//! This test runs in the `tests/` integration-test target, which Cargo
//! places at `target/<profile>/deps/<bin>-<hash>.exe`. The build script
//! (`crates/sonicterm-windows/build.rs`) fires when this test target
//! links — so by the time `main` runs here, the colocated tree is
//! guaranteed to exist on disk.

use std::path::PathBuf;

/// Walk from `current_exe` up to `target/<profile>/`. Integration test
/// binaries live at `.../target/<profile>/deps/<name>-<hash>.exe`, so
/// the profile dir is the parent's parent.
fn locate_target_profile_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let deps = exe.parent().expect("deps dir");
    deps.parent().expect("profile dir").to_path_buf()
}

#[test]
fn target_profile_assets_dir_exists() {
    let profile = locate_target_profile_dir();
    let assets = profile.join("assets");
    assert!(
        assets.exists(),
        "expected colocated assets dir at {} — build.rs did not run or failed silently",
        assets.display()
    );
}

#[test]
fn every_source_subdir_is_mirrored() {
    let profile = locate_target_profile_dir();
    let assets = profile.join("assets");
    for sub in &["fonts", "themes", "keymaps", "icons", "i18n"] {
        let path = assets.join(sub);
        assert!(path.exists(), "missing colocated subdir {}", path.display());
    }
}

#[test]
fn specific_marker_files_present() {
    let profile = locate_target_profile_dir();
    let assets = profile.join("assets");
    for rel in &[
        "themes/gruvbox-dark-hard.toml",
        "fonts/RecMonoSt.Helens-Regular.ttf",
        "keymaps/sonicterm-windows.toml",
    ] {
        let path = assets.join(rel);
        assert!(path.exists(), "missing colocated marker file {}", path.display());
    }
}
