//! Test helper binary for `tests/pty_multi_round_e2e.rs`.
//!
//! Writes "P1>", reads one line from stdin, writes "P2>", reads one line.
//! Cross-platform — used by the multi-round PTY regression test to drive
//! a real shell-style prompt→input→prompt→input flow through the actual
//! `PtyHandle` + `Parser` + `Grid` pipeline.
//!
//! Lives under `src/bin/` so Cargo exposes it to integration tests via
//! `env!("CARGO_BIN_EXE_pty_multi_round_helper")`.

use std::io::{BufRead, Write};

fn main() {
    let stdout = std::io::stdout();
    let stdin = std::io::stdin();
    let mut out = stdout.lock();
    let mut input = stdin.lock();

    // Round 1: emit prompt + newline (ConPTY on Windows can hold back
    // partial lines even after an explicit flush, so terminate each
    // prompt with "\n" — the test only asserts substring presence).
    out.write_all(b"P1>\n").expect("write P1");
    out.flush().expect("flush P1");
    let mut line = String::new();
    input.read_line(&mut line).expect("read round-1 reply");

    // Round 2: emit prompt + newline, wait for input.
    out.write_all(b"P2>\n").expect("write P2");
    out.flush().expect("flush P2");
    line.clear();
    input.read_line(&mut line).expect("read round-2 reply");

    // Final marker so the test knows both rounds completed end-to-end.
    out.write_all(b"DONE\n").expect("write DONE");
    out.flush().expect("flush DONE");
}
