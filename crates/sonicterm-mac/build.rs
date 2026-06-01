//! Mirror of `crates/sonicterm-windows/build.rs` — see that file for the
//! full rationale.
//!
//! Issue #439 + smoke regression: ship the WHOLE `assets/` tree next to
//! the freshly built binary so `<exe-dir>/assets/{themes,keymaps,fonts,
//! icons,i18n}/...` lookups all hit during `cargo run -p sonicterm-mac`
//! (and any dev iteration loop short of `cargo bundle`). The .app
//! bundling step takes over for the user-installed layout (assets land
//! in `Sonic.app/Contents/Resources/assets`), which is the runtime's
//! .app-bundle candidate and is unchanged by this script.

use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo"),
    );
    let src_assets = manifest_dir.join("../../assets");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR must be set by cargo"));
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR should have a target/<profile> ancestor 3 levels up");
    let dst_assets = profile_dir.join("assets");

    println!("cargo:rerun-if-changed={}", src_assets.display());

    if !src_assets.is_dir() {
        println!(
            "cargo:warning=sonicterm-mac build.rs: source assets dir {} does not exist; \
             runtime will panic loading themes/keymaps/fonts (#439).",
            src_assets.display()
        );
        return;
    }

    let mut total: usize = 0;
    if let Err(e) = copy_dir_incremental(&src_assets, &dst_assets, &mut total) {
        println!(
            "cargo:warning=sonicterm-mac build.rs: copy {} -> {} failed: {e}",
            src_assets.display(),
            dst_assets.display()
        );
        return;
    }
    if total > 0 {
        println!(
            "cargo:warning=sonicterm-mac build.rs: copied {total} bundled asset file(s) to {}",
            dst_assets.display()
        );
    }
}

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
            if let (Ok(src_meta), Ok(dst_meta)) = (path.metadata(), dst_path.metadata()) {
                if src_meta.len() == dst_meta.len() {
                    continue;
                }
            }
            std::fs::copy(&path, &dst_path)?;
            *total += 1;
        }
    }
    Ok(())
}
