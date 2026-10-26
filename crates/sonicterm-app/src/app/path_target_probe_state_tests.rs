//! Preserved probe state path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

/// Real filesystem classification follows symlinks to files and folders, and rejects dangling links, loops and sockets.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn filesystem_classification_follows_links_and_rejects_special_entries() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    // Darwin's long per-user temp root can exceed sockaddr_un.sun_path before the socket name.
    let root = Path::new("/tmp").canonicalize().unwrap().join(format!(
        "st-kind-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("notes.txt");
    let directory = root.join("folder");
    let file_link = root.join("file-link");
    let directory_link = root.join("folder-link");
    let dangling_link = root.join("dangling-link");
    let looping_link = root.join("looping-link");
    let socket = root.join("socket");
    std::fs::write(&file, b"notes").unwrap();
    std::fs::create_dir(&directory).unwrap();
    symlink(&file, &file_link).unwrap();
    symlink(&directory, &directory_link).unwrap();
    symlink(root.join("gone"), &dangling_link).unwrap();
    symlink(&looping_link, &looping_link).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();

    assert_eq!(classify_local_target(&file), PathOpenDecision::Openable(PathKind::File));
    assert_eq!(classify_local_target(&directory), PathOpenDecision::Openable(PathKind::Directory));
    assert_eq!(classify_local_target(&file_link), PathOpenDecision::Openable(PathKind::File));
    assert_eq!(
        classify_local_target(&directory_link),
        PathOpenDecision::Openable(PathKind::Directory)
    );
    // A link that exists but resolves to nothing is present, so it blocks instead of reading as missing.
    assert_eq!(classify_local_target(&dangling_link), PathOpenDecision::Blocked);
    assert_eq!(classify_local_target(&looping_link), PathOpenDecision::Blocked);
    assert_eq!(classify_local_target(&socket), PathOpenDecision::Blocked);
    assert_eq!(classify_local_target(&root.join("missing")), PathOpenDecision::Missing);

    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}

/// The Linux open adapter receives an already-opened file identity, not its pathname.
#[test]
fn open_target_adapter_owns_an_open_file() {
    use std::io::Write;

    let path = native_test_root().join(format!(
        "sonicterm-path-open-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let mut created = std::fs::File::create(&path).unwrap();
    created.write_all(b"identity").unwrap();
    drop(created);

    let length = with_opened_target(&path, |file| Ok(file.metadata()?.len())).unwrap();
    assert_eq!(length, 8);
    std::fs::remove_file(&path).unwrap();
    assert!(with_opened_target(&path, |_| Ok(())).is_err());
}

/// Both window roles retain completed probes until a fresh snapshot validates them, never authorizing clicks before that.
#[test]
fn completed_probe_waits_for_fresh_hover_without_reprobing() {
    for main in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let window = app.__test_seed_child_window(&["pending probe"]);
        if main {
            app.main_window_id = Some(window);
        }
        let mut key = probe_key("/work/source.rs", 0);
        key.window_id = window;
        let request =
            app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
        let epoch = request.epoch;
        app.handle_path_probe_finished(openable_result(request));
        assert!(!app.windows[&window].path_probe.authorized(&key, true));
        app.apply_target_lookup(window, Err(()));
        assert_eq!(app.windows[&window].path_probe.epoch, epoch);
        app.apply_target_hover(
            window,
            Some(CellTargetSnapshot {
                label: None,
                pane_id: key.pane_id,
                hover_cells: None,
                display: "./file".into(),
                explicit_hyperlink: false,
                target: ResolvedCellTarget::Path(key.clone()),
            }),
        );
        assert!(app.windows[&window].path_probe.authorized(&key, true));
        assert_eq!(app.windows[&window].path_probe.epoch, epoch);
        assert!(app.windows.get_mut(&window).unwrap().path_probe.request(key).is_none());
    }
}

/// The next snapshot must reject a completed result whose row changed and schedule a new probe.
#[test]
fn completed_probe_cannot_authorize_changed_content() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["changed probe"]);
    let mut key = probe_key("/work/source.rs", 0);
    key.window_id = window;
    let request = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    let epoch = request.epoch;
    app.handle_path_probe_finished(openable_result(request));
    key.rows[0].fingerprint += 1;
    app.apply_target_hover(
        window,
        Some(CellTargetSnapshot {
            label: None,
            pane_id: key.pane_id,
            hover_cells: None,
            display: "./file".into(),
            explicit_hyperlink: false,
            target: ResolvedCellTarget::Path(key.clone()),
        }),
    );
    let state = &app.windows[&window].path_probe;
    assert!(!state.authorized(&key, true));
    assert_ne!(state.epoch, epoch);
    assert_eq!(state.current.as_ref(), Some(&key));
}

