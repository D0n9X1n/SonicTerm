use super::*;

/// An explicitly selected ignored test must execute in the bounded child, not report a skipped body.
#[test]
fn isolated_child_executes_an_ignored_test() {
    const NAME: &str = "app::pty_test_support::pty_test_support_tests::ignored_isolation_fixture";
    let result = run_test_child(NAME, Duration::from_secs(10));
    assert!(!result.timed_out && result.status.success(), "{}", result.diagnostic());
    assert!(result.output.contains("IGNORED_ISOLATION_BODY"), "{}", result.diagnostic());
    assert!(result.output.contains("1 passed; 0 failed"), "{}", result.diagnostic());
}

/// A baseline must retain initial configuration and final summaries beyond the ordinary diagnostic-tail size.
#[test]
fn baseline_complete_capture_keeps_first_and_last_records() {
    const NAME: &str = "app::pty_test_support::pty_test_support_tests::large_output_fixture";
    let result =
        run_test_child_with_config(NAME, IsolationConfig::baseline(Duration::from_secs(10)));
    assert!(!result.timed_out && result.status.success(), "{}", result.diagnostic());
    assert!(
        result.output.contains("BASELINE_INITIAL_CONFIG"),
        "initial configuration was truncated"
    );
    assert!(result.output.contains("BASELINE_FINAL_SUMMARY"), "final summary was lost");
}

/// Overflow is explicit while pipes keep draining to a successful child exit instead of backpressuring it.
#[test]
fn baseline_capture_overflow_is_explicit_and_still_drains() {
    const NAME: &str = "app::pty_test_support::pty_test_support_tests::large_output_fixture";
    let result = run_test_child_with_config(
        NAME,
        IsolationConfig {
            output_limit: 4096,
            ..IsolationConfig::baseline(Duration::from_secs(10))
        },
    );
    assert!(!result.timed_out && result.status.success(), "{}", result.diagnostic());
    assert!(result.capture_overflow, "overflow must reject a complete capture");
    assert_eq!(result.output.len(), 4096);
    assert!(result.diagnostic().contains("capture_overflow=true"));
    assert!(!result.output.contains("BASELINE_FINAL_SUMMARY"));
}

/// Ordinary callers continue to keep only their successful child's last diagnostic bytes.
#[test]
fn ordinary_capture_keeps_its_existing_tail_policy() {
    const NAME: &str = "app::pty_test_support::pty_test_support_tests::large_output_fixture";
    let result = run_test_child(NAME, Duration::from_secs(10));
    assert!(!result.timed_out && result.status.success(), "{}", result.diagnostic());
    assert!(!result.capture_overflow);
    assert_eq!(result.output.len(), OUTPUT_LIMIT);
    assert!(!result.output.contains("BASELINE_INITIAL_CONFIG"));
    assert!(result.output.contains("BASELINE_FINAL_SUMMARY"));
}

/// The public baseline isolation entry must fail, not merely expose a flag, when complete capture overflows.
#[test]
fn baseline_output_overflow_fails_isolation() {
    const NAME: &str = "app::pty_test_support::pty_test_support_tests::overflowing_output_fixture";
    let result = run_test_child(NAME, Duration::from_secs(10));
    assert!(!result.timed_out, "{}", result.diagnostic());
    assert!(!result.status.success(), "overflow was accepted as a successful baseline");
    assert!(
        result.output.contains("complete output exceeded its capture limit"),
        "{}",
        result.diagnostic()
    );
}

/// Exercise the real parent entry point with a child that finishes writing after its finite capture fills.
#[test]
#[ignore = "fixture for complete-output overflow rejection"]
fn overflowing_output_fixture() {
    if std::env::var_os("SONICTERM_OVERFLOW_OUTPUT_LEAF").is_some() {
        large_output_fixture();
        return;
    }
    std::env::set_var("SONICTERM_OVERFLOW_OUTPUT_LEAF", "1");
    std::env::remove_var(CHILD_MARKER);
    assert!(isolated_with_output(IsolationConfig {
        output_limit: 4096,
        ..IsolationConfig::baseline(Duration::from_secs(5))
    }));
}

