# Sonic Terminal Roadmap

Authoritative source for what's done, what's next, and the constraints any
contributor (human or agent) must respect. Update this file when shipping a
version or changing direction.

Last updated: 2026-05-25 (after `7f8b83c`, v0.6 + per-crate `tests/` split)

---

## North Star

A **GPU-accelerated, cross-platform (macOS + Windows first) terminal** that:
- starts in <100 ms
- handles `cat largefile` / `tail -f` without dropping frames
- ships with WezTerm-compatible keybindings, Nerd Font, and beautiful themes
- has a polished tab bar (browser-style) with tab tear-out and cross-window merge
- target performance: meet or beat WezTerm on the same hardware

Linux is **deferred**. SSH / mux / Sixel / Kitty graphics are deferred.

---

## Status snapshot (per commit)

| Capability | Status | Where |
|---|---|---|
| Cargo workspace, 4 crates (flat top-level layout) | ✅ | bootstrap |
| GitHub Actions CI (mac+win: fmt/clippy/test + deny) | ✅ | bootstrap |
| Release pipeline (tag → universal .dmg + x64 .msi) | ✅ | bootstrap |
| VT/ANSI parser (CSI cursor, ED, EL, SGR 256+truecolor, OSC 0/2/8/52) | ✅ | bootstrap |
| Grid model (scrollback, wide chars, resize) | ✅ | bootstrap |
| Cross-platform PTY (portable-pty) | ✅ | bootstrap |
| TabBar model (push/close/reorder/detach) | ✅ | bootstrap |
| PaneTree model (recursive splits, collapse-on-close) | ✅ | bootstrap |
| TOML config + 4 themes + WezTerm keymap | ✅ | bootstrap |
| Original SVG icon (terminal window + cyan speed trails + `>_`) | ✅ v0.3 | `assets/icons/source/sonic.svg` |
| **wgpu+glyphon character rendering** | ✅ v0.2 | `sonic-shared/src/render.rs` |
| **Keyboard input → PTY** | ✅ v0.2 | `sonic-shared/src/app.rs::encode_logical` |
| Cursor rendering + selection + keymap dispatcher + clipboard | ✅ v0.3a | `sonic-shared/src/app.rs` |
| Per-cell color + bold / italic / underline | ✅ v0.3b | `sonic-shared/src/render.rs` |
| Browser-style tab bar UI | ✅ v0.3c | `sonic-shared/src/tabbar_view.rs` + `render.rs` |
| Bound keymap actions | ✅ v0.3d | `sonic-shared/src/app.rs::run_action` (Split/Close/Focus wired) |
| Pane rendering + per-pane PTY | ✅ v0.3d | `sonic-shared/src/pane.rs` + `render.rs` border pass |
| OSC 8 hyperlinks (registry + URL opener) | ✅ v0.4 | `sonic-core::vt` + `sonic-shared` |
| OSC 8 visual + Cmd-click activation | ✅ v0.4 | `sonic-shared/src/render.rs` + `app.rs` |
| In-page search (`Cmd+F`) | ✅ v0.4 | `sonic-shared/src/search.rs` |
| Alt-screen + DEC `?1049` / `?47` / `?25` / `?2004` / `?1006` | ✅ v0.5 | `sonic-core::vt` |
| **In-app graphical preferences UI subsystem** | ✅ v0.6 | `sonic-shared/src/prefs/` (controls + state + `super+comma`; in-window rendering deferred) |
| Tab tear-out + cross-window merge | ⏳ | API hook in `TabBar::detach` |
| Half-transparent / blur backgrounds | ⏳ | — |
| Ligatures, IME, command palette | ⏳ | — |
| In-window preferences control rendering | ⏳ | follow-up to v0.6 |
| Code signing + notarization + auto-update | ⏳ v1.0 | — |
| Linux re-enable, built-in SSH, session restore | ⏳ v1.0 | — |

---

## Releases & milestones

### ✅ v0.1.0-alpha.1 — Bootstrap (commit `99e7c4a`)
Workspace, CI, release, parser, grid, PTY, theme/keymap/config loaders,
tab+pane models, icon, docs.

### ✅ v0.1.0-alpha.2 — Lint + bug fix (commit `3214d5c`)
Local test gate passing, fixed `PaneTree::close()` nested-collapse bug,
20→46 tests.

### ✅ v0.2.0 — Visible terminal (commit `a11d9ef`)
GPU rendering wired up — characters actually appear on screen, typing
into the window reaches the pty, basic event loop with resize.

### ✅ v0.3.x — Color, cursor, chrome, splits
- **v0.3a** (`bcacfcd`): cursor rendering, mouse selection, keymap
  dispatcher, clipboard.
