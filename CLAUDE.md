# CLAUDE.md — SonicTerm 1.0.0

SonicTerm is a GPU-accelerated terminal for macOS and Windows. Keep changes
small, typed, and cross-platform unless the crate is explicitly platform-only.

## Read first

- `docs/ARCHITECTURE.md` — architecture and data flow.
- `docs/MODULES.md` — crate map.
- `docs/LOGGING.md` — logs and diagnostics.
- `docs/RELEASE.md` — tag-driven release process.
- `wiki/` — bilingual user-facing usage/config/keybinding/log/theme docs.

When auditing docs for release blockers, typos, renamed paths, or user-facing
terminology, include `wiki/` alongside README and `docs/`; the wiki is part of
the monitored documentation surface.

When touching a crate, also read that crate's local `CLAUDE.md`.

## Crates

| Crate | Role |
| --- | --- |
| `sonicterm-types` | Shared contract types and trait seams. |
| `sonicterm-vt` | VT/ANSI parsing. |
| `sonicterm-grid` | Cells, scrollback, dirty rows. |
| `sonicterm-cfg` | Config, themes, keymaps, URL safety. |
| `sonicterm-io` | PTY/process/SSH IO. |
| `sonicterm-text` | Glyph atlas and row text cache. |
| `sonicterm-font` | Font discovery, shaping, fallback, rasterization. |
| `sonicterm-block-glyph` | Box/block/Powerline/Braille geometry. |
| `sonicterm-render-model` | Renderer-agnostic frame data. |
| `sonicterm-ui` | Tabs, palette, search, selection, IME. |
| `sonicterm-gpu` | wgpu renderer. |
| `sonicterm-app-core` | Winit-independent reducer/state. |
| `sonicterm-app` | Cross-platform app orchestration. |
| `sonicterm-mac` | macOS binary/glue. |
| `sonicterm-windows` | Windows binary/glue. |
| `sonicterm-mux` | Future mux daemon. |
| `sonicterm-logging` | Logs, panic hook, exit tracing. |

## Local gate

Normal PR/main CI runs workspace unit tests plus a per-crate unit/build gate:

```bash
cargo test --workspace --lib --bins
bash scripts/check-workspace-crates.sh
```

For release prep also run:

```bash
cargo build --release -p sonicterm-mac
bash scripts/test-release-notes.sh
```

## Release

SonicTerm releases are created by pushing a `v*` tag. The tag workflow builds:

- macOS universal `.dmg`
- Windows x64 `.msi`
- release notes from commits since the previous tag

## WezTerm

SonicTerm thanks WezTerm and uses it as the reference for terminal behavior,
font behavior, keymap conventions, and rendering edge cases. Absorb proven
behavior into Sonic-owned crates; do not reintroduce a `vendor/` dependency.
