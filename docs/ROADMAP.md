# Sonic Terminal Roadmap

Authoritative source for what's done, what's next, and the constraints any
contributor (human or agent) must respect. Update this file when shipping a
version or changing direction.

Last updated: 2026-05-25 (after `a11d9ef`, v0.2)

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
| Cargo workspace, 4 crates | ✅ | bootstrap |
| GitHub Actions CI (mac+win: fmt/clippy/test + deny) | ✅ | bootstrap |
| Release pipeline (tag → universal .dmg + x64 .msi) | ✅ | bootstrap |
| VT/ANSI parser (CSI cursor, ED, EL, SGR 256+truecolor, OSC 0/2/8/52) | ✅ | bootstrap |
| Grid model (scrollback, wide chars, resize) | ✅ | bootstrap |
| Cross-platform PTY (portable-pty) | ✅ | bootstrap |
| TabBar model (push/close/reorder/detach) | ✅ | bootstrap |
| PaneTree model (recursive splits, collapse-on-close) | ✅ | bootstrap |
| TOML config + 4 themes + WezTerm keymap | ✅ | bootstrap |
| Original SVG icon + bake script | ✅ | bootstrap |
| **wgpu+glyphon character rendering** | ✅ v0.2 | `sonic-shared/src/render.rs` |
| **Keyboard input → PTY** | ✅ v0.2 | `sonic-shared/src/app.rs::encode_logical` |
| **Per-cell color rendering** | ⏳ v0.3 | scaffold in `render.rs` (`cell_fg`, `indexed`) marked `#[allow(dead_code)]` |
| Cursor rendering | ⏳ v0.3 | — |
| Browser-style tab bar UI | ✅ v0.3c | `sonic-shared/src/tabbar_view.rs` + `render.rs` |
| Bound keymap actions | ⏳ v0.3 | model in `keymap.rs`; no dispatcher |
| Pane rendering (split layout in window) | ⏳ v0.3 | model in `pane.rs`; no draw |
| Selection + clipboard copy | ⏳ v0.3 | — |
| Tab tear-out + cross-window merge | ⏳ v0.4 | API hook in `TabBar::detach` |
| Half-transparent / blur backgrounds | ⏳ v0.4 | — |
| Ligatures, IME, link click | ⏳ v0.4 | — |
| Profile-guided perf, alt-screen, DEC modes | ⏳ v0.5 | — |
| **In-app graphical preferences UI** | ⏳ v0.4 | — |
| Code signing + auto-update | ⏳ v1.0 | — |

---

## Releases & milestones

### ✅ v0.1.0-alpha.1 — Bootstrap (commit `99e7c4a`)
Workspace, CI, release, parser, grid, PTY, theme/keymap/config loaders,
tab+pane models, icon, docs.

### ✅ v0.1.0-alpha.2 — Lint + bug fix (commit `3214d5c`)
Local test gate passing, fixed PaneTree::close() nested-collapse bug,
20→46 tests.

### ✅ v0.2.0 — Visible terminal (commit `a11d9ef`)
GPU rendering wired up — characters actually appear on screen, typing
into the window reaches the pty, basic event loop with resize.

### ⏳ v0.3.0 — Color, cursor, and chrome
Theme: **3 weeks** of focused work.
1. **Per-cell color**: upgrade glyphon to 0.11 (brings cosmic-text 0.13 +
   wgpu 0.20+); switch render path to `Buffer::set_rich_text(spans)`.
   Re-enable `render::cell_fg`/`indexed`. **Acceptance: `ls --color` shows
   colored output.**
2. **Cursor**: render a single colored quad at `(grid.cursor.row, .col)`
   using a tiny custom wgpu pipeline (vertex+frag, instanced rect). Blink
   driven by a 500 ms timer when `cursor_blink = true`.
3. **Bold / italic / underline**: map `CellFlags` into glyphon `Attrs`
   (`weight`, `style`, `Stretch` not used). Underline is a second quad pass.
4. **Tab bar UI**: draw a trapezoidal/rounded-rect bar at top of window
   using the wgpu quad pipeline + glyphon for titles. Mouse-click → activate
   tab; click `×` → close; click `+` → new tab. Hit-test via cell-space.
5. **Pane layout renderer**: walk `PaneTree`, recursively divide the
   content rect by `ratio` along `axis`. Each leaf gets its own grid +
   parser + pty (one PTY per pane). Refactor `App` so it owns
   `HashMap<paneId, PaneState>`.
6. **Keymap dispatcher**: before `encode_logical`, look up
   `keymap.lookup("super+t")` etc; if a binding matches, run the
   `Action`; otherwise fall through to byte encoding.
7. **Selection + copy**: mouse drag → grid coords → range; `Cmd+C` →
   serialize selection from grid → arboard clipboard.
8. Tests for everything new.

**Definition of done**: open Sonic, see colored `ls`, `Cmd+T` opens a
new tab, `Cmd+D` splits the pane, click a tab to switch, drag-select +
`Cmd+C` copies text.

