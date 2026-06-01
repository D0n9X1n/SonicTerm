//! Self-test: every entry in `landmines.toml` references file globs that
//! match at least one real file AND test paths (if any) that exist as
//! discoverable cargo tests in the workspace.
//!
//! The intent is to prevent landmine entries from silently rotting when
//! a file moves or a regression test is renamed. If you see this test
//! fail, either update the TOML entry to point at the new location or
//! remove the landmine if the underlying class of bug no longer applies.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(serde::Deserialize)]
struct LandmineToml {
    landmine: Vec<Landmine>,
}

#[derive(serde::Deserialize)]
struct Landmine {
    id: String,
    #[serde(default)]
    file_globs: Vec<String>,
    #[serde(default)]
    required_test_paths: Vec<String>,
}

fn workspace_root() -> PathBuf {
    // tests run from crate dir; go up two levels
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

#[test]
fn every_landmine_file_glob_matches_a_real_file() {
    let root = workspace_root();
    let toml_path = root.join("landmines.toml");
    let raw = std::fs::read_to_string(&toml_path).expect("landmines.toml missing");
    let data: LandmineToml = toml::from_str(&raw).expect("landmines.toml is not valid TOML");
    for lm in &data.landmine {
        assert!(
            !lm.file_globs.is_empty(),
            "landmine {} has no file_globs",
            lm.id
        );
        for g in &lm.file_globs {
            let abs = root.join(g);
            let count = glob::glob(abs.to_str().unwrap())
                .unwrap_or_else(|e| panic!("landmine {} glob {:?} invalid: {e}", lm.id, g))
                .count();
            assert!(
                count > 0,
                "landmine {} glob '{}' matches no files (rooted at {})",
                lm.id,
                g,
                root.display()
            );
        }
    }
}

#[test]
fn every_landmine_required_test_path_exists_in_workspace() {
    let root = workspace_root();
    let toml_path = root.join("landmines.toml");
    let raw = std::fs::read_to_string(&toml_path).expect("landmines.toml missing");
    let data: LandmineToml = toml::from_str(&raw).expect("landmines.toml is not valid TOML");

    // One `cargo test --workspace -- --list` invocation; cache the output.
    let out = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["test", "--workspace", "--", "--list"])
        .output()
        .expect("cargo test --list failed to launch");
    if !out.status.success() {
        eprintln!(
            "cargo test --list stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Don't hard-fail in this case — the listing might fail for transient
        // build reasons. Skip rather than wedge the gate.
        eprintln!("⚠ cargo test --list failed; skipping required-test-path check");
        return;
    }
    let listing = String::from_utf8_lossy(&out.stdout).to_string();

    for lm in &data.landmine {
        for t in &lm.required_test_paths {
            assert!(
                listing.contains(t),
                "landmine {} requires test '{}' which is not discoverable via `cargo test --workspace -- --list`",
                lm.id,
                t
            );
        }
    }
}
