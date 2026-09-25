//! Real close-call and externally observed process-settlement measurements, with no latency gate.

use super::*;
use anyhow::{ensure, Context};
use std::io;
use std::sync::mpsc;
use std::thread;

/// Samples per native scenario, shared with the explicitly selected harness contract.
pub(super) const SAMPLES: usize = 20;
const SETUP_LIMIT: Duration = Duration::from_secs(4);
const SETTLEMENT_LIMIT: Duration = Duration::from_secs(4);
const CLEANUP_LIMIT: Duration = Duration::from_secs(2);
const OBSERVE_INTERVAL: Duration = Duration::from_millis(5);

/// Select the baseline's isolation independently of ordinary PTY tests.
pub(super) fn baseline_isolation_config() -> pty_test_support::IsolationConfig {
    // Harness allowance, not a production-close bound: the current native path still contains unbounded calls.
    // Twenty ordinary and twenty stalled samples reserve verified finite waits plus startup/scheduling room.
    let native_wait_allowance = Duration::from_millis(500) * 5 + Duration::from_secs(2) * 2;
    let close_and_observe = native_wait_allowance.max(SETTLEMENT_LIMIT + CLEANUP_LIMIT);
    let ordinary = SETUP_LIMIT + close_and_observe + CLEANUP_LIMIT;
    let stalled = SETUP_LIMIT * 2 + close_and_observe + CLEANUP_LIMIT;
    let envelope = (ordinary + stalled) * SAMPLES as u32 + Duration::from_secs(60);
    pty_test_support::IsolationConfig::baseline(envelope)
}

/// The native sample envelope accommodates every finite phase without extending ordinary isolation defaults.
#[test]
fn baseline_envelope_covers_all_censored_samples() {
    let config = baseline_isolation_config();
    // Cancellation shares one 500 ms deadline; reader, writer, termination retry and reap each have their own.
    // Close/drain each use 2 s. Observer completion overlaps that call, rather than adding another serial wait.
    let close_waits = Duration::from_millis(500) * 5 + Duration::from_secs(2) * 2;
    let observation = SETTLEMENT_LIMIT + CLEANUP_LIMIT;
    let concurrent_close = close_waits.max(observation);
    let ordinary = SETUP_LIMIT + concurrent_close + CLEANUP_LIMIT;
    let stalled = SETUP_LIMIT * 2 + concurrent_close + CLEANUP_LIMIT;
    let required = (ordinary + stalled) * SAMPLES as u32 + Duration::from_secs(60);
    assert_eq!(config.timeout, required, "the complete observation envelope must fit every sample");
    assert_eq!(config.timeout, Duration::from_secs(640));
    assert_eq!(config.output_limit, 1024 * 1024);
    assert!(config.complete_output);
    let ordinary = pty_test_support::IsolationConfig::ORDINARY;
    assert_eq!(ordinary.timeout, Duration::from_secs(60));
    assert_eq!(ordinary.output_limit, 64 * 1024);
    assert!(!ordinary.complete_output);
}

/// Measure the app-owned close call separately from native settlement in a killable child.
/// Unix leaders must disappear without this test reaping them; descendants may remain zombies if their new parent
/// does not reap them. Windows waits on retained process handles, including the externally identified console host.
#[test]
#[ignore = "observational real-PTY close baseline; explicitly selected by CI and the local gate"]
fn pty_close_baseline() {
    if pty_test_support::isolated_with_output(baseline_isolation_config()) {
        return;
    }
    let isolation = baseline_isolation_config();
    println!(
        "PTY_CLOSE_BASELINE configuration samples={SAMPLES} panes=1 settlement_limit_ms={} poll_ms={} isolation_seconds={} capture_bytes={} percentiles=nearest_rank caller=app_owning_test_thread",
        SETTLEMENT_LIMIT.as_millis(), OBSERVE_INTERVAL.as_millis(), isolation.timeout.as_secs(), isolation.output_limit
    );
    for scenario in ["ordinary", "stalled"] {
        let mut close_samples = Vec::new();
        let mut native_samples = Vec::new();
        for sample in 1..=SAMPLES {
            let (close, native) =
                measure_close(scenario, sample).expect("run real PTY baseline case");
            close_samples.push(close);
            native_samples.push(native);
        }
        report_summary(scenario, "close_call", close_samples);
        report_summary(scenario, "native_settlement", native_samples);
    }
}

