use super::*;

impl PtyIoThread {
    /// Duplicate this thread's handle so another thread can cancel its synchronous I/O.
    ///
    /// Returns `None` when the thread was already finished or detached, or when duplication fails.
    #[cfg(windows)]
    pub(super) fn cancel_target(&self) -> Option<std::os::windows::io::OwnedHandle> {
        use std::os::windows::io::AsHandle;

        let Some(handle) = self.handle.as_ref() else {
            // When: `handle` absent means the thread was finished or detached, leaving no I/O to cancel.
            return None;
        };
        // A failed duplication leaves this thread's I/O uncancelled; `finish` still bounds the wait for it.
        handle
            .as_handle()
            .try_clone_to_owned()
            .inspect_err(|error| tracing::warn!(%error, "could not duplicate a PTY I/O thread handle for cancellation"))
            .ok()
    }

    pub(super) fn finish(&mut self, name: &'static str) {
        let deadline = Instant::now() + PTY_IO_SHUTDOWN_TIMEOUT;
        while self.handle.as_ref().is_some_and(|handle| !handle.is_finished()) {
            if Instant::now() >= deadline {
                // When: deadline expires before is_finished, keep the handle and native custody for the next teardown pass.
                tracing::warn!("{name} did not exit within the PTY shutdown timeout");
                return;
            }
            // A done message can precede native capture destruction; it is only a wake hint, never permission to join.
            let _ = self.done.try_recv();
            thread::sleep(
                Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                // An unwind never proves successful native cleanup, even though the thread returned.
                self.failed = true;
                tracing::warn!("{name} panicked during shutdown");
            }
        }
    }
}

/// Cancel the pending synchronous I/O of the thread whose duplicated handle is `duplicate`.
#[cfg(windows)]
fn cancel_thread_io(name: &'static str, duplicate: std::os::windows::io::OwnedHandle) -> bool {
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::{
        Foundation::{ERROR_NOT_FOUND, HANDLE},
        System::IO::CancelSynchronousIo,
    };

    let result =
        // SAFETY: duplicate owns the target thread handle for this call; cancellation affects only its pending synchronous IO.
        unsafe { CancelSynchronousIo(HANDLE(duplicate.as_raw_handle())) };
    match result {
        Ok(()) => true,
        Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_NOT_FOUND.0) => true,
        Err(error) => {
            // Errors other than ERROR_NOT_FOUND do not prove successful cancellation.
            tracing::warn!(%error, thread = name, "PTY synchronous IO cancellation failed");
            false
        }
    }
}

