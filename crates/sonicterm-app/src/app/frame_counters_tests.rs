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
    let module = to_lf(include_str!("mod.rs"));
    assert_eq!(handler_methods(&module).len(), 6, "the handler methods changed; review this test");
    assert_eq!(handlers_without_one_dispatch_scope(&module), Vec::<String>::new());
}

/// Each handler method whose body does not open exactly one dispatch scope, with its count.
/// A second scope in one handler takes the spare tally and allocates another per dispatch, so
/// two is as wrong as none.
fn handlers_without_one_dispatch_scope(module: &str) -> Vec<String> {
    handler_methods(module)
        .into_iter()
        .filter_map(|(name, body)| {
            let scope_count = body.matches("let _dispatch = self.frame_dispatch_scope();").count();
            (scope_count != 1).then(|| format!("{name}: {scope_count} scopes"))
        })
        .collect()
}

#[test]
fn the_dispatch_scope_check_rejects_a_missing_or_a_second_scope() {
    // The real handler impl, edited two ways: one handler opens a second scope, another opens none.
    // The check must name both, so neither a duplicate nor a gap can pass.
    let module = to_lf(include_str!("mod.rs"));
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
            found.push((relative, to_lf(&std::fs::read_to_string(&entry_path).unwrap())));
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
const NON_PARSER_LOCKS: &[(&str, &str, usize)] = &[
    ("menubar_bridge.rs", "proxy_slot()", 2),
    ("menubar_bridge.rs", "queue()", 2),
    ("open_script_bridge.rs", "proxy_slot()", 2),
    ("open_script_bridge.rs", "queue()", 2),
    ("os_drag_bridge.rs", "proxy_slot()", 2),
    ("os_drag_bridge.rs", "tab_queue()", 2),
    ("os_drag_bridge.rs", "file_queue()", 2),
    ("os_drag.rs", "inner", 2),
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
    ("bin/pty_multi_round_helper.rs", "stdin", 1),
    ("bin/pty_multi_round_helper.rs", "stdout", 1),
];

/// Every pane-parser lock the event-loop thread takes, by file; each goes through lock_parser.
const PANE_PARSER_LOCKS: &[(&str, usize)] = &[
    ("app/child_tabs.rs", 1),
    ("app/child_window.rs", 2),
    ("app/child_window_pointer.rs", 1),
    ("app/config_apply.rs", 2),
    ("app/keymap_dispatch/explicit_source.rs", 1),
    ("app/misc.rs", 5),
    ("app/overlays.rs", 2),
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

/// The rules are ordered Timeout > Contention > Sync > Streaming, and no predicate after the
/// winner runs: the sync and streaming checks do Acquire loads, and streaming allocates.
#[test]
fn the_winning_deferral_rule_is_counted_and_later_predicates_never_run() {
    let later = std::cell::Cell::new(0_u32);
    let bump = || {
        later.set(later.get() + 1);
        true
    };
    assert_eq!(defer_rule(|| true, bump, bump, bump), Some(DeferRule::Timeout));
    assert_eq!(defer_rule(|| false, || true, bump, bump), Some(DeferRule::Contention));
    assert_eq!(defer_rule(|| false, || false, || true, bump), Some(DeferRule::Sync));
    assert_eq!(later.get(), 0, "no predicate ran after the winner");
    assert_eq!(defer_rule(|| false, || false, || false, || true), Some(DeferRule::Streaming));
    assert_eq!(defer_rule(|| false, || false, || false, || false), None);
}

/// `defer_sync` counts the Sync rule and joins the window record by name between
/// `defer_contention` and `defer_streaming`; a window that never held reports it as zero.
#[test]
fn defer_sync_joins_the_window_record_between_contention_and_streaming() {
    assert_eq!(WindowFrameCounters::default().record(None, 0).count("defer_sync"), Some(0));
    let mut counters = WindowFrameCounters {
        defer_contention: 1,
        defer_streaming: 2,
        ..WindowFrameCounters::default()
    };
    counters.note_defer(DeferRule::Sync);
    counters.note_defer(DeferRule::Sync);
    let record = counters.record(None, 0);
    assert_eq!(record.count("defer_sync"), Some(2));
    let fields = record.line_fields();
    let order: Vec<usize> = ["defer_contention=1", "defer_sync=2", "defer_streaming=2"]
        .iter()
        .map(|name| fields.find(name).unwrap_or_else(|| panic!("{name} in {fields}")))
        .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{fields}");
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
    let methods = handler_methods(include_str!("mod.rs"));
    assert_eq!(methods.len(), 6, "the handler methods changed; review this test");
    for (name, method) in &methods {
        let prints = method.contains("emit_frame_lines(");
        assert_eq!(prints, matches!(name.as_str(), "user_event" | "window_event"), "{name}");
        assert_eq!(method.contains("finish_frame_lines()"), name == "exiting", "{name}");
    }
}

#[test]
fn registration_turns_on_each_counting_windows_renderer() {
    // Every renderer is attached before its window registers, so registration sets its flag.
    let body = source_span(include_str!("mod.rs"), "fn insert_window_registered(", "\n    }\n")
        .expect("registration");
    assert!(body.contains("renderer.set_frame_counting(true)"), "{body}");
}

#[test]
fn every_window_removal_retires_its_counters() {
    // A removed window's totals move to closed_windows before it drops.
    for (name, source) in [
        ("child_window.rs", to_lf(include_str!("child_window.rs"))),
        ("child_tabs.rs", to_lf(include_str!("child_tabs.rs"))),
        ("session.rs", to_lf(include_str!("session.rs"))),
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
    let body = source_span(source, "pub(super) fn emit_frame_lines_at(", "\n    }\n")
        .expect("emit_frame_lines_at");
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
    let body = source_span(include_str!("mod.rs"), "    fn window_event(", "\n    }\n")
        .expect("window_event");
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
    let found = direct_redraw_requests(&sources);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].starts_with("app/frame_counters.rs:"), "{found:#?}");
    let source = include_str!("frame_counters.rs");
    let body = source_span(source, "pub(crate) fn request_native_redraw(", "\n}\n")
        .expect("counting helper");
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
    // The renderer work counters, glyph atlas growth counts included, reach the window's line,
    // snapshot and closed totals; assembly is a microsecond histogram and growth-to-present a
    // millisecond one, each with the App's own bounds and an exact sum.
    use sonicterm_gpu::frame_stats::{FrameStats, ASSEMBLY_BOUNDS_US};
    assert_eq!(ASSEMBLY_BOUNDS_US, MICROS_BOUNDS);
    let mut stats = FrameStats::ZERO;
    stats.full_frames = 2;
    stats.row_cache_invalidate_visits = 40;
    stats.row_cache_invalidate_us = 900;
    stats.recolor_glyphs_visited = 120;
    stats.font_fallback_applies = 3;
    stats.assembly_buckets[2] = 1;
    stats.assembly_buckets[6] = 1;
    stats.assembly_sum_us = 6_080;
    stats.glyph_atlas_growths = 2;
    stats.atlas_growth_abandoned = 1;
    stats.atlas_growth_to_present_buckets[1] = 1;
    stats.atlas_growth_to_present_buckets[9] = 1;
    stats.atlas_growth_to_present_sum_us = 130_000;
    let record = WindowFrameCounters::default().record(Some(stats), 0);
    for (name, value) in [
        ("full_frames", 2),
        ("row_cache_invalidate_visits", 40),
        ("row_cache_invalidate_us", 900),
        ("recolor_glyphs_visited", 120),
        ("font_fallback_applies", 3),
        ("glyph_atlas_growths", 2),
        ("atlas_growth_abandoned", 1),
    ] {
        assert_eq!(record.count(name), Some(value), "{name}");
    }
    // Growth-to-present time is a millisecond histogram on the App's frame bounds.
    use sonicterm_gpu::frame_stats::GROWTH_TO_PRESENT_BOUNDS_MS;
    assert_eq!(GROWTH_TO_PRESENT_BOUNDS_MS, MILLIS_BOUNDS);
    let growth = record.histogram_buckets("atlas_growth_to_present").expect("growth histogram");
    assert_eq!((growth.unit, growth.bounds, growth.sum_us), ("ms", &MILLIS_BOUNDS[..], 130_000));
    assert_eq!(growth.counts, &[0, 1, 0, 0, 0, 0, 0, 0, 0, 1]);
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

#[test]
fn attempt_and_preparation_counters_leave_the_renderer_each_under_its_own_name() {
    // Every attempt, apply-attempt and preparation field reaches the window record with its own
    // value, so a field wired to the wrong source, or left out, shows as a mismatch here.
    use sonicterm_gpu::frame_stats::{AttemptStats, FrameStats};
    let attempt = |base: u64| AttemptStats {
        attempts: base,
        presented: base + 1,
        attempt_ns: base + 2,
        shape_ns: base + 3,
        raster_ns: base + 4,
        shape_requests: base + 5,
        raster_calls: base + 6,
        raster_tiles: base + 7,
    };
    let mut stats = FrameStats::ZERO;
    stats.shape_ns = 1;
    stats.raster_ns = 2;
    stats.raster_calls = 3;
    stats.raster_tiles = 4;
    stats.font_generation_applies = 5;
    stats.font_prepare_ns = 6;
    stats.font_generation_prepare_ns = 7;
    stats.attempts = attempt(10);
    stats.apply_attempts = attempt(20);
    let record = WindowFrameCounters::default().record(Some(stats), 0);
    let mut expected = vec![
        ("shape_ns", 1),
        ("raster_ns", 2),
        ("raster_calls", 3),
        ("raster_tiles", 4),
        ("font_generation_applies", 5),
        ("font_prepare_ns", 6),
        ("font_generation_prepare_ns", 7),
    ];
    for (prefix, base) in [("render_", 10), ("apply_", 20)] {
        for (offset, suffix) in [
            "attempts",
            "attempts_presented",
            "attempt_ns",
            "attempt_shape_ns",
            "attempt_raster_ns",
            "attempt_shape_requests",
            "attempt_raster_calls",
            "attempt_raster_tiles",
        ]
        .into_iter()
        .enumerate()
        {
            let name: &'static str = Box::leak(format!("{prefix}{suffix}").into_boxed_str());
            expected.push((name, base + offset as u64));
        }
    }
    for (name, value) in expected {
        assert_eq!(record.count(name), Some(value), "{name}");
    }
}

/// `text` with CRLF line ends turned into LF, the form every scan reads.
fn to_lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// `text` as a Windows checkout holds it, with CRLF line ends.
fn to_crlf(text: &str) -> String {
    to_lf(text).replace('\n', "\r\n")
}

/// The text of `source` from `start` up to the first `end` after it.
fn source_span(source: &str, start: &str, end: &str) -> Option<String> {
    let source = to_lf(source);
    let begin = source.find(start)?;
    let length = source[begin..].find(end)?;
    Some(source[begin..begin + length].to_owned())
}

/// Each `ApplicationHandler` method in `module`, as `(name, body)` in source order.
fn handler_methods(module: &str) -> Vec<(String, String)> {
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
fn direct_redraw_requests(sources: &[(String, String)]) -> Vec<String> {
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
fn source_scans_read_a_crlf_checkout_as_they_read_an_lf_one() {
    // Windows CI checks sources out with CRLF line ends. Each scan, fed a CRLF copy of its real
    // input, must reach the answer it reaches on the LF copy; every scan that differs is listed.
    let mut differs = Vec::new();
    let module = include_str!("mod.rs");
    assert_eq!(handler_methods(&to_lf(module)).len(), 6);
    if handler_methods(&to_crlf(module)) != handler_methods(&to_lf(module)) {
        differs.push("handler_methods".to_owned());
    }
    for (source, start, end) in [
        (include_str!("mod.rs"), "fn insert_window_registered(", "\n    }\n"),
        (include_str!("mod.rs"), "    fn window_event(", "\n    }\n"),
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
fn retirement_and_exit_finalize_renderer_statistics_before_reading_them() {
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
fn a_device_stopped_between_frames_abandons_its_growth_in_the_closed_totals() {
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
fn run_stopped_growth(event_loop: &winit::event_loop::ActiveEventLoop) -> Result<(), String> {
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
    let font_dirs =
        [std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts")];
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

#[test]
fn stream_clock_exempt_joins_the_window_record_after_defer_streaming() {
    // The harness and perf-compare read the new window count by name, in the record's order.
    // A line omits zero counts, so the neighbours are non-zero to show the order.
    let counters = WindowFrameCounters {
        defer_streaming: 2,
        stream_clock_exempt: 4,
        contention_retry_armed: 1,
        ..WindowFrameCounters::default()
    };
    let record = counters.record(None, 0);
    assert_eq!(record.count("stream_clock_exempt"), Some(4));
    let fields = record.line_fields();
    let defer = fields.find("defer_streaming=").expect("defer_streaming field");
    let exempt = fields.find("stream_clock_exempt=4").expect("stream_clock_exempt field");
    let armed = fields.find("contention_retry_armed=").expect("contention_retry_armed field");
    assert!(defer < exempt && exempt < armed, "{fields}");
}

/// The three display-link counts join the window record by name, after `stream_clock_exempt` and
/// before `contention_retry_armed`, so the harness and perf-compare read them in contract order. A
/// line omits zero counts, so every neighbour is non-zero to show the order.
#[test]
fn display_link_counts_join_the_window_record_after_stream_clock_exempt() {
    let counters = WindowFrameCounters {
        stream_clock_exempt: 1,
        display_link_ticks: 5,
        display_link_admissions: 3,
        display_link_fallbacks: 2,
        contention_retry_armed: 1,
        ..WindowFrameCounters::default()
    };
    let record = counters.record(None, 0);
    assert_eq!(record.count("display_link_ticks"), Some(5));
    assert_eq!(record.count("display_link_admissions"), Some(3));
    assert_eq!(record.count("display_link_fallbacks"), Some(2));
    let fields = record.line_fields();
    let order: Vec<usize> = [
        "stream_clock_exempt=",
        "display_link_ticks=5",
        "display_link_admissions=3",
        "display_link_fallbacks=2",
        "contention_retry_armed=",
    ]
    .iter()
    .map(|name| fields.find(name).unwrap_or_else(|| panic!("{name} in {fields}")))
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{fields}");
    // A window that never ran a link still reports all three, as zero.
    let idle = WindowFrameCounters::default().record(None, 0);
    for name in ["display_link_ticks", "display_link_admissions", "display_link_fallbacks"] {
        assert_eq!(idle.count(name), Some(0), "{name}");
    }
}

#[test]
fn damage_waste_joins_the_renderer_record_after_damaged_frames() {
    // The harness and perf-compare read the union-rect waste count by name, beside its denominator.
    // A line omits zero counts, so the neighbours are non-zero to show the order.
    use sonicterm_gpu::frame_stats::FrameStats;
    let mut stats = FrameStats::ZERO;
    stats.damaged_frames = 3;
    stats.damage_waste_permille_sum = 37;
    stats.software_frames = 1;
    let record = WindowFrameCounters::default().record(Some(stats), 0);
    assert_eq!(record.count("damage_waste_permille_sum"), Some(37));
    let fields = record.line_fields();
    let damaged = fields.find("damaged_frames=3").expect("damaged_frames field");
    let waste =
        fields.find("damage_waste_permille_sum=37").expect("damage_waste_permille_sum field");
    let software = fields.find("software_frames=1").expect("software_frames field");
    assert!(damaged < waste && waste < software, "{fields}");
}

/// `sync_timeouts` joins the App record by name, after `flushes_suppressed` and before
/// `ui_parser_locks`, so the harness and perf-compare read it in contract order; an App whose
/// workers never timed out a synchronized update still reports it, as zero.
#[test]
fn sync_timeouts_joins_the_app_record_after_flushes_suppressed() {
    let counters = AppFrameCounters::new();
    assert_eq!(counters.record().count("sync_timeouts"), Some(0), "supported zero");
    counters.vt.flushes_suppressed.fetch_add(2, Ordering::Relaxed);
    counters.vt.sync_timeouts.fetch_add(3, Ordering::Relaxed);
    counters.dispatch.locks.fetch_add(1, Ordering::Relaxed);
    let record = counters.record();
    assert_eq!(record.count("sync_timeouts"), Some(3));
    let fields = record.line_fields();
    let order: Vec<usize> = ["flushes_suppressed=2", "sync_timeouts=3", "ui_parser_locks=1"]
        .iter()
        .map(|name| fields.find(name).unwrap_or_else(|| panic!("{name} in {fields}")))
        .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{fields}");
}

#[test]
fn partial_counters_join_the_renderer_record_after_full_frames() {
    // The harness and perf-compare read the partial-assembly counters by name, after full_frames.
    // A line omits zero counts, so the neighbours are non-zero to show the order.
    use sonicterm_gpu::frame_stats::FrameStats;
    let mut stats = FrameStats::ZERO;
    stats.full_frames = 2;
    stats.partial_frames = 5;
    stats.partial_fallbacks = 1;
    stats.row_cells_hashed = 400;
    stats.row_cache_invalidate_visits = 3;
    let record = WindowFrameCounters::default().record(Some(stats), 0);
    for (name, value) in
        [("partial_frames", 5), ("partial_fallbacks", 1), ("row_cells_hashed", 400)]
    {
        assert_eq!(record.count(name), Some(value), "{name}");
    }
    let fields = record.line_fields();
    let order: Vec<usize> = [
        "full_frames=2",
        "partial_frames=5",
        "partial_fallbacks=1",
        "row_cells_hashed=400",
        "row_cache_invalidate_visits=3",
    ]
    .iter()
    .map(|field| fields.find(field).unwrap_or_else(|| panic!("{field}: {fields}")))
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{fields}");
}
