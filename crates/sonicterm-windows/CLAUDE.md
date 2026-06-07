# sonicterm-windows

## Purpose
Windows binary and Win32-only glue: ConPTY process host, `muda` menu,
DWM/Mica backdrop, OLE drag/drop, CLI handling, and WiX packaging assets.
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
- ConPTY resize returns an HRESULT; surface failures instead of ignoring.
- Mica/backdrop changes must run after the HWND exists and is shown.
- OLE drag/drop initialization must stay on the window thread.
- Keep packaging paths in sync with `wix/main.wxs` and release workflow.

## Cross-references
- Consumes: `sonicterm-app-core`, `sonicterm-app`, `sonicterm-cfg`,
  `sonicterm-io`, `sonicterm-logging`.
- Consumed by: Windows release packaging.
