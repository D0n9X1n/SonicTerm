//! The Windows half of the F-ConPTY fixture: one test binary plays three roles, chosen by
//! `SONICTERM_CONPTY_FIXTURE_ROLE`.
//!
//! The outer test, with no role, copies its own executable into `packaged\` (with the pinned
//! runtime beside it) and `system\` (without one) and runs each copy as a host inside a job
//! object. A host spawns the same executable as the child through a real pseudoconsole, scans its
//! output for the case sentinels, answers or injects input through its [`Engine`], and reports
//! the runtime check and one row per section as JSON lines on its own stdout, never through the
//! PTY. The child prints each case between sentinels and reports, as hex, the input it received.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

use crossbeam_channel::RecvTimeoutError;
use windows::{
    core::{HSTRING, PCWSTR},
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Console::{
                GetConsoleMode, GetStdHandle, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
                CONSOLE_MODE, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
                ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
                ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
                JobObjectExtendedLimitInformation, QueryInformationJobObject,
                SetInformationJobObject, TerminateJobObject,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            LibraryLoader::GetModuleHandleW,
        },
    },
};

use super::{
    classify, classify_repaint, find, hex, json_field, row_line, Case, Evidence, Expected,
    InputPhase, Outcome, ScanEvent, Scanner, LINE_PREFIX, PTY_COLS as COLS, PTY_ROWS as ROWS,
};
use crate::pty::{CommandSpawn, PtyHandle, ShellSpawnOpts};

/// Selects the role; absent means the outer test.
const ROLE_ENV: &str = "SONICTERM_CONPTY_FIXTURE_ROLE";
/// A directory already staged by `setup-windows-conpty.py --destination`; absent stages one.
const RUNTIME_DIR_ENV: &str = "SONICTERM_CONPTY_FIXTURE_RUNTIME_DIR";
/// Makes a missing packaged runtime fail startup instead of falling back (`conpty_runtime`).
const REQUIRE_ENV: &str = "SONICTERM_REQUIRE_CONPTY";
const CP_UTF8: u32 = 65_001;
const TOTAL_BUDGET: Duration = Duration::from_secs(150);
const HOST_BUDGET: Duration = Duration::from_secs(60);
/// The host's own loop ends first, so its rows and end line reach the outer test in time.
const HOST_LOOP_BUDGET: Duration = Duration::from_secs(50);
/// Silence allowed between two sentinels of one case before its open sections read inconclusive.
const CASE_BUDGET: Duration = Duration::from_secs(5);
/// One case's whole run, from its first section opening, however many sentinels it sends: its
/// output, its input phase and a resize wait together stay under it.
const WHOLE_CASE_BUDGET: Duration = Duration::from_secs(15);
/// Chunks the child's stdin reader may queue ahead of the case reading them; each is at most 4 KiB.
const INPUT_QUEUE_CHUNKS: usize = 16;
const REAP_BUDGET: Duration = Duration::from_secs(5);
const READ_DEADLINE: Duration = Duration::from_secs(2);
const READ_QUIET: Duration = Duration::from_millis(300);
const MAX_READ_BYTES: usize = 64 * 1024;
const RESIZE_DELAY: Duration = Duration::from_millis(700);
const REPAINT_WAIT: Duration = Duration::from_millis(1500);

/// The host's side of the terminal: what it answers while scanning and what a case's reply is.
pub trait Engine {
    /// Bytes to write into the PTY input in response to child output `bytes`.
    fn output(&mut self, bytes: &[u8]) -> Vec<u8>;
    /// The reply this engine sends for `case`'s output, which the child reports before any
    /// injected bytes.
    fn reference_reply(&mut self, case: &Case) -> Vec<u8>;
}

/// A minimal terminal for transport rows: it answers the console's cursor-position and device
/// attribute queries and nothing else, so every other input byte is one the case injected.
#[derive(Default)]
pub struct TransportEngine {
    carry: Vec<u8>,
}