/// An already-expired observation cannot turn a process that exited later into an in-budget sample.
#[test]
fn expired_settlement_observation_stays_censored() {
    use std::process::{Command, Stdio};
    let name = "app::close_baseline_tests::observer_process_fixture";
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start observation fixture");
    let process = NativeProcess::open(child.id(), "descendant").unwrap();
    child.kill().expect("stop observation fixture");
    // This is an observation-only helper, not a PTY shell whose reap belongs to SonicTerm teardown.
    child.wait().expect("reap observation fixture");
    let observed =
        observe_settlement(&[process], Instant::now() - SETTLEMENT_LIMIT - Duration::from_secs(1))
            .unwrap();
    assert_eq!(observed[0].elapsed, None, "a late observation must remain censored");
    assert_eq!(observed[0].state, "unobserved", "expired samples do not query native state");
}

/// Abandoning setup or a refused observer spawn must not leave an already identified fixture process running.
#[test]
fn abandoned_process_guard_terminates_its_fixture() {
    use std::process::{Command, Stdio};
    let name = "app::close_baseline_tests::observer_process_fixture";
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start abandoned fixture");
    let process = NativeProcess::open(child.id(), "descendant").unwrap();
    drop(process);
    let deadline = Instant::now() + CLEANUP_LIMIT;
    let mut exited = child.try_wait().unwrap().is_some();
    while !exited && Instant::now() < deadline {
        thread::sleep(OBSERVE_INTERVAL);
        exited = child.try_wait().unwrap().is_some();
    }
    if !exited {
        child.kill().expect("stop failed cleanup fixture");
        child.wait().expect("reap failed cleanup fixture");
    }
    assert!(exited, "abandoned native custody left its fixture running");
}

/// A zombie direct child is still an unreaped leader, while the same native state ends descendant execution.
#[cfg(unix)]
#[test]
fn zombie_leader_is_not_observed_as_reaped() {
    use std::process::{Command, Stdio};
    let name = "app::close_baseline_tests::observer_process_fixture";
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start zombie observation fixture");
    let process = NativeProcess::open(child.id(), "shell").unwrap();
    let descendant = NativeProcess::open(child.id(), "descendant").unwrap();
    child.kill().expect("stop zombie observation fixture");
    let deadline = Instant::now() + CLEANUP_LIMIT;
    let leader_state = loop {
        let state = process.observe();
        if !matches!(state, Ok((false, "running"))) || Instant::now() >= deadline {
            break state;
        }
        thread::sleep(OBSERVE_INTERVAL);
    };
    let descendant_state = descendant.observe();
    // This helper is not a PTY shell: reap only after the observer itself recorded zombie, absence, or failure.
    child.wait().expect("reap zombie observation fixture");
    assert_eq!(
        leader_state.unwrap(),
        (false, "zombie"),
        "only an observed zombie proves the leader remained unreaped"
    );
    assert_eq!(
        descendant_state.unwrap(),
        (true, "zombie"),
        "an exited descendant no longer executes"
    );
}

/// Keep the observation fixture alive until its owning test terminates it; it creates no descendants.
#[test]
#[ignore = "child process for deadline observation contract"]
fn observer_process_fixture() {
    use std::io::Read;
    let _ = std::io::stdin().read(&mut [0]);
}

#[derive(Clone, Copy)]
struct Sample {
    elapsed: Duration,
    censored: bool,
}

impl Sample {
    fn value(self) -> String {
        format!(
            "{}{:.3}",
            if self.censored { ">" } else { "" },
            self.elapsed.as_secs_f64() * 1000.0
        )
    }
}

fn report_summary(scenario: &str, measure: &str, samples: Vec<Sample>) {
    println!("{}", summary_line(scenario, measure, samples));
}

fn summary_line(scenario: &str, measure: &str, mut samples: Vec<Sample>) -> String {
    samples.sort_by_key(|sample| sample.elapsed);
    let censored = samples.iter().filter(|sample| sample.censored).count();
    let percentile = |rank: usize| samples[(rank * samples.len()).div_ceil(100) - 1].value();
    format!(
        "PTY_CLOSE_BASELINE summary scenario={scenario} measure={measure} samples={} censored={censored} p50_ms={} p95_ms={} max_ms={}",
        samples.len(), percentile(50), percentile(95), samples.last().unwrap().value()
    )
}

/// A summary names censored observations and preserves their lower-bound marker instead of inventing a duration.
#[test]
fn summary_preserves_censored_count_and_bounds() {
    let summary = summary_line(
        "controlled",
        "native_settlement",
        vec![
            Sample { elapsed: Duration::from_millis(1), censored: false },
            Sample { elapsed: SETTLEMENT_LIMIT, censored: true },
        ],
    );
    assert!(summary.contains("samples=2 censored=1"), "{summary}");
    assert!(summary.contains("p95_ms=>4000.000 max_ms=>4000.000"), "{summary}");
}

