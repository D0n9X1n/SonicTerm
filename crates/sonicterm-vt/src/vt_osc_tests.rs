use super::*;
use crate::vt::vt_test_support::*;

/// OSC 52 surfaces the selection and encoded payload without decoding on the parser thread.
#[test]
fn osc52_clipboard_write_is_typed_for_the_host() {
    let mut parser = Parser::new(Grid::new(80, 24));

    let events = parser.advance(b"\x1b]52;c;Q29waWxvdCBjb3B5\x07");

    assert!(events.iter().any(|event| {
        matches!(
            event,
            VtEvent::Clipboard { selection: 'c', data }
                if data == "Q29waWxvdCBjb3B5"
        )
    }));
}

/// OSC 7 parsing keeps authority provenance while decoding the filesystem path.
#[test]
fn osc7_snapshot_preserves_authority_and_decodes_path() {
    assert_eq!(
        parse_osc7_cwd_snapshot("file://remote-host/home/user/My%20Files"),
        Some(Osc7Cwd { authority: "remote-host".into(), path: "/home/user/My Files".into() })
    );
    assert_eq!(
        parse_osc7_cwd_snapshot("file:///tmp/work"),
        Some(Osc7Cwd { authority: String::new(), path: "/tmp/work".into() })
    );
}

/// Malformed file authorities do not produce a path-resolution snapshot.
#[test]
fn osc7_snapshot_rejects_malformed_file_payloads() {
    assert_eq!(parse_osc7_cwd_snapshot("file://remote-host"), None);
    assert_eq!(parse_osc7_cwd_snapshot("file://"), None);
    assert_eq!(parse_osc7_cwd_snapshot("file://bad host/tmp"), None);
    assert_eq!(parse_osc7_cwd_snapshot("file:///tmp/bad%2"), None);
    assert_eq!(parse_osc7_cwd_snapshot("file:///tmp/bad%GG"), None);
    assert_eq!(parse_osc7_cwd_snapshot("file:///tmp/%0Aescape"), None);
}

/// Parser retains typed OSC 7 provenance while preserving the legacy cwd title path.
#[test]
fn parser_exposes_typed_osc7_snapshot_without_changing_cwd() {
    let mut parser = Parser::new(Grid::new(16, 2));
    parser.advance(b"\x1b]7;file://remote-host/home/user\x1b\\");

    assert_eq!(parser.cwd(), Some("/home/user"));
    assert_eq!(
        parser.osc7_cwd(),
        Some(&Osc7Cwd { authority: "remote-host".into(), path: "/home/user".into() })
    );
    let revision = parser.cwd_revision();

    // A malformed update remains available to legacy title rendering but
    // revokes the strict snapshot used to authorize relative local paths.
    parser.advance(b"\x1b]7;file:///tmp/bad%GG\x1b\\");
    assert_eq!(parser.cwd(), Some("/tmp/bad%GG"));
    assert_eq!(parser.osc7_cwd(), None);
    assert_ne!(parser.cwd_revision(), revision);
}

// Full OSC payloads survive separators, Unicode, both terminators, and every PTY chunk boundary.
#[test]
fn osc_semantic_payloads_preserve_full_text() {
    let title = format!("work · {}终端", "part;".repeat(20));
    let path = format!("/tmp/{}项目", "dir;".repeat(20));
    let uri = format!("https://example.com/{}路径", "part;".repeat(20));
    for terminator in ["\x07", "\x1b\\"] {
        let input = format!(
            "\x1b]2;{title}{terminator}\x1b]7;file://localhost{path}{terminator}\x1b]8;foo=bar:id=link;{uri}{terminator}X\x1b]8;;{terminator}Y"
        );
        for split in 0..=input.len() {
            let mut parser = Parser::new(Grid::new(8, 2));
            parser.advance(&input.as_bytes()[..split]);
            parser.advance(&input.as_bytes()[split..]);
            assert_eq!(parser.title(), Some(title.as_str()));
            assert_eq!(parser.osc7_cwd().unwrap().path, path);
            let link = parser.grid().row(0)[0].hyperlink().unwrap();
            let link = parser.hyperlinks().lookup(link).unwrap();
            assert_eq!(&*link.uri, &*uri);
            assert_eq!(link.id.as_deref(), Some("link"));
            assert!(parser.grid().row(0)[1].hyperlink().is_none());
            assert_eq!(parser.retained_amount().bytes, 0);
        }
    }
}

