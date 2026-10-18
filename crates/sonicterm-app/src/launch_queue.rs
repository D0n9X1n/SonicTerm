//! The one queue platform boundaries use to ask the app for a tab.
//!
//! A binary queues its startup launch before the event loop runs, and the macOS
//! open-documents handler queues script drafts at any time. Requests that
//! arrive before a main window exists stay queued until the window is ready.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

pub use sonicterm_types::{LaunchRequest, OpenScriptRequest};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

static QUEUE: OnceLock<Mutex<VecDeque<LaunchRequest>>> = OnceLock::new();
static PROXY: OnceLock<Mutex<Option<EventLoopProxy<UserEvent>>>> = OnceLock::new();

fn queue() -> &'static Mutex<VecDeque<LaunchRequest>> {
    QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn proxy_slot() -> &'static Mutex<Option<EventLoopProxy<UserEvent>>> {
    PROXY.get_or_init(|| Mutex::new(None))
}

/// Install the event-loop proxy used to wake the app after launches are queued.
pub fn install_proxy(proxy: EventLoopProxy<UserEvent>) {
    if let Ok(mut slot) = proxy_slot().lock() {
        *slot = Some(proxy);
    }
}

/// Queue launch requests and return whether the event-loop wake was posted.
// Lock order: acquire `queue` before `proxy_slot`; the queue guard is released before the proxy lock.
pub fn push_requests(requests: Vec<LaunchRequest>) -> bool {
    if let Ok(mut pending) = queue().lock() {
        pending.extend(requests);
    }
    if let Ok(slot) = proxy_slot().lock() {
        // When: `proxy_slot().lock()` yields `slot`, inspect whether a wake target is installed.
        if let Some(proxy) = slot.as_ref() {
            // When: `slot` contains `proxy`, post `LaunchRequests` and report whether winit accepted it.
            return proxy.send_event(UserEvent::LaunchRequests).is_ok();
        }
    }
    false
}

/// Queue script drafts; see [`push_requests`].
pub fn push_scripts(requests: Vec<OpenScriptRequest>) -> bool {
    push_requests(requests.into_iter().map(LaunchRequest::Script).collect())
}

pub(crate) fn drain() -> Vec<LaunchRequest> {
    let Ok(mut pending) = queue().lock() else {
        // When: `queue().lock()` fails, return no requests rather than propagate poisoned shared state.
        return Vec::new();
    };
    pending.drain(..).collect()
}

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
#[path = "launch_queue_tests.rs"]
mod launch_queue_tests;
