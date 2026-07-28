//! Emit the frozen `pty-backend-v1` feasibility evidence to stdout.
//!
//! Single source of truth for the canonical bytes: it prints exactly
//! [`sonicterm_io::pty_backend_feasibility::render_canonical_evidence`] with no
//! extra framing, so `scripts/pty-backend-feasibility.sh` can hash the same
//! bytes with the system hasher and the owner can paste the artifact into the
//! coordination-ledger decision comment.

use std::io::Write;

fn main() {
    let evidence = sonicterm_io::pty_backend_feasibility::render_canonical_evidence();
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    lock.write_all(evidence.as_bytes()).expect("write feasibility evidence to stdout");
    lock.flush().expect("flush feasibility evidence");
}
