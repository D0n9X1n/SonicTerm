# sonicterm-cfg

## Purpose
Configuration, themes, keymaps, bundled/user asset lookup, dimensions,
and target safety. This crate is the only place that should parse
`sonicterm.toml`, theme TOML, keymap TOML, and clickable URI/path text.

## Key files
- `config.rs` - user config schema, defaults, load/fallback behavior.
- `theme.rs` - theme schema and named/path loading.
- `keymap.rs` - keymap schema and action binding resolution.
- `assets.rs` - bundled and user asset directory lookup.
- `url_scan.rs` / `url_open.rs` - typed URL/path detection and safe URI-open policy;
  `url_open/windows.rs`, `url_open/macos.rs`, and `url_open/linux.rs` (every Unix
  except macOS) hold each OS's dispatch.
- `dimension.rs` - size/unit helpers shared with font and UI code.

## Local gate
```bash
cargo test -p sonicterm-cfg
```

## Guardrails
- Startup may fall back to defaults, but an explicit reload should surface
  parse errors clearly instead of silently accepting bad config.
- Preserve unknown/future TOML keys when possible.
- Theme/keymap loading must check both bundled assets and user override
  directories under `~/.sonicterm/`.
- URI/path handling is security-sensitive. Preserve `Uri`, `PathCandidate`, and
  `BareName` provenance; contextual candidate lookup stays separate from explicit
  scanning, spaced-candidate enumeration stays bounded, and filesystem targets
  must never enter the URI opener. `url_open::validate` enforces that by refusing
  every `file:` URI; detection in `url_scan` checks its own scheme list, which
  still recognises `file://` for classification.
- Path and URL detection is one component for every operating system: a Windows
  pane can show POSIX paths, for example from a WSL shell. Keep both `PathStyle`
  grammars in `url_scan.rs` free of OS gates and tested on every host;
  `PathStyle::native()` is the only read of the build target there. Opening is
  the per-OS part, in `url_open/{macos,linux,windows}.rs`.

## Cross-references
- Consumed by: `sonicterm-app`, `sonicterm-mac`, `sonicterm-windows`,
  `sonicterm-linux`, `sonicterm-ui`, `sonicterm-gpu`, `sonicterm-block-glyph`.
