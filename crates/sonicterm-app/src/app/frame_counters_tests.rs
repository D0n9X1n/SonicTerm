//! Pins the frame-counter core: histogram bounds, the line cadence, `lock_parser`'s gate and
//! dispatch scope, the VT section record and the flush slot.

pub(super) use std::sync::atomic::{AtomicU64, Ordering};
pub(super) use std::sync::Arc;
pub(super) use std::time::{Duration, Instant};

pub(super) use parking_lot::Mutex;

use super::*;
pub(super) use crate::app::App;

/// `start` plus `micros` microseconds.
pub(super) fn after_us(start: Instant, micros: u64) -> Instant {
    start + Duration::from_micros(micros)
}

#[test]
pub(super) fn a_source_prints_at_most_one_line_a_second_and_one_final_line() {
    // Only authorizing events print; maintenance wakes never do, and nothing follows final=1.
    let start = Instant::now();
    let mut cadence = LineCadence::default();
    assert!(cadence.line_due(start, true));
    assert!(!cadence.line_due(start + Duration::from_millis(400), true));
    assert!(!cadence.line_due(start + Duration::from_secs(2), false), "maintenance wake");
    assert!(cadence.line_due(start + Duration::from_secs(2), true));
    assert!(!cadence.final_line(false), "no pending counts, no final line");
    assert!(!cadence.line_due(start + Duration::from_secs(9), true), "closed source");
    let mut open = LineCadence::default();
    assert!(open.final_line(true));
    assert!(!open.final_line(true), "final=1 is printed once");
}

/// A new App under `filter`, so its gate does not depend on any process-wide subscriber.
pub(super) fn app_under_filter(filter: &str) -> crate::app::App {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    let subscriber = Registry::default().with(EnvFilter::new(filter));
    sonicterm_logging::test_capture::with_default(subscriber, || {
        crate::app::App::new(Theme::default(), Config::default(), Keymap::default())
    })
}

#[test]
pub(super) fn an_apps_gate_follows_the_frame_counters_target_when_it_is_built() {
    // Debug logging turns the counters on; the default level leaves them off.
    assert!(app_under_filter("warn,frame_counters=debug").frame_counters.is_some());
    assert!(app_under_filter("warn").frame_counters.is_none());
}

#[test]
pub(super) fn forcing_the_gate_succeeds_only_before_the_first_window_or_pane() {
    // Once a window or pane exists, something may already have read the gate.
    let mut early = app_under_filter("warn");
    assert_eq!(early.force_frame_counters_on(), Ok(()));
    assert!(early.frame_counters.is_some());
    let mut after_window = app_under_filter("warn");
    after_window.__test_synthetic_main();
    assert_eq!(after_window.force_frame_counters_on(), Err(FrameCountersTooLate));
    assert!(after_window.frame_counters.is_none());
    let mut after_pane = app_under_filter("warn");
    assert!(after_pane.pane_frame_counters().is_none());
    assert_eq!(after_pane.force_frame_counters_on(), Err(FrameCountersTooLate));
}

#[test]
pub(super) fn two_apps_keep_independent_gates_and_each_pane_shares_its_apps_statistics() {
    // A pane's worker records into its own App's statistics through its own flush slot.
    let mut counting = app_under_filter("warn");
    counting.force_frame_counters_on().unwrap();
    let silent = &mut app_under_filter("warn");
    let first = counting.pane_frame_counters().expect("gate on");
    let second = counting.pane_frame_counters().expect("gate on");
    let app_vt = &counting.frame_counters.as_ref().unwrap().vt;
    assert!(Arc::ptr_eq(&first.vt, app_vt) && Arc::ptr_eq(&second.vt, app_vt));
    assert!(!Arc::ptr_eq(&first.pending_flush, &second.pending_flush));
    assert!(silent.pane_frame_counters().is_none());
}

#[test]
pub(super) fn both_pane_creators_attach_counters_before_starting_the_worker() {
    // The worker clones its handles from the pane, so they must be attached first.
    for (name, source) in [
        ("spawn_pane.rs", to_lf(include_str!("spawn_pane.rs"))),
        ("child_tabs.rs", to_lf(include_str!("child_tabs.rs"))),
    ] {
        let attach = source
            .find("frame_counters = self.pane_frame_counters();")
            .unwrap_or_else(|| panic!("{name} attaches no counters"));
        assert!(source[attach..].contains("spawn_pane_workers("), "{name} starts its worker first");
    }
}

