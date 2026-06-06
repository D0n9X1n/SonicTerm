//! Windows-only test-automation harness sink (issue #508).
//!
//! Compiled in only under `#[cfg(all(target_os = "windows",
//! feature = "harness"))]`. The release binary MUST NOT carry these
//! symbols — `scripts/check-no-harness-in-release.ps1` enforces this
//! on the sibling `sonicterm-windows` crate.
//!
//! ## Why a shared slot, not a trait
//!
//! Per Haiku Step-1 + Opus Step-2 APPROVED-DIAG (#508): the harness
//! named-pipe server (in `sonicterm-windows`) runs on its own thread,
//! reads 4 KiB byte chunks, and needs to forward each chunk to the
//! **currently active** pane's `PtyHandle::in_tx` `Sender`. The active
//! pane can change at any moment — focus click, keyboard tab switch,
//! tear-out, drop, spawn, close. Plumbing a callback through every
//! pane-change site is brittle; instead we keep a single shared slot:
//!
//! ```text
//! pub type HarnessSink = Arc<Mutex<Option<Sender<Vec<u8>>>>>;
//! ```
//!
//! The App holds a clone of this `Arc` and publishes the active pane's
//! sender into the slot on every pane-change. The pipe server holds
//! another clone and reads from the slot on every chunk. Per-chunk
//! atomicity is the contract: a focus change between two chunks is
//! observable and intentional (chunk-1 → pane A, chunk-2 → pane B);
//! sub-chunk atomicity is NOT promised. See `harness_pipe.rs` for the
//! reader half and the race test in `tests/harness_race_test.rs`.
//!
//! ## Drop semantics
//!
//! No `Drop` impl. The App is expected to call `publish(None)`
//! explicitly from the close paths (last pane closed, main window
//! hidden, last tab dropped). Opus endorsed the Haiku design here:
//! implicit Drop racing with teardown is harder to reason about than
//! one explicit "no active pane right now" call.

use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;

/// Bytes pushed into the active PTY's input channel. Mirrors
/// `sonicterm_io::pty::Outgoing` (which is itself `Vec<u8>` today —
/// keep these in sync if `Outgoing` ever grows variants).
pub type HarnessBytes = Vec<u8>;

/// Shared slot the App keeps pointing at the active pane's
/// `PtyHandle::in_tx`. `None` means "no pane to inject into right now"
/// — the read loop drops the chunk and logs at trace.
pub type HarnessSink = Arc<Mutex<Option<Sender<HarnessBytes>>>>;

/// Build a fresh, empty sink. Cheap, no syscalls.
#[must_use]
pub fn new_sink() -> HarnessSink {
    Arc::new(Mutex::new(None))
}

/// Replace the active-pane sender. Called by the App from every
/// active-pane change site (see `app/mod.rs::refresh_harness_sink`).
/// Lock-poisoning is silently swallowed: a poisoned mutex means the
/// previous publisher panicked, which is already going to take the
/// process down via the panic hook.
pub fn publish(sink: &HarnessSink, tx: Option<Sender<HarnessBytes>>) {
    if let Ok(mut g) = sink.lock() {
        *g = tx;
    }
}
