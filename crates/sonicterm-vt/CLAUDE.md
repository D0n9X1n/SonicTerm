# sonicterm-vt

## Purpose
VT/ANSI parser. Wraps `vte::Parser` + a SWAR ASCII fast-path. Drives
the grid via the `Performer` trait. Pure-data; no I/O, no GPU.

## Public surface
- `vt::Parser`, `vt::Performer`
- Trait seams live in `sonicterm-types`.

## Land-mines specific to this crate
- **LM-005** CSI `J` (ED) and `K` (EL) MUST honor mode param.
  J0=below, J1=above, J2=all.
  required test: `shell_prompt_redraw_preserves_above_cursor`
  ref: CLAUDE.md §4 / landmines.toml
- **LM-006** CSI `?1049h` MUST be a no-op when already in alt screen
  (vim/fzf re-entry clobbers `saved_cursor` otherwise).
  required test: `dec_1049h_repeated_does_not_clobber_saved_cursor`
  ref: CLAUDE.md §4 / landmines.toml

## Test gate (local)
```bash
cargo build -p sonicterm-vt
```

## Common pitfalls
- Forgetting the SWAR fast-path's ASCII boundary check on emoji ZWJ joiners
- Adding a CSI handler without a matching test in `tests/vt.rs` or `tests/alt_screen.rs`

## Owning PM(s)
- Primary: either (cross-platform pure-data)
- Hot-file: yes (vt.rs)

## Cross-references
- Consumes traits from: `sonicterm-types`
- Consumed by: `sonicterm-grid`, `sonicterm-app`