/// Baseline configuration honors its requested short deadline and kills only the deliberately spawned child tree.
#[test]
fn baseline_config_deadline_cleans_owned_child_tree() {
    const NAME: &str = "app::pty_test_support::pty_test_support_tests::configured_deadline_fixture";
    let mut config = super::super::close_baseline_tests::baseline_isolation_config();
    config.timeout = Duration::from_secs(2);
    let result = run_test_child_with_config(NAME, config);
    assert!(result.timed_out && !result.status.success(), "{}", result.diagnostic());
    assert!(!result.capture_overflow, "{}", result.diagnostic());
    let descendant = result
        .output
        .lines()
        .find_map(|line| line.strip_prefix("CONTROLLED_DESCENDANT pid=")?.parse::<u32>().ok())
        .expect("owned child reports its descendant");
    let until = Instant::now() + Duration::from_secs(3);
    while process_alive(descendant) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_alive(descendant),
        "controlled descendant {descendant} survived configured deadline"
    );
}

/// This process tree has no PTY, and its handles/pipes belong only to the surrounding deadline regression.
#[test]
#[ignore = "owned child tree for explicit isolation deadline"]
fn configured_deadline_fixture() {
    let child = Command::new(std::env::current_exe().unwrap())
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
        .expect("spawn owned deadline descendant");
    println!("CONTROLLED_DESCENDANT pid={}", child.id());
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(5));
    drop(child);
}

/// Deterministic child output exceeds the ordinary tail without running any native PTY baseline.
#[test]
#[ignore = "child output fixture for bounded complete capture"]
fn large_output_fixture() {
    let mut output = std::io::stdout().lock();
    writeln!(output, "BASELINE_INITIAL_CONFIG").unwrap();
    for _ in 0..80 {
        output.write_all(&[b'x'; 1024]).unwrap();
        writeln!(output).unwrap();
    }
    writeln!(output, "BASELINE_FINAL_SUMMARY").unwrap();
}

/// The observational harness must emit real samples and percentile rows for both scenarios in its bounded child.
#[test]
#[ignore = "runs the complete native close baseline as a harness contract check"]
fn baseline_harness_reports_both_scenarios() {
    const NAME: &str = "app::close_baseline_tests::pty_close_baseline";
    let result = run_test_child_with_config(
        NAME,
        super::super::close_baseline_tests::baseline_isolation_config(),
    );
    assert!(
        !result.timed_out && result.status.success() && !result.capture_overflow,
        "{}",
        result.diagnostic()
    );
    assert!(result.output.contains("1 passed; 0 failed"), "{}", result.diagnostic());
    for scenario in ["ordinary", "stalled"] {
        let process_prefix = format!("PTY_CLOSE_BASELINE process scenario={scenario}");
        let process_rows: Vec<_> =
            result.output.lines().filter(|line| line.starts_with(&process_prefix)).collect();
        assert!(!process_rows.is_empty(), "missing native process observations");
        assert!(
            process_rows.iter().all(|line| line.contains(" state=")),
            "observations omit native state: {process_rows:?}"
        );
        for measure in ["close_call", "native_settlement"] {
            for sample in 1..=super::super::close_baseline_tests::SAMPLES {
                let row = format!(
                    "PTY_CLOSE_BASELINE sample scenario={scenario} measure={measure} sample={sample}"
                );
                assert!(result.output.contains(&row), "missing {row}: {}", result.diagnostic());
            }
            let row = format!("PTY_CLOSE_BASELINE summary scenario={scenario} measure={measure}");
            let summary = result
                .output
                .lines()
                .find(|line| line.starts_with(&row))
                .unwrap_or_else(|| panic!("missing {row}: {}", result.diagnostic()));
            for field in ["censored=", "p50_ms=", "p95_ms=", "max_ms="] {
                assert!(summary.contains(field), "missing {field}: {summary}");
            }
        }
    }
}

