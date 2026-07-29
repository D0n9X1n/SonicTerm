use super::*;
#[cfg(windows)]
use crossbeam_channel::bounded;
use portable_pty::{ChildKiller, ExitStatus};

#[cfg(windows)]
static LIVE_PTY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(windows)]
fn lock_live_pty_test() -> std::sync::MutexGuard<'static, ()> {
    LIVE_PTY_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone)]
struct MockChild {
    try_wait_calls: Arc<AtomicUsize>,
    kill_calls: Arc<AtomicUsize>,
    events: Arc<Mutex<Vec<&'static str>>>,
}

impl ChildKiller for MockChild {
    fn kill(&mut self) -> std::io::Result<()> {
        self.kill_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(self.clone())
    }
}

impl Child for MockChild {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.try_wait_calls.fetch_add(1, Ordering::Relaxed);
        self.events.lock().push("wait");
        Ok(Some(ExitStatus::with_exit_code(0)))
    }

    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        Ok(ExitStatus::with_exit_code(0))
    }

    fn process_id(&self) -> Option<u32> {
        Some(4242)
    }

    #[cfg(windows)]
    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        None
    }
}

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

fn env_str<'a>(builder: &'a CommandBuilder, name: &str) -> &'a str {
    builder.get_env(name).and_then(|v| v.to_str()).unwrap()
}

#[test]
fn default_shell_spawn_opts_keep_sonicterm_term_program() {
    let opts = ShellSpawnOpts::default();
    assert_eq!(opts.term_program, ShellSpawnOpts::DEFAULT_TERM_PROGRAM);
}

#[test]
fn child_pty_env_uses_configured_term_program() {
    let mut builder = CommandBuilder::new("sh");
    apply_child_pty_env(&mut builder, "WezTerm");

    assert_eq!(env_str(&builder, "TERM"), "xterm-256color");
    assert_eq!(env_str(&builder, "COLORTERM"), "truecolor");
    assert_eq!(env_str(&builder, "TERM_PROGRAM"), "WezTerm");
    // Not SonicTerm's own version. See
    // `an_advertised_name_carries_a_version_that_names_gates_accept`.
    assert_eq!(env_str(&builder, "TERM_PROGRAM_VERSION"), "20230712-072601");
}

#[test]
fn running_as_ourselves_advertises_our_own_version() {
    let mut builder = CommandBuilder::new("sh");
    apply_child_pty_env(&mut builder, ShellSpawnOpts::DEFAULT_TERM_PROGRAM);

    assert_eq!(env_str(&builder, "TERM_PROGRAM"), "SonicTerm");
    assert_eq!(env_str(&builder, "TERM_PROGRAM_VERSION"), env!("CARGO_PKG_VERSION"));
}

