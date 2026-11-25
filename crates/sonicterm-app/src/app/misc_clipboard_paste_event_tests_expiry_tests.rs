//! A consumed one-time paste grant is checked live wherever its read touches the clipboard or the
//! PTY: before each content-source operation on the reader thread, at every writer admission,
//! retries included, and when an overdue read settles. A read whose grant ended is never served; it
//! asks again as an ordinary `ask` read. Every test ends the grant by moving the grant clock at an
//! explicit phase signal from the fake clipboard or the reader, never by sleeping.

use super::*;

/// The whole reply the fake clipboard's `secret` makes for the `text/plain` read `k`: OK, the DATA
/// frame, DONE. Matching it exactly tells the data apart from any status the read could carry.
const SERVED_REPLY: &[u8] = b"\x1b]5522;type=read:status=OK:id=k\x1b\\\
    \x1b]5522;type=read:status=DATA:id=k:mime=dGV4dC9wbGFpbg==;c2VjcmV0\x1b\\\
    \x1b]5522;type=read:status=DONE:id=k\x1b\\";

/// A harness whose pane got a paste event and holds its live grant; returns the pane and the
/// event's one-time password.
fn granted() -> (Harness, u64, String) {
    let mut harness = harness();
    let pane = harness.pane;
    feed(&harness.app, pane, b"\x1b[?5522h");
    harness.paste();
    let (_, password) = password_of(&harness.events()[0].1);
    (harness, pane, password)
}

