//! Preserved hover path-target regression cases.

use super::super::path_target_tests::*;
use super::*;

/// With the pointer still and the rows detection read unchanged while another row streams, every
/// frame after the first reuses the cached result: zero further detections and zero further probe
/// submissions, counted through the injected mailbox.
#[cfg(unix)]
#[test]
fn a_still_pointer_over_unchanged_rows_detects_once_while_other_rows_stream() {
    let (mut app, window, pane, mailbox) = counted_hover_app(40, 6);
    assert!(app.__test_advance_child_pane_parser(window, pane, b"/tmp/sonicterm-cache/notes.txt"));
    for frame in 0..20 {
        frame_hover(&mut app, window, pane, (0, 3));
        let streamed = format!("\x1b[5;1H{frame:>4} streamed");
        assert!(app.__test_advance_child_pane_parser(window, pane, streamed.as_bytes()));
    }
    assert_eq!(hover_counts(&app), (20, 1, 19));
    assert_eq!(drained_probes(&mailbox), 1, "only the first frame submits a probe");
    assert_eq!(app.frame_counters.as_ref().unwrap().hover.probes_submitted, 1);
}

/// A change to one row detection read causes exactly one detection on the next lookup, and the
/// lookup after it reuses that result.
#[cfg(unix)]
#[test]
fn one_dependency_row_change_detects_once_then_reuses() {
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 6);
    assert!(app.__test_advance_child_pane_parser(window, pane, b"/tmp/sonicterm-cache/notes.txt"));
    frame_hover(&mut app, window, pane, (0, 3));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1));
    assert!(app.__test_advance_child_pane_parser(window, pane, b"\x1b[1;36Hx"));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "the changed row detects again");
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (4, 2, 2), "and the next lookup reuses it");
}

/// Same-cell motion and a modifier change hit the cache, through both the frame path and the
/// event-time lookup, while the apply that follows still flips the underline's `active` state.
#[test]
fn same_cell_motion_and_modifier_changes_hit_the_cache_and_still_apply() {
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/"));
    let active = |app: &App| app.windows[&window].hovered_url.as_ref().unwrap().active();
    frame_hover(&mut app, window, pane, (0, 3));
    assert!(!active(&app));
    set_open_modifier(&mut app, window, true);
    let lookup = app.hover_lookup(window, pane, 0, 3);
    app.apply_target_lookup(window, lookup);
    assert!(active(&app), "the modifier activates the cached target");
    set_open_modifier(&mut app, window, false);
    frame_hover(&mut app, window, pane, (0, 3));
    assert!(!active(&app), "releasing it deactivates the cached target");
    assert_eq!(hover_counts(&app), (3, 1, 2));
}

/// A probe that completes for an unchanged key is applied by the frame it requests with no new
/// detection and no duplicate probe, and a same-value repaint of the target's row detects once and
/// yields an equal key, so the authorization stands.
#[cfg(unix)]
#[test]
fn probe_completion_and_same_value_repaint_keep_the_cached_authorization() {
    let root = native_test_root().join(format!(
        "sonicterm-hover-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("notes.txt");
    std::fs::write(&path, b"notes").unwrap();
    let (mut app, window, pane, mailbox) = counted_hover_app(200, 4);
    let text = path.to_str().unwrap().to_owned();
    assert!(app.__test_advance_child_pane_parser(window, pane, text.as_bytes()));
    frame_hover(&mut app, window, pane, (0, 3));
    let request = mailbox.take_next().expect("the first frame submits a probe");
    let outcome = probe_candidates(&request.key.candidates, classify_local_target);
    let key = request.key.clone();
    app.handle_path_probe_finished(PathProbeResult {
        failure: outcome.as_ref().err().copied(),
        selection: outcome.ok(),
        request,
    });
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "the completion's frame reuses the detection");
    assert_eq!(drained_probes(&mailbox), 0, "and submits no duplicate probe");
    assert!(app.windows[&window].path_probe.authorized(&key, true));
    let repaint = format!("\x1b[1;1H{text}");
    assert!(app.__test_advance_child_pane_parser(window, pane, repaint.as_bytes()));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "a same-value repaint detects once");
    let fresh = match app.cell_target_at(window, pane, 0, 3).unwrap().target {
        ResolvedCellTarget::Path(fresh) => fresh,
        other => panic!("a path target, not {other:?}"),
    };
    assert_eq!(fresh, key, "the recomputed key is equal");
    assert!(app.windows[&window].path_probe.authorized(&key, true), "authorization stands");
    assert_eq!(drained_probes(&mailbox), 0);
    std::fs::remove_dir_all(root).unwrap();
}