#[test]
fn an_advertised_name_carries_a_version_that_names_gates_accept() {
    // A name/version pair has to be internally consistent to be useful:
    // programs gate features on the version *of the terminal the name
    // claims*. WezTerm versions by datestamp and consumers compare those
    // lexically, so a semver string sorts below every datestamp — the tool
    // takes its WezTerm branch on the name, then disables the features it
    // just decided the terminal was too old for.
    //
    // Asserting the pair rather than either field: checking only that the
    // version is non-empty, or only that the name is passed through, would
    // both pass against the mismatch this exists to prevent.
    let mut builder = CommandBuilder::new("sh");
    apply_child_pty_env(&mut builder, "WezTerm");
    let advertised = env_str(&builder, "TERM_PROGRAM_VERSION");

    // The threshold editors test for styled-underline support.
    const CONSUMER_GATE: &str = "20210203-095643";
    assert!(
        advertised > CONSUMER_GATE,
        "advertised WezTerm version {advertised:?} sorts below the gate \
         {CONSUMER_GATE:?}, so capability checks against the claimed identity fail"
    );

    // And SonicTerm's own version would NOT have cleared it, which is the
    // defect this replaced.
    assert!(
        env!("CARGO_PKG_VERSION") < CONSUMER_GATE,
        "this test is vacuous if our own version already cleared the gate"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windowsapps_filter_skips_user_alias_but_allows_store_package() {
    assert!(is_windowsapps_alias_stub_path(
        "c:\\users\\dotan\\appdata\\local\\microsoft\\windowsapps\\pwsh.exe"
    ));
    assert!(!is_windowsapps_alias_stub_path(
        "c:\\program files\\windowsapps\\microsoft.powershell_7.6.2.0_x64__8wekyb3d8bbwe\\pwsh.exe"
    ));
}

#[cfg(target_os = "windows")]
#[test]
fn powershell_interactive_args_force_utf8_codepage() {
    let args = interactive_shell_args("pwsh.exe");
    assert!(args.iter().any(|a| a == "-NoLogo"));
    assert!(args.iter().any(|a| a == "-NoExit"));
    let command = args.last().expect("command arg present");
    assert!(command.contains("InputEncoding"));
    assert!(command.contains("OutputEncoding"));
    assert!(command.contains("chcp 65001"));
}

#[test]
fn shell_spawn_opts_default_has_no_shell_override() {
    assert_eq!(ShellSpawnOpts::default().shell, None);
}

#[test]
fn resolve_spawn_shell_prefers_nonempty_override() {
    // An explicit, non-empty override wins verbatim (trimmed).
    assert_eq!(resolve_spawn_shell(Some("powershell.exe")), "powershell.exe");
    assert_eq!(resolve_spawn_shell(Some("  pwsh.exe  ")), "pwsh.exe");
    // None / empty / whitespace fall back to auto-detect (= default_shell()).
    assert_eq!(resolve_spawn_shell(None), default_shell());
    assert_eq!(resolve_spawn_shell(Some("")), default_shell());
    assert_eq!(resolve_spawn_shell(Some("   ")), default_shell());
}

#[cfg(target_os = "windows")]
#[test]
fn windows_default_shell_prefers_registered_pwsh_before_store_and_legacy() {
    let chosen = resolve_windows_default_shell_with(
        || None,
        || Some("registered-pwsh.exe".into()),
        || Some("store-pwsh.exe".into()),
        || Some("powershell.exe".into()),
    );
    assert_eq!(chosen, "registered-pwsh.exe");
}

#[cfg(target_os = "windows")]
#[test]
fn windows_default_shell_priority_and_fallback_are_stable() {
    assert_eq!(
        resolve_windows_default_shell_with(
            || Some("path-pwsh.exe".into()),
            || Some("registered-pwsh.exe".into()),
            || Some("store-pwsh.exe".into()),
            || Some("powershell.exe".into()),
        ),
        "path-pwsh.exe"
    );
    assert_eq!(resolve_windows_default_shell_with(|| None, || None, || None, || None), "cmd.exe");
}

#[cfg(target_os = "windows")]
#[test]
fn registered_pwsh_resolves_to_a_real_powershell_7_when_present() {
    let Some(path) = registered_pwsh() else { return };
    assert!(std::path::Path::new(&path).is_file());
    assert_eq!(shell_file_name(&path), "pwsh.exe");
}

#[cfg(target_os = "windows")]
#[test]
fn store_pkg_version_parses_dir_name() {
    use std::path::PathBuf;
    let p = PathBuf::from(
        r"C:\Program Files\WindowsApps\Microsoft.PowerShell_7.6.2.0_x64__8wekyb3d8bbwe\pwsh.exe",
    );
    assert_eq!(store_pkg_version(&p), [7, 6, 2, 0]);
    // Unparseable / missing version sorts to all-zero.
    let q = PathBuf::from(r"C:\nope\pwsh.exe");
    assert_eq!(store_pkg_version(&q), [0, 0, 0, 0]);
}

#[cfg(target_os = "windows")]
#[test]
fn pick_highest_pwsh_chooses_newest_version() {
    use std::path::PathBuf;
    let wa = r"C:\Program Files\WindowsApps";
    let candidates = vec![
        PathBuf::from(format!(r"{wa}\Microsoft.PowerShell_7.4.0.0_x64__8wekyb3d8bbwe\pwsh.exe")),
        PathBuf::from(format!(r"{wa}\Microsoft.PowerShell_7.6.2.0_x64__8wekyb3d8bbwe\pwsh.exe")),
        PathBuf::from(format!(r"{wa}\Microsoft.PowerShell_7.10.0.0_x64__8wekyb3d8bbwe\pwsh.exe")),
    ];
    let picked = pick_highest_pwsh(&candidates).expect("a candidate");
    // 7.10 must beat 7.6 (numeric, not lexical).
    assert!(picked.to_string_lossy().contains("7.10.0.0"));
    // Empty list -> None.
    assert_eq!(pick_highest_pwsh(&[]), None);
}

#[cfg(target_os = "macos")]
#[test]
fn production_macos_zsh_starts_as_login_shell() {
    assert_eq!(shell_startup_args("/bin/zsh", ShellSpawnOpts::default()), vec!["-l"]);
}

#[cfg(target_os = "macos")]
#[test]
fn production_macos_bash_and_fish_start_as_login_shells() {
    assert_eq!(shell_startup_args("/bin/bash", ShellSpawnOpts::default()), vec!["--login"]);
    assert_eq!(
        shell_startup_args("/opt/homebrew/bin/fish", ShellSpawnOpts::default()),
        vec!["--login"]
    );
}

#[cfg(target_os = "macos")]
#[test]
fn clean_e2e_keeps_profile_suppression() {
    let opts = ShellSpawnOpts { clean_e2e: true, ..ShellSpawnOpts::default() };
    assert_eq!(shell_startup_args("/bin/zsh", opts), vec!["-f"]);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn production_non_macos_shell_startup_stays_unchanged() {
    assert!(shell_startup_args("/bin/zsh", ShellSpawnOpts::default()).is_empty());
}

#[test]
fn applies_utf8_locale_when_launch_environment_has_no_locale() {
    assert!(should_apply_utf8_locale_fallback(None, None, None));
    assert_eq!(default_lang_utf8_locale(), "en_US.UTF-8");
    assert_eq!(default_lc_ctype_utf8_locale(), "UTF-8");
}

#[test]
fn skips_fallback_when_effective_locale_is_already_utf8() {
    assert!(!should_apply_utf8_locale_fallback(None, Some("UTF-8"), None));
    assert!(!should_apply_utf8_locale_fallback(None, None, Some("zh_CN.UTF-8")));
    assert!(!should_apply_utf8_locale_fallback(None, None, Some("en_US.UTF8")));
}

#[test]
fn fills_lc_ctype_when_lang_is_present_but_not_utf8() {
    assert!(should_apply_utf8_locale_fallback(None, None, Some("C")));
}

#[test]
fn preserves_explicit_lc_all_override() {
    assert!(!should_apply_utf8_locale_fallback(Some("C"), None, None));
}

#[test]
fn pty_output_queue_applies_backpressure_at_fixed_capacity() {
    let (tx, rx) = pty_output_channel();
    let meter = Arc::new(QueuedOutputMeter::default());
    for _ in 0..PTY_OUTPUT_QUEUE_CAPACITY {
        tx.try_send(PtyOutputChunk::for_test(b"x", PTY_READ_RING_BYTES, &meter))
            .expect("queue has bounded capacity");
    }

    assert!(matches!(
        tx.try_send(PtyOutputChunk::for_test(b"overflow", PTY_READ_RING_BYTES, &meter)),
        Err(crossbeam_channel::TrySendError::Full(_))
    ));
    assert_eq!(rx.len(), PTY_OUTPUT_QUEUE_CAPACITY);
}

#[test]
fn pty_output_send_can_be_cancelled_while_queue_is_full() {
    let (tx, _rx) = pty_output_channel();
    let meter = Arc::new(QueuedOutputMeter::default());
    for _ in 0..PTY_OUTPUT_QUEUE_CAPACITY {
        tx.try_send(PtyOutputChunk::for_test(b"x", PTY_READ_RING_BYTES, &meter))
            .expect("fill bounded queue");
    }
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let (done_tx, done_rx) = crossbeam_channel::bounded(1);
    let blocked = PtyOutputChunk::for_test(b"blocked", PTY_READ_RING_BYTES, &meter);
    std::thread::spawn(move || {
        done_tx.send(send_pty_output(&tx, &cancel_rx, blocked)).expect("report send outcome");
    });

    cancel_tx.send(()).expect("signal cancellation");

    assert!(!done_rx.recv_timeout(std::time::Duration::from_secs(1)).expect("reader unblocked"));
}

/// Many views into one ring cost one ring, not one ring per view.
///
/// This is the whole reason the figure is not a per-chunk sum: 64 keystroke
/// echoes are 64 views of one 64 KiB allocation, and counting them separately
/// would report 4 MiB for 64 bytes of output.
#[test]
fn views_sharing_one_ring_are_charged_once() {
    let meter = Arc::new(QueuedOutputMeter::default());
    let ring = RingCharge::new(PTY_READ_RING_BYTES, &meter);

    let chunks: Vec<PtyOutputChunk> = (0..PTY_OUTPUT_QUEUE_CAPACITY)
        .map(|_| PtyOutputChunk::new(Bytes::from_static(b"x"), ring.clone(), &meter))
        .collect();

    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), PTY_READ_RING_BYTES);
    assert_eq!(meter.payload_bytes.load(Ordering::Acquire), PTY_OUTPUT_QUEUE_CAPACITY);

    drop(chunks);
    drop(ring);
    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), 0);
    assert_eq!(meter.payload_bytes.load(Ordering::Acquire), 0);
}