fn measure_close(scenario: &str, sample: usize) -> Result<(Sample, Sample)> {
    pty_test_support::phase(sample as u64, "baseline-setup");
    let (mut app, pane, processes, slave) = prepare_pane(scenario)?;
    let (start_tx, start_rx) = mpsc::sync_channel(1);
    let observer =
        thread::Builder::new().name("pty-baseline-observer".into()).spawn(move || {
            let start = start_rx.recv_timeout(SETUP_LIMIT).context("close observer start")?;
            let result = observe_settlement(&processes, start);
            Ok::<_, anyhow::Error>((processes, result))
        })?;
    pty_test_support::phase(pane, "baseline-close-begin");
    let start = Instant::now();
    start_tx.try_send(start).context("start close observer")?;
    ensure!(app.close_pty_pane(pane), "baseline pane was not closed");
    let close = Sample { elapsed: start.elapsed(), censored: false };
    pty_test_support::phase(pane, "baseline-close-end");
    // The external slave stays held through observation even when a future managed close returns immediately.
    let observer_deadline = start + SETTLEMENT_LIMIT + CLEANUP_LIMIT;
    while !observer.is_finished() && Instant::now() < observer_deadline {
        thread::sleep(OBSERVE_INTERVAL);
    }
    ensure!(observer.is_finished(), "native process observer did not finish");
    let (processes, observation) =
        observer.join().map_err(|_| anyhow::anyhow!("observer panicked"))??;
    let observed = observation?;
    let native = observed
        .iter()
        .map(|observation| observation.elapsed)
        .collect::<Option<Vec<_>>>()
        .map_or(Sample { elapsed: SETTLEMENT_LIMIT, censored: true }, |times| Sample {
            elapsed: times.into_iter().max().unwrap_or_default(),
            censored: false,
        });
    for (process, observation) in processes.iter().zip(observed) {
        let value = Sample {
            elapsed: observation.elapsed.unwrap_or(SETTLEMENT_LIMIT),
            censored: observation.elapsed.is_none(),
        };
        println!(
            "PTY_CLOSE_BASELINE process scenario={scenario} sample={sample} role={} pid={} identity={} observation={} state={} status={} elapsed_ms={}",
            process.role, process.pid, process.identity(), process.observation(), observation.state,
            if value.censored { "exceeded_limit" } else { "settled" }, value.value()
        );
    }
    for (measure, value) in [("close_call", close), ("native_settlement", native)] {
        println!(
            "PTY_CLOSE_BASELINE sample scenario={scenario} measure={measure} sample={sample} path=synchronous_drop panes=1 status={} elapsed_ms={}",
            if value.censored { "exceeded_limit" } else { "observed" }, value.value()
        );
    }
    // Cleanup starts after both measurements: releasing the slave or killing survivors earlier would alter settlement.
    drop(slave);
    cleanup_processes(&processes)?;
    if processes.iter().find(|process| process.role == "shell").unwrap().settled()? {
        pty_test_support::record_process(processes[0].pid, false);
    }
    drop(app);
    Ok((close, native))
}

#[derive(Clone)]
struct ProcessObservation {
    elapsed: Option<Duration>,
    state: &'static str,
}

fn observe_settlement(
    processes: &[NativeProcess],
    start: Instant,
) -> Result<Vec<ProcessObservation>> {
    let mut observed =
        vec![ProcessObservation { elapsed: None, state: "unobserved" }; processes.len()];
    while start.elapsed() < SETTLEMENT_LIMIT {
        for (process, observation) in processes.iter().zip(&mut observed) {
            if observation.elapsed.is_none() {
                let (settled, state) = process.observe()?;
                observation.state = state;
                let checked_at = start.elapsed();
                if settled && checked_at < SETTLEMENT_LIMIT {
                    observation.elapsed = Some(checked_at);
                }
            }
        }
        if observed.iter().all(|observation| observation.elapsed.is_some()) {
            break;
        }
        thread::sleep(OBSERVE_INTERVAL.min(SETTLEMENT_LIMIT.saturating_sub(start.elapsed())));
    }
    Ok(observed)
}

fn cleanup_processes(processes: &[NativeProcess]) -> Result<()> {
    for process in processes.iter().rev() {
        process.terminate_if_running()?;
    }
    let deadline = Instant::now() + CLEANUP_LIMIT;
    loop {
        if processes
            .iter()
            .map(NativeProcess::running)
            .collect::<Result<Vec<_>>>()?
            .iter()
            .all(|running| !running)
        {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "could not clean the baseline fixture's surviving processes"
        );
        thread::sleep(OBSERVE_INTERVAL);
    }
}

