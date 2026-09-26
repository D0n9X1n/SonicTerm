use super::*;
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::thread::{self, ThreadId};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);
const PROCESS_WAIT: Duration = Duration::from_secs(15);

#[derive(Debug)]
struct DropWitness(Sender<ThreadId>);

// Lifecycle: `DropWitness` reports its disposal thread without retaining worker or App state.
impl Drop for DropWitness {
    fn drop(&mut self) {
        let _ = self.0.send(thread::current().id());
    }
}

/// An unused owner never spawns: its captured processor is disposed on the caller.
#[test]
fn unused_worker_is_lazy() {
    let (dropped, observed) = mpsc::channel();
    let witness = DropWitness(dropped);
    let mut worker = RequestWorker::new(
        move |()| {
            let _keep = &witness;
        },
        |_| {},
    );
    assert_eq!(worker.in_flight(), None);
    assert_eq!(worker.try_result(), Ok(None));
    drop(worker);
    assert_eq!(observed.recv_timeout(WAIT).unwrap(), thread::current().id());
}

/// Busy refusal gives back the exact owned allocation and leaves the admitted ticket intact.
#[test]
fn busy_preserves_the_owned_request() {
    let (started, began) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let (notified, notifications) = mpsc::channel();
    let mut worker = RequestWorker::new(
        move |request: Box<u64>| {
            started.send(()).unwrap();
            resume.recv_timeout(PROCESS_WAIT).unwrap();
            request
        },
        move |ticket| {
            let _ = notified.send(ticket);
        },
    );
    worker.try_request(7, Box::new(71)).unwrap();
    began.recv_timeout(WAIT).unwrap();
    let second = Box::new(82);
    let address = std::ptr::from_ref(second.as_ref());
    match worker.try_request(8, second) {
        Err(RequestError::Busy { ticket, request }) => {
            assert_eq!(ticket, 8);
            assert_eq!(std::ptr::from_ref(request.as_ref()), address);
        }
        other => panic!("expected owned Busy, got {other:?}"),
    }
    assert_eq!(worker.in_flight(), Some(7));
    release.send(()).unwrap();
    assert_eq!(notifications.recv_timeout(WAIT).unwrap(), 7);
    assert_eq!(*worker.try_result().unwrap().unwrap().1, 71);
    assert_eq!(worker.in_flight(), None);
}

/// Multiple jobs execute on the same non-caller thread and yield each tagged result once.
#[test]
fn one_persistent_thread_handles_multiple_jobs() {
    let (ran, executions) = mpsc::channel();
    let (notified, notifications) = mpsc::channel();
    let mut worker = RequestWorker::new(
        move |value: u64| {
            ran.send(thread::current().id()).unwrap();
            value + 1
        },
        move |ticket| {
            let _ = notified.send(ticket);
        },
    );
    let mut identity = None;
    for ticket in 1..=4 {
        worker.try_request(ticket, ticket * 10).unwrap();
        assert_eq!(notifications.recv_timeout(WAIT).unwrap(), ticket);
        let current = executions.recv_timeout(WAIT).unwrap();
        assert_ne!(current, thread::current().id());
        assert_eq!(*identity.get_or_insert(current), current);
        assert_eq!(worker.try_result(), Ok(Some((ticket, ticket * 10 + 1))));
        assert_eq!(worker.try_result(), Ok(None));
        assert_eq!(worker.in_flight(), None);
    }
}

/// Scheduler timeout/retry polling cannot release admission while the processor is still blocked.
#[test]
fn timeout_observation_cannot_admit_a_replacement() {
    let (started, began) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let (notified, notifications) = mpsc::channel();
    let mut worker = RequestWorker::new(
        move |()| {
            started.send(()).unwrap();
            resume.recv_timeout(PROCESS_WAIT).unwrap();
            19
        },
        move |ticket| {
            let _ = notified.send(ticket);
        },
    );
    worker.try_request(1, ()).unwrap();
    began.recv_timeout(WAIT).unwrap();
    for ticket in 2..=5 {
        assert_eq!(worker.try_result(), Ok(None));
        assert!(
            matches!(worker.try_request(ticket, ()), Err(RequestError::Busy { ticket: refused, request: () }) if refused == ticket)
        );
        assert_eq!(worker.in_flight(), Some(1));
    }
    release.send(()).unwrap();
    assert_eq!(notifications.recv_timeout(WAIT).unwrap(), 1);
    assert_eq!(worker.try_result(), Ok(Some((1, 19))));
}