- **v0.3b** (#11): per-cell foreground color + bold / italic / underline
  via glyphon `Attrs`. Verified by `ls --color`.
- **v0.3c** (#19): browser-style tab bar UI — trapezoidal tabs,
  click-to-activate, `×` to close, `+` to add.
- **v0.3d** (#21): pane layout renderer + per-pane PTY. `PaneTree`
  walked, content rect divided by `ratio`/`axis`, each leaf owns its
  own grid + parser + PTY.

### ✅ v0.4.0 — Hyperlinks + search
- **OSC 8 hyperlinks** (#20, #22): data layer (registry + URL opener)
  plus visual underline and Cmd-click activation.
- **In-page search** (`Cmd+F`, #23): match against the visible grid +
  scrollback; highlight hits with a quad pass.

Tab tear-out, IME, command palette, background blur — **deferred** past
v0.4; will be picked up in the chrome/polish slot after the v1.0
production blockers are clear.

### ✅ v0.5.0 — Alt-screen + DEC modes (#24)
- Alt screen (`?1049h/l`, `?47h/l`) so vim / htop / less restore the
  primary buffer cleanly.
- Cursor visibility (`?25`), bracketed paste (`?2004`), SGR mouse
  (`?1006`).

### ✅ v0.6.0 — Graphical preferences UI subsystem (#26)
Shipped:
1. **Settings state**: typed `Prefs` model owning every config tunable.
2. **Form controls** built on the existing quad + glyphon stack:
   toggle / switch, slider, dropdown, color picker, text field, keymap
   recorder.
3. **Binding**: `super+comma` (Cmd+, on macOS) → `open_preferences`
   action wired through `assets/keymaps/wezterm.toml`.

Deferred to a follow-up: a dedicated settings window with in-window
control rendering, theme live-preview, and config hot-reload through the
`notify` watcher. The state and controls already land; surfacing them
in their own window is the next slice.

### ⏳ v1.0.0 — Production
1. **macOS signing + notarization** (requires Apple Developer Program $99/yr).
2. **Windows signing** (requires EV cert, $200-400/yr).
3. **Auto-update**: macOS uses Sparkle; Windows uses Squirrel or WinSparkle.
4. **Built-in SSH** (via `russh`) — optional, behind a feature flag.
5. **Session mux + restore**: persist tabs/panes to disk; restore on next launch.
6. **Linux support**: re-enable `sonic-linux` crate, add to CI matrix and
   release pipeline, AppImage + .deb.

---

## How to pick up work (for the next agent)

1. **Pull main**: `git pull origin main`.
2. **Check current state**: this file, `CHANGELOG.md`, `git log --oneline -20`.
3. **Pick the highest-priority unchecked item from the next pending version.**
   If unsure, ask the user before starting.
4. **Read the spec**: `docs/specs/2026-05-24-sonic-monorepo-bootstrap-design.md`
   (parts of it are now superseded by this ROADMAP — header at the top of
   the spec marks what still applies).
5. **Follow `build-or-fix` skill**: usually one Standard or Complex PR per
   version-level item. Don't bundle unrelated work.
6. **Branch naming**: `feat/<topic>`, `fix/<topic>`, etc.
7. **Local gate before push**:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
8. **Test bar**: every behavior change ships with a unit/integration test
   in the relevant crate's `tests/` folder. Workspace target: **never let
   test count regress.** Current floor: **171** as of v0.6 + per-crate
   `tests/` split (#27).

---

## Constraints (do NOT violate without asking)

- **No Linux work** until v1.0.
- **No signing** until v1.0.
- **No nightly Rust** — stable only (`rust-toolchain.toml`).
- **No `unsafe`** outside platform shim layers in `sonic-mac` / `sonic-windows`,
  and even there only when wrapping a platform API. Workspace lint
  `unsafe_op_in_unsafe_fn = "warn"` is on.
- **Clippy is `all`, not `pedantic`/`nursery`.** Don't re-enable the loud
  groups — we tried, it drowned signal. Add selective allows if needed.
- **Dependabot is patch-only.** Major bumps are done by hand in a feat PR
  with a passing CI run.
- **wgpu / glyphon / cosmic-text are coupled** — bumping one forces the
  others. Current pinned stack (#10): `wgpu 29` + `glyphon 0.11` +
  `cosmic-text 0.18` + `vte 0.15` + `winit 0.30` + `thiserror 2` +
  `toml 1.1`. Coordinate the next upgrade across all three.
- **Release profile uses fat LTO + 1 codegen unit + strip + panic=abort.**
  Don't relax for "build is slow." Use `[profile.dev]` for fast iteration.
- **Spec files live in `docs/specs/YYYY-MM-DD-<topic>-design.md`.**
  Update this `ROADMAP.md` when shipping.
- **Crates live at the top level** (`sonic-core/`, `sonic-shared/`,
  `sonic-mac/`, `sonic-windows/`). The old `crates/` nesting was removed
  in `9c46c39`. Don't reintroduce it.
- **Tests go in each crate's `tests/` folder** (integration-style), not
  inline `#[cfg(test)] mod tests` blocks (#27).
- **Commit format**: Conventional Commits with scope =
  `core` / `shared` / `mac` / `windows` / `ci` / `assets` / `docs`.

---

## Reference WezTerm features we explicitly want (eventually)

Use https://wezfurlong.org/wezterm/ as the gold standard for shortcuts,
config keys, and behaviors when in doubt. Notable items worth porting:

- Quick select mode (`Cmd+Shift+Space`)
- Copy mode (vim-style navigation of scrollback)
- Hyperlink rules (regex → clickable URL)
- Background image / gradient
- Per-domain ("unix domain") config — defer to v1.0+
- Workspace concept — defer; not all WezTerm semantics needed here

---

## Reference performance targets

- Cold start to first prompt visible: **< 100 ms** on Apple Silicon
- Steady-state `cat /usr/share/dict/words` scroll: **60 FPS, no drops**
- Keyboard latency (key down → glyph on screen): **< 16 ms** (1 frame)
- Memory at idle, single tab: **< 80 MB RSS**
- Memory at idle, 8 tabs: **< 200 MB RSS**

Profile with `cargo flamegraph` and `cargo instruments` (mac) /
`tracy` (cross-platform).

---

## Open questions (decide before starting the relevant version)

- v1.0: Auto-update — Sparkle vs a Rust-native solution? Sparkle is the
  proven path but adds an Obj-C dep.
- v1.0: Built-in SSH — feature flag vs always-on? Lean feature flag so
  default builds stay lean.
- Post-v0.6: Vendor `cosmic-text` fork or stay on upstream? Upstream
  until blocked.