/// A delayed old completion cannot displace the current result or survive explicit invalidation.
#[test]
fn completed_probe_ignores_old_epochs_and_clears_on_leave() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["probe epochs"]);
    let mut key = probe_key("/work/source.rs", 0);
    key.window_id = window;
    let old = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    app.clear_target_hover(window);
    let current = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    app.handle_path_probe_finished(openable_result(current));
    app.handle_path_probe_finished(openable_result(old));
    let target = CellTargetSnapshot {
        label: None,
        pane_id: key.pane_id,
        hover_cells: None,
        display: "./file".into(),
        explicit_hyperlink: false,
        target: ResolvedCellTarget::Path(key.clone()),
    };
    app.apply_target_hover(window, Some(target.clone()));
    assert!(app.windows[&window].path_probe.authorized(&key, true));
    app.clear_target_hover(window);
    let request = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    app.handle_path_probe_finished(openable_result(request));
    app.clear_target_hover(window);
    app.apply_target_hover(window, Some(target));
    assert!(!app.windows[&window].path_probe.authorized(&key, true));
}

/// A completed probe requests its own frame even after PTY output and pointer movement stop.
#[test]
fn completed_probe_schedules_its_validation_frame() {
    let source = concat!(
        include_str!("path_target.rs"),
        "\n",
        include_str!("path_target_activate.rs"),
        "\n",
        include_str!("path_target_detect.rs"),
        "\n",
        include_str!("path_target_hardwrap_paths.rs"),
        "\n",
        include_str!("path_target_hardwrap_uri.rs"),
        "\n",
        include_str!("path_target_hover.rs"),
        "\n",
        include_str!("path_target_logical.rs"),
        "\n",
        include_str!("path_target_osc8.rs"),
        "\n",
        include_str!("path_target_pane_spans.rs"),
        "\n",
        include_str!("path_target_probe_state.rs"),
        "\n",
        include_str!("path_target_probe_worker.rs"),
        "\n",
        include_str!("path_target_resolve.rs")
    );
    let handler = source
        .split("fn handle_path_probe_finished(")
        .nth(1)
        .unwrap()
        .split("fn open_modifier_held(")
        .next()
        .unwrap();
    assert!(handler.contains("request_window_redraw()"));
    assert!(!handler.contains("pointer_target("));
}

/// Local previews wait for the exact probe and disappear on modifier release or invalidation.
#[test]
fn local_path_preview_requires_current_authorization() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["preview"]);
    app.frontmost_window = Some(window);
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    let mut key = probe_key("/work/source.rs", 0);
    key.window_id = window;
    let target = CellTargetSnapshot {
        label: None,
        pane_id: key.pane_id,
        hover_cells: None,
        display: "./file".into(),
        explicit_hyperlink: false,
        target: ResolvedCellTarget::Path(key.clone()),
    };
    let request = app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
    app.apply_target_hover(window, Some(target.clone()));
    assert!(
        app.windows[&window].link_preview.is_none(),
        "unverified candidates are not destinations"
    );
    let mut result = openable_result(request);
    result.selection.as_mut().unwrap().decision = PathOpenDecision::SourceReveal;
    assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&result, Some(&key)));
    app.apply_target_hover(window, Some(target.clone()));
    let preview = app.windows[&window].link_preview.as_ref().expect("validated path preview");
    assert!(preview.uri.contains("/work/source.rs"));
    assert!(preview.available);
    app.windows.get_mut(&window).unwrap().modifiers = winit::keyboard::ModifiersState::empty();
    app.apply_target_hover(window, Some(target));
    assert!(app.windows[&window].link_preview.is_none());
    app.clear_target_hover(window);
    assert!(app.windows[&window].link_preview.is_none());
}

