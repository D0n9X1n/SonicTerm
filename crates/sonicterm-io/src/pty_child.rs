use super::*;

impl PtyChildExitProbe {
    /// Return whether the child has exited without waiting for it.
    ///
    /// On Unix, observing a pending child exit also signals the child's process
    /// group before returning `true`, so background descendants cannot survive
    /// after the shell leader exits.
    pub fn has_exited(&self) -> Result<bool> {
        let Some(mut child) = self.child.try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT) else {
            // When: child is owned by retirement, a probe must not wait beyond its bounded observation interval.
            return Ok(false);
        };
        if child.closing || self.progress.closing.load(Ordering::SeqCst) {
            // When: closing owns native cleanup, probes only observe the retained status and never reap the leader.
            return Ok(child.exited);
        }
        #[cfg(windows)]
        return Ok(child.has_exited()?);
        #[cfg(unix)]
        {
            if child.exited {
                // When: `child.exited` records an earlier observation, so no new OS probe is needed.
                return Ok(true);
            }
            let Some(pid) = child.process_id() else {
                // When: `process_id` is unavailable, Unix cannot inspect an unreaped leader yet.
                return Ok(false);
            };
            let observed = unix_child_exit_pending(pid)?;
            if !observed.pending {
                // When: `pending` is false while the leader still runs, so teardown must not start.
                return Ok(false);
            }
            // Recorded before signalling the group: teardown reaps the child,
            // after which the status is unrecoverable.
            child.record_exit_status(observed.status);
            // The exit is observed: publish it before signalling the group or any later reap.
            child.publish_exit();
            signal_process_group_for_platform(&mut child)?;
            Ok(true)
        }
    }

    /// Whether the child's own exit was clean, once it has exited.
    ///
    /// `None` means unknown rather than bad: the child is still running, or
    /// it was reaped by a path that never saw a status. A caller deciding
    /// whether to tear down a pane must not read `None` as a crash —
    /// discarding a user's scrollback on our own uncertainty is a worse
    /// failure than leaving the pane open.
    ///
    /// Only meaningful after [`Self::has_exited`] returns `true`; that call
    /// is what records the status.
    pub fn exit_was_clean(&self) -> Option<bool> {
        self.exit_status().map(|status| status.is_clean())
    }

    /// How the child's own process ended, once it has exited.
    ///
    /// `None` has the same meaning as for [`Self::exit_was_clean`]: the status
    /// was not observed, which callers report as unknown rather than a crash.
    pub fn exit_status(&self) -> Option<PaneExitStatus> {
        self.child.try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT).and_then(|child| child.exit_status.clone())
    }
}

#[cfg(unix)]
pub(super) fn unix_child_exit_pending(pid: u32) -> std::io::Result<UnixExitObservation> {
    let mut info: libc::siginfo_t =
        // SAFETY: zeroed `siginfo_t` is valid writable storage for `waitid` to initialize.
        unsafe { std::mem::zeroed() };
    let result =
        // SAFETY: `info` is writable; `pid` identifies our child, and `WNOWAIT` preserves it for group signalling.
        unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        // When: `result` is -1, so preserve the kernel error instead of interpreting `info`.
        return Err(std::io::Error::last_os_error());
    }
    let pending =
        // SAFETY: `info` was zeroed first, so `si_pid` is valid whether or not `waitid` wrote a status.
        unsafe { info.si_pid() }
            != 0;
    if !pending {
        // When: `pending` is false, `WNOHANG` found no child exit status to record.
        return Ok(UnixExitObservation { pending: false, status: None });
    }
    // `si_code` distinguishes a normal exit from a signal death; `si_status`
    // carries the exit code only in the former case. A child killed by a
    // signal is not a clean exit, and neither is a nonzero code.
    let (code, status) = (
        info.si_code,
        // SAFETY: pending exit status from `waitid` initialized the `si_status` union field.
        unsafe { info.si_status() },
    );
    let exit_status = match code {
        libc::CLD_EXITED => PaneExitStatus::Code(status as u32),
        libc::CLD_KILLED | libc::CLD_DUMPED => PaneExitStatus::Signal(unix_signal_name(status)),
        // Trapped or stopped cannot satisfy `WEXITED`; the kind is unknown, and still not clean.
        _ => PaneExitStatus::Unknown,
    };
    Ok(UnixExitObservation { pending: true, status: Some(exit_status) })
}

