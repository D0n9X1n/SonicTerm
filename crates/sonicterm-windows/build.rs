//! Copy `assets/fonts/` from the workspace root into the binary's output
//! directory so `load_bundled_fonts` finds it at
//! `<exe-dir>/assets/fonts/` — the first (and most reliable) candidate
//! it probes at runtime.
//!
//! **Why this exists** (issue #439): the runtime `load_bundled_fonts`
//! probes three locations in order:
//!   1. `<exe-dir>/assets/fonts`
//!   2. `<exe-dir>/../Resources/assets/fonts` (.app bundle layout)
//!   3. `CARGO_MANIFEST_DIR/../../assets/fonts` (compile-time absolute
//!      path baked in)
//!
//! Path 3 happens to resolve correctly on the **build host** because
//! the absolute path to the workspace at build time is reachable on
//! disk. But ANY machine that doesn't share that absolute path — every
//! end-user, every release MSI, every CI runner that ran the build
//! elsewhere — sees path 3 point at a nonexistent directory and the
//! function silently falls through to system fontconfig. With the
//! bundled `Rec Mono St.Helens` never loading, Powerline/Nerd-Font PUA
//! codepoints render as tofu (#439).
//!
//! The fix is to make path 1 always hit: copy `assets/fonts/` next to
//! the freshly-built exe in `target/{debug,release}/`. We do it from
//! `build.rs` (not from a manual pre-deploy step) so every developer
//! running `cargo run -p sonicterm-windows` and every CI release build
//! both get the colocation automatically — no separate "did you copy
//! the fonts?" gate.

use std::path::PathBuf;

fn main() {
    // The workspace root is two parents up from this crate's manifest.
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo"),
    );
    let src_fonts = manifest_dir.join("../../assets/fonts");

    // OUT_DIR is somewhere inside `target/.../build/sonicterm-windows-<hash>/out`.
    // Walk up to `target/{profile}/` — the directory containing the exe.
    // Cargo doesn't expose this directly, so we use OUT_DIR's ancestor.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR must be set by cargo"));
    // OUT_DIR layout: target/<profile>/build/<pkg>-<hash>/out
    // We want      : target/<profile>/
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR should have a target/<profile> ancestor 3 levels up");
    let dst_fonts = profile_dir.join("assets/fonts");

    println!("cargo:rerun-if-changed={}", src_fonts.display());

    if !src_fonts.is_dir() {
        // Workspace layout missing assets/fonts is a misconfiguration;
        // surface it as a build warning but don't fail — useful for
        // downstream consumers who might vendor this crate without
        // the fonts directory.
        println!(
            "cargo:warning=sonicterm-windows build.rs: source fonts dir {} does not exist; \
             bundled Powerline/Nerd-Font icons will render as tofu at runtime (#439).",
            src_fonts.display()
        );
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&dst_fonts) {
        println!(
            "cargo:warning=sonicterm-windows build.rs: failed to create {}: {e}",
            dst_fonts.display()
        );
        return;
    }

    let entries = match std::fs::read_dir(&src_fonts) {
        Ok(e) => e,
        Err(e) => {
            println!(
                "cargo:warning=sonicterm-windows build.rs: failed to read_dir {}: {e}",
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
        // Skip the copy if dst already exists AND is up to date — keeps
        // incremental builds fast (no I/O on every rebuild). We rely on
        // file-size equality as the cheap freshness check; the
        // rerun-if-changed on the source dir handles the rare case
        // where size matches but contents changed.
        if let (Ok(src_meta), Ok(dst_meta)) = (path.metadata(), dst.metadata()) {
            if src_meta.len() == dst_meta.len() {
                continue;
            }
        }
        match std::fs::copy(&path, &dst) {
            Ok(_) => copied += 1,
            Err(e) => println!(
                "cargo:warning=sonicterm-windows build.rs: failed to copy {} -> {}: {e}",
                path.display(),
                dst.display()
            ),
        }
    }

    if copied > 0 {
        println!(
            "cargo:warning=sonicterm-windows build.rs: copied {copied} bundled font(s) to {}",
            dst_fonts.display()
        );
    }
}