#[test]
pub(super) fn a_counting_apps_dispatch_scope_counts_each_parser_lock() {
    // The App opens a scope at each dispatch; its event-loop parser locks drain into its totals.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    let parser = Mutex::new(0_u32);
    {
        let _dispatch = app.frame_dispatch_scope();
        drop(lock_parser(&parser));
    }
    let totals = &app.frame_counters.as_ref().unwrap().dispatch;
    assert_eq!(totals.locks.load(Ordering::Relaxed), 1);
    assert_eq!(totals.wait.snapshot().count(), 1);
}

#[test]
pub(super) fn every_application_handler_method_opens_a_dispatch_scope() {
    // A dispatch without a scope would lock parsers uncounted.
    let module = to_lf(concat!(
        include_str!("mod.rs"),
        "\n",
        include_str!("application_handler.rs"),
        "\n",
        include_str!("user_event.rs")
    ));
    assert_eq!(handler_methods(&module).len(), 6, "the handler methods changed; review this test");
    assert_eq!(handlers_without_one_dispatch_scope(&module), Vec::<String>::new());
}

/// Each handler method whose body does not open exactly one dispatch scope, with its count.
/// A second scope in one handler takes the spare tally and allocates another per dispatch, so
/// two is as wrong as none.
pub(super) fn handlers_without_one_dispatch_scope(module: &str) -> Vec<String> {
    handler_methods(module)
        .into_iter()
        .filter_map(|(name, body)| {
            let scope_count = body.matches("let _dispatch = self.frame_dispatch_scope();").count();
            (scope_count != 1).then(|| format!("{name}: {scope_count} scopes"))
        })
        .collect()
}

#[test]
pub(super) fn the_dispatch_scope_check_rejects_a_missing_or_a_second_scope() {
    // The real handler impl, edited two ways: one handler opens a second scope, another opens none.
    // The check must name both, so neither a duplicate nor a gap can pass.
    let module = to_lf(concat!(
        include_str!("mod.rs"),
        "\n",
        include_str!("application_handler.rs"),
        "\n",
        include_str!("user_event.rs")
    ));
    let scope_line = "let _dispatch = self.frame_dispatch_scope();";
    let start = module.find("impl ApplicationHandler<UserEvent> for App {").expect("handler impl");
    let first = start + module[start..].find(scope_line).expect("a first scope");
    let second = first
        + scope_line.len()
        + module[first + scope_line.len()..].find(scope_line).expect("a second scope");
    let mut edited = module.clone();
    edited.replace_range(second..second + scope_line.len(), "let _no_scope = ();");
    edited.insert_str(first + scope_line.len(), &format!("\n        {scope_line}"));
    let flagged = handlers_without_one_dispatch_scope(&edited);
    assert_eq!(flagged.len(), 2, "{flagged:?}");
    assert!(flagged.iter().any(|entry| entry.ends_with(": 2 scopes")), "{flagged:?}");
    assert!(flagged.iter().any(|entry| entry.ends_with(": 0 scopes")), "{flagged:?}");
}

/// Every non-test source under `dir` (recursively), as `(path relative to root, text)`.
pub(super) fn app_sources(
    root: &std::path::Path,
    dir: &std::path::Path,
    found: &mut Vec<(String, String)>,
) {
    for entry in std::fs::read_dir(dir).expect("app sources") {
        let entry_path = entry.expect("dir entry").path();
        if entry_path.is_dir() {
            app_sources(root, &entry_path, found);
            continue;
        }
        let name = entry_path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".rs") && !name.ends_with("_tests.rs") && !name.starts_with("test_hooks_")
        {
            let relative =
                entry_path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            found.push((relative, to_lf(&std::fs::read_to_string(&entry_path).unwrap())));
        }
    }
}

