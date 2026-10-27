//! Coalesced output publication and synchronized-output hold contracts.

use super::super::spawn_pane_tests::*;
use super::*;

/// A counting worker reads its clock once before `lock()` and three times under the guard, and
/// records wait, parse and hold from those instants once the guard has dropped.
#[test]
fn counting_worker_times_each_parser_section_from_four_clock_reads() {
    use std::sync::atomic::Ordering::Relaxed;
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let (_pane, handles) = counting_worker_handles(&stats);
    let start = Instant::now();
    let mut clock_reads = 0_u64;
    let clock = || {
        clock_reads += 1;
        start + Duration::from_micros(clock_reads * 10)
    };
    process_pane_vt_batch_with(
        &handles,
        b"hello",
        None,
        &mut None,
        &mut SyncLatch::default(),
        |_| None,
        |_| {},
        clock,
        |_| {},
    );
    assert_eq!(clock_reads, 4, "one read before lock() and three under the guard");
    let (wait, parse, hold) = (
        stats.parser_lock_wait.snapshot(),
        stats.parse.snapshot(),
        stats.parser_lock_hold.snapshot(),
    );
    assert_eq!((wait.sum_us(), parse.sum_us(), hold.sum_us()), (10, 10, 20));
    assert_eq!((stats.batches.load(Relaxed), stats.parse_bytes.load(Relaxed)), (1, 5));
}

/// With its App's gate off a worker reads no clock for a plain batch.
#[test]
fn worker_without_counters_reads_no_clock() {
    let (_pane, handles) = pane_and_worker_handles();
    assert!(handles.frame_counters.is_none());
    let mut clock_reads = 0_u32;
    let clock = || {
        clock_reads += 1;
        Instant::now()
    };
    process_pane_vt_batch_with(
        &handles,
        b"hello",
        None,
        &mut None,
        &mut SyncLatch::default(),
        |_| None,
        |_| {},
        clock,
        |_| {},
    );
    assert_eq!(clock_reads, 0);
}

/// A targeted flush is published before its output event is sent, a second one before the
/// redraw only coalesces, an untargeted one stores nothing, and without counters the send is
/// unchanged. The token is cleared between sends, as servicing each event would.
#[test]
fn output_redraw_publishes_the_flush_before_sending() {
    use std::sync::atomic::Ordering::{Acquire, Relaxed};
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let counters = crate::app::frame_counters::PaneFrameCounters::new(Arc::clone(&stats));
    let window_id = WindowId::from(7);
    let target = Mutex::new(Some(window_id));
    let outstanding = AtomicBool::new(false);
    let mut sent = Vec::new();
    send_output_redraw(&target, &outstanding, Some(&counters), Instant::now, |window| {
        assert_ne!(counters.pending_flush.load(Acquire), 0, "published before the send");
        sent.push(window);
        true
    });
    outstanding.store(false, Relaxed);
    send_output_redraw(&target, &outstanding, Some(&counters), Instant::now, |window| {
        sent.push(window);
        true
    });
    assert_eq!((stats.flushes.load(Relaxed), stats.flushes_coalesced.load(Relaxed)), (2, 1));
    assert_ne!(counters.pending_flush.swap(0, Acquire), 0);
    let untargeted = Mutex::new(None::<WindowId>);
    outstanding.store(false, Relaxed);
    send_output_redraw(&untargeted, &outstanding, Some(&counters), Instant::now, |window| {
        sent.push(window);
        true
    });
    assert_eq!(counters.pending_flush.load(Relaxed), 0, "nothing stored without a target");
    assert_eq!(stats.flushes_untargeted.load(Relaxed), 1);
    send_output_redraw(&target, &outstanding, None, Instant::now, |window| {
        sent.push(window);
        true
    });
    assert_eq!(sent, [window_id; 3]);
}