impl Engine for TransportEngine {
    fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.carry.extend_from_slice(bytes);
        let mut reply = Vec::new();
        if find(&self.carry, b"\x1b[6n").is_some() {
            // The console asked for the cursor position: answer the home cell.
            reply.extend_from_slice(b"\x1b[1;1R");
        }
        if find(&self.carry, b"\x1b[c").is_some() || find(&self.carry, b"\x1b[0c").is_some() {
            // The console asked for primary device attributes: answer a VT220.
            reply.extend_from_slice(b"\x1b[?62;22c");
        }
        let keep = self.carry.len().min(3);
        self.carry.drain(..self.carry.len() - keep);
        reply
    }

    fn reference_reply(&mut self, _case: &Case) -> Vec<u8> {
        Vec::new()
    }
}

/// Run the fixture role this process was started in. `test_name` is the libtest name of the
/// calling `#[ignore]` test; `manifest_dir` locates `scripts/` when no runtime is staged.
pub fn run<E: Engine>(
    test_name: &str,
    manifest_dir: &Path,
    cases: &[Case],
    engine: impl FnOnce() -> E,
) {
    match std::env::var(ROLE_ENV).as_deref() {
        Ok("child") => run_child(cases),
        Ok(role @ ("packaged" | "system")) => run_host(role, test_name, cases, engine()),
        _ => run_outer(test_name, manifest_dir, cases),
    }
}

/// libtest arguments that run exactly the fixture test, even though it is `#[ignore]`.
fn test_args(test_name: &str) -> [String; 5] {
    [test_name, "--exact", "--ignored", "--nocapture", "--test-threads=1"].map(String::from)
}

fn run_host<E: Engine>(role: &str, test_name: &str, cases: &[Case], mut engine: E) {
    let mut go_line = String::new();
    // The outer test writes `go` only after this process is in its job object, so nothing this
    // host spawns can escape the job. Anything but an exact `go` line (EOF, a read error, other
    // bytes) means the parent never confirmed the job, so the host spawns nothing.
    let read = std::io::stdin().read_line(&mut go_line);
    assert!(
        read.is_ok() && go_line == "go\n",
        "the parent did not confirm this host's job object; no PTY is spawned"
    );
    let selection = Arc::new(Mutex::new(None));
    // A fresh host process has no subscriber yet; failure would only lose the selection record,
    // which then fails the runtime check below instead of passing silently.
    let _ = tracing::subscriber::set_global_default(KindCapture(Arc::clone(&selection)));
    let executable = std::env::current_exe().expect("host executable");
    let spawn = CommandSpawn {
        program: executable.clone().into_os_string(),
        args: test_args(test_name).into_iter().map(std::ffi::OsString::from).collect(),
        cwd: executable.parent().expect("host directory").to_path_buf(),
    };
    let opts = ShellSpawnOpts {
        extra_env: vec![(ROLE_ENV.to_owned(), "child".to_owned())],
        ..ShellSpawnOpts::default()
    };
    let spawned = PtyHandle::spawn_command(&spawn, COLS, ROWS, &opts);
    let module_loaded =
        // SAFETY: the name is a live NUL-terminated wide string; this only queries a loaded module.
        unsafe { GetModuleHandleW(&HSTRING::from("conpty.dll")) }.is_ok();
    let selected = selection.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    let expected_selection = if role == "packaged" { "packaged" } else { "development-discovery" };
    let runtime_ok = spawned.is_ok()
        && module_loaded == (role == "packaged")
        && selected.as_deref() == Some(expected_selection);
    println!(
        "{LINE_PREFIX}{{\"kind\":\"runtime\",\"role\":\"{role}\",\"ok\":{runtime_ok},\"spawned\":{},\"conpty_module_loaded\":{module_loaded},\"selection\":\"{}\"}}",
        spawned.is_ok(),
        selected.unwrap_or_default(),
    );
    let closed = match spawned {
        Ok(pty) => drive(&pty, cases, &mut engine),
        Err(error) => {
            // The launch failed, so every row is inconclusive; the error names no payload.
            println!(
                "{LINE_PREFIX}{{\"kind\":\"launch-error\",\"error\":{:?}}}",
                error.to_string()
            );
            HashMap::new()
        }
    };
    emit_rows(cases, &closed, runtime_ok, &mut engine);
    println!("{LINE_PREFIX}{{\"kind\":\"end\",\"role\":\"{role}\"}}");
}

