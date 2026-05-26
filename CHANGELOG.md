# Changelog

All notable changes to Sonic Terminal will be documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [SemVer](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

_Nothing yet — v0.8.0 just shipped. v1.0 work begins next._

## [0.8.0] — 2026-05-26

The "production polish" release. Closes everything between the v0.6
preferences subsystem and the v1.0 production gate: the renderer is cut
over to the B3 atlas path, IME / search / palette / live-reload all
ship, the tab bar gains cross-window drag-to-merge, and a brand-new
visual snapshot regression net keeps WezTerm parity honest. Idle CPU is
finally pinned to ~0%.

### 🎉 Highlights

- **Idle CPU 99% → ~0%** — a render-thread `try_lock` spin was the
  culprit. Sonic now sleeps when idle the way it always should have. (#37)
- **B3 atlas renderer is the default** with a headless GUI bench
  harness and a pixel-diff visual snapshot net so parity stops silently
  drifting. (#42, #44, #47, #74)
- **Tab tear-out + cross-window drag-to-merge**, including OS-level
  cross-process drag via `NSPasteboard` and a live drag chip preview.
  (#43, #48, #59, #62, #64)
- **Command palette (`super+shift+P`) + in-page/scrollback search +
  IME**, all with proper visual overlays anchored to the cursor.
  (#40, #41, #45, #50, #51)
- **WezTerm visual parity within 3 ΔE** on the standard recipe — HiDPI
  at physical pixels, correct sRGB→linear gamma, CJK / emoji color /
  Hangul / Powerline / ZWJ shaping all land. (#49, #57, #63, #65, #68,
  #70, #71, #72, #75, #76, #77)

### ✨ Features

- `#39` Code-signing workflow (macOS notarization + Windows signtool),
  gated on repository secrets. Infra ready; certs deferred to v1.0.
- `#40` IME composition state plumbed through `winit` → `App` → renderer.
- `#41` Command palette bound to `super+shift+P`, searches all keymap
  actions + open tabs.
- `#42` B3 atlas renderer is now the default rendering path.
- `#43` Tab tear-out: drag a tab off the bar to spawn a new window
  carrying its `Grid + Parser + PtyHandle`.
- `#45` Visual overlays for command palette, search, and IME preedit.
- `#46` Font fallback chain for non-ASCII glyphs (CJK / emoji / symbols).
- `#48` In-app cross-window tab drag-to-merge.
- `#49` Emoji color (BGRA) + Hangul + Powerline glyphs + ZWJ sequences.
- `#50` IME preedit anchored under the cursor cell.
- `#51` In-page search + scrollback search, complete with match overlay
  and `n` / `N` navigation.
- `#52` Bracketed paste (`?2004`) + OSC 133 shell-integration prompt
  marks.
- `#53` Live reload for font, theme, and keymap files (via `notify`).
- `#54` Preferences persist to disk and live-apply without a restart.
- `#55` i18n: English, Simplified Chinese, Japanese UI strings.
- `#56` `sonic-mux` daemon — persistent PTY sessions surviving Sonic
  process restarts.
- `#57` Programming-ligature shaping + ZWJ runs through HarfBuzz.
- `#58` SSH client pane via `russh` (behind `ssh` cargo feature).
- `#59` OS-level cross-process tab drag using `NSPasteboard`.
- `#62` Single-tab cross-window merge.
- `#64` Drag chip preview + commit-on-release semantics for tab drag.
- `#66` Native macOS menubar (File / Edit / View / Window / Help).
- `#77` WezTerm-style tab titles (icon + cwd + process).

### 🐛 Fixed

- `#37` **Idle CPU 99% → ~0%** — render loop was `try_lock`-spinning;
  now yields properly when there's nothing to draw.
- `#61` DSR / DA replies wired through; fixes nvim hang on startup
  waiting for terminal identification.
- `#65` Theme colors now round-trip sRGB → linear → sRGB correctly; no
  more washed-out Dracula.
- `#68` CJK render mangled — wide-cell advance was being collapsed by
  the shaper; per-cell advance is now respected.
- `#71` Tab bar flushed correctly under the `wezterm` theme.
- `#72` CJK wide-cell advance + emoji color correct on HiDPI (`inv_s`
  scaling fix).

### ⚡ Performance

- `#37` Idle CPU 99% → ~0%.
- `#38` `PresentMode::Mailbox` for tear-free, low-latency frame delivery.
- `#42` B3 atlas renderer cutover — fewer glyph uploads per frame.
- `#44` Post-cutover bench numbers + a headless GUI bench harness.
- `#47` Renderer capability matrix regression net — guards against
  GPU-feature drift across the mac + win matrix.
- `#74` Headless visual snapshot regression — pixel-diff vs golden
  images on every PR.

### 📝 Docs

- `#60` README + `docs/USER_GUIDE.md` overhaul.
- `#67` `docs/TESTING.md` — local gate, e2e binaries, visual snapshot
  harness, headless GUI bench.
- `#70` `docs/VISUAL_PARITY.md` recipe vs WezTerm.
- `#73` `docs/release/CI-BILLING.md` notes.
- `#75` Visual parity report — 3 ΔE delta closed across the standard
  recipe.

### 🔧 Internals

- `#69` CI fully green again + icon-bake verify step.
- `#47` Renderer capability matrix in CI (macOS + Windows).
- `#74` Headless visual snapshot harness wired into CI.
- `#76` HiDPI cells rasterized at physical px (in flight at tag time —
  tracked for a v0.8.x patch if a regression surfaces).
- `#27` Per-crate `tests/` split; test floor held at **171+** across
  the workspace.

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