### ⏳ v0.4.0 — Drag and polish
1. **Tab tear-out**: on macOS, hook NSDraggingSource on the tab bar; when
   drag exits the bar, create a new window seeded by `TabBar::detach()`.
   On Windows, use OLE drag + drop with custom CF format.
2. **Cross-window merge**: NSDraggingDestination / IDropTarget on the tab
   bar of every window. Dropping a tab inserts it into the target bar.
3. **Background opacity + blur**: macOS NSVisualEffectView behind the
   wgpu surface; Windows DwmExtendFrameIntoClientArea + Mica via
   `windows-sys` `SetWindowCompositionAttribute`.
4. **Ligatures**: enable in cosmic-text via `Shaping::Advanced` (already
   on); add font fallback chain.
5. **Link click**: OSC 8 hyperlinks are already parsed (`VtEvent::Hyperlink`);
   render as underlined + clickable; open via `open` / `start`.
6. **IME**: handle `WindowEvent::Ime` events; render pre-edit text inline.
7. **Command palette**: `Cmd+Shift+P` opens a floating list of actions
   pulled from the keymap.
8. **In-page search** (`Cmd+F`): match against the visible grid +
   scrollback; highlight hits with a quad pass.

(Graphical preferences UI moved to **v0.6** per user direction — it
warrants its own milestone because dropdowns, color pickers, theme
live-preview, and a separate settings window are a full subsystem.)

### ⏳ v0.6.0 — Graphical preferences UI
Dedicated milestone. Spec:

1. **Settings window**: opened by `Cmd+,`. Separate winit `Window`,
   rendered by a second `GpuRenderer` instance against the same
   wgpu+glyphon stack + quad pipeline. Sized ~720×560 logical, resizable.
2. **Layout**: a left-side category list (General / Appearance / Font /
   Keymap / Behavior) + right-side form panel.
3. **Form controls** (all built from quads + glyphon):
   - Toggle / switch (bool)
   - Slider (numeric range — opacity, font size, scrollback)
   - **Dropdown** (theme picker, font family, shell)
   - Color picker (hex input + 16-cell ANSI palette swatch grid)
   - Text field (free-form strings)
   - Keymap recorder (press key combo → captures)
4. **Theme preview**: the Appearance tab shows a mini terminal pane
   inside the settings window rendering a fixed sample (prompt + `ls`
   output + a colored diff) using the currently-selected theme.
   Switching the dropdown live-previews; "Apply" persists.
5. **Persistence**: each control writes through to in-memory `Config`,
   then on "Apply" serializes back to `~/.config/sonic/sonic.toml`.
   TOML stays canonical; the GUI is a typed editor on top.
6. **Hot reload**: existing terminal windows pick up the new config
   without restart via the `notify` watcher already in `sonic-core`.

### ⏳ v0.5.0 — Performance and completeness
1. **Damage tracking**: only rebuild glyphon Buffer for changed rows.
2. **Alt screen** (DEC `?1049h`/`l`) — required by vim, htop, less.
3. **More DEC modes**: cursor visibility (`?25`), bracketed paste
   (`?2004`), mouse SGR (`?1006`, `?1015`).
4. **Profile-guided optimization** on release builds.
5. **Sixel** + **Kitty graphics** support for image previews.
6. **CSI u** modern keyboard reporting.
7. **Bell**: visual flash + optional sound.

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
4. **Read the spec**: `docs/specs/2026-05-24-sonic-monorepo-bootstrap-design.md`.
5. **Follow `build-or-fix` skill**: usually one Standard or Complex PR per
   version-level item. Don't bundle unrelated work.
6. **Branch naming**: `feat/<topic>`, `fix/<topic>`, etc.
7. **Local gate before push**:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
8. **Test bar**: every behavior change ships with a unit test. Workspace
   target: **never let test count regress.** Current floor: **75** as of v0.3c.

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
- **wgpu / glyphon are coupled** — bumping one forces the other. As of
  v0.2: `wgpu 0.19` + `glyphon 0.5` + `cosmic-text 0.10`. Next coherent
  upgrade target: `wgpu 0.20+` + `glyphon 0.11+` + `cosmic-text 0.13+`.
- **Release profile uses fat LTO + 1 codegen unit + strip + panic=abort.**
  Don't relax for "build is slow." Use `[profile.dev]` for fast iteration.
- **Spec files live in `docs/specs/YYYY-MM-DD-<topic>-design.md`.**
  Update this `ROADMAP.md` when shipping.
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
`tracy` (cross-platform) once we hit v0.4.

---

## Open questions (decide before starting the relevant version)

- v0.3: Tab bar should be drawn via wgpu quads (consistent with terminal)
  or via native chrome (NSToolbar / WPF on win)? **Recommendation: wgpu
  quads** — keeps look identical across platforms.
- v0.4: Background blur — implement now or wait until we have a global
  effect pipeline? Probably wait; one-off per-platform code is fine.
- v0.5: Vendor `cosmic-text` fork or stay on upstream? Upstream until
  blocked.
