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

/// Reserved retirement settles an output-ready attached child before fixture cleanup can mask an orphan.
#[test]
fn real_pty_reaper_settles_owned_processes_before_fixture_cleanup() {
    if pty_test_support::isolated() {
        return;
    }
    #[cfg(windows)]
    let ready_output = ReadyOutput::new();
    #[cfg(windows)]
    let output_path = Some(ready_output.path.as_path());
    #[cfg(not(windows))]
    let output_path = None;
    let (mut app, pane, processes, slave) =
        prepare_pane_with_ready_output("ordinary", output_path).unwrap();
    assert!(slave.is_none());
    let start = Instant::now();
    assert!(app.close_pty_pane(pane));
    let observations = observe_settlement(&processes, start).unwrap();
    let settled = app.finish_session();
    let native_settled = observations.iter().all(|observation| observation.elapsed.is_some());
    for (process, observation) in processes.iter().zip(&observations) {
        println!(
            "PTY_REAPER_PROCESS role={} pid={} identity={} state={}",
            process.role,
            process.pid,
            process.identity(),
            observation.state
        );
    }
    cleanup_processes(&processes).unwrap();
    assert_eq!(app.pty_reaper.path_counts(), (1, 0));
    assert!(settled, "reserved native teardown did not settle");
    assert!(native_settled, "an owned process survived reaper cleanup before fixture intervention");
    pty_test_support::record_process(processes[0].pid, false);
}

/// Console transitions and survival replies must name the retained child and arrive before the shared cutoff.
#[cfg(windows)]
#[test]
fn detached_frame_requires_complete_identity_matched_proof() {
    use serde_json::json;
    let identity = (7, 100, 11);
    let before = json!({
        "nonce": "fixture", "pid": 7, "created": 100, "parent": 11,
        "kind": "before", "console_pids": [7, 11]
    });
    let after = json!({
        "nonce": "fixture", "pid": 7, "created": 100, "parent": 11,
        "kind": "after", "freed": true, "window_null": true,
        "census_count": 0, "census_error": 6
    });
    let alive = json!({
        "nonce": "fixture", "pid": 7, "created": 100, "parent": 11,
        "kind": "alive", "challenge": "new-challenge"
    });
    let accepts = |frame: &serde_json::Value, kind, now| {
        detached_frame_matches(frame, "fixture", identity, kind, "new-challenge", 200, now)
    };
    for (frame, kind) in [(&before, "before"), (&after, "after"), (&alive, "alive")] {
        for (key, value) in [
            ("nonce", json!("another-fixture")),
            ("pid", json!(8)),
            ("created", json!(101)),
            ("parent", json!(12)),
            ("kind", json!("other")),
        ] {
            let mut mismatched = frame.clone();
            mismatched[key] = value;
            assert!(!accepts(&mismatched, kind, 199), "accepted mismatched {key}");
        }
        let extra: &[&str] = match kind {
            "before" => &["console_pids"],
            "after" => &["freed", "window_null"],
            _ => &["challenge"],
        };
        for key in ["nonce", "pid", "created", "parent", "kind"].iter().chain(extra) {
            let mut missing = frame.clone();
            missing.as_object_mut().unwrap().remove(*key);
            assert!(!accepts(&missing, kind, 199), "accepted missing {key}");
        }
        for key in ["pid", "created", "parent"] {
            for value in [json!(null), json!("7"), json!(-1), json!(7.5), json!(true)] {
                let mut mistyped = frame.clone();
                mistyped[key] = value;
                assert!(!accepts(&mistyped, kind, 199), "accepted mistyped {key}");
            }
        }
        assert!(!accepts(frame, kind, 200), "accepted a reply at its deadline");
        assert!(!accepts(frame, kind, 201), "accepted a late reply");
    }
    for members in [json!([]), json!([7]), json!([11]), json!(null)] {
        let mut incomplete = before.clone();
        incomplete["console_pids"] = members;
        assert!(!accepts(&incomplete, "before", 199), "accepted incomplete prior attachment");
    }
    for field in ["freed", "window_null"] {
        for value in [json!(false), json!(null), json!(1)] {
            let mut incomplete = after.clone();
            incomplete[field] = value;
            assert!(!accepts(&incomplete, "after", 199), "accepted incomplete detachment proof");
        }
    }
    let mut stale = alive.clone();
    stale["challenge"] = json!("setup-challenge");
    assert!(!accepts(&stale, "alive", 199), "accepted a stale survival reply");
    assert!(accepts(&before, "before", 199), "valid attached-before frame was rejected");
    assert!(accepts(&after, "after", 199), "valid detached-after frame was rejected");
    assert!(accepts(&alive, "alive", 199), "valid fresh survival reply was rejected");
    let mut supplemental = after.clone();
    supplemental["census_count"] = json!(1);
    supplemental["census_error"] = json!(0);
    assert!(accepts(&supplemental, "after", 199), "supplemental census became detachment proof");
}

