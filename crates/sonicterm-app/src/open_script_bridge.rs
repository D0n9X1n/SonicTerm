use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

pub use sonicterm_types::OpenScriptRequest;
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

static QUEUE: OnceLock<Mutex<VecDeque<OpenScriptRequest>>> = OnceLock::new();
static PROXY: OnceLock<Mutex<Option<EventLoopProxy<UserEvent>>>> = OnceLock::new();

fn queue() -> &'static Mutex<VecDeque<OpenScriptRequest>> {
    QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn proxy_slot() -> &'static Mutex<Option<EventLoopProxy<UserEvent>>> {
    PROXY.get_or_init(|| Mutex::new(None))
}

pub fn install_proxy(proxy: EventLoopProxy<UserEvent>) {
    if let Ok(mut slot) = proxy_slot().lock() {
        *slot = Some(proxy);
    }
}

pub fn push_requests(requests: Vec<OpenScriptRequest>) -> bool {
    if let Ok(mut pending) = queue().lock() {
        pending.extend(requests);
    }
    if let Ok(slot) = proxy_slot().lock() {
        if let Some(proxy) = slot.as_ref() {
            return proxy.send_event(UserEvent::OpenScripts).is_ok();
        }
    }
    false
}

pub(crate) fn drain() -> Vec<OpenScriptRequest> {
    let Ok(mut pending) = queue().lock() else { return Vec::new() };
    pending.drain(..).collect()
}

#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
#[path = "open_script_bridge_tests.rs"]
mod open_script_bridge_tests;
