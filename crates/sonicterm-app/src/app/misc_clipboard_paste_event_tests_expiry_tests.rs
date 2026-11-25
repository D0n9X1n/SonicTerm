//! A consumed one-time paste grant is checked live wherever its read touches the clipboard or the
//! PTY: before each native content call on the reader thread, at every writer admission, retries
//! included, and when an overdue read settles. A read whose grant ended is never served; it asks
//! again as an ordinary `ask` read.

use super::*;

/// A harness whose pane got a paste event and holds its grant, aged so it ends `margin` from now;
/// returns the pane and the event's one-time password.
fn granted(margin: Duration) -> (Harness, u64, String) {
    let mut harness = harness();
    let pane = harness.pane;
    feed(&harness.app, pane, b"\x1b[?5522h");
    harness.paste();
    let (_, password) = password_of(&harness.events()[0].1);
    harness.app.clip_state.reads.paste_grants.age(PASTE_GRANT_LIFETIME - margin);
    (harness, pane, password)
}

/// Contract: a grant that ends while the reader waits on the offer's version check reads no
/// content: the reader checks the grant's end after that check, so nothing is read or served and
/// the read asks again.
#[test]
fn grant_ending_during_the_version_check_reads_no_content() {
    let (mut harness, pane, password) = granted(Duration::from_millis(300));
    let release = harness.gate_versions();
    harness.follow_up(pane, "Paste event", &password);
    // The reader waits in its version check until after the grant ended.
    std::thread::sleep(Duration::from_millis(500));
    release.send(()).unwrap();
    release.send(()).unwrap();
    harness.pump_until_reader_idle();
    assert_eq!(harness.reads(), 0, "no content call after the grant ended");
    assert!(!harness.served(pane));
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: a grant that ends during one native content read makes no further one: a read of two
/// types whose first read is still running when the grant ends never reads the second, is not
/// served, and asks again.
#[test]
fn grant_ending_between_native_reads_makes_no_further_read() {
    let (mut harness, pane, password) = granted(Duration::from_millis(300));
    let release = harness.gate_reads();
    harness.follow_up_types(pane, "Paste event", &password, &["text/plain", "text/plain"]);
    harness.pump_until(|harness| harness.reads() == 1);
    // The first content read is still running when the grant ends.
    std::thread::sleep(Duration::from_millis(500));
    release.send(()).unwrap();
    release.send(()).unwrap();
    harness.pump_until_reader_idle();
    assert_eq!(harness.reads(), 1, "no content call after the grant ended");
    assert!(!harness.served(pane));
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: writer admission checks the grant against the clock, not the turn's start: a read
/// that finished while its grant was live is not served by a turn that began before the grant
/// ended but admits the reply after it; the read asks again.
#[test]
fn grant_ending_within_one_admission_turn_is_not_served() {
    let (mut harness, pane, password) = granted(Duration::from_millis(400));
    harness.follow_up(pane, "Paste event", &password);
    // The turn starts while the grant is live; the reader finishes well before it ends.
    let turn = Instant::now();
    std::thread::sleep(Duration::from_millis(600));
    harness.app.pump_clipboard_reads_at(turn);
    assert!(!harness.served(pane), "the reply is admitted after the grant ended");
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: a reply whose writer admission was contended, and whose grant ends before the retry,
/// is neither served nor lost: its stale bytes are dropped and the read asks again.
#[test]
fn contended_retry_past_the_grant_returns_to_consent() {
    let (mut harness, pane, password) = granted(Duration::from_millis(400));
    harness.contend.store(true, Ordering::SeqCst);
    harness.follow_up(pane, "Paste event", &password);
    harness.pump_until(|harness| harness.contended.load(Ordering::SeqCst) == 1);
    std::thread::sleep(Duration::from_millis(600));
    harness.contend.store(false, Ordering::SeqCst);
    harness.app.pump_clipboard_reads();
    assert!(!harness.served(pane), "no reply after the grant ended");
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: a read whose reader returned past its deadline, and whose grant ended meanwhile, asks
/// again instead of being answered `EIO`, when its late result is handled before the deadline is.
/// A read abandoned at its deadline keeps the deadline's handling.
#[test]
fn late_read_with_an_ended_grant_returns_to_consent() {
    let (mut harness, pane, password) = granted(Duration::from_millis(200));
    harness.app.clip_state.reads.reader_deadline = Duration::from_millis(300);
    let release = harness.gate_reads();
    harness.follow_up(pane, "Paste event", &password);
    // The grant ends and the deadline passes while the reader waits in its native call.
    std::thread::sleep(Duration::from_millis(500));
    release.send(()).unwrap();
    // The reader returns before the next turn, so its result is handled as late.
    std::thread::sleep(Duration::from_millis(200));
    harness.pump_until_reader_idle();
    let sent = harness.sent.lock().unwrap().clone();
    assert!(!sent.iter().any(|(_, bytes)| contains(bytes, b"status=EIO")), "no EIO");
    assert!(!harness.served(pane));
    assert_eq!(harness.waiting(), 1, "the read asks again");
}

/// Contract: a read abandoned at its deadline, whose grant ended before that deadline, still gets
/// its deadline answer, exactly once: that answer is a status with no clipboard data, so it needs no
/// live grant. Nothing is served and the late result adds no second answer.
#[test]
fn abandoned_read_past_its_grant_is_answered_once() {
    let (mut harness, pane, password) = granted(Duration::from_millis(200));
    harness.app.clip_state.reads.reader_deadline = Duration::from_millis(300);
    let release = harness.gate_reads();
    harness.follow_up(pane, "Paste event", &password);
    // The grant ends, then the deadline passes, while the reader waits in its native call.
    std::thread::sleep(Duration::from_millis(450));
    // The deadline is handled while the native call still runs.
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
/// not lost and not retried as busy: its data is dropped and the read asks again.
#[test]
fn reply_refused_as_expired_at_admission_returns_to_consent() {
    let (mut harness, pane, password) = granted(Duration::from_secs(30));
    let deadlines = Arc::new(Mutex::new(Vec::new()));
    let seen = deadlines.clone();
    harness.app.clip_state.reads.port = Some(Box::new(move |_pane, reply: SensitiveReply| {
        seen.lock().unwrap().push(reply.valid_until());
        // The writer finds the grant's end passed at its admission instant.
        Err(SensitiveReplyError::Expired(reply))
    }));
    harness.follow_up(pane, "Paste event", &password);
    harness.pump_until_reader_idle();
    let deadlines = deadlines.lock().unwrap().clone();
    assert_eq!(deadlines.len(), 1, "one admission attempt, no busy status after it");
    assert!(deadlines[0].is_some(), "the reply carries its grant's end");
    assert_eq!(harness.waiting(), 1, "the read asks again");
}
