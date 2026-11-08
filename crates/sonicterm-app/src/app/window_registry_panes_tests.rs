//! Pins pane membership: a pane joins a window only through `attach` (an id the window holds is
//! handed back, never replaced), leaves only as a `DetachedPane` that keeps its id and state, and
//! a detached pane dropped without retirement or attachment fails in debug builds.

use super::*;
use crate::app::PaneState;

/// A pane with no PTY, for membership tests.
fn pane() -> PaneState {
    let parser = std::sync::Arc::new(parking_lot::Mutex::new(sonicterm_vt::vt::Parser::new(
        sonicterm_grid::grid::Grid::new(10, 4),
    )));
    PaneState::new_with_media_pool(parser, None, &crate::app::media::InlineMediaPool::new())
}

/// Attach adds a pane; an id the window already holds comes back untouched, and the held pane
/// stays.
#[test]
fn attach_refuses_an_id_already_held() {
    let mut map = PaneMap::default();
    map.attach_new(7, pane());
    let duplicate = DetachedPane::unattached(7, pane());
    let refused = map.attach(duplicate).expect_err("an id already held");
    assert_eq!(refused.id(), 7);
    assert_eq!(map.len(), 1);
    drop(refused.into_state_for_test());
}

/// Detach hands back the pane under its id and the map no longer holds it; attaching it again
/// restores it; detach_all empties the map.
#[test]
fn detach_and_reattach_keep_the_pane() {
    let mut map = PaneMap::default();
    map.attach_new(3, pane());
    map.attach_new(4, pane());
    let detached = map.detach(3).expect("held");
    assert_eq!(detached.id(), 3);
    assert!(!map.contains_key(&3));
    map.attach(detached).map_err(|_| ()).expect("absent again");
    let mut all = map.detach_all();
    all.sort_by_key(DetachedPane::id);
    assert_eq!(all.iter().map(DetachedPane::id).collect::<Vec<_>>(), [3, 4]);
    assert!(map.is_empty());
    for pane in all {
        drop(pane.into_state_for_test());
    }
}

/// A detached set stays ordered by id and moves whole into a new window.
#[test]
fn a_detached_set_moves_whole() {
    let mut set = DetachedPanes::default();
    for id in [9, 2, 5] {
        set.push(DetachedPane::unattached(id, pane()));
    }
    assert_eq!(set.keys().copied().collect::<Vec<_>>(), [2, 5, 9]);
    assert!(set.contains_key(&5) && set.get(&5).is_some());
    let map = PaneMap::from_detached(set);
    assert_eq!(map.len(), 3);
}

/// A detached pane dropped without retirement or attachment fails, so a pane can never leave a
/// window silently.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "was detached and dropped without retirement")]
fn dropping_a_detached_pane_fails() {
    let mut map = PaneMap::default();
    map.attach_new(1, pane());
    let _leaked = map.detach(1);
}

/// Sources whose `.panes` is not a window's `PaneMap`: `TabContainer`'s map in the GPU-free
/// transfer model, and a GPU recovery smoke probe's list of sampled panes.
const OTHER_PANES_FIELDS: &[&str] = &["app/tab_transfer.rs", "app/gpu_recovery_smoke.rs"];

/// Every app source outside the membership module and tests goes through the membership API:
/// no direct write to `.panes`, no in-place replacement of a pane's state, and `into_retired`
/// only in `App::retire_pane`.
#[test]
fn no_source_goes_around_pane_membership() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    production_sources(&root, &mut sources);
    assert!(sources.len() > 100, "the walk found the app's sources");
    let mut problems = Vec::new();
    for (name, text) in &sources {
        if name.ends_with("window_registry_panes.rs") {
            // The module that owns membership defines the operations themselves.
            continue;
        }
        if OTHER_PANES_FIELDS.iter().any(|file| name.ends_with(file)) {
            // A `panes` field of another type: the audit reads names, not types.
            continue;
        }
        let found = crate::app::source_syntax_support::pane_membership_bypasses(text)
            .unwrap_or_else(|| panic!("{name} parses"));
        for bypass in found {
            let retire = bypass == "calls `into_retired`" && name.ends_with("session.rs");
            if !retire {
                problems.push(format!("{name}: {bypass}"));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// Every non-test, non-hook Rust source under `dir`, as (path, text).
fn production_sources(dir: &std::path::Path, found: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("readable sources") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            production_sources(&path, found);
            continue;
        }
        let name = path.to_string_lossy().replace('\\', "/");
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        if file.ends_with(".rs") && !file.ends_with("_tests.rs") && !file.starts_with("test_hooks")
        {
            // When: `file` is production source, its membership changes are audited.
            found.push((name, std::fs::read_to_string(&path).unwrap()));
        }
    }
}

/// The audit sees each bypass it names, and nothing in a test-only item.
#[test]
fn the_membership_audit_sees_each_bypass() {
    let source = r#"
        fn bypass(window: &mut WindowState, other: PaneMap, id: u64, pane: PaneState) {
            window.panes = other;
            std::mem::take(&mut window.panes);
            window.panes.insert(id, pane);
            window.panes.remove(&id);
            window.panes.drain();
            window.panes.clear();
            std::mem::replace(window.panes.get_mut(&id).unwrap(), pane);
            mem::swap(&mut window.panes, &mut other);
            detached.into_retired();
        }
        #[cfg(test)]
        fn fixture(window: &mut WindowState, id: u64, pane: PaneState) {
            window.panes.insert(id, pane);
        }
    "#;
    let found = crate::app::source_syntax_support::pane_membership_bypasses(source).unwrap();
    assert_eq!(
        found,
        [
            "assigns `.panes`",
            "`mem::take` on `.panes`",
            "calls `.panes.insert`",
            "calls `.panes.remove`",
            "calls `.panes.drain`",
            "calls `.panes.clear`",
            "`mem::replace` on a pane's `get_mut`",
            "`mem::swap` on `.panes`",
            "calls `into_retired`",
        ]
    );
}