/// Run slotless cancellation under one shared deadline, retaining recovery slots and unfinished join handles.
///
/// Targets include their opaque duplicate tokens. Neither expiry nor OS spawn refusal can destroy sole custody.
#[cfg(windows)]
pub(super) fn cancel_io_within<T: Send + 'static>(
    targets: &[(&'static str, Arc<Mutex<Option<T>>>)],
    workers: &mut [NativeWorker],
    spawner: &dyn sonicterm_types::lifecycle::NativeWorkerSpawner,
    deadline: Instant,
    cancel: fn(&'static str, T) -> bool,
) -> std::io::Result<bool> {
    let mut spawn_error = None;
    for (index, ((name, target), worker)) in targets.iter().zip(workers.iter_mut()).enumerate() {
        if target.lock().is_none() {
            // When: target is empty, its worker already consumed custody or no cancellation was needed.
            continue;
        }
        let name = *name;
        if let Err(error) = worker.start(
            spawner,
            3 + index,
            "sonic-pty-cancel",
            target.clone(),
            Arc::new(AtomicBool::new(false)),
            move |target| cancel(name, target),
        ) {
            // Spawn refusal keeps the target in its recovery slot while the sibling cancel may start.
            spawn_error = Some(error);
        }
    }
    let mut complete = true;
    for worker in workers {
        if worker.handle.is_some() {
            complete &= worker.wait_until(deadline);
        }
        complete &= !worker.failed;
    }
    match spawn_error {
        Some(error) => Err(error),
        None => Ok(complete),
    }
}

#[cfg(windows)]
pub(super) fn cancel_owned_target(
    name: &'static str,
    mut native: NativeValue<std::os::windows::io::OwnedHandle>,
) -> bool {
    let succeeded =
        cancel_thread_io(name, native.value.take().expect("owned cancellation duplicate"));
    if !succeeded {
        // Native close returns its permit but cannot credit a failed cancellation phase.
        native.phase = None;
    }
    drop(native);
    succeeded
}

impl sonicterm_types::lifecycle::NativeWorkerSpawner for ThreadWorkerSpawner {
    fn spawn(
        &self,
        _slot: usize,
        name: &'static str,
        work: Box<dyn FnOnce() + Send>,
    ) -> std::io::Result<thread::JoinHandle<()>> {
        thread::Builder::new().name(name.into()).spawn(work)
    }
}

impl NativeWorker {
    // Ordering: phase stores Release only after work returns; its observer acquires the successful native phase.
    #[cfg(any(windows, test))]
    pub(super) fn start<T: Send + 'static>(
        &mut self,
        spawner: &dyn sonicterm_types::lifecycle::NativeWorkerSpawner,
        slot: usize,
        name: &'static str,
        native: Arc<Mutex<Option<T>>>,
        phase: Arc<std::sync::atomic::AtomicBool>,
        work: impl FnOnce(T) -> bool + Send + 'static,
    ) -> std::io::Result<()> {
        if self.handle.is_some() || self.joined {
            // When: a handle exists or joined is true, this phase already started and must never consume native custody twice.
            return Ok(());
        }
        self.result = Some(phase.clone());
        let handle = spawner.spawn(
            slot,
            name,
            Box::new(move || {
                let native = native.lock().take();
                if let Some(native) = native {
                    let succeeded = work(native);
                    phase.store(succeeded, Ordering::Release);
                }
            }),
        )?;
        self.handle = Some(handle);
        Ok(())
    }

    // Ordering: result Acquire observes the started worker's Release success publication after actual thread exit.
    pub(super) fn wait_until(&mut self, deadline: Instant) -> bool {
        while self.handle.as_ref().is_some_and(|handle| !handle.is_finished()) {
            if Instant::now() >= deadline {
                // When: deadline expires, retain the handle so the worker and its native permits remain observable.
                return false;
            }
            thread::sleep(
                Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        if let Some(handle) = self.handle.take() {
            self.failed |= handle.join().is_err();
            self.failed |=
                self.result.as_ref().is_none_or(|result| !result.load(Ordering::Acquire));
            self.joined = true;
        }
        self.joined && !self.failed
    }
}

impl<T> NativeValue<T> {
    pub(super) fn new(value: T) -> (Self, PermitSlot) {
        let permits = Arc::new(Mutex::new(PermitCustody::default()));
        (Self { value: Some(value), permits: permits.clone(), phase: None }, permits)
    }
}

impl<T: Read> Read for NativeValue<T> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.value.as_mut().expect("owned reader").read(buffer)
    }
}

impl<T: Write> Write for NativeValue<T> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.value.as_mut().expect("owned writer").write(buffer)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.value.as_mut().expect("owned writer").flush()
    }
}

// Lifecycle: NativeValue drops its T value before permits; an unwind never publishes a successful phase.
impl<T> Drop for NativeValue<T> {
    fn drop(&mut self) {
        drop(self.value.take());
        let permits = {
            let mut custody = self.permits.lock();
            custody.closed = true;
            std::mem::take(&mut custody.permits)
        };
        drop(permits);
        if !thread::panicking() {
            // Successful native destruction precedes phase publication and excludes unwind cleanup.
            if let Some((completion, phase)) = self.phase.take() {
                completion.complete_phase(phase);
            }
        }
    }
}