// A malformed hyperlink must not silently reuse the preceding link's target.
#[test]
fn osc_invalid_links_close_previous_link() {
    for invalid in [b"8;id=bad;\xff".as_slice(), b"8;missing-uri", b"8;;bad\nuri"] {
        let mut parser = Parser::new(Grid::new(8, 2));
        parser.advance(b"\x1b]8;id=old;https://example.com\x07X");
        parser.advance(&[b"\x1b]".as_slice(), invalid, b"\x07Y"].concat());
        assert!(parser.grid().row(0)[0].hyperlink().is_some());
        assert!(parser.grid().row(0)[1].hyperlink().is_none());
    }
}

// Whole-payload capture stays bounded and never dispatches cancelled or interrupted updates.
#[test]
fn osc_semantic_capture_is_bounded_and_recovers() {
    let cap = super::MAX_SEMANTIC_OSC_BYTES;
    for code in [0, 2, 7, 8] {
        for cancel in [b"\x18".as_slice(), b"\x1a", b"\x1b[0m"] {
            let mut parser = Parser::new(Grid::new(8, 2));
            parser.advance(format!("\x1b]{code};pending").as_bytes());
            parser.advance(cancel);
            parser.advance(b"X");
            assert_eq!(parser.grid().row(0)[0].ch, 'X');
            assert_eq!(parser.title(), None);
            assert_eq!(parser.osc7_cwd(), None);
            assert_eq!(parser.retained_amount().bytes, 0);
        }
        let mut parser = Parser::new(Grid::new(8, 2));
        parser.advance(format!("\x1b]{code};").as_bytes());
        parser.advance(&vec![b'a'; cap]);
        assert!(parser.retained_amount().bytes <= cap);
        parser.advance(b"b");
        assert!(parser.discarding_oversized_escape);
        assert_eq!(parser.retained_amount().bytes, 0);
        parser.advance(b"\x1b\\X");
        assert_eq!(parser.grid().row(0)[0].ch, 'X');
        assert_eq!(parser.title(), None);
    }
}

// Host cancellation clears inherited trust and keeps swallowing until the OSC terminator.
#[test]
fn osc_semantic_host_cancellation_clears_prior_link_and_cwd() {
    for pending in [b"\x1b]7;file:///pending".as_slice(), b"\x1b]8;;https://pending"] {
        let mut parser = Parser::new(Grid::new(8, 2));
        parser.advance(b"\x1b]7;file:///tmp\x07\x1b]8;;https://example.com\x07");
        parser.advance(pending);
        assert!(parser.cancel_capture() > 0);
        parser.advance(b"remaining\x07X");
        assert_eq!(parser.grid().row(0)[0].ch, 'X');
        if pending[2] == b'7' {
            assert!(parser.osc7_cwd().is_none());
        } else {
            assert!(parser.current_hyperlink().is_none());
        }
    }
}

// An OSC takeover cancels an incomplete DCS query even when every byte arrives separately.
#[test]
fn osc_semantic_takeover_does_not_complete_decrqss() {
    let mut parser = Parser::new(Grid::new(8, 2));
    let mut reply = Vec::new();
    for byte in b"\x1bP$qm\x1b]0;session;window\x1b\\" {
        reply.extend(parser.advance_with_replies(&[*byte]).2);
    }
    assert!(reply.is_empty());
    assert_eq!(parser.title(), Some("session;window"));
}

// Hyperlink identity uses id= only; unrecognized parameters cannot split the same group.
#[test]
fn osc_hyperlink_ids_ignore_unknown_parameters() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b]8;foo=a:id=same;https://example.com/a;b\x07X");
    parser.advance(b"\x1b]8;id=same:bar=b;https://example.com/a;b\x07Y");
    let row = parser.grid().row(0);
    assert_eq!(row[0].hyperlink(), row[1].hyperlink());
    parser.advance(b"\x1b]8;unknown=x;https://example.com\x07Z");
    let id = parser.grid().row(0)[2].hyperlink().unwrap();
    assert!(parser.hyperlinks().lookup(id).unwrap().id.is_none());
}