#[cfg(windows)]
fn detached_frame_matches(
    frame: &serde_json::Value,
    nonce: &str,
    identity: (u32, u64, u32),
    kind: &str,
    challenge: &str,
    cutoff: u64,
    now: u64,
) -> bool {
    if now >= cutoff
        || frame["nonce"].as_str() != Some(nonce)
        || frame["pid"].as_u64() != Some(u64::from(identity.0))
        || frame["created"].as_u64() != Some(identity.1)
        || frame["parent"].as_u64() != Some(u64::from(identity.2))
        || frame["kind"].as_str() != Some(kind)
    {
        return false;
    }
    match kind {
        "before" => frame["console_pids"].as_array().is_some_and(|members| {
            members.iter().all(|pid| pid.as_u64().is_some_and(|pid| u32::try_from(pid).is_ok()))
                && members.iter().any(|pid| pid.as_u64() == Some(u64::from(identity.0)))
                && members.iter().any(|pid| pid.as_u64() == Some(u64::from(identity.2)))
        }),
        "after" => {
            frame["freed"].as_bool() == Some(true) && frame["window_null"].as_bool() == Some(true)
        }
        "challenge" | "alive" => frame["challenge"].as_str() == Some(challenge),
        "root" | "spawn" | "spawned" | "handoff" | "handed_off" => true,
        "cleanup" => frame["child_cleaned"].as_bool() == Some(true),
        _ => false,
    }
}

#[cfg(windows)]
const DETACHED_ROOT_TEST: &str = "app::close_baseline_tests::detached_root_fixture";
#[cfg(windows)]
const DETACHED_CHILD_TEST: &str = "app::close_baseline_tests::detached_child_fixture";
#[cfg(windows)]
const DETACHED_FRAME_LIMIT: usize = 4096;
#[cfg(windows)]
const DETACHED_LIFETIME_MS: u64 = 45_000;

#[cfg(windows)]
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct DetachedConfig {
    address: std::net::SocketAddr,
    nonce: String,
    setup_end: u64,
    lifetime_end: u64,
    root_pid: u32,
}

#[cfg(windows)]
fn detached_tick() -> u64 {
    // SAFETY: GetTickCount64 reads the machine-wide monotonic clock without changing native state.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

#[cfg(windows)]
fn detached_remaining(end: u64) -> Result<Duration> {
    let now = detached_tick();
    ensure!(now < end, "detached fixture absolute deadline expired");
    Ok(Duration::from_millis(end - now))
}

#[cfg(windows)]
fn detached_args(name: &str, config: &DetachedConfig) -> Result<Vec<String>> {
    use base64::Engine;
    let encoded =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(config)?);
    Ok(vec![
        "--exact".into(),
        name.into(),
        "--ignored".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
        "--skip".into(),
        format!("DETACHED_FIXTURE={encoded}"),
    ])
}

