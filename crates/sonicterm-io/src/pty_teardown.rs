use super::*;

fn run_pty_teardown(teardown: &mut impl PtyTeardownOps) {
    teardown.signal_cancel();
    teardown.cancel_io();
    teardown.terminate_child();
    #[cfg(windows)]
    {
        teardown.finish_io();
        teardown.close_master();
    }
    #[cfg(not(windows))]
    {
        teardown.close_master();
        teardown.finish_io();
    }
    teardown.reap_child();
}

impl PtyTeardown {
    /// Retry unfinished platform phases in teardown order and report only whole-payload settlement.
    pub fn run_remaining(&mut self) -> sonicterm_types::ReapResult {
        self.pass_failed = false;
        run_pty_teardown(self);
        self.join_finished_workers();
        let workers = self.completion.0.workers.try_lock();
        let failed =
            self.pass_failed || workers.as_ref().is_some_and(|workers| workers.any_failed());
        let joined = workers.as_ref().is_some_and(|workers| workers.all_joined());
        if failed {
            // Explicit phase errors and unwinds remain retryable failure, not wait expiry.
            sonicterm_types::ReapResult::Failed
        } else if self.is_complete() && joined {
            // When: is_complete and joined both hold, settlement includes native release and every started worker join.
            sonicterm_types::ReapResult::Settled
        } else {
            // When: completion or joined is still false without a phase error, native work merely exceeded this pass's waits.
            sonicterm_types::ReapResult::TimedOut
        }
    }

    /// Return whether every native phase has successfully completed; thread exit is checked separately.
    pub fn is_complete(&self) -> bool {
        self.completion.is_complete()
    }

    /// Return whether all started IO and native workers actually exited without an observed unwind.
    pub fn workers_finished(&self) -> bool {
        self.completion.workers_finished()
    }

    /// Clone completion custody that the supervisor can observe without locking this payload.
    pub fn completion(&self) -> PtyCompletion {
        self.completion.clone()
    }

    /// Join only worker handles already known to have finished.
    pub fn join_finished_workers(&mut self) {
        self.completion.join_finished_workers();
    }

    /// Use a whole-unit helper grant for every native worker started by this payload.
    pub fn set_worker_spawner(
        &mut self,
        spawner: Arc<dyn sonicterm_types::lifecycle::NativeWorkerSpawner>,
    ) {
        self.spawner = spawner;
    }

    /// Retain a sink-owned counter alongside each slotless cancellation duplicate.
    pub fn set_fallback_duplicate_tracker(
        &mut self,
        tracker: Arc<dyn Fn() -> sonicterm_types::lifecycle::NativePermit + Send + Sync>,
    ) {
        self.fallback_duplicate = Some(tracker);
    }

    /// Transfer final cleanup responsibility to a caller that retains incomplete custody in its process sink.
    pub fn retain_for_supervisor(&mut self) {
        self.direct_drop = false;
    }

    /// Attempt bounded child termination only; force cancellation never reaps the session leader.
    pub fn terminate_only(&mut self) -> sonicterm_types::CancelOutcome {
        if self.is_complete() && self.workers_finished() {
            // When: is_complete and workers_finished both hold, force cancellation has no native work left to perform.
            self.join_finished_workers();
            return sonicterm_types::CancelOutcome::Settled;
        }
        self.signal_cancel();
        self.terminate_child();
        sonicterm_types::CancelOutcome::TimedOut
    }
}

// Lifecycle: PtyTeardown runs unfinished cleanup while it still owns the child and master resources.
impl Drop for PtyTeardown {
    fn drop(&mut self) {
        if self.direct_drop {
            // When: direct_drop owns fixture cleanup, attempt one bounded pass before preserving any incomplete native custody.
            if !self.is_complete() {
                // When: is_complete is false, only direct fixture custody runs its bounded final cleanup pass.
                let _ = self.run_remaining();
            }
            self.join_finished_workers();
            if !self.is_complete() || !self.workers_finished() {
                std::mem::forget(self.child.clone());
                std::mem::forget(self.master.clone());
                std::mem::forget(self.completion.clone());
                std::mem::forget(self.spawner.clone());
                std::mem::forget(self.resize.take());
                #[cfg(windows)]
                {
                    std::mem::forget(self.conpty_drain_reader.clone());
                    for target in &self.cancel_targets {
                        std::mem::forget(target.clone());
                    }
                }
            }
        }
    }
}

