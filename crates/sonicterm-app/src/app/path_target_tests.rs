use super::*;
use sonicterm_cfg::url_scan::DetectedTarget;
use sonicterm_grid::grid::{Cell, CellFlags, Color, Row};

fn ascii_row(text: &str) -> Row {
    Row::from_flat(
        text.chars()
            .map(|ch| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty()))
            .collect(),
    )
}

/// ASCII paths retain typed provenance and exact cell bounds.
#[test]
fn row_lookup_returns_path_candidate_with_cell_span() {
    let row = ascii_row("see ./file now");
    let found = target_at_row_cell(&row, 7, PathStyle::Posix).expect("path under column");

    assert_eq!(found.start_col, 4);
    assert_eq!(found.end_col, 10);
    assert_eq!(found.matched.target, DetectedTarget::PathCandidate("./file".into()));
}

/// A wide continuation is token content, not whitespace that can expose a suffix path.
#[test]
fn wide_cell_rejects_the_entire_surrounding_path_token() {
    let mut cells = "/tmp/"
        .chars()
        .map(|ch| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty()))
        .collect::<Vec<_>>();
    cells.push(Cell::plain('界', Color::Default, Color::Default, CellFlags::WIDE));
    cells.push(Cell::plain(' ', Color::Default, Color::Default, CellFlags::WIDE_CONT));
    cells.extend(
        "/file"
            .chars()
            .map(|ch| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty())),
    );
    let row = Row::from_flat(cells);

    assert!(target_at_row_cell(&row, 1, PathStyle::Posix).is_none());
    assert!(target_at_row_cell(&row, 8, PathStyle::Posix).is_none());
}

/// Combining extras reject a whole token instead of revealing an ASCII fragment.
#[test]
fn combining_extras_reject_the_entire_surrounding_path_token() {
    let mut cells = "./cafe/file"
        .chars()
        .map(|ch| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty()))
        .collect::<Vec<_>>();
    cells[5].set_extras(Some("\u{301}".into()));
    let row = Row::from_flat(cells);

    assert!(target_at_row_cell(&row, 2, PathStyle::Posix).is_none());
    assert!(target_at_row_cell(&row, 8, PathStyle::Posix).is_none());
}

/// POSIX relative paths resolve only from an accepted local OSC 7 snapshot.
#[test]
fn posix_relative_resolution_is_host_aware_and_root_clamped() {
    let cwd = Osc7Cwd { authority: "localhost".into(), path: "/work/project".into() };
    assert_eq!(
        resolve_path_candidate("../../file", PathStyle::Posix, Some(&cwd), "my-host"),
        Some(PathBuf::from("/file"))
    );

    let foreign = Osc7Cwd { authority: "remote-host".into(), path: "/work/project".into() };
    assert_eq!(resolve_path_candidate("./file", PathStyle::Posix, Some(&foreign), "my-host"), None);
    assert_eq!(resolve_path_candidate("./file", PathStyle::Posix, None, "my-host"), None);
}

/// The exact local hostname is accepted case-insensitively without weakening foreign-host checks.
#[test]
fn local_hostname_authority_is_accepted() {
    let cwd = Osc7Cwd { authority: "MY-HOST".into(), path: "/work".into() };
    assert_eq!(
        resolve_path_candidate("./file", PathStyle::Posix, Some(&cwd), "my-host"),
        Some(PathBuf::from("/work/file"))
    );
}

/// Windows OSC 7 drive paths normalize before dot-relative resolution.
#[test]
fn windows_relative_resolution_normalizes_drive_form() {
    let cwd = Osc7Cwd { authority: String::new(), path: "/C:/work/project".into() };
    assert_eq!(
        resolve_path_candidate(r"..\..\file", PathStyle::Windows, Some(&cwd), "host"),
        Some(PathBuf::from(r"C:\file"))
    );
    assert_eq!(
        resolve_path_candidate("C:/Users/dotan/", PathStyle::Windows, None, "host"),
        Some(PathBuf::from(r"C:\Users\dotan"))
    );
}

/// Probe epochs wrap rather than saturate so identity can never stick at u64::MAX.
#[test]
fn probe_epoch_advances_and_wraps() {
    assert_eq!(ProbeEpoch::INITIAL.next(), ProbeEpoch(1));
    assert_eq!(ProbeEpoch(u64::MAX).next(), ProbeEpoch::INITIAL);
}

