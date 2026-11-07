//! Source contract for the `test-support` feature: every `__test_*` hook and `test_*` App field
//! is either compiled only for tests or listed here with the non-test reader that keeps it.

use super::source_scan_support::code_views;

/// The cfg every hook read only by unit or integration tests carries.
const TEST_SUPPORT_GATE: &str = "#[cfg(any(test, feature = \"test-support\"))]";

/// Hooks compiled into every build because an example or production code calls them. An
/// example builds against an old base too (the perf overlay), so it cannot need the feature.
const UNGATED_HOOKS: &[&str] = &[
    "__test_advance_pane_parser",
    "__test_attach_window_renderer",
    "__test_child_pane_ids",
    "__test_child_pane_parser",
    "__test_invoke_activate_main_tab",
    "__test_invoke_reap_empty_child",
    "__test_main_window_id",
    "__test_palette_attached_window",
    "__test_palette_open",
    "__test_pane_grid_size",
    "__test_pane_worker",
    "__test_publish_input_modes",
    "__test_publish_pane_output",
    "__test_resize_visible_panes",
    "__test_sample_pane_retention_now",
    "__test_seed_child_window",
    "__test_seed_counting_tab",
    "__test_seed_tab",
    "__test_set_frontmost_window",
    "__test_set_main_pane_viewport",
    "__test_set_next_echo_arm",
    "__test_set_window_last_render",
    "__test_synthetic_main",
    "__test_window_active_tab_title",
    "__test_window_renderer_mut",
];

/// `test_*` App fields compiled into every build: production code or an example reads them.
const UNGATED_FIELDS: &[&str] = &[
    "pty_write_log_enabled",
    "test_clipboard_files",
    "test_clipboard_text",
    "test_pty_writes",
    "test_viewport_override",
];

/// Every `.rs` file under `dir`, as (path relative to `root`, contents).
fn rust_sources(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("readable source directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            // When: `path` is a directory, descend so nested modules are scanned too.
            rust_sources(root, &path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            || path.file_name().is_some_and(|name| name == "Cargo.toml")
        {
            // When: `path` is Rust source, or a manifest the audit reads for dev features.
            let relative = path.strip_prefix(root).expect("inside the root");
            let text = std::fs::read_to_string(&path).expect("readable source");
            out.push((relative.to_string_lossy().replace('\\', "/"), text));
        }
    }
}

/// The library's own sources, relative to the workspace root.
const LIBRARY: &str = "crates/sonicterm-app/src/";

/// Whether `path` is one of this library's unit-test files or test-declared fixtures, compiled
/// only under `cfg(test)`. Another crate's tests build this library without the feature.
fn is_test_file(path: &str) -> bool {
    if !path.starts_with(LIBRARY) {
        // When: the `path` is outside the library, its test naming says nothing about the gate.
        return false;
    }
    let stem = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".rs");
    stem.ends_with("_tests")
        || stem.contains("_tests_")
        || (stem.starts_with("test_hooks_") && stem.contains("fixture"))
}

/// Whether `path` is a dev target (a test or example, or code only tests compile) of a crate whose
/// `Cargo.toml` among `sources` enables `sonicterm-app`'s `test-support` in its dev-dependencies.
/// The perf harness example is excluded: it builds against older bases without the feature.
fn builds_with_test_support(sources: &[(String, String)], path: &str) -> bool {
    let mut parts = path.splitn(3, '/');
    let (Some("crates"), Some(krate), Some(rest)) = (parts.next(), parts.next(), parts.next())
    else {
        // When: `path` is not inside a crate directory, no manifest applies.
        return false;
    };
    if krate == "sonicterm-app" {
        // When: the `krate` is this one, its own examples build without the feature (perf base).
        return false;
    }
    let dev_target =
        rest.starts_with("examples/") || rest.starts_with("tests/") || rest.ends_with("_tests.rs");
    let manifest = format!("crates/{krate}/Cargo.toml");
    dev_target
        && sources.iter().any(|(source, text)| {
            *source == manifest
                && text.split("[dev-dependencies]").nth(1).is_some_and(|dev| {
                    let section = dev.split("\n[").next().unwrap_or(dev);
                    section.lines().any(|line| {
                        line.starts_with("sonicterm-app") && line.contains("\"test-support\"")
                    })
                })
        })
}

/// `(name, file, gated)` for each `fn __test_*` defined in the non-test `sources`, read from each
/// file's syntax tree, so a gate on an enclosing `impl` or module counts and a name in a comment
/// or string does not.
fn hook_definitions(sources: &[(String, String)]) -> Vec<(String, String, bool)> {
    let mut hooks = Vec::new();
    let library =
        sources.iter().filter(|(path, _)| path.starts_with(LIBRARY) && path.ends_with(".rs"));
    for (path, text) in library.filter(|(path, _)| !is_test_file(path)) {
        let items = super::source_syntax_support::source_items(text)
            .unwrap_or_else(|| panic!("{path} parses"));
        for item in items.into_iter().filter(|item| item.owner.is_none()) {
            if item.name.starts_with("__test_") {
                hooks.push((item.name, path.clone(), item.test_only));
            }
        }
    }
    hooks
}