/// Map `portable-pty`'s status to the typed pane status.
///
/// A signal death carries the signal's name; anything else carries its code.
fn pane_exit_status(status: &portable_pty::ExitStatus) -> PaneExitStatus {
    match status.signal() {
        Some(name) => PaneExitStatus::Signal(name.to_string()),
        None => PaneExitStatus::Code(status.exit_code()),
    }
}

/// Publish that the child has exited or is being stopped.
///
/// Every path that can release the child's identity (a consuming Unix wait, closing the
/// retained Windows handle) calls this first, so a reader that sees `false` knows the pid
/// still names this child.
// Ordering: exit_observed stores Release so a reader's Acquire load that sees true also sees earlier teardown state.
pub(super) fn publish_exit(exit_observed: &AtomicBool) {
    exit_observed.store(true, Ordering::Release);
}

/// Non-consuming exit peek: `Some(pending)` on Unix, `None` where no peek exists.
///
/// On Unix a pid that is unavailable while unpublished reads as not pending, so a live
/// child is never marked exited. Windows has no peek and needs none: its wait releases nothing.
fn peek_child_exit(pid: Option<u32>) -> std::io::Result<Option<bool>> {
    #[cfg(unix)]
    return match pid {
        Some(pid) => Ok(Some(unix_child_exit_pending(pid)?.pending)),
        None => Ok(Some(false)),
    };
    #[cfg(not(unix))]
    {
        let _ = pid;
        Ok(None)
    }
}

