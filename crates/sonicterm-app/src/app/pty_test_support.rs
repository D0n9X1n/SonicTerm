//! Bound live-PTY tests in exact-name subprocesses and retain native phases only on failure.

use std::{
    io::{Read, Write},
    process::{Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const CHILD_MARKER: &str = "SONICTERM_PTY_TEST_CHILD";
const OUTPUT_LIMIT: usize = 64 * 1024;

/// Deadline and capture policy for one isolated test child.
#[derive(Clone, Copy)]
pub(super) struct IsolationConfig {
    /// Runtime envelope, excluding compilation before this process starts.
    pub(super) timeout: Duration,
    /// Combined stdout/stderr bytes retained while both pipes continue draining.
    pub(super) output_limit: usize,
    /// Complete captures reject overflow rather than accepting a truncated diagnostic tail.
    pub(super) complete_output: bool,
}

impl IsolationConfig {
    /// Preserve the ordinary PTY tests' deadline and diagnostic-tail behavior.
    pub(super) const ORDINARY: Self = Self {
        timeout: Duration::from_secs(60),
        output_limit: OUTPUT_LIMIT,
        complete_output: false,
    };

    /// Preserve every baseline record within a finite capture budget.
    pub(super) const fn baseline(timeout: Duration) -> Self {
        Self { timeout, output_limit: 1024 * 1024, complete_output: true }
    }
}

/// Run the calling test once in a killable process, without recursive test spawning.
pub(super) fn isolated() -> bool {
    isolate_with_output(IsolationConfig::ORDINARY, false)
}

/// Run a baseline with its explicit envelope and forward successful complete output.
pub(super) fn isolated_with_output(config: IsolationConfig) -> bool {
    isolate_with_output(config, true)
}

fn isolate_with_output(config: IsolationConfig, forward_output: bool) -> bool {
    let thread = std::thread::current();
    let name = thread.name().expect("named unit test");
    if is_test_child(name) {
        // When: is_test_child matches this exact name, execute its body without recursively spawning.
        return false;
    }
    let result = run_test_child_with_config(name, config);
    assert!(!result.timed_out && result.status.success(), "{}", result.diagnostic());
    assert!(
        !result.capture_overflow,
        "complete output exceeded its capture limit: {}",
        result.diagnostic()
    );
    assert!(
        result.output.contains("1 passed; 0 failed"),
        "no child test ran: {}",
        result.diagnostic()
    );
    if forward_output {
        std::io::stdout().write_all(result.output.as_bytes()).expect("forward baseline output");
    }
    true
}

/// Direct stderr avoids libtest's capture so a stuck native phase survives the timeout.
pub(super) fn phase(pane: u64, phase: &str) {
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let thread = std::thread::current();
    std::io::stderr()
        .write_all(
            format!(
                "PTY_PHASE test={} pane={pane} phase={phase} time={}\n",
                thread.name().unwrap_or("unnamed"),
                at.as_millis()
            )
            .as_bytes(),
        )
        .unwrap();
}

/// Retain only live fixture PIDs, including PTY sessions outside the Unix test process group.
pub(super) fn record_process(pid: u32, started: bool) {
    let state = if started { "start" } else { "end" };
    std::io::stderr().write_all(format!("PTY_PROCESS {state}={pid}\n").as_bytes()).unwrap();
}

fn is_test_child(name: &str) -> bool {
    std::env::var(CHILD_MARKER).is_ok_and(|value| value == name)
}

struct TestResult {
    status: ExitStatus,
    timed_out: bool,
    elapsed: Duration,
    output: String,
    capture_overflow: bool,
    output_limit: usize,
}

impl TestResult {
    fn last_phase(&self) -> &str {
        self.output
            .lines()
            .rev()
            .find(|line| line.starts_with("PTY_PHASE "))
            .unwrap_or("phase=launch")
    }

    fn diagnostic(&self) -> String {
        format!(
            "timed_out={} exit={} elapsed={:?} capture_overflow={} output_limit={} last_phase={}\n{}",
            self.timed_out,
            self.status,
            self.elapsed,
            self.capture_overflow,
            self.output_limit,
            self.last_phase(),
            self.output
        )
    }
}

#[derive(Default)]
struct CapturedOutput {
    bytes: Vec<u8>,
    processes: std::collections::BTreeSet<u32>,
    overflow: bool,
}

/// Drain both capture policies after their cap, retaining a diagnostic tail or flagging incomplete baseline output.
fn capture_pipe(
    mut pipe: impl Read + Send + 'static,
    output: Arc<Mutex<CapturedOutput>>,
    config: IsolationConfig,
) -> std::sync::mpsc::Receiver<()> {
    let (finished, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        let mut pending = Vec::new();
        while let Ok(count) = pipe.read(&mut buffer) {
            if count == 0 {
                // When: count is zero, the pipe reached EOF and the reader can report completion.
                break;
            }
            let mut captured = output.lock().unwrap();
            if config.complete_output {
                let retained = count.min(config.output_limit.saturating_sub(captured.bytes.len()));
                captured.bytes.extend_from_slice(&buffer[..retained]);
                captured.overflow |= retained != count;
            } else {
                // When: complete_output is disabled, evict older bytes so ordinary callers retain only the bounded diagnostic tail.
                captured.bytes.extend_from_slice(&buffer[..count]);
                let excess = captured.bytes.len().saturating_sub(config.output_limit);
                captured.bytes.drain(..excess);
            }
            pending.extend_from_slice(&buffer[..count]);
            while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line);
                if let Some(pid) =
                    line.trim().strip_prefix("PTY_PROCESS start=").and_then(|pid| pid.parse().ok())
                {
                    captured.processes.insert(pid);
                }
                if let Some(pid) = line
                    .trim()
                    .strip_prefix("PTY_PROCESS end=")
                    .and_then(|pid| pid.parse::<u32>().ok())
                {
                    captured.processes.remove(&pid);
                }
            }
            if pending.len() > OUTPUT_LIMIT {
                pending.clear();
            }
        }
        let _ = finished.send(());
    });
    done
}

