# SonicTerm Wiki

[简体中文](Home-zh-CN)

SonicTerm is a native, GPU-accelerated terminal for macOS, Windows, and Linux.
**Start with [Usage](Usage)** to install it and learn everyday actions.

### User guide

- [Usage](Usage) — install, open tabs, split panes, detect file references in terminal messages, select text, and use rmux/tmux
- [Configuration](Configuration) — change defaults in `~/.sonicterm/sonicterm.toml`, reload, and save
- [Keybindings](Keybindings) — find a shortcut or write a binding
- [Themes](Themes) — choose or create a color palette
- [Logging](Logging) — find logs, investigate a problem, and prepare a bug report
- [Memory](Memory) — understand resource limits and retained-memory reports

### How SonicTerm works

Read [Architecture](Architecture) for the map, then
[From Keypress to Pixel](From-Keypress-to-Pixel) for one `A`'s round trip.
Use the references below for a particular subsystem.

- [Architecture](Architecture) — system shape and crate boundaries
- [From Keypress to Pixel](From-Keypress-to-Pixel) — input, child output, grid, glyph, and pixel
- [Runtime Lifecycle](Runtime-Lifecycle) — startup, ownership changes, tab transfers, and shutdown
- [Terminal IO and VT](Terminal-IO-and-VT) — PTYs, parser protocols, shell integration, and grid rules
- [Rendering Modes](Rendering-Modes) — adapter selection and frame pacing
- [Rendering and Fonts](Rendering-and-Fonts) — typography, atlases, GPU/CPU drawing, and damage
- [Platform Integration](Platform-Integration) — AppKit, Win32, X11, and Wayland boundaries
- [Architecture Internals](Architecture-Internals) — correctness, accounting, and lifetime invariants
- [Crate Reference](Crate-Reference) — all 23 crates, their interfaces, and dependencies

### Build and contribute

- [Packaging](Packaging) — build local packages and inspect their layouts
- [Development and Release](Development-and-Release) — exact gates, PR workflow, releases, and Wiki publication
- [Home](Home) — return to this index
