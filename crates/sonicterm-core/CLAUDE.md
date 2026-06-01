# sonicterm-core

## 💀 DEPRECATED FAÇADE — removal target: v1.1

This crate exists only as a back-compat re-export shim. New code MUST
depend on the leaf crate directly. See `docs/migrations/0.9.0.md` for
the mapping.

## Re-exports (as of v0.9)
- `sonicterm_vt::vt`
- `sonicterm_grid::{grid, hyperlink}`
- `sonicterm_cfg::{config, theme, keymap, url_open}`
- `sonicterm_io::{pty, proc_info, ssh, foreground_proc}`

Every `pub use` carries
`#[deprecated(since = "0.9.0", note = "use sonicterm_<leaf>::*")]` (M9).

## Tests
The crate still hosts the historical capability matrices used by §11:
- `tests/vt_capability_matrix.rs`
- The two `pty_dump` and `pty_dump_unicode` examples
These move to their owner crates before this crate is deleted at v1.1.

## Owning PM(s)
- Primary: tag-owner during v1.1 release window (deletes the crate)

## Cross-references
- See `docs/migrations/0.9.0.md` for the leaf crate each item moved to
