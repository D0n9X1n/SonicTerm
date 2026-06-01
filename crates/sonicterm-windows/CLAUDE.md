# sonicterm-windows

## Purpose
Windows binary + Windows-only platform glue: ConPTY, `muda` menu, Mica
backdrop, OLE drag-drop. `main.rs` is ~30 lines: loads config + invokes
`sonicterm_shared::run` (post-M7: `sonicterm_app::run`).

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
# §13 GUI smoke is the WINDOWS PM's responsibility — must be run on win-PM
```

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
