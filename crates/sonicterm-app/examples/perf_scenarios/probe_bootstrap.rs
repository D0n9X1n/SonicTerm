//! Probe setup, process timing, logging and isolated scratch files.

use super::*;

/// Process user and kernel CPU so far, in seconds; zeros when `GetProcessTimes` fails.
#[cfg(windows)]
pub(super) fn cpu_times() -> (f64, f64) {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    let result =
        // SAFETY: the pseudo-handle needs no closing; every output points to writable FILETIME storage.
        unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) };
    if result.is_err() {
        // When: the call failed, zeros mark the figure as missing, as on macOS.
        return (0.0, 0.0);
    }
    let seconds =
        |time: FILETIME| crate::record::filetime_seconds(time.dwLowDateTime, time.dwHighDateTime);
    (seconds(user), seconds(kernel))
}

/// Process user and system CPU so far, in seconds; zeros when `getrusage` fails.
#[cfg(target_os = "macos")]
pub(super) fn cpu_times() -> (f64, f64) {
    let zero = TimeVal { tv_sec: 0, tv_usec: 0 };
    let mut usage = ResourceUsage { ru_utime: zero, ru_stime: zero, _counters: [0; 14] };
    let result =
        // SAFETY: `usage` is a live, writable `struct rusage` with macOS's layout, as `getrusage` requires.
        unsafe { getrusage(RUSAGE_SELF, &mut usage) };
    if result != 0 {
        return (0.0, 0.0);
    }
    let seconds = |time: TimeVal| time.tv_sec as f64 + f64::from(time.tv_usec) / 1e6;
    (seconds(usage.ru_utime), seconds(usage.ru_stime))
}

/// Seconds since the Unix epoch; 0.0 if the clock is before it.
pub(super) fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// Milliseconds from `from` to `to`; zero when `to` is earlier.
pub(super) fn ms_between(from: Instant, to: Instant) -> f64 {
    to.saturating_duration_since(from).as_secs_f64() * 1_000.0
}

/// A seed that differs between runs, for the sentinel nonce.
pub(super) fn nonce_seed() -> u64 {
    let nanos =
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos() as u64);
    nanos ^ (u64::from(std::process::id()) << 32)
}

/// Write `bytes` through a temporary sibling and a rename, so a reader never sees part of a file.
pub(super) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic_with(path, |file| std::io::Write::write_all(file, bytes))
}

/// Fill a temporary sibling of `path` through `fill`, then rename it over `path`.
pub(super) fn write_atomic_with(
    path: &std::path::Path,
    fill: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = std::fs::File::create(&temporary)?;
    fill(&mut file)?;
    drop(file);
    std::fs::rename(&temporary, path)
}

impl Watchdog {
    pub(super) fn arm(deadline: Instant) -> Self {
        let (cancel, receiver) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let wait = deadline.saturating_duration_since(Instant::now()) + WATCHDOG_GRACE;
            if matches!(receiver.recv_timeout(wait), Err(mpsc::RecvTimeoutError::Timeout)) {
                eprintln!(
                    "perf_scenarios: the event loop did not return after the deadline; aborting"
                );
                std::process::abort();
            }
        });
        Self { cancel: Some(cancel), thread: Some(thread) }
    }
}