/// Scan the child's output until `DONE`, the host budget, or a case budget with no sentinel;
/// return each closed section's evidence. Dropping `pty` afterwards kills and reaps the child.
fn drive<E: Engine>(pty: &PtyHandle, cases: &[Case], engine: &mut E) -> HashMap<String, Evidence> {
    let host_deadline = Instant::now() + HOST_LOOP_BUDGET;
    let mut case_deadline = Instant::now() + CASE_BUDGET;
    // The case whose sections are open and when it must finish, whatever sentinels it sends.
    let mut whole_case: Option<(String, Instant)> = None;
    let mut scanner = Scanner::default();
    let mut closed = HashMap::new();
    let mut resize_at: Option<(Instant, u16)> = None;
    loop {
        let now = Instant::now();
        let whole_deadline = whole_case.as_ref().map_or(host_deadline, |(_, due)| *due);
        if now >= host_deadline.min(case_deadline).min(whole_deadline) {
            // When: `now` passed the host, silence or whole-case deadline, open sections read
            // inconclusive.
            return closed;
        }
        if let Some((_, rows)) = resize_at.filter(|(due, _)| now >= *due) {
            resize_at = None;
            if (pty.resize)(COLS, rows).is_err() {
                // The native resize failed; the repaint row records whatever came.
                println!("{LINE_PREFIX}{{\"kind\":\"resize-error\",\"rows\":{rows}}}");
            }
        }
        let chunk = match pty.out_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => chunk,
            Err(RecvTimeoutError::Timeout) => {
                // When: the read hit its Timeout, recheck the budgets and any due resize.
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // When: the output channel is Disconnected, the child is gone; report what closed.
                return closed;
            }
        };
        let reply = engine.output(&chunk);
        if !reply.is_empty() {
            // When: the engine produced a `reply`; a refused one shows as a mismatch in the report.
            let _ = pty.send_input_nonblocking(reply);
        }
        for event in scanner.feed(&chunk) {
            case_deadline = Instant::now() + CASE_BUDGET;
            match event {
                ScanEvent::Opened(name) => {
                    // When: a section `Opened`, an `/in` section may need injected bytes or a resize.
                    let case_name = name.rsplit_once('/').map_or(name.as_str(), |(case, _)| case);
                    if whole_case.as_ref().is_none_or(|(open, _)| open != case_name) {
                        // A section of another case opened, so that case's whole clock starts now.
                        whole_case =
                            Some((case_name.to_owned(), Instant::now() + WHOLE_CASE_BUDGET));
                    }
                    let phase = name
                        .strip_suffix("/in")
                        .and_then(|case_name| cases.iter().find(|case| case.name == case_name))
                        .and_then(|case| case.input.as_ref());
                    match phase {
                        Some(InputPhase::Echo(bytes)) if !bytes.is_empty() => {
                            // When: an `Echo` phase has bytes, inject them; a refusal shows in the row.
                            let _ = pty.send_input_nonblocking(bytes.clone());
                        }
                        Some(InputPhase::Resize { rows, .. }) => {
                            resize_at = Some((Instant::now() + RESIZE_DELAY, *rows));
                        }
                        _ => {
                            // When: any other `phase` (none, or an empty echo) needs no host action.
                        }
                    }
                }
                ScanEvent::Closed(name, evidence) => {
                    closed.insert(name, evidence);
                }
                ScanEvent::Abandoned(_) => {
                    // When: a section was `Abandoned`, its row stays missing and reads inconclusive.
                }
                ScanEvent::Done => {
                    // When: the child printed `Done`, every section it will print has arrived.
                    return closed;
                }
            }
        }
    }
}

