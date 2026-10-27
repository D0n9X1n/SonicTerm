//! VT worker echo, guard and program-output publication contracts.

use super::super::spawn_pane_tests::*;
use super::*;

/// The section in which the target goes from absent to present records the generation its batch
/// publishes and that section's parse instant.
#[test]
fn absent_to_present_records_the_batch_generation() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"a", base + Duration::from_millis(2));
    let appearance = echo_trace(&watch).appearance.expect("the echo appeared");
    assert_eq!(appearance.generation, 1);
    assert_eq!(handles.output_generation.load(Ordering::Acquire), 1, "the batch it names");
    assert_eq!(appearance.parsed_at, base + Duration::from_millis(2));
    assert!(!appearance.sync_open);
}

/// A batch that parses in several sections, split at a DECRQM reply, records the one generation
/// the whole batch publishes, whichever section the echo appears in.
#[test]
fn a_reply_split_batch_records_one_generation() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    echo_batch(&handles, b"z", base);
    arm_echo(&watch, &handles, 1, (0, 2, 'X'), base);
    let stats = Arc::clone(&handles.frame_counters.as_ref().expect("counting").vt);
    let sections_before = sections_parsed(&stats);
    echo_batch(&handles, b"a\x1b[?2026$pX", base + Duration::from_millis(1));
    assert!(sections_parsed(&stats) - sections_before >= 2, "the batch took several sections");
    let appearance = echo_trace(&watch).appearance.expect("the echo appeared");
    assert_eq!(appearance.generation, 2, "the batch's generation, not a section's");
    assert_eq!(handles.output_generation.load(Ordering::Acquire), 2);
}

/// An empty batch parses nothing, so it records no fact.
#[test]
fn an_empty_batch_records_nothing() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"", base);
    let trace = echo_trace(&watch);
    assert_eq!(trace.appearance, None);
    assert!(!(trace.pre_present || trace.identity_changed || trace.lost));
}

/// An echo already in the cell before arming is never an absent-to-present appearance; the next
/// section's pre-read records it as pre-present.
#[test]
fn a_present_echo_before_arming_reads_first_appearance_unobserved() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    echo_batch(&handles, b"a", base);
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"b", base + Duration::from_millis(1));
    let trace = echo_trace(&watch);
    assert!(trace.pre_present);
    assert_eq!(trace.appearance, None);
}

/// Evicting an older history row changes what every absolute index names, even when the target
/// row survives, so the section records an identity change and no appearance.
#[test]
fn eviction_of_an_older_row_reads_row_identity_changed() {
    let (_pane, handles, watch) = echo_worker(10, 2, 2);
    let base = test_base();
    // Two rows in history, two visible: r0, r1 | r2, r3.
    echo_batch(&handles, b"r0\r\nr1\r\nr2\r\nr3", base);
    // The target is r3's column 3, still absent; r0 is evicted by the next line.
    arm_echo(&watch, &handles, 1, (3, 3, 'q'), base);
    echo_batch(&handles, b"\r\nzz", base + Duration::from_millis(1));
    let trace = echo_trace(&watch);
    assert!(trace.identity_changed);
    assert_eq!(trace.appearance, None);
}

/// Evicting the target row itself records an identity change.
#[test]
fn eviction_of_the_target_reads_row_identity_changed() {
    let (_pane, handles, watch) = echo_worker(10, 2, 1);
    let base = test_base();
    // History holds r0; r1 and r2 are visible.
    echo_batch(&handles, b"r0\r\nr1\r\nr2", base);
    arm_echo(&watch, &handles, 1, (0, 5, 'q'), base);
    echo_batch(&handles, b"\r\nzz", base + Duration::from_millis(1));
    let trace = echo_trace(&watch);
    assert!(trace.identity_changed, "the target row was evicted");
    assert_eq!(trace.appearance, None);
}

/// Switching to the alternate screen changes the screen epoch, so the section records an
/// identity change.
#[test]
fn a_screen_switch_reads_row_identity_changed() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"\x1b[?1049ha", base + Duration::from_millis(1));
    let trace = echo_trace(&watch);
    assert!(trace.identity_changed);
    assert_eq!(trace.appearance, None, "an identity change records no appearance");
}