// Prompt input is not command execution, and one A-to-D cycle owns one grid region.
#[test]
fn osc133_prompt_input_does_not_start_command() {
    let mut parser = Parser::new(Grid::new(8, 2));
    let events = parser.advance(b"\x1b]133;A\x07\x1b]133;B\x07");
    assert!(!events
        .iter()
        .any(|event| matches!(event, VtEvent::Command(super::CommandEvent::CmdStart))));
    assert!(events
        .iter()
        .any(|event| matches!(event, VtEvent::Command(super::CommandEvent::PromptEnd))));
    assert_eq!(parser.grid().prompts_len(), 1);
    assert!(parser.grid().prompts().next().unwrap().end_row.is_none());
    let events = parser.advance(b"\x1b]133;C\x07\x1b]133;D;3\x07");
    assert!(events
        .iter()
        .any(|event| matches!(event, VtEvent::Command(super::CommandEvent::CmdStart))));
    assert_eq!(parser.grid().prompts_len(), 1);
    assert_eq!(parser.grid().prompts().next().unwrap().exit_code, Some(3));
}

#[test]
fn rejected_hyperlink_open_emits_close_event() {
    let mut parser = Parser::new(Grid::new(80, 24));
    parser.advance(b"\x1b]8;;https://example.com\x1b\\");
    let mut rejected = b"\x1b]8;;https://example.com/".to_vec();
    rejected.extend(std::iter::repeat_n(b'x', sonicterm_grid::hyperlink::MAX_HYPERLINK_URI_BYTES));
    rejected.extend_from_slice(b"\x1b\\");

    let events = parser.advance(&rejected);

    assert!(events
        .iter()
        .any(|event| matches!(event, VtEvent::Hyperlink { uri, .. } if uri.is_empty())));
}

/// A link written into the pane keeps working after the registry fills.
///
/// The registry is append-only in normal operation, so a pane that has seen
/// `MAX_HYPERLINKS` distinct links used to stop interning for the rest of the
/// session: every later OSC 8 rendered as plain text with no way to recover.
/// The links holding those slots are overwhelmingly ones whose cells scrolled
/// out of scrollback long ago.
///
/// This drives well past the cap through a small grid — so the early links
/// really are gone — and asserts the newest link is still resolvable, which
/// is what the user sees as a working hyperlink.
#[test]
fn a_pane_past_the_link_cap_still_renders_new_links() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let mut grid = Grid::new(20, 4);
    grid.set_scrollback_limit(8);
    let mut parser = Parser::new(grid);

    for index in 0..MAX_HYPERLINKS + 64 {
        parser.advance(
            format!("\x1b]8;;https://example.com/{index}\x07link\x1b]8;;\x07\r\n").as_bytes(),
        );
    }

    parser.advance(b"\x1b]8;;https://example.com/final\x07final\x1b]8;;\x07");

    // Assert on the cell the final link was written to, not the parser's
    // open-span state: the close sequence above correctly clears the latter,
    // while the cell is what the renderer draws and the user clicks. Search
    // that row specifically — earlier rows still hold earlier links.
    let final_row = parser.grid().cursor.row;
    let hid = parser.grid().row(final_row).iter().find_map(|cell| cell.hyperlink()).unwrap_or_else(
        || {
            panic!(
                "a link after {MAX_HYPERLINKS} others must still reach a cell; registry holds {}",
                parser.hyperlinks().len()
            )
        },
    );
    assert_eq!(
        parser.hyperlinks().lookup(hid).map(|link| &*link.uri),
        Some("https://example.com/final"),
        "the interned id must resolve to the URI the application sent"
    );
    assert!(
        parser.hyperlinks().len() <= MAX_HYPERLINKS,
        "reclamation must not push the registry past its own cap"
    );
}

