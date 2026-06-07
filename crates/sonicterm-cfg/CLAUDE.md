# sonicterm-cfg

## Purpose
Configuration, themes, keymaps, bundled/user asset lookup, dimensions,
and URL safety. This crate is the only place that should parse
`sonicterm.toml`, theme TOML, keymap TOML, and clickable URLs.

## Key files
- `config.rs` - user config schema, defaults, load/fallback behavior.
- `theme.rs` - theme schema and named/path loading.
- `keymap.rs` - keymap schema and action binding resolution.
- `assets.rs` - bundled and user asset directory lookup.
- `url_scan.rs` / `url_open.rs` - URL detection and safe open policy.
- `dimension.rs` - size/unit helpers shared with font and UI code.

## Local gate
```bash
cargo test -p sonicterm-cfg
```

## Guardrails
- Startup may fall back to defaults, but hot reload should surface parse
  errors clearly instead of silently accepting bad config.
- Preserve unknown/future TOML keys when possible.
- Theme/keymap loading must check both bundled assets and user override
  directories under `~/.snoicterm/`.
- URL handling is security-sensitive; keep allow/deny policy explicit.

## Cross-references
- Consumed by: `sonicterm-app`, `sonicterm-mac`, `sonicterm-windows`,
  `sonicterm-ui`, `sonicterm-gpu`, `sonicterm-block-glyph`.
