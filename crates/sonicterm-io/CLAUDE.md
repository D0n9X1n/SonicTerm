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
cargo clippy -p sonicterm-io --features ssh --all-targets -- -D warnings
```

The second command is not redundant. `ssh` is an optional feature and the
workspace default feature set is empty, so `--workspace --all-targets` never
compiles `ssh.rs`. Without an explicit `--features ssh` the backend can stop
building against a dependency's newer API while every other gate stays green.

## Guardrails
- `PtyHandle::Drop` must clean up child PTYs/conhosts; orphan processes are
  release blockers.
- Never hold parser/grid locks while writing to the PTY.
- PTY/ConPTY resize can fail. The current callback cannot return that error;
  make failures observable before adding more resize paths, and do not silently
  discard errors in new code.
- Keep platform-specific details behind this crate so app/UI code stays
  cross-platform.

## Cross-references
- Consumed by: `sonicterm-app`, `sonicterm-mux`. Platform binaries reach it
  through `sonicterm-app`.