/// Print one row per section. A failed runtime check makes every row inconclusive; a failed
/// positive control (the first case) makes every other row inconclusive.
fn emit_rows<E: Engine>(
    cases: &[Case],
    closed: &HashMap<String, Evidence>,
    runtime_ok: bool,
    engine: &mut E,
) {
    let mut rows: Vec<(&str, &str, bool, Outcome, Option<&Evidence>)> = Vec::new();
    for case in cases {
        let output = closed.get(&format!("{}/out", case.name));
        let outcome = classify(output, &Expected::of(&case.output));
        rows.push((case.name, "out", case.output_required, outcome, output));
        let Some(phase) = &case.input else {
            // When: the case has no `input` phase, its output row is its only row.
            continue;
        };
        let input = closed.get(&format!("{}/in", case.name));
        let outcome = match phase {
            InputPhase::Echo(injected) => {
                let mut received = engine.reference_reply(case);
                received.extend_from_slice(injected);
                classify(input, &Expected::of(hex(&received).as_bytes()))
            }
            InputPhase::Resize { .. } => classify_repaint(input),
        };
        rows.push((case.name, "in", case.input_required, outcome, input));
    }
    let control = cases.first().map(|case| case.name);
    let control_ok =
        rows.iter().filter(|row| Some(row.0) == control).all(|row| row.3 == Outcome::Exact);
    for (name, phase, required, outcome, evidence) in rows {
        let trusted = runtime_ok && (control_ok || Some(name) == control);
        let outcome = if trusted { outcome } else { Outcome::Inconclusive };
        println!("{}", row_line(name, phase, required, outcome, evidence));
    }
}

// Lock order: stdin (reader thread only) then stdout (this thread only); no thread holds both.
fn run_child(cases: &[Case]) {
    configure_console();
    // Bounded: a burst the current case does not read waits in the console's own buffer, not in
    // an unbounded queue; the reader blocks until the case drains it.
    let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(INPUT_QUEUE_CHUNKS);
    // The reader runs for the child's life: console input arrives whenever the host writes it,
    // and the process exit ends the thread with its blocking read.
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buffer = [0u8; 4096];
        while let Ok(count @ 1..) = stdin.read(&mut buffer) {
            if sender.send(buffer[..count].to_vec()).is_err() {
                // When: `sender` fails, the main thread is gone and nobody reads input.
                return;
            }
        }
    });
    let mut stdout = std::io::stdout().lock();
    for case in cases {
        // Input left from an earlier case must not be counted as this case's reply.
        while receiver.try_recv().is_ok() {}
        let mut frame = case.preamble.clone();
        frame.extend_from_slice(format!("@@BEGIN {}/out@@", case.name).as_bytes());
        frame.extend_from_slice(&case.output);
        frame.extend_from_slice(format!("@@END {}/out@@\r\n", case.name).as_bytes());
        if case.input.is_some() {
            frame.extend_from_slice(format!("@@BEGIN {}/in@@", case.name).as_bytes());
        }
        write_flushed(&mut stdout, &frame);
        match &case.input {
            Some(InputPhase::Echo(_)) => {
                let received = collect_input(&receiver);
                write_flushed(
                    &mut stdout,
                    format!("{}@@END {}/in@@\r\n", hex(&received), case.name).as_bytes(),
                );
            }
            Some(InputPhase::Resize { total_rows, .. }) => {
                let rows: String =
                    (1..=*total_rows).map(|row| format!("ROW-{row:02}\r\n")).collect();
                write_flushed(&mut stdout, rows.as_bytes());
                std::thread::sleep(REPAINT_WAIT);
                write_flushed(&mut stdout, format!("@@END {}/in@@\r\n", case.name).as_bytes());
            }
            None => {
                // When: the case has no `input` phase, its output section is all it prints.
            }
        }
        let mut trailer = case.trailer.clone();
        trailer.extend_from_slice(b"\r\n");
        write_flushed(&mut stdout, &trailer);
    }
    write_flushed(&mut stdout, b"@@DONE@@\r\n");
}

/// Write and flush one frame; a console write error leaves the host to read the section as lost.
fn write_flushed(stdout: &mut impl Write, bytes: &[u8]) {
    let _ = stdout.write_all(bytes);
    let _ = stdout.flush();
}

