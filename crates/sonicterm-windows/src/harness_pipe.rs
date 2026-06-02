//! Test-automation named-pipe input harness (issue #506).
//!
//! Compiled in only under `--features harness`. The release build MUST
//! NOT carry these symbols — `scripts/check-no-harness-in-release.ps1`
//! enforces this.
//!
//! ## Design (per Step-2 APPROVED-DIAG #4599909370)
//!
//! - Owner-only ACL via SDDL `O:OWG:OWD:P(A;;GA;;;OW)`.
//! - `PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE`, max
//!   instances 1, byte stream, blocking.
//! - Accept loop on a dedicated std thread (no tokio).
//! - On connect: audit-log the client pid, then drain 4 KiB chunks via
//!   `ReadFile` and forward raw bytes to the **active main-window
//!   pane**.
//! - On EOF/disconnect: `DisconnectNamedPipe` and loop back to
//!   `ConnectNamedPipe`.
//!
//! ## Active-pane integration (status)
//!
//! The pipe server publishes bytes through a shared slot:
//!
//! ```text
//! pub type HarnessSink = Arc<Mutex<Option<Sender<Vec<u8>>>>>;
//! ```
//!
//! Whoever owns the App is expected to update that slot whenever the
//! active pane changes (the sender is `PtyHandle::in_tx`). The shell
//! wiring that swaps the sender on focus change lives in
//! `crates/sonicterm-app/src/app/mod.rs` and is a HOT_FILE
//! (`docs/HOT_FILES.md`) — that cross-crate change is tracked as a
//! follow-up so this PR can ship the pipe scaffolding and the symbol
//! gate without touching shared state. The default behaviour when the
//! slot is empty is to drop the chunk and log at trace level, which
//! still exercises the secure-pipe path end-to-end and lets the
//! random-bytes test verify the read loop is robust.

#![cfg(all(target_os = "windows", feature = "harness"))]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use crossbeam_channel::Sender;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_BROKEN_PIPE, HANDLE, HLOCAL,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{
    ReadFile, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};

/// Owner-only SDDL: owner SID = current owner, group = current owner,
/// DACL protected (no inheritance), single ACE granting `GA`
/// (`GENERIC_ALL`) to the owner. Nothing else can connect.
const PIPE_SDDL: &str = "O:OWG:OWD:P(A;;GA;;;OW)";

const PIPE_PREFIX: &str = "\\\\.\\pipe\\sonicterm-harness-";
const READ_CHUNK: usize = 4096;

/// Shared sink the App is expected to keep pointing at the active
/// pane's `PtyHandle::in_tx`. `None` means "no pane to inject into
/// right now" — the read loop drops the chunk and logs at trace.
pub type HarnessSink = Arc<Mutex<Option<Sender<Vec<u8>>>>>;

/// Build a fresh, empty sink. Cheap, no syscalls.
pub fn new_sink() -> HarnessSink {
    Arc::new(Mutex::new(None))
}

/// Resolve the user-supplied `--harness-input-pipe <name>` to the full
/// `\\.\pipe\sonicterm-harness-<stem>` form. `"auto"` generates a
/// reasonably-unique stem (time-ns ⊕ pid ⊕ counter — we don't pull a
/// real UUID crate just for this test seam).
pub fn resolve_pipe_name(req: &str) -> String {
    let stem = if req == "auto" { generate_stem() } else { req.to_string() };
    format!("{PIPE_PREFIX}{stem}")
}

fn generate_stem() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let pid = std::process::id() as u128;
    let c = COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
    format!("{:032x}", t ^ (pid << 64) ^ (c << 96))
}

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Build the owner-only `SECURITY_ATTRIBUTES`. The returned buffer
/// owns the descriptor allocation; keep it alive for the lifetime of
/// the pipe handle. Caller frees with `LocalFree` (we do so on drop).
struct PipeSd {
    sd: PSECURITY_DESCRIPTOR,
}
// SAFETY: PSECURITY_DESCRIPTOR is just a `*mut c_void` to a Win32-owned
// blob. We never dereference it across threads (only Win32 APIs do),
// and the drop runs on whichever thread owns the box.
unsafe impl Send for PipeSd {}

/// Send-wrapped Win32 HANDLE so the dedicated accept thread can own
/// it. We never share the handle across threads concurrently; the
/// main thread hands it off and never touches it again.
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}
impl Drop for PipeSd {
    fn drop(&mut self) {
        if !self.sd.0.is_null() {
            // SAFETY: descriptor was allocated by Win32 via
            // ConvertStringSecurityDescriptorToSecurityDescriptorW,
            // which documents LocalFree as the matching deallocator.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.sd.0)));
            }
        }
    }
}

fn build_security_descriptor() -> Result<PipeSd> {
    let wide = to_wide(PIPE_SDDL);
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: wide is null-terminated, sd is a valid out pointer.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .context("ConvertStringSecurityDescriptorToSecurityDescriptorW")?;
    }
    Ok(PipeSd { sd })
}

