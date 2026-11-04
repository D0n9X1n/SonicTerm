//! Pins the Windows connection checks: only a server running as the caller's SID is trusted and a
//! failed lookup is refused; only a local pipe name is ever opened; a name with no server pipe is
//! not live; and connecting to our own pipe sends nothing before the server speaks.

use std::io;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    CreateNamedPipeW, PeekNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};

use super::*;

/// The same SID is trusted; another SID, or a failed lookup, is not.
#[test]
fn only_our_sid_is_trusted() {
    assert!(trust_server(Ok("S-1-5-21-1".to_owned()), "S-1-5-21-1"));
    assert!(!trust_server(Ok("S-1-5-21-2".to_owned()), "S-1-5-21-1"));
    assert!(!trust_server(Ok("S-1-5-18".to_owned()), "S-1-5-21-1"), "SYSTEM is another user");
    assert!(!trust_server(Err(io::Error::other("no token")), "S-1-5-21-1"));
}

/// Only `\\.\pipe\` names are local; a UNC or a file path is refused before anything is opened.
#[test]
fn only_a_local_pipe_is_opened() {
    assert!(is_local_pipe(r"\\.\pipe\sonicterm-1"));
    for remote in [r"\\server\pipe\sonicterm-1", r"C:\pipe", "sonicterm-1"] {
        assert!(!is_local_pipe(remote), "{remote}");
        let refused = connect(remote).err().map(|error| error.kind());
        assert_eq!(refused, Some(io::ErrorKind::InvalidInput), "{remote}");
    }
}

/// A unique pipe name for this test.
fn pipe_name(label: &str) -> String {
    format!(r"\\.\pipe\sonicterm-mcp-test-{label}-{}", std::process::id())
}

/// Our own server pipe with one instance, owned by the returned handle.
fn serve(name: &str) -> OwnedHandle {
    let wide = HSTRING::from(name);
    let handle =
        // SAFETY: `wide` is a NUL-terminated name alive for the call; the handle is owned below.
        unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            4096,
            4096,
            0,
            None,
        )
    };
    assert!(!handle.is_invalid(), "the test pipe could not be created");
    OwnedHandle(handle)
}

/// A pipe name with a server is live; one with none is not.
#[test]
fn only_a_pipe_with_a_server_is_live() {
    let name = pipe_name("live");
    let _server = serve(&name);
    assert!(listening(&name));
    assert!(!listening(&pipe_name("absent")));
}

/// Connecting to a pipe this user serves passes the SID check, and the client writes nothing
/// before the server speaks.
#[test]
fn our_own_server_is_trusted_and_nothing_is_sent_first() {
    let name = pipe_name("own");
    let server = serve(&name);
    let halves = connect(&name).expect("our own pipe is trusted");
    let mut available = 0u32;
    // SAFETY: `server` is a live pipe handle and `available` is writable; nothing is read.
    unsafe { PeekNamedPipe(server.0, None, 0, None, Some(&mut available), None) }.unwrap();
    assert_eq!(available, 0, "connect sends nothing");
    drop(halves);
}
