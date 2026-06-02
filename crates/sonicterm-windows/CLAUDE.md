# sonicterm-windows

## Purpose
Windows binary + Windows-only platform glue: ConPTY, `muda` menu, Mica
backdrop, OLE drag-drop. `main.rs` is ~30 lines: loads config, builds
`sonicterm_app_core::AppStateMachine`, then runs
`sonicterm_app::shell::WindowsShell`.

## Public surface
- `main` — bin entry point
- `os_drag_win` — OLE drag-drop (§5 exception: bin-only, no `tests/`)

## Land-mines specific to this crate
None named in §4. ConPTY corollary of LM-007 (`PtyHandle::Drop`) lives
in `sonicterm-io` — orphan conhost.exe processes if not killed
explicitly.

## Test gate (local)
```bash
cargo test -p sonicterm-windows
cargo test -p sonicterm-windows --test assets_colocated  # PR #453
cargo test -p sonicterm-windows --test wix_manifest      # PR #453
# §13 GUI smoke is the WINDOWS PM's responsibility — must be run on win-PM
```
PR #453: `build.rs` colocates icon/asset files next to the binary;
the two tests above guard that contract + the WiX manifest layout.

## Common pitfalls
- `ResizePseudoConsole` returns an HRESULT — don't ignore
- Mica backdrop must be applied AFTER the window is shown
- OLE drop targets need `OleInitialize` on the same thread as the window

## Owning PM(s)
- Primary: **win-PM** (only this PM can §13 on Windows)
- Hot-file: yes — bin entry plus Windows-only paths

## Cross-references
- Consumes: `sonicterm-app` (post-M6: `sonicterm-app-core` + `sonicterm-app`)
- Consumed by: nothing (bin)
