# sonicterm-windows

## Purpose
Windows binary and Win32-only GUI glue: `muda` menu, DWM/Mica backdrop,
OLE drag/drop, CLI handling, software presentation, and WiX packaging assets.
Local ConPTY process transport is provided through `sonicterm-io`.
It loads config/theme/keymap, installs logging, then runs
`sonicterm_app::WindowsShell`.

## Key files
- `main.rs` - startup and shell construction.
- `cli.rs` - Windows CLI entry handling.
- `chrome.rs`, `backdrop.rs` - window chrome and DWM backdrop.
- `menubar.rs` - native menu integration.
- `os_drag_win.rs`, `tab_drag_os.rs` - OLE drag/drop and tab transfer.

## Local gate
```bash
cargo build -p sonicterm-windows
```

Release MSI builds require the Windows Cairo setup script and WiX.

## Guardrails
- Keep ConPTY/process behavior behind `sonicterm-io`; this crate owns GUI-only
  Win32 integration.
- Mica/backdrop changes must run after the HWND exists and is shown.
- OLE drag/drop initialization must stay on the window thread.
- Keep packaging paths in sync with `wix/main.wxs`, `docs/packaging/`, and the
  release workflow.

## Cross-references
- Consumes: `sonicterm-app-core`, `sonicterm-app`, `sonicterm-cfg`,
  `sonicterm-logging`; local PTY/ConPTY behavior is reached through
  `sonicterm-app` and `sonicterm-io`.
- Consumed by: Windows release packaging.