/// Views into different rings add up.
#[test]
fn views_from_distinct_rings_each_cost_a_ring() {
    let meter = Arc::new(QueuedOutputMeter::default());

    let chunks: Vec<PtyOutputChunk> =
        (0..3).map(|_| PtyOutputChunk::for_test(b"x", PTY_READ_RING_BYTES, &meter)).collect();

    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), 3 * PTY_READ_RING_BYTES);

    drop(chunks);
    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), 0);
}

/// A ring stays charged until its last view goes, not its first.
///
/// The failure this rules out is releasing on the first drop: the allocation
/// is still held by every surviving view, so a figure that let go early would
/// report free memory the process cannot reuse.
#[test]
fn a_ring_is_released_only_when_its_last_view_drops() {
    let meter = Arc::new(QueuedOutputMeter::default());
    let ring = RingCharge::new(PTY_READ_RING_BYTES, &meter);

    let first = PtyOutputChunk::new(Bytes::from_static(b"first"), ring.clone(), &meter);
    let second = PtyOutputChunk::new(Bytes::from_static(b"second"), ring.clone(), &meter);
    drop(ring);

    drop(first);
    assert_eq!(
        meter.ring_bytes.load(Ordering::Acquire),
        PTY_READ_RING_BYTES,
        "the ring is still viewed by the second chunk"
    );
    assert_eq!(meter.payload_bytes.load(Ordering::Acquire), b"second".len());

    drop(second);
    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), 0);
    assert_eq!(meter.payload_bytes.load(Ordering::Acquire), 0);
}

