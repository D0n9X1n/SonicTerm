//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::search::SearchState;

const TEST_OWNERSHIP_EXEMPTIONS: &[(&str, &str)] =
    &[("drag_chip", "passive render-input structs with behavior owned by renderer drag tests")];

/// Every direct behavioral module owns a flat sibling test suite or a stated exemption.
#[test]
fn direct_modules_declare_test_ownership() {
    // Derive modules from files so an unexported direct source cannot bypass ownership review.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut modules: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|file_type| file_type.is_file()))
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "rs"))
        .filter_map(|entry| entry.path().file_stem()?.to_str().map(str::to_owned))
        .filter(|module| module != "lib" && !module.ends_with("_tests"))
        .collect();
    modules.sort();

    for module in modules {
        let source = std::fs::read_to_string(root.join(format!("{module}.rs")))
            .unwrap()
            .replace("\r\n", "\n");
        let declaration =
            format!("#[cfg(test)]\n#[path = \"{module}_tests.rs\"]\nmod {module}_tests;");
        let exemption = TEST_OWNERSHIP_EXEMPTIONS.iter().find(|(name, _)| *name == module);
        assert!(
            source.contains(&declaration) || exemption.is_some(),
            "{module}.rs has no flat sibling test declaration or explicit exemption"
        );
        if source.contains(&declaration) {
            assert!(exemption.is_none(), "{module}.rs has tests and a stale exemption");
        }
    }

    for (module, reason) in TEST_OWNERSHIP_EXEMPTIONS {
        assert!(!reason.trim().is_empty(), "{module}.rs exemption needs a rationale");
        assert!(root.join(format!("{module}.rs")).is_file(), "{module}.rs exemption is stale");
    }
}

#[test]
fn exports_search_state() {
    let search = SearchState::new();
    assert!(search.query.is_empty());
    assert!(search.matches.is_empty());
}
