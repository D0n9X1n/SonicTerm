//! Copy the entire `assets/` directory from the workspace root into the
//! binary's output directory so the runtime's `<exe-dir>/assets/...`
//! lookups (themes, keymaps, fonts, icons) all hit on a `cargo build`
//! / `cargo run` without manual asset copying.
//!
//! **Why this exists** (issue #439 + smoke regression):
//!
//! - `crates/sonicterm-windows/src/main.rs::asset_dir()` returns
//!   `<exe-dir>/assets` if present, else falls back to the compile-time
//!   absolute `CARGO_MANIFEST_DIR/../../assets`. The fallback only
//!   resolves on the build host — every other machine sees a path that
//!   doesn't exist, the binary panics in `Theme::load(...)` before it
//!   even reaches `load_bundled_fonts`, and the user sees an
//!   `Error: load theme / Caused by: read "...\target\release\assets\themes\gruvbox-dark-hard.toml"`
//!   at startup.
//! - `crates/sonicterm-text/src/swash_rasterizer.rs::load_bundled_fonts`
//!   probes `<exe-dir>/assets/fonts` first, with the same compile-time
//!   absolute fallback. Same off-host failure mode (#439): Powerline /
//!   Nerd-Font icons render as tofu.
//!
//! The fix is to colocate the WHOLE `assets/` tree next to the freshly
//! built exe. Doing it from `build.rs` (not a manual pre-deploy step)
//! means every developer running `cargo run -p sonicterm-windows` and
//! every CI release build both Just Work — no separate "did you copy
//! the assets?" gate.
//!
//! **What gets copied:** every file under `assets/`, recursively.
//! Mtime comparison is used as an incremental short-circuit so unchanged
//! files don't re-copy on every `cargo build`. (Size equality was tried
//! first but missed same-size content edits, e.g. a TOML tweak that
//! preserves length — see Haiku review on PR #444.)
//!
//! **Out of scope:** the runtime `asset_dir()` lookup is already
//! correct — it checks `<exe-dir>/assets` first. This script just makes
//! sure that path exists.

use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo"),
    );
    let src_assets = manifest_dir.join("../../assets");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR must be set by cargo"));
    // OUT_DIR layout: target/<profile>/build/<pkg>-<hash>/out
    // We want      : target/<profile>/
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR should have a target/<profile> ancestor 3 levels up");
    let dst_assets = profile_dir.join("assets");

    println!("cargo:rerun-if-changed={}", src_assets.display());

    if !src_assets.is_dir() {
        println!(
            "cargo:warning=sonicterm-windows build.rs: source assets dir {} does not exist; \
             runtime will panic loading themes/keymaps/fonts (#439).",
            src_assets.display()
        );
        return;
    }

    let mut total: usize = 0;
    if let Err(e) = copy_dir_incremental(&src_assets, &dst_assets, &mut total) {
        println!(
            "cargo:warning=sonicterm-windows build.rs: copy {} -> {} failed: {e}",
            src_assets.display(),
            dst_assets.display()
        );
        return;
    }
    if total > 0 {
        println!(
            "cargo:warning=sonicterm-windows build.rs: copied {total} bundled asset file(s) to {}",
            dst_assets.display()
        );
    }
}

/// Recursively mirror `src` into `dst`. Skips files whose destination
/// already exists and has an mtime >= the source mtime (cheap
/// incremental gate that catches same-size content edits which a
/// length-only check would miss). Returns count of files actually
/// copied via `*total`.
fn copy_dir_incremental(src: &Path, dst: &Path, total: &mut usize) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name() else { continue };
        let dst_path = dst.join(file_name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_incremental(&path, &dst_path, total)?;
        } else if file_type.is_file() {
            if !should_copy(&path, &dst_path) {
                continue;
            }
            std::fs::copy(&path, &dst_path)?;
            *total += 1;
        }
        // Symlinks: skip silently. We don't ship any in assets/.
    }
    Ok(())
}

/// Returns true if `src` should be copied to `dst`. Copies when `dst`
/// is missing or when `src`'s mtime is newer than `dst`'s. Any metadata
/// error conservatively returns true so we never silently skip a real
/// change.
fn should_copy(src: &Path, dst: &Path) -> bool {
    let Ok(src_meta) = std::fs::metadata(src) else { return true };
    let Ok(dst_meta) = std::fs::metadata(dst) else { return true };
    let Ok(src_mtime) = src_meta.modified() else { return true };
    let Ok(dst_mtime) = dst_meta.modified() else { return true };
    src_mtime > dst_mtime
}
