//! One lazy persistent request consumer; event-loop calls never wait for work.

use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};

/// A request not admitted; its ticket and owned input remain with the caller.
#[derive(Debug)]
pub(super) enum RequestError<Q> {
    /// Another request still has an unconsumed result or is still running.
    Busy { ticket: u64, request: Q },
    /// The worker failed to start or disconnected; it will never be restarted.
    Disconnected { ticket: u64, request: Q },
}

/// The persistent consumer is gone and no further requests can be admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkerDisconnected;

/// Serialize owned requests on one lazily spawned, immediately detached thread.
///
/// Only consuming a matching result releases admission. Timeouts and advisory
/// events have no API here, so neither can replace a hung consumer or clear work.
/// Dropping this owner closes input without joining. A queued result may drop on
/// the caller; native destructors are not guaranteed nonblocking. A failed result
/// send instead drops that owned result on the worker.
pub(super) struct RequestWorker<Q, R> {
    requests: Option<SyncSender<(u64, Q)>>,
    results: Receiver<(u64, R)>,
    start: Option<Box<dyn FnOnce() + Send>>,
    in_flight: Option<u64>,
    closed: bool,
}

impl<Q: Send + 'static, R: Send + 'static> RequestWorker<Q, R> {
    /// Keep the processor dormant until the first admission; notifications carry
    /// no result ownership and run after the bounded result send succeeds.
    pub(super) fn new(
        mut process: impl FnMut(Q) -> R + Send + 'static,
        notify: impl Fn(u64) + Send + 'static,
    ) -> Self {
        let (requests, incoming) = mpsc::sync_channel::<(u64, Q)>(1);
        let (completed, results) = mpsc::sync_channel::<(u64, R)>(1);
        let start = Box::new(move || {
            while let Ok((ticket, request)) = incoming.recv() {
                let result = process(request);
                if completed.send((ticket, result)).is_err() {
                    // When: `send` fails, its owned result drops here; the event loop can no longer receive it.
                    break;
                }
                // The same consumer cannot process another request until notify returns.
                notify(ticket);
            }
        });
        Self {
            requests: Some(requests),
            results,
            start: Some(start),
            in_flight: None,
            closed: false,
        }
    }

    /// Admit one tagged request without waiting, preserving ownership on refusal.
    /// A spawn failure is terminal just like a disconnected channel.
    pub(super) fn try_request(&mut self, ticket: u64, request: Q) -> Result<(), RequestError<Q>> {
        if self.closed {
            // When: `closed` is latched, a lost consumer is never silently replaced.
            return Err(RequestError::Disconnected { ticket, request });
        }
        if self.in_flight.is_some() {
            // When: `in_flight` remains set, even a timed-out request still owns admission.
            return Err(RequestError::Busy { ticket, request });
        }
        if let Some(start) = self.start.take() {
            // When: `start` is present, this admission is the only opportunity to create the consumer.
            match std::thread::Builder::new().name("sonicterm-gpu-recovery".into()).spawn(start) {
                Ok(handle) => drop(handle),
                Err(_) => {
                    // When: `spawn` fails, the consumed processor cannot be recreated; return the input untouched.
                    self.close();
                    return Err(RequestError::Disconnected { ticket, request });
                }
            }
        }
        match self
            .requests
            .as_ref()
            .expect("open worker owns request sender")
            .try_send((ticket, request))
        {
            Ok(()) => {
                self.in_flight = Some(ticket);
                Ok(())
            }
            Err(TrySendError::Full((ticket, request))) => {
                Err(RequestError::Busy { ticket, request })
            }
            Err(TrySendError::Disconnected((ticket, request))) => {
                self.close();
                Err(RequestError::Disconnected { ticket, request })
            }
        }
    }

    /// Take one actual completion without waiting; an empty channel leaves
    /// admission occupied, while disconnect permanently closes the worker.
    pub(super) fn try_result(&mut self) -> Result<Option<(u64, R)>, WorkerDisconnected> {
        if self.closed {
            // When: `closed` is latched, no processor or result producer can be recreated.
            return Err(WorkerDisconnected);
        }
        match self.results.try_recv() {
            Ok((ticket, result)) => {
                if self.in_flight == Some(ticket) {
                    self.in_flight = None;
                }
                Ok(Some((ticket, result)))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.close();
                Err(WorkerDisconnected)
            }
        }
    }

    /// The admitted request whose actual completion has not been consumed.
    pub(super) fn in_flight(&self) -> Option<u64> {
        self.in_flight
    }

    fn close(&mut self) {
        self.closed = true;
        drop(self.requests.take());
        drop(self.start.take());
    }
}

#[cfg(test)]
#[path = "gpu_recovery_worker_tests.rs"]
mod gpu_recovery_worker_tests;