/// Reclamation never frees a link the user can still see.
///
/// Both the visible screen and retained scrollback are live: a link scrolled
/// off screen but still within history is one the user reaches by scrolling
/// back, so freeing it would break a link that is still on display.
#[test]
fn reclamation_keeps_visible_scrollback_and_open_links() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let mut grid = Grid::new(20, 4);
    grid.set_scrollback_limit(64);
    let mut parser = Parser::new(grid);

    parser.advance(b"\x1b]8;;https://example.com/scrolled\x07history\x1b]8;;\x07\r\n");
    let scrollback_link = parser
        .grid()
        .row(0)
        .iter()
        .find_map(|cell| cell.hyperlink())
        .expect("the first row must carry the link just written");

    // Push it into scrollback but keep it within the retained history.
    for _ in 0..6 {
        parser.advance(b"filler\r\n");
    }
    parser.advance(b"\x1b]8;;https://example.com/visible\x07visible\x1b]8;;\x07\r\n");
    let visible_link = parser
        .grid()
        .rows_iter()
        .flat_map(sonicterm_grid::grid::Row::iter)
        .find_map(|cell| cell.hyperlink())
        .expect("a visible row must carry the second link");

    // Force the registry to its cap so the next intern triggers a sweep.
    for index in 0..MAX_HYPERLINKS + 8 {
        parser.advance(format!("\x1b]8;id=f{index};https://example.com/f{index}\x07").as_bytes());
    }

    for (label, hid) in [("scrollback", scrollback_link), ("visible", visible_link)] {
        assert!(
            parser.hyperlinks().lookup(hid).is_some(),
            "reclamation freed the {label} link, which is still referenced"
        );
    }
}

/// A full-screen program's links do not survive as garbage, but the primary
/// screen behind it does.
///
/// While the alt screen is active the grid holds the *primary* the user will
/// return to. A sweep that walked only the live screen would free every link
/// on the screen behind it, and they would come back dead the moment the
/// program exits.
#[test]
fn reclamation_reaches_the_screen_behind_the_alt_buffer() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let mut parser = Parser::new(Grid::new(20, 4));
    parser.advance(b"\x1b]8;;https://example.com/primary\x07primary\x1b]8;;\x07");
    let primary_link = parser
        .grid()
        .row(0)
        .iter()
        .find_map(|cell| cell.hyperlink())
        .expect("the primary screen must carry the link");

    parser.advance(b"\x1b[?1049h");
    assert!(parser.grid().is_alt(), "precondition: the alt screen is active");

    for index in 0..MAX_HYPERLINKS + 8 {
        parser.advance(format!("\x1b]8;id=a{index};https://example.com/a{index}\x07").as_bytes());
    }

    assert!(
        parser.hyperlinks().lookup(primary_link).is_some(),
        "a sweep during alt-screen use freed a link the primary screen still shows"
    );

    parser.advance(b"\x1b[?1049l");
    assert!(!parser.grid().is_alt());
    assert_eq!(
        parser.grid().row(0).iter().find_map(|cell| cell.hyperlink()),
        Some(primary_link),
        "the restored primary screen must still reference its link"
    );
    assert!(
        parser.hyperlinks().lookup(primary_link).is_some(),
        "the restored link must still resolve after leaving the alt screen"
    );
}

/// RIS drops every interned link.
///
/// A full reset erases every screen and the alt buffer, so no cell can still
/// reference an interned link. Retaining them would leave a fresh terminal
/// carrying the previous session's entire link set against its cap.
#[test]
fn ris_clears_the_hyperlink_registry() {
    let mut parser = Parser::new(Grid::new(20, 4));
    for index in 0..64 {
        parser
            .advance(format!("\x1b]8;;https://example.com/{index}\x07link\x1b]8;;\x07").as_bytes());
    }
    assert!(!parser.hyperlinks().is_empty(), "precondition: links are interned");

    parser.advance(b"\x1bc");

    assert!(parser.hyperlinks().is_empty(), "RIS must drop every interned link");
    assert_eq!(parser.hyperlinks().retained_bytes(), 0);
}

