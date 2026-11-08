# sonicterm-mcp

## Purpose
The stdio MCP bridge agents such as Claude Code and Copilot CLI start as a console
child process. SonicTerm's own binaries are GUI programs, so this small binary finds
the running instance's control endpoint (a Unix socket, or a Windows named pipe),
checks that its server runs as the same user, sends a one-line preamble naming the
caller's pane, and relays newline-delimited JSON-RPC both ways. The MCP server itself
lives in `sonicterm-app` (`src/app/mcp/`).

## Key files
- `main.rs` - argument parsing (`--endpoint`, `--version`, `--help`,
  `--shell-integration`), exit codes, and the discover-connect-relay sequence.
- `shell_integration.rs` - the fixed zsh, bash and PowerShell 7 OSC 133 snippets that read, unset and
  append the pane's `SONICTERM_SHELL_TOKEN`; the snippet never contains a token.
- `discovery.rs` - endpoint choice in fixed order: `--endpoint`, `SONICTERM_CONTROL`,
  then the only live entry in the per-user directory; the no-endpoint and
  several-instances errors.
- `connect_unix.rs` - the endpoint directory (`$XDG_RUNTIME_DIR/sonicterm/`, else
  `$TMPDIR/sonicterm-<uid>/`), peer-uid check, and pid liveness.
- `connect_windows.rs` - `%LOCALAPPDATA%\SonicTerm\control\<pid>` discovery records,
  the overlapped named-pipe client with deadlines, and the server token's SID check.
- `relay.rs` - the preamble, the bounded relay threads, and why a relay ended.

## Local gate
```bash
cargo test -p sonicterm-mcp
```
The end-to-end path against a real app is `python3 scripts/mcp-smoke.py`
(`mcp-smoke` and `windows-mcp-smoke` in the local gate).

## Guardrails
- stdout carries only lines the server sent. Diagnostics, usage errors, and discovery
  failures go to stderr; anything else on stdout corrupts the agent's JSON-RPC stream.
- Never guess between instances: zero or several live endpoints is an error that
  lists what was found, and the caller passes `--endpoint`.
- Check the server's user before sending any byte: the peer uid on Unix, the server
  process token's SID on Windows. A mismatch closes the connection unwritten.
- Keep the relay bounded: lines over 1 MiB end it, server output waits in a queue of
  at most 4 MiB, and a full queue that stdout leaves unread for 5 s ends it (exit 1). A closed
  endpoint ends the bridge only after stdout wrote every queued line, or after stdout took
  nothing for 2 s (exit 1); a closed stdout ends it at once. A client that keeps reading is
  never cut off. One rule decides both limits: the relay gives up only when stdout took no byte
  for the whole interval (5 s with the queue full, 2 s after the close). Writes to stdout are
  capped at 512 bytes on Windows and 16 KiB elsewhere. On macOS and Linux a pipe or socket stdout
  (Node's `child_process` uses a socket on macOS) is written nonblocking and retried every 50 ms
  (`stdout_unix.rs`), so each read the client makes shows up as progress at the transport's
  granularity: any read on a macOS pipe, a 4 KiB page on a Linux pipe, one 16 KiB write on a
  Linux socket.
- No first-party dependencies: only `libc` on Unix and `windows` on Windows, so the
  bridge stays small and builds beside every app binary.

## Cross-references
- Server: `crates/sonicterm-app/src/app/mcp/` (tools in `tools_read.rs` and
  `tools_control.rs`, endpoints in `endpoint_unix.rs` and `endpoint_windows.rs`).
- Packaged by: `scripts/macos-bundle.py`, `scripts/make-linux-packages.sh`, and
  `crates/sonicterm-windows/wix/main.wxs`.
- Wiki: [MCP Server](../../wiki/MCP-Server.md) (enabling the server, connecting an agent, tools),
  [Platform Integration](../../wiki/Platform-Integration.md) (endpoints), and
  [Local Gate](../../wiki/Local-Gate.md) (the MCP smoke).