/// The parent regression invokes this ignored body only through the real isolated-child runner.
#[test]
#[ignore = "fixture for explicitly selected ignored child execution"]
fn ignored_isolation_fixture() {
    if isolated() {
        return;
    }
    println!("IGNORED_ISOLATION_BODY pid={}", std::process::id());
}

/// Baseline callers can publish successful child output while ordinary isolation stays quiet.
#[test]
fn successful_isolation_forwards_output_only_when_requested() {
    for (name, visible) in [("baseline_output_fixture", true), ("quiet_output_fixture", false)] {
        let name = format!("app::pty_test_support::pty_test_support_tests::{name}");
        let result = run_test_child(&name, Duration::from_secs(10));
        assert!(!result.timed_out && result.status.success(), "{}", result.diagnostic());
        assert_eq!(
            result.output.contains("ISOLATED_OUTPUT_BODY"),
            visible,
            "{}",
            result.diagnostic()
        );
    }
}

/// The child fixture must request output through the same entry point as a printed baseline.
#[test]
#[ignore = "fixture for successful baseline output forwarding"]
fn baseline_output_fixture() {
    nested_output_fixture(|| {
        isolated_with_output(IsolationConfig::baseline(Duration::from_secs(10)))
    });
}

/// Existing isolated tests must not begin printing successful child diagnostics.
#[test]
#[ignore = "fixture for quiet successful isolation"]
fn quiet_output_fixture() {
    nested_output_fixture(isolated);
}

fn nested_output_fixture(isolate: fn() -> bool) {
    let thread = std::thread::current();
    let name = thread.name().unwrap();
    if std::env::var("SONICTERM_OUTPUT_FIXTURE_LEAF").is_ok_and(|value| value == name) {
        println!("ISOLATED_OUTPUT_BODY");
        return;
    }
    // Only this exact ignored test runs in its process; reset the marker to exercise the parent entry point.
    std::env::set_var("SONICTERM_OUTPUT_FIXTURE_LEAF", name);
    std::env::remove_var(CHILD_MARKER);
    assert!(isolate());
}

/// An isolated test deadline must kill its real PTY shell and retain the last phase and non-success exit.
#[test]
fn timeout_reaps_real_pty_and_reports_last_phase() {
    const NAME: &str = "app::pty_test_support::pty_test_support_tests::timeout_reaps_real_pty_and_reports_last_phase";
    if is_test_child(NAME) {
        phase(0, "spawn-begin");
        #[cfg(windows)]
        let (program, args) = ("cmd.exe", vec!["/D".into(), "/Q".into()]);
        #[cfg(unix)]
        let (program, args) = ("/bin/sh", vec!["-s".into()]);
        let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(program, &args, 80, 24).unwrap();
        let pid = pty.pid().unwrap();
        record_process(pid, true);
        phase(pid as u64, "intentional-wait");
        std::thread::sleep(Duration::from_secs(8));
        phase(pid as u64, "drop-begin");
        drop(pty);
        record_process(pid, false);
        return;
    }
    let result = run_test_child(NAME, Duration::from_secs(3));
    assert!(result.timed_out, "deadline was not enforced: {}", result.diagnostic());
    assert!(!result.status.success(), "killed child reported success");
    assert!(
        result.elapsed < Duration::from_secs(7),
        "timeout did not bound the complete child lifecycle"
    );
    assert!(result.last_phase().contains("intentional-wait"), "{}", result.diagnostic());
    let pid = result
        .output
        .lines()
        .find_map(|line| line.strip_prefix("PTY_PROCESS start=")?.parse::<u32>().ok())
        .expect("child must report its real shell PID");
    let deadline = Instant::now() + Duration::from_secs(3);
    while process_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_alive(pid),
        "PTY shell {pid} survived its test deadline: {}",
        process_detail(pid)
    );
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };
    // SAFETY: the test only waits on this queried process handle, then closes its owned handle.
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(handle) => {
                let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
                CloseHandle(handle).unwrap();
                running
            }
            Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => false,
            Err(error) => panic!("cannot inspect test shell {pid}: {error}"),
        }
    }
}

