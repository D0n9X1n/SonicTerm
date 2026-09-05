use super::*;
use std::io::{Cursor, Error, ErrorKind};

/// Help names the shipping binary and makes the secure default discoverable.
#[test]
fn usage_names_actual_binary_and_optional_socket() {
    assert_eq!(usage(), "sonic-mux <daemon|list|kill <pane_id>> [--socket <path>]");
    assert_eq!(extract_explicit_socket(&[]).unwrap(), None);
    assert_eq!(
        extract_explicit_socket(&["--socket".into(), "custom.sock".into()]).unwrap(),
        Some("custom.sock".into())
    );
}

/// Linux audit absence degrades to one stable user-scoped fallback identity.
#[test]
fn linux_missing_or_unassigned_audit_session_uses_stable_fallback() {
    assert_eq!(
        linux_login_session_id_from(Err(std::io::Error::from(std::io::ErrorKind::NotFound)))
            .unwrap(),
        0
    );
    assert_eq!(linux_login_session_id_from(Ok(format!("{}\n", u32::MAX))).unwrap(), 0);
    assert_eq!(linux_login_session_id_from(Ok("41\n".into())).unwrap(), 41);
}

/// Windows endpoint names and DACLs retain canonical user and session identity.
#[test]
fn windows_endpoint_contract_uses_the_current_sid() {
    let bytes = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 33, 2, 0, 0];
    let sid = sid_string(&bytes).unwrap();

    assert_eq!(sid, "S-1-5-32-545");
    assert_eq!(windows_endpoint_name(&sid, 17), "sonicterm-mux-S_1_5_32_545-17");
    assert_eq!(current_user_pipe_sddl(&sid), "D:P(A;;GA;;;S-1-5-32-545)");
    assert!(sid_string(&bytes[..12]).is_err());

    const SOURCE: &str = include_str!("main.rs");
    assert!(SOURCE.contains(".security_descriptor(descriptor)"));
}

