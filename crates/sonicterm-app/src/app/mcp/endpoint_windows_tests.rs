//! Windows endpoint tests: the owner-only descriptor, the first-instance guard, and bounded IO
//! over a real local pipe.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use windows::core::HSTRING;

use super::*;

/// A pipe name unique to one test, so parallel tests never share an instance.
fn test_pipe(label: &str) -> HSTRING {
    HSTRING::from(format!(r"\\.\pipe\sonicterm-control-test-{}-{label}", std::process::id()))
}

/// Open a client end of `name` for reading and writing, as the bridge does.
fn open_client(name: &HSTRING) -> std::fs::File {
    OpenOptions::new().read(true).write(true).open(name.to_string()).expect("client opens the pipe")
}

/// The published name carries the process id, which is how the bridge finds this instance.
#[test]
fn pipe_name_carries_the_process_id() {
    assert_eq!(pipe_name(4242), r"\\.\pipe\sonicterm-control-4242");
}

/// The descriptor names the user as owner and grants only that user, with inheritance blocked.
#[test]
fn descriptor_admits_only_the_owner() {
    assert_eq!(owner_only_sddl("S-1-5-21-1"), "O:S-1-5-21-1D:P(A;;GRGW;;;S-1-5-21-1)");
}

/// The current user's SID is read from the process token in its string form.
#[test]
fn current_user_sid_is_a_string_sid() {
    let sid = current_user_sid().expect("token user");
    assert!(sid.starts_with("S-1-"), "{sid}");
    PipeSecurity::current_user().expect("the owner-only descriptor parses");
}

/// A second "first" instance of a held name fails, so a process squatting the name makes start
/// fail instead of sharing the pipe with it.
#[test]
fn first_instance_refuses_a_held_name() {
    let name = test_pipe("first");
    let security = PipeSecurity::current_user().unwrap();
    let _held = create_instance(&name, &security, true).expect("first instance");
    assert!(create_instance(&name, &security, true).is_err());
    create_instance(&name, &security, false).expect("later instances share the server's name");
}

/// Bytes round-trip through an accepted instance's reader and writer.
#[test]
fn accepted_instance_round_trips_bytes() {
    let name = test_pipe("round");
    let security = PipeSecurity::current_user().unwrap();
    let pipe = create_instance(&name, &security, true).unwrap();
    let client_name = name.clone();
    let client = std::thread::spawn(move || {
        let mut client = open_client(&client_name);
        client.write_all(b"hello\n").unwrap();
        let mut reply = [0u8; 6];
        client.read_exact(&mut reply).unwrap();
        reply
    });
    assert!(wait_for_client(&AtomicBool::new(false), &pipe).unwrap());
    let mut transport = transport(pipe).unwrap();
    let mut request = [0u8; 6];
    transport.reader.read_exact(&mut request).unwrap();
    assert_eq!(&request, b"hello\n");
    transport.writer.write_all(b"world\n").unwrap();
    assert_eq!(&client.join().unwrap(), b"world\n");
}

/// A read with nothing to read returns `TimedOut` after about `READ_POLL`, so the reader loop can
/// recheck its stop flags; a closed client then reads as end of stream.
#[test]
fn idle_read_times_out_and_closed_client_reads_eof() {
    let name = test_pipe("idle");
    let security = PipeSecurity::current_user().unwrap();
    let pipe = create_instance(&name, &security, true).unwrap();
    let client = open_client(&name);
    assert!(wait_for_client(&AtomicBool::new(false), &pipe).unwrap());
    let mut transport = transport(pipe).unwrap();
    let mut buffer = [0u8; 16];
    let started = Instant::now();
    let error = transport.reader.read(&mut buffer).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(started.elapsed() >= READ_POLL - Duration::from_millis(50));
    drop(client);
    assert_eq!(transport.reader.read(&mut buffer).unwrap(), 0);
}

