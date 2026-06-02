//! E2E debug binary: spawn the user shell, feed it commands, and dump the
//! resulting grid to stdout. Lets us prove that PTY + VT parser + grid
//! produce visible characters independent of GUI rendering.
//!
//! Run with: `cargo run --example pty_dump -p sonicterm-core`

use std::time::{Duration, Instant};

use sonicterm_core::{
    grid::{CellFlags, Color, Grid},
    pty::PtyHandle,
    vt::Parser,
};

fn main() {
    let pty = PtyHandle::spawn_default_shell(
        80,
        24,
        sonicterm_core::pty::ShellSpawnOpts { clean_e2e: true },
    )
    .expect("spawn shell");
    let mut parser = Parser::new(Grid::new(80, 24));

    drain(&pty, &mut parser, 1500);
    println!("\n=== after shell start ===");
    dump(&parser);

    pty.in_tx.send(b"echo hello-from-sonic\r".to_vec()).unwrap();
    drain(&pty, &mut parser, 1500);
    println!("\n=== after `echo hello-from-sonic` ===");
    dump(&parser);

    pty.in_tx.send(b"ls --color=always /\r".to_vec()).unwrap();
    drain(&pty, &mut parser, 1500);
    println!("\n=== after `ls --color=always /` ===");
    dump(&parser);
    let n_colored = count_colored(&parser);
    println!("[color-cells] non-default fg cells: {n_colored}");
    if n_colored == 0 {
        eprintln!("FAIL: expected colored output from `ls --color=always`");
        std::process::exit(1);
    }

    // ANSI escape: bold + italic + underline + red
    let style_seq = b"printf '\\033[1;3;4;31mSTYLED\\033[0m plain\\n'\r";
    pty.in_tx.send(style_seq.to_vec()).unwrap();
    drain(&pty, &mut parser, 1500);
    println!("\n=== after styled printf ===");
    dump(&parser);
    let (n_bold, n_italic, n_underline) = count_styles(&parser);
    println!("[style-cells] bold={n_bold} italic={n_italic} underline={n_underline}");
    if n_bold == 0 || n_italic == 0 || n_underline == 0 {
        eprintln!("FAIL: expected at least one bold + italic + underline cell");
        std::process::exit(1);
    }

    pty.in_tx.send(b"exit\r".to_vec()).unwrap();
    drain(&pty, &mut parser, 500);
    println!("\n[e2e] OK");
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

fn count_colored(parser: &Parser) -> usize {
    let mut n = 0;
    for r in 0..parser.grid().rows {
        for cell in parser.grid().row(r) {
            if !matches!(cell.fg, Color::Default) {
                n += 1;
            }
        }
    }
    n
}

fn count_styles(parser: &Parser) -> (usize, usize, usize) {
    let (mut b, mut i, mut u) = (0, 0, 0);
    for r in 0..parser.grid().rows {
        for cell in parser.grid().row(r) {
            if cell.flags.contains(CellFlags::BOLD) {
                b += 1;
            }
            if cell.flags.contains(CellFlags::ITALIC) {
                i += 1;
            }
            if cell.flags.contains(CellFlags::UNDERLINE) {
                u += 1;
            }
        }
    }
    (b, i, u)
}
