//! Native program-role execution, GO synchronization and host custody.

use super::*;

/// How often a pane's program polls for its acknowledgement and GO, as the script's `sleep 0.05`.
#[cfg(any(windows, test))]
pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// The pause after each `date` line, as the script's `sleep 0.01`.
#[cfg(any(windows, test))]
pub(super) const DATE_INTERVAL: Duration = Duration::from_millis(10);
/// The pause after each frame, as the script's `sleep 0.016`.
#[cfg(any(windows, test))]
pub(super) const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// What a pane's program needs from its host: a clock, sleeps, file checks and the idle shell.
///
/// Tests inject a fake whose clock moves only by the sleeps it records.
#[cfg(any(windows, test))]
pub(crate) trait ProgramHost {
    /// Seconds since the Unix epoch, for `date` lines.
    fn now_unix_s(&self) -> u64;
    /// Sleeps for `duration`; an error ends the program.
    fn sleep(&mut self, duration: Duration) -> std::io::Result<()>;
    /// Whether `path` exists.
    fn exists(&self, path: &Path) -> bool;
    /// Prepare to be released by GO, before READY is printed; a host that cannot block polls instead.
    fn arm_go(&mut self, _scratch: &Path, _role: usize) {}
    /// Wait until GO releases the role whose marker is `marker`.
    fn wait_go(&mut self, marker: &Path) -> Result<(), String>
    where
        Self: Sized,
    {
        wait_for(self, marker)
    }
    /// Runs the idle shell at the fixed prompt and waits for it to exit.
    fn run_shell(&mut self) -> std::io::Result<()>;
}

/// What every step of one role's run needs.
#[cfg(any(windows, test))]
pub(super) struct RoleRun<'scratch> {
    /// The run's scratch directory.
    pub(super) scratch: &'scratch Path,
    /// The role this program claimed.
    pub(super) role: usize,
    /// The run's sentinel nonce.
    pub(super) nonce: String,
    /// How long an unplanned role, or a role that ends sleeping, sleeps.
    pub(super) bound: Duration,
}

/// Runs a pane's program in `scratch`: claim a role, record the session, wait for the
/// acknowledgement, print READY, wait for GO, then run the role's steps.
///
/// Returns 0 when the steps finish, 1 with no output when the role exits after GO (`role-exit`),
/// or 1 after one line on `err` naming the first error.
#[cfg(any(windows, test))]
pub(crate) fn run_steps(
    scratch: &Path,
    host: &mut impl ProgramHost,
    out: &mut impl Write,
    err: &mut impl Write,
) -> u8 {
    match run_role(scratch, host, out) {
        Ok(code) => code,
        Err(reason) => {
            // The error line is the program's last output; if it cannot be written either,
            // nothing else can report it, so its own failure is ignored.
            let _ = writeln!(err, "perf_scenarios program: {reason}");
            let _ = err.flush();
            1
        }
    }
}

/// One role's whole run and its exit code; the error names the step that failed.
#[cfg(any(windows, test))]
pub(super) fn run_role(
    scratch: &Path,
    host: &mut impl ProgramHost,
    out: &mut impl Write,
) -> Result<u8, String> {
    let spec_path = scratch.join("workload/program.json");
    let text = std::fs::read_to_string(&spec_path)
        .map_err(|error| format!("read {}: {error}", spec_path.display()))?;
    let spec = parse_program_json(&text)?;
    // The plan is rebuilt from this same binary, so roles, fixtures and the bound cannot drift.
    let plan = crate::scenarios::plan(&spec.scenario, &spec.variant, spec.short)
        .ok_or_else(|| format!("program.json names no plan: {} {}", spec.scenario, spec.variant))?;
    let run = RoleRun {
        scratch,
        role: claim_role(scratch)?,
        nonce: spec.nonce,
        bound: Duration::from_secs(plan.timeout_s + ANCHOR_MARGIN_S),
    };
    write_session(&run)?;
    wait_for(host, &scratch.join(format!("acks/{}", run.role)))?;
    host.arm_go(scratch, run.role);
    writeln!(out, "{}", ready_line(run.role))
        .and_then(|()| out.flush())
        .map_err(|error| format!("write READY: {error}"))?;
    // Nothing is printed until GO, so the READY row is the last row before the workload.
    host.wait_go(&scratch.join(format!("go/{}", run.role)))?;
    // An unplanned pane neither prints nor prompts, like the role script's `*)` branch.
    let steps = plan
        .roles
        .get(run.role)
        .map_or_else(|| vec![ProgramStep::SleepBound], |workload| program_steps(*workload));
    for step in steps {
        if step == ProgramStep::ExitAfterGo {
            // When: the role's program exits after GO, it ends here with 1 and prints nothing more.
            return Ok(1);
        }
        run_step(&run, step, host, out)?;
    }
    Ok(0)
}