/// Listing guesses, missing paths, and rejected local links never become preview destinations.
#[test]
fn unresolved_local_path_preview_is_hidden() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["preview"]);
    app.frontmost_window = Some(window);
    app.windows.get_mut(&window).unwrap().modifiers = if cfg!(target_os = "macos") {
        winit::keyboard::ModifiersState::SUPER
    } else {
        winit::keyboard::ModifiersState::CONTROL
    };
    for failure in ["path-error-missing", "path-error-blocked"] {
        app.windows.get_mut(&window).unwrap().path_probe.invalidate();
        let mut key = probe_key("/work/setup-windows-cairo.ps1", 0);
        key.window_id = window;
        let request =
            app.windows.get_mut(&window).unwrap().path_probe.request(key.clone()).unwrap();
        let result = PathProbeResult { request, selection: None, failure: Some(failure) };
        assert!(app.windows.get_mut(&window).unwrap().path_probe.accept(&result, Some(&key)));
        app.apply_target_hover(
            window,
            Some(CellTargetSnapshot {
                label: None,
                pane_id: key.pane_id,
                hover_cells: None,
                display: "AM     3595 setup-windows-cairo.ps1".into(),
                explicit_hyperlink: false,
                target: ResolvedCellTarget::Path(key),
            }),
        );
        assert!(app.windows[&window].link_preview.is_none(), "{failure}");
    }
    app.apply_target_hover(
        window,
        Some(CellTargetSnapshot {
            label: None,
            pane_id: 0,
            hover_cells: None,
            display: "file://remote/missing".into(),
            explicit_hyperlink: true,
            target: ResolvedCellTarget::Rejected("path-error-blocked"),
        }),
    );
    assert!(app.windows[&window].link_preview.is_none());
}

/// Failure reasons cannot survive a new target, stale result, or explicit invalidation.
#[test]
fn probe_failure_is_bound_to_current_identity() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/missing", 0);
    let request = state.request(key.clone()).unwrap();
    let result = PathProbeResult { request, selection: None, failure: Some("path-error-missing") };
    assert!(state.accept(&result, Some(&key)));
    assert_eq!(state.failure_for(&key), Some("path-error-missing"));
    assert!(!state.authorized(&key, true));
    let next = probe_key("/work/other", 0);
    state.request(next.clone()).unwrap();
    assert_eq!(state.failure_for(&next), None);
    assert!(!state.accept(&result, Some(&next)));
    assert_eq!(state.failure_for(&next), None);
    state.invalidate();
    assert_eq!(state.failure_for(&key), None);
}

/// A result authorizes a click only when epoch, key, and live modifier all match.
#[test]
fn probe_state_requires_current_epoch_key_and_modifier() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let request = state.request(key.clone()).expect("new candidate schedules a probe");
    let result = openable_result(request);

    assert!(state.accept(&result, Some(&key)));
    assert!(!state.authorized(&key, false), "modifier release must deactivate authorization");
    assert!(state.authorized(&key, true));
    assert_eq!(state.decision_for(&key), Some(PathOpenDecision::Openable(PathKind::File)));
}

/// Accepted spans retain authorization when a destination adds only a longer candidate.
#[test]
fn probe_state_retains_selection_across_irrelevant_candidate_changes() {
    let mut state = PathProbeState::default();
    let mut key = probe_key("/work/Folder", 20);
    let selected = probe_candidate("Folder", "/work/Folder", 7);
    key.pointed.col = 8;
    key.candidates = vec![selected.clone()];
    let result = openable_result(state.request(key.clone()).unwrap());
    assert!(state.accept(&result, Some(&key)));

    // Shortest-first never lets a longer candidate displace an existing shorter winner.
    let mut moved = key.clone();
    moved.pointed.col = 9;
    moved.candidates.push(probe_candidate("My Folder", "/work/My Folder", 4));
    assert!(state.request(moved.clone()).is_none());
    assert!(state.authorized(&moved, true));
}

/// A newly visible equal or shorter candidate revokes authorization and schedules a fresh probe.
#[test]
fn probe_state_reprobes_for_new_competing_candidates() {
    for competing in [
        probe_candidate("Name Here", "/work/Name Here", 5),
        probe_candidate("Name", "/work/Name", 5),
    ] {
        let mut state = PathProbeState::default();
        let selected = probe_candidate("Left Name", "/work/Left Name", 0);
        let mut key = probe_key("/work/Left Name", 20);
        key.pointed.col = 1;
        key.candidates = vec![selected.clone()];
        let result = openable_result(state.request(key.clone()).unwrap());
        assert!(state.accept(&result, Some(&key)));

        let mut moved = key.clone();
        moved.pointed.col = 6;
        moved.candidates = vec![competing.clone(), selected.clone()];
        assert!(state.request(moved.clone()).is_some(), "candidate: {competing:?}");
        assert!(!state.authorized(&moved, true), "candidate: {competing:?}");
    }
}