#[cfg(windows)]
/// Windows accepts the protected current-user descriptor before listener creation.
#[test]
fn windows_current_user_descriptor_is_valid() {
    current_user_security_descriptor("S-1-5-32-545").unwrap();
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(label: &str) -> Self {
            let sequence = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("sonicterm-mux-{label}-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn owner(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().uid()
    }

    /// macOS can briefly accept connects on an abandoned listener, so wait for the stale precondition.
    fn wait_until_socket_is_stale(path: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match std::os::unix::net::UnixStream::connect(path) {
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => return,
                Ok(stream) => drop(stream),
                Err(error) => panic!("unexpected stale-socket probe error: {error}"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "listener teardown did not reach ConnectionRefused"
            );
            std::thread::yield_now();
        }
    }

    /// macOS login identity remains stable when a client starts in another POSIX session.
    #[cfg(target_os = "macos")]
    #[test]
    fn login_session_identity_survives_a_new_posix_session() {
        const CHILD: &str = "SONICTERM_MUX_LOGIN_SESSION_CHILD";
        if std::env::var_os(CHILD).is_some() {
            println!("login-session={}", unix_login_session_id().unwrap());
            return;
        }

        use std::os::unix::process::CommandExt;
        assert_eq!(std::mem::size_of::<AuditInfoAddress>(), 48);
        let parent = unix_login_session_id().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("main_tests::unix::login_session_identity_survives_a_new_posix_session")
            .arg("--nocapture")
            .env(CHILD, "1");
        // SAFETY: the child calls only async-signal-safe `setsid` before immediately executing the test binary.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let child = stdout
            .lines()
            .find_map(|line| line.strip_prefix("login-session="))
            .unwrap()
            .parse::<u32>()
            .unwrap();

        assert_eq!(child, parent);
    }

    /// A trusted XDG runtime directory wins, while an exposed one falls back safely.
    #[test]
    fn xdg_runtime_directory_must_be_private() {
        let scratch = ScratchDir::new("xdg");
        let runtime = scratch.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let uid = owner(&runtime);

        let socket = resolve_unix_default_socket_with(Some(&runtime), scratch.path(), uid, || {
            panic!("trusted XDG path must not query login identity")
        })
        .unwrap();
        assert_eq!(socket, runtime.join("sonicterm-mux.sock"));

        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
        let fallback =
            resolve_unix_default_socket(Some(&runtime), scratch.path(), uid, 41).unwrap();
        assert_eq!(fallback, fallback_unix_socket_path(scratch.path(), uid, 41));
    }

    /// Fallback paths vary by identity without creating directories for clients.
    #[test]
    fn fallback_namespace_is_side_effect_free_and_identity_scoped() {
        let scratch = ScratchDir::new("fallback");
        let current_uid = owner(scratch.path());
        let first = resolve_unix_default_socket(None, scratch.path(), current_uid, 41).unwrap();
        let other_session =
            resolve_unix_default_socket(None, scratch.path(), current_uid, 42).unwrap();
        let other_user = fallback_unix_socket_path(scratch.path(), current_uid + 1, 41);

        assert_ne!(first, other_session);
        assert_ne!(first, other_user);
        assert!(!first.parent().unwrap().exists());

        ensure_unix_runtime_directory(&first, current_uid).unwrap();
        assert_eq!(
            fs::metadata(first.parent().unwrap()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    /// Listener creation applies user-only access through the pre-bind mode API.
    #[test]
    fn unix_listener_is_created_with_user_only_mode() {
        let scratch = ScratchDir::new("mode");
        let socket = scratch.path().join("mux.sock");

        let listener = bind_unix_listener(&socket).unwrap();

        assert_eq!(fs::metadata(&socket).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(include_str!("main.rs").contains("mode(UNIX_SOCKET_MODE)"));
        drop(listener);
    }

    /// A second daemon refuses the live listener without replacing its filesystem identity.
    #[test]
    fn live_endpoint_collision_preserves_the_first_listener() {
        let scratch = ScratchDir::new("live");
        let socket = scratch.path().join("mux.sock");
        let first = bind_unix_listener(&socket).unwrap();
        let before = fs::symlink_metadata(&socket).unwrap();

        let error = bind_unix_listener(&socket).unwrap_err();
        let after = fs::symlink_metadata(&socket).unwrap();

        assert!(error.to_string().contains("live"), "unexpected error: {error:#}");
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        drop(first);
    }

    /// Non-socket, foreign-owned, and exposed-parent endpoints remain untouched.
    #[test]
    fn unsafe_existing_endpoints_are_preserved() {
        let scratch = ScratchDir::new("unsafe");
        let regular = scratch.path().join("regular");
        fs::write(&regular, b"keep").unwrap();
        assert!(bind_unix_listener(&regular).is_err());
        assert_eq!(fs::read(&regular).unwrap(), b"keep");

        let socket = scratch.path().join("foreign.sock");
        let stale = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        drop(stale);
        let actual_uid = owner(&socket);
        assert!(prepare_unix_endpoint(&socket, actual_uid.wrapping_add(1)).is_err());
        assert!(socket.exists());

        let exposed = scratch.path().join("exposed");
        fs::create_dir(&exposed).unwrap();
        fs::set_permissions(&exposed, fs::Permissions::from_mode(0o755)).unwrap();
        let exposed_socket = exposed.join("mux.sock");
        let stale = std::os::unix::net::UnixListener::bind(&exposed_socket).unwrap();
        drop(stale);
        assert!(prepare_unix_endpoint(&exposed_socket, actual_uid).is_err());
        assert!(exposed_socket.exists());
    }

    /// Darwin refuses a new endpoint when only a public parent could protect the bind window.
    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_fallback_refuses_an_exposed_parent() {
        let scratch = ScratchDir::new("darwin-public");
        fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let socket = scratch.path().join("mux.sock");

        let error = bind_unix_listener(&socket).unwrap_err();

        assert!(error.to_string().contains("owned 0700 directory"), "unexpected error: {error:#}");
        assert!(!socket.exists());
    }

    /// A stale socket owned by the current user is removed and rebound.
    #[test]
    fn stale_owned_socket_is_recovered() {
        let scratch = ScratchDir::new("stale");
        let socket = scratch.path().join("mux.sock");
        let stale = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        drop(stale);
        wait_until_socket_is_stale(&socket);

        let listener = bind_unix_listener(&socket).unwrap();
        let after = fs::symlink_metadata(&socket).unwrap();

        assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
        assert!(after.file_type().is_socket());
        assert_eq!(after.permissions().mode() & 0o777, 0o600);
        drop(listener);
    }
}

fn framed(message: &ServerMsg) -> Cursor<Vec<u8>> {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, message).unwrap();
    Cursor::new(bytes)
}

struct TimeoutReader;

impl Read for TimeoutReader {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Err(Error::new(ErrorKind::TimedOut, "test response deadline"))
    }
}

struct TimeoutTrackingReader {
    inner: Cursor<Vec<u8>>,
    timeout: Option<Duration>,
}

impl Read for TimeoutTrackingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.timeout.is_none() {
            return Err(Error::new(ErrorKind::NotConnected, "response deadline is not armed"));
        }
        self.inner.read(buffer)
    }
}

impl KillResponseReader for TimeoutTrackingReader {
    fn set_kill_response_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.timeout = Some(timeout);
        Ok(())
    }
}

