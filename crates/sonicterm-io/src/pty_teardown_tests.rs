use super::*;
#[cfg(windows)]
use crate::pty::pty_test_support::lock_live_pty_test;
#[cfg(windows)]
use crossbeam_channel::bounded;
#[cfg(windows)]
use portable_pty::{ChildKiller, ExitStatus};

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TeardownStep {
    SignalCancel,
    CancelIo,
    TerminateChild,
    FinishIo,
    CloseMaster,
    ReapChild,
}

#[cfg(windows)]
struct RecordingTeardown {
    steps: Vec<TeardownStep>,
}

#[cfg(windows)]
impl PtyTeardownOps for RecordingTeardown {
    fn signal_cancel(&mut self) {
        self.steps.push(TeardownStep::SignalCancel);
    }

    fn cancel_io(&mut self) {
        self.steps.push(TeardownStep::CancelIo);
    }

    fn terminate_child(&mut self) {
        self.steps.push(TeardownStep::TerminateChild);
    }

    fn finish_io(&mut self) {
        self.steps.push(TeardownStep::FinishIo);
    }

    fn close_master(&mut self) {
        self.steps.push(TeardownStep::CloseMaster);
    }

    fn reap_child(&mut self) {
        self.steps.push(TeardownStep::ReapChild);
    }
}

/// Moving native custody only publishes closing; the owned payload, not the consumed handle, performs teardown.
#[test]
fn into_teardown_moves_native_custody_without_closing_child() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/D".into(), "/Q".into()]);
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-s".into()]);
    let pty =
        PtyHandle::spawn_with_args(program, &args, 80, 24).expect("spawn owned transfer fixture");
    let input = pty.input_sender();
    let child = pty.teardown.as_ref().unwrap().child.clone();
    // Observe the same unreaped native child directly: the public probe becomes cache-only after closing is published.
    let running = || {
        let child = child.lock();
        let native = child.child.as_ref().unwrap().value.as_ref().unwrap();
        #[cfg(windows)]
        {
            use windows::Win32::{Foundation::HANDLE, System::Threading::GetExitCodeProcess};
            let mut code = 0u32;
            // SAFETY: native retains this exact child process handle throughout both observations; no PID reopen occurs.
            unsafe { GetExitCodeProcess(HANDLE(native.as_raw_handle().unwrap()), &mut code) }
                .unwrap();
            code == 259
        }
        #[cfg(unix)]
        {
            !unix_child_exit_pending(native.process_id().unwrap()).unwrap().pending
        }
    };
    let running_before = running();
    let mut teardown = pty.into_teardown();
    let running_after_transfer = running();
    let closing_after_transfer = input.is_closing();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut result = teardown.run_remaining();
    while !teardown.is_complete() && Instant::now() < deadline {
        result = teardown.run_remaining();
    }
    // The fixture is settled before assertions so a failed transfer contract cannot strand its native processes.
    assert!(
        running_before && running_after_transfer,
        "extracting native custody performed premature teardown"
    );
    assert!(closing_after_transfer, "retained input sender must observe intentional closing");
    assert_eq!(result, sonicterm_types::ReapResult::Settled);
    assert!(teardown.is_complete());
    assert!(teardown.workers_finished(), "whole-task completion includes every native worker join");
}

