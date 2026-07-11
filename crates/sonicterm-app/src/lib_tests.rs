//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::{KeymapLoader, ThemeLoader};

#[test]
fn exports_loader_type_aliases() {
    let _: Option<KeymapLoader> = None;
    let _: Option<ThemeLoader> = None;
}
