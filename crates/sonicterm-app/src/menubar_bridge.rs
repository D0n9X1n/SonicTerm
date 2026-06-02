//! Bridge between the macOS native `NSMenu` (built by `sonicterm-mac`)
//! and the winit-driven [`crate::app::App`] event loop.
//!
//! NSMenu items fire on the AppKit main thread via Objective-C
//! selectors. Those callbacks cannot directly call into `App` — the
//! borrow lives behind `event_loop.run_app(&mut app)`. We split the
//! delivery in two:
//!
//! 1. **Static `Mutex<VecDeque<Action>>`** — NSMenu selectors push
//!    the chosen [`Action`] here (data path).
//! 2. **`EventLoopProxy::send_event(UserEvent::MenuAction)`** —
//!    fires a payload-less wake-up so `ControlFlow::Wait` unblocks
//!    and the dispatcher drains the queue (wake path).
//!
//! Cross-platform safe: the Windows bin never calls `push_action`
//! (the menubar module is `#[cfg(target_os = "macos")]`); the proxy
//! slot stays `None`, the queue stays empty.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use sonicterm_cfg::keymap::Action;
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

static QUEUE: OnceLock<Mutex<VecDeque<Action>>> = OnceLock::new();
static PROXY: OnceLock<Mutex<Option<EventLoopProxy<UserEvent>>>> = OnceLock::new();

fn queue() -> &'static Mutex<VecDeque<Action>> {
    QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn proxy_slot() -> &'static Mutex<Option<EventLoopProxy<UserEvent>>> {
    PROXY.get_or_init(|| Mutex::new(None))
}

/// Install the [`EventLoopProxy`] used to wake the winit loop after
/// a menu item pushes an [`Action`]. Called once from the platform
/// bin after the event loop is created.
pub fn install_proxy(proxy: EventLoopProxy<UserEvent>) {
    if let Ok(mut slot) = proxy_slot().lock() {
        *slot = Some(proxy);
    }
}

/// Queue an action for the next `UserEvent::MenuAction` drain and
/// wake the event loop. Returns `true` if the wake-up was posted.
pub fn push_action(action: Action) -> bool {
    if let Ok(mut q) = queue().lock() {
        q.push_back(action);
    }
    if let Ok(slot) = proxy_slot().lock() {
        if let Some(p) = slot.as_ref() {
            return p.send_event(UserEvent::MenuAction).is_ok();
        }
    }
    false
}

/// Drain every queued action. Called by
/// [`crate::app::App::drain_menubar_actions`].
pub(crate) fn drain() -> Vec<Action> {
    let Ok(mut q) = queue().lock() else { return Vec::new() };
    q.drain(..).collect()
}

/// Test bridge: same as [`drain`] but reachable from integration tests
/// in other crates. Hidden from docs.
#[doc(hidden)]
pub fn __test_drain() -> Vec<Action> {
    drain()
}

// Unit tests live in `tests/menubar_bridge.rs`.