/// Keep all pane construction here so managed retirement can replace this synchronous baseline path without
/// changing the shell, pane count, native scenarios, or external settlement observer.
fn prepare_pane(scenario: &str) -> Result<(App, u64, Vec<NativeProcess>, Option<std::fs::File>)> {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default())
        .with_capture_staging_pool(CaptureStagingPool::new())
        .with_inline_media_pool(media::InlineMediaPool::new());
    let pane = app.__test_seed_tab("close baseline");
    #[cfg(windows)]
    let before: BTreeSet<_> = process_snapshot()?.into_iter().map(|entry| entry.pid).collect();
    #[cfg(windows)]
    let app_inspection = ProcessInspection::open(std::process::id())?;
    #[cfg(windows)]
    let fixture_started = fixture_birth_boundary();
    #[cfg(windows)]
    let pty = PtyHandle::spawn_with_args("cmd.exe", &["/D".into(), "/Q".into()], 80, 24)?;
    #[cfg(unix)]
    let pty = PtyHandle::spawn_with_args(
        "/bin/sh",
        &["-c".into(), "trap '' HUP; sleep 120 & child=$!; printf 'SONICTERM_CLOSE_READY:%s:%s:%s\\n' \"$$\" \"$child\" \"$(tty)\"; while IFS= read -r line; do :; done".into()],
        80,
        24,
    )?;
    let shell = pty.pid().context("new baseline shell PID")?;
    pty_test_support::record_process(shell, true);
    let mut processes = vec![NativeProcess::open(shell, "shell")?];
    #[cfg(windows)]
    let slave = {
        pty.send_input_nonblocking(
            b"\x1b[1;1Rstart \"\" /B ping.exe -t 127.0.0.1 >NUL\r\n".to_vec(),
        )
        .map_err(|error| anyhow::anyhow!("start baseline descendant: {error:?}"))?;
        let deadline = Instant::now() + SETUP_LIMIT;
        loop {
            let snapshot = process_snapshot()?;
            let hosts: Vec<_> = snapshot
                .iter()
                .filter(|entry| {
                    entry.parent == std::process::id()
                        && (entry.name.eq_ignore_ascii_case("conhost.exe")
                            || entry.name.eq_ignore_ascii_case("OpenConsole.exe"))
                        && !before.contains(&entry.pid)
                })
                .collect();
            let descendants: Vec<_> = snapshot
                .iter()
                .filter(|entry| entry.parent == shell && !before.contains(&entry.pid))
                .collect();
            for child in &descendants {
                if !processes.iter().any(|process| process.pid == child.pid) {
                    if let Some(owned) = admit_process_candidate(
                        child,
                        ProcessInspection::open(child.pid)?,
                        shell,
                        processes[0].created,
                        &before,
                        "descendant",
                    )? {
                        processes.push(owned);
                    }
                }
            }
            for host in &hosts {
                if !processes.iter().any(|process| process.pid == host.pid) {
                    let origin = fixture_started.max(app_inspection.created);
                    if let Some(owned) = admit_process_candidate(
                        host,
                        ProcessInspection::open(host.pid)?,
                        app_inspection.pid,
                        origin,
                        &before,
                        "conhost",
                    )? {
                        println!(
                            "PTY_CLOSE_BASELINE console_host pid={} parent={} test_pid={} name={} identity={} fixture_start={}",
                            host.pid, host.parent, std::process::id(), host.name, owned.created, fixture_started
                        );
                        processes.push(owned);
                    }
                }
            }
            let owned_host_count =
                processes.iter().filter(|process| process.role == "conhost").count();
            let owned_descendant = descendants.iter().any(|entry| {
                entry.name.eq_ignore_ascii_case("ping.exe")
                    && processes
                        .iter()
                        .any(|process| process.pid == entry.pid && process.role == "descendant")
            });
            if owned_host_count == 1 && owned_descendant {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "could not identify one new console host and the shell descendant"
            );
            // Discard only the setup output currently queued; a concurrent producer cannot extend this loop.
            for _ in 0..pty.out_rx.len() {
                let _ = pty.out_rx.try_recv();
            }
            thread::sleep(OBSERVE_INTERVAL);
        }
        if scenario == "stalled" {
            pty.send_input_nonblocking(
                format!("for /L %i in (1,1,2147483647) do @echo {}\r\n", "X".repeat(1024))
                    .into_bytes(),
            )
            .map_err(|error| anyhow::anyhow!("start ConPTY flood: {error:?}"))?;
            let deadline = Instant::now() + SETUP_LIMIT;
            while !pty.out_rx.is_full() {
                ensure!(Instant::now() < deadline, "ConPTY flood did not fill its output queue");
                thread::sleep(OBSERVE_INTERVAL);
            }
            println!("PTY_CLOSE_BASELINE stall=conpty_flood queued_chunks={}", pty.out_rx.len());
        }
        None
    };
    #[cfg(unix)]
    let slave = {
        let deadline = Instant::now() + SETUP_LIMIT;
        let mut output = Vec::new();
        let (descendant, path) = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "shell did not report its descendant and slave");
            let chunk =
                pty.out_rx.recv_timeout(remaining).context("baseline shell ready marker")?;
            output.extend_from_slice(&chunk);
            ensure!(output.len() < 65536, "baseline setup output exceeded its bound");
            let text = String::from_utf8_lossy(&output);
            if text.ends_with('\n') {
                if let Some(line) =
                    text.lines().find(|line| line.starts_with("SONICTERM_CLOSE_READY:"))
                {
                    let fields: Vec<_> = line.trim().split(':').collect();
                    ensure!(
                        fields.len() == 4 && fields[1].parse::<u32>()? == shell,
                        "invalid shell identity marker"
                    );
                    break (fields[2].parse::<u32>()?, fields[3].to_string());
                }
            }
        };
        processes.push(NativeProcess::open(descendant, "descendant")?);
        if scenario == "stalled" {
            use std::os::unix::fs::OpenOptionsExt;
            let outside =
                // SAFETY: getsid queries existing process identities and changes no session membership.
                unsafe { libc::getsid(0) != libc::getsid(shell as libc::pid_t) };
            ensure!(outside, "slave holder must be outside the PTY shell session");
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
                .open(&path)?;
            println!(
                "PTY_CLOSE_BASELINE stall=external_slave_held shell={shell} holder={} slave={path}",
                std::process::id()
            );
            Some(file)
        } else {
            None
        }
    };
    ensure!(app.__test_set_pane_pty(pane, Some(pty)), "baseline pane installation refused");
    Ok((app, pane, processes, slave))
}

