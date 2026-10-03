//! Pins the frame-counter core: histogram bounds, the line cadence, `lock_parser`'s gate and
//! dispatch scope, the VT section record and the flush slot.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::*;
use crate::app::App;

/// `start` plus `micros` microseconds.
fn after_us(start: Instant, micros: u64) -> Instant {
    start + Duration::from_micros(micros)
}

#[test]
fn histogram_reports_exact_sums_and_bucket_bounds_never_exact_maxima() {
    // A maximum or p95 is only known to lie in a bucket, so it prints as `_le` or `_gt`.
    let mut frames = Histogram::new(HistogramUnit::Millis);
    frames.record_us(3_000);
    frames.record_us(5_500);
    assert_eq!(frames.count(), 2);
    assert_eq!(frames.sum_us(), 8_500);
    assert_eq!(frames.max(), Some(Bound::AtMost(7)));
    assert_eq!(frames.p95(), Some(Bound::AtMost(7)));
    frames.record_us(120_000);
    assert_eq!(frames.max(), Some(Bound::Above(100)));
    let fields = frames.line_fields("present_interval");
    assert!(fields.contains("present_interval_sum_us=128500"), "{fields}");
    assert!(fields.contains("present_interval_max_gt_ms=100"), "{fields}");
    assert!(fields.contains("present_interval_p95_gt_ms=100"), "{fields}");
    // A value equal to a bound belongs to that bound's bucket.
    let mut waits = Histogram::new(HistogramUnit::Micros);
    waits.record_us(10);
    assert_eq!(waits.max(), Some(Bound::AtMost(10)));
    assert!(waits.line_fields("ui_parser_wait").contains("ui_parser_wait_max_le_us=10"));
}

#[test]
fn an_empty_interval_prints_no_fields() {
    // Lines omit zero fields, so a histogram with no events contributes nothing.
    let frames = Histogram::new(HistogramUnit::Millis);
    assert_eq!(frames.line_fields("handler"), "");
    assert_eq!((frames.max(), frames.p95()), (None, None));
}

#[test]
fn interval_bounds_come_only_from_buckets_that_grew() {
    // Counters are cumulative and never reset; a reader takes deltas from its own snapshot.
    let mut frames = Histogram::new(HistogramUnit::Millis);
    frames.record_us(150_000);
    let earlier = frames.clone();
    frames.record_us(3_000);
    let interval = frames.delta_since(&earlier);
    assert_eq!(interval.count(), 1);
    assert_eq!(interval.sum_us(), 3_000);
    assert_eq!(interval.max(), Some(Bound::AtMost(4)));
}

#[test]
fn atomic_histogram_snapshot_matches_its_records() {
    // VT workers record into shared atomics; a snapshot reads the same counts and sum.
    let shared = AtomicHistogram::new(HistogramUnit::Micros);
    shared.record_us(40);
    shared.record_us(6_000);
    let snapshot = shared.snapshot();
    assert_eq!((snapshot.count(), snapshot.sum_us()), (2, 6_040));
    assert_eq!(snapshot.max(), Some(Bound::Above(5_000)));
}

#[test]
fn a_source_prints_at_most_one_line_a_second_and_one_final_line() {
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

#[test]
fn lock_parser_outside_a_counting_dispatch_is_exactly_lock() {
    // Gate off, or no dispatch at all: no clock read and no tally write.
    let parser = Mutex::new(7_u32);
    let mut clock_reads = 0_u32;
    let mut clock = || {
        clock_reads += 1;
        Instant::now()
    };
    assert_eq!(*lock_counted(&parser, &mut clock), 7);
    {
        let _scope = DispatchScope::enter(None);
        assert_eq!(*lock_counted(&parser, &mut clock), 7);
    }
    assert_eq!(clock_reads, 0);
    assert_eq!(current_tally(), (0, 0));
}

#[test]
fn a_counting_dispatch_times_each_lock_and_drains_into_its_app() {
    // Each event-loop lock of a pane parser adds one wait to the App that opened the dispatch.
    let parser = Mutex::new(0_u32);
    let totals = Arc::new(DispatchTotals::default());
    let start = Instant::now();
    let mut ticks =
        [after_us(start, 0), after_us(start, 3), after_us(start, 10), after_us(start, 30)]
            .into_iter();
    let mut clock = || ticks.next().expect("four clock reads");
    {
        let _scope = DispatchScope::enter(Some(Arc::clone(&totals)));
        drop(lock_counted(&parser, &mut clock));
        drop(lock_counted(&parser, &mut clock));
        assert_eq!(totals.locks.load(Ordering::Relaxed), 0, "drained only at dispatch end");
    }
    let waits = totals.wait.snapshot();
    assert_eq!((totals.locks.load(Ordering::Relaxed), waits.count(), waits.sum_us()), (2, 2, 23));
    assert_eq!(current_tally(), (0, 0), "the thread-local is empty after the drain");
}

#[test]
fn nested_and_unwound_scopes_restore_the_outer_dispatch() {
    // A scope is a guard: an inner App's waits go to it, and a panic still drains and restores.
    let parser = Mutex::new(0_u32);
    let outer = Arc::new(DispatchTotals::default());
    let inner = Arc::new(DispatchTotals::default());
    {
        let _outer_scope = DispatchScope::enter(Some(Arc::clone(&outer)));
        {
            let _inner_scope = DispatchScope::enter(Some(Arc::clone(&inner)));
            drop(lock_parser(&parser));
        }
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _gate_off_scope = DispatchScope::enter(None);
            drop(lock_parser(&parser));
            panic!("handler unwinds");
        }));
        assert!(unwound.is_err());
        drop(lock_parser(&parser));
    }
    assert_eq!(inner.locks.load(Ordering::Relaxed), 1);
    assert_eq!(outer.locks.load(Ordering::Relaxed), 1, "the gate-off scope counted nothing");
}