/// On Linux a killed shell counts as ended once it is a zombie: its test process is gone, and a container's PID 1
/// may never reap it.
#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    stat_shows_running(std::fs::read_to_string(format!("/proc/{pid}/stat")), pid)
}

/// Only a missing process, or a zombie or dead state, ends the shell; any other read error fails the check.
#[cfg(target_os = "linux")]
fn stat_shows_running(stat: std::io::Result<String>, pid: u32) -> bool {
    let stat = match stat {
        Ok(stat) => stat,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            return false;
        }
        Err(error) => panic!("cannot inspect test shell {pid}: {error}"),
    };
    // The state letter follows the parenthesized command name, which can itself contain spaces or parentheses.
    let state = stat.rsplit_once(") ").and_then(|(_, rest)| rest.chars().next());
    !matches!(state, Some('Z' | 'X'))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_alive(pid: u32) -> bool {
    let result = {
        // SAFETY: signal zero queries the recorded shell PID without modifying a process.
        unsafe { libc::kill(pid as libc::pid_t, 0) }
    };
    if result == 0 {
        return true;
    }
    let error = std::io::Error::last_os_error();
    // Only a missing process counts as ended; any other error cannot prove termination.
    assert_eq!(error.raw_os_error(), Some(libc::ESRCH), "cannot inspect test shell {pid}: {error}");
    false
}

/// A `/proc` read error other than a missing process must fail the deadline check instead of counting as an end.
#[cfg(target_os = "linux")]
#[test]
fn linux_shell_state_counts_only_absence_or_zombie_as_ended() {
    use std::io::{Error, ErrorKind};
    assert!(!stat_shows_running(Err(Error::from(ErrorKind::NotFound)), 7));
    assert!(!stat_shows_running(Err(Error::from_raw_os_error(libc::ESRCH)), 7));
    assert!(!stat_shows_running(Ok("7 (sh) Z 1 7".into()), 7));
    assert!(stat_shows_running(Ok("7 (a) b) S 1 7".into()), 7));
    let unreadable = std::panic::catch_unwind(|| {
        stat_shows_running(Err(Error::from_raw_os_error(libc::EMFILE)), 7)
    });
    assert!(unreadable.is_err(), "an unreadable stat file counted as an ended shell");
}

/// The shell's kernel state line on Linux, so a survival failure shows whether the process still runs.
#[cfg(target_os = "linux")]
fn process_detail(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_else(|error| error.to_string())
}

#[cfg(not(target_os = "linux"))]
fn process_detail(_pid: u32) -> String {
    String::new()
}