#[cfg(windows)]
struct NativeProcess {
    pid: u32,
    role: &'static str,
    handle: std::os::windows::io::OwnedHandle,
    created: u64,
}

/// Inspection owns only handle closure. Refusal and query errors never acquire termination custody.
#[cfg(windows)]
struct ProcessInspection {
    pid: u32,
    handle: std::os::windows::io::OwnedHandle,
    created: u64,
}

#[cfg(windows)]
impl ProcessInspection {
    fn open(pid: u32) -> Result<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::{Foundation::FILETIME, System::Threading::*};
        let handle =
            // SAFETY: pid is queried by value; the returned handle is immediately placed in a close-only RAII owner.
            unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, false, pid)? };
        let owned =
            // SAFETY: handle is a newly owned process handle, transferred exactly once to OwnedHandle.
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.0) };
        let (mut created, mut exited, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: owned keeps handle open; every output points to writable FILETIME storage.
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user)? };
        Ok(Self { pid, handle: owned, created: filetime_value(created) })
    }

    fn into_owned(self, role: &'static str) -> NativeProcess {
        NativeProcess { pid: self.pid, role, handle: self.handle, created: self.created }
    }
}

#[cfg(windows)]
fn filetime_value(time: windows::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

#[cfg(windows)]
fn fixture_birth_boundary() -> u64 {
    let now =
        // SAFETY: this value-only query returns the current FILETIME in the same units as process creation times.
        unsafe { windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime() };
    filetime_value(now)
}

#[cfg(windows)]
impl NativeProcess {
    /// Only directly spawned fixture processes use this entry; discovered candidates must pass admission first.
    fn open(pid: u32, role: &'static str) -> Result<Self> {
        Ok(ProcessInspection::open(pid)?.into_owned(role))
    }

    fn raw(&self) -> windows::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;
        windows::Win32::Foundation::HANDLE(self.handle.as_raw_handle())
    }

    fn running(&self) -> Result<bool> {
        use windows::Win32::{
            Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::WaitForSingleObject,
        };
        let state =
            // SAFETY: this retained handle pins the process identity; a zero timeout only observes its state.
            unsafe { WaitForSingleObject(self.raw(), 0) };
        match state {
            WAIT_OBJECT_0 => Ok(false),
            WAIT_TIMEOUT => Ok(true),
            _ => Err(io::Error::last_os_error().into()),
        }
    }

    fn observe(&self) -> Result<(bool, &'static str)> {
        let running = self.running()?;
        Ok((!running, if running { "running" } else { "signaled" }))
    }
    fn settled(&self) -> Result<bool> {
        Ok(self.observe()?.0)
    }
    fn identity(&self) -> String {
        self.created.to_string()
    }
    fn observation(&self) -> &'static str {
        "signaled_handle"
    }

    fn terminate_if_running(&self) -> Result<()> {
        if self.running()? {
            let result =
                // SAFETY: the handle belongs to this fixture process and remains retained across termination.
                unsafe { windows::Win32::System::Threading::TerminateProcess(self.raw(), 1) };
            if self.running()? {
                result?;
            }
        }
        Ok(())
    }
}