/// macOS reveal passes one normalized path argument after the reveal flag.
#[test]
fn macos_reveal_spec_is_absolute_and_reveal_only() {
    let spec = macos_reveal_spec("/tmp/file/").expect("valid absolute path");
    assert_eq!(spec.program, PathBuf::from("/usr/bin/open"));
    assert_eq!(spec.args, ["-R", "/tmp/file"]);
    assert_eq!(macos_reveal_spec("relative/file"), None);
}

/// Explorer comes from the trusted shared Windows directory and receives one select argument.
#[test]
fn windows_reveal_spec_uses_one_trusted_select_argument() {
    let spec = windows_reveal_spec(r"C:\Windows", "C:/Users/dotan/")
        .expect("valid Windows directory and target");
    assert_eq!(spec.program, PathBuf::from(r"C:\Windows\explorer.exe"));
    assert_eq!(spec.args, [r"/select,C:\Users\dotan"]);

    let root = windows_reveal_spec("C:", r"C:\file").expect("root install normalizes");
    assert_eq!(root.program, PathBuf::from(r"C:\explorer.exe"));
    assert_eq!(windows_reveal_spec("Windows", r"C:\file"), None);
    assert_eq!(windows_reveal_spec(r"\\server\Windows", r"C:\file"), None);
}

/// Windows directory lookup retries a required-size response and validates termination.
#[test]
fn windows_directory_query_retries_and_normalizes_root() {
    let mut calls = 0;
    let directory = query_system_windows_directory(|buffer| {
        calls += 1;
        if calls == 1 {
            return 300;
        }
        assert_eq!(buffer.len(), 301);
        let encoded = "C:".encode_utf16().collect::<Vec<_>>();
        buffer[..encoded.len()].copy_from_slice(&encoded);
        buffer[encoded.len()] = 0;
        encoded.len() as u32
    })
    .expect("valid root directory");

    assert_eq!(calls, 2);
    assert_eq!(directory, r"C:\");
}

/// Invalid API outputs fail closed before an executable path is constructed.
#[test]
fn windows_directory_query_rejects_invalid_results() {
    assert!(query_system_windows_directory(|_| 0).is_err());
    assert!(query_system_windows_directory(|buffer| {
        let encoded = "Windows".encode_utf16().collect::<Vec<_>>();
        buffer[..encoded.len()].copy_from_slice(&encoded);
        buffer[encoded.len()] = 0;
        encoded.len() as u32
    })
    .is_err());
    assert!(query_system_windows_directory(|buffer| {
        let encoded = r"\\server\Windows".encode_utf16().collect::<Vec<_>>();
        buffer[..encoded.len()].copy_from_slice(&encoded);
        buffer[encoded.len()] = 0;
        encoded.len() as u32
    })
    .is_err());
    assert!(query_system_windows_directory(|buffer| {
        buffer[0] = 0xd800;
        buffer[1] = 0;
        1
    })
    .is_err());
    assert!(query_system_windows_directory(|buffer| {
        let encoded = r"C:\Windows".encode_utf16().collect::<Vec<_>>();
        buffer[..encoded.len()].copy_from_slice(&encoded);
        buffer[encoded.len()] = 1;
        encoded.len() as u32
    })
    .is_err());
}

/// The Linux reveal adapter receives an already-opened file identity, not its pathname.
#[test]
fn reveal_target_adapter_owns_an_open_file() {
    use std::io::Write;

    let path = std::env::temp_dir().join(format!(
        "sonicterm-path-reveal-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let mut created = std::fs::File::create(&path).unwrap();
    created.write_all(b"identity").unwrap();
    drop(created);

    let length = with_opened_reveal_target(&path, |file| Ok(file.metadata()?.len())).unwrap();
    assert_eq!(length, 8);
    std::fs::remove_file(&path).unwrap();
    assert!(with_opened_reveal_target(&path, |_| Ok(())).is_err());
}

fn probe_key(path: &str, view_top: u64) -> PathProbeKey {
    PathProbeKey {
        window_id: winit::window::WindowId::dummy(),
        pane_id: 7,
        viewport_row: 2,
        absolute_row: view_top + 2,
        view_top,
        start_col: 4,
        end_col: 10,
        candidate: "./file".into(),
        resolved_path: PathBuf::from(path),
        cwd: Some(Osc7Cwd { authority: String::new(), path: "/work".into() }),
        cwd_revision: 3,
        content_seq: 11,
        scrollback_evicted: 0,
        alt_screen: false,
    }
}

/// A result authorizes a click only when epoch, key, and live modifier all match.
#[test]
fn probe_state_requires_current_epoch_key_and_modifier() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let request = state.request(key.clone()).expect("new candidate schedules a probe");
    let result = PathProbeResult { request, exists: true };

    assert!(state.accept(&result, Some(&key)));
    assert!(!state.authorized(&key, false), "modifier release must deactivate authorization");
    assert!(state.authorized(&key, true));
    assert_eq!(state.decision_for(&key), Some(true));
}

/// A transient inability to re-read the pointer target must allow the same path to be probed again.
#[test]
fn unavailable_fresh_target_does_not_wedge_probe_state() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let request = state.request(key.clone()).expect("first request");

    assert!(!state.accept(&PathProbeResult { request, exists: true }, None));
    assert!(state.request(key).is_some(), "the same target must be eligible for a retry");
}

/// Delayed results from a prior same-value incarnation cannot survive an ABA transition.
#[test]
fn probe_epoch_rejects_same_key_after_leave_and_reenter() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let first = state.request(key.clone()).expect("first request");
    state.invalidate();
    let second = state.request(key.clone()).expect("re-enter schedules a new epoch");

    let delayed_positive = PathProbeResult { request: first, exists: true };
    assert!(!state.accept(&delayed_positive, Some(&key)));
    assert!(!state.authorized(&key, true));

    let current_positive = PathProbeResult { request: second, exists: true };
    assert!(state.accept(&current_positive, Some(&key)));
    assert!(state.authorized(&key, true));
}

