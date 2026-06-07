# sonicterm-io

## Purpose
Terminal process IO: PTY abstraction, foreground process detection,
process information, and SSH-related seams.

## Key files
- `pty.rs` - PTY handle and platform process boundary.
- `foreground_proc.rs` - foreground command detection.
- `proc_info.rs` - process metadata helpers.
- `ssh.rs` - SSH integration seams.
- `lib.rs` - public exports.

## Local gate
```bash
cargo build -p sonicterm-io
```

## Guardrails
- `PtyHandle::Drop` must clean up child PTYs/conhosts; orphan processes are
  release blockers.
- Never hold parser/grid locks while writing to the PTY.
- Keep platform-specific details behind this crate so app/UI code stays
  cross-platform.

## Cross-references
- Consumed by: `sonicterm-app`, `sonicterm-mux`, platform binaries.