/// A consumed result permits one queued request even while notify is unfinished;
/// the next computation still cannot start until the same consumer advances.
#[test]
fn result_then_request_does_not_overlap_a_slow_notification() {
    let (ran, executions) = mpsc::channel();
    let (notified, notifications) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let mut worker = RequestWorker::new(
        move |value: u64| {
            ran.send((value, thread::current().id())).unwrap();
            value * 2
        },
        move |ticket| {
            notified.send(ticket).unwrap();
            if ticket == 1 {
                resume.recv_timeout(PROCESS_WAIT).unwrap();
            }
        },
    );
    worker.try_request(1, 11).unwrap();
    assert_eq!(notifications.recv_timeout(WAIT).unwrap(), 1);
    let first = executions.recv_timeout(WAIT).unwrap();
    assert_eq!(worker.try_result(), Ok(Some((1, 22))));
    worker.try_request(2, 12).unwrap();
    assert!(matches!(executions.try_recv(), Err(TryRecvError::Empty)));
    assert!(matches!(
        worker.try_request(3, 13),
        Err(RequestError::Busy { ticket: 3, request: 13 })
    ));
    release.send(()).unwrap();
    assert_eq!(notifications.recv_timeout(WAIT).unwrap(), 2);
    let second = executions.recv_timeout(WAIT).unwrap();
    assert_eq!((first.0, second.0), (11, 12));
    assert_eq!(first.1, second.1);
    assert_eq!(worker.try_result(), Ok(Some((2, 24))));
}

/// A panicked consumer disconnects permanently; subsequent admissions return owned input, never restart it.
#[test]
fn panic_and_disconnect_are_terminal() {
    let (ran, executions) = mpsc::channel();
    let mut worker = RequestWorker::new(
        move |_: Box<u64>| -> Box<u64> {
            ran.send(()).unwrap();
            panic!("synthetic request failure");
        },
        |_| {},
    );
    worker.try_request(4, Box::new(4)).unwrap();
    executions.recv_timeout(WAIT).unwrap();
    // Waiting on channel disconnection avoids racing the panic's capture destructors.
    assert!(matches!(worker.results.recv_timeout(WAIT), Err(RecvTimeoutError::Disconnected)));
    assert_eq!(worker.try_result().unwrap_err(), WorkerDisconnected);
    for ticket in 5..=7 {
        let input = Box::new(ticket);
        let address = std::ptr::from_ref(input.as_ref());
        match worker.try_request(ticket, input) {
            Err(RequestError::Disconnected { ticket: refused, request }) => {
                assert_eq!(refused, ticket);
                assert_eq!(std::ptr::from_ref(request.as_ref()), address);
            }
            other => panic!("expected permanent disconnect, got {other:?}"),
        }
    }
    assert_eq!(worker.in_flight(), Some(4));
    assert!(matches!(executions.try_recv(), Err(TryRecvError::Disconnected)));
}

/// Shutdown never joins a blocked processor; a late owned failure is dropped by
/// the worker when the now-absent result receiver rejects its send.
#[test]
fn dropping_owner_does_not_join_and_late_failure_drops_on_worker() {
    let (started, began) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let (dropped, observed) = mpsc::channel();
    let (notified, notifications) = mpsc::channel();
    let mut worker = RequestWorker::new(
        move |()| -> Result<(), DropWitness> {
            started.send(thread::current().id()).unwrap();
            resume.recv_timeout(PROCESS_WAIT).unwrap();
            Err(DropWitness(dropped.clone()))
        },
        move |ticket| {
            let _ = notified.send(ticket);
        },
    );
    worker.try_request(1, ()).unwrap();
    let consumer = began.recv_timeout(WAIT).unwrap();
    let (closed, owner_dropped) = mpsc::channel();
    drop(thread::spawn(move || {
        drop(worker);
        let _ = closed.send(());
    }));
    let shutdown = owner_dropped.recv_timeout(WAIT);
    // Release even on failure so an accidental join cannot strand this test's processor.
    let _ = release.send(());
    assert!(shutdown.is_ok(), "dropping the worker waited for its blocked processor");
    assert_eq!(observed.recv_timeout(WAIT).unwrap(), consumer);
    assert!(matches!(notifications.recv_timeout(WAIT), Err(RecvTimeoutError::Disconnected)));
}

/// Successfully queued results are owned by the receiver: dropping it may run
/// their destructors on the caller, so native disposal is not promised nonblocking.
#[test]
fn queued_result_drops_on_the_owner_thread() {
    let (dropped, observed) = mpsc::channel();
    let (notified, notifications) = mpsc::channel();
    let mut worker = RequestWorker::new(
        move |()| DropWitness(dropped.clone()),
        move |ticket| {
            let _ = notified.send(ticket);
        },
    );
    worker.try_request(1, ()).unwrap();
    assert_eq!(notifications.recv_timeout(WAIT).unwrap(), 1);
    drop(worker);
    assert_eq!(observed.recv_timeout(WAIT).unwrap(), thread::current().id());
}
