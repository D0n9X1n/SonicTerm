# sonicterm-app

## Purpose
Cross-platform application glue around `sonicterm-app-core`. This crate
owns the winit `ApplicationHandler`, window lifecycle, keymap dispatch,
PTY thread wiring, redraw scheduling, explicit config reload, overlays, tab
drag/tear-out, and the platform shell abstractions.

## Key files
- `src/app/mod.rs` - `App` state and window/pane orchestration.
- `src/app/window_event.rs` - keyboard, mouse, IME, search, READONLY routing.
- `src/app/keymap_dispatch.rs` - action execution and READONLY whitelist.
- `src/app/event_loop.rs` - window creation and window-ready hooks.
- `src/app/spawn_pane.rs` - PTY thread pump and redraw coalescing.
- `src/app/path_target.rs` - contextual target resolution, openability probes, and direct-open workers.
- `src/app/tab_transfer.rs`, `tear_out.rs`, `child_window.rs` - tab movement.
- `src/app/config_apply.rs` - explicit reload of `~/.sonicterm/sonicterm.toml`.
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
- Per-pane budgets do not bound a session. The inline-media ceiling is
  process-wide and each pane's share scales with live pane count; two
  independent mechanisms hold the total — the over-ceiling floor for panes
  that are decoding, and the idle-pane walk for the rest. Removing either
  alone still leaves the total bounded, so a test must assert the mechanism
  it means to cover, not just the bound.
- Reclamation that destroys something the user can see logs on
  `memory::reclaimed`, which is admitted at every level including the
  default. Diagnostics belong on `memory`, which is off unless someone is
  investigating.
- Window-ready hooks fire once, immediately after winit creates the window.
- Local-target hover never performs filesystem I/O on the event-loop thread.
  Clickability requires a current epoch-keyed typed openability result. Direct-open
  work stays bounded, revalidates target kind, and blocks executable/launcher,
  symlink/reparse-point, and special-file classes before native dispatch.
- Bare terminal tokens resolve only against the exact pane's trustworthy local
  OSC 7 CWD, after OSC 8, URI, and explicit-path precedence; never fall back to
  process CWD, another pane, or HOME.

## Cross-references
- Consumes: `sonicterm-app-core`, `sonicterm-vt`, `sonicterm-grid`,
  `sonicterm-io`, `sonicterm-cfg`, `sonicterm-render-model`,
  `sonicterm-ui`, `sonicterm-gpu`.
- Consumed by: `sonicterm-mac`, `sonicterm-windows`.
