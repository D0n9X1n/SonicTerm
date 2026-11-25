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

use super::clock::{
    abandon, collect_input, confirmed, reap, settle_input, wait_until, Arrival, CaseClock, Input,
    Launch, RunClock, Teardown, Unjobbed, HOST_BUDGET, REPORT_MARGIN,
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
/// The host's scanning-loop length in milliseconds, set by the outer test from its admitted run.
const LOOP_MS_ENV: &str = "SONICTERM_CONPTY_FIXTURE_LOOP_MS";
const CP_UTF8: u32 = 65_001;
/// Chunks the child's stdin reader may queue ahead of the case reading them; each is at most 4 KiB.
const INPUT_QUEUE_CHUNKS: usize = 16;
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
        confirmed(&read, &go_line),
        "the parent did not confirm this host's job object; no PTY is spawned"
    );
    // The outer test admits the loop's length from its total deadline; a missing or invalid value
    // falls back to the most a host may use.
    let loop_budget = std::env::var(LOOP_MS_ENV)
        .ok()
        .and_then(|millis| millis.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(HOST_BUDGET - REPORT_MARGIN)
        .min(HOST_BUDGET - REPORT_MARGIN);
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
    let (closed, scanned) = match spawned {
        Ok(pty) => drive(&pty, cases, &mut engine, loop_budget),
        Err(error) => {
            // The launch failed, so every row is inconclusive; the error names no payload.
            println!(
                "{LINE_PREFIX}{{\"kind\":\"launch-error\",\"error\":{:?}}}",
                error.to_string()
            );
            (HashMap::new(), false)
        }
    };
    emit_rows(cases, &closed, runtime_ok, &mut engine);
    // `scan` is `done` when the child's DONE arrived in time, `required` when the scan expired only
    // inside a scan-optional case after every other case closed, else `expired`.
    let scan = match (scanned, required_closed(cases, &closed)) {
        (true, _) => "done",
        (false, true) => "required",
        (false, false) => "expired",
    };
    println!("{LINE_PREFIX}{{\"kind\":\"end\",\"role\":\"{role}\",\"scan\":\"{scan}\"}}");
}

