//! Pins `wait_for`'s stream source: text a program overwrote with `\r` still matches in stream
//! mode and not on screen, a pattern spanning many publications matches, text printed before
//! the wait began does not, an overflowing tap says its context was truncated, and the tap is
//! disarmed when the wait ends.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;

use super::*;
use crate::app::mcp::McpShared;
use crate::app::App;

/// A call admitted now on `shared`, for a connection that stays open.
fn scope(shared: &McpShared) -> CallScope {
    CallScope::admit(shared, &Arc::new(AtomicBool::new(false)))
}

/// A stream wait for `pattern`, bounded by `timeout`.
fn stream(pattern: &str, timeout: Duration) -> StreamCondition {
    StreamCondition {
        regex: Regex::new(pattern).unwrap(),
        watch_exit: false,
        quiet: None,
        timeout,
        recheck: Duration::from_millis(20),
    }
}

/// An App with one seeded pane, its MCP entry and its VT handles.
fn pane() -> (App, PaneEntry, crate::app::spawn_pane::PaneVtHandles) {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    let pane_id = app.__test_seed_tab("stream");
    let entry = app.mcp_resolve_pane(pane_id).expect("the seeded pane resolves");
    let handles = crate::app::spawn_pane::PaneVtHandles::from_pane_state(
        app.registry.pane(pane_id).expect("the pane"),
    );
    (app, entry, handles)
}

/// Publish `bytes` through the production VT batch publisher.
fn publish(handles: &crate::app::spawn_pane::PaneVtHandles, bytes: &[u8]) {
    crate::app::spawn_pane::publish_pane_vt_batch_with(
        handles,
        bytes,
        None,
        &mut None,
        &mut crate::app::spawn_pane::SyncLatch::default(),
        |_| None,
        |_| {},
        Instant::now,
        |_| {},
    );
}

/// Run a stream wait on another thread while `write` publishes output, and return its result.
fn wait_while(entry: &PaneEntry, condition: StreamCondition, write: impl FnOnce()) -> Value {
    let shared = McpShared::new(sonicterm_cfg::config::McpControl::Agent, None);
    let entry_for_wait = entry.clone();
    let waiter = std::thread::spawn(move || {
        let scope = scope(&shared);
        wait(&shared, &entry_for_wait, &condition, &scope)
    });
    // The wait arms its tap before any of this output, which the test then publishes.
    wait_for_tap(entry);
    write();
    waiter.join().unwrap()["structuredContent"].clone()
}

/// A progress line rewritten with `\r` matches its first text in stream mode, though the screen
/// shows only the rewrite.
#[test]
fn text_overwritten_on_screen_still_matches_the_stream() {
    let (_app, entry, handles) = pane();
    let result = wait_while(&entry, stream("step 1 of 3", Duration::from_secs(10)), || {
        publish(&handles, b"step 1 of 3\rstep 3 of 3\r\n");
    });
    assert_eq!(result["reason"], "matched", "{result}");
    assert_eq!(result["source"], "stream");
    let screen: String = entry.parser.lock().grid().row(0).iter().map(|cell| cell.ch).collect();
    assert!(!screen.contains("step 1"), "the screen shows only the rewrite: {screen:?}");
    // The hold is released without the lock; the parser frees the ring at its next batch.
    publish(&handles, b"");
    assert!(entry.parser.lock().output_tap_since(0).is_none(), "the wait released its tap");
}

/// A pattern whose two ends arrive 5 KiB apart, in separate publications, matches.
#[test]
fn a_match_spanning_publications_is_found() {
    let (_app, entry, handles) = pane();
    let result = wait_while(&entry, stream("(?s)BEGIN.*END", Duration::from_secs(10)), || {
        publish(&handles, b"BEGIN ");
        for _ in 0..50 {
            publish(&handles, &[b'.'; 100]);
        }
        publish(&handles, b" END\r\n");
    });
    assert_eq!(result["reason"], "matched", "{result}");
    assert_eq!(result["context_truncated"], false);
}

/// Text printed before the wait began is not part of its stream.
#[test]
fn text_from_before_the_wait_does_not_match() {
    let (_app, entry, handles) = pane();
    publish(&handles, b"ready\r\n");
    let result = wait_while(&entry, stream("ready", Duration::from_millis(300)), || {
        publish(&handles, b"other\r\n");
    });
    assert_eq!(result["reason"], "timeout", "{result}");
}

/// More output than the tap holds marks the wait's context as truncated.
#[test]
fn an_overflowing_tap_reports_truncated_context() {
    let (_app, entry, handles) = pane();
    let chunk = vec![b'x'; 8 * 1024];
    let result = wait_while(&entry, stream("never", Duration::from_millis(500)), || {
        for _ in 0..(sonicterm_vt::vt::OUTPUT_TAP_CHARS / chunk.len() + 2) {
            publish(&handles, &chunk);
        }
    });
    assert_eq!(result["reason"], "timeout", "{result}");
    assert_eq!(result["context_truncated"], true);
    assert_eq!(result["output_incomplete"], true);
}