impl TeardownWorkers {
    pub(super) fn any_failed(&self) -> bool {
        [&self.reader, &self.writer]
            .into_iter()
            .any(|worker| worker.as_ref().is_some_and(|worker| worker.failed))
            || self.native.iter().any(|worker| worker.failed)
    }

    fn all_finished(&self) -> bool {
        [&self.reader, &self.writer].into_iter().all(|worker| {
            worker.as_ref().is_none_or(|worker| {
                !worker.failed && worker.handle.as_ref().is_none_or(thread::JoinHandle::is_finished)
            })
        }) && self.native.iter().all(|worker| {
            !worker.failed && worker.handle.as_ref().is_none_or(thread::JoinHandle::is_finished)
        })
    }

    pub(super) fn all_joined(&self) -> bool {
        [&self.reader, &self.writer].into_iter().all(|worker| {
            worker.as_ref().is_none_or(|worker| !worker.failed && worker.handle.is_none())
        }) && self.native.iter().all(|worker| !worker.failed && worker.handle.is_none())
    }

    fn join_finished(&mut self) {
        for (worker, name) in
            [(&mut self.reader, "PTY reader thread"), (&mut self.writer, "PTY writer thread")]
        {
            if let Some(worker) = worker {
                // is_finished proves the native worker returned, so finish cannot block on its JoinHandle.
                if worker.handle.as_ref().is_some_and(thread::JoinHandle::is_finished) {
                    worker.finish(name);
                }
            }
        }
        for worker in &mut self.native {
            if worker.handle.as_ref().is_some_and(thread::JoinHandle::is_finished) {
                worker.wait_until(Instant::now());
            }
        }
    }
}

impl PtyCompletion {
    pub(super) fn new() -> Self {
        #[cfg(windows)]
        let required_phases = 8;
        #[cfg(not(windows))]
        let required_phases = 5;
        Self(Arc::new(CompletionState {
            phases: std::array::from_fn(|_| AtomicBool::new(false)),
            remaining: AtomicUsize::new(required_phases),
            complete: AtomicBool::new(false),
            wake: Mutex::new(None),
            workers: Arc::new(Mutex::new(TeardownWorkers::default())),
        }))
    }

    // Ordering: state phases and remaining use AcqRel once per phase; complete Release publishes all native destruction.
    pub(super) fn complete_phase(&self, phase: usize) {
        let state = &self.0;
        if !state.phases[phase].swap(true, Ordering::AcqRel) {
            // When: phases swaps from false, this phase owns exactly one remaining decrement.
            if state.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
                // When: remaining reaches zero, publish completion before the bounded driver wake hint.
                state.complete.store(true, Ordering::Release);
                if let Some(wake) = state.wake.lock().as_ref() {
                    // When: wake is installed, the driver rechecks phase completion and actual worker exit.
                    let _ = wake.try_send(());
                }
            }
        }
    }

    // Ordering: state phases Acquire observes successful native destruction before a phase is skipped.
    pub(super) fn phase_complete(&self, phase: usize) -> bool {
        let state = &self.0;
        state.phases[phase].load(Ordering::Acquire)
    }

    /// Return whether every required phase successfully released its owned native values.
    // Ordering: state complete Acquire observes the last successful phase's Release publication.
    pub fn is_complete(&self) -> bool {
        let state = &self.0;
        state.complete.load(Ordering::Acquire)
    }

    /// Observe actual exit of every started worker; busy worker custody remains conservatively unfinished.
    pub fn workers_finished(&self) -> bool {
        self.0.workers.try_lock().is_some_and(|workers| workers.all_finished())
    }

    /// Join only already-finished handles, retaining all others for the next observation.
    pub fn join_finished_workers(&self) {
        if let Some(mut workers) = self.0.workers.try_lock() {
            workers.join_finished();
        }
    }

    /// Install a bounded wake hint for the final successful phase; the driver still checks actual thread exit.
    pub fn set_wake(&self, wake: Sender<()>) {
        *self.0.wake.lock() = Some(wake);
    }
}

#[cfg(test)]
#[path = "pty_native_tests.rs"]
mod pty_native_tests;