impl ChildState {
    pub(super) fn new(child: Box<dyn Child + Send + Sync>, unix_session_id: Option<u32>) -> Self {
        Self {
            child: Some(NativeValue::new(child).0),
            closing: false,
            exited: false,
            exit_status: None,
            unix_session_id,
            process_group_signalled: false,
            exit_observed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Publish exit for this child; see [`publish_exit`].
    pub(super) fn publish_exit(&self) {
        publish_exit(&self.exit_observed);
    }

    /// Keep the first observed status: the `waitid` peek and the consuming wait describe one exit.
    fn record_exit_status(&mut self, status: Option<PaneExitStatus>) {
        self.exit_status = self.exit_status.take().or(status);
    }

    pub(super) fn has_exited(&mut self) -> std::io::Result<bool> {
        self.has_exited_with(peek_child_exit, |native| native.try_wait())
    }

    /// `has_exited` with an injectable peek and consuming wait, so tests can pause inside the wait.
    ///
    /// Unless exit is already published, the non-consuming peek runs first: not pending
    /// returns `false` without waiting; pending publishes exit before the wait reaps. Without
    /// a peek, the wait decides and a reported exit is published before returning. Peek and
    /// wait run under the caller's one `ChildState` lock.
    // Ordering: exit_observed loads Acquire; a published exit means a consuming wait is already safe.
    fn has_exited_with<Peek, Wait>(
        &mut self,
        mut peek: Peek,
        mut wait: Wait,
    ) -> std::io::Result<bool>
    where
        Peek: FnMut(Option<u32>) -> std::io::Result<Option<bool>>,
        Wait: FnMut(
            &mut Box<dyn Child + Send + Sync>,
        ) -> std::io::Result<Option<portable_pty::ExitStatus>>,
    {
        if self.exited {
            // When: `self.exited` already consumed the status, so another wait cannot add information.
            return Ok(true);
        }
        if self.child.is_none() {
            // When: child custody was closed after reaping, retain the recorded exit observation.
            return Ok(self.exited);
        }
        if !self.exit_observed.load(Ordering::Acquire) {
            // When: `exit_observed` is false, peek without consuming before any wait may release the pid.
            match peek(self.process_id())? {
                Some(false) => {
                    // When: the peek finds no pending exit, the child is live; skip the wait and stay unpublished.
                    return Ok(false);
                }
                Some(true) => publish_exit(&self.exit_observed),
                None => {
                    // When: no peek exists on this platform, the wait itself decides and releases nothing.
                }
            }
        }
        let native =
            self.child.as_mut().and_then(|child| child.value.as_mut()).expect("owned child");
        let Some(status) = wait(native)? else {
            // When: the wait reports no status, the child still runs and stays unpublished.
            return Ok(false);
        };
        publish_exit(&self.exit_observed);
        self.exited = true;
        // Recorded here because this is the only place the status is available: the wait
        // reaps the child, so a later call returns `None` and the status is gone for good.
        self.record_exit_status(Some(pane_exit_status(&status)));
        Ok(true)
    }

    pub(super) fn process_id(&self) -> Option<u32> {
        (!self.exited).then(|| self.child.as_ref()?.value.as_ref()?.process_id()).flatten()
    }
}

fn terminate_child<G, P>(
    child: &mut ChildState,
    signal_group: G,
    mut signal_pid: P,
) -> std::io::Result<()>
where
    G: FnMut(u32) -> std::io::Result<()>,
    P: FnMut(u32),
{
    // Termination is intent to stop: publish before any signal or wait can release the pid.
    child.publish_exit();
    signal_process_group(child, signal_group)?;
    if child.has_exited()? {
        // When: `child` has exited after group signalling, so direct-pid termination is unnecessary.
        return Ok(());
    }
    if let Some(pid) = child.process_id() {
        signal_pid(pid);
    }
    child
        .child
        .as_mut()
        .expect("unreaped child custody")
        .value
        .as_mut()
        .expect("owned child")
        .kill()
}

fn signal_process_group<G>(child: &mut ChildState, mut signal_group: G) -> std::io::Result<()>
where
    G: FnMut(u32) -> std::io::Result<()>,
{
    if child.process_group_signalled {
        // When: `process_group_signalled` prevents duplicate group kills across teardown retries.
        return Ok(());
    }
    if let Some(unix_session_id) = child.unix_session_id {
        signal_group(unix_session_id)?;
    }
    child.process_group_signalled = true;
    Ok(())
}

#[cfg(unix)]
fn signal_process_group_for_platform(child: &mut ChildState) -> std::io::Result<()> {
    signal_process_group(child, terminate_unix_session)
}

pub(super) fn terminate_child_for_platform(child: &mut ChildState) -> std::io::Result<()> {
    terminate_child(
        child,
        |unix_session_id| {
            #[cfg(unix)]
            return terminate_unix_session(unix_session_id);
            #[cfg(not(unix))]
            {
                let _ = unix_session_id;
                Ok(())
            }
        },
        |pid| {
            #[cfg(unix)]
            {
                // SAFETY: ChildState::has_exited just returned false while
                // holding the child mutex, so the direct pid is not reaped.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
            #[cfg(not(unix))]
            let _ = pid;
        },
    )
}

/// Stop the retained child without calling try_wait, so cancellation cannot release the leader identity.
pub(super) fn terminate_child_without_reap(child: &mut ChildState) -> std::io::Result<()> {
    // Termination is intent to stop: publish before any signal or handle work.
    child.publish_exit();
    #[cfg(unix)]
    signal_process_group_for_platform(child)?;
    if child.exited {
        // When: exited already records a reaped leader, no native termination is needed.
        return Ok(());
    }
    let Some(native) = child.child.as_mut().and_then(|child| child.value.as_mut()) else {
        // When: child custody is absent after native release, no process handle remains to terminate.
        return Ok(());
    };
    #[cfg(unix)]
    {
        if let Some(pid) = native.process_id() {
            // When: process_id exposes the retained leader, signal it without a status-consuming wait.
            let result =
                // SAFETY: native retains our unreaped child identity; kill never reaps or releases it.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                // When: result failed for a reason other than ESRCH, preserve the termination error for retry.
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::HANDLE,
            System::Threading::{GetExitCodeProcess, TerminateProcess},
        };
        let raw = native.as_raw_handle().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "PTY child has no retained process handle",
            )
        })?;
        let handle = HANDLE(raw);
        let mut status = 0u32;
        let observed =
            // SAFETY: native retains this exact process handle under child custody; no PID lookup substitutes another process.
            unsafe { GetExitCodeProcess(handle, &mut status) };
        if observed.is_err() {
            // When: observed fails, report the native query error instead of guessing whether the child exited.
            return Err(std::io::Error::last_os_error());
        }
        if status != 259 {
            // When: status is not STILL_ACTIVE, natural exit already completed without reaping the retained child.
            return Ok(());
        }
        let terminated =
            // SAFETY: handle is the same retained child queried above; termination does not reap or close its identity.
            unsafe { TerminateProcess(handle, 1) };
        if terminated.is_err() {
            // When: terminated fails, only a confirmed concurrent natural exit can turn that failure into success.
            let error = std::io::Error::last_os_error();
            let observed =
                // SAFETY: native still retains handle while checking whether exit raced the failed termination.
                unsafe { GetExitCodeProcess(handle, &mut status) };
            if observed.is_ok() && status != 259 {
                // When: observed sees a terminal status on the same handle, termination has nothing left to stop.
                return Ok(());
            }
            return Err(error);
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    native.kill()
}

#[cfg(test)]
#[path = "pty_child_tests.rs"]
mod pty_child_tests;
