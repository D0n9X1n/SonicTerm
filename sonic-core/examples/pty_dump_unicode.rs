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
/// MUST appear verbatim in the asserted region; anything else is the
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

const BEGIN: &str = "BEGIN_UNICODE";
const END: &str = "END_UNICODE";

fn main() {
    let pty = PtyHandle::spawn_default_shell(120, 24).expect("spawn shell");
    let mut parser = Parser::new(Grid::new(120, 24));

    drain(&pty, &mut parser, 1500);

    // Bracket the actual printf output with ASCII-only sentinels. The
    // shell will echo the entire command line back (containing all the
    // shibboleth chars AND both sentinels), and then `printf` will
    // write its own lines after a CR/LF. The sentinels let us identify
    // the printf OUTPUT region — not the echoed command — to assert
    // against.
    let payload: String = SHIBBOLETH.iter().collect::<String>();
    let line = format!("printf '{BEGIN}\\n%s\\n{END}\\n' '{payload}'\r");
    pty.in_tx.send(line.into_bytes()).unwrap();
    drain(&pty, &mut parser, 2000);

    println!("\n=== grid after unicode printf ===");
    dump(&parser);

    // Collect rows as strings (one per grid row), skipping WIDE_CONT
    // continuation cells so multi-column glyphs only show up once.
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

    // Find the LAST row containing BEGIN_UNICODE — that's the row
    // printf wrote, not the echoed command (which appears earlier).
    // Then find the FIRST row containing END_UNICODE strictly after
    // that. The asserted region is the rows strictly between them.
    let Some(begin_row) = rows.iter().rposition(|r| r.contains(BEGIN)) else {
        eprintln!("FAIL: BEGIN sentinel '{BEGIN}' not found in grid.");
        eprintln!("printf never produced output, or the parser dropped the sentinel.");
        std::process::exit(1);
    };
    let Some(end_offset) = rows[begin_row + 1..].iter().position(|r| r.contains(END)) else {
        eprintln!("FAIL: END sentinel '{END}' not found after BEGIN row {begin_row}.");
        eprintln!("printf produced the BEGIN sentinel but truncated before END.");
        std::process::exit(1);
    };
    let end_row = begin_row + 1 + end_offset;

    // Sanity: there must be at least one row of actual content between
    // the sentinels. If printf wrote BEGIN and END but nothing between
    // them, that's a regression.
    if end_row <= begin_row + 1 {
        eprintln!(
            "FAIL: no content rows between BEGIN (row {begin_row}) and END (row {end_row})."
        );
        std::process::exit(1);
    }

    let region: String = rows[begin_row + 1..end_row].join("\n");
    println!("\n=== asserted region (rows {}..{}) ===", begin_row + 1, end_row);
    println!("{region}");

    let missing: Vec<char> =
        SHIBBOLETH.iter().copied().filter(|c| !region.contains(*c)).collect();
    if !missing.is_empty() {
        eprintln!(
            "FAIL: {} of {} shibboleth chars are missing from the printf output region: {:?}",
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

    // Substitution guards on the ASSERTED REGION ONLY (the echoed
    // command line, if it contains '?' from the shell prompt, must
    // not pollute these checks).
    //
    // - U+FFFD: any occurrence is a bad UTF-8 decode somewhere.
    // - '?': in the printf output region there is no legitimate
    //   reason for a literal question mark — the payload contains
    //   none — so any '?' here is a substitution for an unrenderable
    //   byte.
    let fffd = region.chars().filter(|c| *c == '\u{fffd}').count();
    if fffd > 0 {
        eprintln!("FAIL: U+FFFD REPLACEMENT CHARACTER appeared {fffd} times in printf output region — bad UTF-8 decode somewhere.");
        std::process::exit(1);
    }
    let qmarks = region.chars().filter(|c| *c == '?').count();
    if qmarks > 0 {
        eprintln!("FAIL: '?' appeared {qmarks} times in printf output region — likely '?' substitution for unrenderable bytes.");
        eprintln!("The payload contains no literal '?', so any '?' here is a substitution.");
        std::process::exit(1);
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
