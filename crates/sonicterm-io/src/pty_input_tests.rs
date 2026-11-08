use super::*;

#[test]
fn pty_input_queue_has_fixed_capacity() {
    let (tx, rx) = pty_input_channel();
    for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
        tx.try_send(vec![b'x']).expect("queue has bounded capacity");
    }

    assert!(matches!(tx.try_send(vec![b'y']), Err(crossbeam_channel::TrySendError::Full(_))));
    assert_eq!(rx.len(), PTY_INPUT_QUEUE_CAPACITY);
    assert!(pty_input_message_allowed(MAX_PTY_INPUT_MESSAGE_BYTES));
    assert!(!pty_input_message_allowed(MAX_PTY_INPUT_MESSAGE_BYTES + 1));
}

#[test]
fn saturated_pty_input_queue_returns_the_rejected_bytes() {
    let (tx, _rx) = pty_input_channel();
    let queued = AtomicUsize::new(0);
    for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
        tx.try_send(vec![b'x']).expect("fill bounded input queue");
    }
    let rejected = vec![b'y'];

    assert!(matches!(
        try_queue_pty_input((&tx, &queued), &PtyWriterProgress::new(), rejected.clone(), true),
        Err(PtyInputError::QueueFull(bytes)) if bytes == rejected
    ));
    assert_eq!(
        queued.load(Ordering::Relaxed),
        0,
        "a message the queue refused was never queued memory, so it must not be counted"
    );
}

#[test]
fn multi_megabyte_paste_fits_bounded_input_budget() {
    const TWO_MIB: usize = 2 * 1024 * 1024;
    const MAX_QUEUED_INPUT_BYTES: usize = 64 * 1024 * 1024;

    assert!(pty_input_message_allowed(TWO_MIB));
    assert!(
        max_pty_queued_input_bytes() <= MAX_QUEUED_INPUT_BYTES,
        "supporting large pastes must not make queued input unbounded"
    );
}

/// The typed send refuses a full queue; the raw one blocks.
///
/// Blocking is the consequence that matters. The reply forwarder holds this
/// sender in a thread whose stated reason for existing is that the VT loop
/// must never block pushing replies — and it held the raw channel, so a child
/// that stopped draining stalled the forwarder indefinitely.
#[test]
fn the_typed_send_refuses_a_full_queue_where_the_raw_channel_blocks() {
    use std::{sync::mpsc, time::Duration};

    let (tx, _rx) = crossbeam_channel::bounded::<Vec<u8>>(PTY_INPUT_QUEUE_CAPACITY);
    for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
        tx.try_send(vec![0u8; 8]).expect("precondition: the queue fills");
    }

    assert!(
        matches!(
            try_queue_pty_input(
                (&tx, &AtomicUsize::new(0)),
                &PtyWriterProgress::new(),
                vec![0u8; 8],
                true
            ),
            Err(PtyInputError::QueueFull(_))
        ),
        "the typed path must refuse a full queue and hand the bytes back"
    );

    // The raw channel, for contrast: a send that never returns.
    let (done_tx, done_rx) = mpsc::channel();
    let raw = tx.clone();
    let blocked_thread = std::thread::spawn(move || {
        let _ = raw.send(vec![0u8; 8]);
        let _ = done_tx.send(());
    });
    assert!(
        done_rx.recv_timeout(Duration::from_millis(250)).is_err(),
        "precondition for the whole fix: a raw send on a full queue blocks"
    );

    // Let the blocked thread finish so the test does not leak it.
    drop(_rx);
    let _ = blocked_thread.join();
}

/// The typed send refuses an oversized message; the raw one accepts it.
#[test]
fn the_typed_send_enforces_the_message_cap_the_raw_channel_ignores() {
    let (tx, _rx) = crossbeam_channel::bounded::<Vec<u8>>(PTY_INPUT_QUEUE_CAPACITY);
    let oversized = vec![0u8; MAX_PTY_INPUT_MESSAGE_BYTES + 1];
    let queued = AtomicUsize::new(0);

    assert!(
        matches!(
            try_queue_pty_input((&tx, &queued), &PtyWriterProgress::new(), oversized.clone(), true),
            Err(PtyInputError::MessageTooLarge(_))
        ),
        "the typed path must refuse a message above the cap"
    );
    assert_eq!(
        queued.load(Ordering::Relaxed),
        0,
        "a message refused for size never entered the queue, so it must not be counted"
    );
    assert!(
        tx.try_send(oversized).is_ok(),
        "precondition: the raw channel accepts it, which is why the field is private"
    );
}

