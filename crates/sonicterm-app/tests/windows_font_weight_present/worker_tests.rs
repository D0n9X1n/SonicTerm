use super::*;

// Holding negotiation cannot block submission or result polling, and a second request is returned intact.
#[test]
fn held_request_keeps_owner_nonblocking_and_admission_bounded() {
    let (started, observed) = mpsc::sync_channel(1);
    let (release, held) = mpsc::sync_channel(1);
    let mut worker = Worker::new(
        move |request: Box<u32>| {
            started.send(()).unwrap();
            held.recv_timeout(Duration::from_secs(5)).unwrap();
            request
        },
        || {},
    )
    .unwrap();
    worker.submit(Box::new(7)).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = Box::new(8);
    let pointer = std::ptr::from_ref(second.as_ref());
    let rejected = worker.submit(second).unwrap_err();
    assert_eq!(std::ptr::from_ref(rejected.as_ref()), pointer);
    assert!(worker.try_result().unwrap().is_none());
    release.send(()).unwrap();
    worker.finish(Instant::now() + Duration::from_secs(5)).unwrap();
}

// Callback failure must release the held request, dispose its result on the owner, and preserve the panic payload.
#[test]
fn callback_panic_cleans_worker_before_resuming_original_payload() {
    let owner = thread::current().id();
    let (dropped, disposed) = mpsc::sync_channel(1);
    let (started, observed) = mpsc::sync_channel(1);
    let (release, held) = mpsc::sync_channel::<()>(1);
    let mut worker = Worker::new(
        move |()| {
            started.send(()).unwrap();
            assert!(held.recv_timeout(Duration::from_secs(5)).is_err());
            Witness(dropped.clone())
        },
        || {},
    )
    .unwrap();
    worker.submit(()).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = finish_run::<()>(Err(Box::new(73_u32)), || {
            drop(release);
            worker.finish(Instant::now() + Duration::from_secs(5))
        });
    }))
    .unwrap_err();
    assert_eq!(panic.downcast_ref::<u32>(), Some(&73));
    assert!(worker.thread.is_none());
    assert_eq!(disposed.recv_timeout(Duration::from_secs(5)).unwrap(), owner);
}

// Cleanup errors must remain visible without replacing a callback panic or a normal run error.
#[test]
fn cleanup_failure_preserves_original_run_result() {
    let cleaned = std::cell::Cell::new(false);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = finish_run::<()>(Err(Box::new(73_u32)), || {
            cleaned.set(true);
            Err("cleanup failed".into())
        });
    }))
    .unwrap_err();
    assert!(cleaned.get());
    assert_eq!(panic.downcast_ref::<u32>(), Some(&73));
    assert_eq!(
        finish_run(Ok(Err::<(), _>("run failed")), || Err("cleanup failed".into())),
        (Err("run failed"), Err("cleanup failed".into()))
    );
}

struct Witness(SyncSender<thread::ThreadId>);

// Lifecycle: report the disposal thread without retaining the worker or its result channel.
impl Drop for Witness {
    fn drop(&mut self) {
        let _ = self.0.send(thread::current().id());
    }
}

// Shutdown drains a queued owned failure on the caller and joins only after the worker has finished.
#[test]
fn cleanup_keeps_owned_failure_on_the_owner_thread() {
    let owner = thread::current().id();
    let (dropped, observed) = mpsc::sync_channel(1);
    let (notified, ready) = mpsc::sync_channel(1);
    let mut worker = Worker::new(
        move |()| Err::<(), _>(Witness(dropped.clone())),
        move || {
            notified.send(()).unwrap();
        },
    )
    .unwrap();
    worker.submit(()).unwrap();
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.finish(Instant::now() + Duration::from_secs(5)).unwrap();
    assert_eq!(observed.recv_timeout(Duration::from_secs(5)).unwrap(), owner);
}

// A cleanup deadline leaves a held worker observable; release then allows the same owner to join it.
#[test]
fn cleanup_timeout_does_not_replace_a_held_worker() {
    let (started, observed) = mpsc::sync_channel(1);
    let (release, held) = mpsc::sync_channel(1);
    let mut worker = Worker::new(
        move |()| {
            started.send(()).unwrap();
            held.recv_timeout(Duration::from_secs(5)).unwrap();
        },
        || {},
    )
    .unwrap();
    worker.submit(()).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(worker.finish(Instant::now()).is_err());
    assert!(worker.thread.is_some());
    assert!(worker.submit(()).is_err());
    release.send(()).unwrap();
    worker.finish(Instant::now() + Duration::from_secs(5)).unwrap();
}
