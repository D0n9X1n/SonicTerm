# sonicterm-engine

## Purpose
Small engine seam for font-facing data that is not terminal state.
Today it exposes `FontStack` and `CellMetricsPx` while the font pipeline
continues moving into Sonic-owned crates.

Terminal parsing and grid state live in `sonicterm-vt` and
`sonicterm-grid`, not here.

## Key files
- `fontstack.rs` - font stack selection and cell metrics.
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

## Cross-references
- Consumed by: `sonicterm-block-glyph` and font/rendering code.