/// Links work again as soon as scrollback frees the registry.
///
/// After a sweep that frees nothing — every interned link genuinely still on
/// screen — the parser backs off before scanning again. That guard is right in
/// principle: rescanning per link when nothing is reclaimable turns a dead
/// feature into a stall.
///
/// But the backoff was set on a snapshot of grid state and never invalidated
/// when that state changed. Scrolling every link out of scrollback makes the
/// whole registry garbage, and the next links would still take the skip branch
/// — rendering as plain text despite megabytes being reclaimable. It
/// self-heals after the counter drains, which is exactly what makes it get
/// reported as "hyperlinks stop working sometimes" and resist reproduction.
#[test]
fn links_recover_as_soon_as_scrollback_frees_the_registry() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let mut grid = Grid::new(20, 4);
    // Deep enough to hold every link on screen, so the first sweep frees zero.
    grid.set_scrollback_limit(MAX_HYPERLINKS + 64);
    let mut parser = Parser::new(grid);

    for index in 0..MAX_HYPERLINKS {
        parser.advance(
            format!("\x1b]8;;https://example.com/{index}\x07link\x1b]8;;\x07\r\n").as_bytes(),
        );
    }

    // The registry is full and everything in it is still reachable, so the
    // next link fails and the parser backs off.
    parser.advance(b"\x1b]8;;https://example.com/wedged\x07x\x1b]8;;\x07");

    // Now shrink scrollback so every one of those links falls out of history.
    // Their cells are gone; the entire registry is garbage.
    parser.grid_mut().set_scrollback_limit(4);
    parser.advance(b"\r\n");

    // The very next link must work. It does not have to wait out a counter
    // that was set when the grid looked completely different.
    parser.advance(b"\x1b]8;;https://example.com/after-scrollback\x07visible\x1b]8;;\x07");

    let row = parser.grid().cursor.row;
    let hid = parser
        .grid()
        .row(row)
        .iter()
        .find_map(|cell| cell.hyperlink())
        .expect("the link written after scrollback shrank must reach a cell");
    assert_eq!(
        parser.hyperlinks().lookup(hid).map(|link| &*link.uri),
        Some("https://example.com/after-scrollback"),
        "a link must work as soon as scrollback frees the registry, not after \
         the backoff counter happens to drain"
    );
}

/// An oversized URI does not trigger a reclamation sweep.
///
/// The parser sweeps the grid and retries when the registry refuses a link.
/// That is right for a full registry and wrong for an oversized URI, which is
/// refused by a size check no sweep can change — an `O(visible + scrollback)`
/// walk on the VT hot path to reach the same answer, once per link.
///
/// Observed through the registry's contents rather than by counting sweeps:
/// a sweep that ran would free the unreferenced links this test parks in the
/// registry, so their survival is the evidence it did not run.
#[test]
fn an_oversized_uri_does_not_trigger_a_reclamation_sweep() {
    let mut grid = Grid::new(20, 4);
    grid.set_scrollback_limit(2);
    let mut parser = Parser::new(grid);

    // Park links in the registry, then scroll them out so a sweep would free
    // them. They are unreferenced garbage from here on.
    for index in 0..40 {
        parser.advance(
            format!("\x1b]8;;https://example.com/{index}\x07link\x1b]8;;\x07\r\n").as_bytes(),
        );
    }
    for _ in 0..20 {
        parser.advance(b"scroll\r\n");
    }
    let retained_before = parser.hyperlinks().len();
    assert!(retained_before > 0, "precondition: the registry holds reclaimable links");

    // An oversized URI: rejected on size, and no sweep can change that.
    let huge = "x".repeat(sonicterm_grid::hyperlink::MAX_HYPERLINK_URI_BYTES + 1);
    parser.advance(format!("\x1b]8;;{huge}\x07text\x1b]8;;\x07").as_bytes());

    assert_eq!(
        parser.hyperlinks().len(),
        retained_before,
        "an oversized URI must not trigger a sweep: the registry's contents changed, \
         so the parser scanned the whole grid to reach a rejection the size check had \
         already decided"
    );
}

