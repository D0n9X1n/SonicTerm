#![cfg(target_os = "windows")]
//! F-ConPTY app rows: a child behind a real pseudoconsole sends each query, and the host answers
//! through a SonicTerm pane running the production VT publisher (`App::__test_reply_pane`), so
//! the child reports the exact reply bytes a pane's reply path wrote back through ConPTY. The
//! expected reply comes from a fresh reference pane fed only that case's output.
//!
//! Opt-in: `cargo test --locked -p sonicterm-io -p sonicterm-app --test windows_conpty_protocol_forms
//! --test windows_conpty_app_smoke -- --ignored --nocapture`.

#![warn(clippy::min_ident_chars)]

use sonicterm_app::app::{App, TestReplyPane};
use sonicterm_io::conpty_fixture::{run, Case, Engine, InputPhase, PTY_COLS, PTY_ROWS};

/// The live pane the child talks to; each case's expectation comes from a fresh pane.
struct AppEngine {
    live: TestReplyPane,
}

impl Engine for AppEngine {
    fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.live.replies(bytes)
    }

    fn reference_reply(&mut self, case: &Case) -> Vec<u8> {
        App::__test_reply_pane(PTY_COLS, PTY_ROWS).replies(&case.output)
    }
}

/// Reply rows: each query's exact reply must reach the child. Queries change no pane state, so
/// the reference pane's answer is the live pane's. Synthetic strings only.
fn cases() -> Vec<Case> {
    let reply = || InputPhase::Echo(Vec::new());
    vec![
        Case::new("control", "CONTROL-OK")
            .input(InputPhase::Echo(b"ping".to_vec()))
            .output_required()
            .input_required(),
        Case::new("osc4-query", "\x1b]4;1;?\x1b\\").input(reply()).input_required(),
        Case::new("osc10-query-chain", "\x1b]10;?;?\x07").input(reply()).input_required(),
        Case::new("osc21-query", "\x1b]21;foreground=?;nonsense=?\x1b\\")
            .input(reply())
            .input_required(),
        Case::new("osc22-query", "\x1b]22;?pointer,nonsense\x07").input(reply()).input_required(),
        Case::new("osc7501-query", "\x1b]7501;?\x07").input(reply()).input_required(),
    ]
}

/// Contract: a pane's replies to color, pointer and program-status queries reach a program behind
/// a real pseudoconsole byte for byte on the packaged runtime; the positive control is exact on
/// both runtimes.
#[test]
#[ignore = "needs a staged ConPTY runtime; run with --ignored (the windows-conpty-forms gate step)"]
fn windows_conpty_app_smoke() {
    run(
        "windows_conpty_app_smoke",
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        &cases(),
        || AppEngine { live: App::__test_reply_pane(PTY_COLS, PTY_ROWS) },
    );
}
