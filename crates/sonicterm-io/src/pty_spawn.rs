use super::*;

fn apply_child_cwd(builder: &mut CommandBuilder, explicit: Option<&Path>, home: Option<&str>) {
    if let Some(cwd) = explicit.filter(|cwd| cwd.is_dir()) {
        builder.cwd(cwd);
    } else if let Some(home) = home {
        // When: `home` exists but no valid explicit directory does, use it as the child fallback.
        builder.cwd(home);
    }
}

/// Map a native spawn failure to a fixed reason; `portable-pty`'s text names paths and is not forwarded.
fn sanitize_spawn_error(program: &std::ffi::OsStr, error: &anyhow::Error) -> CommandSpawnError {
    let reason = match error.downcast_ref::<std::io::Error>().map(std::io::Error::kind) {
        Some(std::io::ErrorKind::NotFound) => "the program or its interpreter was not found",
        Some(std::io::ErrorKind::PermissionDenied) => "permission denied",
        _ => "the operating system refused to start it",
    };
    CommandSpawnError::Spawn { program: crate::command_resolve::display_name(program), reason }
}

impl PtyHandle {
    /// Spawn the user's default shell.
    ///
    /// `opts.clean_e2e=true` suppresses shell startup banner/profile and
    /// emits clean-mode args (PowerShell `-NoLogo -NoProfile`, bash
    /// `--norc --noprofile`, zsh `-f`). E2E gates pass `true`; the
    /// production app passes `ShellSpawnOpts::default()` to preserve
    /// interactive behavior.
    pub fn spawn_default_shell(cols: u16, rows: u16, opts: ShellSpawnOpts) -> Result<Self> {
        let shell = resolve_spawn_shell(opts.shell.as_deref());
        let args = shell_startup_args(&shell, opts.clone());
        Self::spawn_with_args_and_opts(&shell, &args, cols, rows, opts)
    }

    /// Spawn `cmd` (may include arguments via shell-style splitting handled
    /// upstream — we expect a single program path here for simplicity).
    pub fn spawn(cmd: &str, cols: u16, rows: u16) -> Result<Self> {
        Self::spawn_with_args(cmd, &[], cols, rows)
    }

    /// Internal: spawn `cmd` with `args`. The public `spawn` + `spawn_default_shell`
    /// converge here so opts-derived args (e.g. `-NoLogo -NoProfile` for
    /// PowerShell clean_e2e) reach `CommandBuilder` consistently.
    ///
    /// Also `pub` (doc-hidden) so integration tests can spawn shells with
    /// args (e.g. `bash -c "trap '' HUP; exec cat"` for the LM-007
    /// regression test) without re-implementing the whole pipeline.
    #[doc(hidden)]
    pub fn spawn_with_args(cmd: &str, args: &[String], cols: u16, rows: u16) -> Result<Self> {
        Self::spawn_with_args_and_opts(cmd, args, cols, rows, ShellSpawnOpts::default())
    }

    /// Internal: spawn `cmd` with `args` and explicit environment options.
    #[doc(hidden)]
    pub fn spawn_with_args_and_opts(
        cmd: &str,
        args: &[String],
        cols: u16,
        rows: u16,
        opts: ShellSpawnOpts,
    ) -> Result<Self> {
        let mut builder = CommandBuilder::new(cmd);
        for a in args {
            builder.arg(a);
        }
        apply_clean_e2e_environment(&mut builder, opts.clean_e2e);
        let home = std::env::var("HOME").ok();
        apply_child_cwd(&mut builder, opts.cwd.as_deref(), home.as_deref());
        apply_child_pty_env(&mut builder, &opts.term_program);
        Self::spawn_built(builder, cmd.to_string(), cols, rows, opts.diagnostic_timestamps)
    }