/// Measure one scenario in a new scratch directory; returns the process exit code.
pub(crate) fn run(request: &RunArgs, allocation_counter: Option<fn() -> u64>) -> u8 {
    // The right to lock lasts only until user input arrives, so the lock comes before anything else;
    // the guard is dropped as `run` returns, on every path.
    #[cfg(windows)]
    let (_foreground_lock, foreground_lock_failure) = ForegroundLock::acquire();
    #[cfg(not(windows))]
    let foreground_lock_failure: Option<String> = None;
    let started = Instant::now();
    let Some(plan) = scenarios::plan(request.scenario, request.variant, request.short) else {
        eprintln!(
            "perf_scenarios: refused: {} has no variant {}",
            request.scenario, request.variant
        );
        return REFUSED;
    };
    let scratch = PathBuf::from(&request.scratch);
    // The new scratch directory and harness.pid come first, before any other file or any window.
    if let Err(error) = std::fs::create_dir(&scratch) {
        eprintln!("perf_scenarios: refused: cannot create {}: {error}", request.scratch);
        return REFUSED;
    }
    if let Err(error) =
        std::fs::write(scratch.join("harness.pid"), format!("{}\n", std::process::id()))
    {
        eprintln!("perf_scenarios: refused: cannot write harness.pid: {error}");
        return REFUSED;
    }
    // Every pane's program inherits the scratch directory through this variable. No other thread
    // exists yet: logging starts in `prepare_scratch` and the watchdog after it.
    #[cfg(windows)]
    std::env::set_var(workload::SCRATCH_ENV, &scratch);
    // Created before the FIFOs are, so every return below, refusals included, removes them.
    #[cfg(unix)]
    let _go_fifos = GoFifos { scratch: scratch.clone(), roles: plan.roles.len() };
    let prepared = match prepare_scratch(&plan, request, &scratch) {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("perf_scenarios: refused: {error}");
            return REFUSED;
        }
    };
    if let Some(note) = &foreground_lock_failure {
        // When: the lock failed, the window may take the foreground as it opens; the run goes on.
        tracing::warn!(target: LOG_TARGET, "{note}");
    }
    let run_deadline = started + Duration::from_secs(plan.timeout_s);
    let watchdog = Watchdog::arm(run_deadline);
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        // Launch must not activate the app, so the user's front application keeps focus.
        builder.with_activate_ignoring_other_apps(false);
    }
    let event_loop = match builder.build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("perf_scenarios: refused: cannot create the event loop: {error}");
            return REFUSED;
        }
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let roles = plan.roles.len();
    let sentinels = (0..roles).map(|role| workload::sentinel_line(role, &prepared.nonce)).collect();
    // The shared clock epoch is set first, so startup's start converts; the boundary does not move.
    crate::guard_transport::api::bootstrap_epoch();
    // Startup is measured from just before the App is built.
    let meter = PhaseMeter::start(
        "startup",
        scenarios::STARTUP_KIND,
        Some(scenarios::STARTUP_ENDPOINT),
        allocation_counter.is_some(),
        None,
        None,
    );
    // Read before the App takes the config, so the presenter record can name the configured mode.
    let software_render_mode = render_mode_text(prepared.config.appearance.software_render_mode);
    let mut app =
        App::new_with_proxy(Theme::default(), prepared.config, Keymap::default(), Some(proxy));
    if plan.status_line {
        // A status-line run counts each taskbar delivery; no platform shell installs a surface here.
        install_counting_taskbar_progress(&mut app);
    }
    let mut probe = Probe {
        sentinels,
        allocation_counter,
        meter: Some(meter),
        software_render_mode,
        foreground_lock_failure,
        ..Probe::new(app, plan, request.clone(), scratch, run_deadline)
    };
    // --counters opens the App's counter gate before its first window; startup's start is the
    // empty baseline read right after, since nothing counted before the gate opened.
    if let Err(reason) = probe.enable_counters() {
        // When: `enable_counters` failed, the run is refused before any window exists.
        eprintln!("perf_scenarios: refused: --counters: {reason}");
        return REFUSED;
    }
    let baseline = probe.counter_totals();
    if let Some(meter) = probe.meter.as_mut() {
        meter.counters_start = baseline;
    }
    if let Err(error) = event_loop.run_app(&mut probe) {
        probe
            .outcome
            .get_or_insert((Status::Invalid, Some(format!("the event loop failed: {error}"))));
    }
    // Every exit path after the App exists tears its sessions down; the group kill ends the anchors.
    let settled = probe.app.finish_session();
    let result = probe.result(settled);
    let status = result.status;
    let document = result.to_json();
    let text = serde_json::to_string_pretty(&document).unwrap_or_else(|_| document.to_string());
    if let Err(error) = write_atomic(&probe.scratch.join("result.json"), text.as_bytes()) {
        eprintln!("perf_scenarios: cannot write result.json: {error}");
    }
    tracing::info!(target: LOG_TARGET, status = status.as_str(), settled, "perf_scenarios result written");
    println!(
        "perf_scenarios {} {}: {} (exit {}){}",
        request.scenario,
        request.variant,
        status.as_str(),
        status.exit_code(),
        result.invalid_reason.map(|reason| format!(": {reason}")).unwrap_or_default()
    );
    drop(probe);
    drop(watchdog);
    drop(prepared.logging);
    status.exit_code()
}

/// Start logging in `dir`. A laps run without `--counters` uses a filter that keeps the counter
/// gate off, so the App counts nothing in a run that reports `off`.
#[cfg(feature = "perf-counters")]
pub(super) fn start_logging(
    config: &sonicterm_logging::LoggingConfig,
    dir: &Path,
    request: &RunArgs,
) -> std::io::Result<sonicterm_logging::LoggingGuard> {
    match workload::logging_filter(request.laps, request.counters) {
        Some(filter) => sonicterm_logging::init_in_with_filter(dir, &filter),
        None => sonicterm_logging::init_in(config, dir),
    }
}

/// Start logging in `dir` at the configured level; this build cannot read the counter gate.
#[cfg(not(feature = "perf-counters"))]
pub(super) fn start_logging(
    config: &sonicterm_logging::LoggingConfig,
    dir: &Path,
    _request: &RunArgs,
) -> std::io::Result<sonicterm_logging::LoggingGuard> {
    sonicterm_logging::init_in(config, dir)
}

