# sonicterm-app

## Purpose
Cross-platform application glue around `sonicterm-app-core`. This crate
owns the winit `ApplicationHandler`, window lifecycle, keymap dispatch,
PTY thread wiring, redraw scheduling, config hot reload, overlays, tab
drag/tear-out, and the platform shell abstractions.

## Key files
- `src/app/mod.rs` - `App` state and window/pane orchestration.
- `src/app/window_event.rs` - keyboard, mouse, IME, search, READONLY routing.
- `src/app/keymap_dispatch.rs` - action execution and READONLY whitelist.
- `src/app/event_loop.rs` - window creation and window-ready hooks.
- `src/app/spawn_pane.rs` - PTY thread pump and redraw coalescing.
- `src/app/tab_transfer.rs`, `tear_out.rs`, `child_window.rs` - tab movement.
- `src/config_watch.rs` - hot reload of `~/.snoicterm/sonicterm.toml`.
- `src/shell.rs` - `MacShell` and `WindowsShell` builders.

## Local gate
```bash
cargo build -p sonicterm-app
```

## Guardrails
- Render paths use `try_lock`, not blocking `lock`; avoid AB-BA deadlocks
  with PTY/parser work on the main thread.
- Keep PTY redraw coalescing burst-aware; never redraw per byte.
- Search input has priority over READONLY. In READONLY, only the explicit
  safe action whitelist may execute or reach the PTY.
- Do not add unconditional heartbeat redraws at the tail of event handling.
- Window-ready hooks fire once, immediately after winit creates the window.

## Cross-references
- Consumes: `sonicterm-app-core`, `sonicterm-vt`, `sonicterm-grid`,
  `sonicterm-io`, `sonicterm-cfg`, `sonicterm-render-model`,
  `sonicterm-ui`, `sonicterm-gpu`.
- Consumed by: `sonicterm-mac`, `sonicterm-windows`.
