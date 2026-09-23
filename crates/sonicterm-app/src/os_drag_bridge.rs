//! Native callbacks queue drop payloads and explicit file destinations without re-entering the borrowed App.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use winit::{event_loop::EventLoopProxy, window::WindowId};

use crate::app::UserEvent;
use crate::os_drag::TabPayload;

type FileDrop = (WindowId, Vec<PathBuf>);

static TAB_QUEUE: OnceLock<Mutex<VecDeque<TabPayload>>> = OnceLock::new();
static FILE_QUEUE: OnceLock<Mutex<VecDeque<FileDrop>>> = OnceLock::new();
static PROXY: OnceLock<Mutex<Option<EventLoopProxy<UserEvent>>>> = OnceLock::new();

fn tab_queue() -> &'static Mutex<VecDeque<TabPayload>> {
    TAB_QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn file_queue() -> &'static Mutex<VecDeque<FileDrop>> {
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
        // When: `proxy_slot().lock()` succeeds, inspect whether an event-loop wake target is installed.
        if let Some(p) = slot.as_ref() {
            // When: `p` is installed, post one payload-free wake so the main loop drains queued drag data.
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

/// Queue file paths for their registered destination only when its event loop can be woken.
// Hold file_queue across wake so failed admission can remove its own item before the event loop drains it.
pub fn push_files(window_id: WindowId, paths: Vec<PathBuf>) -> bool {
    if paths.is_empty() {
        // When: paths is empty, no file drop can be admitted or acknowledged.
        return false;
    }
    let Ok(mut queue) = file_queue().lock() else {
        // When: file_queue is poisoned, refuse the drop rather than report delivery that was not queued.
        return false;
    };
    queue.push_back((window_id, paths));
    if !wake() {
        // When: wake fails, the locked queue still owns the newest item and removes it before any drain can observe it.
        queue.pop_back();
        return false;
    }
    true
}

/// Drain every queued tab payload. Called by
/// [`crate::app::App::drain_os_drag`].
pub(crate) fn drain_tab_payloads() -> Vec<TabPayload> {
    let Ok(mut q) = tab_queue().lock() else {
        // When: `tab_queue().lock()` fails, return no payload rather than propagating poisoned shared state.
        return Vec::new();
    };
    q.drain(..).collect()
}

/// Drain file drops together with the native destination captured at admission.
pub(crate) fn drain_file_drops() -> Vec<(WindowId, Vec<PathBuf>)> {
    let Ok(mut q) = file_queue().lock() else {
        // When: `file_queue().lock()` fails, return no drop rather than propagating poisoned shared state.
        return Vec::new();
    };
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
pub fn __test_drain_files() -> Vec<(WindowId, Vec<PathBuf>)> {
    drain_file_drops()
}

#[cfg(test)]
#[path = "os_drag_bridge_tests.rs"]
mod os_drag_bridge_tests;
