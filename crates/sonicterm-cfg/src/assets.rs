//! Bundled-asset directory probe.
//!
//! Moved from the deprecated `sonicterm-shared` façade `asset_dir` so the
//! app crate no longer needs to depend on the deprecated `sonicterm-shared`
//! façade just to locate `assets/`.

/// Locate packaged assets for app bundles, portable distributions, Linux FHS
/// installs, or a source-tree development run.
///
/// Packaged paths take precedence in this order: macOS
/// `Contents/Resources/assets`, executable-adjacent `assets`, Linux
/// `/usr/share/sonicterm/assets`, then the workspace-root development tree.
/// Startup and live reload both use this function so they cannot select
/// different theme, keymap, font, or localization trees.
#[must_use]
pub fn asset_dir() -> std::path::PathBuf {
    let manifest_fallback =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    let linux_fhs =
        cfg!(target_os = "linux").then_some(std::path::Path::new("/usr/share/sonicterm/assets"));
    resolve_asset_dir(
        std::env::current_exe().ok().as_deref(),
        linux_fhs,
        manifest_fallback,
        std::path::Path::exists,
    )
}

fn resolve_asset_dir(
    current_exe: Option<&std::path::Path>,
    linux_fhs: Option<&std::path::Path>,
    manifest_fallback: std::path::PathBuf,
    exists: impl Fn(&std::path::Path) -> bool,
) -> std::path::PathBuf {
    if let Some(exe) = current_exe {
        // When: `current_exe` contains `exe`, packaged paths must win over development assets.
        if let Some(executable_dir) = exe.parent() {
            // When: `exe.parent()` yields `executable_dir`, inspect bundle and portable layouts.
            if let Some(contents) = executable_dir.parent() {
                // When: `executable_dir.parent()` yields `contents`, this may be a macOS bundle executable.
                let bundled = contents.join("Resources").join("assets");
                if exists(&bundled) {
                    // When: `exists(&bundled)` is true, preserve the signed app's self-contained assets.
                    return bundled;
                }
            }
            let adjacent = executable_dir.join("assets");
            if exists(&adjacent) {
                // When: adjacent assets exist, prefer the relocatable distribution over machine-wide data.
                return adjacent;
            }
        }
    }
    if let Some(fhs) = linux_fhs {
        // When: `linux_fhs` contains `fhs`, use it only when the installed package data exists.
        if exists(fhs) {
            // When: `fhs` exists, the split `/usr/bin` and `/usr/share` package layout is complete.
            return fhs.to_path_buf();
        }
    }
    manifest_fallback
}

#[cfg(test)]
#[path = "assets_tests.rs"]
mod assets_tests;