/// An echo that appears, is overwritten and reappears keeps its first appearance and is marked
/// lost; the reappearance is not a second appearance.
#[test]
fn overwrite_then_reappearance_reads_echo_overwritten() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"a", base + Duration::from_millis(1));
    echo_batch(&handles, b"\rb", base + Duration::from_millis(2));
    echo_batch(&handles, b"\ra", base + Duration::from_millis(3));
    let trace = echo_trace(&watch);
    assert!(trace.lost);
    let appearance = trace.appearance.expect("the first appearance");
    assert_eq!((appearance.generation, appearance.parsed_at), (1, base + Duration::from_millis(1)));
}

/// Every targeted flush takes exactly one token decision and sends once, whatever the watch's
/// state: unarmed, armed with no appearance, eligible, already published, and armed again under a
/// new token while a publication was in flight. Only the eligible flush records a publication and
/// its delivery.
#[test]
fn each_targeted_flush_takes_one_token_decision_in_every_watch_state() {
    use std::sync::atomic::Ordering::Relaxed;
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let stats = Arc::clone(&handles.frame_counters.as_ref().expect("counting").vt);
    let base = test_base();
    let release = |handles: &PaneVtHandles| handles.output_outstanding.store(false, Relaxed);
    // Unarmed.
    assert_eq!(echo_flush(&handles, base, true), [echo_window()]);
    assert_eq!(watch.peek(), None, "nothing recorded while unarmed");
    release(&handles);
    // Armed, no appearance.
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    assert_eq!(echo_flush(&handles, base, true), [echo_window()]);
    assert_eq!(echo_trace(&watch).publication, None);
    release(&handles);
    // Eligible.
    echo_batch(&handles, b"a", base + Duration::from_millis(1));
    assert_eq!(echo_flush(&handles, base + Duration::from_millis(2), true), [echo_window()]);
    let trace = echo_trace(&watch);
    let publication = trace.publication.expect("the eligible flush is published");
    assert_eq!(publication.window, Some(echo_window()));
    let delivery = trace.delivery.expect("and its decision");
    assert_eq!(
        (delivery.seq, delivery.outcome),
        (publication.seq, crate::app::echo_watch::EchoDeliveryOutcome::Sent)
    );
    release(&handles);
    // Already published.
    assert_eq!(echo_flush(&handles, base + Duration::from_millis(3), true), [echo_window()]);
    assert_eq!(echo_trace(&watch).publication, Some(publication), "the first one stays");
    release(&handles);
    // A re-arm between the flush's token load and its slot lock: the stale publication is discarded.
    let rearm_watch = Arc::clone(&watch);
    let identity = trace.target.identity;
    crate::app::echo_watch::pause_next_writer(move || {
        let target = crate::app::echo_watch::EchoWatchTarget {
            abs_row: 0,
            col: 1,
            character: 'b',
            identity,
        };
        rearm_watch.arm(crate::app::echo_watch::ArmToken::for_test(2), target, base);
    });
    assert_eq!(echo_flush(&handles, base + Duration::from_millis(4), true), [echo_window()]);
    let fresh = echo_trace(&watch);
    assert_eq!(
        (fresh.target.character, fresh.publication),
        ('b', None),
        "nothing for the new token"
    );
    assert_eq!(stats.flushes.load(Relaxed), 5, "one decision per flush");
    assert_eq!(stats.flushes_suppressed.load(Relaxed), 0);
}

/// A flush published while the previous event is outstanding still records its decision, as
/// suppressed, and counts as suppressed.
#[test]
fn a_suppressed_flush_reaches_delivery_recording() {
    use std::sync::atomic::Ordering::Relaxed;
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let stats = Arc::clone(&handles.frame_counters.as_ref().expect("counting").vt);
    let base = test_base();
    handles.output_outstanding.store(true, Relaxed);
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"a", base + Duration::from_millis(1));
    assert!(echo_flush(&handles, base + Duration::from_millis(2), true).is_empty());
    let delivery = echo_trace(&watch).delivery.expect("the decision is recorded");
    assert_eq!(delivery.outcome, crate::app::echo_watch::EchoDeliveryOutcome::Suppressed);
    assert_eq!(stats.flushes_suppressed.load(Relaxed), 1);
}

