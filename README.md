<div align="center">

<img src="assets/icons/exports/png/sonic-256.png" alt="SonicTerm" width="160" height="160"/>

# SonicTerm

**SonicTerm 1.0.0 — a GPU-accelerated terminal for macOS and Windows.**

[![CI](https://github.com/D0n9X1n/SonicTerm/actions/workflows/ci.yml/badge.svg)](https://github.com/D0n9X1n/SonicTerm/actions/workflows/ci.yml)
[![Release](https://github.com/D0n9X1n/SonicTerm/actions/workflows/release.yml/badge.svg)](https://github.com/D0n9X1n/SonicTerm/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

## What it is

SonicTerm is a native terminal with tabs, split panes, command palette, search,
copy mode, IME, OSC 8 hyperlinks, OSC 133 shell markers, drag-out tabs, and
GPU rendering through `wgpu`.

Supported platforms for 1.0.0:

- macOS 14+
- Windows 10/11 x64

Linux, code signing, auto-update, and session restore are not part of 1.0.0.

## Install

Every `v*` tag builds release artifacts automatically:

- macOS: `SonicTerm-<tag>-mac-universal.dmg`
- Windows: unsigned `.msi`

Download from <https://github.com/D0n9X1n/SonicTerm/releases>.

From source:

```sh
git clone https://github.com/D0n9X1n/SonicTerm.git
cd SonicTerm

cargo build --release -p sonicterm-mac       # macOS
cargo build --release -p sonicterm-windows   # Windows
```

## Quick start

| Action | macOS | Windows |
| --- | --- | --- |
| New tab | `Cmd+T` | `Ctrl+T` |
| Close pane/tab | `Cmd+W` | `Ctrl+Shift+W` |
| Split right | `Cmd+D` | `Ctrl+Shift+D` |
| Split down | `Cmd+Shift+D` | `Ctrl+Alt+Shift+D` |
| Command palette | `Cmd+Shift+P` | `Ctrl+Shift+P` |
| Search | `Cmd+F` | `Ctrl+Shift+F` |
| Copy mode | `Cmd+[` | `Ctrl+Shift+[` |

The command palette shows the active keymap shortcut for each command. Tabs can
be dragged out into their own window and dragged back to merge.

## Configuration

Config paths:

- macOS: `~/Library/Application Support/SonicTerm/sonicterm.toml`
- Windows: `%APPDATA%\SonicTerm\sonicterm.toml`

Default theme: `wezterm`, a modified Gruvbox dark hard palette with SonicTerm's
near-black background and yellow cursor.

Default font: `Rec Mono St.Helens`, bundled and Nerd-Font-patched.

Minimal config:

```toml
theme = "wezterm"
keymap = "sonicterm-macos"

[font]
family = "Rec Mono St.Helens"
size = 14
line_height = 1.1

[terminal]
cursor_blink = true
cursor_shape = "block"
```

Full bilingual usage/config/keybinding/logging/theme docs live in `wiki/`.

## Developer docs

- `docs/ARCHITECTURE.md` — architecture and data flow.
- `docs/MODULES.md` — crate map.
- `docs/LOGGING.md` — log paths and diagnostics.
- `docs/RELEASE.md` — 1.0.0 release process.

Each crate has a local `CLAUDE.md` with its purpose, public surface, pitfalls,
and test gate.

## Tests

Normal PR and `main` CI run unit tests only:

```sh
cargo test --workspace --lib --bins
```

Release tags run the same unit tests and then build macOS/Windows installers.

## WezTerm acknowledgement

SonicTerm owes a lot to [WezTerm](https://github.com/wezterm/wezterm). We use
WezTerm as the reference for terminal semantics, font behavior, keymap
conventions, and many rendering edge cases. Several proven ideas were absorbed
into Sonic-owned crates rather than kept as a vendor dependency:

- VT/grid behavior in `sonicterm-vt` and `sonicterm-grid`.
- Font fallback, shaping, and rasterization in `sonicterm-font`.
- Box drawing, block glyph, Powerline, Braille, sextant, and octant geometry in
  `sonicterm-block-glyph`.

WezTerm is MIT-licensed; the upstream license for absorbed custom-glyph code is
kept at `crates/sonicterm-block-glyph/LICENSE-WEZTERM`.

## License

SonicTerm is released under the [MIT License](LICENSE).