/// No first-party caller may hold the raw sender; absence is asserted by scanning sources.
#[test]
fn no_first_party_caller_reaches_the_raw_input_channel() {
    const SOURCES: &[(&str, &str)] = &[
        ("spawn_pane.rs", include_str!("../../sonicterm-app/src/app/spawn_pane.rs")),
        ("child_window.rs", include_str!("../../sonicterm-app/src/app/child_window.rs")),
        ("child_tabs.rs", include_str!("../../sonicterm-app/src/app/child_tabs.rs")),
        ("splitter_input.rs", include_str!("../../sonicterm-app/src/app/splitter_input.rs")),
        (
            "child_window_pointer.rs",
            include_str!("../../sonicterm-app/src/app/child_window_pointer.rs"),
        ),
        (
            "child_window_redraw.rs",
            include_str!("../../sonicterm-app/src/app/child_window_redraw.rs"),
        ),
        ("app misc.rs", include_str!("../../sonicterm-app/src/app/misc.rs")),
    ];

    for (name, source) in SOURCES {
        // Code, not prose: a doc comment naming the old field is stale rather
        // than a bypass. Scanning raw text caught exactly that on first run,
        // which is worth keeping — a comment describing a seam that no longer
        // exists misleads the next reader — but it is a different defect.
        let code: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains(".in_tx"),
            "{name} reaches the raw PTY input channel; use `input_sender()` or \
             `send_input_nonblocking`, which apply the cap and refuse rather than block"
        );
    }
}

/// Each admitted user-input write gets the next ordinal and its admission time; the oldest
/// outstanding one is reported, a reply ahead of it is named, and completing all is `Idle`.
#[test]
fn input_progress_names_the_oldest_outstanding_write() {
    let progress = PtyWriterProgress::new();
    let (tx, _rx) = crossbeam_channel::bounded::<Vec<u8>>(PTY_INPUT_QUEUE_CAPACITY);
    let queued = AtomicUsize::new(0);
    assert_eq!(progress.input_progress(), PtyInputProgress::Idle);
    let before = Instant::now();
    try_queue_pty_input((&tx, &queued), &progress, b"a".to_vec(), true).unwrap();
    try_queue_pty_input((&tx, &queued), &progress, b"b".to_vec(), true).unwrap();
    let PtyInputProgress::Oldest { ordinal, admitted_at, on_reply } = progress.input_progress()
    else {
        panic!("two writes are outstanding");
    };
    assert_eq!((ordinal, on_reply), (1, false));
    // Admission times are whole milliseconds since the writer's epoch.
    assert!(admitted_at + Duration::from_millis(2) >= before && admitted_at <= Instant::now());
    progress.input_completed.fetch_add(1, Ordering::Release);
    progress.writer_on.store(WRITER_ON_REPLY, Ordering::Release);
    assert!(matches!(
        progress.input_progress(),
        PtyInputProgress::Oldest { ordinal: 2, on_reply: true, .. }
    ));
    progress.input_completed.fetch_add(1, Ordering::Release);
    assert_eq!(progress.input_progress(), PtyInputProgress::Idle);
}

/// A refused send returns its ordinal, so ordinals stay in channel order and nothing is left
/// outstanding; a slot holding another ordinal's tag is a raced sample, judged as nothing.
#[test]
fn refused_admission_rolls_back_and_a_stale_slot_is_raced() {
    let progress = PtyWriterProgress::new();
    let (tx, _rx) = crossbeam_channel::bounded::<Vec<u8>>(1);
    let queued = AtomicUsize::new(0);
    try_queue_pty_input((&tx, &queued), &progress, b"a".to_vec(), true).unwrap();
    assert!(matches!(
        try_queue_pty_input((&tx, &queued), &progress, b"b".to_vec(), true),
        Err(PtyInputError::QueueFull(_))
    ));
    assert_eq!(progress.input_admitted.load(Ordering::Acquire), 1);
    progress.input_completed.fetch_add(1, Ordering::Release);
    assert_eq!(progress.input_progress(), PtyInputProgress::Idle);
    progress.input_admitted.store(2, Ordering::Release);
    progress.admitted_at[2].store(9 << 44, Ordering::Release);
    assert_eq!(progress.input_progress(), PtyInputProgress::Raced, "the slot holds ordinal 9");
}

/// Diagnostic ordinals compare modulo 2^20, across the 2^20 and the old u32 boundary, on the
/// right slot; the wide counters underneath keep counting past both.
#[test]
fn input_ordinals_wrap_onto_their_own_slot() {
    for start in [u64::from(INPUT_ORDINAL_MASK), u64::from(u32::MAX)] {
        let progress = PtyWriterProgress::new();
        progress.input_admitted.store(start, Ordering::Release);
        progress.input_completed.store(start, Ordering::Release);
        let (tx, _rx) = crossbeam_channel::bounded::<Vec<u8>>(PTY_INPUT_QUEUE_CAPACITY);
        try_queue_pty_input((&tx, &AtomicUsize::new(0)), &progress, b"a".to_vec(), true).unwrap();
        assert!(
            matches!(progress.input_progress(), PtyInputProgress::Oldest { ordinal: 0, .. }),
            "{start}"
        );
    }
}