/// One past the highest descriptor number this process can hold on Linux: `fs.nr_open`, the
/// ceiling on `RLIMIT_NOFILE`.
#[cfg(target_os = "linux")]
fn descriptor_table_end() -> i32 {
    let nr_open = std::fs::read_to_string("/proc/sys/fs/nr_open").expect("read fs.nr_open");
    nr_open.trim().parse().expect("fs.nr_open is a descriptor count")
}

/// One past the highest descriptor number this process can hold on macOS, which allocates
/// descriptors below `kern.maxfilesperproc` whatever `RLIMIT_NOFILE` allows.
#[cfg(target_os = "macos")]
fn descriptor_table_end() -> i32 {
    let mut limit: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let status =
        // SAFETY: the name is NUL-terminated, and `limit` and `size` describe one writable `c_int`.
        unsafe {
            libc::sysctlbyname(
                c"kern.maxfilesperproc".as_ptr(),
                (&raw mut limit).cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
    assert_eq!(status, 0, "read kern.maxfilesperproc");
    limit
}

/// Mark every descriptor above stderr close-on-exec in the isolated child before it execs. macOS
/// has no `pipe2`, so std creates a spawn's pipe before marking it close-on-exec, and a child that
/// another test spawns in between inherits the pipe. An isolated child runs for seconds, so it
/// would hold that test's capture pipe open long after the test's own child exits. Linux marks
/// them all with one `close_range` call when the kernel has it; otherwise, and on macOS, the hook
/// visits every descriptor number below `descriptor_table_end`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn close_inherited_descriptors_on_exec(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    let table_end = descriptor_table_end();
    // SAFETY: the `pre_exec` hook runs between fork and exec and makes only the `close_range` system
    // call and `fcntl` calls, which are async-signal-safe; it allocates nothing and takes no lock.
    unsafe {
        command.pre_exec(move || {
            #[cfg(target_os = "linux")]
            {
                let marked =
                    libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, libc::CLOSE_RANGE_CLOEXEC);
                if marked == 0 {
                    // When: `marked` is 0, `close_range` flagged every descriptor from 3 up, so no loop runs.
                    return Ok(());
                }
            }
            for descriptor in 3..table_end {
                libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            Ok(())
        });
    }
}

fn run_test_child(name: &str, timeout: Duration) -> TestResult {
    run_test_child_with_config(name, IsolationConfig { timeout, ..IsolationConfig::ORDINARY })
}

fn run_test_child_with_config(name: &str, config: IsolationConfig) -> TestResult {
    assert!(
        !config.timeout.is_zero() && config.output_limit > 0,
        "isolated child needs finite nonzero budgets"
    );
    let started = Instant::now();
    let mut command = Command::new(std::env::current_exe().expect("unit-test executable"));
    command
        .args(["--exact", name, "--include-ignored", "--nocapture"])
        .env(CHILD_MARKER, name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        close_inherited_descriptors_on_exec(&mut command);
    }
    let mut child = command.spawn().expect("spawn isolated PTY test");
    let output = Arc::new(Mutex::new(CapturedOutput::default()));
    let out_done = capture_pipe(child.stdout.take().unwrap(), output.clone(), config);
    let err_done = capture_pipe(child.stderr.take().unwrap(), output.clone(), config);
    let deadline = started + config.timeout;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("observe isolated test") {
            // When: try_wait returns status, the completed test cannot need timeout termination.
            break (status, false);
        }
        if Instant::now() >= deadline {
            // When: deadline expires, terminate the isolated tree before waiting for any child-owned pipe.
            let processes = output.lock().unwrap().processes.clone();
            terminate_test_tree(&mut child, &processes);
            break (child.wait().expect("reap timed-out test"), true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let drained = out_done.recv_timeout(Duration::from_secs(2)).is_ok()
        && err_done.recv_timeout(Duration::from_secs(2)).is_ok();
    let captured = output.lock().unwrap();
    let result = TestResult {
        status,
        timed_out,
        elapsed: started.elapsed(),
        output: String::from_utf8_lossy(&captured.bytes).into_owned(),
        capture_overflow: captured.overflow,
        output_limit: config.output_limit,
    };
    assert!(drained, "child pipes did not close: {}", result.diagnostic());
    result
}

fn terminate_test_tree(
    child: &mut std::process::Child,
    _processes: &std::collections::BTreeSet<u32>,
) {
    #[cfg(windows)]
    {
        // Match the native-smoke runner's tree cleanup; taskkill has its own bounded lifetime.
        let mut kill = Command::new("taskkill.exe")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start test-tree termination");
        let deadline = Instant::now() + Duration::from_secs(10);
        while kill.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                // When: deadline expires for taskkill, reap that helper before the direct child-kill fallback.
                kill.kill().expect("stop expired taskkill");
                kill.wait().expect("reap expired taskkill");
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[cfg(unix)]
    {
        for pid in _processes {
            // SAFETY: these still-live fixture PIDs own separate PTY sessions; no ended PID is retained.
            unsafe {
                libc::kill(-(*pid as libc::pid_t), libc::SIGKILL);
            }
        }
        // Let the live test parent reap terminated PTY children before terminating its own process group.
        std::thread::sleep(Duration::from_millis(100));
        // SAFETY: the child owns its new process group; negative PID addresses only that isolated group.
        unsafe {
            libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

#[cfg(test)]
#[path = "pty_test_support_tests.rs"]
mod pty_test_support_tests;
