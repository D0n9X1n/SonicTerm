//! Regression guard for #452: every file shipped under
//! `assets/{keymaps,themes,fonts}/` must be referenced in the
//! Windows MSI manifest (`wix/main.wxs`).
//!
//! Pre-fix the MSI shipped only `sonicterm.toml` even though the
//! default Windows config selects the `sonicterm-windows` keymap
//! (post-#430 rename) — first launch printed
//! "Error: load keymap: file not found" and exited.
//!
//! Scope: keymaps + themes + fonts (the user-facing config surface).
//! Icons live in nested subdirs and are tested manually via the
//! product-icon path in `Cargo.toml`.

use std::fs;
use std::path::PathBuf;

fn workspace_assets() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

fn wxs_text() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wix/main.wxs");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e))
}

#[test]
fn every_keymap_themes_and_fonts_file_is_in_wix_manifest() {
    let wxs = wxs_text();
    let assets = workspace_assets();
    let mut missing: Vec<String> = Vec::new();
    for sub in &["keymaps", "themes", "fonts"] {
        let dir = assets.join(sub);
        let entries =
            fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {}", dir.display(), e));
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let name = match p.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            // Skip README-style docs that are not user-facing config.
            if name.eq_ignore_ascii_case("README.md") || name.starts_with('.') {
                continue;
            }
            if !wxs.contains(name) {
                missing.push(format!("{}/{}", sub, name));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "WiX manifest (wix/main.wxs) is missing references to: {:?}",
        missing
    );
}
