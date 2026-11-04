//! Pins the server check: only a server running as the caller's uid is talked to, and a socket
//! held by another user is refused before any byte is sent.

use std::io::Read;
use std::os::unix::net::UnixListener;

use super::*;

/// The same uid is trusted; another uid, or a failed credential lookup, is not.
#[test]
fn only_our_uid_is_trusted() {
    assert!(trust_server(Ok(501), 501));
    assert!(!trust_server(Ok(0), 501));
    assert!(!trust_server(Err(io::Error::other("no credentials")), 501));
}

/// Connecting to our own listener passes the check and sends nothing on its own.
#[test]
fn our_own_server_is_trusted_and_nothing_is_sent_first() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("1.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (reader, writer) = connect(path.to_str().unwrap()).unwrap();
    let (mut accepted, _) = listener.accept().unwrap();
    accepted.set_nonblocking(true).unwrap();
    let mut buffer = [0u8; 1];
    assert!(accepted.read(&mut buffer).is_err(), "connect sends nothing");
    drop((reader, writer));
    assert!(process_alive(std::process::id()));
}

/// A socket with a listener is live; one whose listener closed, as a crash leaves it, is not, so
/// a reused pid cannot make a stale record look like a second instance.
#[test]
fn only_a_socket_with_a_listener_is_live() {
    let root = tempfile::tempdir().unwrap();
    let live = root.path().join("1.sock");
    let stale = root.path().join("2.sock");
    let _listener = UnixListener::bind(&live).unwrap();
    drop(UnixListener::bind(&stale).unwrap());
    assert!(listening(live.to_str().unwrap()));
    assert!(!listening(stale.to_str().unwrap()));
    assert!(!listening(root.path().join("3.sock").to_str().unwrap()));
}

/// Discovery over a directory with one live and one stale socket, both named after live pids,
/// finds exactly the live one.
#[test]
fn discovery_ignores_a_stale_socket_under_a_live_pid() {
    let root = tempfile::tempdir().unwrap();
    let pid = std::process::id();
    let live = root.path().join(format!("{pid}.sock"));
    let _listener = UnixListener::bind(&live).unwrap();
    drop(UnixListener::bind(root.path().join("1.sock")).unwrap());
    let found = crate::discovery::scan(
        root.path(),
        |path| Some(path.to_string_lossy().into_owned()),
        |_| true,
        listening,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].pid, pid);
}
