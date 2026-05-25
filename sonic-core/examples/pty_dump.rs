//! E2E debug binary: spawn the user shell, feed it commands, and dump the
//! resulting grid to stdout. Lets us prove that PTY + VT parser + grid
//! produce visible characters independent of GUI rendering.
//!
//! Run with: `cargo run --example pty_dump -p sonic-core`

use std::time::{Duration, Instant};

use sonic_core::{
    grid::{CellFlags, Grid},
    pty::PtyHandle,
    vt::Parser,
};

fn main() {
    let pty = PtyHandle::spawn_default_shell(80, 24).expect("spawn shell");
    let mut parser = Parser::new(Grid::new(80, 24));

    drain(&pty, &mut parser, 1500);
    println!("\n=== after shell start ===");
    dump(&parser);

    pty.in_tx.send(b"echo hello-from-sonic\r".to_vec()).unwrap();
    drain(&pty, &mut parser, 1500);
    println!("\n=== after `echo hello-from-sonic` ===");
    dump(&parser);

    pty.in_tx.send(b"ls /\r".to_vec()).unwrap();
    drain(&pty, &mut parser, 1500);
    println!("\n=== after `ls /` ===");
    dump(&parser);

    pty.in_tx.send(b"exit\r".to_vec()).unwrap();
    drain(&pty, &mut parser, 500);
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