#[test]
fn vt_section_derives_wait_parse_and_hold_from_its_four_instants() {
    // Wait is lock minus before, parse is parsed minus locked, hold is released minus locked.
    let stats = VtFrameStats::default();
    let start = Instant::now();
    let times = VtSectionTimes {
        before_lock: start,
        locked_at: after_us(start, 20),
        parsed_at: after_us(start, 520),
        released_at: after_us(start, 530),
    };
    stats.record_section(&times, 4_096);
    let wait = stats.parser_lock_wait.snapshot();
    let parse = stats.parse.snapshot();
    let hold = stats.parser_lock_hold.snapshot();
    assert_eq!((wait.sum_us(), parse.sum_us(), hold.sum_us()), (20, 500, 510));
    assert_eq!(stats.parse_bytes.load(Ordering::Relaxed), 4_096);
}

#[test]
fn coalesced_flushes_give_one_observation_and_count_the_coalescing() {
    // The oldest pending flush is kept; a later one before the redraw only counts as coalesced.
    let stats = VtFrameStats::default();
    let slot = AtomicU64::new(0);
    publish_flush(&slot, 1_000, &stats);
    publish_flush(&slot, 2_000, &stats);
    assert_eq!(consume_flush(&slot, 5_000), Some(4_000));
    assert_eq!(consume_flush(&slot, 6_000), None, "consumed once");
    assert_eq!(stats.flushes.load(Ordering::Relaxed), 2);
    assert_eq!(stats.flushes_coalesced.load(Ordering::Relaxed), 1);
}

#[test]
fn an_untargeted_flush_stores_nothing() {
    // With no redraw target there is no window to consume a timestamp.
    let stats = VtFrameStats::default();
    note_untargeted_flush(&stats);
    assert_eq!(stats.flushes.load(Ordering::Relaxed), 1);
    assert_eq!(stats.flushes_untargeted.load(Ordering::Relaxed), 1);
}

#[test]
fn racing_publication_and_consumption_neither_lose_nor_double_count() {
    // Every publish is either observed once by a swap or counted as coalesced, never both.
    let stats = Arc::new(VtFrameStats::default());
    let slot = Arc::new(AtomicU64::new(0));
    let publishes = 20_000_u64;
    let producer = {
        let (stats, slot) = (Arc::clone(&stats), Arc::clone(&slot));
        std::thread::spawn(move || {
            for index in 1..=publishes {
                publish_flush(&slot, index, &stats);
            }
        })
    };
    let mut observed = 0_u64;
    while !producer.is_finished() {
        observed += u64::from(consume_flush(&slot, u64::MAX).is_some());
    }
    producer.join().unwrap();
    observed += u64::from(consume_flush(&slot, u64::MAX).is_some());
    assert_eq!(observed + stats.flushes_coalesced.load(Ordering::Relaxed), publishes);
}

/// A new App under `filter`, so its gate does not depend on any process-wide subscriber.
fn app_under_filter(filter: &str) -> crate::app::App {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
    let subscriber = Registry::default().with(EnvFilter::new(filter));
    sonicterm_logging::test_capture::with_default(subscriber, || {
        crate::app::App::new(Theme::default(), Config::default(), Keymap::default())
    })
}

#[test]
fn an_apps_gate_follows_the_frame_counters_target_when_it_is_built() {
    // Debug logging turns the counters on; the default level leaves them off.
    assert!(app_under_filter("warn,frame_counters=debug").frame_counters.is_some());
    assert!(app_under_filter("warn").frame_counters.is_none());
}