/// An in-flight result cannot authorize a destination with an unprobed competing candidate.
#[test]
fn probe_state_rejects_results_after_candidate_set_expands() {
    let mut state = PathProbeState::default();
    let selected = probe_candidate("Left Name", "/work/Left Name", 0);
    let mut key = probe_key("/work/Left Name", 20);
    key.pointed.col = 1;
    key.candidates = vec![selected.clone()];
    let request = state.request(key.clone()).unwrap();

    let mut moved = key.clone();
    moved.pointed.col = 6;
    moved.candidates = vec![probe_candidate("Name Here", "/work/Name Here", 5), selected];
    assert!(!state.accept(&openable_result(request), Some(&moved)));
    assert!(state.request(moved).is_some());
}

/// A transient inability to re-read the pointer target must allow the same path to be probed again.
#[test]
fn unavailable_fresh_target_does_not_wedge_probe_state() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let request = state.request(key.clone()).expect("first request");

    assert!(!state.accept(&openable_result(request), None));
    assert!(state.request(key).is_some(), "the same target must be eligible for a retry");
}

/// Delayed results from a prior same-value incarnation cannot survive an ABA transition.
#[test]
fn probe_epoch_rejects_same_key_after_leave_and_reenter() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let first = state.request(key.clone()).expect("first request");
    state.invalidate();
    let second = state.request(key.clone()).expect("re-enter schedules a new epoch");

    assert!(!state.accept(&openable_result(first), Some(&key)));
    assert!(!state.authorized(&key, true));
    assert!(state.accept(&openable_result(second), Some(&key)));
    assert!(state.authorized(&key, true));
}

/// Unrelated grid mutations preserve authorization when pointed-row identity is unchanged.
#[test]
fn probe_state_ignores_unrelated_grid_mutation() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let result = openable_result(state.request(key.clone()).unwrap());
    assert!(state.accept(&result, Some(&key)));

    let unchanged_row = key.clone();
    assert!(state.request(unchanged_row.clone()).is_none());
    assert!(state.authorized(&unchanged_row, true));
}

/// Pointed-row identity changes revoke authorization and schedule a fresh probe.
#[test]
fn probe_state_reprobes_after_pointed_row_changes() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let result = openable_result(state.request(key.clone()).unwrap());
    assert!(state.accept(&result, Some(&key)));

    let mut changed = key.clone();
    changed.rows[0].fingerprint = changed.rows[0].fingerprint.wrapping_add(1);
    assert!(state.request(changed.clone()).is_some());
    assert!(!state.authorized(&changed, true));
}

/// Moving between wrapped fragments preserves authorization only while every row identity matches.
#[test]
fn wrapped_probe_authorization_tracks_all_rows_and_fragments() {
    let candidate = PathProbeCandidate {
        spans: smallvec::smallvec![
            AbsoluteCellSpan { row: 20, start_col: 4, end_col: 10 },
            AbsoluteCellSpan { row: 21, start_col: 0, end_col: 7 },
        ],
        target: DetectedTarget::PathCandidate("src/long/path.rs".into()),
        resolved_path: PathBuf::from("/work/src/long/path.rs"),
        joined: false,
    };
    let mut key = probe_key("/work/src/long/path.rs", 20);
    key.pointed = AbsoluteCell { row: 20, col: 6 };
    key.candidates = vec![candidate.clone()];
    key.rows = smallvec::smallvec![
        PathRowIdentity { row: 20, fingerprint: 11 },
        PathRowIdentity { row: 21, fingerprint: 12 },
    ];
    let mut state = PathProbeState::default();
    let result = openable_result(state.request(key.clone()).unwrap());
    assert!(state.accept(&result, Some(&key)));

    let mut second_fragment = key.clone();
    second_fragment.pointed = AbsoluteCell { row: 21, col: 2 };
    assert!(state.request(second_fragment.clone()).is_none());
    assert!(state.authorized(&second_fragment, true));

    let mut changed_row = second_fragment.clone();
    changed_row.rows[1].fingerprint = changed_row.rows[1].fingerprint.wrapping_add(1);
    assert!(state.request(changed_row.clone()).is_some());
    assert!(!state.authorized(&changed_row, true));
}