/// Lines `held_pipe_report_fixture` printed in an isolated child, and the probe pipe's two
/// descriptors there: the writer's number and a copy near the top of the usable range. Both stay
/// inheritable for the whole spawn; `mark_close_on_exec` decides whether the child marks them.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn held_pipe_report(mark_close_on_exec: bool) -> (Vec<String>, [i32; 2]) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    const NAME: &str = "app::pty_test_support::pty_test_support_tests::held_pipe_report_fixture";
    let (reader, writer) = std::io::pipe().expect("probe pipe");
    let mut identities = Vec::new();
    for descriptor in [reader.as_raw_fd(), writer.as_raw_fd()] {
        let cleared =
            // SAFETY: `reader` and `writer` keep `descriptor` open for this whole test.
            unsafe { libc::fcntl(descriptor, libc::F_SETFD, 0) };
        assert_eq!(cleared, 0, "clear close-on-exec on the probe pipe");
        let (_, identity) = pipe_identity(descriptor).expect("probe pipe identity");
        identities.push(identity);
    }
    let mut open_limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    let status =
        // SAFETY: `open_limit` is a writable `rlimit`.
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut open_limit) };
    assert_eq!(status, 0, "read RLIMIT_NOFILE");
    let usable_end =
        i32::try_from(open_limit.rlim_cur).unwrap_or(i32::MAX).min(descriptor_table_end());
    let high_floor = usable_end.saturating_sub(64).max(3);
    let high_descriptor =
        // SAFETY: `F_DUPFD` copies `writer`'s open descriptor onto a free number without close-on-exec.
        unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_DUPFD, high_floor) };
    assert!(high_descriptor >= high_floor, "copy the probe pipe to a high descriptor");
    let _high_copy =
        // SAFETY: `high_descriptor` is the new descriptor `F_DUPFD` returned, and only this owns it.
        unsafe { OwnedFd::from_raw_fd(high_descriptor) };
    let config = IsolationConfig { timeout: Duration::from_secs(10), ..IsolationConfig::ORDINARY };
    let result = run_test_child_marking(NAME, config, mark_close_on_exec);
    assert!(!result.timed_out && result.status.success(), "{}", result.diagnostic());
    assert!(result.output.contains("HELD_PIPE_SCAN_DONE"), "{}", result.diagnostic());
    let inherited = result
        .output
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("HELD_PIPE "))
        .filter(|line| {
            line.rsplit(' ')
                .next()
                .is_some_and(|identity| identities.iter().any(|known| known == identity))
        })
        .map(str::to_owned)
        .collect();
    (inherited, [writer.as_raw_fd(), high_descriptor])
}

/// An isolated child must not inherit a pipe its parent left open to children, at any descriptor
/// number. On macOS a sibling test's pipes stay inheritable for a moment while it spawns, and an
/// isolated child holding one would keep it open until the child exits. This probe pipe stays
/// inheritable for the whole spawn, and a copy of it sits near the top of the descriptor range.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn isolated_child_does_not_inherit_open_parent_pipes() {
    let (inherited, _) = held_pipe_report(true);
    assert!(inherited.is_empty(), "the isolated child kept the probe pipe open: {inherited:?}");
}

/// The inheritance assertion can fire: without the close-on-exec hook the child reports the probe
/// pipe, including its copy near the top of the range, so an empty report above is not vacuous.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unmarked_child_reports_the_inherited_probe_pipe() {
    let (inherited, [writer, high_descriptor]) = held_pipe_report(false);
    for descriptor in [writer, high_descriptor] {
        let prefix = format!("HELD_PIPE {descriptor} ");
        assert!(
            inherited.iter().any(|line| line.starts_with(&prefix)),
            "descriptor {descriptor} missing from {inherited:?}"
        );
    }
}

/// The fallback walks a bound up to 2^20 and refuses anything larger, never a smaller bound.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn fallback_scans_up_to_two_to_the_twenty_and_refuses_above() {
    assert_eq!(FallbackScan::for_bound(138_240), FallbackScan::Scan(138_240));
    assert_eq!(FallbackScan::for_bound(1 << 20), FallbackScan::Scan(1 << 20));
    assert_eq!(FallbackScan::for_bound((1 << 20) + 1), FallbackScan::Refuse((1 << 20) + 1));
    assert_eq!(FallbackScan::for_bound(2_147_483_584), FallbackScan::Refuse(2_147_483_584));
}

/// With `close_range` skipped and a bound above 2^20, the spawn fails at once with the explicit
/// `EOVERFLOW` refusal instead of walking two billion descriptor numbers.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn forced_fallback_above_the_limit_fails_the_spawn_promptly() {
    let plan = CloseOnExecPlan {
        try_close_range: false,
        fallback: FallbackScan::for_bound(2_147_483_584),
    };
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exit 0"]).stdout(Stdio::null()).stderr(Stdio::null());
    close_inherited_descriptors_on_exec(&mut command, plan);
    let started = Instant::now();
    let error = command.spawn().expect_err("the refused fallback fails the spawn");
    assert!(started.elapsed() < Duration::from_secs(5), "refused promptly");
    assert_eq!(error.raw_os_error(), Some(libc::EOVERFLOW));
    let message = spawn_failure(&error, plan);
    assert!(message.contains("2147483584") && message.contains("2^20"), "{message}");
}

