//! Self-test: every `pub trait` declared in `sonicterm-types` must have a
//! matching `tests/contract_<snake_case_trait>.rs` file. This ensures the
//! contract surface stays test-covered and prevents agents from adding a
//! trait without also adding the invariants document.
//!
//! Parses `pub trait` declarations out of all `.rs` files under `src/` with
//! a simple regex — good enough for our hand-written source. False positives
//! (e.g. `pub trait` inside a comment) would manifest as a confusing
//! missing-file error; in that case use `#[allow(dead_code)] pub(crate)` for
//! anything that's not really part of the contract.

use std::fs;
use std::path::{Path, PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i != 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn collect_pub_traits(dir: &Path, traits: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_pub_traits(&path, traits);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        for line in src.lines() {
            let trimmed = line.trim_start();
            // skip comments
            if trimmed.starts_with("//") || trimmed.starts_with("*") {
                continue;
            }
            // matches: `pub trait Foo`, `pub(crate) trait` is excluded
            if let Some(rest) = trimmed.strip_prefix("pub trait ") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    traits.push(name);
                }
            }
        }
    }
}

#[test]
fn every_pub_trait_has_a_contract_test_file() {
    let root = crate_root();
    let mut traits = Vec::new();
    collect_pub_traits(&root.join("src"), &mut traits);
    traits.sort();
    traits.dedup();

    let mut missing = Vec::new();
    for t in &traits {
        let expected = format!("tests/contract_{}.rs", snake_case(t));
        let p = root.join(&expected);
        if !p.exists() {
            missing.push((t.clone(), expected));
        }
    }
    assert!(
        missing.is_empty(),
        "pub traits without a contract test file: {:?}\n\
         (each pub trait in sonicterm-types must have tests/contract_<snake>.rs \
         asserting trait shape + invariants)",
        missing
    );
}
