use super::*;
#[cfg(windows)]
use crossbeam_channel::bounded;

/// A done signal precedes capture destruction; finish must retain a still-running JoinHandle instead of blocking or detaching.
#[test]
fn io_finish_retains_a_thread_after_done_until_it_really_exits() {
    let (done_tx, done_rx) = crossbeam_channel::bounded(1);
    let (release_tx, release_rx) = crossbeam_channel::bounded(1);
    let handle = thread::spawn(move || {
        let _ = done_tx.send(());
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
    });
    let mut worker = PtyIoThread { handle: Some(handle), done: done_rx, failed: false };
    worker
        .done
        .recv_timeout(Duration::from_secs(1))
        .expect("worker reached completion-report boundary");
    let began = Instant::now();
    worker.finish("controlled unfinished IO");
    let elapsed = began.elapsed();
    let retained = worker.handle.is_some();
    let _ = release_tx.try_send(());
    let until = Instant::now() + Duration::from_secs(2);
    while worker.handle.as_ref().is_some_and(|handle| !handle.is_finished())
        && Instant::now() < until
    {
        thread::yield_now();
    }
    worker.finish("controlled released IO");
    assert!(
        elapsed < Duration::from_secs(2),
        "done notification allowed an unfinished join to hold the caller"
    );
    assert!(retained, "expired IO wait detached its native custody");
    assert!(worker.handle.is_none(), "finished IO worker was not joined");
}

/// Spawn refusal leaves the native value in its shared recovery slot; only a started worker can consume it.
#[test]
fn native_worker_spawn_failure_keeps_payload_for_retry() {
    struct RefuseOnce(std::sync::atomic::AtomicBool);
    impl sonicterm_types::lifecycle::NativeWorkerSpawner for RefuseOnce {
        fn spawn(
            &self,
            _slot: usize,
            name: &'static str,
            work: Box<dyn FnOnce() + Send>,
        ) -> std::io::Result<thread::JoinHandle<()>> {
            if self.0.swap(false, Ordering::SeqCst) {
                return Err(std::io::Error::other("controlled spawn refusal"));
            }
            thread::Builder::new().name(name.into()).spawn(work)
        }
    }
    let native = Arc::new(Mutex::new(Some(41usize)));
    let phase = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut worker = NativeWorker::default();
    let spawner = RefuseOnce(std::sync::atomic::AtomicBool::new(true));
    assert!(worker
        .start(&spawner, 1, "native-test", native.clone(), phase.clone(), |_| true)
        .is_err());
    assert_eq!(*native.lock(), Some(41), "refused thread must not own sole native recovery state");
    assert!(!phase.load(Ordering::Acquire));
    worker.start(&spawner, 1, "native-test", native.clone(), phase.clone(), |_| true).unwrap();
    assert!(worker.wait_until(Instant::now() + Duration::from_secs(2)));
    assert!(phase.load(Ordering::Acquire));
    assert!(native.lock().is_none());
}

/// An expired native wait preserves its JoinHandle and never starts a duplicate worker for the same phase.
#[test]
fn native_worker_expiry_keeps_handle_and_prevents_duplicate_start() {
    let native = Arc::new(Mutex::new(Some(())));
    let phase = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (release_tx, release_rx) = crossbeam_channel::bounded(1);
    let mut worker = NativeWorker::default();
    worker
        .start(&ThreadWorkerSpawner, 1, "native-gated", native.clone(), phase.clone(), move |_| {
            release_rx.recv_timeout(Duration::from_secs(3)).is_ok()
        })
        .unwrap();
    let early = worker.wait_until(Instant::now() + Duration::from_millis(20));
    let kept = worker.handle.is_some();
    let duplicate =
        worker.start(&ThreadWorkerSpawner, 1, "native-duplicate", native, phase.clone(), |_| {
            panic!("duplicate worker ran")
        });
    let _ = release_tx.try_send(());
    let complete = worker.wait_until(Instant::now() + Duration::from_secs(2));
    assert!(!early && kept);
    assert!(duplicate.is_ok(), "already-started phase is reused rather than resubmitted");
    assert!(complete && phase.load(Ordering::Acquire));
}

/// A started native worker that reports phase failure must not be treated as successfully joined work.
#[test]
fn native_worker_false_result_is_failure_not_success() {
    let native = Arc::new(Mutex::new(Some(())));
    let phase = Arc::new(AtomicBool::new(false));
    let mut worker = NativeWorker::default();
    worker
        .start(&ThreadWorkerSpawner, 1, "native-failed", native, phase.clone(), |_| false)
        .unwrap();
    assert!(
        !worker.wait_until(Instant::now() + Duration::from_secs(2)),
        "failed phase was treated as successful worker completion"
    );
    assert!(worker.failed);
    assert!(!phase.load(Ordering::Acquire));
}

