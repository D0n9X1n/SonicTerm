//! Recorded multiplexer target reconstruction and visible span contracts. regression contracts.

use super::*;

/// Every recording whose URL rows end at the pane edge resolves, at a cell in its first, a middle
/// and its last fragment, to the complete URL with every fragment underlined: wraps the pane
/// recorded, the bracketed hard wrap, and Claude Code's own edge-exact break in tmux and rmux,
/// full width and in a split.
#[test]
fn recorded_multiplexer_urls_resolve_from_every_fragment() {
    let mut failures = Vec::new();
    for case in RECORDED_CASES.iter().filter(|case| case.margin == 0) {
        let (app, window, pane) = replay_recording(case.screen_columns(), case.bytes);
        let spans = locate_url_pieces(&app, window, pane, &case.url_pieces());
        assert!(spans.len() >= 3, "{}: the URL spans only {} rows", case.name, spans.len());
        for (label, span) in [
            ("first", spans[0]),
            ("middle", spans[spans.len() / 2]),
            ("last", spans[spans.len() - 1]),
        ] {
            let target = app.cell_target_at(window, pane, span.row, span.start_col);
            let complete = target.as_ref().is_some_and(|target| {
                target.target == ResolvedCellTarget::Uri(CLAUDE_OSC8_URL.to_string())
                    && target.hover_cells.is_some_and(|cells| cells.spans() == spans.as_slice())
            });
            if !complete {
                failures.push(format!(
                    "{} {label} row {}: {}",
                    case.name,
                    span.row,
                    describe_target(target.as_ref())
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A program that leaves even one blank cell before the pane edge did not break the URL there,
/// so its rows are not joined: the first row resolves to its own fragment and a continuation row
/// to nothing, in tmux and rmux.
#[test]
fn recorded_urls_breaking_before_the_edge_are_not_joined() {
    let mut failures = Vec::new();
    for case in RECORDED_CASES.iter().filter(|case| case.margin > 0) {
        let (app, window, pane) = replay_recording(case.screen_columns(), case.bytes);
        let pieces = case.url_pieces();
        let spans = locate_url_pieces(&app, window, pane, &pieces);
        let first = app.cell_target_at(window, pane, spans[0].row, spans[0].start_col);
        if first.as_ref().map(|target| &target.target)
            != Some(&ResolvedCellTarget::Uri(pieces[0].clone()))
        {
            failures.push(format!("{} first: {}", case.name, describe_target(first.as_ref())));
        }
        let continuation = app.cell_target_at(window, pane, spans[1].row, spans[1].start_col);
        if continuation.is_some() {
            failures.push(format!(
                "{} continuation: {}",
                case.name,
                describe_target(continuation.as_ref())
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Claude Code's reply layout at every width it was measured at joins into the complete URL from
/// every row.
#[test]
fn claude_code_reply_urls_join_at_measured_widths() {
    for columns in [64, 80, 103, 120] {
        assert_claude_reply_joins(columns, CLAUDE_OSC8_URL);
    }
}

/// A break whose last fragment is a plain word, or digits of an issue number, still joins: no
/// character class is required of the last fragment.
#[test]
fn hard_wrapped_urls_join_plain_word_and_numeric_tails() {
    assert_claude_reply_joins(30, "https://example.com/docs/a/guide");
    assert_claude_reply_joins(30, "https://example.com/issue/1669");
}

/// A URL that starts a row's content after a `- ` or `1. ` marker joins its continuation rows,
/// which start at the URL's column.
#[test]
fn hard_wrapped_urls_after_list_markers_join() {
    for marker in ["- ", "1. "] {
        let width = 30 - marker.len();
        let url = "https://example.com/release/notes-for-v1-3-11";
        let output = format!(
            "\x1b[?1049h\x1b[1;1H{marker}{}\x1b[2;1H{}{}",
            &url[..width],
            " ".repeat(marker.len()),
            &url[width..]
        );
        let target = target_after(30, 4, &output, 1, marker.len() as u16).unwrap();
        assert_eq!(target.target, ResolvedCellTarget::Uri(url.to_string()), "{marker:?}");
    }
}

/// A URL ending one cell short of the pane edge was not broken by the program, so the row below
/// is never joined to it.
#[test]
fn hard_wrapped_url_one_cell_short_is_not_joined() {
    let url = "https://example.com/a/b/c/d/e";
    let columns = 2 + url.len() as u16 + 1;
    let output = format!("\x1b[?1049h\x1b[1;1H⏺ {url}\x1b[2;1H  more-text");
    let target = target_after(columns, 4, &output, 0, 4).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri(url.into()));
    assert!(target_after(columns, 4, &output, 1, 4).is_none());
}

/// A URL in the middle of a sentence never opens a chain, so the words wrapped after it stay prose.
#[test]
fn url_inside_prose_is_not_joined_with_the_next_row() {
    let output = "\x1b[?1049h\x1b[1;1Hsee https://example.com/a/b\x1b[2;1H  and more";
    let target = target_after(30, 4, output, 0, 6).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri("https://example.com/a/b".into()));
    assert!(target_after(30, 4, output, 1, 3).is_none());
}

/// A URL that starts mid-row after other text never opens a chain (a known limit, documented on
/// the Usage wiki page): it is not joined across a hanging indent. One cell short of the edge it
/// resolves on its own; reaching the edge, the existing fail-closed rule keeps it inert.
#[test]
fn mid_row_url_is_not_joined_across_a_hanging_indent() {
    let line = "Docs: https://example.com/docs/gu";
    let joined = ResolvedCellTarget::Uri("https://example.com/docs/guide".into());
    let short = format!("\x1b[?1049h\x1b[1;1H{line}\x1b[2;1H  ide");
    let columns = line.len() as u16 + 1;
    let target = target_after(columns, 4, &short, 0, 8).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri("https://example.com/docs/gu".into()));
    assert!(target_after(columns, 4, &short, 1, 2).is_none());
    let edge = target_after(line.len() as u16, 4, &short, 0, 8);
    assert!(edge.is_none_or(|target| target.target != joined), "a mid-row URL never joins");
}

/// Once a URL owns the row below, text after its last fragment makes the chain unprovable, so no
/// truncated prefix opens. A row that starts a new URL is never owned: the new URL resolves on
/// its own, and the edge row above keeps today's fail-closed result, never the two joined.
#[test]
fn hard_wrapped_url_followed_by_prose_or_another_target_stays_separate() {
    let url = "https://example.com/a/b/c/de";
    let columns = 2 + url.len() as u16;
    let prose = format!("\x1b[?1049h\x1b[1;1H⏺ {url}\x1b[2;1H  details here");
    assert!(target_after(columns, 4, &prose, 0, 4).is_none(), "owned chain followed by prose");
    assert!(target_after(columns, 4, &prose, 1, 3).is_none(), "the prose row is not a URL");
    let next_url = format!("\x1b[?1049h\x1b[1;1H⏺ {url}\x1b[2;1H  https://example.org/x");
    let first = target_after(columns, 4, &next_url, 0, 4);
    assert!(
        first.is_none_or(|target| !matches!(&target.target, ResolvedCellTarget::Uri(uri) if uri.contains("example.org"))),
        "two URLs are never joined"
    );
    let second = target_after(columns, 4, &next_url, 1, 4).unwrap();
    assert_eq!(second.target, ResolvedCellTarget::Uri("https://example.org/x".into()));
}

/// The bounded ambiguity of the rule: a URL that starts its row and ends exactly at the edge by
/// coincidence, followed by one token at the URL's start column, is joined. The paired cases show
/// that the same token at a deeper column, or followed by more text, is not.
#[test]
fn coincidental_edge_url_and_aligned_token_join() {
    let url = "https://example.com/a";
    let columns = 2 + url.len() as u16;
    let aligned = format!("\x1b[?1049h\x1b[1;3H{url}\x1b[2;1H  next");
    let target = target_after(columns, 4, &aligned, 0, 5).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri(format!("{url}next")));
    let deeper = format!("\x1b[?1049h\x1b[1;3H{url}\x1b[2;1H    next");
    assert!(
        target_after(columns, 4, &deeper, 1, 5).is_none(),
        "a deeper row is not a continuation"
    );
    let sentence = format!("\x1b[?1049h\x1b[1;3H{url}\x1b[2;1H  next word");
    let target = target_after(columns, 4, &sentence, 0, 5);
    assert!(
        target.is_none_or(|target| target.target != ResolvedCellTarget::Uri(format!("{url}next")))
    );
}

/// In a split, a URL reaching its pane's edge joins only rows of its own pane, never text in
/// the neighbouring pane, on either side of the border.
#[test]
fn hard_wrapped_urls_in_a_split_join_only_their_own_pane() {
    // A 41-column screen with its border in column 20: each pane is 20 columns wide, and each
    // `⏺ ` + 18-character URL fills its pane exactly.
    let mut output = format!("\x1b[?1049h{}", pane_border(21, 4));
    output.push_str("\x1b[1;1H⏺ https://a.com/x/yy\x1b[2;1H  tail-left");
    output.push_str("\x1b[1;22H⏺ https://b.com/y/zz\x1b[2;22H  tail-right");
    let left = target_after(41, 4, &output, 1, 3).unwrap();
    assert_eq!(left.target, ResolvedCellTarget::Uri("https://a.com/x/yytail-left".into()));
    let right = target_after(41, 4, &output, 1, 24).unwrap();
    assert_eq!(right.target, ResolvedCellTarget::Uri("https://b.com/y/zztail-right".into()));
}

/// OSC 8 hyperlink cells are never joined to a bare URL by the hard-wrap rule.
#[test]
fn hard_wrapped_url_never_joins_hyperlink_cells() {
    let url = "https://example.com/a/b/c/de";
    let columns = 2 + url.len() as u16;
    let link = "\x1b]8;;https://example.org/elsewhere\x1b\\";
    let output = format!("\x1b[?1049h\x1b[1;1H⏺ {url}\x1b[2;1H  {link}tail\x1b]8;;\x1b\\");
    let target = target_after(columns, 4, &output, 1, 3).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri("https://example.org/elsewhere".into()));
}

/// On the primary screen, Claude Code's edge-exact break joins as on the alternate screen, while
/// an `ls` listing after a URL that coincidentally fills its row is never joined to it.
#[test]
fn primary_screen_hard_wrapped_urls_join_and_listings_do_not() {
    let (alternate, spans) = claude_reply_rows(30, "https://example.com/docs/a/guide");
    let primary = alternate.replacen("\x1b[?1049h", "", 1);
    let target = target_after(30, 12, &primary, spans[1].row, 2).unwrap();
    assert_eq!(target.target, ResolvedCellTarget::Uri("https://example.com/docs/a/guide".into()));
    let url = "https://example.com/a/b/c/d/e/f/";
    let listing = format!("\x1b[1;1H{url}\x1b[2;1HCargo.toml  README.md  src");
    for (row, col) in [(0, 5), (1, 2)] {
        let target = target_after(url.len() as u16, 4, &listing, row, col);
        assert!(
            target.is_none_or(|target| !matches!(&target.target, ResolvedCellTarget::Uri(uri) if uri.len() > url.len())),
            "row {row}: a listing is never joined to the URL"
        );
    }
}

/// Re-records every case from the installed tmux and rmux and checks the rows that show the URL
/// still match the committed recordings, so a multiplexer update that changes how it draws is
/// noticed. Needs both multiplexers and Python 3; run with `--ignored`.
#[cfg(unix)]
#[test]
#[ignore = "re-records from the installed tmux and rmux"]
fn url_wrap_recordings_match_live_multiplexers() {
    let installed = |binary: &str| {
        std::process::Command::new(binary)
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success())
    };
    if !installed("tmux") || !installed("rmux") {
        // When: either multiplexer is missing, there is nothing live to compare against.
        eprintln!("skipped: tmux and rmux must both be installed");
        return;
    }
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let live = std::env::temp_dir().join(format!("sonic-url-wrap-live-{}", std::process::id()));
    let status = std::process::Command::new("python3")
        .arg(repository.join("scripts/capture-multiplexer-url-wrap.py"))
        .arg("--output-dir")
        .arg(&live)
        .status()
        .expect("run the capture script");
    let url_rows = |bytes: &[u8], case: &RecordedCase| {
        let (app, window, pane) = replay_recording(case.screen_columns(), bytes);
        let spans = locate_url_pieces(&app, window, pane, &case.url_pieces());
        let parser = app.windows[&window].panes[&pane].parser.lock();
        spans
            .iter()
            .map(|span| parser.grid().row(span.row).iter().map(|cell| cell.ch).collect::<String>())
            .collect::<Vec<_>>()
    };
    let mut changed = Vec::new();
    if status.success() {
        for case in RECORDED_CASES {
            let fresh = std::fs::read(live.join(format!("{}.bin", case.name)))
                .expect("read a fresh recording");
            if url_rows(&fresh, case) != url_rows(case.bytes, case) {
                changed.push(case.name);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&live);
    assert!(status.success(), "the capture script reported a recording without the URL");
    assert!(changed.is_empty(), "URL rows changed in {changed:?}");
}

/// Claude Code's OSC 8 link, wrapped at the right edge of a full-width tmux pane with a
/// two-space hanging indent, underlines every fragment and opens the complete URI from any row.
/// Synthetic equivalent of a recording of the real CLI through tmux 3.8 with `hyperlinks`.
#[test]
fn tmux_hyperlink_from_claude_code_joins_every_fragment() {
    let (rows, spans) = claude_osc8_rows(10, 1, 103);
    let output = format!("\x1b[?1049h{rows}");
    for span in &spans {
        let target = target_after(103, 20, &output, span.row, span.start_col + 1).unwrap();
        assert_eq!(target.target, ResolvedCellTarget::Uri(CLAUDE_OSC8_URL.to_string()));
        assert_eq!(target.hover_cells.unwrap().spans(), spans.as_slice());
    }
}

/// The same link in the right pane of a tmux split (border in column 52, pane from column 53)
/// spans four rows; every fragment underlines and opens the complete URI.
#[test]
fn tmux_split_hyperlink_from_claude_code_joins_every_fragment() {
    let (rows, spans) = claude_osc8_rows(14, 53, 51);
    assert!(spans.len() >= 4);
    let output = format!("\x1b[?1049h{}{rows}", pane_border(52, 20));
    for span in &spans {
        let target = target_after(103, 20, &output, span.row, span.start_col + 1).unwrap();
        assert_eq!(target.target, ResolvedCellTarget::Uri(CLAUDE_OSC8_URL.to_string()));
        assert_eq!(target.hover_cells.unwrap().spans(), spans.as_slice());
    }
}

/// A native absolute path a program breaks exactly at the pane edge, with continuation rows at its
/// start column, resolves from every row to one joined candidate on both screens. Every candidate
/// contains the hovered cell, and the key's row identity covers every chain row.
#[test]
fn hard_wrapped_native_path_joins_on_both_screens() {
    let path = native_path("tmp/sonic-hardwrap/deeply/nested/dir/file.txt");
    let (alternate, spans) = claude_reply_rows(30, &path);
    let primary = alternate.replacen("\x1b[?1049h", "", 1);
    for output in [&alternate, &primary] {
        for span in &spans {
            let target = target_after(30, 12, output, span.row, span.start_col + 1);
            let Some(ResolvedCellTarget::Path(key)) = target.map(|target| target.target) else {
                panic!("row {}: no path target", span.row);
            };
            let first = &key.candidates[0];
            assert!(first.joined, "row {}: the joined candidate comes first", span.row);
            assert_eq!(first.resolved_path, PathBuf::from(&path));
            assert!(key.candidates.iter().all(|candidate| candidate.contains(key.pointed)));
            for chain_row in &spans {
                assert!(key.rows.iter().any(|identity| identity.row == u64::from(chain_row.row)));
            }
        }
    }
}

/// The joined path is probed first: when it and the opening row's directory both exist it wins;
/// the fragment is used only when the joined path is definitely missing; a blocked joined path
/// never falls back to the fragment.
#[test]
fn joined_path_takes_precedence_over_its_fragment() {
    let fragment = probe_candidate_at("/w/dir/", "/w/dir", 2, 2);
    let joined = PathProbeCandidate {
        joined: true,
        ..probe_candidate_at("/w/dir/file.txt", "/w/dir/file.txt", 2, 2)
    };
    let candidates = vec![fragment.clone(), joined.clone()];
    let classify = |file: PathOpenDecision| {
        move |path: &Path| {
            if path == Path::new("/w/dir") {
                PathOpenDecision::Openable(PathKind::Directory)
            } else {
                file
            }
        }
    };
    let both = probe_candidates(&candidates, classify(PathOpenDecision::Openable(PathKind::File)));
    assert_eq!(both.unwrap().candidate, joined, "a break after a directory resolves the file");
    let missing = probe_candidates(&candidates, classify(PathOpenDecision::Missing));
    assert_eq!(missing.unwrap().candidate, fragment, "a missing join falls back to the fragment");
    let blocked = probe_candidates(&candidates, classify(PathOpenDecision::Blocked));
    assert_eq!(blocked.unwrap_err(), "path-error-blocked", "a blocked join never falls back");
}

/// A joined candidate appearing between hover and click revokes a fragment selected while it was
/// missing, both in `key_preserves_selection` and at open-time revalidation; a fragment
/// disappearing never revokes a joined selection.
#[test]
fn joined_candidates_appearing_between_hover_and_click_revoke_a_fragment() {
    let fragment = probe_candidate_at("/w/dir/", "/w/dir", 2, 2);
    let joined = PathProbeCandidate {
        joined: true,
        ..probe_candidate_at("/w/dir/file.txt", "/w/dir/file.txt", 2, 2)
    };
    let base = probe_key("/w/dir", 0);
    let key_with =
        |candidates: Vec<PathProbeCandidate>| PathProbeKey { candidates, ..base.clone() };
    let fragment_selected = PathProbeSelection {
        candidate: fragment.clone(),
        decision: PathOpenDecision::Openable(PathKind::Directory),
    };
    assert!(!key_preserves_selection(
        &key_with(vec![fragment.clone()]),
        &key_with(vec![joined.clone(), fragment.clone()]),
        &fragment_selected,
    ));
    let joined_selected = PathProbeSelection {
        candidate: joined.clone(),
        decision: PathOpenDecision::Openable(PathKind::File),
    };
    assert!(key_preserves_selection(
        &key_with(vec![joined.clone(), fragment.clone()]),
        &key_with(vec![joined.clone()]),
        &joined_selected,
    ));
    let contenders = [joined.clone(), fragment.clone()]
        .into_iter()
        .filter(|candidate| contends_with(candidate, &fragment))
        .collect::<Vec<_>>();
    assert!(contenders.contains(&joined), "a fragment selection re-probes the joined path");
    let request = PathOpenRequest {
        window_id: winit::window::WindowId::dummy(),
        pane_id: 7,
        path: PathBuf::from("/w/dir"),
        expected_decision: PathOpenDecision::Openable(PathKind::Directory),
        contenders,
    };
    let directory = |path: &Path| {
        if path == Path::new("/w/dir") {
            PathOpenDecision::Openable(PathKind::Directory)
        } else {
            PathOpenDecision::Missing
        }
    };
    assert!(open_request_still_selected(&request, directory), "the join is still missing");
    let both = |_: &Path| PathOpenDecision::Openable(PathKind::File);
    assert!(!open_request_still_selected(&request, both), "the joined file appeared since hover");
}

/// A click on a continuation row whose joined path is missing never opens the opening row's
/// fragment: every candidate of its key contains the clicked cell.
#[test]
fn continuation_click_never_opens_the_opening_row_fragment() {
    let path = native_path("tmp/sonic-hardwrap/missing/dir/file.txt");
    let (output, spans) = claude_reply_rows(30, &path);
    let primary = output.replacen("\x1b[?1049h", "", 1);
    let target = target_after(30, 12, &primary, spans[1].row, spans[1].start_col + 1).unwrap();
    let ResolvedCellTarget::Path(key) = target.target else { panic!("no path target") };
    assert!(key.candidates.iter().all(|candidate| candidate.contains(key.pointed)));
    assert!(key
        .candidates
        .iter()
        .all(|candidate| candidate.spans.iter().any(|span| span.row == key.pointed.row)));
    assert_eq!(
        probe_candidates(&key.candidates, |_| PathOpenDecision::Missing).unwrap_err(),
        "path-error-missing"
    );
}

/// A break inside a source reference's line-number suffix resolves to the full reference.
#[test]
fn hard_wrapped_source_reference_keeps_its_full_suffix() {
    let path = native_path("work/src/main.rs:1234:7");
    let split = path.find(":1234").unwrap() + 3;
    let (output, spans) = claude_reply_rows(split + 2, &path);
    let target =
        target_after(split as u16 + 2, 12, &output, spans[1].row, spans[1].start_col).unwrap();
    let ResolvedCellTarget::Path(key) = target.target else { panic!("no path target") };
    let first = &key.candidates[0];
    assert!(first.joined);
    assert!(matches!(first.target, DetectedTarget::SourceReference(_)), "{:?}", first.target);
    assert!(first.display().contains("1234"), "{}", first.display());
}

/// Primary-screen prose after a path that ends at the edge is never joined to it.
#[test]
fn primary_screen_prose_after_an_edge_path_is_not_joined() {
    let path = native_path("tmp/sonic-hardwrap/deeply/nested");
    let columns = 2 + path.len() as u16;
    let output = format!("\x1b[1;1H⏺ {path}\x1b[2;1H  and then more prose");
    for (row, col) in [(0, 4), (1, 3)] {
        assert!(
            !has_joined_path(target_after(columns, 4, &output, row, col).as_ref()),
            "row {row}"
        );
    }
}

/// UNC and network paths never open a chain on any platform, so nothing probes a host.
#[test]
fn hard_wrapped_unc_paths_are_never_joined() {
    let unc = r"\\server\share\deeply\nested\folder\file.txt";
    let (output, spans) = claude_reply_rows(24, unc);
    for span in &spans {
        assert!(
            !has_joined_path(target_after(24, 12, &output, span.row, span.start_col).as_ref()),
            "row {}",
            span.row
        );
    }
}

/// On Windows one detector serves every pane: a POSIX path printed by WSL or ssh and broken across
/// rows gets the same decision kind as the same path on one row; nothing maps it across hosts.
#[cfg(windows)]
#[test]
fn windows_posix_path_across_rows_keeps_the_single_row_decision() {
    let path = "/tmp/sonic-hardwrap/deeply/nested/dir/file.txt";
    let kind = |target: Option<CellTargetSnapshot>| {
        target.map(|target| std::mem::discriminant(&target.target))
    };
    let single = format!("\x1b[?1049h\x1b[1;1H⏺ {path}");
    let (wrapped, spans) = claude_reply_rows(30, path);
    assert_eq!(
        kind(target_after(30, 12, &wrapped, spans[0].row, 3)),
        kind(target_after(60, 12, &single, 0, 3))
    );
}

/// An OSC 8 link's visible text is captured whole, across a recorded wrap, and compared by the
/// status line; one that touches the view's top or bottom row is not read whole.
#[test]
fn osc8_label_is_captured_with_its_completeness() {
    let link = |label: &str| format!("\x1b]8;;https://evil.example/\x1b\\{label}\x1b]8;;\x1b\\");
    let inside = captured_label(&format!("\r\n{}", link("bank.example")), 40, 6, 1, 2).unwrap();
    assert_eq!(inside, LinkLabel { text: "bank.example".into(), complete: true });
    let wrapped =
        captured_label(&format!("\r\n{}", link("https://bank.example/")), 12, 8, 1, 2).unwrap();
    assert_eq!(wrapped, LinkLabel { text: "https://bank.example/".into(), complete: true });
    let top_row = captured_label(&link("bank.example"), 40, 6, 0, 2).unwrap();
    assert!(!top_row.complete, "a fragment on the top row may continue above the view");
    let check = sonicterm_ui::status_line::check_link_label(
        &inside.text,
        inside.complete,
        "https://evil.example/",
    );
    assert_eq!(
        check,
        sonicterm_ui::status_line::LinkLabelCheck::Mismatch { label_host: "bank.example".into() }
    );
}

/// The waiting count follows the queue: one per waiting window, unchanged by a replacement, and
/// lowered by a take or a cancel; a submit to a stopped worker leaves nothing counted.
#[test]
fn probe_mailbox_publishes_how_many_checks_wait() {
    let (mailbox, wakes) = PathProbeMailbox::new();
    let mut states: Vec<PathProbeState> = (0..3).map(|_| PathProbeState::default()).collect();
    for (window, state) in states.iter_mut().enumerate() {
        let key = window_probe_key(window as u64 + 1, &format!("/work/{window}"));
        mailbox.submit(state.request(key).unwrap()).unwrap();
    }
    assert_eq!(mailbox.waiting(), 3);
    states[0].invalidate();
    mailbox.submit(states[0].request(window_probe_key(1, "/work/again")).unwrap()).unwrap();
    assert_eq!(mailbox.waiting(), 3, "a replacement keeps the count");
    assert!(mailbox.take_next().is_some());
    assert_eq!(mailbox.waiting(), 2);
    assert!(mailbox.cancel_window(WindowId::from(2_u64)));
    assert_eq!(mailbox.waiting(), 1);
    drop(wakes);
    let mut stopped = PathProbeState::default();
    assert!(mailbox.submit(stopped.request(window_probe_key(9, "/work/late")).unwrap()).is_err());
    assert_eq!(mailbox.waiting(), 1, "a submit the stopped worker never takes is not counted");
}