/// The figures come back to zero when the queue drains.
#[test]
fn draining_the_queue_returns_both_figures_to_zero() {
    let (tx, rx) = pty_output_channel();
    let meter = Arc::new(QueuedOutputMeter::default());
    for _ in 0..PTY_OUTPUT_QUEUE_CAPACITY {
        tx.try_send(PtyOutputChunk::for_test(b"payload", PTY_READ_RING_BYTES, &meter))
            .expect("fill bounded queue");
    }

    assert!(meter.ring_bytes.load(Ordering::Acquire) > 0);

    while rx.try_recv().is_ok() {}

    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), 0);
    assert_eq!(meter.payload_bytes.load(Ordering::Acquire), 0);
}

/// The worst case is one ring per slot, and it is the queue's real ceiling.
#[test]
fn the_ring_ceiling_is_one_ring_per_slot() {
    assert_eq!(max_queued_output_ring_bytes(), PTY_OUTPUT_QUEUE_CAPACITY * PTY_READ_RING_BYTES);
    assert_eq!(max_queued_output_ring_bytes(), 4 * 1024 * 1024);
}

#[test]
fn pty_input_queue_has_fixed_capacity() {
    let (tx, rx) = pty_input_channel();
    for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
        tx.try_send(vec![b'x']).expect("queue has bounded capacity");
    }

    assert!(matches!(tx.try_send(vec![b'y']), Err(crossbeam_channel::TrySendError::Full(_))));
    assert_eq!(rx.len(), PTY_INPUT_QUEUE_CAPACITY);
    assert!(pty_input_message_allowed(MAX_PTY_INPUT_MESSAGE_BYTES));
    assert!(!pty_input_message_allowed(MAX_PTY_INPUT_MESSAGE_BYTES + 1));
}