/// N targeted flushes before their event is serviced send one event and count N-1 as
/// suppressed while every flush still counts; the oldest pending timestamp stays; an untargeted
/// flush counts as untargeted, leaves the token alone and is never counted as suppressed.
#[test]
fn outstanding_output_event_coalesces_later_flushes() {
    use std::sync::atomic::Ordering::{Acquire, Relaxed};
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let counters = crate::app::frame_counters::PaneFrameCounters::new(Arc::clone(&stats));
    let window_id = WindowId::from(7);
    let target = Mutex::new(Some(window_id));
    let outstanding = AtomicBool::new(false);
    let mut sent = Vec::new();
    let flush_count = 5_u64;
    let mut first_published = 0;
    for flush in 0..flush_count {
        send_output_redraw(&target, &outstanding, Some(&counters), Instant::now, |window| {
            sent.push(window);
            true
        });
        if flush == 0 {
            first_published = counters.pending_flush.load(Acquire);
        }
    }
    assert_eq!(sent, [window_id], "one event while it is outstanding");
    assert!(outstanding.load(Acquire), "the token stays set until the event is serviced");
    assert_eq!(stats.flushes.load(Relaxed), flush_count);
    assert_eq!(stats.flushes_suppressed.load(Relaxed), flush_count - 1);
    assert_eq!(counters.pending_flush.load(Acquire), first_published, "oldest flush kept");

    let untargeted = Mutex::new(None::<WindowId>);
    send_output_redraw(&untargeted, &outstanding, Some(&counters), Instant::now, |window| {
        sent.push(window);
        true
    });
    assert_eq!(stats.flushes_untargeted.load(Relaxed), 1);
    assert_eq!(
        stats.flushes_suppressed.load(Relaxed),
        flush_count - 1,
        "untargeted is not suppressed"
    );
    assert!(outstanding.load(Acquire), "an untargeted flush leaves the token");
    outstanding.store(false, Relaxed);
    send_output_redraw(&untargeted, &outstanding, Some(&counters), Instant::now, |window| {
        sent.push(window);
        true
    });
    assert!(!outstanding.load(Acquire), "an untargeted flush never sets the token");
    assert_eq!(sent, [window_id]);
}

/// A send the event loop refuses (it has gone) clears the token and is not counted as
/// suppressed, so the next flush sends again; a delivered send leaves the token set.
#[test]
fn failed_output_send_clears_the_token_and_the_next_flush_sends() {
    use std::sync::atomic::Ordering::{Acquire, Relaxed};
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let counters = crate::app::frame_counters::PaneFrameCounters::new(Arc::clone(&stats));
    let window_id = WindowId::from(7);
    let target = Mutex::new(Some(window_id));
    let outstanding = AtomicBool::new(false);
    let mut attempts = Vec::new();
    send_output_redraw(&target, &outstanding, Some(&counters), Instant::now, |window| {
        attempts.push(window);
        false
    });
    assert!(!outstanding.load(Acquire), "a failed send releases the token");
    assert_eq!(stats.flushes_suppressed.load(Relaxed), 0, "a failed send is not suppressed");
    send_output_redraw(&target, &outstanding, Some(&counters), Instant::now, |window| {
        attempts.push(window);
        true
    });
    assert_eq!(attempts, [window_id; 2], "the next flush sends");
    assert!(outstanding.load(Acquire), "a delivered send holds the token");
    send_output_redraw(&target, &outstanding, Some(&counters), Instant::now, |window| {
        attempts.push(window);
        true
    });
    assert_eq!(attempts, [window_id; 2]);
    assert_eq!(stats.flushes_suppressed.load(Relaxed), 1);
}

/// With the App's gate off a worker still coalesces through the token, but reads no flush clock;
/// with it on, every targeted flush reads the clock once, sent or suppressed.
#[test]
fn gate_off_output_events_still_coalesce_without_reading_the_flush_clock() {
    use crate::app::frame_counters::flush_clock_reads;
    let window_id = WindowId::from(7);
    let target = Mutex::new(Some(window_id));
    let outstanding = AtomicBool::new(false);
    let mut sent = Vec::new();
    let before = flush_clock_reads();
    for _ in 0..3 {
        send_output_redraw(&target, &outstanding, None, Instant::now, |window| {
            sent.push(window);
            true
        });
    }
    assert_eq!(sent, [window_id], "the token coalesces with the gate off too");
    assert_eq!(flush_clock_reads(), before, "no clock read with the gate off");

    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let counters = crate::app::frame_counters::PaneFrameCounters::new(Arc::clone(&stats));
    let gate_on = AtomicBool::new(false);
    let before = flush_clock_reads();
    for _ in 0..3 {
        send_output_redraw(&target, &gate_on, Some(&counters), Instant::now, |_| true);
    }
    assert_eq!(flush_clock_reads(), before + 3, "one read per targeted flush with the gate on");
}

