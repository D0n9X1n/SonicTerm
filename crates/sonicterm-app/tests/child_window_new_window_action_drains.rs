//! Haiku review on PR #371 — child-window keyboard dispatch must drain
//! `Action::NewWindow` in the same dispatch turn.
//!
//! The main-window keyboard path already calls `drain_pending_window_creates(el)`
//! immediately after a successful `run_action`. The child-window path added by
//! PR #371 initially forgot that drain, so a Cmd+N typed in a torn-out window
//! set `pending_new_window` but did not create the new window until the next
//! event-loop tick. A live child window plus `ActiveEventLoop` cannot be built
//! in a headless integration test, so this pins the test seam that mirrors the
//! child dispatch order and also source-checks the production child handler.

use std::{fs, path::PathBuf};

use sonicterm_app::app::App;
use sonicterm_cfg::{
    config::Config,
    keymap::{Action, ActionWrapper, Binding, Keymap, Meta},
    theme::{AnsiColors, Appearance, Hex, Palette, TabColors, Theme},
};
use winit::keyboard::{Key, ModifiersState, SmolStr};

fn hex() -> Hex {
    Hex("#000000".to_string())
}

fn ansi() -> AnsiColors {
    AnsiColors {
        black: hex(),
        red: hex(),
        green: hex(),
        yellow: hex(),
        blue: hex(),
        magenta: hex(),
        cyan: hex(),
        white: hex(),
    }
}

fn synth_theme() -> Theme {
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

fn keymap_with_cmd_n() -> Keymap {
    Keymap {
        meta: Meta { name: "test".into(), version: "0".into() },
        bindings: vec![Binding {
            keys: "super+n".into(),
            action: ActionWrapper(Action::NewWindow),
        }],
    }
}

fn child_window_src() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/app/child_window.rs");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn cmd_n_child_dispatch_clears_pending_new_window_in_same_turn() {
    let mut app = App::new(synth_theme(), Config::default(), keymap_with_cmd_n());
    app.__test_seed_tab("main");

    assert!(!app.__test_pending_new_window(), "precondition: no deferred window create");

    let (action, pty_bytes) = app.__test_dispatch_key_or_encode_pty_with_drain(
        &Key::Character(SmolStr::new("n")),
        ModifiersState::SUPER,
        true,
    );

    assert_eq!(action, Some(Action::NewWindow), "Cmd+N must dispatch NewWindow");
    assert_eq!(pty_bytes, None, "Cmd+N must not leak bytes to the PTY");
    assert!(
        !app.__test_pending_new_window(),
        "child-window dispatch must consume NewWindow's pending-create flag in the same turn"
    );
}

#[test]
fn child_window_run_action_path_invokes_pending_create_drain() {
    let source = child_window_src();
    let idx = source
        .find("if self.run_action(&action) {")
        .expect("child full-dispatch run_action path must exist");
    let block = &source[idx..idx + 320];

    assert!(
        block.contains("self.drain_pending_window_creates(el)"),
        "child-window full-dispatch path must mirror main dispatch by draining pending window creates; found:\n{block}",
    );
}