/// A send the event loop refuses records `Refused` and still clears the token.
#[test]
fn a_refused_send_reads_send_refused_and_clears_the_token() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"a", base + Duration::from_millis(1));
    assert_eq!(echo_flush(&handles, base + Duration::from_millis(2), false), [echo_window()]);
    let delivery = echo_trace(&watch).delivery.expect("the decision is recorded");
    assert_eq!(delivery.outcome, crate::app::echo_watch::EchoDeliveryOutcome::Refused);
    assert!(!handles.output_outstanding.load(Ordering::Acquire), "the token is cleared");
}

/// The flush path's own clock is read 0 times unarmed (targeted or not), 0 times armed before any
/// appearance, once for an eligible untargeted flush and twice for an eligible targeted one.
#[test]
fn new_clock_reads_by_state() {
    use std::cell::Cell;
    let base = test_base();
    let reads = Cell::new(0_u32);
    let clock = || {
        reads.set(reads.get() + 1);
        base + Duration::from_millis(5)
    };
    let flush = |handles: &PaneVtHandles, clock: &mut dyn FnMut() -> Instant| {
        send_output_redraw(
            &handles.redraw_target,
            &handles.output_outstanding,
            handles.frame_counters.as_ref(),
            clock,
            |_| true,
        );
        handles.output_outstanding.store(false, Ordering::Relaxed);
    };
    let mut counted = Vec::new();
    let mut measure = |handles: &PaneVtHandles| {
        let before = reads.get();
        let mut tick = clock;
        flush(handles, &mut tick);
        counted.push(reads.get() - before);
    };
    let (_pane, targeted, watch) = echo_worker(80, 24, 100);
    measure(&targeted);
    let (_untargeted_pane, untargeted, untargeted_watch) = echo_worker(80, 24, 100);
    *untargeted.redraw_target.lock() = None;
    measure(&untargeted);
    arm_echo(&watch, &targeted, 1, (0, 0, 'a'), base);
    measure(&targeted);
    arm_echo(&untargeted_watch, &untargeted, 1, (0, 0, 'a'), base);
    echo_batch(&untargeted, b"a", base + Duration::from_millis(1));
    measure(&untargeted);
    echo_batch(&targeted, b"a", base + Duration::from_millis(1));
    measure(&targeted);
    assert_eq!(counted, [0, 0, 0, 1, 2]);
}

/// The publication is in the record when the token decision runs, and is stamped before it.
#[test]
fn publication_is_recorded_before_the_token_decision() {
    use std::cell::Cell;
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"a", base + Duration::from_millis(1));
    let tick = Cell::new(0_u64);
    let mut seen_at_send = None;
    send_output_redraw(
        &handles.redraw_target,
        &handles.output_outstanding,
        handles.frame_counters.as_ref(),
        || {
            tick.set(tick.get() + 1);
            base + Duration::from_millis(10 * tick.get())
        },
        |_| {
            let trace = watch.peek().expect("armed");
            seen_at_send = Some((trace.publication.is_some(), trace.delivery.is_some()));
            true
        },
    );
    assert_eq!(seen_at_send, Some((true, false)), "published, not yet decided, at the send");
    let trace = echo_trace(&watch);
    let published_at = trace.publication.expect("published").published_at;
    assert!(published_at < trace.delivery.expect("decided").decided_at);
}

/// A take between the publication and its decision sees the publication without a decision, and
/// the decision written after the take is discarded; without the pause the decision is recorded,
/// stamped after the publication.
#[test]
fn a_writer_paused_between_publication_and_delivery_reads_not_observed_or_after_present() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"a", base + Duration::from_millis(1));
    let (taken_tx, taken_rx) = std::sync::mpsc::channel();
    let pause_watch = Arc::clone(&watch);
    crate::app::echo_watch::pause_before_delivery(move || {
        let taken = pause_watch.take(crate::app::echo_watch::ArmToken::for_test(1));
        taken_tx.send(taken).expect("test receiver");
    });
    echo_flush(&handles, base + Duration::from_millis(2), true);
    let taken = taken_rx.recv_timeout(Duration::from_secs(5)).expect("the pause point ran");
    let crate::app::echo_watch::SlotTake::Trace(trace) = taken else {
        panic!("take at the pause returned {taken:?}")
    };
    assert!(trace.publication.is_some(), "the publication is in the taken record");
    assert_eq!(trace.delivery, None, "the decision was not yet recorded");
    assert_eq!(echo_trace(&watch).delivery, None, "the late decision was discarded");

    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"a", base + Duration::from_millis(1));
    let tick = std::cell::Cell::new(0_u64);
    send_output_redraw(
        &handles.redraw_target,
        &handles.output_outstanding,
        handles.frame_counters.as_ref(),
        || {
            tick.set(tick.get() + 1);
            base + Duration::from_millis(10 * tick.get())
        },
        |_| true,
    );
    let trace = echo_trace(&watch);
    let decided_at = trace.delivery.expect("recorded").decided_at;
    assert!(decided_at > trace.publication.expect("published").published_at);
}

