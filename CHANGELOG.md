# Changelog

All notable changes to Sonic Terminal will be documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [SemVer](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Split unit tests into per-crate `tests/` folders so they run as
  integration tests against each crate's public API. Test count: **171**
  across the workspace. (#27)

## [0.6.0] — 2026-05-25

### Added
- Graphical preferences UI subsystem: typed `Prefs` state, in-process
  controls (toggle / slider / dropdown / color picker / text / keymap
  recorder), and `super+comma` → `open_preferences` binding. In-window
  control rendering is staged for a follow-up. (#26)

## [0.5.0] — 2026-05-25

### Added
- Alt-screen support (`?1049h/l`, `?47h/l`) so vim / htop / less restore
  the primary buffer cleanly on exit.
- Cursor visibility (`?25`), bracketed paste (`?2004`), and SGR mouse
  reporting (`?1006`) DEC modes. (#24)

## [0.4.0] — 2026-05-25

### Added
- OSC 8 hyperlink registry + URL opener (data layer). (#20)
- OSC 8 hyperlink visual rendering + Cmd-click activation. (#22)
- In-page search (`Cmd+F`) against the visible grid and scrollback,
  with quad-pass highlight of matches. (#23)

## [0.3.x] — 2026-05-25

### Added
- **v0.3a** — cursor rendering, mouse text selection, keymap dispatcher,
  clipboard integration (`bcacfcd`).
- **v0.3b** — per-cell foreground color + bold / italic / underline
  rendering via glyphon `Attrs`. (#11)
- **v0.3c** — browser-style tab bar UI: trapezoidal tabs, click-to-activate,
  `×` to close, `+` to add. (#19)
- **v0.3d** — pane layout renderer + per-pane PTY: `PaneTree` is walked
  and divided by `ratio`/`axis`; each leaf owns its own grid + parser +
  PTY. (#21)
- Brand: official Sonic icon (terminal window + cyan speed trails + `>_`
  prompt) and asset-management system (`source/` SVGs +
  `bake-icons.sh` → `exports/`). (#18)

### Changed
- Upgraded the entire stack to latest stable: wgpu 29, glyphon 0.11,
  cosmic-text 0.18, vte 0.15, thiserror 2, toml 1. (#10)
- Flattened repo layout: crates moved from `crates/sonic-*` to top-level
  `sonic-*` (`9c46c39`).

### Fixed
- ED / EL CSI ops now only operate within their declared mode
  (`9c46c39`).
- macOS hang on UI input — coalesce redraws + `try_lock`
  (`e2deed0`).

## [0.2.0] — 2026-05-24

### Added
- GPU character rendering (wgpu + glyphon) — characters appear on screen.
- Keyboard input → PTY: arrows, ctrl-letter, F-keys, modifiers via
  `encode_logical`. (`a11d9ef`)

## [0.1.0-alpha.2] — 2026-05-24

### Fixed
- `PaneTree::close()` nested-collapse bug; full local lint/test gate
  passing; test count 20 → 46. (`3214d5c`)

## [0.1.0-alpha.1] — 2026-05-24

### Added
- Cargo workspace with 4 crates: `sonic-core`, `sonic-shared`, `sonic-mac`, `sonic-windows`.
- VT/ANSI parser (CSI cursor motion + ED/EL + SGR incl. 256-color & truecolor; OSC 0/2/8/52).
- Grid model with scrollback, unicode width, wide-char support.
- Cross-platform PTY via `portable-pty`.
- `TabBar` model with push / close / reorder / detach.
- Recursive split `PaneTree` with collapse-on-close.
- TOML config loader (font / window / terminal / theme / keymap).
- 4 bundled themes: Tokyo Night, Dracula, Nord, Catppuccin Mocha.
- WezTerm-compatible default keymap (`assets/keymaps/wezterm.toml`).
- Placeholder hedgehog SVG app icon + `bake-icons.sh` for `.icns` / `.ico`
  (superseded by the terminal-window mark in v0.3 — see #18).
- GitHub Actions: CI (macOS-14 + windows-latest) and Release (`.dmg` + `.msi`).
- Dependabot, CODEOWNERS, issue / PR templates.
- `cargo-deny` policy.
- Documentation: README, CONTRIBUTING, design spec.