    /// Run a launcher's program directly in a new PTY, with no shell.
    ///
    /// SonicTerm resolves `spawn.program` itself (see [`CommandSpawn`]) and
    /// hands `portable-pty` an absolute path, so no later layer searches a
    /// path relative to the child's cwd. `spawn.cwd` must be an absolute,
    /// existing directory; the spawn never substitutes a home directory and
    /// never falls back to a shell. Only `opts.term_program` and
    /// `opts.diagnostic_timestamps` apply; the shell, cwd and clean-startup
    /// options describe shells and are ignored.
    ///
    /// Errors are [`CommandSpawnError`] values whose messages name only the
    /// program's file name and the failing rule.
    pub fn spawn_command(
        spawn: &CommandSpawn,
        cols: u16,
        rows: u16,
        opts: &ShellSpawnOpts,
    ) -> std::result::Result<Self, CommandSpawnError> {
        let path_var = crate::command_resolve::process_path_var();
        let resolved = crate::command_resolve::resolve_program(
            &spawn.program,
            &spawn.cwd,
            path_var.as_deref(),
        )?;
        let mut builder = CommandBuilder::new(resolved.as_os_str());
        for arg in &spawn.args {
            builder.arg(arg);
        }
        builder.cwd(&spawn.cwd);
        apply_child_pty_env(&mut builder, &opts.term_program);
        let program_path = resolved.to_string_lossy().into_owned();
        Self::spawn_built(builder, program_path, cols, rows, opts.diagnostic_timestamps)
            .map_err(|error| sanitize_spawn_error(&spawn.program, &error))
    }

    /// Open a PTY, start `builder` on it, and wire the reader, writer and teardown.
    fn spawn_built(
        builder: CommandBuilder,
        program_path: String,
        cols: u16,
        rows: u16,
        diagnostic_timestamps: bool,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);

        let master = pair.master;
        #[cfg(unix)]
        let unix_session_id = child.process_id();
        #[cfg(not(unix))]
        let unix_session_id = None;
        let completion = PtyCompletion::new();
        let (mut reader, reader_permit) = NativeValue::new(master.try_clone_reader()?);
        reader.phase = Some((completion.clone(), READER_PHASE));
        #[cfg(windows)]
        let (mut drain_reader, drain_permit) = NativeValue::new(master.try_clone_reader()?);
        #[cfg(windows)]
        {
            drain_reader.phase = Some((completion.clone(), DRAIN_PHASE));
        }
        let (mut writer, writer_permit) = NativeValue::new(pty_writer(&*master)?);
        writer.phase = Some((completion.clone(), WRITER_PHASE));
        let (mut master, master_permit) = NativeValue::new(master);
        master.phase = Some((completion.clone(), MASTER_PHASE));
        let master = Arc::new(Mutex::new(Some(master)));
        let child = Arc::new(Mutex::new(ChildState::new(child, unix_session_id)));
        // Captured once while the child is unreaped and its custody lock is held.
        let (process_identity, exit_observed) = {
            let state = child.lock();
            (capture_process_identity(&state), state.exit_observed.clone())
        };
        #[cfg(windows)]
        let permit_slots = vec![
            child.lock().child.as_ref().expect("owned child").permits.clone(),
            master_permit.clone(),
            master_permit,
            reader_permit,
            drain_permit,
            writer_permit,
            Arc::new(Mutex::new(PermitCustody::default())),
            Arc::new(Mutex::new(PermitCustody::default())),
        ];
        #[cfg(not(windows))]
        let permit_slots = vec![master_permit, reader_permit, writer_permit];

        let (out_tx, out_rx) = pty_output_channel();
        let (in_tx, in_rx) = pty_input_channel();
        let (reader_cancel, reader_cancel_rx) = crossbeam_channel::bounded(1);
        let (writer_cancel, writer_cancel_rx) = crossbeam_channel::bounded(1);

