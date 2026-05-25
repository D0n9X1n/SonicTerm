<div align="center">

<img src="assets/icons/exports/png/sonic-256.png" alt="Sonic Terminal" width="160" height="160"/>

# Sonic Terminal

**A GPU-accelerated, cross-platform terminal for people who treat the prompt like home.**

[![CI](https://github.com/D0n9X1n/sonic/actions/workflows/ci.yml/badge.svg)](https://github.com/D0n9X1n/sonic/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Roadmap](https://img.shields.io/badge/roadmap-docs%2FROADMAP.md-blue.svg)](docs/ROADMAP.md)
[![Brand](https://img.shields.io/badge/brand-docs%2Fbrand%2Ficon.md-purple.svg)](docs/brand/icon.md)

</div>

---

## What is Sonic?

Sonic is a terminal emulator written in Rust that aims to be **fast first, beautiful second, and configurable third**. It runs on macOS and Windows, ships with WezTerm-compatible default keybindings, bundles a Nerd Font so prompts and icons "just work," and looks like a polished desktop app, not a port.

## Status — `v0.2.0`

Full feature matrix and roadmap: **[`docs/ROADMAP.md`](docs/ROADMAP.md)**.

| Area | State |
|---|---|
| Cargo workspace + 4 crates | ✅ |
| GitHub Actions CI (fmt / clippy / test / deny) — macOS + Windows | ✅ |
| Release pipeline → `.dmg` (universal) + `.msi` (x64) | ✅ |
| Cross-platform PTY (`portable-pty`) | ✅ |
| VT/ANSI parser (SGR, CUP, ED, EL, OSC 0/2/8/52) | ✅ |
| Grid model w/ scrollback, unicode width, wide chars | ✅ |
| WezTerm-compatible keymap | ✅ |
| 4 bundled themes (Tokyo Night, Dracula, Nord, Catppuccin) | ✅ |
| Original app icon | ✅ |
| Tab bar + recursive split tree (model only) | ✅ |
| **GPU character rendering (wgpu + glyphon)** | ✅ |
| **Keyboard input → PTY** (arrows, ctrl-letter, F-keys, ...) | ✅ |
| Per-cell color rendering | ⏳ v0.3 |
| Cursor + tab bar UI | ⏳ v0.3 |
| Tab drag out / cross-window merge | ⏳ v0.4 |
| Sixel / Kitty graphics, SSH, mux, ligatures | ⏳ v0.5 |
| Code signing / notarization, auto-update | ⏳ v1.0 |

## Quick start

```bash
# clone
git clone git@github.com:D0n9X1n/sonic.git
cd sonic

# build everything
cargo build --release

# run on macOS
cargo run --release -p sonic-mac

# run on Windows
cargo run --release -p sonic-windows
```

## Configuration

Sonic reads `~/Library/Application Support/Sonic/sonic.toml` on macOS or
`%APPDATA%\Sonic\sonic.toml` on Windows. Example:

```toml
theme  = "tokyo-night"   # or "dracula", "nord", "catppuccin-mocha"
keymap = "wezterm"

[font]
family      = "JetBrainsMono Nerd Font"
size        = 14.0
line_height = 1.2

[window]
cols        = 100
rows        = 30
padding     = 8.0
decorations = true
opacity     = 1.0
blur        = false

[terminal]
shell        = "/bin/zsh"   # or "powershell.exe"
scrollback   = 10000
cursor_blink = true
```

## Project layout

```
sonic/
├── crates/
│   ├── sonic-core/     VT parser, grid, PTY, config, keymap, theme
│   ├── sonic-shared/   window, tab bar, pane tree, app loop
│   ├── sonic-mac/      macOS entrypoint
│   └── sonic-windows/  Windows entrypoint
├── assets/             icon, themes, fonts, keymaps
├── packaging/          .dmg + .msi build scripts
└── .github/workflows/  CI + release pipelines
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). TL;DR: branch off `main`, run
`cargo fmt && cargo clippy --workspace --all-targets -- -D warnings &&
cargo test --workspace`, open a PR. The CI matrix runs on macOS-14 (arm64)
and windows-latest.

## License

[MIT](LICENSE). Bundled fonts retain their original licenses
(see [`assets/fonts/README.md`](assets/fonts/README.md)).
