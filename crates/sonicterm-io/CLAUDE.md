# sonicterm-io

## Purpose
Terminal process IO: PTY abstraction, foreground process detection, and
process information.

## Key files
- `pty.rs` - PTY handle and platform process boundary.
- `foreground_proc.rs` - foreground command detection.
- `proc_info.rs` - process metadata helpers.
- `lib.rs` - public exports.

## Local gate
```bash
cargo build -p sonicterm-io
```

## Guardrails
- `PtyHandle::into_teardown` transfers owned native cleanup without native calls;
  production App retirement uses its one reaper driver. Direct fixture drops keep
  bounded inline cleanup. Orphan PTYs/conhosts are release blockers.
- `PtyTeardown` retains unfinished native values, permit wrappers and worker join
  handles. Only successful whole-phase completion and joined workers settle it.
  Explicit failures return `Failed`; wait expiry returns `TimedOut`. Neither
  outcome authorizes native destruction or premature accounting release.
- Windows `CancelSynchronousIo` runs only on workers owning actual duplicated
  thread handles. Reserved workers use a whole grant; slotless fallback uses
  `cancel_io_within` and opaque duplicate tokens. Both share one 500 ms cancel
  deadline and retain unfinished workers. `ERROR_NOT_FOUND` means no pending IO;
  other errors are failed phases. Never call cancellation on the dropping thread.
- Native process termination checks the same retained child handle, never a PID
  reopen. Failed termination leaves the leader unreaped. Drain and close run
  concurrently, with 2 s waits; failed drain admission preserves the master.
- The master-side input writer is built by `pty_writer`, never by calling
  `MasterPty::take_writer` at the spawn site. A writer's destructor is part of
  the child's input stream, so one seam decides it per platform. On Unix that
  is a `std::fs::File` over an `F_DUPFD_CLOEXEC` duplicate of the master
  descriptor, whose close is silent; `portable-pty`'s Unix writer instead
  writes a newline and `VEOF` when dropped, which reaches the child as
  synthetic input no source produced. Destroying the writer must add nothing
  to the child's input stream; ordinary terminal input and parser-generated
  replies are unaffected. The duplicate shares the master's open file
  description, so
  file-status flags such as `O_NONBLOCK` stay shared, while `FD_CLOEXEC` is
  per-descriptor and set on the duplicate alone. A Unix master exposing no
  descriptor is an error — never fall back to a writer that injects bytes.
  Windows keeps `take_writer`.
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