#[test]
fn matching_killed_response_is_the_only_success() {
    // Protect success output from being emitted before the requested pane identity is acknowledged.
    let result = read_kill_response(&mut framed(&ServerMsg::Killed { pane_id: 7 }), 7).unwrap();

    assert_eq!(result, "killed pane 7");
}

#[test]
fn kill_exchange_writes_the_requested_kill_frame() {
    // Protect the extracted transport seam from changing MAX_FRAME-compatible request framing.
    let mut response = Vec::new();
    write_frame(&mut response, &ServerMsg::Killed { pane_id: 7 }).unwrap();
    let mut reader = TimeoutTrackingReader { inner: Cursor::new(response), timeout: None };
    let mut written = Vec::new();

    let result = exchange_kill(&mut reader, &mut written, 7).unwrap();

    assert_eq!(result, "killed pane 7");
    assert_eq!(reader.timeout, Some(KILL_RESPONSE_TIMEOUT));
    let request: ClientMsg = read_frame(&mut Cursor::new(written)).unwrap();
    assert!(matches!(request, ClientMsg::Kill { pane_id: 7 }));
}

#[test]
fn server_error_fails_with_kill_context() {
    // Protect daemon-side failures from being reported as a successful kill by the CLI.
    let error =
        read_kill_response(&mut framed(&ServerMsg::Error("unknown pane 7".into())), 7).unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("kill pane 7"));
    assert!(message.contains("unknown pane 7"));
}

#[test]
fn mismatched_killed_response_fails_with_both_pane_ids() {
    // Protect clients from accepting a valid acknowledgement for a different concurrent request.
    let error = read_kill_response(&mut framed(&ServerMsg::Killed { pane_id: 8 }), 7).unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("pane 7"));
    assert!(message.contains("pane 8"));
}

#[test]
fn unexpected_response_fails_with_frame_context() {
    // Protect protocol drift from being silently interpreted as a successful kill.
    let error = read_kill_response(&mut framed(&ServerMsg::Sessions(Vec::new())), 7).unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("kill pane 7"));
    assert!(message.contains("unexpected response"));
    assert!(message.contains("Sessions"));
}

#[test]
fn eof_and_malformed_response_fail_with_read_context() {
    // Protect truncated and invalid frames from losing the operation identity in CLI diagnostics.
    let eof = read_kill_response(&mut Cursor::new(Vec::new()), 7).unwrap_err();
    assert!(format!("{eof:#}").contains("kill pane 7: response read failed"));

    let malformed = read_kill_response(&mut Cursor::new(vec![0, 0, 0, 1, 0xff]), 7).unwrap_err();
    assert!(format!("{malformed:#}").contains("kill pane 7: response read failed"));
}

#[test]
fn transport_timeout_fails_with_read_context() {
    // Protect the bounded transport deadline from being flattened into success or an opaque I/O error.
    let error = read_kill_response(&mut TimeoutReader, 7).unwrap_err();

    let message = format!("{error:#}");
    assert!(message.contains("kill pane 7: response read failed"));
    assert!(message.contains("test response deadline"));
}