/// Spawn the accept loop on a dedicated std thread. Returns
/// immediately with the resolved pipe name (already announced to
/// stdout once). The thread runs for the lifetime of the process.
pub fn spawn(request: &str, sink: HarnessSink) -> Result<String> {
    let pipe_name = resolve_pipe_name(request);
    // Per spec — single stdout line, fixed format.
    println!("harness pipe ready: {pipe_name}");
    tracing::info!(pipe = %pipe_name, "harness pipe ready");

    let name_wide = to_wide(&pipe_name);
    let name_for_thread = pipe_name.clone();
    let sd = build_security_descriptor()?;

    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.sd.0,
        bInheritHandle: false.into(),
    };

    // SAFETY: name is null-terminated; SECURITY_ATTRIBUTES is valid for
    // this call. Output buffer/instances follow the Step-2 spec.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name_wide.as_ptr()),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,                 // max instances
            0,                 // outbound buffer (unused)
            READ_CHUNK as u32, // inbound buffer
            0,                 // default timeout
            Some(&sa as *const SECURITY_ATTRIBUTES),
        )
    };
    if handle.is_invalid() {
        // SAFETY: GetLastError is a thread-local kernel-managed flag.
        let err = unsafe { GetLastError() };
        bail!("CreateNamedPipeW failed: {:?}", err);
    }

    // SECURITY_ATTRIBUTES is `Copy` (POD); it's been read into the
    // Win32 call above and we don't need to keep the binding around.
    let _ = sa;
    let send_handle = SendHandle(handle);
    thread::Builder::new()
        .name("sonic-harness-pipe".to_string())
        .spawn(move || {
            let _sd_keep_alive = sd;
            let handle = send_handle;
            accept_loop(handle.0, name_for_thread, sink);
        })
        .context("spawn harness pipe thread")?;
    Ok(pipe_name)
}

fn accept_loop(handle: HANDLE, pipe_name: String, sink: HarnessSink) {
    loop {
        // SAFETY: handle is owned by this thread; ConnectNamedPipe
        // blocks until a client connects.
        let connected = unsafe { ConnectNamedPipe(handle, None) };
        if let Err(e) = connected {
            // ERROR_PIPE_CONNECTED (535) means a client raced us —
            // still a valid connection.
            let code = e.code().0 as u32 & 0xFFFF;
            if code != 535 {
                tracing::warn!(error = ?e, "ConnectNamedPipe failed; loop will retry");
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
        }

        let mut client_pid: u32 = 0;
        // SAFETY: handle is connected; out pointer valid.
        let _ = unsafe { GetNamedPipeClientProcessId(handle, &mut client_pid) };
        tracing::info!(pipe = %pipe_name, pid = client_pid, "harness pipe connected");

        drain_until_eof(handle, &sink);

        // SAFETY: handle is connected.
        let _ = unsafe { DisconnectNamedPipe(handle) };
        tracing::info!(pipe = %pipe_name, "harness pipe client disconnected");
    }
    // Unreachable in practice; if accept_loop returns, close handle.
    // SAFETY: handle is owned by this thread.
    #[allow(unreachable_code)]
    unsafe {
        let _ = CloseHandle(handle);
    }
}

fn drain_until_eof(handle: HANDLE, sink: &HarnessSink) {
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        let mut read: u32 = 0;
        // SAFETY: buf is valid for `READ_CHUNK` bytes; read is a valid out param.
        let res = unsafe { ReadFile(handle, Some(buf.as_mut_slice()), Some(&mut read), None) };
        match res {
            Ok(()) => {
                if read == 0 {
                    return;
                }
                let chunk = buf[..read as usize].to_vec();
                match sink.lock() {
                    Ok(g) => match g.as_ref() {
                        Some(tx) => {
                            if let Err(e) = tx.send(chunk) {
                                tracing::warn!(error = %e, "harness sink send failed");
                            }
                        }
                        None => {
                            tracing::trace!(
                                bytes = read,
                                "harness pipe chunk dropped: no active pane sink"
                            );
                        }
                    },
                    Err(_) => tracing::warn!("harness sink mutex poisoned"),
                }
            }
            Err(e) => {
                // Broken pipe == client closed → just return.
                let code = e.code().0 as u32 & 0xFFFF;
                if code != ERROR_BROKEN_PIPE.0 {
                    tracing::warn!(error = ?e, "ReadFile on harness pipe failed");
                }
                return;
            }
        }
    }
}

/// Replace the active-pane sender. Intended for the App glue to call
/// whenever focus moves to a new pane (follow-up PR — see module
/// doc). Lock-poisoning is silently ignored: a poisoned mutex here
/// just means the previous publisher panicked, which is already going
/// to take the process down via the panic hook.
#[allow(dead_code)] // wired up by follow-up active-pane PR; see module doc
pub fn publish_active_sender(sink: &HarnessSink, tx: Option<Sender<Vec<u8>>>) {
    if let Ok(mut g) = sink.lock() {
        *g = tx;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_auto_to_unique_pipe_name() {
        let a = resolve_pipe_name("auto");
        let b = resolve_pipe_name("auto");
        assert!(a.starts_with(PIPE_PREFIX));
        assert!(b.starts_with(PIPE_PREFIX));
        assert_ne!(a, b, "auto-generated stems must be unique");
    }

    #[test]
    fn resolves_explicit_stem() {
        assert_eq!(resolve_pipe_name("my-test"), format!("{PIPE_PREFIX}my-test"),);
    }

    #[test]
    fn new_sink_starts_empty() {
        let s = new_sink();
        assert!(s.lock().unwrap().is_none());
    }

    #[test]
    fn publish_round_trips() {
        let s = new_sink();
        let (tx, rx) = crossbeam_channel::unbounded();
        publish_active_sender(&s, Some(tx));
        s.lock().unwrap().as_ref().unwrap().send(b"hi".to_vec()).unwrap();
        assert_eq!(rx.recv().unwrap(), b"hi");
        publish_active_sender(&s, None);
        assert!(s.lock().unwrap().is_none());
    }
}
