# sonicterm-mac

## Purpose
macOS binary and AppKit-only glue. It loads config/theme/keymap, installs
logging, configures the native menu, disables AppKit's native tab strip for
this process, wires OS tab drag/drop, then runs `sonicterm_app::MacShell`.

## Key files
- `main.rs` - startup, asset lookup, menu hooks, window-ready setup.
- `menubar.rs` - NSMenu construction and selector bridge.
- `open_documents.rs` - `kAEOpenDocuments` and `kAEGetURL` handler install; routes each
  classified file to a draft, a run or a refusal.
- `executable_classify.rs` - document-open classification: scripts never run; only a
  regular, unquarantined, executable Mach-O outside any `.app` bundle runs.
- `open_urls.rs` - `ssh:`/`x-man-page:` links to fixed `/usr/bin` clients, or a notice.
- `default_handlers.rs` - the "set as default" palette backend: reads the bundle's `Info.plist`
  claims, refuses a non-bundle, translocated, disk-image or unregistered copy, and calls the
  `NSWorkspace` setters on the main thread with completion blocks.
- `os_drag_mac.rs` - NSPasteboard/drag payload handoff.
- `tab_drag_os.rs` - macOS tab tear-out/drop backend.
- `lib.rs` - macOS module exports.

## Local gate
```bash
cargo build -p sonicterm-mac
```

## Guardrails
- Keep AppKit automatic tabbing disabled through the shared macOS resumed path's
  winit `set_allows_automatic_window_tabbing(false)` before window creation;
  retain the macOS binary's one-shot `setTabbingMode: 2` callback for the initial window.
- Install the native menu after winit creates the AppKit event loop.
- Keep Objective-C calls on the main thread and inside the expected
  autorelease lifetime.
- Bundled assets load from `Contents/Resources/assets`; dev runs fall back
  to workspace `assets/`.
- A file open never executes a script, never adds an execute bit, never removes
  quarantine and never retries through a shell. Classification errors fail closed.
- File and link logs carry reasons, schemes and field names, never a path or value.
- Never set SonicTerm as a Launch Services default at install, startup or reload; the bundle
  only declares Alternate-rank types and the `ssh`/`x-man-page` schemes. Only Apply on the
  default-handler palette command's confirmation page calls the `NSWorkspace` setters.
  Tests never call them; they run against pure functions and refuse as a missing bundle.

## Cross-references
- Consumes: `sonicterm-app-core`, `sonicterm-app`, `sonicterm-cfg`,
  `sonicterm-logging`, `sonicterm-types`.
- Consumed by: release packaging.