#[cfg(windows)]
fn detached_config() -> Result<DetachedConfig> {
    use base64::Engine;
    let mut args = std::env::args();
    let mut encoded = None;
    while let Some(arg) = args.next() {
        if arg == "--skip" {
            if let Some(value) = args
                .next()
                .and_then(|value| value.strip_prefix("DETACHED_FIXTURE=").map(str::to_owned))
            {
                ensure!(
                    encoded.replace(value).is_none(),
                    "duplicate detached fixture configuration"
                );
            }
        }
    }
    let encoded = encoded.context("helper requires explicit detached fixture configuration")?;
    ensure!(encoded.len() <= DETACHED_FRAME_LIMIT, "detached fixture configuration too large");
    let config: DetachedConfig =
        serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded)?)?;
    let now = detached_tick();
    ensure!(
        config.address.ip().is_loopback() && config.address.port() != 0,
        "nonprivate control address"
    );
    ensure!(
        !config.nonce.is_empty() && config.nonce.len() <= 128,
        "invalid detached fixture nonce"
    );
    ensure!(
        now < config.setup_end
            && config.setup_end < config.lifetime_end
            && config.lifetime_end - now <= DETACHED_LIFETIME_MS,
        "invalid detached fixture deadlines"
    );
    Ok(config)
}

#[cfg(windows)]
fn detached_identity(pid: u32) -> Result<(u32, u64, u32)> {
    let inspection = ProcessInspection::open(pid)?;
    let entry = process_snapshot()?
        .into_iter()
        .find(|entry| entry.pid == pid)
        .context("fixture process absent from identity snapshot")?;
    Ok((pid, inspection.created, entry.parent))
}

#[cfg(windows)]
fn detached_frame(
    config: &DetachedConfig,
    identity: (u32, u64, u32),
    kind: &str,
) -> serde_json::Value {
    serde_json::json!({"nonce": config.nonce, "pid": identity.0, "created": identity.1,
        "parent": identity.2, "kind": kind})
}

#[cfg(windows)]
fn detached_send(
    stream: &mut std::net::TcpStream,
    frame: &serde_json::Value,
    end: u64,
) -> Result<()> {
    use std::io::Write;
    let bytes = serde_json::to_vec(frame)?;
    ensure!(bytes.len() <= DETACHED_FRAME_LIMIT, "detached control frame too large");
    let data = [&(bytes.len() as u32).to_be_bytes()[..], &bytes].concat();
    let mut written = 0;
    while written < data.len() {
        stream.set_write_timeout(Some(detached_remaining(end)?))?;
        let count = stream.write(&data[written..])?;
        ensure!(count != 0, "detached control connection closed during write");
        written += count;
    }
    detached_remaining(end)?;
    Ok(())
}

#[cfg(windows)]
fn detached_read_exact(stream: &mut std::net::TcpStream, bytes: &mut [u8], end: u64) -> Result<()> {
    use std::io::Read;
    let mut read = 0;
    while read < bytes.len() {
        stream.set_read_timeout(Some(detached_remaining(end)?))?;
        let count = stream.read(&mut bytes[read..])?;
        ensure!(count != 0, "detached control connection closed during read");
        read += count;
    }
    detached_remaining(end)?;
    Ok(())
}

#[cfg(windows)]
fn detached_receive(stream: &mut std::net::TcpStream, end: u64) -> Result<serde_json::Value> {
    let mut length = [0; 4];
    detached_read_exact(stream, &mut length, end)?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(length != 0 && length <= DETACHED_FRAME_LIMIT, "invalid detached control frame length");
    let mut bytes = vec![0; length];
    detached_read_exact(stream, &mut bytes, end)?;
    let frame = serde_json::from_slice(&bytes)?;
    detached_remaining(end)?;
    Ok(frame)
}

