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
    "__test_command_palette_handle_key",
    "__test_drain_files",
    "__test_enable_pty_write_log",
    "__test_frontmost_window",
    "__test_invoke_activate_main_tab",
    "__test_invoke_reap_empty_child",
    "__test_main_window_id",
    "__test_palette_attached_window",
    "__test_palette_cursor",
    "__test_palette_open",
    "__test_palette_query",
    "__test_pane_grid_size",
    "__test_pane_worker",
    "__test_publish_input_modes",
    "__test_publish_pane_output",
    "__test_pty_write_log",
    "__test_resize_visible_panes",
    "__test_sample_pane_retention_now",
    "__test_search_query_cursor",
    "__test_seed_child_window",
    "__test_seed_counting_tab",
    "__test_seed_tab",
    "__test_set_child_search_query",
    "__test_set_frontmost_window",
    "__test_set_main_pane_viewport",
    "__test_set_main_search_query",
    "__test_set_memory_clipboard",
    "__test_set_next_echo_arm",
    "__test_set_palette_query",
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
        } else if path.extension().is_some_and(|extension| extension == "rs") {
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

/// Whether the attribute and doc lines directly above `line_index` carry a cfg that requires
/// `test` (the feature gate or a plain `cfg(test)`).
fn is_test_gated(lines: &[&str], line_index: usize) -> bool {
    lines[..line_index]
        .iter()
        .rev()
        .map(|line| line.trim())
        .take_while(|line| line.starts_with("#[") || line.starts_with("///"))
        .any(|line| line == TEST_SUPPORT_GATE || line == "#[cfg(test)]")
}

/// `(name, file, gated)` for each `fn __test_*` defined in the non-test `sources`.
fn hook_definitions(sources: &[(String, String)]) -> Vec<(String, String, bool)> {
    let mut hooks = Vec::new();
    let library = sources.iter().filter(|(path, _)| path.starts_with(LIBRARY));
    for (path, text) in library.filter(|(path, _)| !is_test_file(path)) {
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") {
                // When: the `line` is a comment, a `fn __test_` in its prose defines nothing.
                continue;
            }
            let Some(after_fn) = line.split("fn __test_").nth(1) else { continue };
            let tail: String = after_fn
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            hooks.push((format!("__test_{tail}"), path.clone(), is_test_gated(&lines, index)));
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
    // that compiles without the feature (anything but unit and integration tests). Each file
    // is blanked once, since the scan covers the whole workspace for every hook.
    let featureless: Vec<(&str, String)> = sources
        .iter()
        .filter(|(path, _)| !is_test_file(path) && !path.starts_with("crates/sonicterm-app/tests/"))
        .filter(|(_, text)| text.contains("__test_"))
        .map(|(path, text)| (path.as_str(), code_views(text).1))
        .collect();
    let featureless_callers = |name: &str| -> Vec<&str> {
        let call = format!("{name}(");
        let definition = format!("fn {call}");
        featureless
            .iter()
            .filter(|(_, bare)| bare.matches(&call).count() > bare.matches(&definition).count())
            .map(|(path, _)| *path)
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
    let (_, mod_text) = sources
        .iter()
        .find(|(path, _)| path == "crates/sonicterm-app/src/app/mod.rs")
        .expect("the App definition is scanned");
    let lines: Vec<&str> = mod_text.lines().collect();
    let start = lines.iter().position(|line| line.starts_with("pub struct App {")).expect("App");
    let end = start + lines[start..].iter().position(|line| *line == "}").expect("App closes");
    for (index, line) in lines.iter().enumerate().take(end).skip(start) {
        let field = line.trim().trim_start_matches("pub(super) ").trim_start_matches("pub ");
        let Some((name, _)) = field.split_once(':') else { continue };
        if !(name.starts_with("test_") || name == "pty_write_log_enabled") {
            continue;
        }
        if !is_test_gated(&lines, index) && !ungated_fields.contains(&name) {
            // When: an ungated `test_*` field is not listed, release builds would carry it unseen.
            problems.push(format!("App::{name} is ungated, unlisted"));
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
}