/// Every contract violation in `sources`: an ungated hook or field missing from its list, a
/// listed one that is gated, or a listed hook nothing outside tests calls.
fn test_support_audit(
    sources: &[(String, String)],
    ungated_hooks: &[&str],
    ungated_fields: &[&str],
) -> Vec<String> {
    let mut problems = Vec::new();
    let hooks = hook_definitions(sources);
    for (name, path, gated) in &hooks {
        let listed = ungated_hooks.contains(&name.as_str());
        if gated == &listed {
            // When: `gated` and `listed` agree, the hook is gated yet listed, or neither.
            let state = if *gated { "gated but listed as ungated" } else { "ungated, unlisted" };
            problems.push(format!("{path}: {name} is {state}"));
        }
    }
    // A call is any `name(` in real code that is not the definition itself, made from a file
    // that compiles without the feature: anything but this crate's unit and integration tests,
    // and anything but the dev targets of a crate whose manifest turns the feature on. Each file
    // is blanked once, since the scan covers the whole workspace for every hook.
    // Each file's calls are read from its syntax tree: a call inside a test-only fn, impl or
    // module does not count, and a definition is not a call. A file that does not parse (a
    // fixture snippet) falls back to its blanked text.
    let featureless: Vec<(&str, &str, String)> = sources
        .iter()
        // `is_test_file` names this library's own tests; the perf harness's `*_tests.rs` belong to
        // its example target, which builds without the feature, so they are featureless callers.
        .filter(|(path, _)| !is_test_file(path) && !path.starts_with("crates/sonicterm-app/tests/"))
        .filter(|(path, _)| !builds_with_test_support(sources, path))
        .filter(|(path, text)| path.ends_with(".rs") && text.contains("__test_"))
        .map(|(path, text)| (path.as_str(), text.as_str(), code_views(text).1))
        .collect();
    let featureless_callers = |name: &str| -> Vec<&str> {
        let call = format!("{name}(");
        let definition = format!("fn {call}");
        featureless
            .iter()
            .filter(|(path, text, bare)| {
                // An example's own unit tests are `cfg(test)` yet build without the feature, so
                // their calls count; elsewhere only calls outside test-only items do.
                let example_tests = path.starts_with("crates/sonicterm-app/examples/")
                    && path.ends_with("_tests.rs");
                match super::source_syntax_support::method_calls(text, &[name]) {
                    Some(_) if example_tests => {
                        bare.matches(&call).count() > bare.matches(&definition).count()
                    }
                    Some(count) => count > 0,
                    None => bare.matches(&call).count() > bare.matches(&definition).count(),
                }
            })
            .map(|(path, _, _)| *path)
            .collect()
    };
    for listed in ungated_hooks {
        if featureless_callers(listed).is_empty() {
            // When: no example or production call remains, the hook belongs behind the gate.
            problems.push(format!("{listed} has no example or production caller"));
        }
    }
    for (name, path, gated) in &hooks {
        if !*gated {
            continue;
        }
        // A caller inside the library's own hook files may itself be gated; the lib lint with
        // the feature off proves those. Callers outside the library are the ones it cannot see.
        let callers: Vec<&str> = featureless_callers(name)
            .into_iter()
            .filter(|caller| !caller.starts_with(LIBRARY))
            .collect();
        if !callers.is_empty() {
            // When: a gated hook has `callers` built without the feature, those builds fail.
            problems.push(format!("{path}: gated {name} is called from {callers:?}"));
        }
    }
    // Test state lives on the App and in its test-support owner; both are audited.
    let owners = ["App", "TestSupportState"];
    let fields: Vec<_> = sources
        .iter()
        .filter(|(path, _)| {
            path == "crates/sonicterm-app/src/app/mod.rs"
                || path == "crates/sonicterm-app/src/app/test_support_state.rs"
        })
        .flat_map(|(path, text)| {
            super::source_syntax_support::source_items(text)
                .unwrap_or_else(|| panic!("{path} parses"))
        })
        .collect();
    assert!(
        fields.iter().any(|item| item.owner.as_deref() == Some("App")),
        "the App definition is scanned"
    );
    for field in fields.iter().filter(|item| owners.contains(&item.owner.as_deref().unwrap_or("")))
    {
        let name = field.name.as_str();
        if !(name.starts_with("test_") || name == "pty_write_log_enabled") {
            continue;
        }
        if !field.test_only && !ungated_fields.contains(&name) {
            // When: an ungated `test_*` field is not listed, release builds would carry it unseen.
            let owner = field.owner.as_deref().unwrap_or("App");
            problems.push(format!("{owner}::{name} is ungated, unlisted"));
        }
    }
    problems
}

