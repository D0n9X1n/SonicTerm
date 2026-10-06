use std::sync::{Arc, Mutex};

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::{Parser, SyncState};

use super::{line_near_cursor, S10Candidate, S10Line, S10Watch};
use crate::app::sync_frame::SyncAdmission;
use crate::app::App;

/// Records the target of every event, whatever its level.
#[derive(Clone, Default)]
struct TargetLayer {
    targets: Arc<Mutex<Vec<String>>>,
}

impl<S: tracing::Subscriber> Layer<S> for TargetLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        self.targets.lock().expect("not poisoned").push(event.metadata().target().to_string());
    }
}

/// Run `body` with a subscriber that admits every event and return their targets.
fn event_targets(body: impl FnOnce()) -> Vec<String> {
    let layer = TargetLayer::default();
    let subscriber = Registry::default().with(layer.clone());
    sonicterm_logging::test_capture::with_default(subscriber, body);
    let targets = layer.targets.lock().expect("not poisoned").clone();
    targets
}

/// With the frame-counter gate off, arming any pane, seeded or unknown, returns no arming id, so a
/// harness reads attribution as unavailable, and neither arming nor disarming emits a line.
#[test]
fn with_the_gate_off_the_watch_arms_nothing_and_emits_nothing() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    assert!(app.frame_counters.is_none(), "the precondition: no subscriber opened the gate");
    let pane_id = app.__test_seed_tab("s10");
    let mut armed = Vec::new();
    let targets = event_targets(|| {
        armed.push(app.arm_s10_attribution(pane_id, "PERF_DONE 0 0123456789abcdef", "perf$ ", 300));
        armed.push(app.arm_s10_attribution(pane_id + 1_000, "PERF_DONE 0 0", "perf$ ", 1_200));
        app.disarm_s10_attribution(pane_id);
        app.disarm_s10_attribution(pane_id + 1_000);
    });
    assert_eq!(armed, vec![None, None], "no arming id is issued");
    assert!(targets.is_empty(), "no line is emitted: {targets:?}");
}

/// With the gate on, a seeded pane arms with a fresh id each time and re-arming replaces its watch;
/// an unknown pane, an empty or over-long marker and a phase of no updates arm nothing; disarm
/// removes the watch.
#[test]
fn with_the_gate_on_a_pane_arms_with_fresh_ids() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    let pane_id = app.__test_seed_tab("s10");
    let sentinel = "PERF_DONE 0 0123456789abcdef";
    assert_eq!(app.arm_s10_attribution(pane_id, sentinel, "perf$ ", 300), Some(1));
    assert_eq!(app.arm_s10_attribution(pane_id, sentinel, "perf$ ", 300), Some(2), "re-armed");
    let long = "x".repeat(129);
    let refused = [
        ("unknown pane", app.arm_s10_attribution(pane_id + 1_000, sentinel, "perf$ ", 300)),
        ("empty sentinel", app.arm_s10_attribution(pane_id, "", "perf$ ", 300)),
        ("long sentinel", app.arm_s10_attribution(pane_id, &long, "perf$ ", 300)),
        ("empty prompt", app.arm_s10_attribution(pane_id, sentinel, "", 300)),
        ("long prompt", app.arm_s10_attribution(pane_id, sentinel, &long, 300)),
        ("no updates", app.arm_s10_attribution(pane_id, sentinel, "perf$ ", 0)),
    ];
    for (name, armed) in refused {
        assert_eq!(armed, None, "{name}");
    }
    let watches = |app: &App| app.frame_counters.as_ref().unwrap().s10_watches.len();
    assert_eq!(watches(&app), 1, "one watch per pane");
    let at_limit = "y".repeat(128);
    assert_eq!(app.arm_s10_attribution(pane_id, &at_limit, &at_limit, 1), Some(3), "128 bytes fit");
    app.disarm_s10_attribution(pane_id);
    assert_eq!(watches(&app), 0, "disarm removes the watch");
}

/// The markers are found on the real primary screen restored by an alternate-screen exit, at the
/// saved cursor: the sentinel and the prompt are seen near the cursor, never while the alternate
/// screen shows the workload, and a line more than 8 rows above the cursor is not seen.
#[test]
fn the_markers_are_found_near_the_cursor_after_the_alternate_screen_exits() {
    let sentinel = "PERF_DONE 0 0123456789abcdef";
    let mut parser = Parser::new(Grid::new(40, 24));
    parser.advance(b"READY 0\r\n\x1b[?1049h\x1b[2J\x1b[1;1Hvim view\x1b[24;1Hstatus");
    assert!(!line_near_cursor(parser.grid(), sentinel, 8), "not on the alternate screen");
    parser.advance(b"\x1b[?1049lPERF_DONE 0 0123456789abcdef\r\n");
    assert!(line_near_cursor(parser.grid(), sentinel, 8), "the sentinel on the restored screen");
    assert!(!line_near_cursor(parser.grid(), "perf$ ", 8), "no prompt yet");
    parser.advance(b"perf$ ");
    assert!(line_near_cursor(parser.grid(), "perf$ ", 8), "then the prompt");
    // The prompt is one row below the sentinel, so 8 more line feeds leave the sentinel 9 rows above.
    parser.advance(&b"\r\n".repeat(8));
    assert!(!line_near_cursor(parser.grid(), sentinel, 8), "9 rows above the cursor is too far");
    assert!(line_near_cursor(parser.grid(), sentinel, 9), "and only that far");
}

