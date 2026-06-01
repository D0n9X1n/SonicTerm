# sonicterm-app-core

## Purpose
Winit-agnostic application state machine. Pure-data — no winit, wgpu,
arboard, or any backend. The platform layer (`sonicterm-app`) drives
this crate and translates `AppIntent`s into winit/wgpu/arboard calls.

Introduced as ADDITIVE at M6a; consumers migrate over M6b..d.

## Public surface
- `AppState`, `AppStateBuilder` — the (currently minimal) state holder
- `AppIntent` — what the app-core asks the platform layer to do
- `RedrawReason` — coalescing hint for LM-002

## Land-mines specific to this crate
None yet — the platform-side landmines (LM-001, LM-002, LM-003, LM-004)
remain in `sonicterm-app` until the state extraction completes
post-modularization pilot.

## Test gate (local)
```bash
cargo test -p sonicterm-app-core
```

## Common pitfalls
- Adding a `winit::*` or `wgpu::*` type breaks the swappability promise
- Don't store backend handles here — those belong in `sonicterm-app`

## Owning PM(s)
- Primary: either (cross-platform pure-data)
- Hot-file: no (additive, M6a scope)

## Cross-references
- Consumes: `sonicterm-types`
- Consumed by: `sonicterm-app` (M6b+), `sonicterm-mac` (M6b),
  `sonicterm-windows` (M6c)