/// Two threads admitting at once get ordinals in channel order: each send happens under the
/// admission lock, so the ordinal it sends is the one it was admitted with, and they arrive 1..=32.
#[test]
fn concurrent_producers_get_ordinals_in_channel_order() {
    let progress = Arc::new(PtyWriterProgress::new());
    let (tx, rx) = crossbeam_channel::bounded::<u64>(64);
    let producers: Vec<_> = (0..2)
        .map(|_| {
            let (progress, tx) = (Arc::clone(&progress), tx.clone());
            std::thread::spawn(move || {
                for _ in 0..16 {
                    progress
                        .admit(|running| {
                            assert!(running);
                            let ordinal = progress.input_admitted.load(Ordering::Relaxed);
                            tx.try_send(ordinal).map_err(|_| ())
                        })
                        .unwrap();
                }
            })
        })
        .collect();
    for producer in producers {
        producer.join().unwrap();
    }
    let ordinals: Vec<u64> = rx.try_iter().collect();
    assert_eq!(ordinals, (1..=32).collect::<Vec<_>>());
}

#[test]
fn admitted_input_marks_input_seen_and_a_draft_does_not() {
    // The pane's parser trusts a shell token only while nobody has typed: every admitted input
    // message sets the flag before it is queued, a script draft (typed without Enter) never does,
    // and a refused message still sets it, since trust is withdrawn early rather than late.
    let (tx, _rx) = pty_input_channel();
    let queued = AtomicUsize::new(0);
    let seen = Arc::new(AtomicBool::new(false));
    let progress = PtyWriterProgress::with_input_seen(Some(seen.clone()));
    try_queue_pty_input((&tx, &queued), &progress, b"echo draft".to_vec(), false).unwrap();
    assert!(!seen.load(Ordering::SeqCst), "a draft runs nothing, so it keeps the token trusted");
    try_queue_pty_input((&tx, &queued), &progress, b"\r".to_vec(), true).unwrap();
    assert!(seen.load(Ordering::SeqCst), "typed input withdraws the trust");

    let refused = Arc::new(AtomicBool::new(false));
    let progress = PtyWriterProgress::with_input_seen(Some(refused.clone()));
    let oversized = vec![0u8; MAX_PTY_INPUT_MESSAGE_BYTES + 1];
    assert!(try_queue_pty_input((&tx, &queued), &progress, oversized, true).is_err());
    assert!(!refused.load(Ordering::SeqCst), "a message over the cap never reached admission");
    for _ in tx.len()..PTY_INPUT_QUEUE_CAPACITY {
        tx.try_send(vec![b'x']).expect("fill bounded input queue");
    }
    assert!(try_queue_pty_input((&tx, &queued), &progress, vec![b'y'], true).is_err());
    assert!(refused.load(Ordering::SeqCst), "a full queue refuses after admission marked it");
}

