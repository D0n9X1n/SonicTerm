//! Mirror of `crates/sonicterm-windows/build.rs` — see that file for the
//! full rationale. Issue #439: copy `assets/fonts/` next to the freshly
//! built binary so the runtime `load_bundled_fonts` first-candidate
//! probe (`<exe-dir>/assets/fonts`) always wins.
//!
//! On macOS the .app bundling step takes over for the user-installed
//! layout (assets land in `Sonic.app/Contents/Resources/assets/fonts`),
//! which is the runtime's second-candidate probe and is unchanged by
//! this script. This script exists to make `cargo run -p sonicterm-mac`
//! (and any dev iteration loop short of `cargo bundle`) Just Work for
//! Powerline / Nerd-Font glyphs.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo"),
    );
    let src_fonts = manifest_dir.join("../../assets/fonts");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR must be set by cargo"));
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR should have a target/<profile> ancestor 3 levels up");
    let dst_fonts = profile_dir.join("assets/fonts");

    println!("cargo:rerun-if-changed={}", src_fonts.display());

    if !src_fonts.is_dir() {
        println!(
            "cargo:warning=sonicterm-mac build.rs: source fonts dir {} does not exist; \
             bundled Powerline/Nerd-Font icons will render as tofu at runtime (#439).",
            src_fonts.display()
        );
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&dst_fonts) {
        println!(
            "cargo:warning=sonicterm-mac build.rs: failed to create {}: {e}",
            dst_fonts.display()
        );
        return;
    }

    let entries = match std::fs::read_dir(&src_fonts) {
        Ok(e) => e,
        Err(e) => {
            println!(
                "cargo:warning=sonicterm-mac build.rs: failed to read_dir {}: {e}",
                src_fonts.display()
            );
            return;
        }
    };

    let mut copied = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
        if !matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
            continue;
        }
        let Some(file_name) = path.file_name() else { continue };
        let dst = dst_fonts.join(file_name);
        if let (Ok(src_meta), Ok(dst_meta)) = (path.metadata(), dst.metadata()) {
            if src_meta.len() == dst_meta.len() {
                continue;
            }
        }
        match std::fs::copy(&path, &dst) {
            Ok(_) => copied += 1,
            Err(e) => println!(
                "cargo:warning=sonicterm-mac build.rs: failed to copy {} -> {}: {e}",
                path.display(),
                dst.display()
            ),
        }
    }

    if copied > 0 {
        println!(
            "cargo:warning=sonicterm-mac build.rs: copied {copied} bundled font(s) to {}",
            dst_fonts.display()
        );
    }
}
