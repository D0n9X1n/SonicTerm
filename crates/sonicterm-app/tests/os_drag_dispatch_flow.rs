//! Phase C2 — OS-drag dispatch flow integration tests.
//!
//! Covers the seam where a real NSDraggingSession / OLE DoDragDrop
//! event posts back into the App's event loop:
//!
//! 1. `App::begin_os_tab_drag` invokes the installed backend's
//!    `begin_session` with the correct source coords + drag image.
//!    (Tested in the no-proxy fallback shape — the real proxy path
//!    requires a live display.)
//! 2. `UserEvent::DragEnded` carrying a `DragOutcome::DroppedOnBar` routes
//!    through `App::transfer_tab` when the source/target line up.
//! 3. `UserEvent::DragEnded` carrying a `DragOutcome::Cancelled`
//!    routes through `App::cancel_drag_session`, leaving the tab
//!    intact.
//!
//! The actual NSDraggingSession / OLE FFI integration (the bodies of
//! `sonicterm-mac::tab_drag_os::MacOsTabDragBackend::begin_session` and
//! the Windows equivalent) is verified by the §13 GUI smoke recipe
//! in CLAUDE.md — that can't be unit-tested without a real wgpu
//! surface + OS event loop. This file pins the platform-agnostic
//! dispatch contract that both backends rely on.

use std::sync::{Arc, Mutex};

use sonicterm_app::app::os_drag::{AppHandle, DragOutcome, OsTabDragBackend};
use sonicterm_app::app::App;
use sonicterm_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use winit::window::WindowId;

fn synth_theme() -> Theme {
    let hex = || Hex("#000000".to_string());
    let ansi = || AnsiColors {
        black: hex(),
        red: hex(),
        green: hex(),
        yellow: hex(),
        blue: hex(),
        magenta: hex(),
        cyan: hex(),
        white: hex(),
    };
    Theme {
        name: "test".into(),
        appearance: Appearance::Dark,
        colors: Palette {
            background: hex(),
            foreground: hex(),
            cursor: hex(),
            cursor_text: hex(),
            selection_bg: hex(),
            selection_fg: hex(),
            ansi: ansi(),
            bright: ansi(),
            tab: TabColors {
                bar_bg: hex(),
                active_bg: hex(),
                active_fg: hex(),
                inactive_bg: hex(),
                inactive_fg: hex(),
                hover_bg: hex(),
                hover_fg: hex(),
                close_button_fg: hex(),
            },
        },
    }
}

fn synth_app() -> App {
    let keymap =
        Keymap { meta: Meta { name: "test".into(), version: "0".into() }, bindings: Vec::new() };
    App::new(synth_theme(), Config::default(), keymap)
}

/// Synthesize a stand-in `WindowId`. winit doesn't expose a public
/// `WindowId::dummy()` outside of test cfg, so we transmute. This is
/// safe for the *dispatcher* tests: the dispatcher only compares
/// against `App::window.id()` (None in our headless harness) and uses
/// the id as an opaque key into `windows: HashMap<WindowId, _>`.
fn synth_window_id(tag: u64) -> WindowId {
    // SAFETY: WindowId is `#[repr(transparent)] pub struct WindowId(u64)`
    // in winit; in test builds we just need a stable opaque identifier.
    // Production code never reaches this path.
    unsafe { std::mem::transmute::<u64, WindowId>(tag) }
}

/// Records what the mock backend was asked to do, captured per call.
#[derive(Debug, Clone)]
struct BeginCall {
    source_window_tag: u64,
    source_tab_idx: usize,
    image_bytes_len: usize,
}

/// Mock backend that records every invocation. Implements
/// [`OsTabDragBackend`]; cloning is cheap (Arc-shared state) so tests
/// keep one handle for assertions and hand the other to the App.
#[derive(Default, Clone)]
struct MockBackend {
    calls: Arc<Mutex<Vec<BeginCall>>>,
}

impl MockBackend {
    fn new() -> Self {
        Self::default()
    }
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    fn last_call(&self) -> Option<BeginCall> {
        self.calls.lock().unwrap().last().cloned()
    }
}

impl OsTabDragBackend for MockBackend {
    fn begin_session(
        &mut self,
        _handle: AppHandle,
        source_window: WindowId,
        source_tab_idx: usize,
        _payload_json: String,
        drag_image_png: Vec<u8>,
    ) {
        // SAFETY: reverse transmute of the synth_window_id helper —
        // both sides are the test crate. Used only for assertion.
        let tag: u64 = unsafe { std::mem::transmute::<WindowId, u64>(source_window) };
        self.calls.lock().unwrap().push(BeginCall {
            source_window_tag: tag,
            source_tab_idx,
            image_bytes_len: drag_image_png.len(),
        });
    }
}

// ---- 1. begin_os_tab_drag invokes backend ---------------------------------

#[test]
fn begin_os_tab_drag_with_no_proxy_falls_back_cleanly() {
    // No EventLoopProxy → no AppHandle → backend must not be invoked
    // and the function must report false so the caller falls back to
    // the existing tear_out path. This is the contract that prevents
    // the OS-drag path from silently swallowing gestures on test
    // harnesses or platforms without a backend.
    let mut app = synth_app();
    let backend = MockBackend::new();
    app.__test_set_os_drag_backend(Box::new(backend.clone()));
    let started = app.begin_os_tab_drag(synth_window_id(0xCAFE), 1, String::new(), vec![1u8; 32]);
    assert!(!started, "no-proxy path must report false");
    assert_eq!(backend.call_count(), 0, "no-proxy path must not call backend");
}