/// Claims the lowest free role by creating `roles/<n>`, as the script's `mkdir` loop does.
#[cfg(any(windows, test))]
pub(super) fn claim_role(scratch: &Path) -> Result<usize, String> {
    let mut role = 0;
    loop {
        match std::fs::create_dir(scratch.join(format!("roles/{role}"))) {
            Ok(()) => return Ok(role),
            // When: another pane claimed this role first, the next number is tried.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => role += 1,
            Err(error) => return Err(format!("claim role {role}: {error}")),
        }
    }
}

/// Writes `sessions/<role>.json` through a temporary sibling and a rename, so no reader sees part.
#[cfg(any(windows, test))]
pub(super) fn write_session(run: &RoleRun<'_>) -> Result<(), String> {
    let path = run.scratch.join(format!("sessions/{}.json", run.role));
    let temporary = run.scratch.join(format!("sessions/{}.json.tmp", run.role));
    let record = program_session_json(run.role, std::process::id()) + "\n";
    std::fs::write(&temporary, record)
        .and_then(|()| std::fs::rename(&temporary, &path))
        .map_err(|error| format!("write {}: {error}", path.display()))
}

/// Connect to the port role `role`'s program published when it armed GO, releasing its accept; a role
/// that published none polls for the marker, so there is nothing to connect to.
#[cfg(windows)]
pub(crate) fn connect_go(scratch: &Path, role: usize) -> std::io::Result<()> {
    let Ok(text) = std::fs::read_to_string(scratch.join(format!("go/{role}.port"))) else {
        // When: the role published no port, it polls for the marker instead.
        return Ok(());
    };
    let port: u16 = text
        .trim()
        .parse()
        .map_err(|error| std::io::Error::other(format!("GO port {text:?}: {error}")))?;
    std::net::TcpStream::connect(("127.0.0.1", port)).map(drop)
}

/// Polls `path` every 50 ms until it exists.
#[cfg(any(windows, test))]
pub(super) fn wait_for(host: &mut impl ProgramHost, path: &Path) -> Result<(), String> {
    while !host.exists(path) {
        host.sleep(POLL_INTERVAL)
            .map_err(|error| format!("wait for {}: {error}", path.display()))?;
    }
    Ok(())
}

/// Copies `path` to `out` and flushes, as `cat` does.
#[cfg(any(windows, test))]
pub(super) fn copy_file(path: &Path, out: &mut impl Write) -> Result<(), String> {
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    std::io::copy(&mut file, &mut *out)
        .and_then(|_| out.flush())
        .map_err(|error| format!("copy {}: {error}", path.display()))
}