impl PtyTeardownOps for PtyTeardown {
    fn signal_cancel(&mut self) {
        // Publish intentional closure before rejecting retained reply senders.
        self.writer_progress.closing.store(true, Ordering::SeqCst);
        self.replies.close();
        let _ = self.reader_cancel.try_send(());
        let _ = self.writer_cancel.try_send(());
    }

    // Lock order: completion workers before cancel_targets, permit_slots, and native permits; no worker takes the workers lock.
    fn cancel_io(&mut self) {
        #[cfg(windows)]
        {
            let deadline = Instant::now() + PTY_IO_SHUTDOWN_TIMEOUT;
            let mut workers = self.completion.0.workers.lock();
            for (index, name, phase) in [
                (0, "PTY reader thread", READER_CANCEL_PHASE),
                (1, "PTY writer thread", WRITER_CANCEL_PHASE),
            ] {
                if self.completion.phase_complete(phase)
                    || workers.native[index + 2].handle.is_some()
                    || workers.native[index + 2].joined
                {
                    // When: this phase already started, retries must not duplicate cancellation against the same IO thread.
                    continue;
                }
                let io = if index == 0 { &workers.reader } else { &workers.writer };
                if io
                    .as_ref()
                    .is_none_or(|io| io.handle.as_ref().is_none_or(thread::JoinHandle::is_finished))
                {
                    // When: the IO thread finished, there is no cancellation target; return its unused native permit once.
                    drop(self.cancel_targets[index].lock().take());
                    let permits = {
                        let mut custody = self.permit_slots[6 + index].lock();
                        custody.closed = true;
                        std::mem::take(&mut custody.permits)
                    };
                    drop(permits);
                    self.completion.complete_phase(phase);
                    continue;
                }
                if self.cancel_targets[index].lock().is_none() {
                    // When: cancel_targets is empty, create one owned duplicate; a failed spawn retains it for retry.
                    let Some(duplicate) = io.as_ref().and_then(PtyIoThread::cancel_target) else {
                        // When: cancel_target cannot duplicate a live thread, retain a retryable cancellation failure.
                        self.pass_failed = true;
                        continue;
                    };
                    let (mut native, _) = NativeValue::new(duplicate);
                    native.permits = self.permit_slots[6 + index].clone();
                    native.phase = Some((self.completion.clone(), phase));
                    if let Some(tracker) = &self.fallback_duplicate {
                        native.permits.lock().permits.push(tracker());
                    }
                    *self.cancel_targets[index].lock() = Some(native);
                }
                if !self.permits_installed {
                    // When: permits_installed is false, the fallback entry below starts both token-bearing targets together.
                    continue;
                }
                let result = workers.native[index + 2].start(
                    &*self.spawner,
                    3 + index,
                    "sonic-pty-cancel",
                    self.cancel_targets[index].clone(),
                    Arc::new(AtomicBool::new(false)),
                    move |native| cancel_owned_target(name, native),
                );
                if let Err(error) = result {
                    self.pass_failed = true;
                    tracing::warn!(%error, thread = name, "could not start PTY cancellation worker");
                }
            }
            if !self.permits_installed {
                // Slotless cancellation keeps tokens and join handles without borrowing an admitted grant.
                let targets = [
                    ("PTY reader thread", self.cancel_targets[0].clone()),
                    ("PTY writer thread", self.cancel_targets[1].clone()),
                ];
                if let Err(error) = cancel_io_within(
                    &targets,
                    &mut workers.native[2..],
                    &*self.spawner,
                    deadline,
                    cancel_owned_target,
                ) {
                    self.pass_failed = true;
                    tracing::warn!(%error, "could not start slotless PTY cancellation worker");
                }
            } else {
                // When: permits_installed is true, both reserved cancels share one deadline and retain unfinished handles.
                for worker in &mut workers.native[2..] {
                    worker.wait_until(deadline);
                }
            }
        }
    }

