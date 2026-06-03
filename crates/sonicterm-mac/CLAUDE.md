# sonicterm-mac

## Purpose
macOS binary + macOS-only platform glue (NSMenu via objc, libproc-based
foreground process detection, OS-drag). `main.rs` is ~30 lines: loads
config, builds `sonicterm_app_core::AppStateMachine`, then runs
`sonicterm_app::shell::MacShell`.

## Public surface
- `main` — bin entry point
- `menubar` — NSMenu integration (private items per §5 exception)
- `os_drag` — OS-drag glue

## Land-mines specific to this crate
None named in §4 — but this crate is the only place §13 GUI smoke can
be run from the mac PM. **Every render/input/VT/window-state PR must
include a §13 smoke screenshot from here.**

## Test gate (local)
```bash
cargo test -p sonicterm-mac
cargo build --release -p sonicterm-mac
# Then §13 GUI smoke — see crates/sonicterm-app/CLAUDE.md
```

## Common pitfalls
- objc autorelease pool not held across NSMenu calls → segfault
- `register`/`lookup`/`scan_themes` in `menubar.rs` are private and
  tested via the `main` path; do not extract to a `tests/` folder
  (§5 exception — small macOS-only surface)

## Owning PM(s)
- Primary: **mac-PM** (only this PM can §13)
- Hot-file: yes — bin entry plus mac-only paths

## Cross-references
- Consumes: `sonicterm-app` (post-M6: `sonicterm-app-core` + `sonicterm-app`)
- Consumed by: nothing (bin)

## Stable contract: `SONICTERM_WINDOW_READY` stdout marker (#554)
The bin prints one line to **stdout** (NOT tracing — the harness greps
raw stdout) the instant winit hands AppKit the window, from the
`with_on_window_ready` hook in `main.rs`:

```
SONICTERM_WINDOW_READY cg_window_id=<u32> pid=<u32> window_index=0
```

- `cg_window_id` — CoreGraphics window number from `[NSWindow windowNumber]`.
  Directly feedable to `screencapture -l <id>`. `-1` means the NSView
  had no attached NSWindow (should not happen post-`create_window`;
  harness must treat as missing and fall back).
- `pid` — `std::process::id()` of the mac bin, for cross-checking.
- `window_index=0` — reserved for future torn-out child windows;
  always `0` today.

The harness (`testing/workflows/run_case.sh`) greps this marker to
skip the ~3s `cg-window-id.swift` poll. Mirrors the Windows path —
**do not rename, reorder, or repurpose the keys** without bumping
both run_case.sh and run_case.ps1 in the same commit.