        // Reader thread: pty -> out_rx.
        let output_meter = Arc::new(QueuedOutputMeter::default());
        let reader_thread = spawn_reader_thread(
            Box::new(reader),
            out_tx,
            reader_cancel_rx,
            output_meter.clone(),
            diagnostic_timestamps,
        );
        // Writer thread: in_rx -> pty.
        let queued_input_bytes = Arc::new(AtomicUsize::new(0));
        let writer_progress = Arc::new(PtyWriterProgress::new());
        let (replies, reply_reader) = reply_spool();
        let writer_thread = spawn_writer_thread(
            Box::new(writer),
            in_rx,
            writer_cancel_rx,
            queued_input_bytes.clone(),
            writer_progress.clone(),
            reply_reader,
        );

        let resize_master = master.clone();
        // Callers re-apply geometry on every tab activation, and each native
        // call is a ConPTY reflow or SIGWINCH, so unchanged sizes are skipped.
        let resize = resize_callback(Box::new(move |cols: u16, rows: u16| {
            let master = resize_master
                .try_lock_for(PTY_IO_SHUTDOWN_TIMEOUT)
                .ok_or_else(|| anyhow::anyhow!("PTY master custody is busy"))?;
            let native = master
                .as_ref()
                .and_then(|master| master.value.as_ref())
                .ok_or_else(|| anyhow::anyhow!("PTY master is closing"))?;
            native.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
            Ok(())
        }));
        {
            let mut workers = completion.0.workers.lock();
            workers.reader = Some(reader_thread);
            workers.writer = Some(writer_thread);
        }

        Ok(Self {
            out_rx,
            in_tx,
            queued_input_bytes,
            writer_progress: writer_progress.clone(),
            replies: replies.clone(),
            resize,
            teardown: Some(PtyTeardown {
                reader_cancel,
                writer_cancel,
                child,
                exit_observed,
                writer_progress,
                replies,
                master,
                #[cfg(windows)]
                conpty_drain_reader: Arc::new(Mutex::new(Some(drain_reader))),
                #[cfg(windows)]
                cancel_targets: std::array::from_fn(|_| Arc::new(Mutex::new(None))),
                permit_slots,
                permits_installed: false,
                resize: None,
                completion,
                spawner: Arc::new(ThreadWorkerSpawner),
                fallback_duplicate: None,
                direct_drop: true,
                pass_failed: false,
            }),
            shell_program_path: program_path,
            output_meter,
            process_identity,
        })
    }
}

/// Pid and start token of the retained, unreaped child; `None` when either is unavailable.
fn capture_process_identity(child: &ChildState) -> Option<crate::proc_info::ProcessIdentity> {
    let native = child.child.as_ref()?.value.as_ref()?;
    let pid = native.process_id()?;
    let start = process_start_for_platform(&**native, pid)?;
    Some(crate::proc_info::ProcessIdentity { pid, start })
}

/// macOS start token, read by pid while the caller holds the unreaped child.
#[cfg(target_os = "macos")]
fn process_start_for_platform(_native: &(dyn Child + Send + Sync), pid: u32) -> Option<u64> {
    crate::proc_info::process_start_token(pid)
}

/// Windows creation time, from the retained child handle; the pid is never reopened.
#[cfg(windows)]
fn process_start_for_platform(native: &(dyn Child + Send + Sync), _pid: u32) -> Option<u64> {
    use windows::Win32::{
        Foundation::{FILETIME, HANDLE},
        System::Threading::GetProcessTimes,
    };
    let raw = native.as_raw_handle()?;
    let mut creation = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let queried =
        // SAFETY: `raw` is the retained child's own process handle, held under its custody lock; all four outputs are writable.
        unsafe { GetProcessTimes(HANDLE(raw), &mut creation, &mut exited, &mut kernel, &mut user) };
    queried.ok()?;
    Some((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

/// No start token on other platforms, so no identity and no foreground worker.
#[cfg(not(any(target_os = "macos", windows)))]
fn process_start_for_platform(_native: &(dyn Child + Send + Sync), _pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
#[path = "pty_spawn_tests.rs"]
mod pty_spawn_tests;
