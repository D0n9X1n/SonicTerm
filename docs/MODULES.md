# Modules

Developer documentation: [Architecture](ARCHITECTURE.md) · **Modules** · [Logging](LOGGING.md) · [Packaging](packaging/README.md)

| Crate | Role |
| --- | --- |
| `sonicterm-types` | Contract crate: shared value types and trait seams. |
| `sonicterm-resource` | Resource governor: sharded ledger, owner hierarchy, reservation tokens, bounded reaper supervisor. |
| `sonicterm-vt` | VT/ANSI parser and terminal protocol handling. |
| `sonicterm-grid` | Cells, scrollback, wide characters, dirty rows. |
| `sonicterm-cfg` | TOML config, themes, keymaps, URL safety. |
| `sonicterm-io` | PTY/process/SSH-facing IO abstractions. |
| `sonicterm-text` | Glyph atlas, row glyph cache, text rendering support. |
| `sonicterm-font` | Font discovery, fallback, shaping, rasterization (DirectWrite default on Windows, FreeType elsewhere/fallback). |
| `sonicterm-block-glyph` | Box drawing, block glyphs, Powerline, Braille geometry. |
| `sonicterm-render-model` | Renderer-agnostic frame and pane input data. |
| `sonicterm-ui` | Tabs, palette, search, selection, IME, copy mode. |
| `sonicterm-gpu` | wgpu renderer, quad/glyph presentation pipelines, Windows deterministic software-render path. |
| `sonicterm-app-core` | Winit-independent app reducer/state machine. |
| `sonicterm-app` | Cross-platform window/tab/pane orchestration. |
| `sonicterm-mac` | macOS binary, NSMenu, AppKit hooks, mac drag/drop. |
| `sonicterm-windows` | Windows binary, DPI/Win32 glue, Mica, OLE drag/drop, software presentation, WiX packaging; local ConPTY transport stays behind `sonicterm-io`. |
| `sonicterm-mux` | Future persistent PTY mux daemon. |
| `sonicterm-logging` | Logs, panic hooks, exit tracing. |
| `sonicterm-engine` | WezTerm-compatible font engine adapter surface. |
| `sonicterm-font-config` | Font configuration value helpers. |
| `sonicterm-fontconfig` | fontconfig build/link shim. |
| `sonicterm-freetype` | FreeType/libpng/zlib bindings. |
| `sonicterm-harfbuzz` | HarfBuzz bindings. |

Every crate has a local `CLAUDE.md` with purpose, public surface, test gate, and
pitfalls.