/// A full registry still triggers a sweep, so the skip is targeted.
///
/// Without this the test above could pass by disabling reclamation entirely,
/// which would reintroduce the permanent hyperlink wedge.
#[test]
fn a_full_registry_still_triggers_a_reclamation_sweep() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let mut grid = Grid::new(20, 4);
    grid.set_scrollback_limit(2);
    let mut parser = Parser::new(grid);

    for index in 0..MAX_HYPERLINKS {
        parser.advance(
            format!("\x1b]8;;https://example.com/{index}\x07l\x1b]8;;\x07\r\n").as_bytes(),
        );
    }
    let full = parser.hyperlinks().len();
    assert_eq!(full, MAX_HYPERLINKS, "precondition: the registry is at its count cap");

    // A normal link against a full registry must sweep, free the scrolled-away
    // entries, and succeed.
    parser.advance(b"\x1b]8;;https://example.com/after\x07visible\x1b]8;;\x07");

    assert!(
        parser.hyperlinks().len() < full,
        "a retryable rejection must still sweep: {} entries retained, expected fewer \
         than {full}",
        parser.hyperlinks().len()
    );
    let row = parser.grid().cursor.row;
    assert!(
        parser.grid().row(row).iter().any(|cell| cell.hyperlink().is_some()),
        "and the link must end up on a cell"
    );
}

// ---------------------------------------------------------------------------
// Capture staging budget
//
// A capture is staging, not retained: it lives between an APC/DCS introducer
// and its terminator. What makes it worth bounding is that the terminator is
// not guaranteed to arrive — a stalled transfer pins its buffer until the pane
// dies, and no eviction pass can reclaim it.
// ---------------------------------------------------------------------------

/// A program-state event as the tests compare it; `VtEvent` holds media and is not `PartialEq`.
#[derive(Debug, PartialEq)]
enum Seen {
    Progress(Option<ProgressReport>),
    Notify { title: Option<String>, body: String },
    Reset,
}

/// Program-state events from `bytes`: progress, notifications and resets, in order.
fn program_events(bytes: &[u8]) -> Vec<Seen> {
    let mut parser = Parser::new(Grid::new(80, 24));
    parser
        .advance(bytes)
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::Progress(report) => Some(Seen::Progress(report)),
            VtEvent::Notify { title, body } => Some(Seen::Notify { title, body }),
            VtEvent::ProgramStateReset => Some(Seen::Reset),
            _ => None,
        })
        .collect()
}

/// The progress report `9;4;…` yields, or `None` when the sequence is ignored.
fn progress_of(params: &str) -> Option<Option<ProgressReport>> {
    let events = program_events(format!("\x1b]9;4;{params}\x1b\\").as_bytes());
    match events.as_slice() {
        [Seen::Progress(report)] => Some(*report),
        [] => None,
        other => panic!("one sequence yielded {other:?}"),
    }
}

#[test]
fn osc9_progress_accepts_each_state_and_clamps_the_percent() {
    // `9;4` maps state digits 0–4, clamps percents over 100, and reads a missing percent as 0.
    let report = |state, percent| Some(Some(ProgressReport { state, percent }));
    assert_eq!(progress_of("0"), Some(None), "0 clears");
    assert_eq!(progress_of("1;50"), report(ProgressState::Normal, 50));
    assert_eq!(progress_of("2;25"), report(ProgressState::Error, 25));
    assert_eq!(progress_of("3"), report(ProgressState::Indeterminate, 0));
    assert_eq!(progress_of("4;40"), report(ProgressState::Paused, 40));
    assert_eq!(progress_of("1;150"), report(ProgressState::Normal, 100));
    assert_eq!(progress_of("1"), report(ProgressState::Normal, 0));
    assert_eq!(progress_of("1;56;extra"), report(ProgressState::Normal, 56), "extras are ignored");
    // BEL terminates the same sequence as ST.
    let bel = program_events(b"\x1b]9;4;1;7\x07");
    assert_eq!(
        bel,
        vec![Seen::Progress(Some(ProgressReport { state: ProgressState::Normal, percent: 7 }))]
    );
}

#[test]
fn osc9_progress_ignores_every_malformed_form() {
    // A malformed report leaves the state unchanged: fractions are rejected, never rounded.
    for malformed in ["5", "12", "", "1;56.2", "1;-1", "1;+5", "1;1000", "1;"] {
        assert_eq!(progress_of(malformed), None, "9;4;{malformed}");
    }
}