/// Every input of a detection invalidates the cache on its own: each change below is followed by
/// one lookup of the same cell, which must detect again. A theme change is not an input, so the
/// lookup after it hits.
#[test]
fn each_detection_input_invalidates_the_cache() {
    type Change = fn(&mut App, WindowId, u64);
    let changes: [(&str, Change); 10] = [
        ("resize", |app, window, pane| {
            app.windows[&window].panes[&pane].parser.lock().grid_mut().resize(41, 4);
        }),
        ("alt-screen enter", |app, window, pane| {
            assert!(app.__test_advance_child_pane_parser(window, pane, b"\x1b[?1049h"));
        }),
        ("cursor leave", |app, window, _| app.clear_target_hover(window)),
        ("pane hover invalidation", |app, window, _| {
            app.windows.get_mut(&window).unwrap().invalidate_path_hover();
        }),
        ("clickable_local_targets", |app, _, _| {
            app.config.terminal.clickable_local_targets ^= true;
        }),
        ("clickable_bare_names", |app, _, _| app.config.terminal.clickable_bare_names ^= true),
        ("clickable_path_max_chars", |app, _, _| app.config.terminal.clickable_path_max_chars = 9),
        ("cwd revision", |app, window, pane| {
            assert!(app.__test_advance_child_pane_parser(
                window,
                pane,
                b"\x1b]7;file:///tmp\x1b\\"
            ));
        }),
        ("dependency row write", |app, window, pane| {
            assert!(app.__test_advance_child_pane_parser(window, pane, b"\x1b[1;30Hz"));
        }),
        ("history clear", |app, window, pane| {
            app.windows[&window].panes[&pane].parser.lock().grid_mut().clear_scrollback();
        }),
    ];
    for (label, change) in changes {
        let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
        // Two rows of history, so the history clear has rows to drop.
        let output = b"one\r\ntwo\r\nhttps://example.com/\r\nfour\r\nfive";
        assert!(app.__test_advance_child_pane_parser(window, pane, output));
        frame_hover(&mut app, window, pane, (0, 3));
        frame_hover(&mut app, window, pane, (0, 3));
        assert_eq!(hover_counts(&app), (2, 1, 1), "{label}: warm");
        change(&mut app, window, pane);
        frame_hover(&mut app, window, pane, (0, 3));
        assert_eq!(hover_counts(&app).1, 2, "{label}: detects again");
    }
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/"));
    frame_hover(&mut app, window, pane, (0, 3));
    app.theme = Theme::default();
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "a theme change keeps the cached result");
}

/// Leaving the alternate screen invalidates the cache: the primary screen's rows are back on
/// display, so the next lookup detects again and resolves the primary screen's URL.
#[test]
fn alt_screen_exit_invalidates_the_cache() {
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
    let output = b"https://primary.example/\x1b[?1049h\x1b[1;1Hhttps://alternate.example/";
    assert!(app.__test_advance_child_pane_parser(window, pane, output));
    frame_hover(&mut app, window, pane, (0, 3));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "warm on the alternate screen");
    assert_eq!(
        app.windows[&window].hovered_url.as_ref().unwrap().url,
        "https://alternate.example/"
    );
    assert!(app.__test_advance_child_pane_parser(window, pane, b"\x1b[?1049l"));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "the exit detects again");
    assert_eq!(app.windows[&window].hovered_url.as_ref().unwrap().url, "https://primary.example/");
}

