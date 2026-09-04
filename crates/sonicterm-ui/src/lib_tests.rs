//! Public-surface and direct-module test-ownership contracts for `sonicterm-ui`.

use crate::search::SearchState;

const TEST_OWNERSHIP_EXEMPTIONS: &[(&str, &str)] =
    &[("drag_chip", "passive render-input structs with behavior owned by renderer drag tests")];

fn validate_test_ownership(
    root: &std::path::Path,
    exemptions: &[(&str, &str)],
) -> Result<(), String> {
    let entries: Vec<std::fs::DirEntry> = std::fs::read_dir(root)
        .map_err(|error| format!("read {}: {error}", root.display()))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("read entry under {}: {error}", root.display()))?;
    for entry in &entries {
        if entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?
            .is_dir()
        {
            return Err(format!(
                "{} is a source directory; direct modules must use flat .rs files",
                entry.file_name().to_string_lossy()
            ));
        }
    }
    let mut modules: Vec<String> = entries
        .into_iter()
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "rs"))
        .filter_map(|entry| entry.path().file_stem()?.to_str().map(str::to_owned))
        .filter(|module| module != "lib" && !module.ends_with("_tests"))
        .collect();
    modules.sort();

    for module in modules {
        let source = std::fs::read_to_string(root.join(format!("{module}.rs")))
            .map_err(|error| format!("read {module}.rs: {error}"))?
            .replace("\r\n", "\n");
        let declaration =
            format!("#[cfg(test)]\n#[path = \"{module}_tests.rs\"]\nmod {module}_tests;");
        let has_declaration = source.contains(&declaration);
        let exemption = exemptions.iter().find(|(name, _)| *name == module);
        if !has_declaration && exemption.is_none() {
            return Err(format!(
                "{module}.rs has no flat sibling test declaration or explicit exemption"
            ));
        }
        if has_declaration && exemption.is_some() {
            return Err(format!("{module}.rs has tests and a stale exemption"));
        }
        if has_declaration {
            let test_file = format!("{module}_tests.rs");
            let tests = std::fs::read_to_string(root.join(&test_file))
                .map_err(|error| format!("read {test_file}: {error}"))?
                .replace("\r\n", "\n");
            if !tests.contains("#[test]") {
                return Err(format!("{test_file} contains no behavioral tests"));
            }
        }
    }

    for (module, reason) in exemptions {
        if reason.trim().is_empty() {
            return Err(format!("{module}.rs exemption needs a rationale"));
        }
        if !root.join(format!("{module}.rs")).is_file() {
            return Err(format!("{module}.rs exemption is stale"));
        }
    }
    Ok(())
}

struct OwnershipFixture(std::path::PathBuf);

impl OwnershipFixture {
    fn new(label: &str) -> Self {
        for sequence in 0..1024 {
            let path = std::env::temp_dir().join(format!(
                "sonicterm-ui-test-ownership-{label}-{}-{sequence}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create ownership fixture: {error}"),
            }
        }
        panic!("could not allocate ownership fixture");
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for OwnershipFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Every direct behavioral module owns a flat sibling test suite or a stated exemption.
#[test]
fn direct_modules_declare_test_ownership() {
    // Derive modules from files so an unexported direct source cannot bypass ownership review.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    validate_test_ownership(&root, TEST_OWNERSHIP_EXEMPTIONS).unwrap();
}

/// A declared but empty sibling suite does not satisfy behavioral test ownership.
#[test]
fn empty_sibling_suite_is_rejected() {
    let fixture = OwnershipFixture::new("empty-suite");
    std::fs::write(
        fixture.path().join("feature.rs"),
        "#[cfg(test)]\n#[path = \"feature_tests.rs\"]\nmod feature_tests;\n",
    )
    .unwrap();
    std::fs::write(fixture.path().join("feature_tests.rs"), "").unwrap();

    let error = validate_test_ownership(fixture.path(), &[]).unwrap_err();
    assert!(error.contains("feature_tests.rs"), "unexpected error: {error}");
}

/// A direct source-module directory fails loudly instead of bypassing the flat inventory.
#[test]
fn source_module_directory_is_rejected() {
    let fixture = OwnershipFixture::new("source-directory");
    std::fs::create_dir(fixture.path().join("nested")).unwrap();
    std::fs::write(fixture.path().join("nested/mod.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();

    let error = validate_test_ownership(fixture.path(), &[]).unwrap_err();
    assert!(error.contains("nested"), "unexpected error: {error}");
}

#[test]
fn exports_search_state() {
    let search = SearchState::new();
    assert!(search.query.is_empty());
    assert!(search.matches.is_empty());
}