/// An unknown `source` is refused, and `"screen"` or none selects the row scan.
#[test]
fn the_source_argument_is_checked() {
    assert!(!wants_stream(&serde_json::json!({})).unwrap());
    assert!(!wants_stream(&serde_json::json!({ "source": "screen" })).unwrap());
    assert!(wants_stream(&serde_json::json!({ "source": "stream" })).unwrap());
    assert!(wants_stream(&serde_json::json!({ "source": "log" })).is_err());
}

/// A wait whose parser stays busy past its timeout still returns at the timeout: releasing the
/// tap takes no parser lock, so cleanup never waits on the holder. The holder keeps the lock until
/// the waiter has finished, so a cleanup that took it would block until the release instead.
#[test]
fn a_held_parser_does_not_delay_the_end_of_a_wait() {
    let (_app, entry, _handles) = pane();
    let shared = McpShared::new(sonicterm_cfg::config::McpControl::Agent, None);
    let condition = stream("never", Duration::from_millis(200));
    let waiter = {
        let entry = entry.clone();
        std::thread::spawn(move || {
            let scope = scope(&shared);
            let started = Instant::now();
            let result = wait(&shared, &entry, &condition, &scope);
            (started.elapsed(), result)
        })
    };
    // Only after the wait armed its tap does the holder take the parser.
    wait_for_tap(&entry);
    let (held, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
    let holder = {
        let parser = Arc::clone(&entry.parser);
        let (held_tx, release_rx) = (held.0, release.1);
        std::thread::spawn(move || {
            let guard = parser.lock();
            held_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(10)).ok();
            drop(guard);
        })
    };
    held.1.recv().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !waiter.is_finished() {
        assert!(Instant::now() < deadline, "the wait did not end while the parser was held");
        std::thread::sleep(Duration::from_millis(5));
    }
    release.0.send(()).unwrap();
    holder.join().unwrap();
    let (elapsed, result) = waiter.join().unwrap();
    assert!(elapsed < Duration::from_secs(1), "the wait ended at its timeout: {elapsed:?}");
    let result = &result["structuredContent"];
    assert_eq!(result["reason"], "timeout", "{result}");
}

/// Output announced while the parser was busy is never reported as `quiet`, the timeout that ends
/// such a wait says its output is incomplete, and a passed quiet deadline does not shorten the
/// retries: each one still sleeps the poll interval. The batch is announced with the parser
/// already held, so the wait cannot have read it.
#[test]
fn unread_output_holds_off_quiet_and_marks_the_result_incomplete() {
    let (_app, entry, _handles) = pane();
    let mut condition = stream("never", Duration::from_millis(600));
    condition.quiet = Some(Duration::from_millis(50));
    // A long recheck, so only the poll interval paces the retries.
    condition.recheck = Duration::from_secs(1);
    let shared = McpShared::new(sonicterm_cfg::config::McpControl::Agent, None);
    let entry_for_wait = entry.clone();
    let waiter = std::thread::spawn(move || {
        let scope = scope(&shared);
        let result = wait(&shared, &entry_for_wait, &condition, &scope);
        (result, PASSES.with(std::cell::Cell::get))
    });
    wait_for_tap(&entry);
    let guard = entry.parser.lock();
    crate::app::spawn_pane::publish_grid_change(&entry.output_generation, &entry.change_signal);
    let (result, passes) = waiter.join().unwrap();
    drop(guard);
    let result = &result["structuredContent"];
    assert_eq!(result["reason"], "timeout", "unread output is not quiet: {result}");
    assert_eq!(result["output_incomplete"], true);
    // 600 ms of 5 ms lock attempts and 50 ms sleeps is about a dozen passes; a spin is a hundred.
    assert!(passes <= 20, "{passes} passes: the retries did not sleep");
}

/// Block until a wait on `entry` armed its tap.
fn wait_for_tap(entry: &PaneEntry) {
    let armed_by = Instant::now() + Duration::from_secs(5);
    while entry.parser.lock().output_tap_since(0).is_none() {
        assert!(Instant::now() < armed_by, "the wait armed its tap");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A closed pane whose output was all read ends the wait at once with `pane_closed`, instead of
/// rescanning its ring until the close drain runs out.
#[test]
fn a_closed_pane_ends_the_wait_without_spinning() {
    let (_app, entry, handles) = pane();
    let mut closed_at = None;
    let result = wait_while(&entry, stream("never", Duration::from_secs(10)), || {
        publish(&handles, b"last words\r\n");
        entry.change_signal.close();
        closed_at = Some(Instant::now());
    });
    let after_close = closed_at.unwrap().elapsed();
    assert_eq!(result["reason"], "pane_closed", "{result}");
    // Well inside the one-second close drain: the wait ended on its read, not on the drain.
    assert!(after_close < Duration::from_millis(500), "ended {after_close:?} after the close");
    assert_eq!(result["output_incomplete"], false);
}