#[test]
fn conemu_subcommands_are_never_notifications() {
    // `9;<1..=12>` is a ConEmu subcommand: only 4 is implemented, none falls through to text.
    assert!(program_events(b"\x1b]9;1;100\x1b\\").is_empty());
    assert!(program_events(b"\x1b]9;3;x\x1b\\").is_empty());
    assert!(program_events(b"\x1b]9;12\x1b\\").is_empty());
}

#[test]
fn plain_osc9_text_is_one_notification() {
    // Any other first parameter is notification text, its parameters rejoined with `;`.
    assert_eq!(
        program_events(b"\x1b]9;hello\x1b\\"),
        vec![Seen::Notify { title: None, body: "hello".to_owned() }]
    );
    assert_eq!(
        program_events(b"\x1b]9;hello;world\x07"),
        vec![Seen::Notify { title: None, body: "hello;world".to_owned() }]
    );
    assert_eq!(
        program_events(b"\x1b]9;13 files\x07"),
        vec![Seen::Notify { title: None, body: "13 files".to_owned() }],
        "13 is outside the subcommand range"
    );
}

#[test]
fn osc777_accepts_only_notify_and_rejoins_the_body() {
    // `777;notify;T;a;b` is title `T`, body `a;b`; other subcommands and empty ones are ignored.
    assert_eq!(
        program_events(b"\x1b]777;notify;T;a;b\x1b\\"),
        vec![Seen::Notify { title: Some("T".to_owned()), body: "a;b".to_owned() }]
    );
    assert!(program_events(b"\x1b]777;other;x\x1b\\").is_empty());
    assert!(program_events(b"\x1b]777;notify;;\x07").is_empty(), "empty title and body");
    assert_eq!(
        program_events(b"\x1b]777;notify;;body\x07"),
        vec![Seen::Notify { title: None, body: "body".to_owned() }],
        "an empty title is no title"
    );
}

#[test]
fn program_notifications_keep_every_part_past_the_parameter_cap() {
    // vte keeps 16 parameters; an OSC 9 or OSC 777 body of 20 `;`-separated parts arrives whole.
    let parts: Vec<String> = (0..20).map(|index| format!("p{index}")).collect();
    let joined = parts.join(";");
    assert_eq!(
        program_events(format!("\x1b]777;notify;T;{joined}\x1b\\").as_bytes()),
        vec![Seen::Notify { title: Some("T".to_owned()), body: joined.clone() }]
    );
    assert_eq!(
        program_events(format!("\x1b]9;{joined}\x07").as_bytes()),
        vec![Seen::Notify { title: None, body: joined }]
    );
}

#[test]
fn zero_padded_program_osc_numbers_are_captured_like_the_literal() {
    // `OSC 0777` and `OSC 09` are 777 and 9: their payloads are captured raw past vte's
    // 16-parameter cap, and the ConEmu subcommand rule after `09` still holds.
    let parts: Vec<String> = (0..20).map(|index| format!("p{index}")).collect();
    let joined = parts.join(";");
    assert_eq!(
        program_events(format!("\x1b]0777;notify;T;{joined}\x07").as_bytes()),
        vec![Seen::Notify { title: Some("T".to_owned()), body: joined.clone() }]
    );
    assert_eq!(
        program_events(format!("\x1b]09;{joined}\x1b\\").as_bytes()),
        vec![Seen::Notify { title: None, body: joined }]
    );
    assert!(program_events(b"\x1b]09;001;100\x07").is_empty(), "a zero-padded subcommand");
    assert_eq!(
        program_events(b"\x1b]0009;4;1;50\x07"),
        vec![Seen::Progress(Some(ProgressReport { state: ProgressState::Normal, percent: 50 }))]
    );
    // A title OSC with a zero-padded number keeps its own capture.
    let mut parser = Parser::new(Grid::new(80, 24));
    let events = parser.advance(b"\x1b]02;padded\x07");
    assert!(events
        .iter()
        .any(|event| matches!(event, VtEvent::SetTitle(title) if title == "padded")));
}