/// A record of pane 7 with a closed update after `resets` resets.
fn candidate(resets: u64) -> S10Candidate {
    S10Candidate {
        pane_id: 7,
        state: SyncState { set: false, epoch: resets, resets },
        sentinel: false,
        prompt: false,
        admission: SyncAdmission::Closed,
    }
}

/// A watch emits `4 × updates + 64` present lines, then exactly one overflow line naming that count,
/// then nothing.
#[test]
fn a_watch_emits_its_capacity_then_one_overflow_line() {
    let mut watch = S10Watch::new(5, "PERF_DONE 0 0", "perf$ ", 1);
    let lines: Vec<_> = (0..80).filter_map(|resets| watch.admit(&candidate(resets))).collect();
    assert_eq!(lines.len(), 69, "68 present lines and one overflow line");
    assert!(lines[..68].iter().all(|line| matches!(line, S10Line::Present { arming: 5, .. })));
    assert_eq!(lines[68], S10Line::Overflow { arming: 5, pane_id: 7, emitted: 68 });
}

/// A writer that keeps everything a subscriber formats.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("not poisoned").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `body` under a plain-text WARN formatter, as the run log writes lines, and return its text.
fn formatted(body: impl FnOnce()) -> String {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || writer.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, body);
    let text = String::from_utf8(captured.0.lock().expect("not poisoned").clone()).expect("UTF-8");
    text
}

/// Only a presented attempt emits: each watched pane's line names the arming, the presented sequence,
/// the guarded update identity, the markers and the admission, at WARN on `sonic::perf_present`, in
/// the form the comparison parses. An attempt that did not present emits nothing.
#[test]
fn only_a_presented_attempt_emits_its_lines() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    let pane_id = app.__test_seed_tab("s10");
    let arming = app.arm_s10_attribution(pane_id, "PERF_DONE 0 0", "perf$ ", 300).unwrap();
    let record = S10Candidate {
        pane_id,
        state: SyncState { set: true, epoch: 4, resets: 3 },
        sentinel: true,
        prompt: false,
        admission: SyncAdmission::Credit,
    };
    let dropped = formatted(|| app.commit_s10_candidates(vec![record], None));
    assert!(dropped.is_empty(), "a failed or retried attempt emits nothing: {dropped}");
    let text = formatted(|| app.commit_s10_candidates(vec![record], Some(41)));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "{text}");
    let expected = format!(
        "WARN sonic::perf_present: perf_present kind=\"present\" arming={arming} seq=41 window=\"main\" \
         pane={pane_id} resets=3 epoch=4 set=true sentinel=true prompt=false admission=\"credit\""
    );
    assert!(lines[0].ends_with(&expected), "{}", lines[0]);
}

/// The main redraw copies the records under the frame's guards after the guarded recheck admits the
/// attempt and before the guards go to the renderer, and commits them only after the attempt
/// settles, numbering them with the main renderer's presented count.
#[test]
fn the_main_redraw_records_under_its_guards_and_commits_after_settling() {
    let source = include_str!("window_event.rs").replace("\r\n", "\n");
    let at = |needle: &str| source.find(needle).unwrap_or_else(|| panic!("{needle}"));
    let recheck = at("self.abandon_synchronized_frame(win_id, &sync_states, now)");
    let collect = at("self.s10_candidates(");
    let render = at("r.render_releasing(");
    let settle = at("self.complete_window_redraw(win_id, snapshot, outcome);");
    let commit = at("self.commit_s10_candidates(s10_candidates, presented_seq);");
    assert!(recheck < collect && collect < render, "copied under the guards after the recheck");
    assert!(settle < commit, "committed after the attempt settles");
    assert!(source.contains("Some(super::redraw::FrameSettlement::Presented)"));
    assert!(source.contains("GpuRenderer::successful_frame_count"));
}

/// The attributes on the item whose line is `item` in `source`, in source order, each joined onto one
/// line. As in Rust, blank and comment lines between an attribute and its item are skipped, and an
/// attribute may span several lines; any other line ends the item's attributes.
fn attributes_above(source: &str, item: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().map(str::trim).collect();
    let matches: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| **line == item)
        .map(|(line_index, _)| line_index)
        .collect();
    assert_eq!(matches.len(), 1, "exactly one `{item}` line");
    let mut attributes = Vec::new();
    let mut cursor = matches[0];
    'lines: while cursor > 0 {
        cursor -= 1;
        let line = lines[cursor];
        if line.is_empty() || line.starts_with("//") {
            // When: the line is blank or a comment, Rust still applies the attributes above it.
            continue;
        }
        if !line.ends_with(']') {
            // When: the line is no attribute's end, it belongs to another item.
            break;
        }
        // An attribute may span lines: walk back to the line that opens it.
        let mut opening = cursor;
        while !lines[opening].starts_with("#[") && !lines[opening].starts_with("#![") {
            let previous = lines[opening.saturating_sub(1)];
            if opening == 0 || previous.is_empty() || previous.ends_with([';', '{', '}']) {
                // When: no opening `#[` precedes the closing `]`, the line is not an attribute.
                break 'lines;
            }
            opening -= 1;
        }
        attributes.push(lines[opening..=cursor].join(" "));
        cursor = opening;
    }
    attributes.reverse();
    attributes
}