/// Under a held parser guard the published word equals the parser's `synchronized_output()`, the
/// reset count is the parser's, and a set word carries the deadline stored with its epoch: the
/// injected clock at the batch that opened the epoch plus the 150 ms bound. A repeated set keeps
/// that deadline, and a new epoch gets a new one.
#[test]
fn sync_word_matches_the_parser_under_its_guard() {
    use crate::app::sync_clock;
    let (_pane, handles) = pane_and_worker_handles();
    let origin = sync_clock::origin();
    let mut clock_ms = 10_u64;
    let mut latch = SyncLatch::default();
    let feed = |bytes: &[u8], at_ms: u64, latch: &mut SyncLatch| {
        let at = origin + Duration::from_millis(at_ms);
        process_pane_vt_batch_with(
            &handles,
            bytes,
            None,
            &mut None,
            latch,
            |_| None,
            |_| {},
            || at,
            |_| {},
        );
    };
    let check = |expected_deadline_ms: Option<u64>| {
        let parser = handles.parser.lock();
        let state = parser.synchronized_output();
        let (word, deadline, resets) = published_sync(&handles);
        assert_eq!(word, sync_word_of(state), "{state:?}");
        assert_eq!(resets, state.resets);
        if let Some(deadline_ms) = expected_deadline_ms {
            let expected = Duration::from_millis(deadline_ms) + SYNC_OUTPUT_TIMEOUT;
            assert_eq!(deadline, Some(origin + expected));
        }
    };
    check(None);
    // A fresh pane holds nothing: its word is clear and its deadline has already passed.
    assert_eq!(published_sync(&handles), (0, Some(origin), 0));

    feed(b"\x1b[?2026hrow", clock_ms, &mut latch);
    check(Some(10));
    clock_ms += 40;
    feed(b"\x1b[?2026hmore", clock_ms, &mut latch);
    check(Some(10));
    clock_ms += 40;
    feed(b"\x1b[?2026l", clock_ms, &mut latch);
    check(None);
    assert!(latch.reset_pending, "the reset is latched for the flush decision");
    clock_ms += 40;
    feed(b"\x1b[?2026h", clock_ms, &mut latch);
    check(Some(130));
    feed(b"\x1bc", clock_ms, &mut latch);
    check(None);
    assert_eq!(published_sync(&handles).2, 2, "RIS is published as a reset");
}

/// A reader that takes the parser guard while a worker thread opens and closes updates always sees
/// the word that matches the parser it holds: both are written under that guard.
#[test]
fn sync_word_never_disagrees_with_a_held_parser_across_threads() {
    let (_pane, handles) = pane_and_worker_handles();
    let worker_handles = handles.clone();
    let worker = std::thread::spawn(move || {
        let mut latch = SyncLatch::default();
        for _ in 0..500 {
            for bytes in [&b"\x1b[?2026hx"[..], b"\x1b[?2026l", b"\x1b[?2026h\x1b[?2026ly"] {
                process_pane_vt_batch_and_publish(
                    &worker_handles,
                    bytes,
                    None,
                    &mut None,
                    &mut latch,
                    None,
                    |_| {},
                );
            }
        }
    });
    let mut checks = 0_u32;
    while !worker.is_finished() || checks == 0 {
        let parser = handles.parser.lock();
        let state = parser.synchronized_output();
        assert_eq!(handles.sync_word.load(Ordering::Acquire), sync_word_of(state));
        assert_eq!(handles.sync_resets.load(Ordering::Relaxed), state.resets);
        checks += 1;
    }
    worker.join().unwrap();
}