#[cfg(windows)]
fn detached_accept(listener: &std::net::TcpListener, end: u64) -> Result<std::net::TcpStream> {
    loop {
        let remaining = detached_remaining(end)?;
        match listener.accept() {
            Ok((stream, peer)) => {
                ensure!(peer.ip().is_loopback(), "nonlocal detached fixture peer");
                // Accepted sockets must not inherit a nonblocking listener's behavior on Windows.
                stream.set_nonblocking(false)?;
                detached_remaining(end)?;
                return Ok(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(OBSERVE_INTERVAL.min(remaining));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(windows)]
fn detached_expect(
    stream: &mut std::net::TcpStream,
    config: &DetachedConfig,
    identity: (u32, u64, u32),
    kind: &str,
    challenge: &str,
    end: u64,
) -> Result<serde_json::Value> {
    let frame = detached_receive(stream, end)?;
    ensure!(
        detached_frame_matches(
            &frame,
            &config.nonce,
            identity,
            kind,
            challenge,
            end,
            detached_tick()
        ),
        "invalid or late detached {kind} frame"
    );
    Ok(frame)
}

// The root owns its original Child until main confirms retained-handle admission; failure cleanup never reopens a PID.
#[cfg(windows)]
struct DetachedChildGuard {
    child: std::process::Child,
    handed_off: bool,
    cleanup_attempted: bool,
}

#[cfg(windows)]
impl DetachedChildGuard {
    fn cleanup(&mut self) -> Result<()> {
        self.cleanup_attempted = true;
        if self.handed_off || self.child.try_wait()?.is_some() {
            return Ok(());
        }
        let kill = self.child.kill();
        if self.child.try_wait()?.is_none() {
            kill.context("terminate untransferred detached child")?;
        }
        let end = detached_tick() + CLEANUP_LIMIT.as_millis() as u64;
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(OBSERVE_INTERVAL.min(detached_remaining(end)?));
        }
    }
}

// Lifecycle: panic/setup failures retain direct Child cleanup until acknowledged admission transfers custody.
#[cfg(windows)]
impl Drop for DetachedChildGuard {
    fn drop(&mut self) {
        if !self.cleanup_attempted {
            if let Err(error) = self.cleanup() {
                eprintln!("detached child direct cleanup failed: {error:#}");
            }
        }
    }
}

/// The root remains attached and alive after handing its child to main; only pane close may end the normal path.
#[cfg(windows)]
#[test]
#[ignore = "requires explicit private detached fixture configuration"]
fn detached_root_fixture() -> Result<()> {
    use std::net::TcpStream;
    use std::process::{Command, Stdio};
    let mut config = detached_config()?;
    ensure!(config.root_pid == 0, "root fixture received descendant configuration");
    let root = detached_identity(std::process::id())?;
    config.root_pid = root.0;
    let mut stream =
        TcpStream::connect_timeout(&config.address, detached_remaining(config.setup_end)?)?;
    detached_send(&mut stream, &detached_frame(&config, root, "root"), config.setup_end)?;
    detached_expect(&mut stream, &config, root, "spawn", "", config.setup_end)?;
    let child = Command::new(std::env::current_exe()?)
        .args(detached_args(DETACHED_CHILD_TEST, &config)?)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut guard = DetachedChildGuard { child, handed_off: false, cleanup_attempted: false };
    let result = (|| -> Result<()> {
        let child = detached_identity(guard.child.id())?;
        ensure!(child.2 == root.0 && child.1 >= root.1, "spawned child identity mismatch");
        detached_send(&mut stream, &detached_frame(&config, child, "spawned"), config.setup_end)?;
        detached_expect(&mut stream, &config, child, "handoff", "", config.setup_end)?;
        ensure!(guard.child.try_wait()?.is_none(), "detached child exited before handoff");
        guard.handed_off = true;
        // Main will not close until this receipt proves the root disabled direct child cleanup.
        detached_send(
            &mut stream,
            &detached_frame(&config, child, "handed_off"),
            config.setup_end,
        )?;
        while let Ok(remaining) = detached_remaining(config.lifetime_end) {
            thread::sleep(OBSERVE_INTERVAL.min(remaining));
        }
        anyhow::bail!("attached root reached fixture expiry without pane closure")
    })();
    let cleanup = guard.cleanup();
    if !guard.handed_off {
        let mut receipt = detached_frame(&config, root, "cleanup");
        receipt["child_cleaned"] = serde_json::json!(cleanup.is_ok());
        detached_send(&mut stream, &receipt, config.setup_end + CLEANUP_LIMIT.as_millis() as u64)?;
    }
    cleanup?;
    result
}

/// The child proves prior attachment, detaches, then answers only a new identity-bound post-close challenge.
#[cfg(windows)]
#[test]
#[ignore = "requires explicit private detached fixture configuration"]
fn detached_child_fixture() -> Result<()> {
    use windows::Win32::System::Console::{FreeConsole, GetConsoleProcessList, GetConsoleWindow};
    let config = detached_config()?;
    let identity = detached_identity(std::process::id())?;
    ensure!(
        config.root_pid != 0 && identity.2 == config.root_pid,
        "descendant root identity mismatch"
    );
    let mut stream = std::net::TcpStream::connect_timeout(
        &config.address,
        detached_remaining(config.setup_end)?,
    )?;
    let mut members = [0u32; 128];
    let count =
        // SAFETY: members is writable storage and this queries only the caller's attached console.
        unsafe { GetConsoleProcessList(&mut members) } as usize;
    ensure!(
        count != 0 && count <= members.len(),
        "attached console census failed or exceeded bound"
    );
    let mut before = detached_frame(&config, identity, "before");
    before["console_pids"] = serde_json::json!(&members[..count]);
    ensure!(
        detached_frame_matches(
            &before,
            &config.nonce,
            identity,
            "before",
            "",
            config.setup_end,
            detached_tick()
        ),
        "child and root were not jointly attached"
    );
    detached_send(&mut stream, &before, config.setup_end)?;
    // SAFETY: only this fixture process releases its own console attachment; no other process is modified.
    unsafe { FreeConsole()? };
    let window_null =
        // SAFETY: GetConsoleWindow observes only this process's current console attachment.
        unsafe { GetConsoleWindow() }.0.is_null();
    ensure!(window_null, "detached child still has a console window");
    let count =
        // SAFETY: members remains writable; this post-detachment census is supplemental diagnostic evidence only.
        unsafe { GetConsoleProcessList(&mut members) };
    let error = if count == 0 { io::Error::last_os_error().raw_os_error() } else { None };
    let mut after = detached_frame(&config, identity, "after");
    after["freed"] = serde_json::json!(true);
    after["window_null"] = serde_json::json!(window_null);
    after["census_count"] = serde_json::json!(count);
    after["census_error"] = serde_json::json!(error);
    detached_send(&mut stream, &after, config.setup_end)?;
    let challenge = detached_receive(&mut stream, config.lifetime_end)?;
    let token = challenge["challenge"].as_str().context("missing survival challenge")?;
    ensure!(!token.is_empty() && token != config.nonce, "invalid survival challenge");
    ensure!(
        detached_frame_matches(
            &challenge,
            &config.nonce,
            identity,
            "challenge",
            token,
            config.lifetime_end,
            detached_tick()
        ),
        "invalid post-close challenge identity"
    );
    let mut alive = detached_frame(&config, identity, "alive");
    alive["challenge"] = serde_json::json!(token);
    detached_send(&mut stream, &alive, config.lifetime_end)?;
    while let Ok(remaining) = detached_remaining(config.lifetime_end) {
        thread::sleep(OBSERVE_INTERVAL.min(remaining));
    }
    anyhow::bail!("detached child reached fixture expiry without owned cleanup")
}

/// Pane close must settle its retained root and conhost while a proven detached descendant remains responsive.
#[cfg(windows)]
#[test]
fn detached_process_survives_pane_close() {
    if pty_test_support::isolated() {
        return;
    }
    run_detached_preservation().expect("detached-process preservation fixture");
}

#[cfg(windows)]
fn run_detached_preservation() -> Result<()> {
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let before: BTreeSet<_> = process_snapshot()?.into_iter().map(|entry| entry.pid).collect();
    let app_inspection = ProcessInspection::open(std::process::id())?;
    let origin = fixture_birth_boundary();
    let start = detached_tick();
    let config = DetachedConfig {
        address: listener.local_addr()?,
        nonce: format!("{}-{origin}-{}", std::process::id(), listener.local_addr()?.port()),
        setup_end: start + SETUP_LIMIT.as_millis() as u64,
        lifetime_end: start + DETACHED_LIFETIME_MS,
        root_pid: 0,
    };
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default())
        .with_capture_staging_pool(CaptureStagingPool::new())
        .with_inline_media_pool(media::InlineMediaPool::new());
    let pane = app.__test_seed_tab("detached descendant preservation");
    let executable = std::env::current_exe()?;
    let pty = PtyHandle::spawn_with_args(
        executable.to_str().context("Unicode test executable path")?,
        &detached_args(DETACHED_ROOT_TEST, &config)?,
        80,
        24,
    )?;
    let root_pid = pty.pid().context("detached fixture root PID")?;
    pty_test_support::record_process(root_pid, true);
    let mut processes = vec![NativeProcess::open(root_pid, "shell")?];
    ensure!(app.__test_set_pane_pty(pane, Some(pty)), "install detached fixture PTY");
    app.reconcile_pane_owners();
    let mut root_connection = None;
    let mut spawn_requested = false;
    let mut handoff_confirmed = false;
    let outcome = (|| -> Result<()> {
        let pane_state =
            app.main().and_then(|window| window.panes.get(&pane)).context("fixture pane")?;
        ensure!(pane_state.reap_slot.is_some(), "fixture pane lacks reserved teardown capacity");
        // INHERIT_CURSOR requires a startup response; queuing it does not claim native consumption.
        pane_state
            .pty
            .as_ref()
            .context("fixture PTY")?
            .send_input_nonblocking(b"\x1b[1;1R".to_vec())
            .map_err(|error| anyhow::anyhow!("queue initial cursor response: {error:?}"))?;
        let root = (root_pid, processes[0].created, std::process::id());
        root_connection = Some(detached_accept(&listener, config.setup_end)?);
        let root_stream = root_connection.as_mut().unwrap();
        detached_expect(root_stream, &config, root, "root", "", config.setup_end)?;
        spawn_requested = true;
        detached_send(root_stream, &detached_frame(&config, root, "spawn"), config.setup_end)?;
        let spawned = detached_receive(root_stream, config.setup_end)?;
        let child_pid =
            u32::try_from(spawned["pid"].as_u64().context("missing spawned child PID")?)?;
        let child_birth = spawned["created"].as_u64().context("missing spawned child birth")?;
        let child = (child_pid, child_birth, root_pid);
        ensure!(
            detached_frame_matches(
                &spawned,
                &config.nonce,
                child,
                "spawned",
                "",
                config.setup_end,
                detached_tick()
            ),
            "invalid spawned-child frame"
        );
        let candidate = process_snapshot()?
            .into_iter()
            .find(|entry| entry.pid == child_pid)
            .context("spawned child missing from snapshot")?;
        let inspection = ProcessInspection::open(child_pid)?;
        ensure!(inspection.created == child_birth, "spawned child creation time changed");
        let expected_name =
            executable.file_name().context("test executable name")?.to_string_lossy();
        ensure!(
            candidate.name.eq_ignore_ascii_case(&expected_name),
            "spawned child image mismatch"
        );
        processes.push(
            admit_process_candidate(
                &candidate,
                inspection,
                root_pid,
                root.1,
                &before,
                "descendant",
            )?
            .context("spawned descendant admission refused")?,
        );
        let mut child_stream = detached_accept(&listener, config.setup_end)?;
        let attached =
            detached_expect(&mut child_stream, &config, child, "before", "", config.setup_end)?;
        println!("DETACHED_PRESERVATION before={attached}");
        let after =
            detached_expect(&mut child_stream, &config, child, "after", "", config.setup_end)?;
        println!("DETACHED_PRESERVATION after={after}");
        loop {
            detached_remaining(config.setup_end)?;
            let hosts: Vec<_> = process_snapshot()?
                .into_iter()
                .filter(|entry| {
                    entry.parent == app_inspection.pid
                        && !before.contains(&entry.pid)
                        && (entry.name.eq_ignore_ascii_case("conhost.exe")
                            || entry.name.eq_ignore_ascii_case("OpenConsole.exe"))
                })
                .collect();
            ensure!(hosts.len() <= 1, "ambiguous new console host candidates");
            if let Some(host) = hosts.first() {
                if let Some(owned) = admit_process_candidate(
                    host,
                    ProcessInspection::open(host.pid)?,
                    app_inspection.pid,
                    origin.max(app_inspection.created),
                    &before,
                    "conhost",
                )? {
                    processes.push(owned);
                    break;
                }
            }
            thread::sleep(OBSERVE_INTERVAL.min(detached_remaining(config.setup_end)?));
        }
        detached_send(root_stream, &detached_frame(&config, child, "handoff"), config.setup_end)?;
        detached_expect(root_stream, &config, child, "handed_off", "", config.setup_end)?;
        handoff_confirmed = true;
        ensure!(
            processes
                .iter()
                .map(NativeProcess::running)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .all(|running| running),
            "a fixture process exited before pane close"
        );
        let pty = app
            .main()
            .and_then(|window| window.panes.get(&pane))
            .and_then(|pane| pane.pty.as_ref())
            .context("fixture PTY before close")?;
        for _ in 0..pty.out_rx.len() {
            let _ = pty.out_rx.try_recv();
        }
        detached_remaining(config.setup_end)?;
        // Root and conhost become a contiguous observation slice without releasing the descendant's retained handle.
        processes.swap(1, 2);
        let close_start = Instant::now();
        ensure!(app.close_pty_pane(pane), "detached fixture pane close refused");
        let observations = observe_settlement(&processes[..2], close_start)?;
        let finished = app.finish_session();
        for (process, observation) in processes[..2].iter().zip(&observations) {
            println!(
                "DETACHED_PRESERVATION role={} pid={} created={} state={} settled={}",
                process.role,
                process.pid,
                process.created,
                observation.state,
                observation.elapsed.is_some()
            );
        }
        ensure!(
            observations.iter().all(|observation| observation.elapsed.is_some()),
            "retained root/conhost did not settle before cleanup"
        );
        ensure!(finished, "detached fixture native teardown did not settle");
        ensure!(
            app.pty_reaper.path_counts() == (1, 0),
            "detached fixture bypassed reserved teardown"
        );
        let survival_end =
            (detached_tick() + SETUP_LIMIT.as_millis() as u64).min(config.lifetime_end);
        let challenge = format!("{}-post-close-{}", config.nonce, detached_tick());
        let mut request = detached_frame(&config, child, "challenge");
        request["challenge"] = serde_json::json!(challenge);
        let survival = (|| -> Result<()> {
            detached_remaining(survival_end)?;
            ensure!(processes[2].running()?, "retained detached child is signaled");
            detached_send(&mut child_stream, &request, survival_end)?;
            detached_expect(&mut child_stream, &config, child, "alive", &challenge, survival_end)?;
            ensure!(processes[2].running()?, "retained detached child exited after reply");
            detached_remaining(survival_end)?;
            Ok(())
        })();
        survival.context("detached child did not survive pane close")?;
        println!("DETACHED_PRESERVATION child_pid={} child_created={} fresh_reply=true retained_running=true", child.0, child.1);
        Ok(())
    })();
    let direct_cleanup = (|| -> Result<()> {
        if outcome.is_err()
            && spawn_requested
            && !handoff_confirmed
            && !processes.iter().any(|process| process.role == "descendant")
        {
            if let Some(stream) = root_connection.as_mut() {
                // Half-close cancels the root's ACK read without discarding its direct-child cleanup receipt.
                stream.shutdown(std::net::Shutdown::Write)?;
                let end = config.setup_end + CLEANUP_LIMIT.as_millis() as u64;
                loop {
                    let frame = detached_receive(stream, end)?;
                    if frame["kind"].as_str() == Some("cleanup") {
                        ensure!(
                            detached_frame_matches(
                                &frame,
                                &config.nonce,
                                (root_pid, processes[0].created, std::process::id()),
                                "cleanup",
                                "",
                                end,
                                detached_tick()
                            ),
                            "root did not verify direct-child cleanup"
                        );
                        break;
                    }
                    ensure!(
                        frame["kind"].as_str() == Some("spawned")
                            && frame["nonce"].as_str() == Some(config.nonce.as_str()),
                        "unexpected frame while awaiting direct-child cleanup"
                    );
                }
            }
        }
        Ok(())
    })();
    // Fixture intervention occurs only after observations; failures still attempt cleanup of every admitted identity.
    let cleanup = cleanup_processes(&processes);
    let finished = app.finish_session();
    let root_settled = processes[0].settled();
    if matches!(&root_settled, Ok(true)) {
        pty_test_support::record_process(root_pid, false);
    }
    // Preserve the failed observation while reporting cleanup independently; secondary failures cannot replace its cause.
    if let Err(error) = outcome {
        eprintln!("DETACHED_PRESERVATION outcome={error:#} cleanup={cleanup:?} direct_cleanup={direct_cleanup:?} finish_session={finished} root_settled={root_settled:?}");
        return Err(error);
    }
    cleanup.context("detached preservation fixture cleanup failed")?;
    direct_cleanup.context("detached root direct-child cleanup unverified")?;
    ensure!(
        root_settled.context("detached root final settlement query failed")?,
        "detached root remained running after cleanup"
    );
    ensure!(finished, "detached fixture final teardown did not settle");
    Ok(())
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
    let (reaper_closes, fallback_closes) = app.pty_reaper.path_counts();
    ensure!(
        (reaper_closes, fallback_closes) == (1, 0),
        "baseline requires one reserved reaper close, observed reaper={reaper_closes} fallback={fallback_closes}"
    );
    println!(
        "PTY_CLOSE_BASELINE paths scenario={scenario} sample={sample} reaper={reaper_closes} fallback={fallback_closes}"
    );
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
            "PTY_CLOSE_BASELINE sample scenario={scenario} measure={measure} sample={sample} path=reaper panes=1 status={} elapsed_ms={}",
            if value.censored { "exceeded_limit" } else { "observed" }, value.value()
        );
    }
    // Cleanup starts after both measurements: releasing the slave or killing survivors earlier would alter settlement.
    drop(slave);
    let teardown_settled = app.finish_session();
    println!(
        "PTY_CLOSE_BASELINE cleanup scenario={scenario} sample={sample} teardown_settled={teardown_settled}"
    );
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
    prepare_pane_with_ready_output(scenario, None)
}

