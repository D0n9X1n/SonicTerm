# sonicterm-logging

## Purpose
Panic hook + rolling-file logger. Initialized at the very top of every
binary's `main()` so even bootstrap errors land in:
- macOS: `~/Library/Logs/SonicTerm/sonicterm.log.*`
- Windows: `%LOCALAPPDATA%\SonicTerm\Logs\sonicterm.log.*`

Retention: ~60 MB rolling + 10 crash dumps. Full spec: `docs/LOGGING.md`.

## Public surface
- `init()` — call before anything else in `main`
- `panic_hook` — installed by `init`

## Land-mines specific to this crate
- **No raw `process::exit` in shipped code** (enforced by
  `scripts/check-no-raw-process-exit.sh`). All exits route through
  the logger so the last frame is captured.

## Test gate (local)
```bash
cargo test -p sonicterm-logging
bash scripts/check-no-raw-process-exit.sh
```

## Common pitfalls
- Initializing logging AFTER config load — bootstrap errors lost
- Logging a `Debug` value of a struct holding secrets — sanitize first
- Holding the log lock across PTY writes — same lock-ordering hazard as
  LM-001 but for the writer thread

## Owning PM(s)
- Primary: either
- Hot-file: no (additive)

## Cross-references
- Consumed by: every bin (`sonicterm-mac`, `sonicterm-windows`, `sonicterm-mux`)
