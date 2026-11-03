//! Unix endpoint tests: the directory and ancestor rules, peer admission, stale-socket cleanup,
//! and a real socket that answers `initialize` and goes away when the server stops.

use std::io::{BufReader, Write};
use std::os::unix::fs::symlink;
use std::time::Duration;

use sonicterm_cfg::config::McpControl;

use super::*;

/// A private directory with mode `mode`, inside a fresh temporary directory.
fn dir_with_mode(root: &Path, name: &str, mode: u32) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
    dir
}

/// The directory must be exactly `0700` and ours; anything looser or foreign is refused, not
/// repaired.
#[test]
fn the_directory_must_be_private_and_ours() {
    let root = tempfile::tempdir().unwrap();
    let uid = current_uid();
    assert_eq!(check_dir(&dir_with_mode(root.path(), "ok", 0o700), uid), Ok(()));
    assert_eq!(
        check_dir(&dir_with_mode(root.path(), "group", 0o750), uid),
        Err(DirRefusal::Mode(0o750))
    );
    assert_eq!(
        check_dir(&dir_with_mode(root.path(), "open", 0o777), uid),
        Err(DirRefusal::Mode(0o777))
    );
    // Another owner is simulated by checking against a uid that is not the directory's.
    assert_eq!(
        check_dir(&dir_with_mode(root.path(), "theirs", 0o700), uid + 1),
        Err(DirRefusal::Owner)
    );
}

/// A symlink to a valid directory is refused: the check opens without following links.
#[test]
fn a_symlinked_directory_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let real = dir_with_mode(root.path(), "real", 0o700);
    let link = root.path().join("link");
    symlink(&real, &link).unwrap();
    assert_eq!(check_dir(&link, current_uid()), Err(DirRefusal::NotDirectory));
}

/// The production path refuses a symlinked directory too: only the parent is resolved.
#[test]
fn prepare_dir_refuses_a_symlinked_directory() {
    let root = tempfile::tempdir().unwrap();
    let real = dir_with_mode(root.path(), "real", 0o700);
    let link = root.path().join("link");
    symlink(&real, &link).unwrap();
    let error = prepare_dir(&link, current_uid()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    assert!(prepare_dir(&real, current_uid()).is_ok());
}

/// An ancestor that other users can write without the sticky bit lets them swap the directory,
/// so it is refused; the same ancestor with the sticky bit is accepted.
#[test]
fn a_writable_ancestor_needs_the_sticky_bit() {
    let root = tempfile::tempdir().unwrap();
    let uid = current_uid();
    let shared = dir_with_mode(root.path(), "shared", 0o777);
    let inner = dir_with_mode(&shared, "inner", 0o700);
    assert!(
        matches!(check_ancestors(&inner, uid), Err(DirRefusal::AncestorWritable(path)) if path == shared)
    );
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
    // The walk continues to `/`, so only the verdict on `shared` itself is asserted.
    assert!(
        !matches!(check_ancestors(&inner, uid), Err(DirRefusal::AncestorWritable(path)) if path == shared)
    );
}

/// An ancestor owned by someone other than us or root is refused.
#[test]
fn an_ancestor_owned_by_another_user_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let inner = dir_with_mode(root.path(), "inner", 0o700);
    let uid = current_uid();
    if uid == 0 {
        // As root (CI containers), every temporary directory is root's, which the check trusts.
        // Hand the parent to a uid that is neither ours nor root's instead.
        const STRANGER: u32 = 54_321;
        let path = std::ffi::CString::new(root.path().as_os_str().as_encoded_bytes()).unwrap();
        let changed =
            // SAFETY: `path` is a NUL-terminated path alive for the call; only ownership changes.
            unsafe { libc::chown(path.as_ptr(), STRANGER, STRANGER) };
        assert_eq!(changed, 0);
        assert!(matches!(
            check_ancestors(&inner, uid),
            Err(DirRefusal::AncestorOwner(owner)) if owner == root.path()
        ));
    } else {
        // When: `uid` is not root, our own temporary directory is another user's from the point
        // of view of a different uid.
        assert!(matches!(check_ancestors(&inner, uid + 1), Err(DirRefusal::AncestorOwner(_))));
    }
}

/// Only a peer with our uid is admitted; a failed credential lookup is refused.
#[test]
fn only_our_uid_is_admitted() {
    assert!(admit_peer(Ok(501), 501));
    assert!(!admit_peer(Ok(502), 501));
    assert!(!admit_peer(Err(io::Error::other("no credentials")), 501));
}

/// The peer-credential lookup reports our own uid for a socket pair we hold both ends of.
#[test]
fn peer_uid_reads_our_own_uid() {
    let (left, _right) = UnixStream::pair().unwrap();
    assert_eq!(peer_uid(&left).unwrap(), current_uid());
}

/// A socket whose process is gone is removed; a live one and unrelated files stay.
#[test]
fn stale_sockets_are_removed() {
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join(format!("{}.sock", std::process::id()));
    // Pid numbers this high are never handed out on macOS or Linux.
    let stale = root.path().join("2147483646.sock");
    let other = root.path().join("notes.txt");
    for path in [&live, &stale, &other] {
        std::fs::write(path, b"").unwrap();
    }
    remove_stale_sockets(root.path());
    assert!(live.exists() && other.exists());
    assert!(!stale.exists());
}

/// The guard leaves a socket that was replaced since it was bound: its inode changed.
#[test]
fn the_guard_leaves_a_replaced_socket() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("1.sock");
    std::fs::write(&path, b"old").unwrap();
    let meta = std::fs::symlink_metadata(&path).unwrap();
    let guard = SocketGuard { path: path.clone(), identity: (meta.dev(), meta.ino()) };
    // Renaming keeps the old inode alive, so the replacement cannot reuse its number.
    std::fs::rename(&path, root.path().join("kept")).unwrap();
    std::fs::write(&path, b"new").unwrap();
    drop(guard);
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
}

/// A started server binds a `0600` socket in its private directory, answers `initialize`, and
/// its guard removes the socket as soon as the server is dropped.
#[test]
fn a_started_server_answers_and_cleans_up() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("sonicterm");
    let shared = McpShared::new(McpControl::Agent, None);
    let endpoint = start_in(&shared, &dir).unwrap();
    let path = PathBuf::from(&endpoint.path);
    let mode = std::fs::symlink_metadata(&path).unwrap().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o7777, 0o700);

    let mut client = UnixStream::connect(&path).unwrap();
    client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    client
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\"}}\n")
        .unwrap();
    let reply = super::super::read_reply_line(&mut BufReader::new(&client)).expect("a reply line");
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");

    // Dropping the guard removes the socket at once, before the listener thread notices the stop,
    // so a process that exits right after its server is dropped leaves nothing behind.
    shared.stop();
    drop(endpoint.guard);
    assert!(!path.exists(), "the socket outlived the server");
}
