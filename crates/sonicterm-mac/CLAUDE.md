# sonicterm-mac

## Purpose
macOS binary + macOS-only platform glue (NSMenu via objc, libproc-based
foreground process detection, OS-drag). `main.rs` is ~30 lines: loads
config + invokes `sonicterm_shared::run` (post-M7: `sonicterm_app::run`).

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