/// Without synchronized output the worker's flush decision is the coalescer it has always been:
/// a small batch waits, 128 KiB or 8 ms of pending age flushes, the 3 ms quiet wake flushes once,
/// and disconnect flushes pending output. Separate workers keep each case to one interval flush,
/// which the debug coalescer probe spaces in real time.
#[test]
fn unbracketed_output_flushes_on_size_age_quiet_and_disconnect() {
    let (_pane, handles) = pane_and_worker_handles();
    let start = Instant::now();
    let mut sends = Vec::new();

    let mut by_size = OutputFlush::new(1, &handles);
    assert_eq!(by_size.wait(start), PANE_IDLE_WAIT, "an idle worker parks");
    worker_step(&handles, &mut by_size, b"a", start, &mut sends);
    assert!(sends.is_empty(), "a small fresh batch is coalesced");
    assert_eq!(by_size.wait(start), crate::app::PTY_REDRAW_QUIESCENT);
    let large = vec![b'x'; crate::app::PTY_REDRAW_FLUSH_BYTES];
    worker_step(&handles, &mut by_size, &large, start, &mut sends);
    assert_eq!(sends.len(), 1, "128 KiB pending flushes at once");
    assert_eq!(by_size.wait(start), PANE_IDLE_WAIT);

    let mut by_age = OutputFlush::new(1, &handles);
    worker_step(&handles, &mut by_age, b"a", start, &mut sends);
    worker_step(&handles, &mut by_age, b"b", start + Duration::from_millis(7), &mut sends);
    assert_eq!(sends.len(), 1, "7 ms of pending age is not enough");
    worker_step(&handles, &mut by_age, b"c", start + Duration::from_millis(8), &mut sends);
    assert_eq!(sends.len(), 2, "8 ms of pending age flushes");

    let mut quiet = OutputFlush::new(1, &handles);
    quiet.on_quiet(&handles, start, || sends.push(0));
    assert_eq!(sends.len(), 2, "a quiet wake with nothing pending sends nothing");
    worker_step(&handles, &mut quiet, b"a", start, &mut sends);
    quiet.on_quiet(&handles, start + Duration::from_millis(3), || sends.push(0));
    assert_eq!(sends.len(), 3, "the quiet wake flushes pending output once");

    let mut disconnect = OutputFlush::new(1, &handles);
    disconnect.on_disconnect(|| sends.push(0));
    assert_eq!(sends.len(), 3, "a disconnect with nothing pending sends nothing");
    worker_step(&handles, &mut disconnect, b"a", start, &mut sends);
    disconnect.on_disconnect(|| sends.push(0));
    assert_eq!(sends.len(), 4, "a disconnect flushes pending output");
}

/// While a synchronized update is open the worker keeps parsing but sends nothing, past 128 KiB
/// and past 8 ms of pending age, and waits only until the update's deadline; the reset sends
/// exactly one output event and the worker returns to idle.
#[test]
fn an_open_update_holds_output_until_its_reset() {
    let (_pane, handles) = pane_and_worker_handles();
    let mut flush = OutputFlush::new(1, &handles);
    let mut sends = Vec::new();
    let start = test_base();
    worker_step(&handles, &mut flush, b"\x1b[?2026h", start, &mut sends);
    let deadline = published_deadline(&handles);
    let large = vec![b'x'; crate::app::PTY_REDRAW_FLUSH_BYTES];
    worker_step(&handles, &mut flush, &large, start + Duration::from_millis(1), &mut sends);
    worker_step(&handles, &mut flush, b"row", start + Duration::from_millis(9), &mut sends);
    assert!(sends.is_empty(), "held output sends nothing past the size and age thresholds");
    let at = start + Duration::from_millis(10);
    assert_eq!(flush.wait(at), deadline.saturating_duration_since(at), "wake at the deadline");
    assert!(flush.wait(at) > crate::app::PTY_REDRAW_QUIESCENT);
    flush.on_quiet(&handles, start + Duration::from_millis(12), || sends.push(0));
    assert!(sends.is_empty(), "a quiet wake before the deadline sends nothing");
    worker_step(
        &handles,
        &mut flush,
        b"\x1b[?2026l",
        start + Duration::from_millis(20),
        &mut sends,
    );
    assert_eq!(sends.len(), 1, "the reset sends exactly one output event");
    assert_eq!(flush.wait(start + Duration::from_millis(20)), PANE_IDLE_WAIT);
}