/// Refusing the first drain spawn keeps the actual ConPTY master owned; a retry starts its workers and settles exactly once.
#[cfg(windows)]
#[test]
fn teardown_drain_spawn_failure_keeps_master_and_permits_for_retry() {
    struct RefuseDrainOnce(std::sync::atomic::AtomicBool);
    impl sonicterm_types::lifecycle::NativeWorkerSpawner for RefuseDrainOnce {
        fn spawn(
            &self,
            slot: usize,
            name: &'static str,
            work: Box<dyn FnOnce() + Send>,
        ) -> std::io::Result<thread::JoinHandle<()>> {
            if slot == 1 && self.0.swap(false, Ordering::SeqCst) {
                return Err(std::io::Error::other("controlled drain refusal"));
            }
            thread::Builder::new().name(name.into()).spawn(work)
        }
    }
    struct Permit(Arc<AtomicUsize>);
    // Lifecycle: Permit decrements the exact retained native count when its wrapper closes.
    impl Drop for Permit {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _live_pty_guard = lock_live_pty_test();
    let mut pty =
        PtyHandle::spawn_with_args("cmd.exe", &["/D".into(), "/Q".into()], 80, 24).unwrap();
    let live = Arc::new(AtomicUsize::new(PTY_NATIVE_HANDLE_DEMAND));
    pty.install_native_permits(
        (0..PTY_NATIVE_HANDLE_DEMAND)
            .map(|_| Box::new(Permit(live.clone())) as sonicterm_types::lifecycle::NativePermit)
            .collect(),
    )
    .unwrap();
    let mut teardown = pty.into_teardown();
    teardown
        .set_worker_spawner(Arc::new(RefuseDrainOnce(std::sync::atomic::AtomicBool::new(true))));
    let completion = teardown.completion();
    let first = teardown.run_remaining();
    let still_incomplete = !completion.is_complete();
    let held_after_failure = live.load(Ordering::SeqCst);
    let until = Instant::now() + Duration::from_secs(15);
    while !completion.is_complete() && Instant::now() < until {
        let _ = teardown.run_remaining();
    }
    teardown.join_finished_workers();
    assert_eq!(
        first,
        sonicterm_types::ReapResult::Failed,
        "spawn refusal is a retryable failure, not wait expiry"
    );
    assert!(still_incomplete);
    assert!(held_after_failure > 0, "failed drain destroyed the undrained native master");
    assert!(completion.is_complete() && completion.workers_finished());
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

/// Windows must inspect the retained process handle and surface denied termination, without trusting a masking Child::kill.
#[cfg(windows)]
#[test]
fn teardown_checked_termination_reports_access_denied_without_reaping() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::{
        Foundation::{DuplicateHandle, DUPLICATE_HANDLE_OPTIONS, HANDLE},
        System::Threading::{GetCurrentProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    #[derive(Debug)]
    struct QueryOnlyChild(OwnedHandle);
    impl ChildKiller for QueryOnlyChild {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            panic!("no killer duplication")
        }
    }
    impl Child for QueryOnlyChild {
        fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
            panic!("termination must not reap")
        }
        fn wait(&mut self) -> std::io::Result<ExitStatus> {
            panic!("termination must not wait")
        }
        fn process_id(&self) -> Option<u32> {
            None
        }
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            Some(self.0.as_raw_handle())
        }
    }
    let _live_pty_guard = lock_live_pty_test();
    let pty = PtyHandle::spawn_with_args("cmd.exe", &["/D".into(), "/Q".into()], 80, 24).unwrap();
    let mut duplicate = HANDLE::default();
    {
        let child = pty.teardown.as_ref().unwrap().child.lock();
        let handle = child.child.as_ref().unwrap().value.as_ref().unwrap().as_raw_handle().unwrap();
        // SAFETY: child retains the live fixture handle; only its query-only duplicate is returned, never reopened by PID.
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                HANDLE(handle),
                GetCurrentProcess(),
                &mut duplicate,
                PROCESS_QUERY_LIMITED_INFORMATION.0,
                false,
                DUPLICATE_HANDLE_OPTIONS(0),
            )
            .unwrap();
        }
    }
    let query_only =
        // SAFETY: DuplicateHandle created this owned fixture handle and no other wrapper owns the duplicate.
        unsafe { OwnedHandle::from_raw_handle(duplicate.0) };
    let mut child = ChildState::new(Box::new(QueryOnlyChild(query_only)), None);
    let result = terminate_child_without_reap(&mut child);
    drop(child);
    drop(pty);
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
}