#[test]
fn forcing_the_gate_succeeds_only_before_the_first_window_or_pane() {
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
fn two_apps_keep_independent_gates_and_each_pane_shares_its_apps_statistics() {
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
fn both_pane_creators_attach_counters_before_starting_the_worker() {
    // The worker clones its handles from the pane, so they must be attached first.
    for (name, source) in [
        ("spawn_pane.rs", include_str!("spawn_pane.rs")),
        ("child_tabs.rs", include_str!("child_tabs.rs")),
    ] {
        let attach = source
            .find("frame_counters = self.pane_frame_counters();")
            .unwrap_or_else(|| panic!("{name} attaches no counters"));
        assert!(source[attach..].contains("spawn_pane_workers("), "{name} starts its worker first");
    }
}

#[test]
fn a_counting_apps_dispatch_scope_counts_each_parser_lock() {
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
fn every_application_handler_method_opens_a_dispatch_scope() {
    // A dispatch without a scope would lock parsers uncounted.
    let module = include_str!("mod.rs");
    let start = module.find("impl ApplicationHandler<UserEvent> for App {").expect("handler impl");
    let body = &module[start..start + module[start..].find("\n}\n").expect("impl end")];
    let methods = body.matches("\n    fn ").count();
    assert_eq!(methods, 6, "the handler methods changed; review this test");
    assert_eq!(body.matches("let _dispatch = self.frame_dispatch_scope();").count(), methods);
}

/// Every non-test source under `dir` (recursively), as `(path relative to root, text)`.
fn app_sources(root: &std::path::Path, dir: &std::path::Path, found: &mut Vec<(String, String)>) {
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
            found.push((relative, std::fs::read_to_string(&entry_path).unwrap()));
        }
    }
}

/// `text` with each line comment blanked to spaces, so commented-out code never counts; byte
/// offsets and line numbers are unchanged. A line with a quote before `//` is kept whole.
fn without_line_comments(text: &str) -> String {
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
fn function_range(text: &str, name: &str) -> Option<std::ops::Range<usize>> {
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
fn plain_locks(text: &str, exempt: &[std::ops::Range<usize>]) -> Vec<(usize, String)> {
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
fn parser_lock_audit(
    sources: &[(String, String)],
    non_parser: &[(&str, &str, usize)],
    pane_parser: &[(&str, usize)],
    exempt: &[(&str, &str)],
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen: std::collections::BTreeMap<(String, String), Vec<usize>> = Default::default();
    for (file, text) in sources {
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
const NON_PARSER_LOCKS: &[(&str, &str, usize)] = &[
    ("menubar_bridge.rs", "proxy_slot()", 2),
    ("menubar_bridge.rs", "queue()", 2),
    ("open_script_bridge.rs", "proxy_slot()", 2),
    ("open_script_bridge.rs", "queue()", 2),
    ("os_drag_bridge.rs", "proxy_slot()", 2),
    ("os_drag_bridge.rs", "tab_queue()", 2),
    ("os_drag_bridge.rs", "file_queue()", 2),
    ("os_drag.rs", "inner", 2),
    ("app/reaper_driver.rs", "abandoned", 3),
    ("app/reaper_driver.rs", "state", 2),
    ("app/reaper_driver.rs", "observations", 2),
    ("app/reaper_driver.rs", "paths", 3),
    ("app/window_setup/windows.rs", "brushes", 1),
    ("app/media.rs", "charge", 3),
    ("app/command_events.rs", "command_events", 1),
    ("app/os_drag.rs", "snapshots", 6),
    ("app/os_drag.rs", "moved", 2),
    ("app/os_drag.rs", "ended", 3),
    ("app/pty_test_support.rs", "output", 3),
    ("app/redraw_target.rs", "redraw_target", 1),
    ("app/session.rs", "redraw_target", 1),
    ("app/tab_state.rs", "redraw_target", 1),
    ("app/tear_out.rs", "redraw_target", 1),
    ("app/path_target.rs", "queue", 1),
    ("app/input_dispatch.rs", "test_pty_writes", 1),
    ("app/frame_counters.rs", "native", 8),
    ("bin/pty_multi_round_helper.rs", "stdin", 1),
    ("bin/pty_multi_round_helper.rs", "stdout", 1),
];

/// Every pane-parser lock the event-loop thread takes, by file; each goes through lock_parser.
const PANE_PARSER_LOCKS: &[(&str, usize)] = &[
    ("app/child_tabs.rs", 1),
    ("app/child_window.rs", 2),
    ("app/child_window_pointer.rs", 4),
    ("app/config_apply.rs", 2),
    ("app/keymap_dispatch/explicit_source.rs", 1),
    ("app/misc.rs", 5),
    ("app/overlays.rs", 2),
    ("app/pane_refresh.rs", 3),
    ("app/pane_state.rs", 1),
    ("app/scroll.rs", 1),
    ("app/search_handle.rs", 2),
    ("app/spawn_pane.rs", 1),
    ("app/viewport_anchor.rs", 1),
    ("app/window_keyboard.rs", 2),
    ("app/window_pointer.rs", 3),
];

/// The only functions allowed a plain parser lock: the VT worker's own section, which runs off
/// the event-loop thread, and the counting helper that lock_parser wraps.
const WORKER_LOCK_SECTIONS: &[(&str, &str)] = &[
    ("app/spawn_pane.rs", "process_pane_vt_batch_with"),
    ("app/frame_counters.rs", "lock_counted"),
];

#[test]
fn event_loop_pane_parser_locks_go_through_lock_parser() {
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
fn the_parser_lock_audit_reports_each_unconverted_site() {
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

#[test]
fn the_winning_deferral_rule_is_counted_and_later_predicates_never_run() {
    // The streaming check allocates and does Acquire loads, so it must not run after a winner.
    let later = std::cell::Cell::new(0_u32);
    let bump = || {
        later.set(later.get() + 1);
        true
    };
    assert_eq!(defer_rule(|| true, bump, bump), Some(DeferRule::Timeout));
    assert_eq!(defer_rule(|| false, || true, bump), Some(DeferRule::Contention));
    assert_eq!(later.get(), 0, "no predicate ran after the winner");
    assert_eq!(defer_rule(|| false, || false, || true), Some(DeferRule::Streaming));
    assert_eq!(defer_rule(|| false, || false, || false), None);
}

#[test]
fn settlements_count_each_outcome_and_the_present_interval() {
    // Every FrameSettlement has its own counter; only presented frames time an interval.
    use crate::app::redraw::{FrameSettlement, RedrawCause};
    use sonicterm_gpu::core::SurfaceRetryReason;
    let start = Instant::now();
    let mut counters = WindowFrameCounters::default();
    for outcome in [
        FrameSettlement::Presented,
        FrameSettlement::Cached,
        FrameSettlement::Settled,
        FrameSettlement::Retry(RedrawCause::Input),
        FrameSettlement::SurfaceRetry(SurfaceRetryReason::Timeout),
        FrameSettlement::Stopped(3),
        FrameSettlement::Failed,
    ] {
        counters.record_settlement(outcome, start);
    }
    counters.record_settlement(FrameSettlement::Presented, after_us(start, 16_000));
    let outcomes = (
        counters.presented,
        counters.cached,
        counters.settled,
        counters.retry,
        counters.surface_retry,
        counters.stopped,
        counters.failed,
    );
    assert_eq!(outcomes, (2, 1, 1, 1, 1, 1, 1));
    let interval = &counters.present_interval;
    assert_eq!((interval.count(), interval.sum_us()), (1, 16_000));
    counters.note_defer(DeferRule::Streaming);
    counters.note_contention(false);
    assert_eq!((counters.defer_streaming, counters.contention_parser), (1, 1));
}

#[test]
fn a_counting_apps_windows_count_outcomes_and_contention_on_their_paths() {
    // Only a counting App's windows carry counters; the shared paths feed them.
    use crate::app::redraw::FrameSettlement;
    use crate::app::visible_frame::FrameUnavailable;
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    let now = Instant::now();
    let window = app.windows.get_mut(&main).unwrap();
    let snapshot = window.redraw.snapshot();
    window.redraw.settle(snapshot, FrameSettlement::Presented, now);
    let contended = FrameUnavailable::Contended { pane_id: 1, images: true };
    app.visible_frame_unavailable(main, contended, false, now);
    let counters = app.windows[&main].redraw.frame_counters.as_deref().expect("counting window");
    let counted = (
        counters.presented,
        counters.contention_images,
        counters.contention_parser,
        counters.contention_retry_armed,
    );
    assert_eq!(counted, (1, 1, 0, 1));
    let mut silent = app_under_filter("warn");
    silent.__test_synthetic_main();
    let silent_main = silent.main_window_id.unwrap();
    assert!(silent.windows[&silent_main].redraw.frame_counters.is_none());
}

#[test]
fn wakes_and_dispatch_stalls_count_by_cause_and_kind() {
    // Each StartCause has its own wake counter; each dispatch kind its own duration histogram.
    use winit::event::StartCause;
    let start = Instant::now();
    let mut counters = AppFrameCounters::new();
    for cause in [
        StartCause::Init,
        StartCause::Poll,
        StartCause::WaitCancelled { start, requested_resume: None },
        StartCause::ResumeTimeReached { start, requested_resume: start },
        StartCause::ResumeTimeReached { start, requested_resume: start },
    ] {
        counters.note_wake(&cause);
    }
    let wakes = (
        counters.wake_init,
        counters.wake_poll,
        counters.wake_wait_cancelled,
        counters.wake_resume_time,
    );
    assert_eq!(wakes, (1, 1, 1, 2));
    counters.record_dispatch(DispatchKind::AboutToWait, 2_000);
    counters.record_dispatch(DispatchKind::UserEvent, 30_000);
    counters.record_dispatch(DispatchKind::NewEvents, 500);
    let sums = (
        counters.about_to_wait.sum_us(),
        counters.user_event.sum_us(),
        counters.new_events.sum_us(),
    );
    assert_eq!(sums, (2_000, 30_000, 500));
}

#[test]
fn probes_are_timed_once_per_call_and_an_empty_batch_is_not_counted() {
    // A single and a batch probe count once each; an empty batch makes no snapshot and no count.
    let totals = Arc::new(DispatchTotals::default());
    let start = Instant::now();
    let mut ticks =
        [after_us(start, 0), after_us(start, 40), after_us(start, 100), after_us(start, 300)]
            .into_iter();
    let mut clock = || ticks.next().expect("two reads per counted probe");
    let mut probes = 0_u32;
    {
        let _scope = DispatchScope::enter(Some(Arc::clone(&totals)));
        time_probe_with(1, &mut clock, || probes += 1);
        time_probe_with(3, &mut clock, || probes += 1);
        time_probe_with(0, &mut clock, || probes += 1);
    }
    assert_eq!(probes, 3, "every probe still runs");
    let counted =
        (totals.probe_calls.load(Ordering::Relaxed), totals.probe_panes.load(Ordering::Relaxed));
    assert_eq!(counted, (2, 4));
    assert_eq!(totals.probe.snapshot().sum_us(), 240);
}

#[test]
fn a_redraw_takes_each_pending_flush_once_into_flush_to_redraw() {
    // Oldest pending flush to the first redraw that takes it; a second redraw finds nothing,
    // and an empty slot is no observation.
    let slots = [AtomicU64::new(1_000_000), AtomicU64::new(0)];
    let mut counters = WindowFrameCounters::default();
    for now_ns in [5_000_000, 9_000_000] {
        for slot in &slots {
            counters.take_flush(slot, now_ns);
        }
    }
    let flushes = &counters.flush_to_redraw;
    assert_eq!((flushes.count(), flushes.sum_us()), (1, 4_000));
}

#[test]
fn a_counting_apps_window_counts_requests_redraws_and_handler_time() {
    // The App's hooks reach the window's counters; with the gate off no clock is read.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    app.note_user_request_redraw(main);
    app.note_redraw_requested(main);
    app.note_window_handler(main, Instant::now() - Duration::from_millis(5), true);
    app.request_owner_redraw(main, crate::app::redraw::RedrawCause::Output);
    let counters = app.windows[&main].redraw.frame_counters.as_deref().expect("counting window");
    assert_eq!(
        (counters.user_request_redraw, counters.redraw_requested, counters.handler.count()),
        (1, 1, 1)
    );
    // The synthetic window has no native window, so no request reached winit and none counts.
    let snapshot = app.frame_counters_snapshot().expect("counting app");
    assert_eq!(snapshot.windows[0].1.count("native_request_redraw"), Some(0));
    assert!(app_under_filter("warn").frame_clock_start().is_none());
}

#[test]
fn a_source_line_prints_deltas_at_most_once_a_second_then_one_final_line() {
    // Lines carry span_ms and only nonzero deltas; maintenance wakes never print, and nothing
    // follows final=1.
    let start = Instant::now();
    let mut line = LineState::new(start);
    let mut record = CounterRecord::default();
    record.push_count("presented", 3);
    record.push_count("failed", 0);
    let first =
        line.line("main", &record, start + Duration::from_millis(200), true).expect("first");
    assert!(first.starts_with("window=main span_ms=200 "), "{first}");
    assert!(first.contains("presented=3") && !first.contains("failed"), "{first}");
    assert!(line.line("main", &record, start + Duration::from_millis(700), true).is_none());
    let mut later = CounterRecord::default();
    later.push_count("presented", 5);
    later.push_count("failed", 0);
    let three_s = start + Duration::from_secs(3);
    assert!(line.line("main", &later, three_s, false).is_none(), "a maintenance wake");
    let second = line.line("main", &later, three_s, true).expect("second");
    assert!(second.contains("presented=2"), "{second}");
    assert!(line.final_line("main", &later, three_s).is_none(), "nothing pending");
    let mut closing = LineState::new(start);
    let closed =
        closing.final_line("child-2", &later, start + Duration::from_millis(50)).expect("final");
    assert!(closed.starts_with("window=child-2 final=1 span_ms=50 "), "{closed}");
    assert!(closing.line("child-2", &later, three_s, true).is_none(), "nothing after final=1");
}

#[test]
fn counter_records_merge_by_name_and_take_deltas() {
    // Closed windows merge into one fixed-size record; lines print deltas from the last one.
    let mut waits = Histogram::new(HistogramUnit::Millis);
    waits.record_us(3_000);
    let mut first = CounterRecord::default();
    first.push_count("presented", 2);
    first.push_histogram("handler", waits.clone());
    let mut second = CounterRecord::default();
    second.push_count("presented", 5);
    second.push_count("failed", 1);
    second.push_histogram("handler", waits);
    let mut closed = CounterRecord::default();
    closed.merge(&first);
    closed.merge(&second);
    assert_eq!((closed.count("presented"), closed.count("failed")), (Some(7), Some(1)));
    let handler = (closed.histogram_count("handler"), closed.histogram_sum_us("handler"));
    assert_eq!(handler, (Some(2), Some(6_000)));
    let delta = second.delta_since(&first);
    assert_eq!((delta.count("presented"), delta.histogram_count("handler")), (Some(3), Some(0)));
}

#[test]
fn a_closed_windows_totals_move_to_closed_windows_and_late_vt_counts_still_appear() {
    // The snapshot's sum never drops when a window closes, and VT counts are app-wide.
    use crate::app::redraw::FrameSettlement;
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    let window = app.windows.get_mut(&main).unwrap();
    let snapshot = window.redraw.snapshot();
    window.redraw.settle(snapshot, FrameSettlement::Presented, Instant::now());
    let pane = app.pane_frame_counters().expect("gate on");
    let open = app.frame_counters_snapshot().expect("counting app");
    assert_eq!(open.windows.len(), 1);
    assert_eq!(open.windows[0].1.count("presented"), Some(1));
    let mut removed = app.windows.remove(&main).unwrap();
    app.retire_window_counters(main, &mut removed);
    pane.vt.note_batch();
    let closed = app.frame_counters_snapshot().expect("counting app");
    assert!(closed.windows.is_empty());
    assert_eq!(closed.closed_windows.count("presented"), Some(1));
    assert_eq!(closed.app.count("batches"), Some(1), "a late increment from a closed pane");
    assert!(app_under_filter("warn").frame_counters_snapshot().is_none());
}

#[test]
fn only_authorizing_dispatches_print_lines_and_exit_prints_the_final_ones() {
    // Maintenance wakes count but never print, and no timer is armed for a line.
    let module = include_str!("mod.rs");
    let start = module.find("impl ApplicationHandler<UserEvent> for App {").expect("handler impl");
    let body = &module[start..start + module[start..].find("\n}\n").expect("impl end")];
    for method in body.split("\n    fn ").skip(1) {
        let name = &method[..method.find('(').expect("signature")];
        let prints = method.contains("emit_frame_lines(");
        assert_eq!(prints, matches!(name, "user_event" | "window_event"), "{name}");
        assert_eq!(method.contains("finish_frame_lines()"), name == "exiting", "{name}");
    }
}

#[test]
fn registration_turns_on_each_counting_windows_renderer() {
    // Every renderer is attached before its window registers, so registration sets its flag.
    let module = include_str!("mod.rs");
    let start = module.find("fn insert_window_registered(").expect("registration");
    let body = &module[start..start + module[start..].find("\n    }\n").expect("end")];
    assert!(body.contains("renderer.set_frame_counting(true)"), "{body}");
}

#[test]
fn every_window_removal_retires_its_counters() {
    // A removed window's totals move to closed_windows before it drops.
    for (name, source) in [
        ("child_window.rs", include_str!("child_window.rs")),
        ("child_tabs.rs", include_str!("child_tabs.rs")),
        ("session.rs", include_str!("session.rs")),
    ] {
        let removals = source.matches("self.windows.remove(").count();
        assert_eq!(source.matches("self.retire_window_counters(").count(), removals, "{name}");
    }
}

#[test]
fn readiness_is_known_before_any_record_is_built() {
    // Building a record allocates, so a dispatch checks readiness first and builds nothing for
    // a frame that cannot print a line.
    let start = Instant::now();
    let mut cadence = LineCadence::default();
    assert!(cadence.ready(start));
    assert!(cadence.line_due(start, true));
    assert!(!cadence.ready(start + Duration::from_millis(500)), "printed within a second");
    assert!(cadence.ready(start + Duration::from_secs(2)));
    cadence.final_line(false);
    assert!(!cadence.ready(start + Duration::from_secs(9)), "closed");
}

#[test]
fn line_records_are_built_only_when_a_line_could_print() {
    // A RedrawRequested arrives every frame; the record is built at most once a second per source.
    let source = include_str!("frame_counters.rs");
    let start = source.find("pub(super) fn emit_frame_lines_at(").expect("emit_frame_lines_at");
    let body = &source[start..start + source[start..].find("\n    }\n").expect("end")];
    let window_ready = body.find("counters.line.ready(now)").expect("window readiness check");
    assert!(window_ready < body.find("counters.record(").expect("window record"));
    let app_ready = body.find("app.line.ready(now)").expect("app readiness check");
    assert!(app_ready < body.find("app.record()").expect("app record"));
}

#[test]
fn refused_redraws_after_the_line_interval_build_no_record() {
    // A RedrawRequested arrives every frame; once the second has passed, a redraw with no new
    // attempt or flush is refused before anything is built, however many arrive.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    let start = Instant::now();
    let counters = |app: &App| -> u64 {
        app.windows[&main].redraw.frame_counters.as_deref().expect("counting").records_built
    };
    app.windows.get_mut(&main).unwrap().redraw.frame_counters.as_deref_mut().unwrap().attempts += 1;
    app.emit_frame_lines_at(Some((main, true)), start);
    assert_eq!(counters(&app), 1, "the attempt authorized one line");
    for second in 2..6 {
        app.emit_frame_lines_at(Some((main, true)), start + Duration::from_secs(second));
    }
    assert_eq!(counters(&app), 1, "refused redraws after expiry built no record");
}

#[test]
fn a_first_interval_with_no_activity_authorizes_no_redraw_line() {
    // A field that was never printed and is still zero has not grown.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    app.emit_frame_lines_at(Some((main, true)), Instant::now() + Duration::from_secs(2));
    let counters = app.windows[&main].redraw.frame_counters.as_deref().expect("counting");
    assert_eq!(counters.records_built, 0, "a zero-activity redraw built a record");
}

#[test]
fn the_gate_seal_is_plain_event_loop_state() {
    // Pane creation runs with the gate off too, so sealing must not be an atomic store.
    let app = app_under_filter("warn");
    let seal = std::any::type_name_of_val(&app.frame_counters_sealed);
    assert_eq!(seal, std::any::type_name::<std::cell::Cell<bool>>());
}

#[test]
fn with_the_gate_off_the_production_hooks_write_nothing() {
    // The hooks the event loop calls on every dispatch read no clock, attach no counters and
    // write no tally when the App's gate is off; only the seal is set.
    let mut app = app_under_filter("warn");
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    {
        let _dispatch = app.frame_dispatch_scope();
        assert!(app.frame_clock_start().is_none(), "no clock read");
        app.note_frame_wake(&winit::event::StartCause::Poll);
        app.note_frame_user_wake();
        app.note_frame_dispatch(DispatchKind::AboutToWait, None);
        app.note_redraw_requested(main);
        app.note_user_request_redraw(main);
        assert!(!app.begin_window_handler(main));
        app.note_window_handler(main, Instant::now(), false);
        let parser = Mutex::new(0_u8);
        drop(lock_parser(&parser));
        app.emit_frame_lines(Some((main, true)));
    }
    assert_eq!(current_tally(), (0, 0), "no tally written");
    assert!(app.pane_frame_counters().is_none());
    assert!(app.windows[&main].redraw.frame_counters.is_none());
    assert!(app.windows[&main].panes.values().all(|pane| pane.frame_counters.is_none()));
    assert!(app.frame_counters_snapshot().is_none());
    assert_eq!(app.force_frame_counters_on(), Err(FrameCountersTooLate), "sealed");
}

#[test]
fn a_tab_shows_its_leaves_or_only_its_zoomed_pane() {
    // The walk visits in place, allocating nothing, and zoom hides every other leaf.
    use sonicterm_cfg::keymap::Direction;
    use sonicterm_ui::pane::PaneTree;
    let shown = |tree: &PaneTree| {
        let mut visited = Vec::new();
        for_each_shown_pane(tree, &mut |pane_id| visited.push(pane_id));
        visited
    };
    let mut tree = PaneTree::leaf(1);
    assert_eq!(shown(&tree), vec![1]);
    assert!(tree.split(1, Direction::Right, 2));
    assert!(tree.split(2, Direction::Down, 3));
    assert_eq!(shown(&tree), vec![1, 2, 3]);
    assert!(tree.toggle_zoom(2));
    assert_eq!(shown(&tree), vec![2]);
}

/// Give every pane of window `id` counters, as pane creation does with the gate on.
fn attach_pane_counters(app: &mut App, id: winit::window::WindowId) {
    let pane_ids: Vec<u64> = app.windows[&id].panes.keys().copied().collect();
    for pane_id in pane_ids {
        let counters = app.pane_frame_counters().expect("gate on");
        app.windows.get_mut(&id).unwrap().panes.get_mut(&pane_id).unwrap().frame_counters =
            Some(counters);
    }
}

/// The pending-flush slot of pane `pane_id` in window `id`.
fn flush_slot(app: &App, id: winit::window::WindowId, pane_id: u64) -> &AtomicU64 {
    let pane = &app.windows[&id].panes[&pane_id];
    pane.frame_counters.as_ref().expect("counting pane").pending_flush.as_ref()
}

#[test]
fn a_background_tabs_flush_waits_until_its_tab_is_shown() {
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
fn a_pane_hidden_by_zoom_keeps_its_flush() {
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
fn a_closing_childs_handler_time_reaches_closed_windows() {
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
fn window_event_decides_its_destination_before_dispatching() {
    // The window may be gone when the handler returns, so whether it counted is read first.
    let module = include_str!("mod.rs");
    let start = module.find("    fn window_event(").expect("window_event");
    let body = &module[start..start + module[start..].find("\n    }\n").expect("end")];
    let decided = body.find("self.begin_window_handler(win_id)").expect("destination read");
    assert!(decided < body.find("self.do_window_event(").expect("dispatch"), "{body}");
    assert!(body.contains("self.note_window_handler(win_id, started, counted)"), "{body}");
}

#[test]
fn native_redraw_requests_count_per_window_inside_a_counting_dispatch() {
    // Requests reach winit from raw window handles across the App, so each is tallied by window
    // id in the open dispatch and attributed when the dispatch drains; none counts outside one.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    let child = app.__test_seed_child_window(&["only"]);
    let main = app.main_window_id.expect("synthetic main");
    note_native_request(main);
    {
        let _dispatch = app.frame_dispatch_scope();
        note_native_request(main);
        note_native_request(main);
        note_native_request(child);
    }
    let snapshot = app.frame_counters_snapshot().expect("counting app");
    let count = |id| {
        let record =
            snapshot.windows.iter().find(|(window, _)| *window == id).map(|entry| &entry.1);
        record.and_then(|record| record.count("native_request_redraw"))
    };
    assert_eq!((count(main), count(child)), (Some(2), Some(1)));
    // A window closed by the dispatch that requested its redraw keeps that request too.
    {
        let _dispatch = app.frame_dispatch_scope();
        note_native_request(child);
        assert!(app.close_child_window(child));
    }
    let closed = app.frame_counters_snapshot().expect("counting app").closed_windows;
    assert_eq!(closed.count("native_request_redraw"), Some(2));
    let after = app.frame_counters_snapshot().expect("counting app");
    assert_eq!(after.windows.len(), 1, "nothing stale is attributed to the closed child");
}

#[test]
fn with_the_gate_off_a_native_request_writes_nothing() {
    // A non-counting App's dispatch opens no tally, so the request only reaches winit.
    let mut app = app_under_filter("warn");
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    {
        let _dispatch = app.frame_dispatch_scope();
        note_native_request(main);
        assert_eq!(pending_native_requests(), 0);
    }
    assert!(app.frame_counters_snapshot().is_none());
}

#[test]
fn every_app_redraw_request_goes_through_the_counting_helper() {
    // A direct winit request_redraw() outside request_native_redraw would go uncounted.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    app_sources(&root, &root, &mut sources);
    let mut found = Vec::new();
    for (file, text) in &sources {
        let code = without_line_comments(text);
        for (offset, _) in code.match_indices(".request_redraw()") {
            found.push(format!("{file}:{}", code[..offset].matches('\n').count() + 1));
        }
    }
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].starts_with("app/frame_counters.rs:"), "{found:#?}");
    let source = include_str!("frame_counters.rs");
    let start = source.find("pub(crate) fn request_native_redraw(").expect("counting helper");
    let body = &source[start..start + source[start..].find("\n}\n").expect("end")];
    assert!(body.contains("note_native_request(window.id())"), "{body}");
    assert!(body.contains("window.request_redraw()"), "{body}");
}

/// A log sink shared between a test and the subscriber it installs.
#[derive(Clone, Default)]
struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

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
fn a_closing_childs_final_line_carries_its_closing_handler_time() {
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

#[test]
fn repeated_requesting_dispatches_reuse_the_tally_storage() {
    // After warm-up a dispatch opens with request storage already in place, so its first
    // native request allocates nothing, cycle after cycle.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    for _ in 0..3 {
        let _dispatch = app.frame_dispatch_scope();
        note_native_request(main);
    }
    for _ in 0..100 {
        let _dispatch = app.frame_dispatch_scope();
        let before = pending_native_capacity();
        assert!(before >= 1, "the dispatch opened with no storage, so its request allocates");
        note_native_request(main);
        assert_eq!(pending_native_capacity(), before, "the request grew the storage");
    }
}

#[test]
fn requests_for_unregistered_windows_share_one_bounded_counter() {
    // Ids that never register keep no entry: their requests land in one app-wide count, so
    // storage stays fixed however many distinct ids are abandoned, in one dispatch or many.
    let mut app = app_under_filter("warn");
    app.force_frame_counters_on().unwrap();
    app.__test_synthetic_main();
    let main = app.main_window_id.expect("synthetic main");
    let abandoned = |index: u64| winit::window::WindowId::from(u64::MAX / 2 + index);
    for round in 0..50 {
        let _dispatch = app.frame_dispatch_scope();
        for offset in 0..4 {
            note_native_request(abandoned(round * 4 + offset));
        }
        note_native_request(main);
    }
    {
        let _dispatch = app.frame_dispatch_scope();
        for index in 1_000..1_100 {
            note_native_request(abandoned(index));
        }
        assert!(
            pending_native_capacity() <= NATIVE_TALLY_CAPACITY,
            "one dispatch grew past the cap"
        );
    }
    let dispatch = &app.frame_counters.as_ref().expect("counting").dispatch;
    assert_eq!(dispatch.registered_entries(), 1, "only the registered main keeps an entry");
    let snapshot = app.frame_counters_snapshot().expect("counting app");
    assert_eq!(snapshot.app.count("native_request_redraw_unregistered"), Some(300));
    let main_record = &snapshot.windows.iter().find(|(id, _)| *id == main).expect("main").1;
    assert_eq!(main_record.count("native_request_redraw"), Some(50));
}

#[test]
fn histogram_buckets_export_the_used_slots_and_the_exact_sum() {
    // The harness writes each histogram as unit, bounds, used bucket counts (overflow last)
    // and the exact microsecond sum; a count or a missing name has no histogram.
    let mut handler = Histogram::new(HistogramUnit::Millis);
    handler.record_us(3_000);
    handler.record_us(150_000);
    let mut wait = Histogram::new(HistogramUnit::Micros);
    wait.record_us(10);
    wait.record_us(6_000);
    let mut record = CounterRecord::default();
    record.push_histogram("handler", handler);
    record.push_histogram("ui_parser_wait", wait);
    record.push_count("attempts", 1);
    let millis = record.histogram_buckets("handler").expect("ms histogram");
    assert_eq!((millis.unit, millis.bounds, millis.sum_us), ("ms", &MILLIS_BOUNDS[..], 153_000));
    assert_eq!(millis.counts, &[1, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    let micros = record.histogram_buckets("ui_parser_wait").expect("us histogram");
    assert_eq!((micros.unit, micros.bounds, micros.sum_us), ("us", &MICROS_BOUNDS[..], 6_010));
    assert_eq!(micros.counts, &[1, 0, 0, 0, 0, 0, 1]);
    assert!(record.histogram_buckets("attempts").is_none());
    assert!(record.histogram_buckets("missing").is_none());
}

#[test]
fn renderer_work_counters_join_the_window_record_with_assembly_in_us_buckets() {
    // The five renderer work counters reach the window's line, snapshot and closed totals;
    // assembly is a microsecond histogram with the App's own bounds and an exact sum.
    use sonicterm_gpu::frame_stats::{FrameStats, ASSEMBLY_BOUNDS_US};
    assert_eq!(ASSEMBLY_BOUNDS_US, MICROS_BOUNDS);
    let mut stats = FrameStats::ZERO;
    stats.full_frames = 2;
    stats.row_cache_invalidate_visits = 40;
    stats.row_cache_invalidate_us = 900;
    stats.recolor_glyphs_visited = 120;
    stats.assembly_buckets[2] = 1;
    stats.assembly_buckets[6] = 1;
    stats.assembly_sum_us = 6_080;
    let record = WindowFrameCounters::default().record(Some(stats), 0);
    for (name, value) in [
        ("full_frames", 2),
        ("row_cache_invalidate_visits", 40),
        ("row_cache_invalidate_us", 900),
        ("recolor_glyphs_visited", 120),
    ] {
        assert_eq!(record.count(name), Some(value), "{name}");
    }
    let assembly = record.histogram_buckets("assembly").expect("assembly histogram");
    assert_eq!(
        (assembly.unit, assembly.bounds, assembly.sum_us),
        ("us", &MICROS_BOUNDS[..], 6_080)
    );
    assert_eq!(assembly.counts, &[0, 0, 1, 0, 0, 0, 1]);
    let fields = record.line_fields();
    assert!(
        fields.contains("full_frames=2") && fields.contains("assembly_us=[0,0,1,0,0,0,1]"),
        "{fields}"
    );
}