/// Bytes received within [`READ_DEADLINE`], ending [`READ_QUIET`] after the last one, at most
/// [`MAX_READ_BYTES`].
fn collect_input(receiver: &mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    let deadline = Instant::now() + READ_DEADLINE;
    let mut received = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let wait = if received.is_empty() {
            left
        } else {
            // When: some input was `received`, the reply ends after READ_QUIET of silence.
            left.min(READ_QUIET)
        };
        let Ok(bytes) = receiver.recv_timeout(wait) else {
            // When: the wait expired (or the reader ended), the reply is complete.
            break;
        };
        received.extend_from_slice(&bytes);
        if received.len() >= MAX_READ_BYTES {
            // When: `received` reached MAX_READ_BYTES, stop; the row then shows the cut.
            received.truncate(MAX_READ_BYTES);
            break;
        }
    }
    received
}

/// UTF-8 code pages, raw VT input (no line editing, echo or Ctrl-C processing) and VT output.
fn configure_console() {
    let _ =
        // SAFETY: a plain console call with a constant code page; a failure leaves the default.
        unsafe { SetConsoleCP(CP_UTF8) };
    let _ =
        // SAFETY: a plain console call with a constant code page; a failure leaves the default.
        unsafe { SetConsoleOutputCP(CP_UTF8) };
    let raw_input = |mode: CONSOLE_MODE| {
        (mode | ENABLE_VIRTUAL_TERMINAL_INPUT)
            & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT)
    };
    set_mode(STD_INPUT_HANDLE, raw_input);
    set_mode(STD_OUTPUT_HANDLE, |mode| {
        mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING
    });
}

/// Apply `change` to the console mode of standard handle `which`; a non-console handle is left.
fn set_mode(
    which: windows::Win32::System::Console::STD_HANDLE,
    change: impl Fn(CONSOLE_MODE) -> CONSOLE_MODE,
) {
    let handle =
        // SAFETY: GetStdHandle only reads this process's standard handle table.
        unsafe { GetStdHandle(which) };
    let Ok(handle) = handle else {
        // When: no standard `handle` exists, there is no console mode to change.
        return;
    };
    let mut mode = CONSOLE_MODE(0);
    let queried =
        // SAFETY: `mode` is a live out-parameter for this call only; `handle` is the process's own.
        unsafe { GetConsoleMode(handle, &mut mode) };
    if queried.is_err() {
        // When: the mode cannot be `queried`, the handle is not a console.
        return;
    }
    let _ =
        // SAFETY: `handle` is the process's own console handle; the mode is a plain flag set.
        unsafe { SetConsoleMode(handle, change(mode)) };
}

fn run_outer(test_name: &str, manifest_dir: &Path, cases: &[Case]) {
    let executable = std::env::current_exe().expect("test executable");
    let file_name = executable.file_name().expect("test executable name").to_owned();
    // Beside the executable rather than in TEMP, whose short-name spelling would differ from the
    // loader's canonical path for the copied runtime.
    let base = executable
        .parent()
        .expect("test executable directory")
        .join(format!("conpty-fixture-{}", std::process::id()));
    // The total budget covers staging the runtime too, so a slow copy cannot add to it unseen.
    let started = Instant::now();
    // A leftover directory from an earlier process with the same id is replaced.
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("create fixture directory");
    let runtime = std::env::var_os(RUNTIME_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| stage_runtime(manifest_dir, &base.join("staged")));
    let mut problems = Vec::new();
    for role in ["packaged", "system"] {
        let role_dir = base.join(role);
        std::fs::create_dir_all(&role_dir).expect("create role directory");
        let host = role_dir.join(&file_name);
        std::fs::copy(&executable, &host).expect("copy host executable");
        if role == "packaged" {
            let target = role_dir.join("conpty");
            std::fs::create_dir_all(&target).expect("create runtime directory");
            for member in ["conpty.dll", "OpenConsole.exe"] {
                std::fs::copy(runtime.join("conpty").join(member), target.join(member))
                    .expect("copy staged runtime member");
            }
        }
        let budget = TOTAL_BUDGET.saturating_sub(started.elapsed()).min(HOST_BUDGET);
        let report = run_one_host(&host, role, test_name, budget);
        check_report(role, cases, &report, &mut problems);
    }
    // Copies an unreaped host still holds stay behind; that host is already a reported problem.
    let _ = std::fs::remove_dir_all(&base);
    assert!(problems.is_empty(), "F-ConPTY fixture failed:\n{}", problems.join("\n"));
}

