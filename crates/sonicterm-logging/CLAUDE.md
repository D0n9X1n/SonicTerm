# sonicterm-logging

## Purpose
Panic hook, crash dumps, rolling log sinks, retention cleanup, log paths,
and exit tracing. Platform binaries initialize this before most startup
work so config-load failures and panics are visible.

## Key files
- `config.rs` - logging config schema/defaults.
- `sinks.rs` - tracing subscriber/log sink setup.
- `crash.rs` - panic hook and crash dump writing.
- `cleanup.rs` - retention cleanup.
- `exit_trace.rs` - signal/drop-guard exit markers.
- `path.rs` - `~/.sonicterm/logs` path helpers.

## Local gate
```bash
cargo test -p sonicterm-logging
```

## Guardrails
- Do not log secrets, tokens, environment dumps, or full command payloads
  without sanitization.
- Avoid holding logging locks across PTY or renderer operations.
- Init can happen only once; preserve the current bootstrap-then-user-config
  behavior in macOS and Windows binaries.

## Cross-references
- Consumed by: `sonicterm-mac`, `sonicterm-windows`, `sonicterm-app`.
