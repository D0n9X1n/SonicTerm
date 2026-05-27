# Sonic Terminal Roadmap

Authoritative source for what's done, what's next, and the constraints any
contributor (human or agent) must respect. Update this file when shipping a
version or changing direction.

Last updated: 2026-05-27 (v1.0-RC — 29+ PRs this session: crate decomposition #145/#151–#160, render+app module split #157/#160, Windows MVP #133–#139, perf pass #129–#142, P0 ANSI-bg fix #163. Release notes in [`CHANGELOG.md`](../CHANGELOG.md); tag pending operational cert procurement and honest perf-parity sign-off; see [`RELEASE.md`](../RELEASE.md))

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
| Cargo workspace, 10 leaf crates under `crates/` (post-#145, #151–#158) | ✅ | `crates/` |
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
| Tab tear-out + cross-window merge | ✅ v0.8 | `sonic-shared/src/tabs.rs` (#43, #48, #59, #62, #64) |
| Command palette (`super+shift+P`) | ✅ v0.8 | (#41, #45) |
| IME composition + preedit anchoring | ✅ v0.8 | (#40, #50) |
| In-page + scrollback search | ✅ v0.8 | (#51) |
| Bracketed paste + OSC 133 shell-integration | ✅ v0.8 | (#52) |
| Font / theme / keymap live-reload + prefs persist | ✅ v0.8 | (#53, #54) |
| i18n (en / zh-CN / ja) | ✅ v0.8 | (#55) |
| `sonic-mux` daemon (persistent PTY sessions) | ✅ v0.8 | `sonic-mux/` (#56) |
| Programming ligatures + ZWJ shaping | ✅ v0.8 | (#57) |
| SSH client pane (russh, feature-gated) | ✅ v0.8 | (#58) |
| Native macOS menubar | ✅ v0.8 | (#66) |
| B3 atlas renderer + headless GUI bench | ✅ v0.8 | (#42, #44, #74) |
| WezTerm visual parity (≤ 3 ΔE on standard recipe) | ✅ v0.8 | (#70, #75) |
| Windows MVP (MSI, titlebar+Mica, menu, OLE drag, fg-proc probe) | ✅ v1.0 | (#133, #134, #135, #137, #139) |
| Renderer + VT + PTY perf pass (8 wins) | ⏳ partial | landed: #129, #130, #131, #132, #136, #138, #140, #141, #142; still 6–302× behind WezTerm on vtebench — Phase E continues |
| Crate decomposition (sonic-core → sonic-{vt,grid,cfg,io} + sonic-{types,text,ui,render-model,gpu,app}) | ✅ v1.0-RC | (#145, #151, #152, #153, #154, #155, #156, #157, #158, #160) |
| Per-cell ANSI background colors render correctly (P0 regression fix) | ✅ v1.0-RC | (#161 spec → #163 fix) |
| PTY burst flag converted to generation counter (race fix) | ✅ v1.0-RC | (#162) |
| Default font switched to St Helens (system-installed, not bundled) | ✅ v1.0-RC | (#148) |
| Code-signing pipeline (Azure Trusted Signing + macOS notarization) | ✅ pipeline / ⏳ certs | (#39, #128); certs pending procurement |
| Honest perf parity with WezTerm on vtebench | ⏳ v1.x | — |
| Half-transparent / blur backgrounds | ⏳ | — |
| Auto-update (Sparkle / WinSparkle) | ⏳ post-v1.0 | — |
| Linux re-enable, session restore | ⏳ post-v1.0 | — |

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

### ✅ v0.8.0 — Production polish (2026-05-26)

The bridge from v0.6 to v1.0. Everything between "preferences ship" and
"ready to sign + release to the public" lands here. Full per-PR detail
in [`CHANGELOG.md`](../CHANGELOG.md); cut script in [`RELEASE.md`](../RELEASE.md).

Highlights:
1. **Idle CPU 99% → ~0%** — render `try_lock` spin fixed (#37).
2. **B3 atlas renderer is default** (#42) with headless GUI bench (#44),
   capability matrix regression net (#47), and pixel-diff visual
   snapshot harness (#74).
3. **Tab tear-out + cross-window drag-to-merge** including OS-level
   `NSPasteboard` cross-process drag (#43, #48, #59, #62, #64).
4. **Command palette, in-page/scrollback search, IME with anchored
   preedit, visual overlays** (#40, #41, #45, #50, #51).
5. **WezTerm visual parity within 3 ΔE** on the standard recipe (#70,
   #75) — HiDPI physical-px rasterization (#63, #72, #76), sRGB→linear
   gamma (#65), CJK / emoji / Hangul / Powerline / ZWJ (#49, #57, #68).
6. **Live reload** of font/theme/keymap + prefs persist & live-apply
   (#53, #54).
7. **`sonic-mux` daemon** for persistent PTY sessions (#56).
8. **SSH client pane** behind `ssh` feature flag (#58).
9. **Native macOS menubar** (#66) + WezTerm-style tab titles (#77).
10. **i18n** en / zh-CN / ja (#55).
11. **Code-signing workflow** infra (#39) — gated on secrets; certs
    deferred to v1.0.
12. **Bracketed paste + OSC 133** (#52), DSR/DA replies fix nvim hang
    (#61).
13. **Docs**: README/USER_GUIDE overhaul (#60), TESTING.md (#67),
    VISUAL_PARITY.md (#70), CI-BILLING.md (#73).

### ⏳ v1.0-RC — Production candidate (IN PROGRESS, 2026-05-27)

29+ PRs landed in this session; release notes staged in
[`CHANGELOG.md`](../CHANGELOG.md). Tag pending operational cert
procurement AND an honest perf-parity sign-off (see below).

PRs landed this session:

- **Crate decomposition (refactor)**: #145 (move to `crates/` nesting),
  #151 (extract `sonic-types`), #152 (split `sonic-core` into
  `sonic-vt` / `sonic-grid` / `sonic-cfg` / `sonic-io` + façade), #153
  (extract `sonic-text`: shape + atlas + raster), #154 (extract
  `sonic-ui`: tabs / panes / search / palette / IME / prefs / i18n),
  #155 (extract `sonic-render-model`), #156 (extract `sonic-gpu`),
  #157 (split `render.rs` → `render/{color,metrics,tab_spans,cursor,drag_chip,core}.rs`),
  #158 (extract `sonic-app`), #160 (split `app.rs` into 16 modules
  under `app/`).
- **P0 correctness fix**: #161 (spec) → #163 (fix) — per-cell ANSI
  background colors were silently dropped before reaching the text
  pipeline; htop/tmux/fzf rendered with theme bg instead of cell bg.
- **PTY race fix**: #162 — `input_dirty` flipped from bool to
  generation counter so the renderer can't lose a burst that lands
  between "clear flag" and "draw frame".
- **Default font**: #148 — `St Helens` is the new default (system,
  not bundled); `Rec Mono Casual` remains as guaranteed fallback.
- **Windows MVP**: #133 (MSI pipeline), #134 (foreground-process
  probe), #135 (cross-platform menu abstraction), #137 (custom
  titlebar + Mica backdrop), #139 (OLE drag-drop for tab tear-out +
  file drop).
- **Renderer / VT / PTY perf**: #129 (4k LRU shape cache), #130
  (dirty bitset foundation), #131 (zero-copy PTY BytesMut ring),
  #132 (vsync pacing via `ControlFlow::WaitUntil`), #136 (atlas LRU
  eviction), #138 (SWAR ASCII fast-path), #140 (per-row glyph cache),
  #141 (VecDeque visible rows), #142 (pre-baked box-drawing +
  Powerline glyphs).
- **Signing pipeline**: #128 (Azure Trusted Signing for Windows +
  macOS notarization plumbing).

#### Honest perf status

The 8 perf wins closed ~30–60% of the gap on cat-large-file and
tail-f hot paths. They did **not** achieve WezTerm parity. Current
`vtebench` numbers vs WezTerm on the same hardware show Sonic
**6×–302× slower** depending on the benchmark — worst on heavy SGR
attribute streams and dense scrollback writes (see
`/tmp/sonic-vs-wezterm.md` notes). Phase E (perf parity) is ongoing,
not done. Do not call this "complete" in commit messages or PR
bodies.

Remaining gates before the v1.0 tag goes out:

1. **Honest perf-parity sign-off** — vtebench within ~2× of WezTerm
   on the standard suite. This is the substantive blocker.
2. **macOS signing + notarization** — Apple Developer Program ($99/yr).
   Workflow infra in place (#39 + #128); add the secrets and re-tag.
3. **Windows signing** — Azure Trusted Signing tenant + cert.
   Workflow infra ready (#128); secrets pending.

Still deferred past v1.0:

4. **Auto-update**: Sparkle on macOS, Squirrel or WinSparkle on Windows.
5. **Session restore**: persist tab/pane layouts to disk on shutdown,
   restore on next launch (complements `sonic-mux` from v0.8).
6. **Linux support**: re-enable `sonic-linux`, add to CI matrix and
   release pipeline, AppImage + `.deb`.
7. **Half-transparent / blur backgrounds** (rolled forward from v0.8).

---

## How to pick up work (for the next agent)

1. **Pull main**: `git pull origin main`.
2. **Check current state**: this file, `CHANGELOG.md`, `git log --oneline -20`.
3. **Pick the highest-priority unchecked item from the next pending version.**
   If unsure, ask the user before starting.
4. **Follow `build-or-fix` skill**: usually one Standard or Complex PR per
   version-level item. Don't bundle unrelated work.
5. **Branch naming**: `feat/<topic>`, `fix/<topic>`, etc.
6. **Local gate before push**:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
7. **Test bar**: every behavior change ships with a unit/integration test
   in the relevant crate's `tests/` folder. Workspace target: **never let
   test count regress.** Current floor: **824** as of v1.0-RC.

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
  `toml 1`. Coordinate the next upgrade across all three.
- **Release profile uses fat LTO + 1 codegen unit + strip + panic=abort.**
  Don't relax for "build is slow." Use `[profile.dev]` for fast iteration.
- **Spec files**: kept in PRs/commit messages alongside the
  [user guide](USER_GUIDE.md), not as standalone spec entries in this
  repo. Update this `ROADMAP.md` when shipping.
- **Crates live under `crates/`** — flat top-level layout was used between
  `9c46c39` and #145; PR #145 restored the nested `crates/<name>/`
  layout to keep room for the leaf decomposition (#151–#158). Don't
  flatten again without an explicit plan.
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
