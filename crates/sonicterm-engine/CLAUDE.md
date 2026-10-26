# sonicterm-engine

## Purpose
Small engine seam for font-facing data that is not terminal state.
Today it exposes `FontStack` and `CellMetricsPx` while the font pipeline
continues moving into Sonic-owned crates.

Terminal parsing and grid state live in `sonicterm-vt` and
`sonicterm-grid`, not here.

## Key files
- `fontstack.rs` - font stack selection and cell metrics. Each stack memoizes its four
  bold/italic faces until the shared configuration's `face_epoch` moves; clones and
  `with_font_size` views keep their own memo against that one epoch.
- `lib.rs` - public re-exports.

## Local gate
```bash
cargo build -p sonicterm-engine
```

## Guardrails
- Do not grow this into a second app core; keep it focused on engine seams.
- Prefer moving stable contracts into `sonicterm-types` when multiple
  crates need them.
- Do not depend on vendor font modules; use Sonic-owned font crates.
- Preserve configured packaged font directories alongside native discovery; every
  package relies on the bundled Symbols Nerd Font Mono loading from them without
  installation, ahead of system fallback. The text face is the platform default
  (`config::DEFAULT_MONOSPACE_FAMILIES`), and a missing configured family falls back to it.

## Cross-references
- Consumes: `sonicterm-font-config`, `sonicterm-font`, `sonicterm-text`,
  `sonicterm-types`.
- Consumed by: `sonicterm-gpu`, `sonicterm-linux`.