/// Contract: a grant that ends while the reader waits on the offer's version check reads no
/// content: the reader checks the grant's end after that check. The test proves the version call
/// was reached before the grant ends.
#[test]
fn grant_ending_during_the_version_check_reads_no_content() {
    let (mut harness, pane, password) = granted();
    let release = harness.gate_versions();
    let before = harness.version_calls();
    harness.follow_up(pane, "Paste event", &password);
    harness.wait_for(|harness| harness.version_calls() == before + 1);
    harness.end_grants();
    release.send(()).unwrap();
    harness.pump_until_reader_idle();
    assert_eq!(harness.reads(), 0, "no content call after the grant ended");
    assert!(!harness.served(pane));
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: a grant that ends during one content read starts no further one: a read of two types
/// whose first read has entered when the grant ends never reads the second, and asks again.
#[test]
fn grant_ending_between_native_reads_makes_no_further_read() {
    let (mut harness, pane, password) = granted();
    let release = harness.gate_reads();
    harness.follow_up_types(pane, "Paste event", &password, &["text/plain", "text/plain"]);
    harness.wait_for(|harness| harness.reads() == 1);
    harness.end_grants();
    release.send(()).unwrap();
    release.send(()).unwrap();
    harness.pump_until_reader_idle();
    assert_eq!(harness.reads(), 1, "no content call after the grant ended");
    assert!(!harness.served(pane));
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: admission checks the grant against the clock, not the turn's start: a read that
/// completed on time with data while its grant was live is not served by a turn that began before
/// the grant ended; the read asks again.
#[test]
fn grant_ending_within_one_admission_turn_is_not_served() {
    let (mut harness, pane, password) = granted();
    harness.follow_up(pane, "Paste event", &password);
    harness.wait_for(|harness| harness.app.clip_state.reads.peek_finished_read().is_some());
    let finished = harness.app.clip_state.reads.peek_finished_read();
    let on_time = Some((Some(SERVED_REPLY.to_vec()), true));
    assert_eq!(finished, on_time, "an on-time completion carrying the DATA frame");
    let turn = Instant::now();
    harness.end_grants();
    harness.app.pump_clipboard_reads_at(turn);
    assert!(!harness.served(pane), "the reply is admitted after the grant ended");
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: a reply whose writer admission was contended, and whose grant ends before the retry,
/// is neither served nor lost: its stale bytes are dropped and the read asks again. The contended
/// attempt is checked to have carried the DATA frame, so the retry withheld real data.
#[test]
fn contended_retry_past_the_grant_returns_to_consent() {
    let (mut harness, pane, password) = granted();
    harness.contend.store(true, Ordering::SeqCst);
    harness.follow_up(pane, "Paste event", &password);
    harness.pump_until(|harness| harness.contended.lock().unwrap().len() == 1);
    harness.end_grants();
    harness.contend.store(false, Ordering::SeqCst);
    harness.app.pump_clipboard_reads();
    let contended = harness.contended.lock().unwrap().clone();
    assert_eq!(contended, [SERVED_REPLY], "the one contended attempt carried the DATA frame");
    assert!(!harness.served(pane), "no reply after the grant ended");
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: a read whose result completed past its deadline, and whose grant ended before that
/// result is handled, asks again instead of being answered `EIO`. The test waits on the completion
/// message itself.
#[test]
fn late_read_with_an_ended_grant_returns_to_consent() {
    let (mut harness, pane, password) = granted();
    // A zero deadline makes every completion late; the reader's result waits unsettled.
    harness.app.clip_state.reads.reader_deadline = Duration::ZERO;
    harness.follow_up(pane, "Paste event", &password);
    harness.wait_for(|harness| harness.app.clip_state.reads.peek_finished_read().is_some());
    let finished = harness.app.clip_state.reads.peek_finished_read();
    let late = Some((Some(SERVED_REPLY.to_vec()), false));
    assert_eq!(finished, late, "a late completion carrying the DATA frame");
    harness.end_grants();
    harness.pump_until_reader_idle();
    let sent = harness.sent.lock().unwrap().clone();
    assert!(!sent.iter().any(|(_, bytes)| contains(bytes, b"status=EIO")), "no EIO");
    assert!(!harness.served(pane));
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: a read abandoned at its deadline, whose grant ended before that deadline was handled,
/// still gets its deadline answer exactly once: a status with no clipboard data needs no grant.
#[test]
fn abandoned_read_past_its_grant_is_answered_once() {
    let (mut harness, pane, password) = granted();
    harness.app.clip_state.reads.reader_deadline = Duration::ZERO;
    let release = harness.gate_reads();
    harness.follow_up(pane, "Paste event", &password);
    // The reader is inside its native call when the grant ends and the deadline is handled.
    harness.wait_for(|harness| harness.reads() == 1);
    harness.end_grants();
    harness.app.pump_clipboard_reads();
    release.send(()).unwrap();
    harness.pump_until_reader_idle();
    let sent = harness.sent.lock().unwrap().clone();
    let answers = sent.iter().filter(|(_, bytes)| contains(bytes, b"status=EIO")).count();
    assert_eq!(answers, 1, "the deadline answer goes out once");
    assert!(!harness.served(pane), "no clipboard data");
}

/// Contract: a paste-approved reply carries its grant's end to the PTY writer, which checks it under
/// its admission lock immediately before queuing; a reply the writer refuses there as expired is
/// not lost and not retried as busy: its data is dropped and the read asks again. The refused
/// attempt carried the DATA frame and exactly the end its grant was issued with.
#[test]
fn reply_refused_as_expired_at_admission_returns_to_consent() {
    let (mut harness, pane, password) = granted();
    // The pane holds one grant; the follow-up consumes it, so its end is read first.
    let issued = harness.app.clip_state.reads.paste_grants.oldest_issue().expect("a held grant");
    let grant_end = issued + PASTE_GRANT_LIFETIME;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let seen = attempts.clone();
    harness.app.clip_state.reads.port = Some(Box::new(move |_pane, reply: SensitiveReply| {
        seen.lock().unwrap().push((reply.valid_until(), reply.bytes().to_vec()));
        // The writer finds the grant's end passed at its admission instant.
        Err(SensitiveReplyError::Expired(reply))
    }));
    harness.follow_up(pane, "Paste event", &password);
    harness.pump_until_reader_idle();
    let attempts = attempts.lock().unwrap().clone();
    // One admission attempt, no busy status after it, with the data and the grant's own end.
    assert_eq!(attempts, [(Some(grant_end), SERVED_REPLY.to_vec())]);
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: a read whose reader cannot start, and whose grant ended by then, returns its ticket to
/// `waiting` through its custody, and its consent page opens in that same pump turn, without
/// waiting for an unrelated event.
#[test]
fn ticket_requeued_when_its_reader_cannot_start_is_presented_at_once() {
    let (mut harness, pane, password) = granted();
    let main = harness.app.registry.main_window_id.expect("a main window");
    harness.app.registry.frontmost_window = Some(main);
    let clock = harness.app.clip_state.reads.grant_clock.clone();
    harness.app.clip_state.reads.fail_spawn = Some(Box::new(move || {
        // The grant ends while the reader is starting.
        clock.advance(PASTE_GRANT_LIFETIME + Duration::from_secs(1));
    }));
    harness.follow_up(pane, "Paste event", &password);
    assert!(!harness.served(pane));
    assert!(harness.app.clip_state.reads.presented.is_some(), "the page opens in the same turn");
}
