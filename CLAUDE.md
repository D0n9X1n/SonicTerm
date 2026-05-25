# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

The intent of this file is that **any Claude agent dropped into this repo can be productive in 5 minutes**. It is dense on purpose — read it all once before editing anything.

---

## 0. North Star (do not violate without asking)

A **GPU-accelerated, cross-platform terminal** for macOS + Windows.

- Performance first. Beat WezTerm if at all possible.
- Linux, code signing, auto-update, SSH, mux are **explicitly deferred** to v1.0.
- WezTerm-compatible keymap is the default.
- The icon is canonical and user-supplied — don't replace it.

The authoritative running-status doc is **`docs/ROADMAP.md`**. Read it first. Update it when you ship a milestone.

---

## 1. What ships and where it lives

### Crates (flat layout — at top of repo, NOT under `crates/`)

| Crate | Role | Key items |
|---|---|---|
| `sonic-core/` | Platform-agnostic engine | `vt::Parser` (vte + Performer), `grid::Grid` (cells, scrollback, wide chars, alt screen), `pty::PtyHandle`, `keymap::{Action, Keymap}`, `theme::Theme`, `config::Config`, `hyperlink::HyperlinkRegistry`, `url_open` |
| `sonic-shared/` | GPU rendering + app loop + UI models | `app::App` (winit ApplicationHandler), `render::GpuRenderer` (wgpu+glyphon), `quad::QuadPipeline`, `tabs::TabBar`, `tabbar_view::TabBarLayout`, `pane::PaneTree`, `selection::Selection`, `search::SearchState`, `prefs/` subsystem |
| `sonic-mac/` | macOS bin | `main.rs` is ~30 lines — loads config + `sonic_shared::run` |
| `sonic-windows/` | Windows bin | same shape |

### Assets

- `assets/icons/source/sonic.svg` — hand-authored SVG master (user-supplied final design). Variants: `sonic-mono.svg`, `sonic-glyph.svg`.
- `assets/icons/exports/` — committed PNG / .icns / .ico bakes. Regenerate with `bash assets/icons/bake-icons.sh` (needs `brew install librsvg`).
- `assets/themes/{tokyo-night,dracula,nord,catppuccin-mocha}.toml`
- `assets/keymaps/wezterm.toml`
- `assets/fonts/` — JetBrainsMono Nerd Font (provisioned by build / bake script).

### Docs

- `docs/ROADMAP.md` — ground truth for milestone status + constraints.
- `docs/brand/icon.md` — brand guide: palette, geometry, usage rules.
- `docs/specs/` — historical design specs (mark "superseded" rather than delete).

---

## 2. The local gate — run before every commit

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --example pty_dump -p sonic-core --release   # must print [e2e] OK
cargo build --release -p sonic-mac                     # confirms fat-LTO build works
```

`cargo-deny` runs in CI on Ubuntu (`cargo install cargo-deny --locked` + `cargo deny check` locally if you touched any dep). CI matrix is `macos-14` + `windows-latest` only.

**Test floor: never let workspace test count regress. Current floor = 171.** Watch the per-crate breakdown in `cargo test --workspace 2>&1 | grep "test result"` and confirm the sum.

### E2E binaries (use these to verify, not just unit tests)

- `cargo run --example pty_dump -p sonic-core --release` — spawns the user shell, runs `ls --color=always /` + a bold/italic/underline `printf`. **Exits non-zero if grid lacks colored/styled cells.** This is the canonical end-to-end gate; any VT/grid/PTY change must keep this green.
- `cargo run --example altscreen_smoke -p sonic-core --release`
- `cargo run --example pane_smoke -p sonic-shared --release`

---

## 3. Big-picture architecture

```
                          ┌──────────────────────────────┐
   shell stdout ──pty▶    │  Parser (vte + Performer)    │ ─▶ Grid (cells, scrollback,
   ▲                       │  sonic-core::vt              │     wide chars, alt screen)
   │                       └──────────────────────────────┘                │
   │                                                                       ▼
   PTY thread (16ms coalesce)                                       Grid is the
   ▲                                                                shared state
   │
   App (winit ApplicationHandler) ──▶ GpuRenderer (wgpu+glyphon) ──▶ frame
   ▲       sonic-shared::app             sonic-shared::render
   │
   keys/mouse  ──▶ keymap dispatcher (sonic_core::keymap::Action)
                   super+T new tab, super+D split, super+, prefs, etc.