/// With history full and the view scrolled back, a line of output evicts the oldest history row:
/// the view keeps its text, but every absolute row index shifts, so the cache must not compare
/// stamps at the old indices. The eviction invalidates it and the next lookup detects again.
#[test]
fn history_eviction_while_scrolled_back_invalidates_the_cache() {
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
    let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
    parser.lock().grid_mut().set_scrollback_limit(10);
    let lines: String = (0..20).map(|line| format!("https://example.com/{line:02}\r\n")).collect();
    assert!(app.__test_advance_child_pane_parser(window, pane, lines.as_bytes()));
    let evicted_before = parser.lock().grid().scrollback_evicted();
    assert!(evicted_before > 0, "the fixture fills history");
    app.windows.get_mut(&window).unwrap().panes.get_mut(&pane).unwrap().pin_viewport_top(Some(2));
    frame_hover(&mut app, window, pane, (0, 3));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "warm while scrolled back");
    let hovered = app.windows[&window].hovered_url.as_ref().unwrap().url.clone();
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/new\r\n"));
    assert_eq!(parser.lock().grid().scrollback_evicted(), evicted_before + 1, "one row evicted");
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "the eviction detects again");
    assert_eq!(
        app.windows[&window].hovered_url.as_ref().unwrap().url,
        hovered,
        "the anchored view still shows, and resolves, the same row"
    );
}

/// The child-window frame path, `refresh_target_hover_from_parsers` with the held parser both
/// redraw adapters pass it, reuses the cache while nothing changes. Moving another tab out of the
/// window completes a topology change, which clears the cache, so the next frame detects again.
#[test]
fn the_child_frame_path_uses_the_cache_and_a_tab_transfer_invalidates_it() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    let window = app.__test_seed_child_window(&["moved", "pointed"]);
    let target = app.__test_seed_child_window(&["target"]);
    // A 400 × 80 px viewport of 10 × 20 px cells is a 40 × 4 grid with no renderer.
    let viewport = sonicterm_ui::pane::Rect::new(0.0, 0.0, 400.0, 80.0);
    for id in [window, target] {
        assert!(app.__test_set_child_pane_viewport(id, viewport, 10.0, 20.0));
    }
    let child = &app.windows[&window];
    assert_eq!(child.tabs.active_index(), 1, "the last seeded tab is active");
    let pane = child.tab_states[1].active_pane;
    for pane_state in app.windows[&window].panes.values() {
        pane_state.parser.lock().grid_mut().resize(40, 4);
    }
    assert!(app.__test_advance_child_pane_parser(window, pane, b"https://example.com/"));
    // The pointer rests on row 0, column 3 of the pointed pane.
    app.windows.get_mut(&window).unwrap().cursor_pos = (35.0, 10.0);
    let frame = |app: &mut App| {
        let parser = Arc::clone(&app.windows[&window].panes[&pane].parser);
        let guard = parser.lock();
        app.refresh_target_hover_from_parsers(window, [(pane, &*guard)]);
    };
    frame(&mut app);
    frame(&mut app);
    assert_eq!(hover_counts(&app), (2, 1, 1), "the frame path reuses the cache");
    assert_eq!(app.windows[&window].hovered_url.as_ref().unwrap().url, "https://example.com/");
    app.transfer_tab(Some(window), 0, Some(target), 1).expect("the inactive tab moves");
    assert_eq!(app.windows[&window].tab_states[0].active_pane, pane, "the pointed tab stays");
    assert!(app.windows[&window].hover_detection.is_empty(), "the transfer cleared the cache");
    frame(&mut app);
    frame(&mut app);
    assert_eq!(hover_counts(&app), (4, 2, 2), "detects once after the transfer, then reuses");
}

/// Authorization is revoked through the cache: a probed, authorized path whose row is rewritten
/// to a different path is detected again on the next frame, the old key is no longer authorized,
/// and exactly one probe is submitted, for the new path.
#[cfg(unix)]
#[test]
fn rewriting_an_authorized_path_revokes_it_through_the_cache() {
    let root = native_test_root().join(format!(
        "sonicterm-hover-revoke-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let (first_path, second_path) = (root.join("first.txt"), root.join("other.txt"));
    for path in [&first_path, &second_path] {
        std::fs::write(path, b"notes").unwrap();
    }
    let (first_text, second_text) =
        (first_path.to_str().unwrap().to_owned(), second_path.to_str().unwrap().to_owned());
    let (mut app, window, pane, mailbox) = counted_hover_app(200, 4);
    assert!(app.__test_advance_child_pane_parser(window, pane, first_text.as_bytes()));
    frame_hover(&mut app, window, pane, (0, 3));
    let request = mailbox.take_next().expect("the first frame submits a probe");
    let outcome = probe_candidates(&request.key.candidates, classify_local_target);
    let first_key = request.key.clone();
    app.handle_path_probe_finished(PathProbeResult {
        failure: outcome.as_ref().err().copied(),
        selection: outcome.ok(),
        request,
    });
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "the completion's frame reuses the detection");
    assert!(app.windows[&window].path_probe.authorized(&first_key, true));
    let submitted_before = app.frame_counters.as_ref().unwrap().hover.probes_submitted;

    let rewrite = format!("\x1b[1;1H{second_text}\x1b[K");
    assert!(app.__test_advance_child_pane_parser(window, pane, rewrite.as_bytes()));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "the rewritten row detects again");
    assert!(
        !app.windows[&window].path_probe.authorized(&first_key, true),
        "the old path's key is no longer authorized"
    );
    let second_request = mailbox.take_next().expect("the new path submits a probe");
    assert_ne!(second_request.key, first_key);
    assert!(
        second_request.key.candidates.iter().any(|candidate| candidate.display() == second_text),
        "the probe is for the new path"
    );
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(drained_probes(&mailbox), 0, "exactly one new probe");
    assert_eq!(app.frame_counters.as_ref().unwrap().hover.probes_submitted, submitted_before + 1);
    std::fs::remove_dir_all(root).unwrap();
}