#[test]
fn program_osc_spellings_the_probe_misses_are_dropped_never_cut() {
    // Raw capture is the only parser of OSC 9 and 777: a number spelled past its 9-digit probe
    // falls to vte, which must emit nothing rather than a body cut at its 16-parameter cap.
    let parts: Vec<String> = (0..20).map(|index| format!("p{index}")).collect();
    let joined = parts.join(";");
    let long_777 = format!("\x1b]0000000777;notify;T;{joined}\x07");
    assert_eq!(program_events(long_777.as_bytes()), [], "a 10-digit 777");
    assert_eq!(program_events(b"\x1b]0000000009;hello\x07"), [], "a 10-digit 9");
    assert_eq!(program_events(b"\x1b]0000000009;4;1;50\x1b\\"), [], "no progress either");
    // The C1 OSC introducer bypasses raw capture the same way, so it is dropped too.
    assert_eq!(program_events(b"\x9d9;hello\x9c"), [], "C1 OSC 9");
    // Every spelling raw capture recognizes still delivers every part and every report.
    for number in ["777", "0777"] {
        let events = program_events(format!("\x1b]{number};notify;T;{joined}\x07").as_bytes());
        assert_eq!(events, [Seen::Notify { title: Some("T".to_owned()), body: joined.clone() }]);
    }
    for number in ["9", "09"] {
        let events = program_events(format!("\x1b]{number};{joined}\x1b\\").as_bytes());
        assert_eq!(events, [Seen::Notify { title: None, body: joined.clone() }], "{number}");
    }
    let progress =
        || Seen::Progress(Some(ProgressReport { state: ProgressState::Normal, percent: 50 }));
    assert_eq!(program_events(b"\x1b]9;4;1;50\x07"), [progress()], "BEL");
    assert_eq!(program_events(b"\x1b]9;4;1;50\x1b\\"), [progress()], "ST");
}

#[test]
fn a_whitespace_padded_notification_reaches_the_host_whole() {
    // The parser never cuts text before the host sanitizes it: 4096 spaces then `hello` keep
    // `hello`, which the host's whitespace collapse then shows.
    let padded = format!("{}hello", " ".repeat(4096));
    let events = program_events(format!("\x1b]9;{padded}\x07").as_bytes());
    assert_eq!(events, vec![Seen::Notify { title: None, body: padded }]);
}

#[test]
fn a_notification_past_the_raw_safety_bound_is_refused_whole() {
    // Past MAX_NOTIFY_BYTES the sequence is swallowed: no partial text, nothing printed.
    let mut parser = Parser::new(Grid::new(80, 24));
    let long = "x".repeat(MAX_NOTIFY_BYTES + 1);
    let events = parser.advance(format!("\x1b]777;notify;T;{long}\x07after").as_bytes());
    assert!(!events.iter().any(|event| matches!(event, VtEvent::Notify { .. })));
    assert_eq!(row_text(&parser, 0).trim_end(), "after", "the payload is swallowed, never printed");
}

#[test]
fn osc777_notify_requires_its_body_separator() {
    // The grammar is `notify;<title>;<body>`: a title with no `;` after it is malformed.
    assert!(program_events(b"\x1b]777;notify;T\x1b\\").is_empty());
    assert!(program_events(b"\x1b]777;notify\x07").is_empty());
    assert_eq!(
        program_events(b"\x1b]777;notify;T;\x07"),
        vec![Seen::Notify { title: Some("T".to_owned()), body: String::new() }],
        "an empty body after the separator is allowed"
    );
}

#[test]
fn zero_padded_conemu_subcommands_are_never_notifications() {
    // Any all-digit first parameter whose value is 1..=12 is a subcommand, leading zeros included.
    assert!(program_events(b"\x1b]9;001;100\x1b\\").is_empty());
    assert!(program_events(b"\x1b]9;0012\x07").is_empty());
    assert_eq!(
        program_events(b"\x1b]9;0004;1;50\x07"),
        vec![Seen::Progress(Some(ProgressReport { state: ProgressState::Normal, percent: 50 }))]
    );
    assert_eq!(
        program_events(b"\x1b]9;0013\x07"),
        vec![Seen::Notify { title: None, body: "0013".to_owned() }],
        "13 is outside the subcommand range"
    );
}

#[test]
fn ris_reports_a_program_state_reset() {
    // RIS clears program-reported state on the host side; the event carries no grid change.
    assert_eq!(program_events(b"\x1bc"), vec![Seen::Reset]);
}
