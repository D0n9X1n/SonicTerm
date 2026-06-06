# sonicterm-mux

## Purpose
Persistent PTY multiplexer daemon (shipped v0.8, #56). Survives the GUI
process so reattach is instant; feature-gated remote attach is post-v1.0.

## Public surface
- daemon entry + IPC protocol (TBD when remote attach lands)

## Land-mines specific to this crate
None named in §4. LM-007 corollary applies: the daemon owns long-lived
PTYs and must clean up on signal / disconnect.

## Test gate (local)
```bash
cargo build -p sonicterm-mux
```

## Common pitfalls
- IPC socket path collision between user sessions
- Reattach must restore alt-screen state, not just the primary grid

## Owning PM(s)
- Primary: either; mac-PM has the bulk of the daemon plumbing
- Hot-file: no (low traffic)

## Cross-references
- Consumes: `sonicterm-io`, `sonicterm-grid`, `sonicterm-vt`
- Consumed by: external (daemon bin)