// Lifecycle: the observation handle is also fixture cleanup custody on setup failure, panic, or refused spawn.
impl Drop for NativeProcess {
    fn drop(&mut self) {
        if let Err(error) = self.terminate_if_running() {
            eprintln!("PTY_CLOSE_BASELINE cleanup_error pid={} error={error}", self.pid);
        }
        let deadline = Instant::now() + CLEANUP_LIMIT;
        while self.running().unwrap_or(false) && Instant::now() < deadline {
            thread::sleep(OBSERVE_INTERVAL);
        }
    }
}

#[cfg(windows)]
struct ProcessEntry {
    pid: u32,
    parent: u32,
    name: String,
}

/// Validate discovered ownership before moving the same close-only handle into termination custody.
#[cfg(windows)]
fn admit_process_candidate(
    candidate: &ProcessEntry,
    inspection: ProcessInspection,
    expected_parent: u32,
    not_before: u64,
    preexisting: &BTreeSet<u32>,
    role: &'static str,
) -> Result<Option<NativeProcess>> {
    if candidate.pid != inspection.pid
        || candidate.parent != expected_parent
        || preexisting.contains(&candidate.pid)
    {
        return Ok(None);
    }
    if !candidate_has_owned_origin(
        candidate,
        expected_parent,
        inspection.created,
        not_before,
        preexisting,
    ) {
        return Ok(None);
    }
    // Keep the inspected handle alive across the table recheck and transfer this handle, never a PID reopen.
    let current = process_snapshot()?;
    let Some(confirmed) = current.iter().find(|entry| entry.pid == inspection.pid) else {
        return Ok(None);
    };
    if confirmed.parent != expected_parent || !confirmed.name.eq_ignore_ascii_case(&candidate.name)
    {
        return Ok(None);
    }
    Ok(Some(inspection.into_owned(role)))
}

/// Metadata-only discovered-process admission predicate; no process is opened by these checks.
#[cfg(windows)]
fn candidate_has_owned_origin(
    candidate: &ProcessEntry,
    expected_parent: u32,
    created: u64,
    not_before: u64,
    preexisting: &BTreeSet<u32>,
) -> bool {
    candidate.parent == expected_parent
        && !preexisting.contains(&candidate.pid)
        && created >= not_before
}

/// A stale numeric parent must never give cleanup ownership of a process present before this fixture started.
#[cfg(windows)]
#[test]
fn candidate_admission_rejects_stale_parent_and_reused_pid() {
    let candidate = ProcessEntry { pid: 7, parent: 11, name: "ping.exe".into() };
    assert!(!candidate_has_owned_origin(&candidate, 11, 120, 100, &BTreeSet::from([7])));
}

/// Even without a snapshot hit, creation before the shell excludes a stale child of a recycled shell PID.
#[cfg(windows)]
#[test]
fn candidate_admission_rejects_creation_before_fixture_origin() {
    let candidate = ProcessEntry { pid: 7, parent: 11, name: "ping.exe".into() };
    assert!(!candidate_has_owned_origin(&candidate, 11, 99, 100, &BTreeSet::new()));
}

/// A rejected discovered candidate only closes its inspection handle; the owned test child must remain running.
#[cfg(windows)]
#[test]
fn candidate_admission_rejection_preserves_the_live_test_child() {
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::close_baseline_tests::observer_process_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start candidate rejection fixture");
    let candidate =
        process_snapshot().unwrap().into_iter().find(|entry| entry.pid == child.id()).unwrap();
    let inspection = ProcessInspection::open(child.id()).unwrap();
    let birth = inspection.created;
    let refused = admit_process_candidate(
        &candidate,
        inspection,
        std::process::id(),
        birth + 1,
        &BTreeSet::new(),
        "descendant",
    );
    let survived = child.try_wait().unwrap().is_none();
    // This child belongs to the test; cleanup follows the observation even when an assertion will fail.
    if survived {
        child.kill().expect("stop candidate rejection fixture");
    }
    child.wait().expect("reap candidate rejection fixture");
    assert!(refused.unwrap().is_none(), "older candidate was adopted");
    assert!(survived, "a rejected inspection acquired termination custody");
}

