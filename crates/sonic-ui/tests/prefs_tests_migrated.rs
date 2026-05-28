use std::path::Path;

#[test]
fn prefs_source_has_no_inline_cfg_test_modules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/prefs");
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap();
        assert!(
            !text.contains("#[cfg(test)]"),
            "inline #[cfg(test)] remains in {}",
            entry.path().display()
        );
    }
}
