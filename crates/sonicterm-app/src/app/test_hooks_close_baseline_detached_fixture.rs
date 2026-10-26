//! Identity-framed detached-console child ownership and protocol.

use super::*;

#[cfg(windows)]
pub(super) fn detached_frame_matches(
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
pub(super) const DETACHED_ROOT_TEST: &str = "app::close_baseline_tests::test_hooks_close_baseline_detached_fixture::test_hooks_close_baseline_detached_fixture_tests::detached_root_fixture";
#[cfg(windows)]
pub(super) const DETACHED_CHILD_TEST: &str = "app::close_baseline_tests::test_hooks_close_baseline_detached_fixture::test_hooks_close_baseline_detached_fixture_tests::detached_child_fixture";
#[cfg(windows)]
pub(super) const DETACHED_FRAME_LIMIT: usize = 4096;
#[cfg(windows)]
pub(super) const DETACHED_LIFETIME_MS: u64 = 45_000;

#[cfg(windows)]
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct DetachedConfig {
    address: std::net::SocketAddr,
    nonce: String,
    setup_end: u64,
    lifetime_end: u64,
    root_pid: u32,
}

#[cfg(windows)]
pub(super) fn detached_tick() -> u64 {
    // SAFETY: GetTickCount64 reads the machine-wide monotonic clock without changing native state.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}

#[cfg(windows)]
pub(super) fn detached_remaining(end: u64) -> Result<Duration> {
    let now = detached_tick();
    ensure!(now < end, "detached fixture absolute deadline expired");
    Ok(Duration::from_millis(end - now))
}

#[cfg(windows)]
pub(super) fn detached_args(name: &str, config: &DetachedConfig) -> Result<Vec<String>> {
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
pub(super) fn detached_config() -> Result<DetachedConfig> {
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
pub(super) fn detached_identity(pid: u32) -> Result<(u32, u64, u32)> {
    let inspection = ProcessInspection::open(pid)?;
    let entry = process_snapshot()?
        .into_iter()
        .find(|entry| entry.pid == pid)
        .context("fixture process absent from identity snapshot")?;
    Ok((pid, inspection.created, entry.parent))
}

#[cfg(windows)]
pub(super) fn detached_frame(
    config: &DetachedConfig,
    identity: (u32, u64, u32),
    kind: &str,
) -> serde_json::Value {
    serde_json::json!({"nonce": config.nonce, "pid": identity.0, "created": identity.1,
        "parent": identity.2, "kind": kind})
}

#[cfg(windows)]
pub(super) fn detached_send(
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
pub(super) fn detached_read_exact(
    stream: &mut std::net::TcpStream,
    bytes: &mut [u8],
    end: u64,
) -> Result<()> {
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
pub(super) fn detached_receive(
    stream: &mut std::net::TcpStream,
    end: u64,
) -> Result<serde_json::Value> {
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
pub(super) fn detached_accept(
    listener: &std::net::TcpListener,
    end: u64,
) -> Result<std::net::TcpStream> {
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
pub(super) fn detached_expect(
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
pub(super) struct DetachedChildGuard {
    child: std::process::Child,
    handed_off: bool,
    cleanup_attempted: bool,
}

#[cfg(windows)]
impl DetachedChildGuard {
    pub(super) fn cleanup(&mut self) -> Result<()> {
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

#[cfg(windows)]
pub(super) fn run_detached_preservation() -> Result<()> {
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

#[cfg(test)]
#[path = "test_hooks_close_baseline_detached_fixture_tests.rs"]
mod test_hooks_close_baseline_detached_fixture_tests;
