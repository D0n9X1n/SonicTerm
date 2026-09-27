use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(super) struct Worker<Q, R> {
    requests: Option<SyncSender<Q>>,
    results: Receiver<R>,
    thread: Option<JoinHandle<()>>,
    in_flight: bool,
}

impl<Q: Send + 'static, R: Send + 'static> Worker<Q, R> {
    pub(super) fn new(
        mut process: impl FnMut(Q) -> R + Send + 'static,
        notify: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        let (requests, incoming) = mpsc::sync_channel(1);
        let (completed, results) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("font-probe-device".into())
            .spawn(move || {
                while let Ok(request) = incoming.recv() {
                    if completed.send(process(request)).is_err() {
                        // The caller no longer owns the receiver; the rejected result is released on this worker.
                        break;
                    }
                    notify();
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { requests: Some(requests), results, thread: Some(thread), in_flight: false })
    }

    pub(super) fn submit(&mut self, request: Q) -> Result<(), Q> {
        if self.in_flight {
            return Err(request);
        }
        let Some(sender) = &self.requests else { return Err(request) };
        sender.try_send(request).map_err(|error| match error {
            mpsc::TrySendError::Full(request) | mpsc::TrySendError::Disconnected(request) => {
                request
            }
        })?;
        self.in_flight = true;
        Ok(())
    }

    pub(super) fn try_result(&mut self) -> Result<Option<R>, String> {
        match self.results.try_recv() {
            Ok(result) => {
                self.in_flight = false;
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("font negotiation worker disconnected".into()),
        }
    }

    pub(super) fn finish(&mut self, deadline: Instant) -> Result<(), String> {
        drop(self.requests.take());
        loop {
            while let Ok(result) = self.results.try_recv() {
                drop(result);
                self.in_flight = false;
            }
            if self.thread.as_ref().is_none_or(JoinHandle::is_finished) {
                if let Some(thread) = self.thread.take() {
                    thread.join().map_err(|_| "font negotiation worker panicked".to_string())?;
                }
                while let Ok(result) = self.results.try_recv() {
                    drop(result);
                    self.in_flight = false;
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("font negotiation worker did not finish within cleanup deadline".into());
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}

pub(super) fn finish_run<T>(
    run: thread::Result<T>,
    cleanup: impl FnOnce() -> Result<(), String>,
) -> (T, Result<(), String>) {
    let cleanup = cleanup();
    if let Err(error) = &cleanup {
        eprintln!("font negotiation cleanup: {error}");
    }
    // Release caller-owned native results before resuming the original callback panic.
    let run = run.unwrap_or_else(|payload| std::panic::resume_unwind(payload));
    (run, cleanup)
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod worker_tests;