/// Whether any attribute, outer or inner, names a `cfg` or `cfg_attr`.
fn gates(attributes: &[impl AsRef<str>]) -> bool {
    attributes.iter().any(|attribute| attribute.as_ref().contains("cfg"))
}

/// Check that the watch methods in `source` (`perf_present.rs`) are never gated: not by the file's inner
/// attributes, the impl's attributes or any attribute inside it, nor by `mod perf_present;` in
/// `mod_source` (`app/mod.rs`).
fn assert_watch_methods_ungated(source: &str, mod_source: &str) {
    // A Windows checkout may use CRLF line endings; the `\n` searches below need plain LF.
    let source = &source.replace("\r\n", "\n");
    let mod_source = &mod_source.replace("\r\n", "\n");
    let inner: Vec<&str> =
        source.lines().map(str::trim).filter(|line| line.starts_with("#![")).collect();
    assert!(!gates(&inner), "the module is gated by an inner attribute: {inner:?}");
    let impl_attributes = attributes_above(source, "impl super::App {");
    assert!(!gates(&impl_attributes), "the impl is gated: {impl_attributes:?}");
    let methods = &source[source.find("impl super::App {").expect("the App impl")..];
    // The impl ends at its first closing brace in column 0; the test module follows it.
    let methods = &methods[..methods.find("\n}\n").expect("the impl ends")];
    for name in ["pub fn arm_s10_attribution(", "pub fn disarm_s10_attribution("] {
        assert!(methods.contains(name), "{name} is defined in the impl");
    }
    assert!(!methods.contains("#[cfg"), "a method in the impl is cfg-gated");
    let module_attributes = attributes_above(mod_source, "mod perf_present;");
    assert!(!gates(&module_attributes), "the module declaration is gated: {module_attributes:?}");
}

/// The methods are ordinary doc-hidden methods, never behind a cfg or a Cargo feature, so a harness
/// overlaid on any base that has them compiles and both sides build the same feature set. Every
/// enclosing gate counts: the file's inner attributes, the impl's attributes, the methods inside it,
/// and the `mod perf_present` declaration in `app/mod.rs`.
#[test]
fn the_attribution_watch_methods_are_never_cfg_gated() {
    assert_watch_methods_ungated(include_str!("perf_present.rs"), include_str!("mod.rs"));
}

/// A Windows checkout with CRLF line endings reads the same as an LF one: the check finds the impl's
/// end and the module declaration whatever the line endings, so CI on Windows runs the same check.
#[test]
fn the_gate_check_reads_a_crlf_checkout_as_an_lf_one() {
    let crlf = |source: &str| source.replace("\r\n", "\n").replace('\n', "\r\n");
    assert_watch_methods_ungated(
        &crlf(include_str!("perf_present.rs")),
        &crlf(include_str!("mod.rs")),
    );
}

/// The attribute reader sees a gate however Rust lets it be written: across a blank line, behind a
/// comment, or spread over several lines. An ungated item, and attributes that are not gates, read as
/// ungated, and a line ending in `]` that opens no attribute ends the scan.
#[test]
fn the_attribute_reader_follows_rust_attribute_placement() {
    let item = "mod perf_present;";
    let gated = [
        ("across a blank line", "#[cfg(test)]\n\nmod perf_present;\n"),
        ("over several lines", "#[cfg(\n    test\n)]\nmod perf_present;\n"),
        ("behind a comment", "#[cfg(test)]\n// the watch module\nmod perf_present;\n"),
        (
            "under another attribute",
            "#[cfg(test)]\n#[allow(\n    dead_code\n)]\nmod perf_present;\n",
        ),
    ];
    for (spelling, source) in gated {
        assert!(gates(&attributes_above(source, item)), "a gate {spelling} is seen");
    }
    let ungated = [
        "mod path_target;\nmod perf_present;\n",
        "#[doc(hidden)]\n\n#[allow(\n    dead_code\n)]\nmod perf_present;\n",
        "const ROWS: [u8; 2] = [\n    1, 2,\n]\nmod perf_present;\n",
    ];
    for source in ungated {
        assert!(!gates(&attributes_above(source, item)), "{source:?} is not gated");
    }
    assert_eq!(
        attributes_above("#[cfg(\n    test\n)]\n\nmod perf_present;\n", item),
        vec!["#[cfg( test )]".to_owned()],
        "a multiline attribute is read whole"
    );
}
