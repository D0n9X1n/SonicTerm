# sonicterm-io

## Purpose
PTY + process probes + SSH. Owns the platform PTY abstraction (mac
`portable-pty`, win `conpty`) and the foreground-process detection used
for prompt-aware features.

## Public surface
- `pty::PtyHandle` — implements `sonicterm_types::PtyTransport` (M4+)
- `proc_info`
- `foreground_proc` (Windows-only)
- `ssh` (feature-gated)

## Land-mines specific to this crate
- **LM-007** `PtyHandle::Drop` MUST kill the child explicitly. Just
  dropping the trait object doesn't terminate the shell — orphans
  accumulate per pane.
  ref: `crates/sonicterm-io/src/pty.rs` — caught by Haiku review of PR #21

## Test gate (local)
```bash
cargo test -p sonicterm-io
cargo run --example pty_dump -p sonicterm-core --release   # must print [e2e] OK
```

## Common pitfalls
- Not setting `CLOEXEC` on the master fd → fd leaks into child shells
- Resize race: must signal the child via `TIOCSWINSZ` (mac) or
  `ResizePseudoConsole` (win), not just resize the buffer
- `foreground_proc::snapshot_processes` is private; expose only via
  `pub use` in `lib.rs` and document under §5 exceptions in CLAUDE.md

## Owning PM(s)
- Primary: split — mac-PM owns mac path, win-PM owns ConPTY path
- Hot-file: pty.rs (cross-platform regressions easy to ship)

## Cross-references
- Consumes traits from: `sonicterm-types::PtyTransport`
- Consumed by: `sonicterm-app::spawn_pane`
