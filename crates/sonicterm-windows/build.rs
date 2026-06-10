//! Build script — colocates the workspace `assets/` tree under
//! `target/<profile>/assets/` so the runtime's first font-discovery
//! candidate (`<exe>/assets/fonts`) always hits after `cargo build`.
//!
//! Without this, `cargo build` puts the binary at `target/<profile>/`
//! but leaves `assets/` only at the repo root — the runtime then
//! silently falls back to system fonts which may not be Nerd-patched
//! (Powerline / icon glyphs render as tofu). See issue #439.
//!
//! The copy is incremental: a destination file is only rewritten when
//! it's missing or older than the source. On metadata-read failure we
//! copy conservatively (assume stale) so a broken filesystem doesn't
//! silently skip an update.

use std::fs;
use std::path::{Path, PathBuf};

/// Embed the SonicTerm icon as a Win32 resource in the `.exe` so Explorer,
/// taskbar pinning, and Alt+Tab show the logo. Compiled only when building
/// for Windows (the resource compiler needs a Windows toolchain). Failure
/// is non-fatal: we warn and continue so cross/dev builds without a
/// resource compiler still link.
#[cfg(windows)]
fn embed_windows_resources() {
    let ico = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons/exports/sonic.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    if !ico.exists() {
        println!(
            "cargo:warning=sonicterm-windows build.rs: icon {} missing; exe will have no embedded icon",
            ico.display()
        );
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&ico.to_string_lossy());
    if let Err(e) = res.compile() {
        println!(
            "cargo:warning=sonicterm-windows build.rs: embedding icon resource failed: {e}"
        );
    }
}

#[cfg(not(windows))]
fn embed_windows_resources() {}

fn main() {
    println!("cargo:rerun-if-changed=../../assets");

    embed_windows_resources();

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_root = manifest_dir.join("../../assets");
    let Some(dest_root) = locate_target_profile_dir() else {
        println!(
            "cargo:warning=sonicterm-windows build.rs: could not locate target/<profile> dir; assets not colocated"
        );
        return;
    };
    let dest_assets = dest_root.join("assets");

    for sub in &["fonts", "themes", "keymaps", "icons", "i18n"] {
        let src = src_root.join(sub);
        let dst = dest_assets.join(sub);
        if let Err(e) = copy_tree(&src, &dst) {
            println!(
                "cargo:warning=sonicterm-windows build.rs: failed copying {} -> {}: {}",
                src.display(),
                dst.display(),
                e
            );
        }
    }
}

/// Walk up from `OUT_DIR` (`target/<profile>/build/<pkg>/out`) to find
/// `target/<profile>/`. If `CARGO_TARGET_DIR` is set, use it combined
/// with the profile inferred from `OUT_DIR`.
fn locate_target_profile_dir() -> Option<PathBuf> {
    let out_dir = std::env::var_os("OUT_DIR").map(PathBuf::from)?;
    // OUT_DIR = .../target/<profile>/build/<pkg-hash>/out
    //                  ^ want this dir
    let profile_dir = out_dir.parent()?.parent()?.parent()?.to_path_buf();
    Some(profile_dir)
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.exists() {
        return Ok(());
    }
    if src.is_file() {
        if should_copy(src, dst) {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(src, dst)?;
        }
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let s = entry.path();
        let d = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_tree(&s, &d)?;
        } else if ft.is_file() && should_copy(&s, &d) {
            if let Some(parent) = d.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

/// Returns true if `dst` should be (re)written from `src`. Conservative
/// on metadata-read errors: copying a stale file is safer than skipping
/// an update.
fn should_copy(src: &Path, dst: &Path) -> bool {
    if !dst.exists() {
        return true;
    }
    let Ok(src_meta) = fs::metadata(src) else { return true };
    let Ok(dst_meta) = fs::metadata(dst) else { return true };
    let Ok(src_t) = src_meta.modified() else { return true };
    let Ok(dst_t) = dst_meta.modified() else { return true };
    src_t > dst_t
}
