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
- `src/app/reaper_driver.rs` - one App-owned native PTY teardown driver and retained transport custody.
- `src/app/path_target.rs` - contextual target resolution, openability probes, and direct-open workers.
- `src/app/tab_transfer.rs` - pure GPU-free `TabContainer` transfer/reorder helper for tab movement tests.
- `src/app/tab_state.rs` - production `App` tab-state attach/detach helpers for main and child windows.
- `src/app/tear_out.rs` - native tear-out drag and child-window lifecycle.
- `src/app/shared_gpu.rs` - the live GPU context a New Window renderer shares.
- `src/app/child_window.rs` - child-window event routing, resizing, and PTY/VT wiring.
- `src/app/config_apply.rs` - explicit reload of `~/.sonicterm/sonicterm.toml`.
- `src/app/viewport_anchor.rs` - scrolled-back viewport anchor rebased across history eviction.
- `src/app/selection_gesture.rs` - local selection gestures bound to their press pane and anchor.
- `src/shell.rs` - shared shell runner with thin macOS, Windows, and Linux builders.

## Local gate
```bash
cargo build -p sonicterm-app
```

## Guardrails
- All pane destruction uses `retire_pane`; transfers preserve the PTY and its
  `ReapSlot`. Reserved native waits run only through the one `ReaperDriver`.
  Admission refusal retries once, then logs/counts synchronous fallback.
- Retired native custody owns one process-root `PtyTransport` and `ReaperWork`
  item until actual settlement or process-exit sink retention, independent of
  the former window/pane lifetime. `QueueFull` may last until process exit.
- `finish_session` retires every window's panes, including hidden main, before
  shutdown control. The shared shell calls it on both run outcomes. Preserve the
  original result; only actual teardown settlement permits a clean-session marker.
- Render paths use `try_lock`, not blocking `lock`; avoid AB-BA deadlocks
  with PTY/parser work on the main thread.
- Keep PTY redraw coalescing burst-aware; never redraw per byte. OSC 52 writes
  must stay bounded and reach the native clipboard only on the event-loop thread;
  clipboard reads/queries remain unsupported.
- Main and child keyboard, IME, focus and modifier events share source-WindowId
  ownership. Search input has priority over READONLY; quick-select retains its
  hint keys. In READONLY, only the explicit safe action whitelist may execute.
- Select the native drop owner before every main, warm, tear-out or new window
  is created. Failed registration must precede PTY startup or pane transfer.
- Every window after the first renders on the live GPU device: New Window through
  `App::shared_gpu_context`, warm-pool and tear-out windows through the main
  renderer's `shared_context`. Only a renderer built when none exists opens one.
- Do not add unconditional heartbeat redraws at the tail of event handling.
- A scrolled-back viewport is anchored to history identity. Writers repin through
  the pane's anchor setter with a baseline read under the lock that chose the row;
  readers resolve through the anchor, and both render collectors reconcile every
  held pane before reading `viewport_top_abs`, which stays a compatibility projection.
- A local selection drag belongs to its press pane: motion maps through that pane's
  rendered column edges and clamps to its addressable cells, a contended press or one
  on a cell the held grid lacks starts no gesture, and ownership is checked before any
  layout lookup, so a removed pane or tab, any tab switch, a screen change, an evicted
  anchor, or any real resize of the press pane's grid (`Grid::size_generation`) cancels
  the drag instead of retargeting it.
- Per-pane budgets do not impose a process quota; process and window owners
  are tracking-only. Inline media has a 256 MiB process target plus a possible
  4 MiB newest-image residual per live pane. Decode-time trimming and the
  idle-pane walk enforce distinct parts of that policy; tests must identify
  the mechanism they exercise, not just assert an aggregate bound.
- Reclamation that destroys something the user can see logs on
  `memory::reclaimed`, which is admitted at every level including the
  default. Diagnostics belong on `memory`, which is off unless someone is
  investigating.
- Window-ready hooks fire once, immediately after winit creates the window.
- Every terminal window enforces the shared 30-column by 10-row native inner-size
  floor from live renderer geometry and refreshes it after metric/DPI changes.
- Local-target hover never performs filesystem I/O on the event-loop thread.
  Clickability requires a current epoch-keyed typed navigation-or-reveal result.
  All platforms navigate directories and select files in their containing folder;
  local OSC 8 destinations must use the same probes, never the URI opener.
  Unverified bare names neither preview nor trigger failure notifications or clipboard writes.
  Explicit filepath failures show the path and reason on modifier-click without copying;
  a second modifier-click on the same failed path while its error is visible copies and
  reports the actual clipboard result. Only current validated targets dispatch native actions.
  File type, executable mode, and content never prevent reveal-only selection. macOS package
  directories are selected rather than launched. Hover never copies; native failures return
  only to the originating window/pane. Native dispatch revalidates identity and kind,
  retaining symlink/reparse-point, locality, and special-file protections.
- Contextual terminal candidates, including names containing ordinary spaces,
  resolve only against the exact pane's trustworthy local OSC 7 CWD, after OSC 8,
  URI, and explicit-path precedence; never fall back to process CWD, another pane,
  or HOME. Candidate enumeration and background probes stay explicitly bounded.
- Wrapped local targets join only recorded automatic wraps, at most eight visible
  rows and 4 KiB. Authorization binds every row hash/wrap bit, ordered absolute
  spans, pointed cell, viewport, screen epoch, eviction generation, and pane CWD;
  hard lines, incomplete chains, unsafe cells, or any identity change fail closed.

## Cross-references
- Consumes: `sonicterm-app-core`, `sonicterm-vt`, `sonicterm-grid`,
  `sonicterm-io`, `sonicterm-cfg`, `sonicterm-render-model`,
  `sonicterm-ui`, `sonicterm-gpu`.
- Consumed by: `sonicterm-mac`, `sonicterm-windows`, `sonicterm-linux`.