/// The configured `[appearance].software_render_mode`, as the config file spells it.
pub(super) fn render_mode_text(mode: SoftwareRenderMode) -> &'static str {
    match mode {
        SoftwareRenderMode::Auto => "auto",
        SoftwareRenderMode::Force => "force",
        SoftwareRenderMode::Off => "off",
    }
}

/// Lay out the scratch directory, load its config, start logging there and write the workload.
pub(super) fn prepare_scratch(
    plan: &Plan,
    request: &RunArgs,
    scratch: &Path,
) -> Result<Prepared, String> {
    for dir in [
        "config",
        "logs",
        "workload/fixtures",
        "roles",
        "sessions",
        "acks",
        "go",
        "done",
        "checkpoints",
    ] {
        std::fs::create_dir_all(scratch.join(dir))
            .map_err(|error| format!("create {dir}: {error}"))?;
    }
    #[cfg(unix)]
    for role in 0..plan.roles.len() {
        // Each role script blocks reading its GO FIFO, so GO releases it at once, with no poll.
        let marker = scratch.join(format!("go/{role}"));
        let made = std::process::Command::new("mkfifo").arg(&marker).status();
        if !made.as_ref().is_ok_and(std::process::ExitStatus::success) {
            // When: mkfifo is missing or failed, the run cannot release its roles without a poll.
            return Err(format!("mkfifo {}: {made:?}", marker.display()));
        }
    }
    let config_path = scratch.join("config/sonicterm.toml");
    std::fs::write(&config_path, scratch_config_text(plan, request)?)
        .map_err(|error| format!("write the config: {error}"))?;
    let config =
        Config::load_strict(&config_path).map_err(|error| format!("load the config: {error:#}"))?;
    let logging = start_logging(&config.logging, &scratch.join("logs"), request)
        .map_err(|error| format!("start logging: {error}"))?;
    // The path exactly as given, so a search of other logs for it proves nothing was written there.
    let (scenario, variant, given) = (plan.scenario, plan.variant, &request.scratch);
    tracing::info!(target: LOG_TARGET, "perf_scenarios run: scenario={scenario} variant={variant} scratch={given}");
    let fixtures = workload::fixtures(plan);
    let nonce = workload::choose_nonce(nonce_seed(), &fixtures);
    let fixture_root = scratch.join("workload/fixtures");
    let mut fixture_bytes = 0_usize;
    for fixture in &fixtures {
        fixture
            .write_under(&fixture_root)
            .map_err(|error| format!("write fixture {}: {error}", fixture.relative_path))?;
        fixture_bytes += fixture.byte_len();
    }
    let fixture_files = fixtures.len();
    tracing::info!(target: LOG_TARGET, fixture_files, fixture_bytes, "perf_scenarios fixtures written");
    write_role_program(plan, request, scratch, &nonce)?;
    Ok(Prepared { config, logging, nonce })
}

/// The scratch config on macOS: the generated role script is every pane's shell.
#[cfg(unix)]
pub(super) fn scratch_config_text(plan: &Plan, request: &RunArgs) -> Result<String, String> {
    Ok(workload::config_toml(plan, &request.scratch, request.laps))
}

/// The scratch config on Windows: this harness binary is every pane's shell, in program mode.
///
/// A path that would break the TOML literal string is refused before any window opens.
#[cfg(windows)]
pub(super) fn scratch_config_text(plan: &Plan, request: &RunArgs) -> Result<String, String> {
    let harness = std::env::current_exe()
        .map_err(|error| format!("cannot find the harness executable: {error}"))?;
    let harness = harness
        .to_str()
        .ok_or_else(|| format!("harness path {} is not UTF-8", harness.display()))?
        .to_owned();
    crate::cli::check_harness_shell(&harness)?;
    Ok(workload::config_toml_with_shell(plan, &harness, request.laps))
}

/// Write `workload/role.sh`, the shell every pane runs on macOS, and make it executable.
#[cfg(unix)]
pub(super) fn write_role_program(
    plan: &Plan,
    request: &RunArgs,
    scratch: &Path,
    nonce: &str,
) -> Result<(), String> {
    let script_path = scratch.join("workload/role.sh");
    std::fs::write(&script_path, workload::role_script(plan, &request.scratch, nonce))
        .map_err(|error| format!("write role.sh: {error}"))?;
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("make role.sh executable: {error}"))
}

/// Write `workload/program.json`, from which each pane's program rebuilds the plan and nonce.
#[cfg(windows)]
pub(super) fn write_role_program(
    plan: &Plan,
    _request: &RunArgs,
    scratch: &Path,
    nonce: &str,
) -> Result<(), String> {
    let spec = workload::program_json(plan, nonce);
    write_atomic(&scratch.join("workload/program.json"), spec.as_bytes())
        .map_err(|error| format!("write program.json: {error}"))
}
