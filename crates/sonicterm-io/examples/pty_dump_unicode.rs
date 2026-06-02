//! Unicode capability E2E binary — sibling to `pty_dump`.
//!
//! Why this exists: `pty_dump` only emits ASCII output and so passes
//! against a renderer that has lost every non-ASCII glyph (the PR #42
//! B3-cutover regression). This binary feeds the user's real shell a
//! `printf` line containing one representative character from each
//! class SonicTerm claims to support, then verifies the resulting `Grid`
//! contains the literal codepoints — NOT `?` substitution, NOT
//! U+FFFD, NOT silent drops.
//!
//! ## Echoed-command pitfall (Haiku review of PR #47)
//!
//! An interactive shell echoes the command line itself. If we just
//! scanned the whole grid, the echoed `printf '...中文ひカ한🎉...'`
//! line would satisfy the "contains shibboleth chars" assertion even
//! if `printf` produced no output (or substituted everything with
//! tofu/`?`). To avoid that, we bracket the actual `printf` output
//! with two ASCII-only sentinels — `BEGIN_UNICODE` and `END_UNICODE`
//! — and assert ONLY on the rows that fall strictly between the LAST
//! occurrence of the BEGIN sentinel (which is the printf OUTPUT, not
//! the echo of the command line that produced it) and the FIRST
//! occurrence of the END sentinel after it. The asserted region is
//! exactly the printf payload row(s).
//!
//! Wired into the local gate in `CLAUDE.md` §2.
//!
//! Run with: `cargo run --example pty_dump_unicode -p sonicterm-io --release --features test_support`

use std::time::{Duration, Instant};

use sonicterm_grid::grid::{CellFlags, Grid};
use sonicterm_io::pty::PtyHandle;
use sonicterm_io::test_support::shell_dialect::dialect_for_shell;
use sonicterm_vt::vt::Parser;

/// One character from every class the capability matrix covers. Each char
/// here MUST appear verbatim in the asserted region; anything else is the
/// PR-#42-class of silent regression we're trying to catch. Includes the
/// CJK regression chars (中恶臭) per #461 to lock down the wrong-glyph-id
/// safety from `shape.rs:259-289`.
const SHIBBOLETH: &[char] = &[
    '中', '文', '恶', '臭', // CJK + #461 regression guards
    'ひ', 'カ', // Hiragana / Katakana
    '한', // Hangul
    '🎉', // Emoji single
    '─', '╭', '╮', // Box-drawing
    '\u{e0b0}', '\u{f015}', // Powerline / Nerd Font PUA
    '［', '］', // Fullwidth Latin
    'é', 'ñ', // Latin-1 supplement
];

const BEGIN: &str = "BEGIN_UNICODE";
const END: &str = "END_UNICODE";

fn main() {
    let pty = PtyHandle::spawn_default_shell(
        120,
        24,
        sonicterm_io::pty::ShellSpawnOpts { clean_e2e: true },
    )
    .expect("spawn shell");
    let mut parser = Parser::new(Grid::new(120, 24));

    // #457: pick the dialect for whatever shell got resolved (pwsh /
    // powershell / bash / zsh / sh). Errors out loudly if cmd.exe /
    // fish / unknown — those will never produce expected output.
    let dialect = match dialect_for_shell(pty.shell_program_path()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("FAIL: {e}");
            eprintln!("e2e gate doesn't support {:?} — see #457", pty.shell_program_path());
            std::process::exit(2);
        }
    };
    eprintln!("[unicode-e2e] shell={:?} dialect={}", pty.shell_program_path(), dialect.name());

    drain(&pty, &mut parser, 1500);

    // Bracket the actual output with BEGIN/END sentinels via the dialect.
    // PosixDialect emits `printf '...'` syntax; PowerShellDialect emits
    // `[Console]::Out.WriteLine(...)` with UTF-8 OutputEncoding forced.
    let payload: String = SHIBBOLETH.iter().collect::<String>();
    let cmd = dialect.emit_unicode_markers(&payload);
    pty.in_tx.send(cmd).unwrap();
    drain(&pty, &mut parser, 2500);

    println!("\n=== grid after unicode emit ({} dialect) ===", dialect.name());
    dump(&parser);

    let rows: Vec<String> = parser
        .grid()
        .rows_iter()
        .map(|row| {
            row.iter()
                .filter(|c| !c.flags.contains(CellFlags::WIDE_CONT))
                .map(|c| c.ch)
                .collect::<String>()
        })
        .collect();

    let Some(begin_row) = rows.iter().rposition(|r| r.contains(BEGIN)) else {
        eprintln!("FAIL: BEGIN sentinel '{BEGIN}' not found in grid.");
        eprintln!(
            "Shell ({} dialect) never produced output, or the parser dropped the sentinel.",
            dialect.name()
        );
        std::process::exit(1);
    };
    let Some(end_offset) = rows[begin_row + 1..].iter().position(|r| r.contains(END)) else {
        eprintln!("FAIL: END sentinel '{END}' not found after BEGIN row {begin_row}.");
        eprintln!(
            "Shell ({} dialect) produced BEGIN sentinel but truncated before END.",
            dialect.name()
        );
        std::process::exit(1);
    };
    let end_row = begin_row + 1 + end_offset;

    if end_row <= begin_row + 1 {
        eprintln!("FAIL: no content rows between BEGIN (row {begin_row}) and END (row {end_row}).");
        std::process::exit(1);
    }

    let region: String = rows[begin_row + 1..end_row].join("\n");
    println!("\n=== asserted region (rows {}..{}) ===", begin_row + 1, end_row);
    println!("{region}");

    let missing: Vec<char> = SHIBBOLETH.iter().copied().filter(|c| !region.contains(*c)).collect();
    if !missing.is_empty() {
        eprintln!(
            "FAIL: {} of {} shibboleth chars are missing from the {} output region: {:?}",
            missing.len(),
            SHIBBOLETH.len(),
            dialect.name(),
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

    let fffd = region.chars().filter(|c| *c == '\u{fffd}').count();
    if fffd > 0 {
        eprintln!("FAIL: U+FFFD REPLACEMENT CHARACTER appeared {fffd} times in output region — bad UTF-8 decode somewhere.");
        std::process::exit(1);
    }
    let qmarks = region.chars().filter(|c| *c == '?').count();
    if qmarks > 0 {
        eprintln!("FAIL: '?' appeared {qmarks} times in output region — likely '?' substitution for unrenderable bytes.");
        eprintln!("The payload contains no literal '?', so any '?' here is a substitution.");
        std::process::exit(1);
    }

    pty.in_tx.send(b"exit\r".to_vec()).unwrap();
    drain(&pty, &mut parser, 500);
    println!(
        "\n[unicode-e2e] OK ({} shibboleth chars verified via {} dialect)",
        SHIBBOLETH.len(),
        dialect.name()
    );
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
