//! OS-level cross-process tab drag — shared payload definitions.
//!
//! When the user drags a tab off a Sonic window and the cursor leaves
//! every Sonic-owned window (i.e. they're dropping it on a *different*
//! Sonic process, not just into another window of the same process —
//! that's handled by `tab_drag`), we fall through to the platform's
//! native drag-and-drop facility. This module defines the wire format
//! the source and destination processes agree on.
//!
//! ## Why this exists
//!
//! winit cross-process drag is not a thing. We have to talk to AppKit
//! (`NSPasteboard` + `NSDraggingSession`) on macOS and OLE
//! (`IDataObject` + `DoDragDrop`) on Windows. Both expect a typed blob
//! identified by a stable, reverse-DNS-style format name. That name
//! and the payload schema must match between *whichever* two Sonic
//! processes happen to be involved — pinning v1 in the type name lets
//! us evolve the schema later without ambiguity.
//!
//! ## Live-PTY transfer is not in scope for v1
//!
//! Handing a live `pty(4)` master FD across processes is technically
//! possible on macOS (Unix-domain socket `SCM_RIGHTS`) and Windows
//! (`DuplicateHandle` across PIDs) but it's a huge correctness
//! surface — the child process is still parented to the source app,
//! the source app still has the controlling TTY, signals route to the
//! wrong place. v1 takes the pragmatic route: serialize *intent*
//! (cwd, cmd, env, scrollback as history), source kills its local
//! tab, destination spawns a fresh one with the same cwd/cmd/env and
//! shows the captured scrollback as read-only history above the new
//! shell's first prompt.
//!
//! ## Format ID stability
//!
//! [`PASTEBOARD_TYPE`] is the single source of truth. Both the macOS
//! `NSPasteboard.declareTypes:owner:` call and the Windows
//! `RegisterClipboardFormatW` call use this exact string. Tests pin
//! it so a rename never silently breaks the wire.

use serde::{Deserialize, Serialize};

/// Stable identifier for the custom pasteboard / clipboard type that
/// carries a serialized [`TabPayload`].
///
/// Reverse-DNS naming follows Apple's convention for custom UTIs and
/// is also a valid Windows clipboard format name (the latter has no
/// formal naming convention but is case-sensitive and process-global).
///
/// **Do not rename without bumping the version suffix** — both the
/// source and the destination process must agree on this exact byte
/// sequence or the drag silently no-ops.
pub const PASTEBOARD_TYPE: &str = "com.sonic-terminal.tab.v1";

/// JSON payload exchanged between Sonic processes during an
/// OS-level tab drag.
///
/// Field names are kebab-friendly snake_case to match the JSON wire
/// format directly (no `#[serde(rename)]` games). The schema is
/// versioned via the type name [`PASTEBOARD_TYPE`], so adding new
/// optional fields here is forward-compatible only if older readers
/// tolerate unknown keys — which `serde_json::from_str` does by
/// default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabPayload {
    /// PID of the PTY child in the *source* process at the moment of
    /// drag. Purely informational on the destination side — used for
    /// the history banner ("torn from PID 12345") and for log
    /// correlation. The destination MUST NOT try to act on this PID;
    /// the source kills the child before releasing the drag.
    pub pty_pid: i32,
    /// Human-readable tab title (typically the shell prompt or the
    /// last `OSC 0`/`OSC 2` title the source process saw).
    pub tab_title: String,
    /// Base64-encoded UTF-8 scrollback text. Encoded so the JSON
    /// stays single-byte-clean even if the buffer contains nulls or
    /// raw escape sequences (we strip CSI before serializing, but
    /// users sometimes paste arbitrary binary).
    pub scrollback_b64: String,
    /// Current working directory of the shell at drag time. Best
    /// effort — pulled from `OSC 7` if the shell advertised it,
    /// otherwise empty.
    pub cwd: String,
    /// Argv\[0\] (and only argv\[0\]) of the shell to respawn on the
    /// destination side. We deliberately do NOT carry argv\[1..\] —
    /// re-running an interactive shell with the source's flags is
    /// usually wrong (e.g. `--login` should only fire once per
    /// session).
    pub cmd: String,
    /// Subset of environment variables the source side believes the
    /// destination should inherit (e.g. `LANG`, `TERM`, `COLORTERM`,
    /// user-set `PROMPT_COMMAND`). Stored as `(key, value)` pairs to
    /// preserve order and allow duplicates if a wild shell ever sets
    /// them.
    pub env: Vec<(String, String)>,
}