/// The echo's publication finds an earlier flush still pending, so it is marked coalesced.
#[test]
fn a_coalesced_flush_sets_coalesced() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    echo_batch(&handles, b"z", base);
    echo_flush(&handles, base, true);
    handles.output_outstanding.store(false, Ordering::Relaxed);
    arm_echo(&watch, &handles, 1, (0, 1, 'a'), base);
    echo_batch(&handles, b"a", base + Duration::from_millis(1));
    echo_flush(&handles, base + Duration::from_millis(2), true);
    assert!(echo_trace(&watch).publication.expect("published").coalesced);
}

/// An echo parsed inside a synchronized update is held with it: nothing is published until the
/// reset's flush, and the appearance records that the update was open.
#[test]
fn an_echo_held_by_a_synchronized_update_publishes_at_the_reset_with_sync_open() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let mut flush = OutputFlush::new(1, &handles);
    let base = test_base();
    let mut sends = Vec::new();
    echo_worker_step(&handles, &mut flush, b"\x1b[?2026h", base, &mut sends);
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_worker_step(&handles, &mut flush, b"a", base + Duration::from_millis(1), &mut sends);
    assert!(sends.is_empty(), "held while the update is open");
    assert_eq!(echo_trace(&watch).publication, None);
    echo_worker_step(
        &handles,
        &mut flush,
        b"\x1b[?2026l",
        base + Duration::from_millis(4),
        &mut sends,
    );
    assert_eq!(sends, [echo_window()], "the reset flushes once");
    let trace = echo_trace(&watch);
    assert!(trace.appearance.expect("appeared").sync_open);
    let published_at = trace.publication.expect("published at the reset").published_at;
    assert_eq!(published_at, base + Duration::from_millis(4));
}

/// An update that never resets is released at its published deadline, and the echo it held is
/// published by that release.
#[test]
fn an_echo_held_past_the_deadline_publishes_at_release_timeout() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let mut flush = OutputFlush::new(1, &handles);
    let base = test_base();
    let mut sends = Vec::new();
    echo_worker_step(&handles, &mut flush, b"\x1b[?2026h", base, &mut sends);
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_worker_step(&handles, &mut flush, b"a", base + Duration::from_millis(1), &mut sends);
    let deadline = published_deadline(&handles);
    flush.on_quiet(&handles, deadline, || sends.extend(echo_flush(&handles, deadline, true)));
    assert_eq!(sends, [echo_window()], "released once at the deadline");
    let published_at = echo_trace(&watch).publication.expect("published").published_at;
    assert_eq!(published_at, deadline);
}

/// An armed batch takes the slot lock on the worker thread; once the harness takes the record the
/// worker takes none. Each count brackets only the worker's batch and flush, on its own thread.
#[test]
fn a_taken_watch_stops_the_workers_slot_locks() {
    use crate::app::echo_watch::slot_locks;
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    let before = slot_locks();
    echo_batch(&handles, b"a", base + Duration::from_millis(1));
    echo_flush(&handles, base + Duration::from_millis(2), true);
    let armed_locks = slot_locks() - before;
    assert!(armed_locks >= 1, "an armed batch locks the slot");
    watch.take(crate::app::echo_watch::ArmToken::for_test(1));
    handles.output_outstanding.store(false, Ordering::Relaxed);
    let before = slot_locks();
    echo_batch(&handles, b"b", base + Duration::from_millis(3));
    echo_flush(&handles, base + Duration::from_millis(4), true);
    assert_eq!(slot_locks() - before, 0, "a taken watch costs the worker no lock");
}