/// The descriptor listing finds an open pipe, and an unreadable listing fails the fixture rather
/// than reporting "no pipes".
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn unreadable_descriptor_listing_fails_the_fixture() {
    use std::os::fd::AsRawFd;
    let listing = if cfg!(target_os = "linux") { "/proc/self/fd" } else { "/dev/fd" };
    let (reader, _writer) = std::io::pipe().expect("probe pipe");
    let descriptors = open_descriptors(std::path::Path::new(listing)).expect("list descriptors");
    assert!(descriptors.contains(&reader.as_raw_fd()), "{descriptors:?}");
    let missing = std::env::temp_dir().join("sonicterm-no-such-descriptor-listing");
    let outcome = std::panic::catch_unwind(|| report_held_pipes_listed(&missing));
    assert!(outcome.is_err(), "an unreadable listing must panic before HELD_PIPE_SCAN_DONE");
}

/// `descriptor`'s `fstat` identity as `dev:ino` and whether it is a pipe, or `None` when it is not
/// open. Parent and child both use `fstat`: on macOS a `/dev/fd/N` path stat names the fdesc node,
/// not the pipe, so the two would never match.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn pipe_identity(descriptor: i32) -> Option<(bool, String)> {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let borrowed =
        // SAFETY: `ManuallyDrop` below never closes `descriptor`; `fstat` on a closed number just fails.
        unsafe { std::fs::File::from_raw_fd(descriptor) };
    // `ManuallyDrop` keeps the borrowed descriptor open; `metadata` is one `fstat`.
    let file = std::mem::ManuallyDrop::new(borrowed);
    let metadata = file.metadata().ok()?;
    Some((metadata.file_type().is_fifo(), format!("{}:{}", metadata.dev(), metadata.ino())))
}

/// Print a `HELD_PIPE <descriptor> <dev>:<ino>` line for each of `descriptors` above stderr that is
/// a pipe. A number closed since it was listed (the listing's own directory) is skipped.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn report_held_pipes(descriptors: impl IntoIterator<Item = i32>) {
    for descriptor in descriptors.into_iter().filter(|descriptor| *descriptor > 2) {
        let Some((true, identity)) = pipe_identity(descriptor) else {
            // When: pipe_identity is None or not a pipe: closed since listing, or another file type.
            continue;
        };
        println!("HELD_PIPE {descriptor} {identity}");
    }
}

/// Report the pipes among the descriptors `listing` names, panicking when it cannot be read whole,
/// then print the completion marker.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn report_held_pipes_listed(listing: &std::path::Path) {
    let descriptors = open_descriptors(listing)
        .unwrap_or_else(|error| panic!("list open descriptors in {}: {error}", listing.display()));
    report_held_pipes(descriptors);
    println!("HELD_PIPE_SCAN_DONE");
}

/// Report every pipe this process holds above stderr, at any descriptor number, then a completion
/// marker. Linux lists `/proc/self/fd`, whose `fs.nr_open` can be near 2^31; macOS walks every
/// number below `kern.maxfilesperproc`. Each report line gives the number and the pipe's identity.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[ignore = "fixture for inherited pipe reporting"]
fn held_pipe_report_fixture() {
    #[cfg(target_os = "linux")]
    report_held_pipes_listed(std::path::Path::new("/proc/self/fd"));
    #[cfg(target_os = "macos")]
    {
        let open = (3..descriptor_table_end()).filter(|descriptor| {
            // SAFETY: `F_GETFD` only reads `descriptor`'s flags, and fails harmlessly when it is closed.
            unsafe { libc::fcntl(*descriptor, libc::F_GETFD) >= 0 }
        });
        report_held_pipes(open);
        println!("HELD_PIPE_SCAN_DONE");
    }
}