#[test]
fn kill_exchange_arms_timeout_before_reading_acknowledgement() {
    // Protect the command path from reading a response before the transport has a finite deadline.
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &ServerMsg::Killed { pane_id: 7 }).unwrap();
    let mut reader = TimeoutTrackingReader { inner: Cursor::new(bytes), timeout: None };

    let result = exchange_kill(&mut reader, io::sink(), 7).unwrap();

    assert_eq!(result, "killed pane 7");
    assert_eq!(reader.timeout, Some(KILL_RESPONSE_TIMEOUT));
}

#[test]
fn readiness_wait_rejects_an_unarmed_deadline() {
    // Protect Windows reads from polling forever when a caller omitted the kill response deadline.
    let error = wait_until_readable(None, || Ok(0)).unwrap_err();

    assert_eq!(error.kind(), ErrorKind::NotConnected);
}

#[test]
fn readiness_wait_times_out_without_readable_bytes() {
    // Protect the polling deadline independently of either platform's transport wrapper.
    let started = Instant::now();
    let error =
        wait_until_readable(Some(started + Duration::from_millis(20)), || Ok(0)).unwrap_err();

    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn readiness_wait_returns_when_bytes_arrive() {
    // Protect the transition from polling to the one blocking read after availability becomes nonzero.
    let mut polls = 0;
    wait_until_readable(Some(Instant::now() + Duration::from_secs(1)), || {
        polls += 1;
        Ok(usize::from(polls >= 3))
    })
    .unwrap();

    assert_eq!(polls, 3);
}

#[test]
fn readiness_wait_propagates_transport_errors() {
    // Protect disconnected-pipe diagnostics from being flattened into a timeout.
    let error = wait_until_readable(Some(Instant::now() + Duration::from_secs(1)), || {
        Err(Error::new(ErrorKind::BrokenPipe, "peer closed"))
    })
    .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::BrokenPipe);
}

#[cfg(unix)]
#[test]
fn real_transport_read_timeout_bounds_a_silent_peer() {
    // Protect production Unix behavior with the kernel-backed timeout used by the concrete stream.
    use std::{os::unix::net::UnixStream, time::Instant};

    let (reader_socket, _silent_peer) = UnixStream::pair().unwrap();
    let concrete = interprocess::os::unix::uds_local_socket::Stream::from(reader_socket);
    let mut reader = Stream::UdSocket(concrete);
    reader.set_kill_response_timeout(Duration::from_millis(20)).unwrap();
    let started = Instant::now();

    let error = read_kill_response(&mut reader, 7).unwrap_err();

    assert!(started.elapsed() < Duration::from_secs(1), "read deadline was not enforced");
    assert!(matches!(
        error.downcast_ref::<io::Error>().map(io::Error::kind),
        Some(ErrorKind::TimedOut | ErrorKind::WouldBlock)
    ));
}

#[cfg(windows)]
#[test]
fn real_named_pipe_timeout_bounds_a_silent_peer() {
    // Protect the production PeekNamedPipe deadline on the overlapped transport used by Windows.
    use interprocess::local_socket::{GenericNamespaced, ListenerOptions};

    let name_text = format!("sonicterm-mux-kill-timeout-{}", std::process::id());
    let name = name_text.to_ns_name::<GenericNamespaced>().unwrap();
    let listener = ListenerOptions::new().name(name).create_sync().unwrap();
    let server = std::thread::spawn(move || {
        let _silent_peer = listener.accept().unwrap();
        std::thread::sleep(Duration::from_secs(1));
    });
    let stream = Stream::connect(name_text.to_ns_name::<GenericNamespaced>().unwrap()).unwrap();
    let mut reader = DeadlineStream { stream, deadline: None };
    reader.set_kill_response_timeout(Duration::from_millis(20)).unwrap();
    let started = Instant::now();

    let error = read_kill_response(&mut reader, 7).unwrap_err();

    assert!(started.elapsed() < Duration::from_secs(1), "read deadline was not enforced");
    assert_eq!(error.downcast_ref::<io::Error>().map(io::Error::kind), Some(ErrorKind::TimedOut));
    server.join().unwrap();
}