/// A worker handle created after facts were recorded (a new handle for each batch, as the test
/// hooks make) resumes from the record: the echo it saw appear reads lost when overwritten, never
/// pre-present.
#[test]
fn a_new_worker_handle_resumes_from_the_recorded_facts() {
    let (pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    arm_echo(&watch, &handles, 1, (0, 0, 'a'), base);
    echo_batch(&handles, b"a", base + Duration::from_millis(1));
    let fresh = PaneVtHandles::from_pane_state(&pane);
    echo_batch(&fresh, b"\rb", base + Duration::from_millis(2));
    let trace = echo_trace(&watch);
    assert!(trace.lost, "the overwrite after the appearance is lost");
    assert!(!trace.pre_present, "the recorded appearance is not re-read as pre-present");
}

/// A new pane's reader stamps output reads exactly when its frame counters are on, and the other
/// spawn options are the launch's own: the shell, `TERM_PROGRAM` and working directory pass
/// through.
#[test]
fn pane_spawn_options_stamp_reads_only_with_the_frame_counter_gate() {
    let app = App::new(Theme::default(), Config::default(), Keymap::default());
    let launch = super::super::super::pane_launch::PaneLaunch {
        cwd: Some(std::path::PathBuf::from("/work")),
        ..Default::default()
    };
    for gate in [false, true] {
        let opts = app.pane_spawn_opts(&launch, gate);
        assert_eq!(opts.diagnostic_timestamps, gate, "gate {gate}");
        assert_eq!(opts.cwd, Some(std::path::PathBuf::from("/work")));
        assert_eq!(opts.term_program, app.config.terminal.term_program);
        assert!(!opts.clean_e2e, "no runtime smoke");
    }
}

/// Both pane creation paths, main and child, read the gate before spawning and pass it to the PTY
/// through the one options helper and the one launch spawn helper, then store the same counters on the pane, so no path starts a reader
/// that disagrees with its pane. A child pane needs a native window, so this pins the source.
#[test]
fn main_and_child_panes_take_the_read_stamp_flag_from_the_gate_before_spawning() {
    for (name, source) in [
        (
            "spawn_pane.rs",
            concat!(
                include_str!("spawn_pane.rs"),
                "\n",
                include_str!("spawn_pane_commands.rs"),
                "\n",
                include_str!("spawn_pane_output.rs"),
                "\n",
                include_str!("spawn_pane_vt.rs")
            ),
        ),
        ("child_tabs.rs", include_str!("child_tabs.rs")),
    ] {
        let gate = source
            .find("let frame_counters = self.pane_frame_counters();")
            .unwrap_or_else(|| panic!("{name} reads the gate"));
        let opts = source
            .find("self.pane_spawn_opts(launch, frame_counters.is_some())")
            .unwrap_or_else(|| panic!("{name} passes the gate to the spawn options"));
        let spawn = source[opts..]
            .find("self.spawn_launch_pty(pane_id, &parser, launch, cols, rows, &shell_opts)")
            .map(|offset| opts + offset)
            .unwrap_or_else(|| panic!("{name} spawns with those options"));
        assert!(
            gate < opts && opts < spawn,
            "{name} reads the gate, builds the options, then spawns"
        );
        assert!(
            source[spawn..].contains("frame_counters = frame_counters;"),
            "{name} stores the same counters"
        );
    }
}

/// Every section of a reply-split batch consumes the same chunk, so the section where the echo
/// appears, here a later one, carries that chunk's read stamp, not its own parse instant.
#[test]
fn a_later_section_of_one_chunk_carries_the_chunks_read_stamp() {
    let (_pane, handles, watch) = echo_worker(80, 24, 100);
    let base = test_base();
    echo_batch(&handles, b"z", base);
    arm_echo_timeline(&watch, &handles, 1, base);
    let stats = Arc::clone(&handles.frame_counters.as_ref().expect("counting").vt);
    let sections_before = sections_parsed(&stats);
    let read_at = base + Duration::from_millis(1);
    echo_batch_read(&handles, b"a\x1b[?2026$pX", Some(read_at), base + Duration::from_millis(3));
    assert!(sections_parsed(&stats) - sections_before >= 2, "the batch took several sections");
    let timeline = echo_timeline_of(&watch, 1);
    assert!(timeline.chunk_timestamps);
    assert_eq!(chunk_reads(&timeline), [1_000_000], "the chunk's read, 1 ms after arm");
}

/// An unstamped chunk, and a chunk read before arm, record no `LatestChunkRead`; neither is ever
/// replaced by zero or by the parse instant.
#[test]
fn an_unstamped_or_pre_arm_chunk_records_no_read() {
    for pre_arm in [false, true] {
        let (_pane, handles, watch) = echo_worker(80, 24, 100);
        let base = test_base();
        // The pre-arm read is an explicit offset before the arm, never the process's own `now()`.
        let read_at = pre_arm.then(|| base - Duration::from_millis(1));
        echo_batch(&handles, b"z", base);
        arm_echo_timeline(&watch, &handles, 1, base);
        echo_batch_read(&handles, b"aX", read_at, base + Duration::from_millis(3));
        assert!(echo_trace(&watch).appearance.is_some(), "the echo appeared ({read_at:?})");
        assert!(chunk_reads(&echo_timeline_of(&watch, 1)).is_empty(), "{read_at:?}");
    }
}

/// A counting worker publishes each parser section with D2.1a's own `before_lock` and `locked_at`
/// reads, so a record's endpoints are exactly the instants the section counters recorded.
#[test]
fn a_counting_worker_publishes_its_section_with_the_section_reads() {
    let epoch = crate::app::frame_counters::clock_epoch();
    let (_pane, handles, log) = counting_worker_with_log();
    let mut tick_ns = 1_000_u64;
    process_pane_vt_batch_with(
        &handles,
        b"abc",
        None,
        &mut None,
        &mut SyncLatch::default(),
        |_| None,
        |_| {},
        || {
            tick_ns += 1_000;
            epoch + Duration::from_nanos(tick_ns)
        },
        |_| {},
    );
    let pane = log.take(crate::app::guard_correlation::LogBuffers::new()).0;
    let expected = crate::app::guard_correlation::SectionRecordV1 {
        section_seq: 1,
        before_lock_ns: 2_000,
        locked_at_ns: 3_000,
    };
    assert_eq!(pane.records, vec![expected], "the first read is before_lock, the second locked_at");
    assert!(pane.pending.is_none() && pane.abandoned.is_empty());
}

/// T4: a worker section that unwinds after `locked_at` abandons its registration only after the parser
/// guard is released, so the log mutex is never taken under a parser guard; nothing stays pending.
#[test]
fn a_worker_section_unwinding_after_its_lock_is_abandoned_after_the_guard() {
    let epoch = crate::app::frame_counters::clock_epoch();
    let (_pane, handles, log) = counting_worker_with_log();
    let parser = Arc::clone(&handles.parser);
    let parser_free = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&parser_free);
    crate::app::guard_correlation::ABANDON_PROBE.with(|probe| {
        *probe.borrow_mut() =
            Some(Box::new(move || seen.store(parser.try_lock().is_some(), Ordering::SeqCst)));
    });
    let mut reads = 0_u64;
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        process_pane_vt_batch_with(
            &handles,
            b"abc",
            None,
            &mut None,
            &mut SyncLatch::default(),
            |_| None,
            |_| {},
            || {
                reads += 1;
                // The third read is parsed_at, under the parser guard and after locked_at.
                assert!(reads < 3, "the section unwinds after locked_at");
                epoch + Duration::from_nanos(reads * 1_000)
            },
            |_| {},
        );
    }));
    crate::app::guard_correlation::ABANDON_PROBE.with(|probe| *probe.borrow_mut() = None);
    assert!(unwound.is_err());
    assert!(
        parser_free.load(Ordering::SeqCst),
        "the parser guard was released before the log mutex"
    );
    let pane = log.take(crate::app::guard_correlation::LogBuffers::new()).0;
    assert_eq!(pane.abandoned.iter().map(|entry| entry.section_seq).collect::<Vec<_>>(), vec![1]);
    assert!(pane.pending.is_none() && pane.records.is_empty());
}