/// Preservation: a slotless cancel keeps its duplicate token while its real worker is stalled and joins after release.
#[cfg(windows)]
#[test]
fn teardown_slotless_cancel_keeps_duplicate_tokens_until_worker_exit() {
    struct GatedCancels {
        release: Receiver<()>,
        started: Sender<()>,
    }
    impl sonicterm_types::lifecycle::NativeWorkerSpawner for GatedCancels {
        fn spawn(
            &self,
            slot: usize,
            name: &'static str,
            work: Box<dyn FnOnce() + Send>,
        ) -> std::io::Result<thread::JoinHandle<()>> {
            let release = self.release.clone();
            let started = self.started.clone();
            thread::Builder::new().name(name.into()).spawn(move || {
                if slot >= 3 {
                    let _ = started.try_send(());
                    release.recv_timeout(Duration::from_secs(10)).expect("bounded cancel gate");
                }
                work();
            })
        }
    }
    struct DuplicateToken(Arc<AtomicUsize>);
    // Lifecycle: DuplicateToken decrements the tracked slotless duplicate after native close.
    impl Drop for DuplicateToken {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _live_pty_guard = lock_live_pty_test();
    let pty = PtyHandle::spawn_with_args("cmd.exe", &["/D".into(), "/Q".into()], 80, 24).unwrap();
    let mut teardown = pty.into_teardown();
    let live = Arc::new(AtomicUsize::new(0));
    let tracked = live.clone();
    teardown.set_fallback_duplicate_tracker(Arc::new(move || {
        tracked.fetch_add(1, Ordering::SeqCst);
        Box::new(DuplicateToken(tracked.clone()))
    }));
    let (release_tx, release_rx) = bounded(2);
    let (started_tx, started_rx) = bounded(2);
    teardown
        .set_worker_spawner(Arc::new(GatedCancels { release: release_rx, started: started_tx }));
    // Run the cancellation phase before publishing the normal IO cancel tokens so both fixture threads remain live.
    teardown.cancel_io();
    let started = started_rx.try_iter().count();
    let held = live.load(Ordering::SeqCst);
    let (held_handles, joined_early) = {
        let workers = teardown.completion.0.workers.lock();
        (
            workers.native[2..].iter().filter(|worker| worker.handle.is_some()).count(),
            workers.all_joined(),
        )
    };
    let _ = release_tx.try_send(());
    let _ = release_tx.try_send(());
    let until = Instant::now() + Duration::from_secs(15);
    let mut result = teardown.run_remaining();
    while !result.releases_charge() && Instant::now() < until {
        result = teardown.run_remaining();
    }
    assert_eq!((started, held, held_handles), (2, 2, 2));
    assert!(!joined_early);
    assert_eq!(result, sonicterm_types::ReapResult::Settled);
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

/// A real drain read failure cannot publish DRAIN_PHASE or claim whole native settlement.
#[cfg(windows)]
#[test]
fn teardown_unexpected_drain_read_error_stays_failed() {
    struct FailingDrain;
    impl Read for FailingDrain {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "controlled drain read failure",
            ))
        }
    }
    let _live_pty_guard = lock_live_pty_test();
    let pty = PtyHandle::spawn_with_args("cmd.exe", &["/D".into(), "/Q".into()], 80, 24).unwrap();
    let mut teardown = pty.into_teardown();
    {
        let mut reader = teardown.conpty_drain_reader.lock();
        let native = reader.as_mut().unwrap();
        // Keep the same phase/permit wrapper while replacing only its reader operation.
        native.value = Some(Box::new(FailingDrain));
    }
    let result = teardown.run_remaining();
    let drain_complete = teardown.completion.phase_complete(DRAIN_PHASE);
    // Restore a reader so a correctly retained master can be cleaned up before the assertion.
    let reader = teardown
        .master
        .lock()
        .as_ref()
        .and_then(|master| master.value.as_ref())
        .and_then(|master| master.try_clone_reader().ok());
    if let Some(reader) = reader {
        let (mut native, _) = NativeValue::new(reader);
        native.phase = Some((teardown.completion.clone(), DRAIN_PHASE));
        *teardown.conpty_drain_reader.lock() = Some(native);
        teardown.completion.0.workers.lock().native[0] = NativeWorker::default();
        let _ = teardown.run_remaining();
    }
    assert_eq!(
        result,
        sonicterm_types::ReapResult::Failed,
        "unexpected drain read failure was credited as settlement"
    );
    assert!(!drain_complete, "failed drain published phase success");
}

/// CancelSynchronousIo errors other than no-pending-IO must not publish successful cancellation.
#[cfg(windows)]
#[test]
fn teardown_cancel_wrong_handle_type_stays_failed() {
    use std::os::windows::io::BorrowedHandle;
    let _live_pty_guard = lock_live_pty_test();
    let pty = PtyHandle::spawn_with_args("cmd.exe", &["/D".into(), "/Q".into()], 80, 24).unwrap();
    let mut teardown = pty.into_teardown();
    let duplicate = {
        let child = teardown.child.lock();
        let raw = child.child.as_ref().unwrap().value.as_ref().unwrap().as_raw_handle().unwrap();
        let borrowed =
            // SAFETY: child retains raw for this scope; duplicating only borrows that live fixture handle.
            unsafe { BorrowedHandle::borrow_raw(raw) };
        borrowed.try_clone_to_owned().unwrap()
    };
    let (mut native, _) = NativeValue::new(duplicate);
    native.phase = Some((teardown.completion.clone(), READER_CANCEL_PHASE));
    *teardown.cancel_targets[0].lock() = Some(native);
    teardown.cancel_io();
    let phase_completed = teardown.completion.phase_complete(READER_CANCEL_PHASE);
    let phase_failed = teardown.completion.0.workers.lock().native[2].failed;
    // Retry normal cancellation on the actual IO thread before destroying the real fixture.
    teardown.completion.0.workers.lock().native[2] = NativeWorker::default();
    let _ = teardown.run_remaining();
    assert!(
        phase_failed && !phase_completed,
        "invalid cancellation target was credited as phase success"
    );
}

#[cfg(windows)]
struct CloseGatedReader {
    reader: Box<dyn Read + Send>,
    close_started: Receiver<()>,
    drained: Sender<()>,
    synchronized: bool,
}

#[cfg(windows)]
impl std::io::Read for CloseGatedReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if !self.synchronized {
            self.close_started
                .recv_timeout(Duration::from_secs(1))
                .map_err(std::io::Error::other)?;
            self.drained.send_timeout((), Duration::from_secs(1)).map_err(std::io::Error::other)?;
            self.synchronized = true;
        }
        self.reader.read(buffer)
    }
}