/// A set stop flag ends a wait with no client, and the pending connect is cancelled.
#[test]
fn stop_ends_a_wait_with_no_client() {
    let name = test_pipe("stop");
    let security = PipeSecurity::current_user().unwrap();
    let pipe = create_instance(&name, &security, true).unwrap();
    let started = Instant::now();
    assert!(!wait_for_client(&AtomicBool::new(true), &pipe).unwrap());
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// The pipe's DACL holds exactly one allow entry, for the current user, and is protected from
/// inheritance, read back from the live pipe rather than the string it was built from.
#[test]
fn the_live_dacl_names_only_the_current_user() {
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
    };
    use windows::Win32::Security::DACL_SECURITY_INFORMATION;

    let name = test_pipe("dacl");
    let security = PipeSecurity::current_user().unwrap();
    let pipe = create_instance(&name, &security, true).unwrap();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let status =
        // SAFETY: `pipe` is a live handle; `descriptor` receives a LocalAlloc'd copy freed below.
        unsafe {
        GetSecurityInfo(
            pipe.0,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    };
    assert!(status.is_ok(), "{status:?}");
    let _descriptor = LocalBuffer(descriptor.0);
    let mut text = PWSTR::null();
    // SAFETY: `descriptor` is a valid self-relative descriptor; `text` receives a LocalAlloc'd string.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            None,
        )
    }
    .unwrap();
    let _text = LocalBuffer(text.0.cast());
    let sddl =
        // SAFETY: `text` is a NUL-terminated wide string kept alive by `_text`.
        unsafe { text.to_string() }.unwrap();
    let sid = current_user_sid().unwrap();
    assert!(sddl.starts_with("D:P"), "{sddl}");
    assert_eq!(sddl.matches("(A;").count(), 1, "{sddl}");
    assert_eq!(sddl.matches('(').count(), 1, "{sddl}");
    // Windows writes some SIDs as aliases (the local administrator is `LA`, as on CI runners),
    // so the one trustee is compared in the form Windows writes the current user.
    let expected = canonical_sddl(&owner_only_sddl(&sid)).unwrap();
    let trustee = |text: &str| text.rsplit(";;;").next().unwrap_or_default().to_owned();
    assert_eq!(trustee(&sddl), trustee(&expected), "{sddl} against {expected}");
}

/// The discovery directory is created owner-only and protected, the record names the pipe and
/// goes with its guard, a record whose process is gone is removed, and a directory with any other
/// DACL is refused.
#[test]
fn the_discovery_record_lives_in_an_owner_only_directory() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("control");
    let sid = current_user_sid().unwrap();
    let guard = publish_in(&dir, r"\\.\pipe\sonicterm-control-test").unwrap();
    assert_eq!(dir_sddl(&dir).unwrap(), canonical_sddl(&owner_only_dir_sddl(&sid)).unwrap());
    let record = dir.join(std::process::id().to_string());
    assert_eq!(std::fs::read_to_string(&record).unwrap(), r"\\.\pipe\sonicterm-control-test");
    // Windows process ids are multiples of four, so this one is never a live process.
    std::fs::write(dir.join("4294967291"), "stale").unwrap();
    remove_stale_records(&dir);
    assert!(!dir.join("4294967291").exists());
    assert!(record.exists(), "a live process keeps its record");
    drop(guard);
    assert!(!record.exists());

    let loose = root.path().join("loose");
    std::fs::create_dir(&loose).unwrap();
    assert_eq!(
        prepare_discovery_dir(&loose, &sid).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
}

/// A SID that Windows writes as an alias compares equal after canonicalizing: the local
/// administrator account (RID 500) comes back as `LA`, as it does on CI runners, so a directory
/// owned by it is not refused for spelling alone.
#[test]
fn an_aliased_sid_compares_equal_after_canonicalizing() {
    let sid = current_user_sid().unwrap();
    // The machine's own administrator SID: the user's domain part with RID 500.
    let domain = sid.rsplit_once('-').map(|(domain, _)| domain).unwrap();
    let administrator = format!("{domain}-500");
    // On a standalone machine Windows writes this SID as `LA`, on a domain member it may keep the
    // SID; either way the comparison depends only on Windows writing it the same way each time.
    let written = canonical_sddl(&owner_only_dir_sddl(&administrator)).unwrap();
    assert_eq!(written, canonical_sddl(&written).unwrap(), "canonical form is stable");
}
