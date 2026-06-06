# sonicterm-app

## Purpose
Winit `ApplicationHandler` + platform glue. The orchestrator: receives
winit events, dispatches keymap actions, owns per-pane PTY threads,
schedules redraw, runs overlays. Split across `app/{mod,window_event,
event_loop,spawn_pane,keymap_dispatch,key_encoding,input,redraw,
overlays,tab_state,tear_out,child_window,config_apply,search_handle,
misc}.rs` (#158, #160).

Post-M6: state machine extracted to `sonicterm-app-core`; this crate is
the winit-specific wiring around it.

## Public surface
- `app::App` (the ApplicationHandler impl)
- `menu`, `os_drag`, `tab_drag`, `config_watch`

## Land-mines specific to this crate
- **LM-001** Render path uses `try_lock`, not `lock`. AB-BA deadlock on
  the macOS main thread otherwise. `window_event.rs` (lines ~143/162),
  `child_window.rs`, `misc.rs`.
- **LM-002** PTY-thread redraw coalescer = **3 ms min + 128 KB byte
  flush**. Never per-byte redraw. `spawn_pane.rs` (~line 76).
- **LM-003** PTY burst flag is a **generation counter**, not a bool.
  See `window_event.rs` ~line 34. PR #162 fixed the race.
- **LM-004** No unconditional heartbeat redraw at end of `window_event` —
  feedback loop.

## Test gate (local)
```bash
cargo build -p sonicterm-app
```
PR #454 added `WindowState::clear_drag_chip()` helper in `src/app/mod.rs`
to clean up the drag chip after OS drag completion; `tests/os_drag_cleanup.rs`
guards the cleanup invariant.

### §13 GUI smoke (must include RED-BG + CJK + emoji)

Prefer `just visual mac` — it runs the hardened harness (focus-verify,
window-local screencap, multi-PID tracking — see issue #464). The ad-hoc
snippet below is for one-off checks; it follows the same focus-verify
pattern so a stray focus loss can't silently leak keystrokes.

```bash
pkill -9 -f sonicterm-mac 2>/dev/null; sleep 0.3
./target/release/sonicterm-mac > /tmp/gui-smoke.log 2>&1 &
SONIC_PID=$!
sleep 2.5

# Position window + verify frontmost BEFORE any keystroke.
osascript >/dev/null 2>&1 <<'EOF'
tell application "System Events"
  tell process "sonicterm-mac"
    set frontmost to true
    set position of window 1 to {500, 200}
    set size of window 1 to {1000, 700}
  end tell
end tell
EOF
sleep 0.3
front=$(osascript -e 'tell application "System Events" to name of first process whose frontmost is true')
[[ "$front" == "sonicterm-mac" ]] || { echo "ABORT: front=$front (would leak); not running keystrokes"; kill -9 $SONIC_PID; exit 1; }

osascript -e 'tell application "System Events" to keystroke "printf '"'"'\\033[41mRED-BG\\033[0m echo 中文 🎉\\n'"'"' && date"'
osascript -e 'tell application "System Events" to key code 36'
sleep 1

# Window-local screencap (avoids capturing whatever is behind sonicterm-mac).
WID=$(osascript -e 'tell application "System Events" to tell process "sonicterm-mac" to get id of window 1')
screencapture -x -l "$WID" /tmp/gui-smoke.png
kill -9 "$SONIC_PID"
```

Inspect: theme bg matches, RED-BG cells red, CJK glyphs render, emoji
color, cursor visible, sharp on Retina, CPU < 5%.

## Common pitfalls
- Adding a winit event handler without coalescing redraw → 100% idle CPU
- Holding parser lock across PTY write → AB-BA deadlock returns
- Forgetting `ControlFlow::WaitUntil` in `event_loop.rs` → busy loop

## Perf status (v1.0-RC, honest)
SonicTerm is **6×–302× slower than WezTerm** on `vtebench` depending on
workload. Don't describe perf work as "done" in commit messages.

## Window-ready hook (`with_on_window_ready`)
Both `MacShell` and `WindowsShell` expose a one-shot
`with_on_window_ready(Box<dyn FnOnce(RawWindowHandle) + Send>)` builder
method, plumbed into the cross-platform firing site at
`app/event_loop.rs` (`App::resumed`). Fired exactly once, the instant
winit's `create_window` returns. Per-platform usage today:

- **Windows**: muda menubar install + DWM backdrop (HWND-bound APIs).
- **mac (#554)**: prints the `SONICTERM_WINDOW_READY cg_window_id=…
  pid=… window_index=0` stdout marker for the test harness. Stable
  contract — see `crates/sonicterm-mac/CLAUDE.md`.

## Owning PM(s)
- Primary: cross-platform — first PM to claim a hot file blocks the other
- Hot-file: every file in `app/` is a hot file

## Cross-references
- Consumes: `sonicterm-app-core`, `sonicterm-vt`, `sonicterm-grid`,
  `sonicterm-io`, `sonicterm-cfg`, `sonicterm-render-model`, `sonicterm-ui`
- Consumed by: `sonicterm-mac`, `sonicterm-windows`