/// A silent stuck update is released at its deadline, not before: one output event, one
/// `sync_timeouts`, and one debug line naming the pane and epoch. Later output in that epoch
/// flushes normally, and a new update holds again.
#[test]
fn a_stuck_update_is_released_once_at_its_deadline() {
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let (_pane, handles) = counting_worker_handles(&stats);
    let mut flush = OutputFlush::new(7, &handles);
    let mut sends = Vec::new();
    let start = test_base();
    worker_step(&handles, &mut flush, b"\x1b[?2026hrow", start, &mut sends);
    let deadline = published_deadline(&handles);
    flush.on_quiet(&handles, deadline - Duration::from_nanos(1), || sends.push(0));
    assert!(sends.is_empty(), "one nanosecond early sends nothing");
    let log = capture_sync_log(|| flush.on_quiet(&handles, deadline, || sends.push(0)));
    assert_eq!(sends.len(), 1, "the deadline releases the update once");
    assert_eq!(stats.sync_timeouts.load(Ordering::Relaxed), 1);
    let released: Vec<_> =
        log.iter().filter(|event| event.target == "sonicterm_app::sync_output").collect();
    assert_eq!(released.len(), 1, "{log:?}");
    assert!(released[0].message.contains("held past 150 ms"), "{:?}", released[0]);
    assert!(released[0].fields.contains(&("pane_id".to_owned(), 7)), "{:?}", released[0]);
    assert!(released[0].fields.contains(&("epoch".to_owned(), 1)), "{:?}", released[0]);
    flush.on_quiet(&handles, deadline + Duration::from_millis(1), || sends.push(0));
    assert_eq!(sends.len(), 1, "a released update is not released twice");

    let after = deadline + Duration::from_millis(1);
    worker_step(&handles, &mut flush, b"more", after, &mut sends);
    assert_eq!(flush.wait(after), crate::app::PTY_REDRAW_QUIESCENT, "the epoch flushes normally");
    flush.on_quiet(&handles, after + Duration::from_millis(3), || sends.push(0));
    assert_eq!(sends.len(), 2);
    assert_eq!(stats.sync_timeouts.load(Ordering::Relaxed), 1);

    // The mode is still set after a timeout, so a new update starts with its reset.
    worker_step(
        &handles,
        &mut flush,
        b"\x1b[?2026l",
        after + Duration::from_millis(10),
        &mut sends,
    );
    assert_eq!(sends.len(), 3, "the late reset flushes");
    let reopened = after + Duration::from_millis(20);
    worker_step(&handles, &mut flush, b"\x1b[?2026hnext", reopened, &mut sends);
    let next_deadline = published_deadline(&handles);
    flush.on_quiet(&handles, next_deadline - Duration::from_millis(1), || sends.push(0));
    assert_eq!(sends.len(), 3, "a new update holds again");
}

/// A pane that closes in the middle of an update still shows its final output: disconnect
/// flushes whatever the mode.
#[test]
fn disconnect_flushes_a_held_update() {
    let (_pane, handles) = pane_and_worker_handles();
    let mut flush = OutputFlush::new(1, &handles);
    let mut sends = Vec::new();
    worker_step(&handles, &mut flush, b"\x1b[?2026hlast", test_base(), &mut sends);
    assert!(sends.is_empty());
    flush.on_disconnect(|| sends.push(0));
    assert_eq!(sends.len(), 1);
}

/// A batch that ends one update and opens the next (`?2026l … ?2026h`) sends exactly one output
/// event, and the next update holds from its own epoch until its reset.
#[test]
fn a_reset_and_set_in_one_batch_flushes_once_and_holds_again() {
    let (_pane, handles) = pane_and_worker_handles();
    let mut flush = OutputFlush::new(1, &handles);
    let mut sends = Vec::new();
    let start = test_base();
    worker_step(&handles, &mut flush, b"\x1b[?2026hA", start, &mut sends);
    worker_step(&handles, &mut flush, b"B\x1b[?2026lC\x1b[?2026hD", start, &mut sends);
    assert_eq!(sends.len(), 1, "the reset inside the batch flushes it");
    assert!(!flush.sync_latch.reset_pending);
    worker_step(&handles, &mut flush, b"E", start + Duration::from_millis(9), &mut sends);
    let deadline = published_deadline(&handles);
    flush.on_quiet(&handles, deadline - Duration::from_millis(1), || sends.push(0));
    assert_eq!(sends.len(), 1, "the new epoch holds");
    worker_step(
        &handles,
        &mut flush,
        b"\x1b[?2026l",
        start + Duration::from_millis(20),
        &mut sends,
    );
    assert_eq!(sends.len(), 2);
}