/// Scan the child's output until `DONE` or the deadline [`CaseClock`] puts in force; return each
/// section's evidence that closed in time and whether `DONE` arrived. A chunk's time is when the
/// host received it from the PTY reader. Dropping `pty` afterwards kills and reaps the child.
fn drive<E: Engine>(
    pty: &PtyHandle,
    cases: &[Case],
    engine: &mut E,
    loop_budget: Duration,
) -> (HashMap<String, Evidence>, bool) {
    let started = Instant::now();
    let mut clock = CaseClock::new(started, started + loop_budget);
    let mut scanner = Scanner::default();
    let mut closed = HashMap::new();
    let mut resize_at: Option<(Instant, u16)> = None;
    loop {
        if let Some((_, rows)) = resize_at.filter(|(due, _)| Instant::now() >= *due) {
            resize_at = None;
            if (pty.resize)(COLS, rows).is_err() {
                // The native resize failed; the repaint row records whatever came.
                println!("{LINE_PREFIX}{{\"kind\":\"resize-error\",\"rows\":{rows}}}");
            }
        }
        // Measured after the resize call, so a slow native resize cannot stretch the wait.
        let Some(left) =
            clock.deadline().checked_duration_since(Instant::now()).filter(|left| !left.is_zero())
        else {
            // When: checked_duration_since finds the deadline in force passed, open sections read
            // inconclusive.
            return (closed, false);
        };
        // The wait never runs past the deadline in force, so a chunk is stamped when it arrived.
        let chunk = match pty.out_rx.recv_timeout(left.min(Duration::from_millis(50))) {
            Ok(chunk) => chunk,
            Err(RecvTimeoutError::Timeout) => {
                // When: the read hit its Timeout, recheck the budgets and any due resize.
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // When: the output channel is Disconnected, the child is gone; report what closed.
                return (closed, false);
            }
        };
        let received_at = Instant::now();
        if clock.admits(received_at) == Arrival::Late {
            // When: admits says Arrival::Late, the chunk is refused whole: none of its sentinels
            // may end the expired section, open another or finish the run.
            return (closed, false);
        }
        let reply = engine.output(&chunk);
        if !reply.is_empty() {
            // When: the engine produced a `reply`; a refused one shows as a mismatch in the report.
            let _ = pty.send_input_nonblocking(reply);
        }
        for event in scanner.feed(&chunk) {
            match event {
                ScanEvent::Opened(name) => {
                    // When: a section `Opened`, an `/in` section may need injected bytes or a resize.
                    if clock.opened(&name, received_at) == Arrival::Late {
                        // When: clock.opened says Arrival::Late, nothing after it is evidence.
                        return (closed, false);
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
                    // When: a section `Closed`, its evidence counts only inside its case's deadline.
                    if clock.closed(received_at) == Arrival::Late {
                        // When: clock.closed says Arrival::Late, the late row is refused.
                        return (closed, false);
                    }
                    closed.insert(name, evidence);
                }
                ScanEvent::Abandoned(_) => {
                    // When: a section was `Abandoned`, its row stays missing and reads
                    // inconclusive; silence is measured from it.
                    if clock.abandoned(received_at) == Arrival::Late {
                        // When: clock.abandoned says Arrival::Late, nothing after it is evidence.
                        return (closed, false);
                    }
                }
                ScanEvent::Done => {
                    // When: the child printed `Done`, every section it will print has arrived.
                    return (closed, true);
                }
            }
        }
    }
}

/// Whether every section of every case that is not scan-optional closed in time.
fn required_closed(cases: &[Case], closed: &HashMap<String, Evidence>) -> bool {
    cases.iter().filter(|case| !case.scan_optional).all(|case| {
        closed.contains_key(&format!("{}/out", case.name))
            && (case.input.is_none() || closed.contains_key(&format!("{}/in", case.name)))
    })
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
        // Input left from an earlier case must not be counted as this case's reply; input that
        // never settles within its bounds makes the reply untrustworthy rather than wrong.
        let settled = !matches!(case.input, Some(InputPhase::Echo(_))) || settle_input(&receiver);
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
                let input = if settled {
                    collect_input(&receiver)
                } else {
                    // When: settled is false, earlier input never stopped, so no reply is trusted.
                    Input::Overflow
                };
                // An overflow is reported as such, never as hex, so it cannot read as a reply.
                let report = match input {
                    Input::Bytes(received) => hex(&received),
                    Input::Overflow => "OVERFLOW".to_owned(),
                    Input::Unsettled => "UNSETTLED".to_owned(),
                };
                write_flushed(
                    &mut stdout,
                    format!("{report}@@END {}/in@@\r\n", case.name).as_bytes(),
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
    // One total deadline covers staging, both hosts and their teardowns; nothing restarts it.
    let run = RunClock::new(Instant::now());
    // A leftover directory from an earlier process with the same id is replaced; the filesystem
    // work runs under the work deadline like every copy.
    let fresh = base.clone();
    let prepared = within(run.work_deadline(), move || {
        let _ = std::fs::remove_dir_all(&fresh);
        std::fs::create_dir_all(&fresh)
    });
    if let Err(problem) = prepared {
        // The fixture directory could not be made before the deadline.
        panic!("F-ConPTY fixture failed: preparing the fixture directory failed: {problem}");
    }
    let runtime = match std::env::var_os(RUNTIME_DIR_ENV) {
        Some(staged) => PathBuf::from(staged),
        None => stage_runtime(manifest_dir, &base.join("staged"), &run),
    };
    let mut problems = Vec::new();
    // A copy abandoned at the deadline may still be writing, so its directory is left alone.
    let mut abandoned_copy = false;
    for role in ["packaged", "system"] {
        let role_dir = base.join(role);
        let host = role_dir.join(&file_name);
        let (source, runtime) = (executable.clone(), runtime.clone());
        let copy_host = host.clone();
        let copied = within(run.work_deadline(), move || {
            std::fs::create_dir_all(&role_dir)?;
            std::fs::copy(&source, &copy_host)?;
            if role == "packaged" {
                // The packaged host gets the pinned runtime beside it.
                let target = role_dir.join("conpty");
                std::fs::create_dir_all(&target)?;
                for member in ["conpty.dll", "OpenConsole.exe"] {
                    std::fs::copy(runtime.join("conpty").join(member), target.join(member))?;
                }
            }
            Ok(())
        });
        if let Err(problem) = copied {
            // When: copied is an Err (a failed copy or a passed deadline), the host never launches.
            abandoned_copy |= problem == LATE;
            problems.push(format!("{role}: preparing the host failed: {problem}"));
            continue;
        }
        let Some(launch) = run.launch(Instant::now()) else {
            // When: launch finds too little time left for the host's rows, it never starts.
            problems
                .push(format!("{role}: the total deadline left too little time to run this host"));
            continue;
        };
        let report = run_one_host(&host, role, test_name, launch, &run);
        check_report(role, cases, &report, &mut problems);
    }
    problems.extend(remove_fixture_directory(&base, abandoned_copy, run.total_deadline()));
    assert!(problems.is_empty(), "F-ConPTY fixture failed:\n{}", problems.join("\n"));
}

/// Remove the fixture directory before `deadline`, or say why it remains: a copy abandoned at its
/// deadline may still write, so its directory is reported and left. An unreaped host's files also
/// stay; that host is already a reported problem.
fn remove_fixture_directory(
    base: &Path,
    abandoned_copy: bool,
    deadline: Instant,
) -> Option<String> {
    if abandoned_copy {
        // When: abandoned_copy is true, the directory is left for that copy and reported.
        return Some(format!(
            "the fixture directory {} was left for an abandoned copy",
            base.display()
        ));
    }
    let doomed = base.to_path_buf();
    let removed = within(deadline, move || std::fs::remove_dir_all(&doomed));
    removed.err().map(|problem| format!("removing the fixture directory failed: {problem}"))
}

/// What [`within`] reports for work that did not finish before its deadline.
const LATE: &str = "it did not finish before the total deadline";

/// Run `work` on its own thread and wait for it until `deadline`; a late result is an error and
/// the thread is left to finish on its own, so a stuck copy cannot hold the fixture past it.
fn within(
    deadline: Instant,
    work: impl FnOnce() -> std::io::Result<()> + Send + 'static,
) -> Result<(), String> {
    if Instant::now() >= deadline {
        // When: now is at or past the deadline, the work is never started.
        return Err(LATE.to_owned());
    }
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        // A worker scheduled at or past the deadline starts nothing; nobody waits for it.
        if Instant::now() >= deadline {
            // When: now is at or past the deadline on the worker, the work is never started.
            return;
        }
        let result = work();
        // Stamped at completion, so a result that finished late is late however soon it is read.
        // A send fails only when the deadline already passed and nobody waits.
        let _ = sender.send((result, Instant::now()));
    });
    let left = deadline.saturating_duration_since(Instant::now());
    match receiver.recv_timeout(left) {
        Ok((_, completed_at)) if completed_at >= deadline => Err(LATE.to_owned()),
        Ok((result, _)) => result.map_err(|error| error.to_string()),
        Err(_) => Err(LATE.to_owned()),
    }
}