```

**Per-pane state** (since v0.3d): each pane in a tab owns its own `Grid + Parser + PtyHandle`. Splitting a pane spawns a new shell. `impl Drop for PtyHandle` explicitly kills the child to prevent orphans (caught by Haiku review of PR #21).

**Rendering pipeline** has two layers per frame:
1. **glyphon** for shaped text — fed via `Buffer::set_rich_text(spans)` with one styled span per attribute run (color + weight + italic).
2. **Custom wgpu quad pipeline** (`sonic-shared/src/quad.rs`) for everything else — cursor, selection highlight, underlines, hyperlink tints, tab-bar chrome, search-match highlight, focused-pane border.

---

## 4. Land-mines (each was a real bug; do not re-introduce)

### Threading / event loop
- **Render uses `try_lock` on the parser, not `lock`.** Earlier `lock()` deadlocked the macOS main thread under shell-startup output bursts.
- **VT thread coalesces redraw requests to ≥16ms.** Otherwise the OS marks the app unresponsive.
- **No unconditional "heartbeat redraw" at the end of `window_event`.** It creates a feedback loop. Real triggers (pty bytes / mouse drag / key / resize) cover every case.

### Parser correctness
- **CSI `J` (ED) and `K` (EL) MUST honor the mode parameter.** `J0` is "erase below", `J1` is "above", `J2` is "all". The original code erased everything regardless — every shell prompt redraw wiped output. Regression: `tests/vt.rs::shell_prompt_redraw_preserves_above_cursor`.
- **CSI `?1049h` must be a no-op when already in alt screen.** Otherwise vim/fzf re-entry clobbers `saved_cursor`. Regression: `dec_1049h_repeated_does_not_clobber_saved_cursor`.
- **`PtyHandle::Drop` kills the child explicitly.** Just dropping the trait object doesn't terminate the shell.

### Security / safety
- **`sonic_core::url_open::validate()` is mandatory before spawning anything.** OSC 8 URIs come from untrusted pty output; on Windows `cmd /C start` re-tokenizes through cmd's parser. Allow-list: `http`, `https`, `mailto`, `file`. Denylist: `& | ^ < > " ' \` CR LF NUL` + other control chars. Length capped at 4096.

### Rendering
- **`wgpu::CurrentSurfaceTexture::Suboptimal(frame)` must drop the SurfaceTexture before calling `surface.configure(...)`.** Otherwise wgpu 29 panics ("texture still alive").
- **`set_text` vs `set_rich_text`**: per-cell color/weight/style requires `set_rich_text(spans, default_attrs, Shaping::Advanced, None)` — the cosmic-text 0.18 API.

### Coupling
- **`wgpu`, `glyphon`, `cosmic-text` are a coherent triple.** Bumping one forces the others. Current: `wgpu 29` + `glyphon 0.11` + `cosmic-text 0.18`. Don't upgrade just one.
- **Clippy is `all`, not `pedantic`/`nursery`.** The loud groups were tried and removed (noise > signal at this stage). Add `#[allow(...)]` selectively if needed.

---

## 5. Coding conventions

- **Prefer per-crate `tests/` folder** (one `.rs` per source module). PR #27 moved all of `sonic-core` + `sonic-shared`'s pre-v0.6 tests out of source files. **New tests should follow this pattern.** The `sonic-shared/src/prefs/` subsystem (layout.rs, controls.rs, state.rs) still has inline `#[cfg(test)] mod tests {}` blocks from v0.6 and is the known exception — feel free to migrate them when you next touch that area, but it's not blocking.
- **Test-only items that must remain accessible to integration tests stay `pub` with `#[doc(hidden)]`.** No `__test_support` shim modules — that pattern was explicitly removed.
- **Public API for actions**: adding a new bindable user action means adding a variant to `sonic_core::keymap::Action` AND a match arm in `sonic_shared::app::App::run_action`.
- **Conventional Commits** with scope: `feat(v0.3d): ...`, `fix(vt): ...`, `chore(deps): ...`, `docs: ...`.
- **Commit trailer**: every Claude-authored commit ends with:
  ```
  Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
  ```

---

## 6. The shipping workflow (multi-PR + multi-agent)

This repo uses an **agent-driven PR pipeline**. The PM agent (you, in a Claude Code session) dispatches sub-agents for implementation and review.

### Roles

| Agent | Model | Job |
|---|---|---|
| PM | foreground Claude | Picks scope, dispatches workers, merges, fixes rebase conflicts, talks to user |
| Implementer | `opus` sub-agent | Writes the feature on its own branch + opens the PR |
| Reviewer | `haiku` sub-agent | Reviews PR diff + pulls + runs tests + posts `APPROVED` / `CHANGES REQUESTED` PR comment |

### Per-PR loop