/// `text` with each line comment blanked to spaces, so commented-out code never counts; byte
/// offsets and line numbers are unchanged. A line with a quote before `//` is kept whole.
pub(super) fn without_line_comments(text: &str) -> String {
    text.split('\n')
        .map(|line| match line.find("//") {
            Some(start) if !line[..start].contains('"') => {
                format!("{}{}", &line[..start], " ".repeat(line.len() - start))
            }
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The byte range of top-level function `name` in `text`, through its closing `}` at column 0.
pub(super) fn function_range(text: &str, name: &str) -> Option<std::ops::Range<usize>> {
    let needle = format!("fn {name}");
    let start = text
        .match_indices(&needle)
        .map(|(offset, _)| offset)
        .find(|offset| matches!(text.as_bytes().get(offset + needle.len()), Some(b'(' | b'<')))?;
    let end = start + text[start..].find("\n}\n")? + 3;
    Some(start..end)
}

/// Each plain `.lock()` call in `text` outside `exempt`, as `(line, receiver)`. Whitespace and
/// line breaks between the receiver, the dot and the call are allowed, so a call split across
/// lines is still found; a call receiver such as `slot()` keeps its parentheses.
pub(super) fn plain_locks(text: &str, exempt: &[std::ops::Range<usize>]) -> Vec<(usize, String)> {
    let code = without_line_comments(text);
    let bytes = code.as_bytes();
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut found = Vec::new();
    for (offset, _) in code.match_indices("lock") {
        let word_start = offset == 0 || !is_ident(bytes[offset - 1]);
        let word_end = bytes.get(offset + 4).is_none_or(|byte| !is_ident(*byte));
        let before = code[..offset].trim_end();
        let after = code[offset + 4..].trim_start();
        let empty_call = after.starts_with('(') && after[1..].trim_start().starts_with(')');
        if !word_start || !word_end || !before.ends_with('.') || !empty_call {
            continue;
        }
        if exempt.iter().any(|range| range.contains(&offset)) {
            continue;
        }
        let receiver_end = before[..before.len() - 1].trim_end();
        let (stem, suffix) = match receiver_end.strip_suffix("()") {
            Some(stem) => (stem, "()"),
            None => (receiver_end, ""),
        };
        let ident_start = stem
            .char_indices()
            .rev()
            .take_while(|(_, character)| character.is_alphanumeric() || *character == '_')
            .last()
            .map_or(stem.len(), |(index, _)| index);
        let ident = &stem[ident_start..];
        let receiver =
            if ident.is_empty() { "<expression>".to_owned() } else { format!("{ident}{suffix}") };
        found.push((code[..offset].matches('\n').count() + 1, receiver));
    }
    found
}

/// Problems with `sources`' parser locks: a plain `lock()` whose file and receiver are not in
/// the verified non-parser inventory (outside the `exempt` functions), a per-file `lock_parser`
/// count that differs from its inventory, or an inventory entry nothing matches any more.
pub(super) fn parser_lock_audit(
    sources: &[(String, String)],
    non_parser: &[(&str, &str, usize)],
    pane_parser: &[(&str, usize)],
    exempt: &[(&str, &str)],
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen: std::collections::BTreeMap<(String, String), Vec<usize>> = Default::default();
    for (file, text) in sources {
        // A CRLF checkout is read as LF, so function ends and comment blanking match either way.
        let text = &to_lf(text);
        let ranges: Vec<_> = exempt
            .iter()
            .filter(|(exempt_file, _)| exempt_file == file)
            .filter_map(|(_, name)| function_range(text, name))
            .collect();
        for (line, receiver) in plain_locks(text, &ranges) {
            seen.entry((file.clone(), receiver)).or_default().push(line);
        }
        let converted = without_line_comments(text).matches("lock_parser(").count();
        let listed =
            pane_parser.iter().find(|(listed, _)| listed == file).map_or(0, |entry| entry.1);
        if converted != listed {
            problems
                .push(format!("{file}: {converted} lock_parser calls, inventory says {listed}"));
        }
    }
    for ((file, receiver), lines) in &seen {
        let listed = non_parser
            .iter()
            .find(|(listed, name, _)| listed == file && name == receiver)
            .map(|entry| entry.2);
        match listed {
            Some(count) if count == lines.len() => {}
            Some(count) => problems.push(format!(
                "{file}:{lines:?} {receiver}.lock() taken {} times, inventory says {count}",
                lines.len()
            )),
            None => problems.push(format!(
                "{file}:{lines:?} plain {receiver}.lock() is not a verified non-parser lock; \
                 lock a pane parser with lock_parser"
            )),
        }
    }
    for (file, receiver, _) in non_parser {
        if !seen.contains_key(&((*file).to_owned(), (*receiver).to_owned())) {
            problems.push(format!("{file}: inventory lists {receiver}.lock(), none found"));
        }
    }
    for (file, _) in pane_parser {
        if !sources.iter().any(|(source, _)| source == file) {
            problems.push(format!("{file}: inventory lists lock_parser calls, file not found"));
        }
    }
    problems
}

/// Every plain `lock()` in the crate's non-test sources by file and receiver, each verified to
/// lock a mutex that is not a pane parser: event-loop proxies, the reaper's state, drag
/// snapshots, media charges, command queues, redraw targets and the per-window native-request
/// totals. The worker's image-store and
/// command-queue locks sit inside its exempt section.
pub(super) const NON_PARSER_LOCKS: &[(&str, &str, usize)] = &[
    ("menubar_bridge.rs", "proxy_slot()", 2),
    ("menubar_bridge.rs", "queue()", 2),
    ("launch_queue.rs", "proxy_slot()", 2),
    ("launch_queue.rs", "queue()", 2),
    ("os_drag_bridge.rs", "proxy_slot()", 2),
    ("os_drag_bridge.rs", "tab_queue()", 2),
    ("os_drag_bridge.rs", "file_queue()", 2),
    ("os_drag.rs", "inner", 2),
    ("taskbar_progress.rs", "slot", 2),
    ("taskbar_progress.rs", "thread_slot", 1),
    ("app/fg_probe.rs", "map", 9),
    ("app/fg_probe.rs", "proxy", 1),
    ("app/fg_probe.rs", "worker", 3),
    ("app/reaper_driver.rs", "abandoned", 3),
    ("app/reaper_driver.rs", "state", 2),
    ("app/reaper_driver.rs", "observations", 2),
    ("app/reaper_driver.rs", "paths", 3),
    ("app/window_setup/windows.rs", "brushes", 1),
    ("app/media.rs", "charge", 3),
    ("app/command_events.rs", "command_events", 1),
    // The request targets an accessibility adapter's handler resolves against; a plain std mutex.
    ("app/accessibility.rs", "targets", 4),
    // The event-loop proxy a default-handler completion posts its result through.
    ("app/default_handlers.rs", "proxy", 1),
    ("app/os_drag.rs", "snapshots", 6),
    ("app/os_drag.rs", "moved", 2),
    ("app/os_drag.rs", "ended", 3),
    ("app/pty_test_support.rs", "output", 3),
    ("app/redraw_target.rs", "redraw_target", 1),
    ("app/shared_gpu.rs", "proxy", 1),
    ("app/session.rs", "redraw_target", 1),
    ("app/tab_state.rs", "redraw_target", 1),
    ("app/tear_out.rs", "redraw_target", 1),
    ("app/path_target.rs", "queue", 1),
    ("app/input_dispatch.rs", "test_pty_writes", 1),
    ("app/frame_counters.rs", "native", 8),
    ("app/echo_watch.rs", "slot", 1),
    // The VT worker's media helpers, after the parser section: the pane's image store, not a parser.
    ("app/spawn_pane_media.rs", "inline_images", 2),
    ("app/guard_correlation.rs", "state", 5),
    ("bin/pty_multi_round_helper.rs", "stdin", 1),
    ("bin/pty_multi_round_helper.rs", "stdout", 1),
];

/// Every pane-parser lock the event-loop thread takes, by file; each goes through lock_parser.
pub(super) const PANE_PARSER_LOCKS: &[(&str, usize)] = &[
    // The About pane's column count, read for its layout and for the resize check, and its text feed.
    ("app/about_window.rs", 3),
    ("app/child_tabs.rs", 1),
    ("app/child_window.rs", 2),
    ("app/child_window_pointer.rs", 1),
    ("app/config_apply.rs", 2),
    ("app/keymap_dispatch/explicit_source.rs", 1),
    // The report pane's theme seed at creation, and the report feed that replaces its text.
    ("app/memory_report_window.rs", 2),
    ("app/misc.rs", 5),
    ("app/overlays.rs", 2),
    // The local status line a held pane or a failed command launch writes into its own grid.
    ("app/pane_exit.rs", 1),
    ("app/pane_refresh.rs", 3),
    ("app/pane_state.rs", 1),
    ("app/scroll.rs", 1),
    ("app/search_handle.rs", 2),
    // The pane spawn, and the worker latch reading the parser once before its first batch.
    ("app/spawn_pane.rs", 2),
    ("app/viewport_anchor.rs", 1),
    ("app/window_keyboard.rs", 2),
];

/// The only functions allowed a plain parser lock: the VT worker's own section, which runs off
/// the event-loop thread, and the counting helper that lock_parser wraps.
pub(super) const WORKER_LOCK_SECTIONS: &[(&str, &str)] = &[
    ("app/spawn_pane.rs", "process_pane_vt_batch_with"),
    ("app/frame_counters.rs", "lock_counted"),
];

#[test]
pub(super) fn event_loop_pane_parser_locks_go_through_lock_parser() {
    // An uncounted lock would hide event-loop waits. Every plain lock() outside the worker
    // section is a verified non-parser mutex and every converted site is listed, so a new
    // site of either kind fails until it is classified.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    app_sources(&root, &root, &mut sources);
    let problems =
        parser_lock_audit(&sources, NON_PARSER_LOCKS, PANE_PARSER_LOCKS, WORKER_LOCK_SECTIONS);
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
pub(super) fn the_parser_lock_audit_reports_each_unconverted_site() {
    // Negative fixtures: a call split across lines, another receiver name and a parser lock
    // outside the exempt worker function are each reported; try_lock and comments are not.
    let fixture = "fn process_batch() {\n    let guard = handles.parser.lock();\n}\n\n\
                   fn redraw() {\n    let guard = handles\n        .parser\n        .lock();\n    \
                   let other = vt_state . lock ( );\n    let fine = state.try_lock();\n    \
                   // stale.lock() in a comment\n}\n";
    let sources = vec![("app/fixture.rs".to_owned(), fixture.to_owned())];
    let worker = [("app/fixture.rs", "process_batch")];
    let problems = parser_lock_audit(&sources, &[], &[], &worker);
    assert_eq!(problems.len(), 2, "{problems:#?}");
    assert!(problems.iter().any(|problem| problem.contains("[8] plain parser.lock()")));
    assert!(problems.iter().any(|problem| problem.contains("[9] plain vt_state.lock()")));
    let unexempt = parser_lock_audit(&sources, &[], &[], &[]);
    assert!(
        unexempt.iter().any(|problem| problem.contains("[2, 8] plain parser")),
        "{unexempt:#?}"
    );
    let listed = [("app/fixture.rs", "parser", 1), ("app/fixture.rs", "vt_state", 1)];
    assert!(parser_lock_audit(&sources, &listed, &[], &worker).is_empty());
    // A converted site the inventory does not list is reported too, so the inventory stays exact.
    let converted = vec![(
        "app/fixture.rs".to_owned(),
        "fn redraw() { lock_parser(&pane.parser); }\n".to_owned(),
    )];
    assert_eq!(parser_lock_audit(&converted, &[], &[], &[]).len(), 1);
}

/// Give every pane of window `id` counters, as pane creation does with the gate on.
pub(super) fn attach_pane_counters(app: &mut App, id: winit::window::WindowId) {
    let pane_ids: Vec<u64> = app.windows[&id].panes.keys().copied().collect();
    for pane_id in pane_ids {
        let counters = app.pane_frame_counters().expect("gate on");
        app.windows.get_mut(&id).unwrap().panes.get_mut(&pane_id).unwrap().frame_counters =
            Some(counters);
    }
}

/// The pending-flush slot of pane `pane_id` in window `id`.
pub(super) fn flush_slot(app: &App, id: winit::window::WindowId, pane_id: u64) -> &AtomicU64 {
    let pane = &app.windows[&id].panes[&pane_id];
    pane.frame_counters.as_ref().expect("counting pane").pending_flush.as_ref()
}

#[test]
pub(super) fn a_background_tabs_flush_waits_until_its_tab_is_shown() {
    // A redraw measures only the panes it draws; a hidden tab's flush survives it and is taken
    // by the first redraw after its tab is shown.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    let child = app.__test_seed_child_window(&["front", "back"]);
    attach_pane_counters(&mut app, child);
    let window = app.windows.get_mut(&child).unwrap();
    window.tabs.activate(0);
    let back_pane = window.tab_states[1].active_pane;
    flush_slot(&app, child, back_pane).store(flush_clock_ns(), Ordering::Release);
    app.note_redraw_requested(child);
    let taken = |app: &App| {
        let counters = app.windows[&child].redraw.frame_counters.as_deref().expect("counting");
        (counters.redraw_requested, counters.flush_to_redraw.count())
    };
    assert_eq!(taken(&app), (1, 0), "the foreground redraw left the hidden flush");
    assert_ne!(flush_slot(&app, child, back_pane).load(Ordering::Acquire), 0);
    app.windows.get_mut(&child).unwrap().tabs.activate(1);
    app.note_redraw_requested(child);
    assert_eq!(taken(&app), (2, 1), "shown, its flush is measured once");
    assert_eq!(flush_slot(&app, child, back_pane).load(Ordering::Acquire), 0);
}

#[test]
pub(super) fn a_pane_hidden_by_zoom_keeps_its_flush() {
    // While another pane is zoomed the hidden pane is not drawn, so its flush stays pending.
    use sonicterm_cfg::keymap::Direction;
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    let child = app.__test_seed_child_window(&["front", "back"]);
    attach_pane_counters(&mut app, child);
    let window = app.windows.get_mut(&child).unwrap();
    // The back tab's pane is reused as the front tab's second leaf; only the front tab is shown.
    let back_pane = window.tab_states[1].active_pane;
    window.tabs.activate(0);
    let front = &mut window.tab_states[0];
    let front_pane = front.active_pane;
    assert!(front.tree.split(front_pane, Direction::Right, back_pane));
    assert!(front.tree.toggle_zoom(front_pane));
    flush_slot(&app, child, back_pane).store(flush_clock_ns(), Ordering::Release);
    flush_slot(&app, child, front_pane).store(flush_clock_ns(), Ordering::Release);
    app.note_redraw_requested(child);
    let counters = app.windows[&child].redraw.frame_counters.as_deref().expect("counting");
    assert_eq!(counters.flush_to_redraw.count(), 1, "only the zoomed pane was measured");
    assert_ne!(flush_slot(&app, child, back_pane).load(Ordering::Acquire), 0);
}

#[test]
pub(super) fn a_closing_childs_handler_time_reaches_closed_windows() {
    // A child's CloseRequested retires its counters inside the dispatch; the dispatch's own
    // duration, recorded after, must still land in closed_windows rather than vanish.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    let child = app.__test_seed_child_window(&["only"]);
    let started = Instant::now() - Duration::from_millis(5);
    let counted = app.begin_window_handler(child);
    assert!(app.close_child_window(child));
    app.note_window_handler(child, started, counted);
    let snapshot = app.frame_counters_snapshot().expect("counting app");
    assert_eq!(snapshot.closed_windows.histogram_count("handler"), Some(1));
    assert!(snapshot.closed_windows.histogram_sum_us("handler").unwrap() >= 5_000);
}

#[test]
pub(super) fn window_event_decides_its_destination_before_dispatching() {
    // The window may be gone when the handler returns, so whether it counted is read first.
    let body = source_span(
        concat!(
            include_str!("mod.rs"),
            "\n",
            include_str!("application_handler.rs"),
            "\n",
            include_str!("user_event.rs")
        ),
        "    fn window_event(",
        "\n    }\n",
    )
    .expect("window_event");
    let decided = body.find("self.begin_window_handler(win_id)").expect("destination read");
    assert!(decided < body.find("self.do_window_event(").expect("dispatch"), "{body}");
    assert!(body.contains("self.note_window_handler(win_id, started, counted)"), "{body}");
}

/// A log sink shared between a test and the subscriber it installs.
#[derive(Clone, Default)]
pub(super) struct CapturedLog(pub(super) Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
pub(super) fn a_closing_childs_final_line_carries_its_closing_handler_time() {
    // A child's CloseRequested retires its counters inside the dispatch; the final=1 line must
    // wait for that dispatch's handler time, or no log line ever exports it. window_event makes
    // exactly these calls around do_window_event.
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let log = CapturedLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn,frame_counters=debug"))
        .with_writer(move || writer.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, || {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        let child = app.__test_seed_child_window(&["only"]);
        let started = Instant::now() - Duration::from_millis(7);
        let _dispatch = app.frame_dispatch_scope();
        let counted = app.begin_window_handler(child);
        assert!(app.close_child_window(child));
        app.note_window_handler(child, started, counted);
    });
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    let final_line = text
        .lines()
        .find(|line| line.contains("window=child-") && line.contains("final=1"))
        .unwrap_or_else(|| panic!("no final=1 line for the child:\n{text}"));
    let sum_us: u64 = final_line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("handler_sum_us="))
        .unwrap_or_else(|| panic!("final line without handler time: {final_line}"))
        .parse()
        .unwrap();
    assert!(sum_us >= 7_000, "{final_line}");
}

/// `text` with CRLF line ends turned into LF, the form every scan reads.
pub(super) fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// `text` as a Windows checkout holds it, with CRLF line ends.
pub(super) fn to_crlf(text: &str) -> String {
    to_lf(text).replace('\n', "\r\n")
}

/// The text of `source` from `start` up to the first `end` after it.
pub(super) fn source_span(source: &str, start: &str, end: &str) -> Option<String> {
    let source = to_lf(source);
    let begin = source.find(start)?;
    let length = source[begin..].find(end)?;
    Some(source[begin..begin + length].to_owned())
}

/// Each `ApplicationHandler` method in `module`, as `(name, body)` in source order.
pub(super) fn handler_methods(module: &str) -> Vec<(String, String)> {
    let Some(body) = source_span(module, "impl ApplicationHandler<UserEvent> for App {", "\n}\n")
    else {
        return Vec::new();
    };
    body.split("\n    fn ")
        .skip(1)
        .map(|method| (method[..method.find('(').unwrap_or(0)].to_owned(), method.to_owned()))
        .collect()
}

/// Each direct `.request_redraw()` call in `sources`, as `file:line`, comments ignored.
pub(super) fn direct_redraw_requests(sources: &[(String, String)]) -> Vec<String> {
    let mut found = Vec::new();
    for (file, text) in sources {
        let code = without_line_comments(&to_lf(text));
        for (offset, _) in code.match_indices(".request_redraw()") {
            found.push(format!("{file}:{}", code[..offset].matches('\n').count() + 1));
        }
    }
    found
}

#[test]
pub(super) fn source_scans_read_a_crlf_checkout_as_they_read_an_lf_one() {
    // Windows CI checks sources out with CRLF line ends. Each scan, fed a CRLF copy of its real
    // input, must reach the answer it reaches on the LF copy; every scan that differs is listed.
    let mut differs = Vec::new();
    let module = concat!(
        include_str!("mod.rs"),
        "\n",
        include_str!("application_handler.rs"),
        "\n",
        include_str!("user_event.rs")
    );
    assert_eq!(handler_methods(&to_lf(module)).len(), 6);
    if handler_methods(&to_crlf(module)) != handler_methods(&to_lf(module)) {
        differs.push("handler_methods".to_owned());
    }
    for (source, start, end) in [
        (
            concat!(
                include_str!("mod.rs"),
                "\n",
                include_str!("application_handler.rs"),
                "\n",
                include_str!("user_event.rs")
            ),
            "fn insert_window_registered(",
            "\n    }\n",
        ),
        (
            concat!(
                include_str!("mod.rs"),
                "\n",
                include_str!("application_handler.rs"),
                "\n",
                include_str!("user_event.rs")
            ),
            "    fn window_event(",
            "\n    }\n",
        ),
        (include_str!("frame_counters.rs"), "pub(super) fn emit_frame_lines_at(", "\n    }\n"),
        (include_str!("frame_counters.rs"), "pub(crate) fn request_native_redraw(", "\n}\n"),
    ] {
        let expected = source_span(&to_lf(source), start, end);
        assert!(expected.is_some(), "{start}");
        if source_span(&to_crlf(source), start, end) != expected {
            differs.push(format!("source_span {start}"));
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    app_sources(&root, &root, &mut sources);
    let as_lf: Vec<_> = sources.iter().map(|(file, text)| (file.clone(), to_lf(text))).collect();
    let as_crlf: Vec<_> =
        sources.iter().map(|(file, text)| (file.clone(), to_crlf(text))).collect();
    let audit = |sources: &[(String, String)]| {
        parser_lock_audit(sources, NON_PARSER_LOCKS, PANE_PARSER_LOCKS, WORKER_LOCK_SECTIONS)
    };
    if audit(&as_crlf) != audit(&as_lf) {
        differs.push(format!("parser_lock_audit: {:#?}", audit(&as_crlf)));
    }
    if direct_redraw_requests(&as_crlf) != direct_redraw_requests(&as_lf) {
        differs.push("direct_redraw_requests".to_owned());
    }
    assert!(differs.is_empty(), "{differs:#?}");
}

/// A retiring or exiting window's renderer finalizes its statistics before the App copies them, so
/// growth episodes the renderer would only settle in `Drop` reach the final line and the closed
/// totals.
#[test]
pub(super) fn retirement_and_exit_finalize_renderer_statistics_before_reading_them() {
    let source = to_lf(include_str!("frame_counters.rs"));
    for (name, end) in
        [("fn retire_window_counters(", "\n    }\n"), ("fn finish_frame_lines(", "\n    }\n")]
    {
        let body = source_span(&source, name, end).expect(name);
        let finalize = body.find("finalize_frame_stats()").unwrap_or_else(|| panic!("{name}"));
        let read = body.find("GpuRenderer::frame_stats").expect("the statistics read");
        assert!(finalize < read, "{name} finalizes before it reads");
    }
}

/// On Windows, a real GDI renderer whose glyph atlas grows and whose device then stops between
/// frames reports, in the closed-window totals, every growth once, one abandoned episode and no
/// growth-to-present sample. The growth comes from the App's own redraw; the stop goes through
/// `begin_window_redraw`'s stopped path, which assembles nothing; the totals come from the
/// window's retirement, so they are read before the renderer drops.
#[cfg(windows)]
#[test]
pub(super) fn a_device_stopped_between_frames_abandons_its_growth_in_the_closed_totals() {
    use crate::app::pty_test_support::isolated;
    use winit::{
        application::ApplicationHandler,
        event_loop::{ActiveEventLoop, EventLoop},
        platform::windows::EventLoopBuilderExtWindows,
    };
    if isolated() {
        return;
    }
    struct Probe {
        outcome: Option<Result<(), String>>,
    }
    impl ApplicationHandler<crate::app::UserEvent> for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            self.outcome = Some(run_stopped_growth(event_loop));
            event_loop.exit();
        }
        fn window_event(
            &mut self,
            _: &ActiveEventLoop,
            _: winit::window::WindowId,
            _: winit::event::WindowEvent,
        ) {
        }
    }
    let event_loop = EventLoop::<crate::app::UserEvent>::with_user_event()
        .with_any_thread(true)
        .build()
        .unwrap();
    let mut probe = Probe { outcome: None };
    event_loop.run_app(&mut probe).unwrap();
    probe.outcome.expect("resumed runs").unwrap_or_else(|error| panic!("{error}"));
}

/// The body of the stopped-growth case, inside the one event loop its isolated process runs.
#[cfg(windows)]
pub(super) fn run_stopped_growth(
    event_loop: &winit::event_loop::ActiveEventLoop,
) -> Result<(), String> {
    use sonicterm_cfg::config::{ScrollbarMode, SoftwareRenderMode};
    use sonicterm_cfg::theme::Theme;
    use sonicterm_gpu::core::{GlyphAtlasStart, GpuRenderer, RendererSettings, SurfaceAppearance};
    use sonicterm_gpu::device_errors::GpuFaultKind;
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::{dpi::PhysicalSize, window::Window};

    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().map_err(|_| "the counter gate turns on")?;
    let pane = app.__test_seed_tab("growth");
    // Distinct printable characters on several rows: at 160 px they outgrow the 256 floor at once.
    let text: String =
        ('!'..='~').collect::<Vec<_>>().chunks(12).fold(String::new(), |mut rows, chunk| {
            rows.extend(chunk);
            rows.push_str("\r\n");
            rows
        });
    if !app.__test_advance_pane_parser(pane, text.as_bytes()) {
        return Err("the pane parser takes the text".into());
    }
    let id = app.__test_main_window_id().ok_or("no main window")?;
    let window = std::sync::Arc::new(
        event_loop
            .create_window(
                Window::default_attributes()
                    .with_visible(true)
                    .with_inner_size(PhysicalSize::new(1000, 700)),
            )
            .map_err(|error| error.to_string())?,
    );
    let font_dirs = [std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../sonicterm-font/test-fonts/rec-mono")];
    let settings = RendererSettings {
        font_family: "Rec Mono St.Helens",
        font_dirs: &font_dirs,
        font_size: 160.0,
        line_height_mult: 1.0,
        font_weight_scale: 1.0,
        subpixel_aa: Default::default(),
        padding: [0.0; 4],
        appearance: SurfaceAppearance {
            backdrop: Default::default(),
            opacity: 1.0,
            scrollbar: ScrollbarMode::Never,
            panel_padding: 0.0,
            software_render_mode: SoftwareRenderMode::Force,
        },
        role: "stopped-growth-test",
        glyph_atlas_start: GlyphAtlasStart::Minimum,
    };
    let mut renderer = GpuRenderer::new(window.clone(), event_loop, &Theme::default(), settings)
        .map_err(|error| error.to_string())?;
    // Registration turns counting on; the test attach does not register, so it is set here.
    renderer.set_frame_counting(true);
    if !app.__test_attach_window_renderer(id, window, renderer) {
        return Err("the renderer attaches".into());
    }
    let growths = |app: &mut App| {
        app.__test_window_renderer_mut(id)
            .map_or(0, |renderer| renderer.glyph_atlas_facts().growths)
    };
    // One real redraw at a time, until one grows the atlas; that frame retries and presents nothing.
    for _ in 0..4 {
        app.__test_set_window_last_render(id, Instant::now() - Duration::from_secs(1));
        ApplicationHandler::window_event(&mut app, event_loop, id, WindowEvent::RedrawRequested);
        if growths(&mut app) > 0 {
            break;
        }
    }
    let grown = growths(&mut app);
    let renderer = app.__test_window_renderer_mut(id).ok_or("the renderer")?;
    let pending = renderer.frame_stats();
    if grown == 0 || pending.atlas_growth_to_present_buckets.iter().sum::<u64>() != 0 {
        return Err(format!("precondition: a growth with no present yet, got {grown} growths"));
    }
    renderer.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    if renderer.device_accepts_gpu_work() {
        return Err("the device stopped".into());
    }
    if app.begin_window_redraw(id, Instant::now()) {
        return Err("a stopped window starts no frame".into());
    }
    let mut removed = app.windows.remove(&id).ok_or("the window is tracked")?;
    app.retire_window_counters(id, &mut removed);
    let closed = app.frame_counters_snapshot().ok_or("counting app")?.closed_windows;
    let reported = (
        closed.count("glyph_atlas_growths"),
        closed.count("atlas_growth_abandoned"),
        closed.histogram_count("atlas_growth_to_present").unwrap_or(0),
    );
    if reported != (Some(grown), Some(1), 0) {
        return Err(format!("closed totals {reported:?}, want ({grown} growths, 1 abandoned, 0)"));
    }
    Ok(())
}