/// Screen incarnation and viewport projection remain part of wrapped authorization identity.
#[test]
fn wrapped_probe_rejects_screen_or_viewport_aba() {
    for mutate in [
        |key: &mut PathProbeKey| key.screen_epoch = key.screen_epoch.wrapping_add(1),
        |key: &mut PathProbeKey| key.view_top = key.view_top.saturating_add(1),
    ] {
        let mut state = PathProbeState::default();
        let key = probe_key("/work/file", 20);
        let result = openable_result(state.request(key.clone()).unwrap());
        assert!(state.accept(&result, Some(&key)));

        let mut changed = key.clone();
        mutate(&mut changed);
        assert!(state.request(changed.clone()).is_some());
        assert!(!state.authorized(&changed, true));
    }
}

/// A viewport round trip with the same visible value still gets a distinct epoch.
#[test]
fn probe_epoch_rejects_viewport_round_trip_results() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let old = state.request(key.clone()).expect("initial request");
    state.invalidate();
    let away = probe_key("/work/file", 30);
    state.request(away).expect("scroll-away request");
    state.invalidate();
    let current = state.request(key.clone()).expect("scroll-back request");

    assert!(!state
        .accept(&PathProbeResult { failure: None, request: old, selection: None }, Some(&key)));
    assert_eq!(state.decision_for(&key), None);
    assert!(state.accept(&openable_result(current), Some(&key)));
}

/// The shortest openable candidate wins, and a blocked shortest tier ends the probe instead of falling to a longer path.
#[test]
fn candidate_selection_prefers_shortest_and_stops_at_blocked() {
    let candidates = vec![
        probe_candidate("OneDrive - Microsoft", "/work/OneDrive - Microsoft", 0),
        probe_candidate("OneDrive", "/work/OneDrive", 0),
    ];
    let selected =
        select_openable_candidate(&candidates, |_| PathOpenDecision::Openable(PathKind::Directory))
            .expect("shortest candidate");
    assert_eq!(selected.candidate.display(), "OneDrive");

    let longer = select_openable_candidate(&candidates, |path| {
        if path.ends_with("OneDrive") {
            PathOpenDecision::Missing
        } else {
            PathOpenDecision::Openable(PathKind::Directory)
        }
    })
    .expect("a missing shorter candidate permits the longer one");
    assert_eq!(longer.candidate.display(), "OneDrive - Microsoft");

    assert!(select_openable_candidate(&candidates, |path| {
        if path.ends_with("OneDrive") {
            PathOpenDecision::Blocked
        } else {
            PathOpenDecision::Openable(PathKind::Directory)
        }
    })
    .is_none());
}

/// A trimmed prose path wins over its punctuation-ending literal; only its absence lets the literal open.
#[test]
fn candidate_selection_prefers_trimmed_path_over_literal() {
    let candidates = vec![
        probe_candidate("src/main.rs,", "/work/src/main.rs,", 0),
        probe_candidate("src/main.rs", "/work/src/main.rs", 0),
    ];
    let trimmed =
        select_openable_candidate(&candidates, |_| PathOpenDecision::Openable(PathKind::File))
            .expect("trimmed path wins");
    assert_eq!(trimmed.candidate.display(), "src/main.rs");

    let literal = select_openable_candidate(&candidates, |path| {
        if path.ends_with("main.rs") {
            PathOpenDecision::Missing
        } else {
            PathOpenDecision::Openable(PathKind::File)
        }
    })
    .expect("a missing trimmed path permits the literal");
    assert_eq!(literal.candidate.display(), "src/main.rs,");

    assert!(select_openable_candidate(&candidates, |path| {
        if path.ends_with("main.rs") {
            PathOpenDecision::Blocked
        } else {
            PathOpenDecision::Openable(PathKind::File)
        }
    })
    .is_none());
}