/// Performs one step, writing what the role script's lines for it print.
#[cfg(any(windows, test))]
pub(super) fn run_step(
    run: &RoleRun<'_>,
    step: ProgramStep,
    host: &mut impl ProgramHost,
    out: &mut impl Write,
) -> Result<(), String> {
    let fixture_root = run.scratch.join("workload/fixtures");
    match step {
        ProgramStep::Yes(lines) => write_yes(&mut *out, lines)
            .and_then(|()| out.flush())
            .map_err(|error| format!("write {lines} y lines: {error}")),
        ProgramStep::Cat(name) => copy_file(&fixture_root.join(name), out),
        // The loop never ends by itself; only an error, or the pane's teardown, ends it.
        ProgramStep::DateLoop => loop {
            writeln!(out, "{}", date_line(host.now_unix_s()))
                .and_then(|()| out.flush())
                .map_err(|error| format!("write a date line: {error}"))?;
            host.sleep(DATE_INTERVAL)
                .map_err(|error| format!("pause after a date line: {error}"))?;
        },
        ProgramStep::Frames(count) => {
            // The same order and names as the script's `frames/$frame` loop.
            for frame in 0..count {
                copy_file(&fixture_root.join(format!("frames/{frame}")), out)?;
                host.sleep(FRAME_INTERVAL)
                    .map_err(|error| format!("pause after frame {frame}: {error}"))?;
            }
            Ok(())
        }
        ProgramStep::Sentinel => writeln!(out, "{}", sentinel_line(run.role, &run.nonce))
            .and_then(|()| out.flush())
            .map_err(|error| format!("write the sentinel: {error}")),
        ProgramStep::Done => std::fs::write(run.scratch.join(format!("done/{}", run.role)), b"")
            .map_err(|error| format!("write done/{}: {error}", run.role)),
        ProgramStep::Shell => {
            // The shell writes to the console itself, so this program's output must land first.
            out.flush().map_err(|error| format!("flush before the shell: {error}"))?;
            host.run_shell().map_err(|error| format!("run the idle shell: {error}"))
        }
        ProgramStep::ExitAfterGo => Err("ExitAfterGo ends the role in run_role".to_owned()),
        ProgramStep::RestFixture => {
            let target = run.scratch.join("workload").join(REST_TARGET_NAME);
            std::fs::write(&target, b"")
                .map_err(|error| format!("create {}: {error}", target.display()))?;
            write!(out, "\x1b[H\x1b[2J{REST_URL}\n{}\n\n", target.display())
                .and_then(|()| out.flush())
                .map_err(|error| format!("write the rest fixture: {error}"))
        }
        // The loop never ends by itself; only an error, or the pane's teardown, ends it.
        ProgramStep::RestRegion => {
            let top = crate::scenarios::REST_REGION_TOP;
            write!(out, "\x1b[{top}r\x1b[999;1H")
                .and_then(|()| out.flush())
                .map_err(|error| format!("set the rest scroll region: {error}"))?;
            loop {
                writeln!(out, "{}", date_line(host.now_unix_s()))
                    .and_then(|()| out.flush())
                    .map_err(|error| format!("write a date line: {error}"))?;
                host.sleep(DATE_INTERVAL)
                    .map_err(|error| format!("pause after a date line: {error}"))?;
            }
        }
        ProgramStep::SleepBound => {
            host.sleep(run.bound).map_err(|error| format!("sleep out the bound: {error}"))
        }
    }
}

/// The real host: the system clock, real sleeps, the file system and `cmd.exe`.
#[cfg(windows)]
pub(super) struct NativeHost {
    /// The loopback listener GO connects to, bound by `arm_go`; `None` waits for the marker by poll.
    pub(super) go_listener: Option<std::net::TcpListener>,
}

#[cfg(windows)]
impl ProgramHost for NativeHost {
    fn now_unix_s(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs())
    }

    fn sleep(&mut self, duration: Duration) -> std::io::Result<()> {
        std::thread::sleep(duration);
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    /// Bind a loopback port and publish it beside the GO marker, so GO releases a blocking accept.
    fn arm_go(&mut self, scratch: &Path, role: usize) {
        let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", 0)) else {
            // When: no loopback port can be bound, GO is awaited by poll.
            return;
        };
        let Ok(port) = listener.local_addr().map(|address| address.port()) else {
            // When: the bound port cannot be read, GO is awaited by poll.
            return;
        };
        let staged = scratch.join(format!("go/{role}.port.tmp"));
        let published = std::fs::write(&staged, port.to_string())
            .and_then(|()| std::fs::rename(&staged, scratch.join(format!("go/{role}.port"))));
        if published.is_ok() {
            // When: the port file is in place, the harness connects to it before writing the marker.
            self.go_listener = Some(listener);
        }
    }

    /// Block in accept until GO connects; a marker already written (a replay) releases at once.
    fn wait_go(&mut self, marker: &Path) -> Result<(), String> {
        match self.go_listener.take() {
            Some(_) if marker.exists() => Ok(()),
            Some(listener) => listener
                .accept()
                .map(|_| ())
                .map_err(|error| format!("wait for GO on {}: {error}", marker.display())),
            None => wait_for(self, marker),
        }
    }

    /// `cmd.exe /d`, skipping AutoRun commands, at `PROMPT=perf$$$S`, on the inherited console.
    fn run_shell(&mut self) -> std::io::Result<()> {
        let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        // The shell's exit status is the session's, not the role's: the role ran its workload, so
        // the program ends normally whichever code the shell returns.
        std::process::Command::new(shell).arg("/d").env("PROMPT", CMD_PROMPT).status().map(|_| ())
    }
}

/// Runs this process as a pane's role program in `scratch`; returns the process exit code.
#[cfg(windows)]
pub(crate) fn run_program(scratch: &Path) -> u8 {
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    run_steps(
        scratch,
        &mut NativeHost { go_listener: None },
        &mut stdout.lock(),
        &mut stderr.lock(),
    )
}

#[cfg(test)]
#[path = "workload_native_tests.rs"]
mod workload_native_tests;