/// Stage the pinned runtime with the reviewed setup script into `destination` before `deadline`;
/// a script still running then is killed and reaped, and the failure says whether it was.
fn stage_runtime(manifest_dir: &Path, destination: &Path, run: &RunClock) -> PathBuf {
    let deadline = run.work_deadline();
    let staging = destination.to_path_buf();
    if let Err(problem) = within(deadline, move || std::fs::create_dir_all(&staging)) {
        // The staging directory was not made before the deadline.
        panic!("creating the staging directory failed: {problem}");
    }
    let script = manifest_dir.join("../../scripts/setup-windows-conpty.py");
    let mut child = Command::new("python")
        .arg(&script)
        .arg("--destination")
        .arg(destination)
        .spawn()
        .expect("run python scripts/setup-windows-conpty.py");
    let mut status = None;
    let finished = wait_until(deadline, || {
        status = child.try_wait().ok().flatten();
        status.is_some()
    });
    if !finished {
        // The script is still running at the deadline: end it within the teardown budget.
        let (killed, reaped) = abandon(&mut child, run.teardown_deadline(Instant::now()));
        panic!("staging the ConPTY runtime passed the total deadline (killed: {killed}, reaped: {reaped})");
    }
    assert!(
        status.is_some_and(|status| status.success()),
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

fn run_one_host(
    host: &Path,
    role: &str,
    test_name: &str,
    launch: Launch,
    run: &RunClock,
) -> HostReport {
    let mut command = Command::new(host);
    command
        .args(test_args(test_name))
        .env(ROLE_ENV, role)
        .env(LOOP_MS_ENV, launch.loop_budget.as_millis().to_string())
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
        let (killed, reaped) = abandon(&mut child, run.teardown_deadline(Instant::now()));
        panic!("assign fixture host to its job: {error} (killed: {killed}, reaped: {reaped})");
    }
    if let Some(mut stdin) = child.stdin.take() {
        // When: the host's `stdin` pipe exists, send `go`; a failed write shows as a budget expiry.
        let _ = stdin.write_all(b"go\n");
    }
    let stdout = child.stdout.take().expect("host stdout");
    let (sender, receiver) = mpsc::channel();
    // Each line is stamped when the reader takes it from the pipe, so a late end line is late
    // even if the outer test is slow to dequeue it.
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send((line, Instant::now())).is_err() {
                // When: `sender` fails, the outer test stopped listening for this output.
                return;
            }
        }
    });
    let mut lines = Vec::new();
    let mut finished = false;
    // The deadline was fixed when the run admitted this host, so spawning and the handshake
    // count against it. A line counts by when the reader took it from the pipe: lines already
    // queued with a timely stamp are still read after the clock passes the deadline.
    loop {
        let left = launch.host_deadline.saturating_duration_since(Instant::now());
        let Ok((line, read_at)) = receiver.recv_timeout(left) else {
            // When: `recv_timeout` found no queued line in time, or the reader ended, none follows.
            break;
        };
        if read_at >= launch.host_deadline {
            // When: read_at is at or past launch.host_deadline, the line is late and not evidence.
            break;
        }
        let Some(at) = line.find(LINE_PREFIX) else {
            // When: the `line` has no LINE_PREFIX, it is libtest's own output, not a control line.
            continue;
        };
        let control = line[at..].to_owned();
        // Only an end line whose scan reached DONE, or closed every case but a scan-optional one,
        // is a finished run.
        finished = json_field(&control, "kind").as_deref() == Some("end")
            && matches!(json_field(&control, "scan").as_deref(), Some("done" | "required"));
        let ended = json_field(&control, "kind").as_deref() == Some("end");
        lines.push(control);
        if ended {
            // When: the host `ended` with its end line, every row it reports has arrived.
            break;
        }
    }
    // One teardown allowance from when teardown starts, never past the run's total deadline.
    let started = Instant::now();
    let deadline = run.teardown_deadline(started);
    let reaped = reap(&mut HostTeardown { child: &mut child, job: &job }, started, deadline);
    HostReport { lines, finished, reaped }
}

/// The real teardown steps of one host and its job.
struct HostTeardown<'a> {
    child: &'a mut std::process::Child,
    job: &'a Job,
}

impl Teardown for HostTeardown<'_> {
    fn host_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    fn terminate_job(&mut self) -> bool {
        self.job.terminate()
    }

    fn job_empty(&mut self) -> bool {
        self.job.is_empty()
    }
}

impl Unjobbed for std::process::Child {
    fn kill(&mut self) -> bool {
        std::process::Child::kill(self).is_ok()
    }

    fn exited(&mut self) -> bool {
        matches!(self.try_wait(), Ok(Some(_)))
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
        problems.push(format!(
            "{role}: the host did not finish within its budget, or its scan expired before DONE"
        ));
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

    /// End every process in the job; `false` when the native call failed.
    fn terminate(&self) -> bool {
        // SAFETY: `self.0` is this job's live handle. A failure leaves the close-time kill.
        unsafe { TerminateJobObject(self.0, 1) }.is_ok()
    }

    /// Whether the job holds no live process; a failed query counts as not empty.
    fn is_empty(&self) -> bool {
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
        queried.is_ok() && info.ActiveProcesses == 0
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
