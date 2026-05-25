# Changelog

All notable changes to Sonic Terminal will be documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [SemVer](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
- Original SVG app icon + `bake-icons.sh` for `.icns` / `.ico`.
- GitHub Actions: CI (macOS-14 + windows-latest) and Release (`.dmg` + `.msi`).
- Dependabot, CODEOWNERS, issue / PR templates.
- `cargo-deny` policy.
- Documentation: README, CONTRIBUTING, design spec.