/// Stage the pinned runtime with the reviewed setup script into `destination`.
fn stage_runtime(manifest_dir: &Path, destination: &Path) -> PathBuf {
    std::fs::create_dir_all(destination).expect("create staging directory");
    let script = manifest_dir.join("../../scripts/setup-windows-conpty.py");
    let status = Command::new("python")
        .arg(&script)
        .arg("--destination")
        .arg(destination)
        .status()
        .expect("run python scripts/setup-windows-conpty.py");
    assert!(
        status.success(),
        "staging the ConPTY runtime failed; stage it with `python scripts/setup-windows-conpty.py --destination <dir>` and set {RUNTIME_DIR_ENV}=<dir>"
    );
    destination.to_path_buf()
}

/// One host's control lines and teardown result.
struct HostReport {
    lines: Vec<String>,
    finished: bool,
    reaped: bool,
}

fn run_one_host(host: &Path, role: &str, test_name: &str, budget: Duration) -> HostReport {
    let mut command = Command::new(host);
    command
        .args(test_args(test_name))
        .env(ROLE_ENV, role)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if role == "packaged" {
        command.env(REQUIRE_ENV, "1");
    } else {
        // When: the `role` is the system host, REQUIRE_ENV is removed so development discovery runs.
        command.env_remove(REQUIRE_ENV);
    }
    let job = Job::new().expect("create job object");
    let mut child = command.spawn().expect("spawn fixture host");
    if let Err(error) = job.assign(&child) {
        // The host is outside the job: kill and reap it before the failure, so it never reads
        // `go` from a closed pipe and spawns its PTY.
        let killed = child.kill().is_ok();
        let reaped = wait_exit(&mut child, REAP_BUDGET);
        panic!("assign fixture host to its job: {error} (killed: {killed}, reaped: {reaped})");
    }
    if let Some(mut stdin) = child.stdin.take() {
        // When: the host's `stdin` pipe exists, send `go`; a failed write shows as a budget expiry.
        let _ = stdin.write_all(b"go\n");
    }
    let stdout = child.stdout.take().expect("host stdout");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                // When: `sender` fails, the outer test stopped listening for this output.
                return;
            }
        }
    });
    let deadline = Instant::now() + budget;
    let mut lines = Vec::new();
    let mut finished = false;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(line) = receiver.recv_timeout(left) else {
            // When: `recv_timeout` expired or the reader ended, no further `line` arrives in budget.
            break;
        };
        let Some(at) = line.find(LINE_PREFIX) else {
            // When: the `line` has no LINE_PREFIX, it is libtest's own output, not a control line.
            continue;
        };
        let control = line[at..].to_owned();
        finished = json_field(&control, "kind").as_deref() == Some("end");
        lines.push(control);
        if finished {
            // When: the host `finished` with its end line, every row it reports has arrived.
            break;
        }
    }
    let exited = wait_exit(&mut child, REAP_BUDGET);
    if !exited {
        // The host outlived its budget: end its whole tree; it is already a failure.
        job.terminate();
    }
    let reaped = exited && job.wait_empty(REAP_BUDGET);
    if !exited {
        wait_exit(&mut child, REAP_BUDGET);
    }
    HostReport { lines, finished, reaped }
}

