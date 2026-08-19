<div align="center">

<img src="assets/icons/exports/png/sonic-256.png" alt="SonicTerm logo" width="150" height="150"/>

# SonicTerm

**A fast, native, GPU-accelerated terminal for people who live in the shell.**

[![CI](https://github.com/D0n9X1n/SonicTerm/actions/workflows/ci.yml/badge.svg)](https://github.com/D0n9X1n/SonicTerm/actions/workflows/ci.yml)
[![Release](https://github.com/D0n9X1n/SonicTerm/actions/workflows/release.yml/badge.svg)](https://github.com/D0n9X1n/SonicTerm/actions/workflows/release.yml)
[![Version](https://img.shields.io/github/v/tag/D0n9X1n/SonicTerm?sort=semver&label=version)](https://github.com/D0n9X1n/SonicTerm/tags)
[![Downloads](https://img.shields.io/github/downloads/D0n9X1n/SonicTerm/total?label=downloads)](https://github.com/D0n9X1n/SonicTerm/releases)
[![Stars](https://img.shields.io/github/stars/D0n9X1n/SonicTerm?style=flat&label=stars)](https://github.com/D0n9X1n/SonicTerm/stargazers)
[![Issues](https://img.shields.io/github/issues/D0n9X1n/SonicTerm?label=issues)](https://github.com/D0n9X1n/SonicTerm/issues)
[![Pull Requests](https://img.shields.io/github/issues-pr/D0n9X1n/SonicTerm?label=prs)](https://github.com/D0n9X1n/SonicTerm/pulls)
[![Last Commit](https://img.shields.io/github/last-commit/D0n9X1n/SonicTerm?label=last%20commit)](https://github.com/D0n9X1n/SonicTerm/commits/main)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

SonicTerm is a native macOS, Windows, and Linux terminal with the quality-of-life
features you expect from a modern editor: searchable commands, split panes,
draggable tabs, editable TOML config, bundled Nerd-Font-patched typography, and
a renderer that is designed around GPU quads instead of CPU blits.

It aims to feel small, sharp, and quiet: no Electron shell, no heavyweight
runtime, no required GUI preferences pane, and no global dotfile sprawl. User
state lives under one directory: `~/.sonicterm`.

Need installation, configuration, keybindings, or theme authoring docs? Read the
full bilingual docs in the repository-tracked [`wiki/`](wiki/) directory. The
README is only the product overview: why SonicTerm exists, what it feels like,
and why you might want to use it.

## Screenshots

### Command palette + editable config

The command palette is the center of the UI: search commands, see the active
keymap shortcut, open config/keymaps, rename tabs, split panes, toggle chrome,
and reload settings without leaving the terminal.

![SonicTerm command palette editing config](assets/screenshots/command-palette-config.png)

### Split panes built for AI CLIs and real terminal apps

SonicTerm supports multiple panes, per-pane PTYs, tab titles, Nerd Font icons,
Powerline glyphs, emoji, CJK, and terminal apps that draw their own UI.

![SonicTerm split panes running AI CLIs](assets/screenshots/split-panes-ai-cli.png)

### READONLY mode for safe navigation

READONLY mode blocks terminal input while keeping search, tab switching, and
pane focus available. It is built for safely inspecting scrollback, config, or
long-running output without accidentally typing into the active shell.

![SonicTerm READONLY mode with search](assets/screenshots/read-only-search.png)

### Broadcast input across panes

Broadcast mode sends the same keyboard input to every pane in the current tab,
which is useful for mirrored shell commands, paired AI CLI sessions, and quick
multi-pane setup work.

![SonicTerm broadcast input demo](assets/screenshots/broadcast-input.gif)

### Drag tabs between windows

This is SonicTerm's most important workflow feature: tabs are not trapped in one
window. Drag a busy terminal out into its own window, work there, then drag it
back when the context belongs with the original session again.

![SonicTerm tab drag demo](assets/screenshots/tab-drag-demo.gif)

## Why try it?

- **Native macOS, Windows, and Linux app** — small binaries, no Electron, no web runtime.
- **GPU renderer** — wgpu on Metal, D3D12, and Vulkan, with atlas-backed text and batched
  quads for cells, chrome, selection, cursor, search, and overlays. Glyphs
  rasterize with DirectWrite on Windows (FreeType elsewhere and as fallback).
  Detects the no-GPU case (RDP / VM software rasterizer) and degrades gracefully
  to stay responsive, with a deterministic full-screen software path on Windows.
- **Bounded memory, and it tells you** — every subsystem that can grow has a
  ceiling, and the ones that matter are process-wide rather than per-pane, so
  a session with many panes cannot sum its way past them. When SonicTerm does
  discard something you can see — an image transfer that stopped arriving, or
  older images in idle panes — it says so in the log at the default level,
  without needing diagnostics turned on beforehand. `session retention` lines
  break a session's usage down by subsystem when you want the detail.
- **Real pane workflow** — split panes, close/resize behavior, per-pane PTYs,
  pane focus, READ ONLY mode for safe scrollback navigation, quick-select URL
  hints, and search.
- **Command palette first** — commands are searchable and display shortcuts from
  your current keymap config.
- **Config is just files** — `~/.sonicterm/sonicterm.toml`, plus editable
  `themes/` and `keymaps/` examples seeded on first launch.
- **Bundled typography** — `Rec Mono St.Helens` ships with the app and is
  Nerd-Font-patched, so icons and prompt glyphs work out of the box.
- **WezTerm-inspired behavior** — terminal, font, keymap, and rendering details
  follow WezTerm-proven semantics where SonicTerm has absorbed them.

## Documentation lives in `wiki/`

The README intentionally avoids operational details. If you want to install it,
change preferences, edit keybindings, author a theme, inspect logs, or build from
source, use the repository-tracked bilingual documentation:

| Topic | Documentation page |
| --- | --- |
| Usage and installation | [Usage](wiki/Usage.md) |
| Preferences and `sonicterm.toml` | [Configuration](wiki/Configuration.md) |
| Keymap editing | [Keybindings](wiki/Keybindings.md) |
| Theme authoring | [Themes](wiki/Themes.md) |
| Logs and diagnostics | [Logging](wiki/Logging.md) |

## Thanks, WezTerm

SonicTerm owes a lot to [WezTerm](https://github.com/wezterm/wezterm). WezTerm
is the reference for terminal semantics, font behavior, keymap conventions, and
many rendering edge cases. SonicTerm has absorbed WezTerm-proven ideas into
Sonic-owned crates:

- VT/grid behavior in `sonicterm-vt` and `sonicterm-grid`.
- Font fallback, shaping, and rasterization in `sonicterm-font`.
- Box drawing, block glyph, Powerline, Braille, sextant, and octant geometry in
  `sonicterm-block-glyph`.

WezTerm is MIT-licensed; the upstream license for absorbed custom-glyph code is
kept at `crates/sonicterm-block-glyph/LICENSE-WEZTERM`.

## License

SonicTerm is released under the [MIT License](LICENSE).