/// Openable and revealable results share one tier, where the earlier start wins the tie in either input order.
#[test]
fn actionable_candidate_kinds_tie_break_by_start() {
    let candidates = vec![
        probe_candidate("left.lua", "/work/left.lua", 0),
        probe_candidate("right.rs", "/work/right.rs", 9),
    ];
    for reversed in [false, true] {
        let mut ordered = candidates.clone();
        if reversed {
            ordered.reverse();
        }
        let selected = select_openable_candidate(&ordered, |path| {
            if path.ends_with("left.lua") {
                PathOpenDecision::Revealable(PathKind::File)
            } else {
                PathOpenDecision::Openable(PathKind::File)
            }
        })
        .expect("earlier candidate");
        assert_eq!(selected.candidate.display(), "left.lua");
        assert_eq!(selected.decision, PathOpenDecision::Revealable(PathKind::File));
    }
}

/// Revealable probe results authorize the same epoch-keyed activation path as openable results.
#[test]
fn revealable_selection_is_authorized_without_weakening_freshness() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/source.lua", 20);
    let request = state.request(key.clone()).expect("new candidate schedules a probe");
    let result = PathProbeResult {
        failure: None,
        selection: Some(PathProbeSelection {
            candidate: request.key.candidates[0].clone(),
            decision: PathOpenDecision::Revealable(PathKind::File),
        }),
        request,
    };

    assert!(state.accept(&result, Some(&key)));
    assert!(state.authorized(&key, true));
    assert!(!state.authorized(&key, false));
}

/// Real filesystem classification selects an existing short prefix first and the complete spaced entry once it is gone.
#[test]
fn filesystem_selection_prefers_shortest_existing_entry() {
    let root = native_test_root().join(format!(
        "sonicterm-spaced-selection-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let short = root.join("OneDrive");
    let complete = root.join("OneDrive - Microsoft");
    std::fs::create_dir_all(&short).unwrap();
    std::fs::create_dir(&complete).unwrap();
    let candidates = vec![
        PathProbeCandidate {
            spans: smallvec::smallvec![AbsoluteCellSpan { row: 0, start_col: 0, end_col: 20 }],
            target: DetectedTarget::BareName("OneDrive - Microsoft".into()),
            resolved_path: complete.clone(),
            joined: false,
        },
        PathProbeCandidate {
            spans: smallvec::smallvec![AbsoluteCellSpan { row: 0, start_col: 0, end_col: 8 }],
            target: DetectedTarget::BareName("OneDrive".into()),
            resolved_path: short.clone(),
            joined: false,
        },
    ];

    let selected = select_openable_candidate(&candidates, classify_local_target)
        .expect("short directory is openable");
    assert_eq!(selected.candidate.resolved_path, short);
    std::fs::remove_dir(&short).unwrap();
    let selected = select_openable_candidate(&candidates, classify_local_target)
        .expect("complete spaced directory is openable");
    assert_eq!(selected.candidate.resolved_path, complete);
    std::fs::remove_dir_all(root).unwrap();
}

/// Equal-length present candidates resolve to the earlier start rather than staying inert.
#[test]
fn equal_length_openable_candidates_prefer_earlier_start() {
    let candidates = vec![
        probe_candidate("Name Here", "/work/Name Here", 5),
        probe_candidate("Left Name", "/work/Left Name", 0),
    ];
    let selected =
        select_openable_candidate(&candidates, |_| PathOpenDecision::Openable(PathKind::Directory))
            .expect("earlier start");
    assert_eq!(selected.candidate.display(), "Left Name");
}

/// Filesystem probes authorize an openable entry and revoke the same entry once missing.
#[test]
fn filesystem_openability_controls_path_authorization() {
    let path = native_test_root().join(format!(
        "sonicterm-path-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::write(&path, b"present").unwrap();

    let mut state = PathProbeState::default();
    let key = probe_key(path.to_str().unwrap(), 20);
    let request = state.request(key.clone()).unwrap();
    let existing = PathProbeResult {
        failure: None,
        selection: select_openable_candidate(&request.key.candidates, classify_local_target),
        request,
    };
    assert!(state.accept(&existing, Some(&key)));
    assert!(state.authorized(&key, true));

    std::fs::remove_file(&path).unwrap();
    state.invalidate();
    let request = state.request(key.clone()).unwrap();
    let missing = PathProbeResult {
        failure: None,
        selection: select_openable_candidate(&request.key.candidates, classify_local_target),
        request,
    };
    assert!(state.accept(&missing, Some(&key)));
    assert!(!state.authorized(&key, true));
}
