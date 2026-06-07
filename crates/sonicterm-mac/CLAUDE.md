# sonicterm-mac

## Purpose
macOS binary and AppKit-only glue. It loads config/theme/keymap, installs
logging, configures the native menu, disables AppKit's native tab strip for
this process, wires OS tab drag/drop, then runs `sonicterm_app::MacShell`.

## Key files
- `main.rs` - startup, asset lookup, menu hooks, window-ready marker.
- `menubar.rs` - NSMenu construction and selector bridge.
- `os_drag_mac.rs` - NSPasteboard/drag payload handoff.
- `tab_drag_os.rs` - macOS tab tear-out/drop backend.
- `lib.rs` - macOS module exports.

## Local gate
```bash
cargo build -p sonicterm-mac
```

Render/input/VT/window changes need a macOS GUI smoke. Prefer
`just visual mac`; verify SonicTerm is frontmost before keystrokes and use
window-local `screencapture -l`.

## Guardrails
- Keep AppKit automatic tabbing disabled with
  `NSWindow.setAllowsAutomaticWindowTabbing(false)` plus per-window
  `setTabbingMode: 2`; SonicTerm draws its own tab bar.
- Install the native menu after winit creates the AppKit event loop.
- Keep Objective-C calls on the main thread and inside the expected
  autorelease lifetime.
- Bundled assets load from `Contents/Resources/assets`; dev runs fall back
  to workspace `assets/`.

## Stable contract
`main.rs` prints this stdout marker when winit hands over the AppKit window:

```text
SONICTERM_WINDOW_READY cg_window_id=<u32> pid=<u32> window_index=0
```

Automation feeds `cg_window_id` to `screencapture -l`. Do not rename,
reorder, or repurpose the keys without updating callers in the same commit.

## Cross-references
- Consumes: `sonicterm-app-core`, `sonicterm-app`, `sonicterm-cfg`,
  `sonicterm-logging`.
- Consumed by: release packaging and macOS smoke automation.