/// The release event is sent only after the batch is published: when the sender runs, the
/// generation has advanced and the batch's decoded image is already installed. Servicing the event
/// then advances the window's `Output` cause, and no other cause, through the visible-output filter.
#[test]
fn release_is_sent_after_the_batch_generation_is_published() {
    use crate::app::{
        output_event::OutputEvent,
        redraw::{FrameSettlement, RedrawCause},
    };
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    )
    .with_capture_staging_pool(CaptureStagingPool::new())
    .with_inline_media_pool(crate::app::media::InlineMediaPool::new());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let pane_id = app.windows[&main].tab_states[0].active_pane;
    let pane = &app.windows[&main].panes[&pane_id];
    *pane.redraw_target.lock() = Some(main);
    let handles = PaneVtHandles::from_pane_state(pane);
    let mut flush = OutputFlush::new(pane_id, &handles);
    let base = test_base();
    let decode = |media: &MediaEvent| {
        Some(InlineImage {
            id: 1,
            row: media.row,
            col: media.col,
            width: 1,
            height: 1,
            bgra: Arc::from([1, 2, 3, 255]),
        })
    };
    let mut queued = Vec::new();
    let deliver = |handles: &PaneVtHandles, queued: &mut Vec<(u64, usize)>| {
        send_output_redraw(
            &handles.redraw_target,
            &handles.output_outstanding,
            None,
            Instant::now,
            |_| {
                queued.push((
                    handles.output_generation.load(Ordering::Acquire),
                    handles.inline_images.lock().len(),
                ));
                true
            },
        );
    };

    // A previous batch has been flushed, serviced and presented, so its generation is settled.
    flush.receive(5, base);
    publish_pane_vt_batch_with(
        &handles,
        b"first",
        None,
        &mut None,
        &mut flush.sync_latch,
        decode,
        |_| {},
        || base,
        |_| {},
    );
    deliver(&handles, &mut queued);
    app.service_output_event(OutputEvent::Pane { window_id: main, pane_id }, base);
    let snapshot = app.snapshot_window_redraw_at(main, base).unwrap();
    app.finish_window_redraw(main, &snapshot, FrameSettlement::Presented, base);
    queued.clear();

    let update = b"\x1b[?2026h\x1b_Gf=100,a=T;image\x1b\\paint\x1b[?2026l";
    let at = base + Duration::from_millis(5);
    flush.receive(update.len(), at);
    publish_pane_vt_batch_with(
        &handles,
        update,
        None,
        &mut None,
        &mut flush.sync_latch,
        decode,
        |_| {},
        || at,
        |_| {},
    );
    flush.after_batch(&handles, at, || deliver(&handles, &mut queued));
    assert_eq!(queued, [(2, 1)], "sent once, after the generation and the decoded image");
    let causes = |app: &App| {
        let redraw = &app.windows[&main].redraw;
        (redraw.cause_generation(RedrawCause::Output), redraw.cause_generation(RedrawCause::Chrome))
    };
    let (output_before, chrome_before) = causes(&app);
    app.service_output_event(OutputEvent::Pane { window_id: main, pane_id }, at);
    assert_eq!(causes(&app), (output_before + 1, chrome_before), "the Output route, not Chrome");
}

/// A reset survives later parser sections of the same batch: with the reset in the first or the
/// middle of several reply-split sections and the mode set again at the end, the batch sends
/// exactly one event after its generation is published, the latch is cleared after that send, and
/// the new update holds; a batch whose sections hold no reset sends nothing.
#[test]
fn a_reset_split_from_the_batch_end_by_replies_still_flushes() {
    let cases: [(&str, &[u8], usize, u64); 3] = [
        ("first section", b"\x1b[?2026l\x1b[?2026$pX\x1b[?2026h", 1, 2),
        ("middle section", b"a\x1b[?2026$p\x1b[?2026l\x1b[?2026h\x1b[?2026$pb", 1, 3),
        ("no reset", b"a\x1b[?2026$pb\x1b[?2026$pc", 0, 3),
    ];
    for (name, batch, expected_sends, expected_sections) in cases {
        let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
        let (_pane, handles) = counting_worker_handles(&stats);
        let mut flush = OutputFlush::new(1, &handles);
        let mut sends = Vec::new();
        let start = test_base();
        worker_step(&handles, &mut flush, b"\x1b[?2026h", start, &mut sends);
        let sections_before = sections_parsed(&stats);
        worker_step(&handles, &mut flush, batch, start + Duration::from_millis(1), &mut sends);
        assert_eq!(sections_parsed(&stats) - sections_before, expected_sections, "{name}");
        let generation = handles.output_generation.load(Ordering::Acquire);
        assert_eq!(sends, vec![generation; expected_sends], "{name}");
        assert!(!flush.sync_latch.reset_pending, "{name}: cleared after the flush");
        worker_step(&handles, &mut flush, b"next", start + Duration::from_millis(9), &mut sends);
        let deadline = published_deadline(&handles);
        flush.on_quiet(&handles, deadline - Duration::from_millis(1), || sends.push(0));
        assert_eq!(sends.len(), expected_sends, "{name}: the update still open holds");
    }
}