1. PM picks one well-scoped task (one milestone item).
2. **Dispatch Opus implementer** in the background via the Agent tool. Prompt MUST include:
   - "NO polling — go immediately."
   - Working dir `cd /tmp && rm -rf <name> && git clone -b main git@github.com:D0n9X1n/sonic.git <name> && cd <name> && git checkout -b <branch> && git config user.email noreply@anthropic.com && git config user.name "Claude Opus 4"`.
     **Always work in a `/tmp/<scratch>` clone, not in `/Users/d0n9x1n/Workspace/fun-code/sonic`** — multiple agents running in parallel will trash each other's working tree otherwise.
   - The complete local gate (fmt + clippy + test + pty_dump e2e + release build).
   - "Up to 3 fix cycles. Reply with PR URL + 1 line when done."
3. **Dispatch Haiku reviewer** when the PR opens. Haiku reads the diff, pulls a fresh clone, runs tests, posts the verdict as a PR comment via `gh pr comment <N> -R D0n9X1n/sonic --body "..."`, and replies to PM with verdict + URL.
4. PM acts on the verdict:
   - `APPROVED` → `gh pr merge <N> -R D0n9X1n/sonic --squash --admin --delete-branch`.
   - `CHANGES REQUESTED` → either dispatch Opus again with the specific feedback OR fix it directly (small bugs). Then re-dispatch Haiku.

### When `gh pr merge` says "not mergeable"

That's a real merge conflict with main. **The local working tree is not the source of truth.** Use a fresh clone:
```bash
cd /tmp && rm -rf rebase-NN
git clone -b <pr-branch> git@github.com:D0n9X1n/sonic.git rebase-NN
cd rebase-NN
git config user.email noreply@anthropic.com && git config user.name "Claude Opus 4"
git fetch origin main:main
git rebase main
# resolve conflicts; run the gate; cargo test must still print exactly N tests
git push --force-with-lease
gh pr merge <N> -R D0n9X1n/sonic --squash --admin --delete-branch
```

### Parallelism rules

- **Truly independent files only.** Two PRs that both touch `render.rs` or `app.rs` WILL conflict — serialize them.
- Safe parallel: `sonic-core/src/hyperlink.rs` (new) + `sonic-shared/src/search.rs` (new) + `sonic-shared/src/quad.rs` (existing).
- Risky parallel: anything touching render.rs / app.rs / Cell.
- Documentation work is independent of code — always parallelizable.

### CI status

Per user direction, **CI status is not a merge blocker for these admin-merged PRs** — the local gate is. Use `--admin` on `gh pr merge`. Still fix CI failures in a follow-up PR (cargo-deny license updates, action-version bumps, etc.) so the badge stays green.

### Background-agent pitfalls

- Background agents that say "I'll wait until X merges, polling every 60s" will silently bail after one tick. **Don't ask agents to wait.** Either pre-merge the dependency, or dispatch the agent AFTER the dependency lands.
- The system reminder "task tools haven't been used recently" is informational — only spawn TaskCreate/TaskUpdate calls when they're useful for tracking, not in response to the reminder alone.

---

## 7. Useful one-liners

```bash
# What's open?
gh pr list -R D0n9X1n/sonic --state open --json number,title,headRefName

# What's on main vs my branch?
git log --oneline origin/main..HEAD

# Latest CI run on a branch
gh run list -R D0n9X1n/sonic --branch <branch> --limit 1

# Quick fmt+clippy after a single-file edit
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings

# Total workspace test count (sum across all binaries)
cargo test --workspace 2>&1 | grep "test result" | awk -F'[ .,]' '{s+=$5} END {print "TOTAL:", s}'
```

---

## 8. Configuration runtime

User config lives at:
- macOS: `~/Library/Application Support/Sonic/sonic.toml`
- Windows: `%APPDATA%\Sonic\sonic.toml`

Bundled defaults: `assets/themes/*.toml` + `assets/keymaps/wezterm.toml`. The keymap action enum is the public surface — adding a bindable action requires a variant + dispatcher arm (see §5).

---

## 9. Release

```bash
git tag v0.3.0 && git push origin v0.3.0
```

triggers `.github/workflows/release.yml` → produces a universal macOS `.dmg` + x64 Windows `.msi`. No code signing yet (deferred). The release workflow installs `librsvg + imagemagick` then runs `bash assets/icons/bake-icons.sh` so the bundles always carry the fresh icon.

---

## 10. When you're stuck

- Default to consulting the user, not guessing — the project has a real human PM driving direction.
- Real correctness bugs go in a fresh fix-up commit on the PR branch (don't squash silently).
- If you find yourself fighting `git branch --show-current` reporting one thing while you're really on another, you're probably in the wrong working directory. Always use a fresh `/tmp/` clone for each PR's work.