/// Production teardown must run drain and close concurrently; neither side can pass the bounded rendezvous alone.
#[cfg(windows)]
#[test]
fn conpty_close_runs_while_output_reader_is_draining() {
    struct RendezvousSpawner {
        close_started: Sender<()>,
        drained: Receiver<()>,
        overlapped: Arc<AtomicBool>,
    }
    impl sonicterm_types::lifecycle::NativeWorkerSpawner for RendezvousSpawner {
        fn spawn(
            &self,
            slot: usize,
            name: &'static str,
            work: Box<dyn FnOnce() + Send>,
        ) -> std::io::Result<thread::JoinHandle<()>> {
            let close_started = self.close_started.clone();
            let drained = self.drained.clone();
            let overlapped = self.overlapped.clone();
            thread::Builder::new().name(name.into()).spawn(move || {
                if slot == 2 {
                    let met = close_started.send_timeout((), Duration::from_secs(1)).is_ok()
                        && drained.recv_timeout(Duration::from_secs(1)).is_ok();
                    overlapped.store(met, Ordering::SeqCst);
                }
                // Always invoke native cleanup, even if the rendezvous fails, before the test asserts.
                work();
            })
        }
    }
    let _live_pty_guard = lock_live_pty_test();
    let pty = PtyHandle::spawn_with_args("cmd.exe", &["/D".into(), "/Q".into()], 80, 24)
        .expect("spawn production close fixture");
    let mut teardown = pty.into_teardown();
    let (close_started_tx, close_started_rx) = bounded(1);
    let (drained_tx, drained_rx) = bounded(1);
    let overlapped = Arc::new(AtomicBool::new(false));
    {
        let mut reader = teardown.conpty_drain_reader.lock();
        // Keep draining the actual pipe after the rendezvous; synthetic EOF would change native close behavior.
        let native = reader.as_mut().expect("owned drain reader");
        native.value = Some(Box::new(CloseGatedReader {
            reader: native.value.take().expect("native drain reader"),
            close_started: close_started_rx,
            drained: drained_tx,
            synchronized: false,
        }));
    }
    teardown.set_worker_spawner(Arc::new(RendezvousSpawner {
        close_started: close_started_tx,
        drained: drained_rx,
        overlapped: overlapped.clone(),
    }));
    let result = teardown.run_remaining();
    teardown.join_finished_workers();
    assert!(overlapped.load(Ordering::SeqCst), "production drain and close did not overlap");
    assert_eq!(result, sonicterm_types::ReapResult::Settled);
    assert!(teardown.completion.is_complete());
    assert!(teardown.completion.0.workers.lock().all_joined());
}

#[cfg(windows)]
#[test]
fn teardown_cancels_and_finishes_io_before_closing_conpty_master() {
    let mut teardown = RecordingTeardown { steps: Vec::new() };

    run_pty_teardown(&mut teardown);

    assert_eq!(
        teardown.steps,
        [
            TeardownStep::SignalCancel,
            TeardownStep::CancelIo,
            TeardownStep::TerminateChild,
            TeardownStep::FinishIo,
            TeardownStep::CloseMaster,
            TeardownStep::ReapChild,
        ]
    );
}

#[cfg(windows)]
#[test]
fn dropping_live_windows_pty_terminates_native_io_threads() {
    let _live_pty_guard = lock_live_pty_test();
    let baseline = active_pty_io_threads();
    let args = vec![
        "/D".to_string(),
        "/Q".to_string(),
        "/C".to_string(),
        "ping -t 127.0.0.1 >NUL".to_string(),
    ];
    let pty = PtyHandle::spawn_with_args("cmd.exe", &args, 80, 24).expect("spawn Windows PTY");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while active_pty_io_threads() < baseline + 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(active_pty_io_threads(), baseline + 2);

    drop(pty);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while active_pty_io_threads() != baseline && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(active_pty_io_threads(), baseline);
}

// ---------------------------------------------------------------------------
// Raw seam privacy
//
// The rule: "Make raw bounded seams private where bypass
// would invalidate invariants (for example raw PTY senders). Expose typed
// operations that enforce reservation, ordering, and error ownership."
//
// This is that seam, and these are the invariants bypass invalidated.
// ---------------------------------------------------------------------------

#[test]
fn intentional_teardown_cancels_sender_without_a_writer_failure_classification() {
    // Dropping the real PTY must publish intentional closure before a retained sender can report rejection.
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let pty = PtyHandle::spawn_default_shell(
        80,
        24,
        ShellSpawnOpts { clean_e2e: true, ..Default::default() },
    )
    .unwrap();
    let sender = pty.input_sender();
    let replies = pty.reply_sender();
    assert!(!sender.is_closing());
    drop(pty);
    assert!(sender.is_closing());
    assert_eq!(replies.send(vec![1]).unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
}
