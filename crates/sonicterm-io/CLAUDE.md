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
- PTY/ConPTY resize can fail, and the callback returns `anyhow::Result<()>` so
  the caller sees it. A zero column or row count is refused as an `InvalidInput`
  error before the native call. Only a successful native call caches the applied
  size, so a failed request is not deduplicated away: the next identical request
  reaches the native call again. Nothing retries automatically. Do not discard
  the error in new code, and do not cache a size the native call rejected.
- Keep platform-specific details behind this crate so app/UI code stays
  cross-platform.
- Unix automatic shell selection is executable `$SHELL`, then the current
  user's executable passwd shell, then `/bin/sh`; explicit config still wins.

## Cross-references
- Consumed by: `sonicterm-app`. Platform binaries reach it through
  `sonicterm-app`.