/// A viewport round trip with the same visible value still gets a distinct epoch.
#[test]
fn probe_epoch_rejects_viewport_round_trip_results() {
    let mut state = PathProbeState::default();
    let key = probe_key("/work/file", 20);
    let old = state.request(key.clone()).expect("initial request");
    state.invalidate();
    let away = probe_key("/work/file", 30);
    state.request(away).expect("scroll-away request");
    state.invalidate();
    let current = state.request(key.clone()).expect("scroll-back request");

    assert!(!state.accept(&PathProbeResult { request: old, exists: false }, Some(&key)));
    assert_eq!(state.decision_for(&key), None);
    assert!(state.accept(&PathProbeResult { request: current, exists: true }, Some(&key)));
}

/// Filesystem probe results authorize an existing entry and revoke the same entry once missing.
#[test]
fn filesystem_existence_controls_path_authorization() {
    let path = std::env::temp_dir().join(format!(
        "sonicterm-path-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::write(&path, b"present").unwrap();

    let mut state = PathProbeState::default();
    let key = probe_key(path.to_str().unwrap(), 20);
    let existing = PathProbeResult {
        request: state.request(key.clone()).unwrap(),
        exists: path_exists(&path),
    };
    assert!(state.accept(&existing, Some(&key)));
    assert!(state.authorized(&key, true));

    std::fs::remove_file(&path).unwrap();
    state.invalidate();
    let missing = PathProbeResult {
        request: state.request(key.clone()).unwrap(),
        exists: path_exists(&path),
    };
    assert!(state.accept(&missing, Some(&key)));
    assert!(!state.authorized(&key, true));
}

/// The latest-request mailbox retains only the newest request while one wake is pending.
#[test]
fn probe_mailbox_coalesces_to_the_latest_request() {
    let (mailbox, wakes) = PathProbeMailbox::new();
    let mut state = PathProbeState::default();
    let first = state.request(probe_key("/work/first", 20)).unwrap();
    state.invalidate();
    let latest = state.request(probe_key("/work/latest", 20)).unwrap();

    mailbox.submit(first).unwrap();
    mailbox.submit(latest.clone()).unwrap();
    assert_eq!(wakes.len(), 1, "only one worker wake is queued");
    wakes.recv().unwrap();
    assert_eq!(mailbox.take_latest(), Some(latest));
    assert_eq!(mailbox.take_latest(), None);
}
