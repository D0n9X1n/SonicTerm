# sonicterm-io

## Purpose
Terminal process IO: PTY abstraction, foreground process detection, and
process information.

## Key files
- `pty.rs` - PTY handle and platform process boundary, including `spawn_command`.
- `command_resolve.rs` - program resolution for direct command launches.
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
- The writer flushes after every message. That costs nothing on either platform: the Unix
  writer is a `std::fs::File`, whose `flush` is a no-op, and the pinned Windows writer
  (`filedescriptor` 0.8.3, `windows.rs`) returns `Ok(())` without a call. The flush only marks a
  user-input message complete (`input_completed`), so keep it per message.
- The reply spool caps live spilled bytes at `SPILL_BUDGET_BYTES` (16 MiB). A reply past it fails
  with a `ReplyBudgetExceeded` error (`is_budget_refusal`), is dropped alone and counted (`PtyReplySender::stats`), and
  must not latch failure; every other spool error still latches. Compaction keeps the file below
  twice the budget plus one record. Every spool's file length also counts against one shared
  `SpillPool` (256 MiB, `PROCESS_SPILL_BUDGET_BYTES`), refused the same way. `Spill` owns its
  `SpillCharge`, declared last so the file closes before the refund; never add a release path
  outside it. Unit tests get a private unlimited pool from `reply_spool()`; a budget test passes
  its own to `reply_spool_with_pool`.
- `ShellSpawnOpts::input_seen` is set before any input message is admitted to the queue,
  through `send_input_nonblocking` or a `PtyInputSender`; only `send_draft_nonblocking` (a
  script draft typed without Enter) and the reply spool leave it clear. The parser stops
  latching a shell token once it is set, so a new input path must mark it too.
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
- `PtyHandle::spawn_command` runs a launcher's program with no shell. It
  resolves the program itself (Unix: `PATH` absolute entries, `/` paths against
  the launch cwd, a regular executable whose header is `#!` or native magic, so
  libc's `ENOEXEC` shell retry never runs; Windows: `.exe`/`.com` only, batch
  files refused) and hands `portable-pty` an absolute path. An invalid cwd is an
  error, never a home-directory fallback, and a failure never starts a shell.
  Error text names only the program's file name; native error text is mapped.
- The exit probe records the first observed `PaneExitStatus` (code, signal
  name or unknown); a later consuming wait does not overwrite it.

## Cross-references
- Consumed by: `sonicterm-app`. Platform binaries reach it through
  `sonicterm-app`.
