//! Identity-framed detached-console child ownership and protocol. regression contracts.

use super::*;

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