/// Actual admission must retain the opened kernel handle through its live snapshot recheck, without a PID reopen.
#[cfg(windows)]
#[test]
fn candidate_admission_transfers_the_same_inspection_handle() {
    if pty_test_support::isolated() {
        return;
    }
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::close_baseline_tests::observer_process_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start handle transfer fixture");
    let candidate =
        process_snapshot().unwrap().into_iter().find(|entry| entry.pid == child.id()).unwrap();
    let inspection = ProcessInspection::open(child.id()).unwrap();
    let raw = inspection.handle.as_raw_handle();
    let birth = inspection.created;
    let owned = admit_process_candidate(
        &candidate,
        inspection,
        std::process::id(),
        birth,
        &BTreeSet::new(),
        "descendant",
    )
    .expect("inspect owned candidate")
    .expect("accept owned candidate");
    let accepted_raw = owned.handle.as_raw_handle();
    // Cleanup precedes the assertion, so even a second-handle acceptance cannot leak this directly owned child.
    drop(owned);
    child.wait().expect("reap handle transfer fixture");
    assert_eq!(accepted_raw, raw, "admission replaced the already-open inspection handle");
}

/// A live inspection cannot borrow another candidate's metadata to acquire termination custody.
#[cfg(windows)]
#[test]
fn candidate_admission_rejects_mismatched_inspection_pid() {
    if pty_test_support::isolated() {
        return;
    }
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::close_baseline_tests::observer_process_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start inspection identity fixture");
    let mut candidate =
        process_snapshot().unwrap().into_iter().find(|entry| entry.pid == child.id()).unwrap();
    // Only this owned child is opened; the synthetic PID must fail before metadata can grant custody.
    candidate.pid = 0;
    let inspection = ProcessInspection::open(child.id()).unwrap();
    let birth = inspection.created;
    let result = admit_process_candidate(
        &candidate,
        inspection,
        std::process::id(),
        birth,
        &BTreeSet::new(),
        "descendant",
    );
    let refused = matches!(&result, Ok(None));
    drop(result);
    let survived = child.try_wait().unwrap().is_none();
    if survived {
        child.kill().expect("stop inspection identity fixture");
    }
    child.wait().expect("reap inspection identity fixture");
    assert!(refused, "mismatched candidate and inspection were admitted");
    assert!(survived, "mismatched inspection acquired termination custody");
}

/// Admission rechecks the live snapshot; a mismatched image name must reject custody without stopping the child.
#[cfg(windows)]
#[test]
fn candidate_admission_recheck_rejects_mismatched_name_without_termination() {
    if pty_test_support::isolated() {
        return;
    }
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::close_baseline_tests::observer_process_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start snapshot recheck fixture");
    let mut candidate =
        process_snapshot().unwrap().into_iter().find(|entry| entry.pid == child.id()).unwrap();
    // Keep PID, parent and birth valid so only the second live snapshot can reject this changed name.
    candidate.name.push_str(".mismatch");
    let inspection = ProcessInspection::open(child.id()).unwrap();
    let birth = inspection.created;
    let result = admit_process_candidate(
        &candidate,
        inspection,
        std::process::id(),
        birth,
        &BTreeSet::new(),
        "descendant",
    );
    let refused = matches!(&result, Ok(None));
    drop(result);
    let survived = child.try_wait().unwrap().is_none();
    if survived {
        child.kill().expect("stop snapshot recheck fixture");
    }
    child.wait().expect("reap snapshot recheck fixture");
    assert!(refused, "snapshot name disagreement was admitted");
    assert!(survived, "snapshot rejection acquired termination custody");
}

/// Candidate metadata cannot replace the live parent check; refusal leaves the directly owned child running.
#[cfg(windows)]
#[test]
fn candidate_admission_recheck_rejects_mismatched_parent_without_termination() {
    if pty_test_support::isolated() {
        return;
    }
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::close_baseline_tests::observer_process_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start parent recheck fixture");
    let mut candidate =
        process_snapshot().unwrap().into_iter().find(|entry| entry.pid == child.id()).unwrap();
    let incorrect_parent = child.id();
    assert_ne!(candidate.parent, incorrect_parent, "fixture child cannot be its own parent");
    // Both claimed parents agree so the preliminary predicate passes; only the live parent comparison can refuse.
    candidate.parent = incorrect_parent;
    let inspection = ProcessInspection::open(child.id()).unwrap();
    let birth = inspection.created;
    let result = admit_process_candidate(
        &candidate,
        inspection,
        incorrect_parent,
        birth,
        &BTreeSet::new(),
        "descendant",
    );
    let refused = matches!(&result, Ok(None));
    drop(result);
    let survived = child.try_wait().unwrap().is_none();
    if survived {
        child.kill().expect("stop parent recheck fixture");
    }
    child.wait().expect("reap parent recheck fixture");
    assert!(refused, "live snapshot parent disagreement was admitted");
    assert!(survived, "parent rejection acquired termination custody");
}