impl TabPayload {
    /// Encode the payload as a JSON string ready to drop on a
    /// pasteboard / clipboard.
    ///
    /// Returns a `Result` rather than panicking so a future change to
    /// the struct (e.g. adding a non-string-serializable field) is
    /// caught at the call site instead of crashing a UI handler.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Decode a JSON string read off the pasteboard / clipboard back
    /// into a [`TabPayload`].
    ///
    /// Tolerant of unknown trailing fields (default `serde_json`
    /// behavior) so a newer source can drop on an older destination
    /// without the destination outright rejecting the drag.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Convenience: encode arbitrary bytes (e.g. raw scrollback) as
    /// base64 suitable for [`Self::scrollback_b64`].
    ///
    /// Hand-rolled to avoid pulling in a `base64` crate just for this
    /// one use site — the algorithm is RFC 4648 standard alphabet
    /// (`A-Z a-z 0-9 + /`) with `=` padding.
    pub fn encode_scrollback(raw: &[u8]) -> String {
        const ALPH: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(raw.len().div_ceil(3) * 4);
        let mut i = 0;
        while i + 3 <= raw.len() {
            let n = ((raw[i] as u32) << 16) | ((raw[i + 1] as u32) << 8) | (raw[i + 2] as u32);
            out.push(ALPH[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPH[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPH[((n >> 6) & 0x3F) as usize] as char);
            out.push(ALPH[(n & 0x3F) as usize] as char);
            i += 3;
        }
        let rem = raw.len() - i;
        if rem == 1 {
            let n = (raw[i] as u32) << 16;
            out.push(ALPH[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPH[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        } else if rem == 2 {
            let n = ((raw[i] as u32) << 16) | ((raw[i + 1] as u32) << 8);
            out.push(ALPH[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPH[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPH[((n >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        out
    }

    /// Convenience: decode the [`Self::scrollback_b64`] field back
    /// into raw bytes. Returns `None` on malformed input rather than
    /// erroring so the destination can gracefully fall back to "no
    /// scrollback" without aborting the drop.
    pub fn decode_scrollback(b64: &str) -> Option<Vec<u8>> {
        // Lookup table for the RFC-4648 alphabet. 0xFF = invalid.
        let mut table = [0xFFu8; 256];
        for (i, &c) in
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().enumerate()
        {
            table[c as usize] = i as u8;
        }
        let bytes = b64.as_bytes();
        if !bytes.len().is_multiple_of(4) {
            return None;
        }
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
        let mut i = 0;
        while i < bytes.len() {
            let c = [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
            let v0 = table[c[0] as usize];
            let v1 = table[c[1] as usize];
            if v0 == 0xFF || v1 == 0xFF {
                return None;
            }
            let n0 = v0 as u32;
            let n1 = v1 as u32;
            if c[2] == b'=' && c[3] == b'=' {
                out.push(((n0 << 2) | (n1 >> 4)) as u8);
            } else if c[3] == b'=' {
                let v2 = table[c[2] as usize];
                if v2 == 0xFF {
                    return None;
                }
                let n2 = v2 as u32;
                out.push(((n0 << 2) | (n1 >> 4)) as u8);
                out.push((((n1 & 0xF) << 4) | (n2 >> 2)) as u8);
            } else {
                let v2 = table[c[2] as usize];
                let v3 = table[c[3] as usize];
                if v2 == 0xFF || v3 == 0xFF {
                    return None;
                }
                let n2 = v2 as u32;
                let n3 = v3 as u32;
                out.push(((n0 << 2) | (n1 >> 4)) as u8);
                out.push((((n1 & 0xF) << 4) | (n2 >> 2)) as u8);
                out.push((((n2 & 0x3) << 6) | n3) as u8);
            }
            i += 4;
        }
        Some(out)
    }
}

/// Outcome of a single OS-drag handoff attempt.
///
/// This is the load-bearing contract for the data-loss fix landed in
/// the (review) follow-up to PR #59: the source tab is *only*
/// detached/killed when the sink reports [`Accepted`](Self::Accepted).
/// Anything else — including "we wrote the payload to a pasteboard
/// but have not heard back from a receiver" — leaves the source tab
/// alive, because v1 has no cross-process consumption-ack channel
/// yet and the alternative (kill-on-write-success) destroys user
/// sessions when no second Sonic.app is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragAck {
    /// A destination has positively acknowledged the handoff and is
    /// now the owner of the tab's intent (cwd/cmd/env/scrollback).
    /// Source MUST kill its local copy to maintain at-most-one-live
    /// semantics. v1 has no transport that yields this on the macOS
    /// or Windows path; reserved for v2 once we add a heartbeat /
    /// reply-pasteboard-key protocol.
    Accepted,
    /// The sink published the payload (e.g. pasteboard write returned
    /// YES) OR it failed outright. In either case no receiver has
    /// confirmed adoption, so the source MUST NOT detach its local
    /// tab — that's the data-loss path Haiku flagged on PR #59.
    /// Callers should log/observe but otherwise behave as if the OS
    /// handoff did not happen.
    NotAcknowledged,
}

/// Trait implemented by platform-specific OS-drag senders.
///
/// `sonic-shared` knows when a tab has been dragged outside every
/// Sonic-owned window — that's the trigger for an OS-level handoff.
/// What it does NOT know is how to actually start an NSDragging
/// session or a `DoDragDrop` call; those live in `sonic-mac` and
/// `sonic-windows` respectively. The platform binary installs an
/// `OsDragSink` impl at startup; the app dispatches into it.
///
/// The return value gates whether the source tab dies — see
/// [`DragAck`]. Until v2 wires a cross-process consumption ack, real
/// platform sinks should return [`DragAck::NotAcknowledged`] so the
/// user's live session is preserved if the drop falls on the floor.
/// Thread-safe one-slot mailbox for an incoming [`TabPayload`].
///
/// Added in the Windows OLE drag-drop PR: unlike macOS, where the
/// pending payload lives on the system `NSPasteboard` and is read
/// synchronously on app activation, Windows delivers `IDropTarget::Drop`
/// callbacks on the OLE worker thread. The Windows side stashes the
/// payload here from that thread; the winit/main thread drains it on
/// the next event-loop tick.
///
/// Single-slot semantics on purpose: only the most recent unconsumed
/// drop is kept. If two drags land before the main loop drains, the
/// older one is replaced — matching macOS's pasteboard semantics where
/// later writes overwrite earlier ones for the same type. Future work
/// (queue, dedup) can build on this; v1 keeps it dead simple.
///
/// Mac currently reads from `NSPasteboard` directly and does not use
/// this slot. It's available for any future platform receiver that
/// runs off-main-thread and needs a thread-safe rendezvous.
#[derive(Debug, Default)]
pub struct PendingPayloadSlot {
    inner: std::sync::Mutex<Option<TabPayload>>,
}

impl PendingPayloadSlot {
    /// Construct an empty slot.
    pub const fn new() -> Self {
        Self { inner: std::sync::Mutex::new(None) }
    }

    /// Store a payload, replacing any older un-drained entry. Lock
    /// poisoning is recovered from in place — a panicked writer left
    /// the slot in a defined state (either Some or None) and the next
    /// reader/writer is allowed to proceed. The Windows OLE worker
    /// thread is short-lived and stateless beyond this call, so there
    /// is no follow-on invariant to repair.
    pub fn put(&self, payload: TabPayload) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(payload);
    }

    /// Drain the slot. Returns the payload (if any) and leaves the
    /// slot empty.
    pub fn take(&self) -> Option<TabPayload> {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.take()
    }
}

/// Trait implemented by platform-specific OS-drag senders.
///
/// `sonic-shared` knows when a tab has been dragged outside every
/// Sonic-owned window — that's the trigger for an OS-level handoff.
/// What it does NOT know is how to actually start an NSDragging
/// session or a `DoDragDrop` call; those live in `sonic-mac` and
/// `sonic-windows` respectively. The platform binary installs an
/// `OsDragSink` impl at startup; the app dispatches into it.
pub trait OsDragSink: Send + Sync {
    /// Hand the payload off to the OS. On macOS this writes it to
    /// the general `NSPasteboard` under [`PASTEBOARD_TYPE`]. On
    /// Windows v1 this is a no-op stub. On unsupported platforms it
    /// logs a warning.
    ///
    /// Returns [`DragAck::Accepted`] *only* when the sink is certain
    /// a destination has taken ownership; otherwise
    /// [`DragAck::NotAcknowledged`] so the caller keeps the source
    /// tab alive.
    fn begin_drag(&self, payload: &TabPayload) -> DragAck;
}

// Unit tests live in `tests/os_drag.rs`.
