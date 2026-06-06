# Modules

| Crate | Role |
| --- | --- |
| `sonicterm-types` | Contract crate: shared value types and trait seams. |
| `sonicterm-vt` | VT/ANSI parser and terminal protocol handling. |
| `sonicterm-grid` | Cells, scrollback, wide characters, dirty rows. |
| `sonicterm-cfg` | TOML config, themes, keymaps, URL safety. |
| `sonicterm-io` | PTY/process/SSH-facing IO abstractions. |
| `sonicterm-text` | Glyph atlas, row glyph cache, text rendering support. |
| `sonicterm-font` | Font discovery, fallback, shaping, rasterization. |
| `sonicterm-block-glyph` | Box drawing, block glyphs, Powerline, Braille geometry. |
| `sonicterm-render-model` | Renderer-agnostic frame and pane input data. |
| `sonicterm-ui` | Tabs, palette, search, selection, IME, copy mode. |
| `sonicterm-gpu` | wgpu renderer, quad/glyph presentation pipelines. |
| `sonicterm-app-core` | Winit-independent app reducer/state machine. |
| `sonicterm-app` | Cross-platform window/tab/pane orchestration. |
| `sonicterm-mac` | macOS binary, NSMenu, AppKit hooks, mac drag/drop. |
| `sonicterm-windows` | Windows binary, ConPTY, Mica, OLE drag/drop, WiX packaging. |
| `sonicterm-mux` | Future persistent PTY mux daemon. |
| `sonicterm-logging` | Logs, panic hooks, exit tracing. |
| `sonicterm-engine` | WezTerm-compatible font engine adapter surface. |
| `sonicterm-font-config` | Font configuration value helpers. |
| `sonicterm-fontconfig` | fontconfig build/link shim. |
| `sonicterm-freetype` | Freetype/libpng/zlib bindings. |
| `sonicterm-harfbuzz` | Harfbuzz bindings. |

Every crate has a local `CLAUDE.md` with purpose, public surface, test gate, and
pitfalls.