/// Through a real PTY: the parser's DA/DSR replies, which go through the reply spool, leave the
/// pane's `input_seen` clear, so a shell integration can still latch after the terminal answered
/// its startup queries; input through `send_input_nonblocking` or a `PtyInputSender` sets it.
#[test]
fn only_real_input_marks_a_live_pty_as_typed_into() {
    #[cfg(windows)]
    let _live_pty_guard = crate::pty::pty_test_support::lock_live_pty_test();
    for through_sender in [false, true] {
        let seen = Arc::new(AtomicBool::new(false));
        let pty = PtyHandle::spawn_default_shell(
            80,
            24,
            ShellSpawnOpts {
                clean_e2e: true,
                input_seen: Some(seen.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        pty.reply_sender().send(b"\x1b[?62;22c\x1b[1;1R".to_vec()).unwrap();
        pty.send_draft_nonblocking(b"echo draft".to_vec()).unwrap();
        assert!(!seen.load(Ordering::SeqCst), "replies and a draft are not typing");
        if through_sender {
            // When: `through_sender`, the cloneable sender the VT worker and dispatchers hold.
            pty.input_sender().send(b"\r".to_vec()).unwrap();
        } else {
            pty.send_input_nonblocking(b"\r".to_vec()).unwrap();
        }
        assert!(seen.load(Ordering::SeqCst), "through_sender {through_sender}: input is typing");
    }
}

/// A delivery receipt is the admitted count, compared whole: past the old u32 boundary a later
/// ticket is still later, a message is `Queued` until the writer completes it, then `Written`, and
/// a writer that stops first makes it `Failed`.
#[test]
fn delivery_receipts_cross_the_old_u32_boundary() {
    let progress = Arc::new(PtyWriterProgress::new());
    let below = u64::from(u32::MAX) - 1;
    progress.input_admitted.store(below, Ordering::Release);
    progress.input_completed.store(below, Ordering::Release);
    let probe = crate::pty::PtyDeliveryProbe::for_tests(Arc::clone(&progress));
    let (tx, _rx) = crossbeam_channel::bounded::<Vec<u8>>(PTY_INPUT_QUEUE_CAPACITY);
    for _ in 0..3 {
        try_queue_pty_input((&tx, &AtomicUsize::new(0)), &progress, b"a".to_vec(), true).unwrap();
    }
    let ticket = crate::pty::last_admitted_ticket();
    assert_eq!(ticket, below + 3, "the ticket kept counting past u32::MAX");
    assert_eq!(probe.status(ticket), PtyDelivery::Queued);
    progress.input_completed.fetch_add(2, Ordering::Release);
    assert_eq!(probe.status(ticket), PtyDelivery::Queued, "one message is still pending");
    progress.input_completed.fetch_add(1, Ordering::Release);
    assert_eq!(probe.status(ticket), PtyDelivery::Written);
    try_queue_pty_input((&tx, &AtomicUsize::new(0)), &progress, b"b".to_vec(), true).unwrap();
    let later = crate::pty::last_admitted_ticket();
    progress.record_terminal(TERMINAL_FAILED_WRITE);
    assert_eq!(probe.status(later), PtyDelivery::Failed);
    assert_eq!(probe.status(ticket), PtyDelivery::Written, "what was written stays written");
}

/// Each sender's ticket is its own admission's ordinal: a later admission on another thread does
/// not move it, so another producer's failure cannot mark this write `Failed`.
#[test]
fn a_ticket_belongs_to_the_admitting_write() {
    let progress = Arc::new(PtyWriterProgress::new());
    let probe = crate::pty::PtyDeliveryProbe::for_tests(Arc::clone(&progress));
    let (tx, _rx) = crossbeam_channel::bounded::<Vec<u8>>(PTY_INPUT_QUEUE_CAPACITY);
    try_queue_pty_input((&tx, &AtomicUsize::new(0)), &progress, b"a".to_vec(), true).unwrap();
    let mine = crate::pty::last_admitted_ticket();
    // Another producer admits B before this sender reads anything more.
    let other = {
        let (progress, tx) = (Arc::clone(&progress), tx.clone());
        std::thread::spawn(move || {
            try_queue_pty_input((&tx, &AtomicUsize::new(0)), &progress, b"b".to_vec(), true)
                .unwrap();
            crate::pty::last_admitted_ticket()
        })
        .join()
        .unwrap()
    };
    assert_eq!((mine, other), (1, 2));
    assert_eq!(crate::pty::last_admitted_ticket(), mine, "another thread's admission left it");
    // A completes; the writer then fails on B. A is still written.
    progress.input_completed.fetch_add(1, Ordering::Release);
    progress.record_terminal(TERMINAL_FAILED_WRITE);
    assert_eq!(probe.status(mine), PtyDelivery::Written);
    assert_eq!(probe.status(other), PtyDelivery::Failed);
}

/// A write that completes while the writer stops, between `status`'s two loads, is `Written`: the
/// end is read first, so a completion published before it is never missed.
#[test]
fn a_completion_racing_the_writer_end_is_written() {
    let progress = Arc::new(PtyWriterProgress::new());
    let probe = crate::pty::PtyDeliveryProbe::for_tests(Arc::clone(&progress));
    let (tx, _rx) = crossbeam_channel::bounded::<Vec<u8>>(PTY_INPUT_QUEUE_CAPACITY);
    try_queue_pty_input((&tx, &AtomicUsize::new(0)), &progress, b"a".to_vec(), true).unwrap();
    let ticket = crate::pty::last_admitted_ticket();
    let racing = Arc::clone(&progress);
    crate::pty::BETWEEN_STATUS_LOADS.set(Some(Box::new(move || {
        // The writer flushes the message and then records its end.
        racing.input_completed.fetch_add(1, Ordering::Release);
        racing.record_terminal(TERMINAL_FAILED_WRITE);
    })));
    let status = probe.status(ticket);
    crate::pty::BETWEEN_STATUS_LOADS.set(None);
    assert_eq!(status, PtyDelivery::Written);
}