/// Every first-party crate's Rust source, relative to the workspace root. Callers outside this
/// crate's library and integration tests (any example, any other crate) build without the
/// feature, so each of them keeps the hooks it calls ungated.
fn workspace_sources() -> Vec<(String, String)> {
    let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/");
    let workspace = crates_dir.parent().expect("workspace root");
    let mut sources = Vec::new();
    for entry in std::fs::read_dir(crates_dir).expect("readable crates directory") {
        let path = entry.expect("crate entry").path();
        if path.is_dir() && !path.ends_with("sonicterm-winit") {
            // When: `path` is a first-party crate; the patched winit tree defines no App hooks.
            rust_sources(workspace, &path, &mut sources);
        }
    }
    sources
}

#[test]
fn test_hooks_are_gated_unless_an_example_or_production_reads_them() {
    // Release builds compile the library without `test-support`, so every hook only tests
    // read must carry the gate, and every exemption must still have a non-test caller.
    let problems = test_support_audit(&workspace_sources(), UNGATED_HOOKS, UNGATED_FIELDS);
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_test_support_audit_reports_each_violation() {
    // Negative fixtures: an ungated unlisted hook, a gated listed hook, a listed hook with no
    // caller and an ungated unlisted field are each reported once.
    let library = format!(
        "pub struct App {{\n    pub(super) test_flag: bool,\n}}\nimpl App {{\n    \
         pub fn __test_loose() {{}}\n    {TEST_SUPPORT_GATE}\n    pub fn __test_listed() {{}}\n    \
         pub fn __test_orphan() {{}}\n}}\n"
    );
    let sources = vec![("crates/sonicterm-app/src/app/mod.rs".to_owned(), library)];
    let problems = test_support_audit(&sources, &["__test_listed", "__test_orphan"], &[]);
    assert_eq!(problems.len(), 5, "{problems:#?}");
    for expected in [
        "__test_loose is ungated, unlisted",
        "__test_listed is gated but listed as ungated",
        "__test_listed has no example or production caller",
        "__test_orphan has no example or production caller",
        "App::test_flag is ungated, unlisted",
    ] {
        assert!(problems.iter().any(|problem| problem.contains(expected)), "{expected}");
    }
    // An example call keeps a listed hook ungated, and a listed field passes.
    let mut covered = sources.clone();
    covered.push((
        "crates/sonicterm-app/examples/probe.rs".to_owned(),
        "app.__test_orphan();\n".to_owned(),
    ));
    let remaining = test_support_audit(&covered, &["__test_orphan"], &["test_flag"]);
    assert_eq!(remaining, vec![format!("{LIBRARY}app/mod.rs: __test_loose is ungated, unlisted")]);
    // A gated hook called from another crate's example would not compile there.
    covered
        .push(("crates/sonicterm-mac/examples/probe.rs".to_owned(), "__test_listed();".to_owned()));
    let leaked = test_support_audit(&covered, &["__test_orphan", "__test_loose"], &["test_flag"]);
    assert_eq!(leaked.len(), 2, "{leaked:#?}");
    assert!(leaked.iter().any(|problem| problem.contains("gated __test_listed is called from")));
    // That crate's manifest turning the feature on for its dev targets makes the call legal.
    covered.push((
        "crates/sonicterm-mac/Cargo.toml".to_owned(),
        "[dependencies]\nsonicterm-app.workspace = true\n\n[dev-dependencies]\nsonicterm-app = \
         { workspace = true, features = [\"test-support\"] }\n"
            .to_owned(),
    ));
    let enabled = test_support_audit(&covered, &["__test_orphan", "__test_loose"], &["test_flag"]);
    assert_eq!(enabled.len(), 1, "{enabled:#?}");
}

/// The audit reads ownership and cfg from the syntax tree: an ungated field moved into
/// `TestSupportState` is still reported, and a call made only from a test-only `impl` is not a
/// production caller, so a listed hook with no other caller is reported as removable.
#[test]
fn the_audit_follows_moved_fields_and_test_only_impls() {
    let app = "pub struct App {\n    pub(super) config: u8,\n}\nimpl App {\n    pub fn __test_listed() {}\n}\n"
        .to_owned();
    let state = "pub struct TestSupportState {\n    pub test_moved: bool,\n}\n".to_owned();
    let caller = format!(
        "{TEST_SUPPORT_GATE}\nimpl Probe {{\n    fn run(app: &App) {{ app.__test_listed(); }}\n}}\n"
    );
    let sources = vec![
        ("crates/sonicterm-app/src/app/mod.rs".to_owned(), app),
        ("crates/sonicterm-app/src/app/test_support_state.rs".to_owned(), state),
        ("crates/sonicterm-app/examples/probe.rs".to_owned(), caller),
    ];
    let problems = test_support_audit(&sources, &["__test_listed"], &[]);
    assert!(problems
        .iter()
        .any(|problem| problem.contains("TestSupportState::test_moved is ungated")));
    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("__test_listed has no example or production caller")),
        "{problems:#?}"
    );
    // The same call outside the gated impl keeps the hook listed.
    let mut ungated = sources.clone();
    ungated[2].1 = "fn run(app: &App) { app.__test_listed(); }\n".to_owned();
    let problems = test_support_audit(&ungated, &["__test_listed"], &["test_moved"]);
    assert!(problems.is_empty(), "{problems:#?}");
}