/// A fake cancel that blocks on `gate` when one is given, then reports its target's name on `ran`.
#[cfg(windows)]
struct GatedCancel {
    gate: Option<Receiver<()>>,
    ran: Sender<&'static str>,
}

#[cfg(windows)]
fn run_gated_cancel(name: &'static str, target: GatedCancel) -> bool {
    if let Some(gate) = target.gate {
        if gate.recv_timeout(Duration::from_secs(10)).is_err() {
            return false;
        }
    }
    let _ = target.ran.send(name);
    true
}

/// A cancel that does not return must not hold teardown past its limit or delay the other thread's cancel, and it
/// still finishes on its own helper once it unblocks.
#[cfg(windows)]
#[test]
fn stalled_cancel_does_not_hold_teardown_past_its_limit() {
    const LIMIT: Duration = Duration::from_millis(200);
    // Slack for a loaded host: the assertions are about order and the limit, not speed.
    const SLACK: Duration = Duration::from_secs(5);
    let (gate_tx, gate_rx) = bounded::<()>(1);
    let (ran_tx, ran_rx) = bounded::<&'static str>(2);
    // The blocked target comes first, so cancels run inline would not reach the fast one before the gate opens.
    let targets = [
        (
            "blocked",
            Arc::new(Mutex::new(Some(GatedCancel { gate: Some(gate_rx), ran: ran_tx.clone() }))),
        ),
        ("fast", Arc::new(Mutex::new(Some(GatedCancel { gate: None, ran: ran_tx })))),
    ];
    let (outcome_tx, outcome_rx) = bounded(1);
    let caller = thread::spawn(move || {
        let started = Instant::now();
        let mut workers = [NativeWorker::default(), NativeWorker::default()];
        let all_returned = cancel_io_within(
            &targets,
            &mut workers,
            &ThreadWorkerSpawner,
            started + LIMIT,
            run_gated_cancel,
        )
        .unwrap();
        let _ = outcome_tx.send((all_returned, started.elapsed()));
        for worker in &mut workers {
            assert!(worker.wait_until(Instant::now() + SLACK));
        }
    });

    let fast = ran_rx.recv_timeout(SLACK);
    let outcome = outcome_rx.recv_timeout(LIMIT + SLACK);
    // Open the gate before asserting, so a failed assertion does not leave the blocked cancel waiting.
    let _ = gate_tx.send(());

    assert_eq!(fast, Ok("fast"), "the blocked cancel delayed the other thread's cancel");
    let (all_returned, elapsed) = outcome.expect("cancel_io_within must return within its limit");
    assert!(!all_returned, "a cancel still running at the limit must be reported");
    assert!(
        elapsed >= LIMIT,
        "returned before the limit while a cancel was still running: {elapsed:?}"
    );
    assert!(elapsed <= LIMIT + SLACK, "returned later than the limit plus slack: {elapsed:?}");
    assert_eq!(
        ran_rx.recv_timeout(SLACK),
        Ok("blocked"),
        "the late cancel must still finish on its own helper"
    );
    caller.join().expect("caller thread");
}

/// Production cancellation reaches live threads through duplicated handles and returns promptly, and a thread that
/// has been joined yields no cancel target.
#[cfg(windows)]
#[test]
fn cancel_reaches_live_threads_through_duplicated_handles() {
    let mut io_threads = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..2 {
        let (release_tx, release_rx) = bounded::<()>(1);
        let (done_tx, done_rx) = bounded::<()>(1);
        // Each thread parks outside any I/O, so its cancel finds nothing to abort and returns at once.
        let handle = thread::spawn(move || {
            let _ = release_rx.recv();
            let _ = done_tx.send(());
        });
        io_threads.push(PtyIoThread { handle: Some(handle), done: done_rx, failed: false });
        releases.push(release_tx);
    }
    let targets: Vec<_> = io_threads
        .iter()
        .zip(["first", "second"])
        .map(|(io_thread, name)| {
            (
                name,
                Arc::new(Mutex::new(Some(
                    io_thread.cancel_target().expect("a live thread yields a duplicated handle"),
                ))),
            )
        })
        .collect();

    let mut workers = [NativeWorker::default(), NativeWorker::default()];
    assert!(
        cancel_io_within(
            &targets,
            &mut workers,
            &ThreadWorkerSpawner,
            Instant::now() + Duration::from_secs(5),
            cancel_thread_io
        )
        .unwrap(),
        "cancelling threads that are not in I/O must return within the limit"
    );

    for release in releases {
        let _ = release.send(());
    }
    for io_thread in &mut io_threads {
        io_thread.done.recv_timeout(Duration::from_secs(5)).expect("a released thread must finish");
        io_thread.handle.take().expect("live join handle").join().expect("thread join");
        assert!(io_thread.cancel_target().is_none(), "a joined thread has nothing to cancel");
    }
}
