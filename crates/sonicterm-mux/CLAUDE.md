# sonicterm-mux

## Purpose
Persistent PTY multiplexer daemon. It owns long-lived PTY sessions outside
the GUI process and frames protocol messages for attach/reattach paths.

## Key files
- `main.rs` - daemon entry point.
- `server.rs` - session server loop.
- `proto.rs` - client/server protocol types.
- `frame.rs` - framing helpers.
- `lib.rs` - public module exports.

## Local gate
```bash
cargo build -p sonicterm-mux
```

## Guardrails
- The daemon owns long-lived PTYs; clean up on signal, disconnect, and
  explicit shutdown.
- Reattach must preserve terminal modes and alt-screen state, not only the
  primary grid.
- Avoid user-global socket collisions; namespace IPC paths by user/session.

## Cross-references
- Consumes: `sonicterm-io` — PTY spawn/read/write/resize and kill-on-Drop go
  through `sonicterm_io::pty::PtyHandle`, the same seam the GUI panes use, so
  there is one PTY implementation. The `sonicterm-grid`/`sonicterm-vt` seams
  are still planned (mux currently forwards raw bytes, no server-side parse).
- Consumed by: external daemon clients and future app attach flows.