#[test]
fn saturated_pty_input_queue_returns_the_rejected_bytes() {
    let (tx, _rx) = pty_input_channel();
    let queued = AtomicUsize::new(0);
    for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
        tx.try_send(vec![b'x']).expect("fill bounded input queue");
    }
    let rejected = vec![b'y'];

    assert!(matches!(
        try_queue_pty_input(&tx, &queued, rejected.clone()),
        Err(PtyInputError::QueueFull(bytes)) if bytes == rejected
    ));
    assert_eq!(
        queued.load(Ordering::Relaxed),
        0,
        "a message the queue refused was never queued memory, so it must not be counted"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn child_exit_probe_observes_short_lived_process() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    #[cfg(unix)]
    let command = "/usr/bin/true";
    #[cfg(windows)]
    let command = "whoami.exe";

    let pty = PtyHandle::spawn(command, 80, 24).expect("spawn short-lived process");
    let probe = pty.child_exit_probe();
    #[cfg(windows)]
    pty.send_input_nonblocking(b"\x1b[1;1R".to_vec()).expect("answer ConPTY cursor query");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut output = Vec::new();
    while !probe.has_exited().expect("probe child") && std::time::Instant::now() < deadline {
        while let Ok(chunk) = pty.out_rx.try_recv() {
            output.extend_from_slice(&chunk);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(
        probe.has_exited().expect("final child probe"),
        "short-lived child remained active; output={output:?}"
    );
}

#[cfg(unix)]
#[test]
fn observed_shell_exit_still_kills_background_process_group() {
    let args = vec!["-c".to_string(), "trap '' HUP; sleep 30 & echo $!; exit 0".to_string()];
    let pty = PtyHandle::spawn_with_args("/bin/sh", &args, 80, 24).expect("spawn shell");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut output = Vec::new();
    while !output.contains(&b'\n') && std::time::Instant::now() < deadline {
        match pty.out_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                panic!("PTY output closed before background pid")
            }
        }
    }
    let background_pid = String::from_utf8_lossy(&output)
        .trim()
        .parse::<libc::pid_t>()
        .expect("numeric background pid");
    let probe = pty.child_exit_probe();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !probe.has_exited().expect("probe shell") && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(probe.has_exited().expect("shell exited"));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while unsafe { libc::kill(background_pid, 0) } == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: signal 0 only probes existence and does not modify the process.
    assert_eq!(unsafe { libc::kill(background_pid, 0) }, -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    drop(pty);
}

#[test]
fn termination_signals_group_before_reaping_exited_leader() {
    let try_wait_calls = Arc::new(AtomicUsize::new(0));
    let kill_calls = Arc::new(AtomicUsize::new(0));
    let events = Arc::new(Mutex::new(Vec::new()));
    let child = MockChild {
        try_wait_calls: try_wait_calls.clone(),
        kill_calls: kill_calls.clone(),
        events: events.clone(),
    };
    let child = Arc::new(Mutex::new(ChildState::new(Box::new(child), Some(4242))));

    let mut signalled_groups = Vec::new();
    let mut signalled_pids = Vec::new();
    terminate_child(
        &mut child.lock(),
        |pgid| {
            events.lock().push("group");
            signalled_groups.push(pgid);
            Ok(())
        },
        |pid| signalled_pids.push(pid),
    )
    .unwrap();
    terminate_child(
        &mut child.lock(),
        |pgid| {
            signalled_groups.push(pgid);
            Ok(())
        },
        |pid| signalled_pids.push(pid),
    )
    .unwrap();

    assert_eq!(signalled_groups, [4242]);
    assert!(signalled_pids.is_empty());
    assert_eq!(kill_calls.load(Ordering::Relaxed), 0);
    assert_eq!(try_wait_calls.load(Ordering::Relaxed), 1);
    assert_eq!(*events.lock(), ["group", "wait"]);
}

#[test]
fn failed_session_cleanup_remains_retryable() {
    let child = MockChild {
        try_wait_calls: Arc::new(AtomicUsize::new(0)),
        kill_calls: Arc::new(AtomicUsize::new(0)),
        events: Arc::new(Mutex::new(Vec::new())),
    };
    let mut child = ChildState::new(Box::new(child), Some(4242));

    let first = signal_process_group(&mut child, |_| {
        Err(std::io::Error::other("process table unavailable"))
    });
    assert!(first.is_err());
    assert!(!child.process_group_signalled);

    signal_process_group(&mut child, |_| Ok(())).unwrap();
    assert!(child.process_group_signalled);
}

#[cfg(windows)]
struct CloseGatedReader {
    close_started: Receiver<()>,
    drained: Sender<()>,
    finished: bool,
}

#[cfg(windows)]
impl std::io::Read for CloseGatedReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.finished {
            return Ok(0);
        }
        self.close_started.recv_timeout(Duration::from_secs(1)).map_err(std::io::Error::other)?;
        buffer[0] = b'x';
        self.drained.send(()).map_err(std::io::Error::other)?;
        self.finished = true;
        Ok(1)
    }
}

