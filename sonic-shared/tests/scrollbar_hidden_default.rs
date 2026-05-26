//! Regression: Sonic must not render a scrollbar by default.
//!
//! WezTerm's default is `enable_scroll_bar = false`. Sonic matches that
//! parity by simply not having a scrollbar UI at all (scrollback works
//! via keyboard / mouse-wheel). This test pins that behavior so a future
//! change can't silently add an always-on scrollbar — if a scrollbar is
//! introduced later, it MUST be gated behind a config flag that defaults
//! to off, and this test should be updated to assert the default-off state
//! via that flag rather than via source scanning.
//!
//! The check is a source-level scan rather than a render-pass assertion
//! because adding a scrollbar would require introducing new symbols
//! (a `Scrollbar` type, a `scrollbar_thumb` quad emitter, a
//! `show_scrollbar` config knob, etc.). Catching the symbol introduction
//! is sufficient and avoids spinning up wgpu in a unit test.

use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at sonic-shared/. Parent is the workspace root.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().expect("workspace root above sonic-shared").to_path_buf()
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            // Skip build output and any vendored deps.
            if name == "target" || name == ".git" || name == "node_modules" {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_scrollbar_rendering_by_default() {
    let root = workspace_root();
    // Only scan the runtime crates that contribute to a rendered frame.
    let crates = ["sonic-core/src", "sonic-shared/src", "sonic-mac/src", "sonic-windows/src"];

    let mut files = Vec::new();
    for c in &crates {
        collect_rs_files(&root.join(c), &mut files);
    }
    assert!(!files.is_empty(), "expected to scan some source files; check workspace layout");

    // This file itself contains the forbidden tokens in comments and string
    // literals — exclude it from the scan.
    let this_file =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("scrollbar_hidden_default.rs");

    let forbidden = ["Scrollbar", "ScrollBar", "scrollbar_thumb"];
    let mut hits: Vec<(PathBuf, &str, usize)> = Vec::new();
    for f in &files {
        if f == &this_file {
            continue;
        }
        let Ok(src) = fs::read_to_string(f) else {
            continue;
        };
        for (lineno, line) in src.lines().enumerate() {
            for tok in &forbidden {
                if line.contains(tok) {
                    hits.push((f.clone(), tok, lineno + 1));
                }
            }
        }
    }

    assert!(
        hits.is_empty(),
        "Sonic must not draw a scrollbar by default (WezTerm parity). \
         If you are intentionally adding one, gate it behind a config flag \
         that defaults to false and update this test. Hits: {:?}",
        hits
    );
}
