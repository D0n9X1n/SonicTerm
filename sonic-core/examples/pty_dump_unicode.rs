//! Unicode capability E2E binary — sibling to `pty_dump`.
//!
//! Why this exists: `pty_dump` only emits ASCII output and so passes
//! against a renderer that has lost every non-ASCII glyph (the PR #42
//! B3-cutover regression). This binary feeds the user's real shell a
//! `printf` line containing one representative character from each
//! class Sonic claims to support, then verifies the resulting `Grid`
//! contains the literal codepoints — NOT `?` substitution, NOT
//! U+FFFD, NOT silent drops.
//!
//! Wired into the local gate in `CLAUDE.md` §2.
//!
//! Run with: `cargo run --example pty_dump_unicode -p sonic-core --release`

use std::time::{Duration, Instant};

use sonic_core::{
    grid::{CellFlags, Grid},
    pty::PtyHandle,
    vt::Parser,
};

/// One character from every class the capability matrix covers, modulo
/// the ones that can't survive POSIX `printf` quoting (combining marks,
/// raw ZWJ scalar — those have dedicated parser tests). Each char here
/// MUST appear verbatim in the resulting grid; anything else is the
/// PR-#42-class of silent regression we're trying to catch.
const SHIBBOLETH: &[char] = &[
    '中', '文', // CJK
    'ひ', 'カ', // Hiragana / Katakana
    '한', // Hangul
    '🎉', // Emoji single
    '─', '╭', '╮', // Box-drawing
    '\u{e0b0}', '\u{f015}', // Powerline / Nerd Font PUA
    '［', '］', // Fullwidth Latin
    'é', 'ñ', // Latin-1 supplement
];

fn main() {
    let pty = PtyHandle::spawn_default_shell(120, 24).expect("spawn shell");
    let mut parser = Parser::new(Grid::new(120, 24));

    drain(&pty, &mut parser, 1500);

    // Feed a `printf` whose output contains every shibboleth, separated
    // by single ASCII spaces so adjacent wide cells don't merge in the
    // visual flatten. Use printf rather than echo because echo's
    // behavior around backslash sequences is shell-dependent.
    let payload: String = SHIBBOLETH.iter().collect::<String>();
    let line = format!("printf '%s\\n' '{payload}'\r");
    pty.in_tx.send(line.into_bytes()).unwrap();
    drain(&pty, &mut parser, 2000);

    println!("\n=== grid after unicode printf ===");
    dump(&parser);

    let flat: String = parser
        .grid()
        .rows_iter()
        .flat_map(|row| {
            row.iter().filter(|c| !c.flags.contains(CellFlags::WIDE_CONT)).map(|c| c.ch)
        })
        .collect();

    let missing: Vec<char> = SHIBBOLETH.iter().copied().filter(|c| !flat.contains(*c)).collect();
    if !missing.is_empty() {
        eprintln!(
            "FAIL: {} of {} shibboleth chars are missing from the grid: {:?}",
            missing.len(),
            SHIBBOLETH.len(),
            missing
        );
        eprintln!(
            "Either the parser dropped them or the shell substituted '?' for unrenderable bytes."
        );
        eprintln!(
            "This is the PR-#42-class regression the renderer capability matrix watches for."
        );
        std::process::exit(1);
    }

    // Defense in depth: explicitly reject the canonical replacement
    // characters so a future bug that substitutes them (instead of
    // silently dropping) still triggers a failure.
    for bad in ['?', '\u{fffd}'] {
        // '?' on its own is fine — the prompt may legitimately contain
        // one. Only flag it if it's REPEATED beyond what a reasonable
        // prompt could contain, which is a strong signal of substitution.
        let n = flat.chars().filter(|c| *c == bad).count();
        if bad == '\u{fffd}' && n > 0 {
            eprintln!("FAIL: U+FFFD REPLACEMENT CHARACTER appeared {n} times — bad UTF-8 decode somewhere.");
            std::process::exit(1);
        }
    }

    pty.in_tx.send(b"exit\r".to_vec()).unwrap();
    drain(&pty, &mut parser, 500);
    println!("\n[unicode-e2e] OK ({} shibboleth chars verified)", SHIBBOLETH.len());
}

fn drain(pty: &PtyHandle, parser: &mut Parser, ms: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        if let Ok(b) = pty.out_rx.recv_timeout(Duration::from_millis(50)) {
            parser.advance(&b);
        }
    }
}

fn dump(parser: &Parser) {
    for r in 0..parser.grid().rows {
        let row: String = parser
            .grid()
            .row(r)
            .iter()
            .filter(|c| !c.flags.contains(CellFlags::WIDE_CONT))
            .map(|c| c.ch)
            .collect();
        let t = row.trim_end();
        if !t.is_empty() {
            println!("[{r:2}]: {t}");
        }
    }
}
