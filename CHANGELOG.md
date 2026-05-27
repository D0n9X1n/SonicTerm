# Changelog

All notable changes to Sonic Terminal will be documented in this file.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versions follow [SemVer](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

_v1.0.0 release notes below are staged here pending tag cut._

## [1.0.0] — 2026-05-27

The production release. Sonic crosses the line from "v0.8 polish" to
"v1.0 ship": Windows MVP lands (MSI installer, custom titlebar with
Mica backdrop, cross-platform menu, OLE drag-drop, foreground process
probe), the renderer + VT + PTY stack gets a deep perf pass, and the
code-signing pipeline is wired end-to-end on both platforms (Azure
Trusted Signing on Windows; Developer ID notarization on macOS — both
pending operational cert procurement to flip the switch).

### 🎉 Highlights

- **Windows MVP**: cargo-wix MSI installer, custom titlebar + Mica /
  Acrylic backdrop, cross-platform menu abstraction (NSMenu + muda),
  OLE drag-drop for tab tear-out + file drop, and a foreground-process
  probe via `NtQuerySystemInformation` so tab titles match macOS.
  (#133, #134, #135, #137, #139)
- **Renderer perf pass**: per-row glyph cache so clean rows don't
  re-shape, VecDeque-backed visible rows for O(1) scroll, LRU eviction
  in the glyph atlas before growing VRAM, and pre-baked
  box-drawing + Powerline glyphs at font load. (#130, #136, #140,
  #141, #142)
- **VT + PTY hot paths**: SWAR ASCII fast-path bypasses `vte` for
  printable runs, 4k LRU shape cache replaces the old clear-on-overflow
  cache, PTY reads go zero-copy through a `BytesMut` ring, and the app
  loop now gates redraws to the vsync cadence via
  `ControlFlow::WaitUntil`. (#129, #131, #132, #138)
- **Signing pipeline ready**: Windows release flow switched to Azure
  Trusted Signing; macOS Developer ID notarization plumbing wired.
  Certs still to be procured before the next tagged build is signed.
  (#128)

### ✨ Added

- `#128` Switch Windows release signing to Azure Trusted Signing; carry
  over macOS notarization plumbing from #39.
- `#133` `cargo-wix` MSI pipeline wired in CI, with the full asset
  bundle (themes, keymap, fonts) and the Sonic icon embedded.
- `#134` Windows foreground-process probe via
  `NtQuerySystemInformation`, feeding the WezTerm-style tab title.
- `#135` Cross-platform menu abstraction: NSMenu on macOS, `muda` on
  Windows, single keymap-action surface.
- `#137` Windows custom titlebar with Mica / Acrylic backdrop on
  Windows 11 (graceful fallback on Windows 10).
- `#139` OLE drag-drop on Windows: tab tear-out across windows + file
  drop into a pane (parity with the existing `NSPasteboard` path on
  macOS).

### ⚡ Improved (Performance)

- `#129` 4k LRU shape cache replaces clear-on-overflow — no more shape
  thrash when scrolling large buffers.
- `#130` Grid exposes a dirty bitset + invalidation hooks; foundation
  for the per-row renderer cache.
- `#131` PTY reads use a `BytesMut` ring; zero-copy from kernel → vte.
- `#132` Frame pacing via `ControlFlow::WaitUntil` — redraws gate to
  the vsync cadence instead of busy-waking.
- `#136` Glyph atlas now LRU-evicts before growing the GPU texture,
  bounding VRAM under heavy glyph churn.
- `#138` SWAR ASCII fast-path in the VT parser bypasses `vte` entirely
  for printable ASCII runs — the common case for `cat`/`tail -f`.
- `#140` Per-row glyph cache: the renderer skips re-shaping rows that
  haven't changed since the last frame.
- `#141` `VecDeque`-backed visible rows give O(1) scroll instead of
  O(n) memmove.
- `#142` Box-drawing + Powerline glyphs pre-baked into the atlas at
  font load — no first-frame stutter on TUIs that lean on them.

### 🔧 Internals

- Test floor held; per-PR gate (fmt + clippy + workspace test +
  pty_dump + pty_dump_unicode + release build + GUI smoke for any
  render/input/VT touch) enforced on all 15 PRs.

### ⏳ Still deferred past v1.0

- Linux support (`sonic-linux` re-enable + CI matrix + AppImage / .deb).
- Cert procurement for Apple Developer ID + Azure Trusted Signing —
  pipelines are ready, signed builds ship once the certs land.
- Auto-update (Sparkle / WinSparkle).
- Session restore on relaunch.
- Half-transparent / blur backgrounds.

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