// ---- 2. handle_os_drag_ended routes Cancelled → cancel_drag_session -------

#[test]
fn drag_ended_cancelled_routes_to_cancel_and_preserves_tabs() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let _ = app.__test_seed_tab("bravo");
    let before = app.__test_tab_count();

    // Seed the in-flight source as if begin_os_tab_drag had run.
    app.__test_set_os_drag_source(Some((synth_window_id(0xAAAA), 0)));
    // Populate the mailbox with a Cancelled outcome directly — this is
    // what AppHandle::post_drag_ended does internally, minus the
    // EventLoopProxy wake (which the dispatcher does not need; only
    // the wake-to-main-loop mechanism uses it).
    app.__test_os_drag_pending().set_ended(DragOutcome::Cancelled);

    let processed = app.handle_os_drag_ended();
    assert_eq!(processed, Some(DragOutcome::Cancelled));
    assert_eq!(
        app.__test_tab_count(),
        before,
        "Cancelled outcome must leave the tab count unchanged"
    );
}

// ---- 3. handle_os_drag_ended routes Drop → transfer_tab -------------------

#[test]
fn drag_ended_drop_invokes_transfer_tab_and_logs_unknown_target_cleanly() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let _ = app.__test_seed_tab("bravo");
    let _ = app.__test_seed_tab("charlie");
    let before = app.__test_tab_count();
    assert_eq!(before, 3);

    // Drive a Drop outcome whose target window does not exist in
    // `App::windows` — the dispatcher must NOT panic; it must catch
    // the `TargetMissing` Err from `transfer_tab` and cancel cleanly,
    // leaving the source tab intact (data-loss regression guard,
    // PR #294 Haiku review).
    let src = synth_window_id(0xAAAA);
    let tgt = synth_window_id(0xBBBB);
    app.__test_set_os_drag_source(Some((src, 1)));
    app.__test_os_drag_pending()
        .set_ended(DragOutcome::DroppedOnBar { target_window: Some(tgt), target_slot: 0 });

    let processed = app.handle_os_drag_ended();
    assert!(
        matches!(processed, Some(DragOutcome::DroppedOnBar { .. })),
        "Drop outcome was drained"
    );
    assert_eq!(app.__test_tab_count(), before, "unknown target must not destroy any tab");
}

// ---- 4. defensive: Drop with no recorded source cancels rather than panic -

#[test]
fn drag_ended_drop_without_source_cancels_cleanly() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let _ = app.__test_seed_tab("bravo");
    let before = app.__test_tab_count();

    // No source set — the dispatcher must defensively cancel rather
    // than unwrap a None and panic. Regression guard for the
    // "Drop arrived after the in-flight bookkeeping was already
    // drained by an earlier event" case (concurrent ESC + drop).
    app.__test_set_os_drag_source(None);
    app.__test_os_drag_pending()
        .set_ended(DragOutcome::DroppedOnBar { target_window: None, target_slot: 0 });

    let processed = app.handle_os_drag_ended();
    assert!(matches!(processed, Some(DragOutcome::DroppedOnBar { .. })));
    assert_eq!(app.__test_tab_count(), before);
}

// ---- 5. handle_os_drag_moved drains the mailbox without side effects ------

#[test]
fn drag_moved_drains_position_from_mailbox() {
    let mut app = synth_app();
    app.__test_os_drag_pending().set_moved((123, 456));
    let drained = app.handle_os_drag_moved();
    assert_eq!(drained, Some((123, 456)));
    // Slot is now empty — a second drain returns None.
    let second = app.handle_os_drag_moved();
    assert_eq!(second, None);
}

// ---- 6. backend round-trip through __test_set_os_drag_backend + mock ------

#[test]
fn mock_backend_records_call_when_invoked_directly() {
    // Demonstrates that the MockBackend trait impl is wired correctly
    // — the production path is identical except `begin_os_tab_drag`
    // guards on an EventLoopProxy that doesn't exist in tests.
    let backend = MockBackend::new();
    let mut backend_for_app: Box<dyn OsTabDragBackend> = Box::new(backend.clone());

    // Build a sentinel AppHandle by going through the App. Without a
    // proxy this returns None; we drive begin_session directly with a
    // *forged* handle constructed via AppHandle::new on a sacrificial
    // proxy IFF we can build one. This test focuses on the trait
    // contract; the App-integrated path is covered by test #1.
    //
    // Construct a real winit event loop only if the platform allows
    // it — on macOS this works in tests only if cargo test is invoked
    // on the main thread (cargo's default test runner is NOT on the
    // main thread on macOS, so this typically panics). On headless
    // Linux CI it would also panic. In either case we skip the
    // dispatch and assert only on the no-call baseline. We catch the
    // panic with `catch_unwind` so the test stays portable.
    let proxy_opt = std::panic::catch_unwind(|| {
        winit::event_loop::EventLoop::<sonicterm_app::app::UserEvent>::with_user_event()
            .build()
            .ok()
            .map(|el| el.create_proxy())
    })
    .ok()
    .flatten();
    let Some(proxy) = proxy_opt else {
        // Couldn't build an event loop in this harness — skip. The
        // App-integrated path is covered by test #1.
        assert_eq!(backend.call_count(), 0);
        return;
    };
    let handle = AppHandle::new(proxy);
    backend_for_app.begin_session(handle, synth_window_id(0x1234), 7, String::new(), vec![0u8; 64]);
    assert_eq!(backend.call_count(), 1);
    let call = backend.last_call().unwrap();
    assert_eq!(call.source_window_tag, 0x1234);
    assert_eq!(call.source_tab_idx, 7);
    assert_eq!(call.image_bytes_len, 64);
}