    fn terminate_child(&mut self) {
        if self.completion.phase_complete(TERMINATE_PHASE) {
            // When: TERMINATE_PHASE is complete, repeat passes must not signal an already settled child identity.
            return;
        }
        let Some(mut child) = self.child.try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT) else {
            // When: child custody stays busy through the bound, leave termination incomplete for the next pass.
            return;
        };
        child.closing = true;
        match terminate_child_without_reap(&mut child) {
            Ok(()) => self.completion.complete_phase(TERMINATE_PHASE),
            Err(error) => {
                self.pass_failed = true;
                tracing::warn!(%error, "failed to terminate PTY child");
            }
        }
    }

    fn finish_io(&mut self) {
        let mut workers = self.completion.0.workers.lock();
        if let Some(reader) = &mut workers.reader {
            reader.finish("PTY reader thread");
        }
        if let Some(writer) = &mut workers.writer {
            writer.finish("PTY writer thread");
        }
    }

    fn close_master(&mut self) {
        if self.completion.phase_complete(MASTER_PHASE) {
            // When: MASTER_PHASE is complete, the native master and its permits have already closed.
            return;
        }
        #[cfg(windows)]
        {
            let mut workers = self.completion.0.workers.lock();
            let drain = workers.native[0].start(
                &*self.spawner,
                1,
                "sonic-conpty-drain",
                self.conpty_drain_reader.clone(),
                Arc::new(AtomicBool::new(false)),
                |mut reader| {
                    let mut buffer = [0u8; 8192];
                    loop {
                        // When: reader reports EOF or a closed pipe, drain succeeded; an unexpected native read failure stays incomplete.
                        match reader.read(&mut buffer) {
                            Ok(0) => break,
                            Ok(_) => {}
                            Err(error)
                                if error
                                    .raw_os_error()
                                    .is_some_and(|code| code == 109 || code == 232) =>
                            {
                                break
                            }
                            Err(error) => {
                                reader.phase = None;
                                tracing::warn!(%error, "ConPTY output drain failed");
                                return false;
                            }
                        }
                    }
                    drop(reader);
                    true
                },
            );
            if let Err(error) = drain {
                // When: drain spawn is refused, leave the master and its recovery slot untouched for a later retry.
                self.pass_failed = true;
                tracing::warn!(%error, "could not start ConPTY drain worker");
                return;
            }
            if workers.native[0].handle.as_ref().is_some_and(thread::JoinHandle::is_finished) {
                workers.native[0].wait_until(Instant::now());
            }
            if workers.native[0].failed {
                // When: the drain already failed, keep the undrained master in recovery custody rather than starting its closer.
                self.pass_failed = true;
                return;
            }
            // The callback contains only an Arc to master custody; dropping it cannot close the retained native master.
            drop(self.resize.take());
            let close = workers.native[1].start(
                &*self.spawner,
                2,
                "sonic-conpty-close",
                self.master.clone(),
                Arc::new(AtomicBool::new(false)),
                |master| {
                    drop(master);
                    true
                },
            );
            if let Err(error) = close {
                // When: close spawn fails, retain the master recovery slot and reuse the already-started drain on retry.
                self.pass_failed = true;
                tracing::warn!(%error, "could not start ConPTY close worker");
                return;
            }
            workers.native[1].wait_until(Instant::now() + CONPTY_CLOSE_TIMEOUT);
            workers.native[0].wait_until(Instant::now() + CONPTY_CLOSE_TIMEOUT);
        }
        #[cfg(not(windows))]
        {
            let Some(mut master) = self.master.try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT) else {
                // When: master custody is busy, retain its descriptor for a later bounded close attempt.
                return;
            };
            let native = master.take();
            drop(master);
            drop(self.resize.take());
            drop(native);
        }
    }

    fn reap_child(&mut self) {
        if self.completion.phase_complete(REAP_PHASE) {
            // When: REAP_PHASE is complete, the child handle has already closed and its exit status stays cached.
            return;
        }
        if !self.completion.phase_complete(TERMINATE_PHASE) {
            // When: TERMINATE_PHASE remains incomplete, keep the leader unreaped so its session identity cannot be reused.
            return;
        }
        let deadline = Instant::now() + PTY_IO_SHUTDOWN_TIMEOUT;
        loop {
            let Some(mut child) = self.child.try_lock_until(deadline) else {
                // When: child custody cannot be acquired before deadline, retain the leader handle for retry.
                return;
            };
            // When: has_exited succeeds, close the retained process handle; errors fail this pass and live children keep polling.
            match child.has_exited() {
                Ok(true) => {
                    drop(child.child.take());
                    self.completion.complete_phase(REAP_PHASE);
                    return;
                }
                Err(error) => {
                    self.pass_failed = true;
                    tracing::warn!(%error, "failed to reap PTY child");
                    return;
                }
                Ok(false) => {}
            }
            drop(child);
            if Instant::now() >= deadline {
                // When: deadline expires while the leader is live, preserve its handle rather than waiting without a bound.
                return;
            }
            thread::sleep(
                Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}

#[cfg(test)]
#[path = "pty_teardown_tests.rs"]
mod pty_teardown_tests;