#[cfg(windows)]
struct ReadyOutput {
    path: std::path::PathBuf,
}

#[cfg(windows)]
impl ReadyOutput {
    fn new() -> Self {
        let stamp =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir()
            .join(format!("sonicterm-child-ready-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&directory).expect("create private child readiness directory");
        Self { path: directory.join("ready.txt") }
    }
}

// Lifecycle: ReadyOutput removes only its private marker and directory after the fixture releases its child.
#[cfg(windows)]
impl Drop for ReadyOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(self.path.parent().unwrap());
    }
}

fn prepare_pane_with_ready_output(
    scenario: &str,
    ready_output: Option<&std::path::Path>,
) -> Result<(App, u64, Vec<NativeProcess>, Option<std::fs::File>)> {
    #[cfg(not(windows))]
    let _ = ready_output;
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
        // Baseline callers keep NUL and creation-only admission; only the settled-client test requests output readiness.
        let target = ready_output
            .map_or_else(|| "NUL".to_string(), |path| format!("\"{}\"", path.display()));
        pty.send_input_nonblocking(
            format!("\x1b[1;1Rstart \"\" /B ping.exe -t 127.0.0.1 >{target}\r\n").into_bytes(),
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
            let ready = ready_output.is_none_or(|path| {
                std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0)
            });
            if owned_host_count == 1 && owned_descendant && ready {
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
    app.reconcile_pane_owners();
    ensure!(
        app.main()
            .and_then(|window| window.panes.get(&pane))
            .is_some_and(|pane| pane.reap_slot.is_some()),
        "baseline pane did not reserve native teardown capacity"
    );
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