/// Poll `child` for exit within `budget`.
fn wait_exit(child: &mut std::process::Child, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            // When: the `child` matches an exit status, the host has exited and is reaped.
            return true;
        }
        if Instant::now() >= deadline {
            // When: the `deadline` passed with the host alive, report it as not exited.
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Record every way `report` breaks the hard assertions: an unfinished or unreaped host, a failed
/// runtime check, a positive control that is not `exact`, a required packaged row that is not
/// `exact`, or a missing row.
fn check_report(role: &str, cases: &[Case], report: &HostReport, problems: &mut Vec<String>) {
    for line in &report.lines {
        println!("{role}: {line}");
    }
    if !report.finished {
        problems.push(format!("{role}: the host did not finish within its budget"));
    }
    if !report.reaped {
        problems.push(format!("{role}: a process in the host's job outlived teardown"));
    }
    let runtime_ok = report.lines.iter().any(|line| {
        json_field(line, "kind").as_deref() == Some("runtime")
            && json_field(line, "ok").as_deref() == Some("true")
    });
    if !runtime_ok {
        problems.push(format!("{role}: the runtime check failed, so every row is inconclusive"));
    }
    let control = cases.first().map(|case| case.name);
    let mut row_count = 0;
    for line in
        report.lines.iter().filter(|line| json_field(line, "kind").as_deref() == Some("row"))
    {
        row_count += 1;
        let case = json_field(line, "case").unwrap_or_default();
        let exact = json_field(line, "outcome").as_deref() == Some("exact");
        let required = json_field(line, "required").as_deref() == Some("true");
        if !exact && Some(case.as_str()) == control {
            problems.push(format!("{role}: the positive control is not exact: {line}"));
        } else if !exact && required && role == "packaged" {
            // When: a `required` row is not `exact` on the packaged runtime, the hard assertion fails.
            problems.push(format!("{role}: a required row is not exact: {line}"));
        }
    }
    let wanted = cases.len() + cases.iter().filter(|case| case.input.is_some()).count();
    if row_count != wanted {
        problems.push(format!("{role}: {row_count} rows reported, {wanted} expected"));
    }
}

/// A job object that kills every process still in it when closed.
struct Job(HANDLE);

impl Job {
    fn new() -> windows::core::Result<Self> {
        let handle =
            // SAFETY: no security attributes and no name; `Job` owns the returned handle.
            unsafe { CreateJobObjectW(None, PCWSTR::null()) }?;
        let job = Self(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is a live extended-limit record of the size passed, read during the call.
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }?;
        Ok(job)
    }

    fn assign(&self, child: &std::process::Child) -> windows::core::Result<()> {
        let process = HANDLE(child.as_raw_handle());
        // SAFETY: both handles are live for the call; `child` keeps ownership of its handle.
        unsafe { AssignProcessToJobObject(self.0, process) }
    }

    fn terminate(&self) {
        let _ =
            // SAFETY: `self.0` is this job's live handle. A failure leaves the close-time kill.
            unsafe { TerminateJobObject(self.0, 1) };
    }

    /// Wait within `budget` for the job to hold no live process.
    fn wait_empty(&self, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            let queried =
                // SAFETY: `info` is a live accounting record of the size passed, written by this call.
                unsafe {
                QueryInformationJobObject(
                    Some(self.0),
                    JobObjectBasicAccountingInformation,
                    std::ptr::from_mut(&mut info).cast(),
                    std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    None,
                )
            };
            if queried.is_ok() && info.ActiveProcesses == 0 {
                // When: the job reports zero ActiveProcesses, every process in it was reaped.
                return true;
            }
            if Instant::now() >= deadline {
                // When: the `deadline` passed with a process left, the job was not emptied.
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

// Lifecycle: Job drop calls CloseHandle once; KILL_ON_JOB_CLOSE then ends any process left in it.
impl Drop for Job {
    fn drop(&mut self) {
        let _ =
            // SAFETY: `self.0` is the handle CreateJobObjectW returned, closed exactly once here.
            unsafe { CloseHandle(self.0) };
    }
}

/// A subscriber that keeps only the console runtime's selection record (`kind`).
struct KindCapture(Arc<Mutex<Option<String>>>);

impl tracing::Subscriber for KindCapture {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "sonicterm_io::conpty_runtime"
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        event.record(&mut KindVisitor(&self.0));
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

struct KindVisitor<'a>(&'a Mutex<Option<String>>);

impl tracing::field::Visit for KindVisitor<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "kind" {
            *self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}