/// Authorization is revoked through the cache on every platform, Windows included: a relative path
/// resolved against the pane's OSC 7 directory and authorized by the platform classifier is
/// detected again once its row is rewritten to another file, the old key loses its authorization,
/// and exactly one probe is submitted, for the new path.
#[test]
fn rewriting_an_authorized_relative_path_revokes_it_through_the_cache() {
    let root = native_test_root().join(format!(
        "sonicterm-hover-revoke-relative-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(root.join("notes")).unwrap();
    let (first_text, second_text) = ("notes/first.txt", "notes/other.txt");
    for relative in [first_text, second_text] {
        std::fs::write(root.join(relative), b"notes").unwrap();
    }
    // File URIs carry a slash-prefixed drive path on Windows.
    let uri_path = if cfg!(windows) {
        PathBuf::from(format!("/{}", root.to_str().unwrap().replace('\\', "/")))
    } else {
        root.clone()
    };
    let cwd = local_file_uri(&uri_path).unwrap();
    let (mut app, window, pane, mailbox) = counted_hover_app(80, 4);
    let first_output = format!("\x1b]7;{cwd}\x1b\\{first_text}");
    assert!(app.__test_advance_child_pane_parser(window, pane, first_output.as_bytes()));
    frame_hover(&mut app, window, pane, (0, 3));
    let request = mailbox.take_next().expect("the first frame submits a probe");
    let outcome = probe_candidates(&request.key.candidates, classify_local_target);
    assert_eq!(
        outcome.as_ref().map(|selection| selection.candidate.resolved_path.clone()),
        Ok(root.join(first_text)),
        "the platform classifier authorizes the first file"
    );
    let first_key = request.key.clone();
    app.handle_path_probe_finished(PathProbeResult {
        failure: outcome.as_ref().err().copied(),
        selection: outcome.ok(),
        request,
    });
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (2, 1, 1), "the completion's frame reuses the detection");
    assert!(app.windows[&window].path_probe.authorized(&first_key, true));
    let submitted_before = app.frame_counters.as_ref().unwrap().hover.probes_submitted;

    let rewrite = format!("\x1b[1;1H{second_text}\x1b[K");
    assert!(app.__test_advance_child_pane_parser(window, pane, rewrite.as_bytes()));
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(hover_counts(&app), (3, 2, 1), "the rewritten row detects again");
    assert!(
        !app.windows[&window].path_probe.authorized(&first_key, true),
        "the old path's key is no longer authorized"
    );
    let second_request = mailbox.take_next().expect("the new path submits a probe");
    assert_ne!(second_request.key, first_key);
    assert_eq!(
        probe_candidates(&second_request.key.candidates, classify_local_target)
            .map(|selection| selection.candidate.resolved_path),
        Ok(root.join(second_text)),
        "the probe is for the new path"
    );
    frame_hover(&mut app, window, pane, (0, 3));
    assert_eq!(drained_probes(&mailbox), 0, "exactly one new probe");
    assert_eq!(app.frame_counters.as_ref().unwrap().hover.probes_submitted, submitted_before + 1);
    std::fs::remove_dir_all(root).unwrap();
}

/// The cache holds one entry per window: another cell (a font or DPI change moves the pointer to
/// one), another pane, another view top or another window each detect, and each window keeps its
/// own entry, so returning to a window's cell hits.
#[test]
fn the_cache_is_one_entry_per_window_keyed_by_cell_pane_and_view() {
    let (mut app, first, first_pane, _mailbox) = counted_hover_app(40, 4);
    let second = app.__test_seed_child_window(&["second", "tab"]);
    let panes = app.__test_child_pane_ids(second).unwrap();
    let output: &[u8] = b"one\r\ntwo\r\nhttps://example.com/\r\nfour\r\nfive\r\nsix";
    assert!(app.__test_advance_child_pane_parser(first, first_pane, output));
    for pane in &panes {
        app.windows[&second].panes[pane].parser.lock().grid_mut().resize(40, 4);
        assert!(app.__test_advance_child_pane_parser(second, *pane, output));
    }
    frame_hover(&mut app, first, first_pane, (0, 3));
    frame_hover(&mut app, second, panes[0], (0, 3));
    assert_eq!(hover_counts(&app), (2, 2, 0), "each window detects for itself");
    frame_hover(&mut app, first, first_pane, (0, 3));
    frame_hover(&mut app, second, panes[0], (0, 3));
    assert_eq!(hover_counts(&app), (4, 2, 2), "and keeps its own entry");
    frame_hover(&mut app, first, first_pane, (0, 4));
    assert_eq!(hover_counts(&app).1, 3, "another cell detects");
    if let Some(other_pane) = panes.get(1) {
        frame_hover(&mut app, second, *other_pane, (0, 3));
        assert_eq!(hover_counts(&app).1, 4, "another pane detects");
    }
    let before = hover_counts(&app).1;
    frame_hover_at(&mut app, first, first_pane, (0, 3), Some(0));
    assert_eq!(hover_counts(&app).1, before + 1, "another view top detects");
}

/// An OSC 8 link rewritten with the same label and a different destination is detected again and
/// resolves to the new destination: the rewrite restamps the row, so no hyperlink id has to join
/// the key.
#[test]
fn an_osc8_link_with_the_same_label_and_a_new_destination_detects_again() {
    let (mut app, window, pane, _mailbox) = counted_hover_app(40, 4);
    let link = |uri: &str| format!("\x1b[1;1H\x1b]8;;{uri}\x1b\\label\x1b]8;;\x1b\\");
    assert!(app.__test_advance_child_pane_parser(
        window,
        pane,
        link("https://a.example/").as_bytes()
    ));
    frame_hover(&mut app, window, pane, (0, 2));
    assert_eq!(app.windows[&window].hovered_url.as_ref().unwrap().url, "https://a.example/");
    assert!(app.__test_advance_child_pane_parser(
        window,
        pane,
        link("https://b.example/").as_bytes()
    ));
    frame_hover(&mut app, window, pane, (0, 2));
    assert_eq!(hover_counts(&app), (2, 2, 0));
    assert_eq!(app.windows[&window].hovered_url.as_ref().unwrap().url, "https://b.example/");
}

/// Span mapping by binary search returns what the linear scans it replaced return, on a recorded-
/// wrap line of exactly 16 KiB (the cap) whose spaced tokens give many candidates per cell: each
/// mapping helper asserts its result against the linear scan under tests, at every pointed cell.
#[test]
fn span_mapping_matches_the_linear_scan_on_a_full_sixteen_kib_line() {
    let (cols, rows) = (512_u16, 32_u16);
    let mut grid = Grid::new(cols, rows);
    let unit = "./ab cd ef gh ij kl mn op ";
    let text = unit.repeat(usize::from(cols) * usize::from(rows) / unit.len() + 1);
    for character in text.chars().take(usize::from(cols) * usize::from(rows)) {
        grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
    }
    let mut most_candidates = 0;
    for col in (0..cols).step_by(7) {
        let pointed = AbsoluteCell { row: 16, col };
        if let Some(scan) = logical_path_scan_at_cell(&grid, 0, pointed, PathStyle::Posix, true) {
            assert!(scan
                .candidates
                .iter()
                .all(|candidate| candidate.spans.iter().any(|span| span.contains(pointed))));
            most_candidates = most_candidates.max(scan.candidates.len());
        }
    }
    assert!(most_candidates >= 8, "the line offers many candidates per cell: {most_candidates}");
}
