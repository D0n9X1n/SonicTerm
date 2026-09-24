# sonicterm-app-core

## Purpose
Backend-free application state machine. This crate owns pure state,
reducers, effects, and intents. `sonicterm-app` observes intents through the
reducer and discards the effects; it owns the live topology and performs
winit, GPU, clipboard, and PTY operations through its own explicit-target
paths.

Keep this crate free of `winit`, `wgpu`, platform handles, and blocking IO.

## Key files
- `app_state.rs` - durable state owned by the reducer.
- `state_machine.rs` - mutation boundary driven by platform shells.
- `reducer.rs` - state transitions.
- `effect.rs` / `intent.rs` - the intent and effect vocabulary; the app
  observes it rather than executing effects.
- `supporting.rs` - small helper types shared by the reducer.

## Local gate
```bash
cargo build -p sonicterm-app-core
```

## Guardrails
- Put operational behavior in the app's explicit-target paths, not in
  reducer effects: the app observes the reducer and does not execute its
  effects. Keep effect values and ordering stable, because observers and
  tests depend on them, and do not reach back into `sonicterm-app`.
- Keep public types serializable/testable where practical; this is the
  easiest crate to unit-test without a window.
- If a public item is exposed through `sonicterm-types`, review the cross-crate
  contract in `Architecture-Internals` and update affected documentation.

## Cross-references
- Consumes: `sonicterm-types`.
- Consumed by: `sonicterm-app`, `sonicterm-mac`, `sonicterm-windows`,
  `sonicterm-linux`.