/// In the worker, the size/age and quiet flushes are reached only past the synchronized-output
/// hold check, while the disconnect flush, and the loop's disconnect arm, consult no hold; the
/// disconnect arm sends the final output before it reports the exit; and the loop takes its flush
/// decision only after the batch has been published.
#[test]
fn threshold_and_quiet_flushes_sit_behind_the_hold_and_disconnect_does_not() {
    let source = concat!(
        include_str!("spawn_pane.rs"),
        "\n",
        include_str!("spawn_pane_commands.rs"),
        "\n",
        include_str!("spawn_pane_output.rs"),
        "\n",
        include_str!("spawn_pane_vt.rs")
    )
    .replace("\r\n", "\n");
    let after_batch = item_body(&source, "pub(in crate::app) fn after_batch(");
    let hold = after_batch.find("self.held").expect("after_batch checks the hold");
    let threshold = after_batch.find("should_flush_pending_pty_redraw").expect("threshold");
    assert!(hold < threshold, "{after_batch}");
    let quiet = item_body(&source, "pub(in crate::app) fn on_quiet(");
    let hold = quiet.find("self.held").expect("on_quiet checks the hold");
    let flush = quiet.find("if self.pending").expect("quiet flush");
    assert!(hold < flush, "{quiet}");
    let disconnect = item_body(&source, "pub(in crate::app) fn on_disconnect(");
    assert!(disconnect.contains("send()"), "{disconnect}");
    assert!(!disconnect.contains("held") && !disconnect.contains("sync"), "{disconnect}");
    let arm_start =
        source.find("Err(crossbeam_channel::RecvTimeoutError::Disconnected) =>").unwrap();
    let arm = &source[arm_start..arm_start + source[arm_start..].find("break;").unwrap()];
    assert!(arm.contains("flush.on_disconnect(") && !arm.contains("held"), "{arm}");
    // The shell's final output is sent before its exit is reported, so the last frame is not lost.
    let final_output = arm.find("flush.on_disconnect(").unwrap();
    let exit = arm.find("report_pane_exit(").expect("the disconnect arm reports the exit");
    assert!(final_output < exit, "{arm}");
    // The worker loop decides only after the batch is published, so a release sees its generation.
    let ok_arm = &source[source.find("Ok(bytes) =>").unwrap()..arm_start];
    let publish = ok_arm.find("process_pane_vt_batch_and_publish(").expect("loop publishes");
    let decide = ok_arm.find("flush.after_batch(").expect("loop decides");
    assert!(publish < decide, "{ok_arm}");
}

/// A lock-free reader that loads the pane between a new epoch's deadline store and its word store
/// sees the old epoch, its reset count, and no deadline, never the next epoch's deadline; once the
/// word is stored, the new epoch, its resets and its own deadline read together.
#[test]
fn a_lock_free_reader_never_pairs_a_word_with_another_epochs_deadline() {
    let (_pane, handles) = pane_and_worker_handles();
    let base = crate::app::sync_clock::origin() + Duration::from_secs(5);
    let open = b"\x1b[?2026h";
    publish_pane_vt_batch_with(
        &handles,
        open,
        None,
        &mut None,
        &mut SyncLatch::default(),
        |_| None,
        |_| {},
        || base,
        |_| {},
    );
    let first = read_published_sync(&handles.sync_word, &handles.sync_deadline_word);
    assert_eq!((first.set, first.epoch, first.resets), (true, 1, 0));
    assert_eq!(first.deadline, Some(stored_deadline(base)));

    let (paused_tx, paused_rx) = crossbeam_channel::bounded(1);
    let (resume_tx, resume_rx) = crossbeam_channel::bounded::<()>(1);
    let worker_handles = handles.clone();
    let later = base + Duration::from_millis(40);
    let worker = std::thread::spawn(move || {
        set_publish_pause(Some(Box::new(move || {
            paused_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
        })));
        let renew = b"\x1b[?2026l\x1b[?2026h";
        publish_pane_vt_batch_with(
            &worker_handles,
            renew,
            None,
            &mut None,
            &mut SyncLatch::default(),
            |_| None,
            |_| {},
            || later,
            |_| {},
        );
    });
    paused_rx.recv_timeout(Duration::from_secs(3)).expect("the worker pauses before its word");
    let torn = read_published_sync(&handles.sync_word, &handles.sync_deadline_word);
    resume_tx.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!((torn.set, torn.epoch, torn.resets), (true, 1, 0), "the word is not stored yet");
    assert_eq!(torn.deadline, None, "the next epoch's deadline is not this epoch's");
    let second = read_published_sync(&handles.sync_word, &handles.sync_deadline_word);
    assert_eq!((second.set, second.epoch, second.resets), (true, 2, 1));
    assert_eq!(second.deadline, Some(stored_deadline(later)));
}

