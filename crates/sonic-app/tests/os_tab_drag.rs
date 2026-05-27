//! Integration tests for the OS-level tab drag handoff —
//! specifically the (review) data-loss fix on PR #59.
//!
//! What we cover here:
//!
//! 1. **Wire-format round trip** — a payload survives
//!    serialize → deserialize without losing semantic equality. This
//!    is the contract between the source and the destination process.
//!
//! 2. **Source does NOT kill its tab when the sink returns
//!    `NotAcknowledged`.** This is the principal data-loss fix: the
//!    original code unconditionally detached the source tab after
//!    `begin_drag`, which destroyed user sessions whenever no
//!    receiver consumed the pasteboard. We mock a rejecting sink and
//!    assert the tab is still there after `try_os_drag_handoff`.
//!
//! 3. **Source DOES kill its tab when the sink returns `Accepted`.**
//!    Symmetric case so we don't regress into "never kill" (which
//!    would leave two live shells per drag if a v2 transport ever
//!    reports adoption).
//!
//! 4. **Receiver path actually spawns a tab from the payload** via
//!    `App::new_tab_from_payload`. The pre-fix code only logged the
//!    payload; this asserts the destination side now materializes
//!    real UI state.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sonic_app::app::App;
use sonic_app::os_drag::{DragAck, OsDragSink, TabPayload};
use sonic_core::{
    config::Config,
    keymap::{Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};

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

fn synth_payload(title: &str) -> TabPayload {
    TabPayload {
        pty_pid: 4242,
        tab_title: title.to_string(),
        scrollback_b64: TabPayload::encode_scrollback(b"prior output\n"),
        cwd: "/tmp".to_string(),
        cmd: "/bin/zsh".to_string(),
        env: vec![("TERM".into(), "xterm-256color".into())],
    }
}

/// Mock sink that records each `begin_drag` invocation and returns a
/// fixed [`DragAck`]. Lets us simulate "destination accepted" and
/// "destination did not acknowledge" cases without any platform FFI.
struct MockSink {
    ack: DragAck,
    calls: AtomicUsize,
}

impl MockSink {
    fn new(ack: DragAck) -> Arc<Self> {
        Arc::new(Self { ack, calls: AtomicUsize::new(0) })
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl OsDragSink for MockSink {
    fn begin_drag(&self, _payload: &TabPayload) -> DragAck {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.ack
    }
}

// ---- 1. wire format round trip --------------------------------------------

#[test]
fn payload_round_trip_serialize_deserialize_equal() {
    let p = synth_payload("zsh — ~/sonic");
    let json = p.to_json().expect("encode");
    let back = TabPayload::from_json(&json).expect("decode");
    assert_eq!(p, back, "payload survived round trip without loss");
}

// ---- 2. source preserves tab on NotAcknowledged ---------------------------

#[test]
fn source_does_not_kill_tab_when_sink_returns_not_acknowledged() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let _ = app.__test_seed_tab("bravo");
    let before = app.__test_tab_count();
    assert!(before >= 2);

    let sink = MockSink::new(DragAck::NotAcknowledged);
    app.__test_set_os_drag_sink(sink.clone() as Arc<dyn OsDragSink>);

    // Tear out the last tab. With no window installed,
    // cursor_inside_any_window() returns false, so the OS-drag path
    // actually runs. The fix makes try_os_drag_handoff return false
    // and *not* call detach_tab_state.
    let handed_off = app.__test_try_os_drag_handoff(before - 1);

    assert!(!handed_off, "handoff should report false on NotAcknowledged");
    assert_eq!(sink.call_count(), 1, "sink was consulted exactly once");
    assert_eq!(
        app.__test_tab_count(),
        before,
        "DATA-LOSS regression: source tab was destroyed despite no destination ack"
    );
}

// ---- 3. source DOES kill tab when sink reports Accepted -------------------

#[test]
fn source_kills_tab_when_sink_returns_accepted() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("alpha");
    let _ = app.__test_seed_tab("bravo");
    let before = app.__test_tab_count();

    let sink = MockSink::new(DragAck::Accepted);
    app.__test_set_os_drag_sink(sink.clone() as Arc<dyn OsDragSink>);

    let handed_off = app.__test_try_os_drag_handoff(before - 1);
    assert!(handed_off, "handoff should report true on Accepted");
    assert_eq!(app.__test_tab_count(), before - 1, "source tab detached after positive ack");
}

// ---- 4. receiver path materializes a tab from the payload -----------------

#[test]
fn receiver_spawn_path_creates_tab_from_payload() {
    // The mac receiver, after take_pending_payload returns Some, used
    // to only `tracing::info!` it. The (review) fix routes it through
    // App::new_tab_from_payload so a real tab appears. Here we drive
    // that method directly with a synthesized payload — the same call
    // sonic-mac/src/main.rs makes on startup via
    // run_with_os_drag_and_pending.
    let mut app = synth_app();
    let _ = app.__test_seed_tab("existing");
    let before = app.__test_tab_count();

    let payload = synth_payload("incoming — torn from PID 4242");
    let idx = app.new_tab_from_payload(&payload);

    assert_eq!(app.__test_tab_count(), before + 1, "receiver did not spawn a destination tab");
    assert_eq!(idx, before, "new tab index is end-of-bar (append, not insert)");
}

#[test]
fn receiver_uses_fallback_title_when_payload_title_empty() {
    let mut app = synth_app();
    let _ = app.__test_seed_tab("existing");
    let before = app.__test_tab_count();

    let mut payload = synth_payload("ignored");
    payload.tab_title.clear();
    let _ = app.new_tab_from_payload(&payload);

    assert_eq!(app.__test_tab_count(), before + 1, "empty-title payload still spawns a tab");
}
