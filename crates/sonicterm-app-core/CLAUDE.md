# sonicterm-app-core

## Purpose
Backend-free application state machine. This crate owns pure state,
reducers, effects, and intents; `sonicterm-app` translates those intents
into winit, GPU, clipboard, and PTY operations.

Keep this crate free of `winit`, `wgpu`, platform handles, and blocking IO.

## Key files
- `app_state.rs` - durable state owned by the reducer.
- `state_machine.rs` - mutation boundary driven by platform shells.
- `reducer.rs` - state transitions.
- `effect.rs` / `intent.rs` - commands emitted to the outer app layer.
- `supporting.rs` - small helper types shared by the reducer.

## Local gate
```bash
cargo build -p sonicterm-app-core
```

## Guardrails
- Add new behavior through reducer/effect boundaries instead of reaching
  back into `sonicterm-app`.
- Keep public types serializable/testable where practical; this is the
  easiest crate to unit-test without a window.
- If a public item is exposed through `sonicterm-types`, update
  `docs/CONTRACTS.md` in the same PR.

## Cross-references
- Consumes: `sonicterm-types`.
- Consumed by: `sonicterm-app`, `sonicterm-mac`, `sonicterm-windows`.
