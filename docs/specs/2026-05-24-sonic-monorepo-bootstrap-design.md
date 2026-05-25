# Sonic Terminal Bootstrap — Design Spec

> **⚠️ Partially superseded (2026-05-25).** This is the original v0.1.0
> bootstrap spec, preserved for historical context. Several sections no
> longer reflect the shipped repo. The authoritative current state lives
> in [`docs/ROADMAP.md`](../ROADMAP.md). Specifically superseded:
>
> - **§3 Repository Layout** — the `crates/` nesting was flattened in
>   `9c46c39`; crates now live at the top level (`sonic-core/`,
>   `sonic-shared/`, `sonic-mac/`, `sonic-windows/`).
> - **§4 Tech Stack** — pinned versions are out of date. Current stack
>   (after #10): `wgpu 29`, `glyphon 0.11`, `cosmic-text 0.18`,
>   `vte 0.15`, `winit 0.30`, `portable-pty 0.9`, `thiserror 2`,
>   `toml 1`. See ROADMAP "Constraints" for the canonical pinned set.
> - **§8 Icon Design** — the original "stylized lightning-fast hedgehog"
>   was replaced by the terminal-window + cyan-speed-trails + `>_`
>   mark in #18. See [`docs/brand/icon.md`](../brand/icon.md) for the
>   current brand guide.
> - **§10 Acceptance** — all v0.1.0 acceptance criteria shipped; the
>   project is now at **v0.6** with 171 tests. Subsequent acceptance
>   criteria live in the ROADMAP per-version sections.
>
> Sections **§1, §2, §5, §6, §7, §9, §11, §12** remain broadly accurate
> as historical record of the bootstrap intent.

- **Doc**: `docs/specs/2026-05-24-sonic-monorepo-bootstrap-design.md`
- **Track**: Complex (framework-cap-override: user explicitly removed 1000-word limit)
- **Status**: Approved (bootstrap shipped; later milestones tracked in ROADMAP)

## 1. Purpose
Build the **Sonic Terminal** v0.1.0 — a cross-platform, GPU-accelerated terminal emulator targeting macOS and Windows, in a single monorepo. Goal: ship a usable terminal (PTY + VT parsing + GPU render + tabs + splits + WezTerm keymap + original icon) plus complete CI/release infrastructure.

## 2. Out of Scope (v0.1.0)
- Linux build (folder reserved but not in CI/release matrix)
- Sixel / Kitty graphics protocols
- Built-in SSH client and multiplexer
- Font ligatures, IME (basic only)
- Code signing & notarization
- Cross-window tab drag-out / merge (API hook reserved; implemented in v0.2)
- Auto-update
- > 100% WezTerm performance parity (engineering best-effort, not a release blocker)

## 3. Repository Layout
```
sonic/
├── Cargo.toml                  workspace, release profile = fat LTO
├── rust-toolchain.toml         stable + rustfmt + clippy
├── rustfmt.toml / clippy.toml / deny.toml / .editorconfig
├── LICENSE (MIT) / README.md / CONTRIBUTING.md / CHANGELOG.md
├── crates/
│   ├── sonic-core/             lib: VT parser, grid, PTY, config, keymap, theme
│   ├── sonic-shared/           lib: window/render/tab abstractions, app loop
│   ├── sonic-mac/              bin: macOS entrypoint
│   └── sonic-windows/          bin: Windows entrypoint
├── assets/
│   ├── icons/                  SVG master + bake script for .icns / .ico
│   ├── themes/                 4 bundled TOML themes
│   ├── fonts/                  JetBrainsMono Nerd Font (OFL)
│   └── keymaps/wezterm.toml    WezTerm default keymap port
├── packaging/{mac,windows}     dmg + msi build scripts
├── docs/{specs,plans,reviews}
└── .github/
    ├── workflows/{ci,release}.yml
    ├── ISSUE_TEMPLATE/
    ├── pull_request_template.md
    ├── CODEOWNERS
    └── dependabot.yml
```

## 4. Tech Stack
| Layer | Choice | Why |
|---|---|---|
| Language | Rust 2021, stable | Performance + safety + WezTerm/Alacritty precedent |
| Windowing | `winit 0.30` | Cross-platform, mature, supports raw-window-handle |
| Rendering | `wgpu 0.20` | Single abstraction over Metal (mac) + DX12 (win) |
| Text | `glyphon 0.5` | Cosmic-text + wgpu glyph atlas; SDF-ready |
| PTY | `portable-pty 0.8` | Wraps openpty (mac) + ConPTY (win) |
| VT parsing | `vte 0.13` + semantic layer in `sonic-core::vt` | Battle-tested, alacritty also uses it |
| Async | `tokio` (multi-thread) + `crossbeam-channel` | Decouple PTY/render/UI |
| Config | `serde` + `toml` + `notify` | Hot-reload TOML |
| Logging | `tracing` + `tracing-subscriber` | Structured |

## 5. CI Pipeline (`.github/workflows/ci.yml`)
- **Trigger**: pull_request + push to `main`
- **Matrix**: `macos-14` (arm64) × `windows-latest` (x64)
- **Steps**:
  1. checkout
  2. install rust-toolchain (auto from `rust-toolchain.toml`)
  3. `Swatinem/rust-cache` for registry + target
  4. `cargo fmt --all -- --check`
  5. `cargo clippy --workspace --all-targets -- -D warnings`
  6. `cargo test --workspace --all-features`
  7. `cargo deny check` (advisories + licenses + bans + sources)
- **Failure policy**: any step fails → block merge

## 6. Release Pipeline (`.github/workflows/release.yml`)
- **Trigger**: push tag matching `v[0-9]+.[0-9]+.[0-9]+*`
- **Jobs**:
  1. `build-mac` (macos-14):
     - build `x86_64-apple-darwin` + `aarch64-apple-darwin`
     - `lipo` into universal binary
     - bundle via `packaging/mac/make-dmg.sh` → `Sonic-${VERSION}-mac-universal.dmg`
  2. `build-windows` (windows-latest):
     - build `x86_64-pc-windows-msvc`
     - `cargo wix` → `Sonic-${VERSION}-win-x64.msi`
  3. `publish` (needs both): `softprops/action-gh-release` uploads `.dmg` + `.msi`, body auto-generated from `git log` since previous tag
- **No code signing** (deferred). Notes added to README about right-click-open on mac and SmartScreen on win.

## 7. Default Keymap (WezTerm-compatible)
Ported to `assets/keymaps/wezterm.toml`. Highlights:
- `Cmd/Ctrl+T` new tab
- `Cmd/Ctrl+W` close tab
- `Cmd/Ctrl+Shift+Enter` toggle fullscreen
- `Cmd/Ctrl+D` split right
- `Cmd/Ctrl+Shift+D` split down
- `Cmd/Ctrl+Shift+H/J/K/L` focus left/down/up/right pane
- `Cmd/Ctrl+1..9` activate tab N
- `Cmd/Ctrl+Plus / Minus / 0` font size
- `Cmd/Ctrl+F` search
- `Cmd/Ctrl+Shift+P` command palette
- `Cmd/Ctrl+C / V` clipboard (with text-selection awareness)

## 8. Icon Design
Original SVG: stylized lightning-fast hedgehog silhouette in profile, blue→purple gradient (#3B82F6 → #8B5CF6), with three speed-trail lines behind. Squircle background for macOS, square for Windows. Master in `assets/icons/sonic.svg`; `bake-icons.sh` generates `.icns` (1024 down to 16) and `.ico` (256/128/64/48/32/16).
**Superseded** by `docs/brand/icon.md`: the shipped mark is a terminal window with `>_` prompt and cyan speed trails on a navy squircle (user-supplied master at `assets/icons/source/sonic.svg`). The hedgehog draft was never released.

## 9. Must-Pass Tests
```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-features
cargo deny check
```
All exit 0 on macOS + Windows.

## 10. Acceptance
- `cargo check --workspace` passes locally
- CI green on both platforms
- Tag `v0.1.0-alpha.1` triggers release.yml → GitHub Release with `.dmg` + `.msi` attached
- App launches, opens a PTY, displays shell prompt, accepts input, supports at least one tab and one split, loads WezTerm keymap, applies one bundled theme, shows the bundled icon

## 11. Risks
| Risk | Mitigation |
|---|---|
| wgpu+glyphon API churn | Pin minor versions; lockfile committed |
| ConPTY edge cases | `portable-pty` abstracts; documented quirks |
| WiX missing on runner | Install step in workflow uses `cargo install cargo-wix` cached |
| Mac universal lipo failure | Per-arch fallback artifact |

## 12. Telemetry (added on merge)
`Cost: ~N dispatches, ~M minutes wall-clock, models: claude-opus-4.7-xhigh (PM+dev) + gpt-5.5 (audit, tech lead)`
