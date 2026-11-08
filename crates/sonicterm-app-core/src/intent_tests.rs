//! Pins which intents the live App drives: input it must deliver, exit, and the PTY lifecycle.
//! The reducer-only topology, selection, search and palette intents are observed.

use bytes::Bytes;
use sonicterm_types::{ModKey, WindowKey};

use super::{AppIntent, IntentAuthority};
use crate::supporting::{KeyCode, PaneId, SplitDir};

/// A window key for samples.
const WINDOW: WindowKey = WindowKey(1);

/// One sample of every intent `App::dispatch_intent` executes.
fn driven_samples() -> Vec<AppIntent> {
    vec![
        AppIntent::PtyWrite { pane: PaneId(1), bytes: Bytes::from_static(b"x") },
        AppIntent::PtyExit { pane: PaneId(1), status: 0 },
        AppIntent::PtyBurst { pane: PaneId(1), generation: 1 },
        AppIntent::ForegroundProcChanged { pane: PaneId(1), name: None },
        AppIntent::RedrawRequested { window: WINDOW },
        AppIntent::Key { window: WINDOW, code: KeyCode(0), mods: ModKey::empty(), pressed: true },
        AppIntent::ImeStart { window: WINDOW },
        AppIntent::ImeEnd { window: WINDOW },
        AppIntent::ImePreedit { window: WINDOW, text: String::new(), cursor: 0..0 },
        AppIntent::ImeCommit { window: WINDOW, text: String::new() },
        AppIntent::HoverUrl { window: WINDOW, url: None },
        AppIntent::ScrollUp { window: WINDOW, lines: 1 },
        AppIntent::ScrollDown { window: WINDOW, lines: 1 },
        AppIntent::ScrollPageUp { window: WINDOW },
        AppIntent::ScrollPageDown { window: WINDOW },
        AppIntent::ScrollToTop { window: WINDOW },
        AppIntent::ScrollToBottom { window: WINDOW },
        AppIntent::ScrollToCursor { window: WINDOW },
        AppIntent::MouseWheel { window: WINDOW, delta_y: 1.0, delta_x: 0.0, mods: ModKey::empty() },
        AppIntent::Paste { window: WINDOW, text: String::new(), bracketed: false },
        AppIntent::ClickUrl { window: WINDOW, url: String::new() },
        AppIntent::Exit,
    ]
}

/// Every driven sample reports `Drives`; topology and a key release report `Observes`, since the
/// App's own handlers already did that work.
#[test]
fn input_and_lifecycle_are_driven_and_topology_is_observed() {
    for intent in driven_samples() {
        assert_eq!(intent.authority(), IntentAuthority::Drives, "{intent:?}");
    }
    for intent in [
        AppIntent::NewTab { window: WINDOW, cwd: None },
        AppIntent::SplitPane { window: WINDOW, dir: SplitDir::Right },
        AppIntent::ClosePane { window: WINDOW },
        AppIntent::OpenSearch { window: WINDOW },
        AppIntent::Key { window: WINDOW, code: KeyCode(0), mods: ModKey::empty(), pressed: false },
    ] {
        assert_eq!(intent.authority(), IntentAuthority::Observes, "{intent:?}");
    }
}