/// A worker that keeps receiving output during an update releases it on the batch that reaches
/// the deadline, with one clock for publication and decision: a batch one nanosecond earlier
/// sends nothing, the batch at the deadline sends one release and counts one timeout, and later
/// output in that epoch flushes normally.
#[test]
fn a_busy_worker_releases_an_update_on_the_batch_at_its_deadline() {
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let (_pane, handles) = counting_worker_handles(&stats);
    let mut flush = OutputFlush::new(3, &handles);
    let mut sends = Vec::new();
    let base = test_base();
    worker_step(&handles, &mut flush, b"\x1b[?2026hrow", base, &mut sends);
    let deadline = published_deadline(&handles);
    assert_eq!(deadline, base + SYNC_OUTPUT_TIMEOUT, "published from the decision's clock");
    worker_step(&handles, &mut flush, b"a", deadline - Duration::from_nanos(1), &mut sends);
    assert!(sends.is_empty(), "one nanosecond early sends nothing");
    worker_step(&handles, &mut flush, b"b", deadline, &mut sends);
    assert_eq!(sends.len(), 1, "the batch at the deadline releases once");
    assert_eq!(stats.sync_timeouts.load(Ordering::Relaxed), 1);
    worker_step(&handles, &mut flush, b"c", deadline + Duration::from_millis(1), &mut sends);
    assert_eq!(sends.len(), 1, "later output coalesces normally");
    worker_step(&handles, &mut flush, b"d", deadline + Duration::from_millis(9), &mut sends);
    assert_eq!(sends.len(), 2, "and flushes on age");
    assert_eq!(stats.sync_timeouts.load(Ordering::Relaxed), 1);
}

/// The worker identifies released updates by the parser's full epoch, not the published 31-bit
/// one: an update timed out as the last epoch before the published epoch wraps does not stop the
/// next update, published as epoch 0, from holding and then expiring normally.
#[test]
fn an_update_after_the_published_epoch_wraps_still_holds() {
    let stats = Arc::new(crate::app::frame_counters::VtFrameStats::default());
    let (_pane, handles) = counting_worker_handles(&stats);
    handles.parser.lock().__test_set_sync_epoch((1 << 31) - 2);
    let mut flush = OutputFlush::new(5, &handles);
    let mut sends = Vec::new();
    let base = test_base();
    worker_step(&handles, &mut flush, b"\x1b[?2026hlast", base, &mut sends);
    let last_deadline = published_deadline(&handles);
    flush.on_quiet(&handles, last_deadline, || sends.push(0));
    assert_eq!((sends.len(), stats.sync_timeouts.load(Ordering::Relaxed)), (1, 1));
    let reset_at = last_deadline + Duration::from_millis(10);
    worker_step(&handles, &mut flush, b"\x1b[?2026l", reset_at, &mut sends);
    assert_eq!(sends.len(), 2, "the late reset flushes");

    let opened = reset_at + Duration::from_millis(10);
    worker_step(&handles, &mut flush, b"\x1b[?2026hfirst", opened, &mut sends);
    let published = read_published_sync(&handles.sync_word, &handles.sync_deadline_word);
    assert_eq!((published.set, published.epoch), (true, 0), "the published epoch wrapped");
    let deadline = published_deadline(&handles);
    flush.on_quiet(&handles, opened + Duration::from_millis(3), || sends.push(0));
    assert_eq!(sends.len(), 2, "the wrapped epoch holds");
    flush.on_quiet(&handles, deadline, || sends.push(0));
    assert_eq!(sends.len(), 3, "and expires once at its deadline");
    assert_eq!(stats.sync_timeouts.load(Ordering::Relaxed), 2);
}