/// A console host can precede the shell, but it must follow the pre-spawn fixture boundary and name our parent.
#[cfg(windows)]
#[test]
fn console_host_admission_uses_fixture_boundary_not_shell_birth() {
    let host = ProcessEntry { pid: 7, parent: 11, name: "conhost.exe".into() };
    assert!(candidate_has_owned_origin(&host, 11, 110, 100, &BTreeSet::new()));
    assert!(!candidate_has_owned_origin(&host, 11, 90, 100, &BTreeSet::new()));
    assert!(!candidate_has_owned_origin(&host, 12, 110, 100, &BTreeSet::new()));
}

#[cfg(windows)]
fn process_snapshot() -> Result<Vec<ProcessEntry>> {
    use std::os::windows::io::FromRawHandle;
    use windows::Win32::{Foundation::ERROR_NO_MORE_FILES, System::Diagnostics::ToolHelp::*};
    let snapshot =
        // SAFETY: snapshot takes only flags; its returned handle is immediately given one RAII owner.
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? };
    let _owned =
        // SAFETY: snapshot is a valid owned handle transferred from CreateToolhelp32Snapshot.
        unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(snapshot.0) };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: entry advertises its writable size and snapshot remains open through enumeration.
    unsafe { Process32FirstW(snapshot, &mut entry)? };
    let mut entries = Vec::new();
    loop {
        let end =
            entry.szExeFile.iter().position(|value| *value == 0).unwrap_or(entry.szExeFile.len());
        entries.push(ProcessEntry {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            name: String::from_utf16_lossy(&entry.szExeFile[..end]),
        });
        ensure!(entries.len() < 65536, "process snapshot exceeded fixture bound");
        let next =
            // SAFETY: entry remains sized writable storage and the snapshot guard has not been dropped.
            unsafe { Process32NextW(snapshot, &mut entry) };
        match next {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(entries),
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(unix)]
struct NativeProcess {
    pid: u32,
    role: &'static str,
    started: (u64, u64),
}

#[cfg(unix)]
impl NativeProcess {
    fn open(pid: u32, role: &'static str) -> Result<Self> {
        let (started, zombie) =
            unix_identity(pid)?.context("fixture process disappeared during setup")?;
        ensure!(!zombie, "fixture process exited during setup");
        Ok(Self { pid, role, started })
    }
    fn identity(&self) -> String {
        format!("{}:{}", self.started.0, self.started.1)
    }
    fn observation(&self) -> &'static str {
        if self.role == "shell" {
            "reaped"
        } else {
            "exited_or_zombie"
        }
    }
    fn running(&self) -> Result<bool> {
        Ok(unix_identity(self.pid)?.is_some_and(|(start, zombie)| start == self.started && !zombie))
    }
    fn observe(&self) -> Result<(bool, &'static str)> {
        Ok(match unix_identity(self.pid)? {
            None => (true, "absent"),
            Some((start, _)) if start != self.started => (true, "identity_replaced"),
            Some((_, true)) => (self.role != "shell", "zombie"),
            Some((_, false)) => (false, "running"),
        })
    }
    fn settled(&self) -> Result<bool> {
        Ok(self.observe()?.0)
    }
    fn terminate_if_running(&self) -> Result<()> {
        if self.running()? {
            let result =
                // SAFETY: this PID's start time was matched to the fixture immediately before signaling it.
                unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) };
            if result != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                return Err(io::Error::last_os_error().into());
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn unix_identity(pid: u32) -> Result<Option<((u64, u64), bool)>> {
    let text = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => text,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error.into()),
    };
    let (_, suffix) = text.rsplit_once(") ").context("process stat fields")?;
    let fields: Vec<_> = suffix.split_whitespace().collect();
    ensure!(fields.len() > 19, "truncated process stat");
    Ok(Some(((fields[19].parse()?, 0), matches!(fields[0], "Z" | "X" | "x"))))
}

#[cfg(target_os = "macos")]
fn unix_identity(pid: u32) -> Result<Option<((u64, u64), bool)>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    // Include zombies so an unreaped leader cannot appear absent merely because it no longer executes.
    let read =
        // SAFETY: info is writable, correctly aligned storage for the exact advertised proc_bsdinfo size.
        unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            1,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if read == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(error.into());
    }
    ensure!(read == size as i32, "incomplete process identity for {pid}");
    let info =
        // SAFETY: proc_pidinfo initialized the complete structure as verified by its exact return size.
        unsafe { info.assume_init() };
    Ok(Some(((info.pbi_start_tvsec, info.pbi_start_tvusec), info.pbi_status == libc::SZOMB)))
}