#[cfg(windows)]
#[test]
fn conpty_close_runs_while_output_reader_is_draining() {
    let (close_started_tx, close_started_rx) = bounded(1);
    let (drained_tx, drained_rx) = bounded(1);
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let closed_thread = closed.clone();
    let reader = Box::new(CloseGatedReader {
        close_started: close_started_rx,
        drained: drained_tx,
        finished: false,
    });

    let completed = close_master_with_drain(
        reader,
        move || {
            close_started_tx.send(()).expect("start old-style close");
            drained_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("output must be drained during close");
            closed_thread.store(true, Ordering::Release);
        },
        Duration::from_secs(1),
    );

    assert!(completed, "old-style close must complete within its deadline");
    assert!(closed.load(Ordering::Acquire));
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

#[test]
fn multi_megabyte_paste_fits_bounded_input_budget() {
    const TWO_MIB: usize = 2 * 1024 * 1024;
    const MAX_QUEUED_INPUT_BYTES: usize = 64 * 1024 * 1024;

    assert!(pty_input_message_allowed(TWO_MIB));
    assert!(
        max_pty_queued_input_bytes() <= MAX_QUEUED_INPUT_BYTES,
        "supporting large pastes must not make queued input unbounded"
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

/// The typed send refuses a full queue; the raw one blocks.
///
/// Blocking is the consequence that matters. The reply forwarder holds this
/// sender in a thread whose stated reason for existing is that the VT loop
/// must never block pushing replies — and it held the raw channel, so a child
/// that stopped draining stalled the forwarder indefinitely.
#[test]
fn the_typed_send_refuses_a_full_queue_where_the_raw_channel_blocks() {
    use std::sync::mpsc;
    use std::time::Duration;

    let (tx, _rx) = crossbeam_channel::bounded::<Vec<u8>>(PTY_INPUT_QUEUE_CAPACITY);
    for _ in 0..PTY_INPUT_QUEUE_CAPACITY {
        tx.try_send(vec![0u8; 8]).expect("precondition: the queue fills");
    }

    assert!(
        matches!(
            try_queue_pty_input(&tx, &AtomicUsize::new(0), vec![0u8; 8]),
            Err(PtyInputError::QueueFull(_))
        ),
        "the typed path must refuse a full queue and hand the bytes back"
    );

    // The raw channel, for contrast: a send that never returns.
    let (done_tx, done_rx) = mpsc::channel();
    let raw = tx.clone();
    let blocked_thread = std::thread::spawn(move || {
        let _ = raw.send(vec![0u8; 8]);
        let _ = done_tx.send(());
    });
    assert!(
        done_rx.recv_timeout(Duration::from_millis(250)).is_err(),
        "precondition for the whole fix: a raw send on a full queue blocks"
    );

    // Let the blocked thread finish so the test does not leak it.
    drop(_rx);
    let _ = blocked_thread.join();
}

/// The typed send refuses an oversized message; the raw one accepts it.
#[test]
fn the_typed_send_enforces_the_message_cap_the_raw_channel_ignores() {
    let (tx, _rx) = crossbeam_channel::bounded::<Vec<u8>>(PTY_INPUT_QUEUE_CAPACITY);
    let oversized = vec![0u8; MAX_PTY_INPUT_MESSAGE_BYTES + 1];
    let queued = AtomicUsize::new(0);

    assert!(
        matches!(
            try_queue_pty_input(&tx, &queued, oversized.clone()),
            Err(PtyInputError::MessageTooLarge(_))
        ),
        "the typed path must refuse a message above the cap"
    );
    assert_eq!(
        queued.load(Ordering::Relaxed),
        0,
        "a message refused for size never entered the queue, so it must not be counted"
    );
    assert!(
        tx.try_send(oversized).is_ok(),
        "precondition: the raw channel accepts it, which is why the field is private"
    );
}

/// No first-party caller may hold the raw sender.
///
/// Asserted by scanning the sources rather than by types, because the property
/// is *absence* — a compiler check would only catch a caller that exists. Three
/// sites held the raw channel before this: two reply forwarders and the mux
/// input path, and only one of the three applied the cap.
#[test]
fn no_first_party_caller_reaches_the_raw_input_channel() {
    const SOURCES: &[(&str, &str)] = &[
        ("spawn_pane.rs", include_str!("../../sonicterm-app/src/app/spawn_pane.rs")),
        ("child_window.rs", include_str!("../../sonicterm-app/src/app/child_window.rs")),
        ("mux server.rs", include_str!("../../sonicterm-mux/src/server.rs")),
        ("app misc.rs", include_str!("../../sonicterm-app/src/app/misc.rs")),
    ];

    for (name, source) in SOURCES {
        // Code, not prose: a doc comment naming the old field is stale rather
        // than a bypass. Scanning raw text caught exactly that on first run,
        // which is worth keeping — a comment describing a seam that no longer
        // exists misleads the next reader — but it is a different defect.
        let code: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains(".in_tx"),
            "{name} reaches the raw PTY input channel; use `input_sender()` or \
             `send_input_nonblocking`, which apply the cap and refuse rather than block"
        );
    }
}

/// The cap lives in one place.
///
/// The mux path previously checked the size by hand and then sent on the raw
/// channel — correct, but a copy of the rule that had to stay in agreement
/// with the original. Two copies of a limit is the drift shape this milestone
/// exists to remove.
#[test]
fn the_message_cap_is_not_reimplemented_by_callers() {
    const MUX: &str = include_str!("../../sonicterm-mux/src/server.rs");
    assert!(
        !MUX.contains("pty_input_message_allowed"),
        "the mux input path must not restate the size check; `PtyInputSender::send` \
         applies it, and a second copy is a second thing to keep in agreement"
    );
}
