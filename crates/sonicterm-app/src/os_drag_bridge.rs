//! Bridge between the platform-specific OLE / NSPasteboard drop
//! callbacks and the winit-driven [`crate::app::App`] event loop.
//!
//! Mirrors [`crate::menubar_bridge`] in shape: an off-main-thread
//! callback (OLE worker, AppKit dragging session) cannot touch
//! `&mut App` directly because the borrow lives behind
//! `event_loop.run_app(&mut app)`. We split delivery in two:
//!
//! 1. **Static `Mutex<VecDeque<...>>` queues** — the platform DropTarget
//!    pushes the parsed [`TabPayload`] or the parsed file path list here
//!    (data path).
//! 2. **`EventLoopProxy::send_event(UserEvent::OsDrag)`** — fires a
//!    payload-less wake-up so `ControlFlow::Wait` unblocks and the
//!    dispatcher drains the queue (wake path).
//!
//! This decouples the OLE worker thread from the App's `&mut self`
//! borrow and — critically — fixes the v1 bug where the Windows main
//! drained `take_pending_payload()` exactly ONCE at startup, so any
//! drop after the first never reached `new_tab_from_payload`. Each
//! drop now posts its own wake-up, every subsequent drop is observed.
//!
//! Cross-platform safe: every platform either uses this bridge or
//! ignores it. Mac currently reads from `NSPasteboard` synchronously
//! and may migrate here later.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;
use crate::os_drag::TabPayload;

static TAB_QUEUE: OnceLock<Mutex<VecDeque<TabPayload>>> = OnceLock::new();
static FILE_QUEUE: OnceLock<Mutex<VecDeque<Vec<PathBuf>>>> = OnceLock::new();
static PROXY: OnceLock<Mutex<Option<EventLoopProxy<UserEvent>>>> = OnceLock::new();

fn tab_queue() -> &'static Mutex<VecDeque<TabPayload>> {
    TAB_QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn file_queue() -> &'static Mutex<VecDeque<Vec<PathBuf>>> {
    FILE_QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn proxy_slot() -> &'static Mutex<Option<EventLoopProxy<UserEvent>>> {
    PROXY.get_or_init(|| Mutex::new(None))
}

/// Install the [`EventLoopProxy`] used to wake the winit loop after a
/// drop callback pushes a payload. Called once from the platform bin
/// after the event loop is created.
pub fn install_proxy(proxy: EventLoopProxy<UserEvent>) {
    if let Ok(mut slot) = proxy_slot().lock() {
        *slot = Some(proxy);
    }
}

fn wake() -> bool {
    if let Ok(slot) = proxy_slot().lock() {
        if let Some(p) = slot.as_ref() {
            return p.send_event(UserEvent::OsDrag).is_ok();
        }
    }
    false
}

/// Queue a [`TabPayload`] from an OLE / NSPasteboard drop and wake the
/// event loop. Returns `true` if the wake-up was posted.
pub fn push_tab_payload(payload: TabPayload) -> bool {
    if let Ok(mut q) = tab_queue().lock() {
        q.push_back(payload);
    }
    wake()
}

/// Queue a CF_HDROP / file-drop path list and wake the event loop.
/// Returns `true` if the wake-up was posted.
pub fn push_files(paths: Vec<PathBuf>) -> bool {
    if paths.is_empty() {
        return false;
    }
    if let Ok(mut q) = file_queue().lock() {
        q.push_back(paths);
    }
    wake()
}

/// Drain every queued tab payload. Called by
/// [`crate::app::App::drain_os_drag`].
pub(crate) fn drain_tab_payloads() -> Vec<TabPayload> {
    let Ok(mut q) = tab_queue().lock() else { return Vec::new() };
    q.drain(..).collect()
}

/// Drain every queued file-drop path list.
pub(crate) fn drain_file_drops() -> Vec<Vec<PathBuf>> {
    let Ok(mut q) = file_queue().lock() else { return Vec::new() };
    q.drain(..).collect()
}

/// Test bridge: same as [`drain_tab_payloads`] but reachable from
/// integration tests in other crates. Hidden from docs.
#[doc(hidden)]
pub fn __test_drain_tabs() -> Vec<TabPayload> {
    drain_tab_payloads()
}

/// Test bridge: same as [`drain_file_drops`] but reachable from
/// integration tests in other crates. Hidden from docs.
#[doc(hidden)]
pub fn __test_drain_files() -> Vec<Vec<PathBuf>> {
    drain_file_drops()
}

// Unit tests live in `tests/os_drag_bridge.rs`.
