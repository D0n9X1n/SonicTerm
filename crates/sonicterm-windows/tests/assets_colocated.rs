//! Regression guard for issue #439 + smoke fallout: `build.rs` must
//! colocate the WHOLE `assets/` tree next to the binary so the runtime
//! `asset_dir()` lookup at `<exe-dir>/assets` hits without falling
//! through to the compile-time absolute `CARGO_MANIFEST_DIR` path
//! (which only resolves on the build host).
//!
//! The exe panicked at startup in the smoke build because only
//! `assets/fonts/` was colocated and `Theme::load("themes/...")`
//! couldn't find its file. This test catches that class of regression
//! at `cargo test` time instead of at GUI smoke time.

use std::path::PathBuf;

/// Workspace-root `assets/` — the source of truth.
fn src_assets() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

/// `target/<profile>/assets/` — where `build.rs` mirrors the source.
/// The test binary lives at `target/<profile>/deps/<test-bin>` so
/// `<test-bin>/../../assets` is the colocation dir.
fn dst_assets() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(|d| d.parent())
        .expect("test exe should live at target/<profile>/deps/")
        .join("assets")
}

#[test]
fn build_rs_colocates_assets_dir_next_to_binary() {
    let dst = dst_assets();
    assert!(
        dst.is_dir(),
        "build.rs must produce {} for runtime asset_dir() to hit; missing dir means \
         the binary will panic at startup loading themes (issue #439 / smoke regression).",
        dst.display()
    );
}

#[test]
fn build_rs_colocates_every_top_level_asset_subdir() {
    let src = src_assets();
    let dst = dst_assets();
    let src_subdirs: Vec<_> = std::fs::read_dir(&src)
        .expect("read source assets dir")
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name())
        .collect();
    assert!(!src_subdirs.is_empty(), "source assets dir has no subdirs — test fixture broken");

    let mut missing: Vec<String> = Vec::new();
    for name in &src_subdirs {
        let dst_sub = dst.join(name);
        if !dst_sub.is_dir() {
            missing.push(name.to_string_lossy().into_owned());
        }
    }
    assert!(
        missing.is_empty(),
        "build.rs failed to colocate {} of {} assets subdirs: {:?}. Runtime asset_dir() lookups \
         into these subdirs will fall through to the compile-time absolute fallback and panic on \
         user machines (issue #439 / smoke regression).",
        missing.len(),
        src_subdirs.len(),
        missing
    );
}

#[test]
fn build_rs_colocates_specific_runtime_load_targets() {
    // Pin the specific files the runtime loads at startup. Each of
    // these missing == panic at startup. Listed explicitly so a future
    // change that drops one from build.rs gets caught with a meaningful
    // failure message naming the missing file.
    let dst = dst_assets();
    let must_exist = [
        // Theme loaded by sonicterm-windows/src/main.rs::load_theme via
        // asset_dir().join("themes").join("{name}.toml"). The default
        // theme is "gruvbox-dark-hard" — the file whose absence the PM
        // observed in the smoke run.
        "themes/gruvbox-dark-hard.toml",
        // Bundled font discovery target — the #439 root cause.
        "fonts/RecMonoSt.Helens-Regular.ttf",
    ];
    let mut missing: Vec<&str> = Vec::new();
    for rel in must_exist {
        if !dst.join(rel).is_file() {
            missing.push(rel);
        }
    }
    assert!(
        missing.is_empty(),
        "build.rs colocated assets dir is missing required runtime files: {:?} (under {})",
        missing,
        dst.display()
    );
}
