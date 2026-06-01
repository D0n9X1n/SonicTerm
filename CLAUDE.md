# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

The intent of this file is that **any Claude agent dropped into this repo can be productive in 5 minutes**. It is dense on purpose — read it all once before editing anything.

---

## 0. North Star (do not violate without asking)

A **GPU-accelerated, cross-platform terminal** for macOS + Windows.

- Performance first. Beat WezTerm if at all possible. We are not there yet — see §14.
- Linux, code signing (cert procurement), auto-update, session restore are **explicitly deferred** past v1.0.
- WezTerm-compatible keymap is the default.
- The icon is canonical and user-supplied — don't replace it.

The authoritative running-status doc is **`docs/ROADMAP.md`**. Read it first. Update it when you ship a milestone.

---

## 1. What ships and where it lives

### Crates (under `crates/` directory — restored to nested layout in PR #145)

Pre-#145 the crates lived flat at the repo root. The reorganization in PR #145 moved everything under `crates/`, and PRs #151–#158 then decomposed the original `sonicterm-core` + `sonicterm-shared` monoliths into ten leaf crates plus two thin façades. New code should depend on the leaf crate directly; `sonicterm-core` remains as a deprecated re-export shim for back-compat.

| Crate | Role | Key items |
|---|---|---|
| `crates/sonicterm-types/` | Zero-dep value types | `Cell`, `Pos`, `Action`, `GlyphKey`, `HyperlinkId`, geometry primitives (#151) |
| `crates/sonicterm-vt/` | VT/ANSI parser | `vt::Parser`, `vt::Performer` (vte + Performer), SWAR ASCII fast-path (#152, #138) |
| `crates/sonicterm-grid/` | Terminal grid + scrollback | `grid::Grid` (cells, scrollback, wide chars, alt screen), `hyperlink::HyperlinkRegistry`, dirty bitset (#152, #130) |
| `crates/sonicterm-cfg/` | Config / theme / keymap / URL safety | `config::Config`, `theme::Theme`, `keymap::{Action, Keymap}`, `url_open::validate` (#152) |
| `crates/sonicterm-io/` | PTY + process probes + SSH | `pty::PtyHandle`, `proc_info`, `foreground_proc` (Windows), `ssh` (feature-gated) (#152) |
| `crates/sonicterm-text/` | Shaping + atlas | `shape` (LRU shape cache), `swash_rasterizer`, `glyph_atlas`, `row_glyph_cache` (#153) |
| `crates/sonicterm-render-model/` | Renderer-agnostic frame model | `geometry`, `inputs`, `painter` traits — what to draw, not how (#155) |
| `crates/sonicterm-ui/` | UI widgets & overlays | `tabs`, `tabbar_view`, `pane`, `selection`, `search`, `command_palette`, `ime`, `cursor`, `i18n` (#154) |
| `crates/sonicterm-gpu/` | wgpu pipelines | `quad::QuadPipeline`, `text_pipeline`, `atlas_upload` (#156) |
| `crates/sonicterm-app/` | Winit app loop + platform glue | `app::App` (winit ApplicationHandler) split across `app/{mod,window_event,event_loop,spawn_pane,keymap_dispatch,key_encoding,input,redraw,overlays,tab_state,tear_out,child_window,config_apply,search_handle,misc}.rs`; `menu`, `os_drag`, `tab_drag`, `config_watch` (#158, #160) |
| `crates/sonicterm-core/` | **Deprecated façade** | re-exports `sonicterm_vt::vt`, `sonicterm_grid::{grid,hyperlink}`, `sonicterm_cfg::{config,theme,keymap,url_open}`, `sonicterm_io::{pty,proc_info,ssh,foreground_proc}` for back-compat |
| `crates/sonicterm-shared/` | **Thin façade** | re-exports `sonicterm_ui::*` + `render/` module split across `render/{mod,core,color,metrics,tab_spans,cursor,drag_chip}.rs` (#157) |
| `crates/sonicterm-mac/` | macOS bin | `main.rs` is ~30 lines — loads config + `sonicterm_shared::run` |
| `crates/sonicterm-windows/` | Windows bin | same shape |
| `crates/sonicterm-mux/` | Persistent PTY mux daemon | shipped v0.8 (#56), feature-gated remote attach is post-v1.0 |

See **`docs/ARCHITECTURE.md`** for the full dep graph.

### Assets

- `assets/icons/source/sonic.svg` — hand-authored SVG master (user-supplied final design). Variants: `sonic-mono.svg`, `sonic-glyph.svg`.
- `assets/icons/exports/` — committed PNG / .icns / .ico bakes. Regenerate with `bash assets/icons/bake-icons.sh` (needs `brew install librsvg`).
- `assets/themes/{tokyo-night,dracula,nord,catppuccin-mocha,gruvbox-dark-hard,wezterm}.toml`
- `assets/keymaps/sonicterm.toml`
- `assets/fonts/` — `Rec Mono Casual` shipped as guaranteed-present fallback. **Default font is `St Helens`** (#148), system-installed, not bundled — renderer falls through to system mono if missing.

### Docs

- `docs/ROADMAP.md` — ground truth for milestone status + constraints.
- `docs/ARCHITECTURE.md` — 10-crate dep graph and module split rationale.
- `docs/brand/icon.md` — brand guide: palette, geometry, usage rules.
- `docs/specs/` — historical design specs (mark "superseded" rather than delete).

---

## 2. The local gate — run before every commit

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/check-no-raw-process-exit.sh                      # exit logging gate (no raw process::exit in shipped code)
bash scripts/check-deny.sh                                     # supply-chain gate (advisories + licenses + bans + sources)
cargo run --example pty_dump -p sonicterm-core --release           # must print [e2e] OK
cargo run --example pty_dump_unicode -p sonicterm-core --release   # must print [unicode-e2e] OK
bash scripts/check-visual-snapshots.sh                         # render dHash drift gate (closes #283)
cargo build --release -p sonicterm-mac                             # confirms fat-LTO build works
bash scripts/bench.sh                                          # perf-bench subset vs baseline.json; warns locally, exits 1 in CI
```

`cargo-deny` is wired into the local gate via `scripts/check-deny.sh` (install with `cargo install cargo-deny --locked`). The policy lives in `deny.toml` at the repo root — advisories deny-on-warn, licenses are an explicit allowlist, duplicate versions warn, and only crates.io is an allowed source. CI wiring lands in a follow-up PR after the in-flight workflow refactor; until then the local gate is authoritative. CI matrix is `macos-14` + `windows-latest` only.

**Two-tier CI** (PR #N): the **PR/main quick gate** runs `fmt + clippy + cargo test --workspace --lib --bins` (unit + bin tests only — integration tests under each crate's `tests/` folder are skipped for speed, ~2-3 min target). The **release gate** runs on `v*` tag push only and adds full `cargo test --workspace` (incl. integration), both `pty_dump` e2e examples, `scripts/bench.sh` in CI mode, and the unsigned `.dmg` + `.msi` build/publish. Integration tests must therefore be exercised locally before tagging a release (the pre-commit gate in this section still requires `cargo test --workspace`).

**Test floor: never let workspace test count regress. Current floor = 878** (post-#143 quality polish + per-pane resize regression tests in `crates/sonicterm-app/tests/per_pane_resize.rs`; was 824 at #160 split, 171 at v0.6). Watch the per-crate breakdown in `cargo test --workspace 2>&1 | grep "test result"` and confirm the sum.

### E2E binaries (use these to verify, not just unit tests)

- `cargo run --example pty_dump -p sonicterm-core --release` — spawns the user shell, runs `ls --color=always /` + a bold/italic/underline `printf`. **Exits non-zero if grid lacks colored/styled cells.** Canonical end-to-end gate; any VT/grid/PTY change must keep this green.
- `cargo run --example pty_dump_unicode -p sonicterm-core --release` — feeds one shibboleth char from every Unicode class we promise to support (CJK, Hiragana, Katakana, Hangul, emoji, box-drawing, Powerline PUA, fullwidth, Latin-1) and **exits non-zero if any are missing from the resulting grid**. Canonical Unicode-end-to-end gate — added to catch the PR-#42-class regression where every existing test used ASCII only.
- `cargo run --example altscreen_smoke -p sonicterm-core --release`
- `cargo run --example pane_smoke -p sonicterm-shared --release`

---

## 3. Big-picture architecture

```
                          ┌──────────────────────────────┐
   shell stdout ──pty▶    │  Parser (vte + SWAR fast-path)│ ─▶ Grid (cells, scrollback,
   ▲                       │  sonicterm-vt::vt                 │     wide chars, alt screen,
   │                       └──────────────────────────────┘     dirty bitset)
   │                                                                       │  sonicterm-grid
   PTY thread (BytesMut ring,                                              ▼
   redraw-coalesced — see                                            Grid is the
   spawn_pane.rs)                                                    shared state
   ▲                                                                       │
   │                                                                       ▼
   App (winit ApplicationHandler)  ──▶  shape (LRU) ──▶ atlas ──▶ wgpu (quad + text)
   ▲   sonicterm-app::app::App                sonicterm-text          sonicterm-gpu
   │   split across app/*.rs (#160)
   │
   keys/mouse  ──▶ keymap dispatcher (sonicterm_cfg::keymap::Action)
                   super+T new tab, super+D split, palette actions, etc.
```

**Per-pane state** (since v0.3d): each pane in a tab owns its own `Grid + Parser + PtyHandle`. Splitting a pane spawns a new shell. `impl Drop for PtyHandle` explicitly kills the child to prevent orphans (caught by Haiku review of PR #21).

**Rendering pipeline** has two layers per frame:
1. **Text** through `sonicterm-text` (LRU shape cache → swash rasterizer → glyph atlas → row-glyph cache) into the wgpu text pipeline in `sonicterm-gpu`. Per-cell foreground + background ANSI colors and bold/italic/underline are emitted as styled attributes per run.
2. **Custom wgpu quad pipeline** (`sonicterm-gpu/src/quad.rs`) for everything else — cursor, selection highlight, underlines, hyperlink tints, tab-bar chrome, search-match highlight, focused-pane border.

---

## 4. Land-mines (each was a real bug; do not re-introduce)

### Threading / event loop
- **Render uses `try_lock` on the parser, not `lock`.** Earlier `lock()` deadlocked the macOS main thread under shell-startup output bursts. Current locations: `crates/sonicterm-app/src/app/window_event.rs` (the redraw path) and `crates/sonicterm-app/src/app/{child_window,misc}.rs`. Comments in window_event.rs at lines ~143 / ~162 explain the AB-BA deadlock.
- **PTY-thread redraw coalescing.** Live in `crates/sonicterm-app/src/app/spawn_pane.rs` (~line 76: "Coalesce redraw requests so a burst of pty output…"). **3 ms min interval + 128 KB byte-threshold early flush** (Epic #300 P3, dropped from the original 16 ms to match wezterm). 3 ms is safe because the macOS "not responding" beach ball is driven by *main-thread* blocking, not by how often a *background* PTY thread posts redraw requests — the main thread coalesces RedrawRequested via vsync (PR #132). The coalescer is therefore a CPU-efficiency throttle, not an OS-perception safety knob. **Rule still stands: must coalesce, never per-byte redraw.** PR #132 layered vsync pacing on top via `ControlFlow::WaitUntil` in `event_loop.rs`.
- **PTY burst flag is a generation counter, not a bool** (PR #162). The original `bool input_dirty` raced when the renderer cleared it between two bursts. The counter version compares "what I last drew" vs "what arrived" — see `crates/sonicterm-app/src/app/window_event.rs` ~line 34: "with this flag still false and continue to coalesce". Don't revert to `bool`.
- **No unconditional "heartbeat redraw" at the end of `window_event`.** It creates a feedback loop. Real triggers (pty bytes / mouse drag / key / resize) cover every case.

### Parser correctness
- **CSI `J` (ED) and `K` (EL) MUST honor the mode parameter.** `J0` is "erase below", `J1` is "above", `J2` is "all". The original code erased everything regardless — every shell prompt redraw wiped output. Lives in `crates/sonicterm-vt/src/vt.rs`. Regression: `crates/sonicterm-vt/tests/vt.rs::shell_prompt_redraw_preserves_above_cursor`.
- **CSI `?1049h` must be a no-op when already in alt screen.** Otherwise vim/fzf re-entry clobbers `saved_cursor`. `crates/sonicterm-vt/src/vt.rs`. Regression: `dec_1049h_repeated_does_not_clobber_saved_cursor`.
- **`PtyHandle::Drop` kills the child explicitly.** Just dropping the trait object doesn't terminate the shell. Lives in `crates/sonicterm-io/src/pty.rs`.

### Security / safety
- **`sonicterm_cfg::url_open::validate()` is mandatory before spawning anything.** OSC 8 URIs come from untrusted pty output; on Windows `cmd /C start` re-tokenizes through cmd's parser. Allow-list: `http`, `https`, `mailto`, `file`. Denylist: `& | ^ < > " ' \` CR LF NUL` + other control chars. Length capped at 4096. Lives in `crates/sonicterm-cfg/src/url_open.rs`.

### Rendering
- **Per-cell ANSI background colors must be emitted (P0, #161 → #163).** Pre-#163 the text pipeline silently dropped the `bg` field — only fg + attrs reached glyphon. A whole class of TUIs (`htop` selected-row stripe, `tmux` statusline, fzf preview) rendered with the theme background instead of the cell background. Fix in `crates/sonicterm-shared/src/render/core.rs` + `crates/sonicterm-gpu/src/text_pipeline.rs`. Don't regress: the test floor includes a "colored background round-trip" check fed by `pty_dump`.
- **`wgpu::CurrentSurfaceTexture::Suboptimal(frame)` must drop the SurfaceTexture before calling `surface.configure(...)`.** Otherwise wgpu 29 panics ("texture still alive"). `crates/sonicterm-shared/src/render/core.rs`.
- **`set_text` vs `set_rich_text`**: per-cell color/weight/style requires `set_rich_text(spans, default_attrs, Shaping::Advanced, None)` — the cosmic-text 0.18 API. Lives in `crates/sonicterm-shared/src/render/core.rs`.

### Coupling
- **`wgpu`, `glyphon`, `cosmic-text` are a coherent triple.** Bumping one forces the others. Current: `wgpu 29` + `glyphon 0.11` + `cosmic-text 0.18`. Don't upgrade just one.
- **Clippy is `all`, not `pedantic`/`nursery`.** The loud groups were tried and removed (noise > signal at this stage). Add `#[allow(...)]` selectively if needed.

---

## 5. Coding conventions

- **Per-crate `tests/` folder** (one `.rs` per source module). PR #27 moved all of `sonicterm-core` + `sonicterm-shared`'s pre-v0.6 tests out of source files; issue #190 finished the workspace migration. **New tests follow this pattern.** Documented exceptions (kept inline with a `// NOTE (CLAUDE.md §5):` comment naming the blocker):
  - `sonicterm-windows/src/os_drag_win.rs` — bin-only crate (no `lib.rs`), no `tests/` route.
  - `sonicterm-mac/src/menubar.rs` — small macOS-only surface, private `register`/`lookup`/`scan_themes`.
  - `sonicterm-io/src/foreground_proc.rs` — private `snapshot_processes`/`resolve_process_name`/`ProcEntry`.
- **Test-only items that must remain accessible to integration tests stay `pub` with `#[doc(hidden)]`.** No `__test_support` shim modules — that pattern was explicitly removed.
- **Public API for actions**: adding a new bindable user action means adding a variant to `sonicterm_cfg::keymap::Action` AND a match arm in `sonicterm_app::app::App` (via `keymap_dispatch.rs`).
- **Conventional Commits** with scope: `feat(v1.0): ...`, `fix(vt): ...`, `chore(deps): ...`, `docs: ...`, `refactor(crates): ...`.
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
| Reviewer | `haiku` sub-agent | Reviews PR diff + pulls + runs tests + posts `APPROVED` / `CHANGES REQUESTED` PR comment (per-PM — no cross-PM review, see §15) |

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
# resolve conflicts; run the gate; cargo test must still print ≥ floor tests
git push --force-with-lease
gh pr merge <N> -R D0n9X1n/sonic --squash --admin --delete-branch
```

### Parallelism rules

- **Truly independent files only.** Two PRs that both touch `app/window_event.rs` or `render/core.rs` WILL conflict — serialize them.
- Safe parallel: new files in `sonicterm-text/`, new modules under `app/`, documentation work.
- Risky parallel: anything touching `app/window_event.rs`, `render/core.rs`, `Cell`, `keymap::Action`.
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
- macOS: `~/Library/Application Support/SonicTerm/sonicterm.toml`
- Windows: `%APPDATA%\SonicTerm\sonicterm.toml`

Bundled defaults: `assets/themes/*.toml` + `assets/keymaps/sonicterm.toml`. The keymap action enum is the public surface — adding a bindable action requires a variant + dispatcher arm (see §5). Default font is `St Helens` (system, not bundled); `Rec Mono Casual` ships under `assets/fonts/` as guaranteed fallback.

---

## 9. Release

```bash
# 1. Run the full UX release-testing checklist (docs/RELEASE_TESTING.md)
#    on a freshly built release binary; tick every box.
bash scripts/check-release-testing.sh   # MUST exit 0 — gates the tag.

# 2. Tag + push (CI also runs the gate before building).
git tag v1.0.0 && git push origin v1.0.0
```

triggers `.github/workflows/release.yml` → first runs the `release-gate` job (which re-runs `scripts/check-release-testing.sh`) + full integration tests + both `pty_dump` e2e examples + `scripts/bench.sh` in CI mode, then produces a universal macOS `.dmg` + x64 Windows `.msi`. **All shipped artifacts are UNSIGNED.** Signing (Developer ID notarization for macOS, Azure Trusted Signing for Windows) has been removed from the release workflow pending cert procurement; when certs land, re-add the steps in a follow-up PR. The release workflow installs `librsvg + imagemagick` then runs `bash assets/icons/bake-icons.sh` so the bundles always carry the fresh icon.

The checklist itself (`docs/RELEASE_TESTING.md`) is a **49-section** sweep covering tab/pane/palette/config-edit/tear-out/nvim-stress/ANSI/URL/IME/multi-window/idle/perf/drag-drop/quit plus scrollback+copy, search overlay, resize semantics, HiDPI/multi-monitor, theme+font live-reload, shell exit/kill, Ctrl-letter encoding, alt-screen round-trip, OSC8+URL safety extended, mouse modes, wide-chars/grapheme clusters, cursor styles, crash hygiene, accessibility, first-run, locale/non-UTF8, TCC permissions, 1-hour stability, drag-drop edge cases, config validation, per-OS tab chrome/new-tab/title/padding/keymap parity, cheatsheet, copy mode + quick select, broadcast input, pane zoom + resize, accessibility modes, theme import/export, OSC 133 command badges, notifications, and CLAUDE.md §4 land-mine coverage — i.e. exactly the user-facing surfaces that the §13 single-pane GUI smoke does NOT exercise. **v0.8.1 is the first release using this gate.**

`crates/sonicterm-logging/` is initialized at the top of every binary's `main()` (before config load) so even bootstrap errors land in `~/Library/Logs/SonicTerm/sonicterm.log.*` / `%LOCALAPPDATA%\SonicTerm\Logs\sonicterm.log.*`. Retention is ~60 MB rolling + 10 crash dumps; see `docs/LOGGING.md`.

---

## 10. When you're stuck

- Default to consulting the user, not guessing — the project has a real human PM driving direction.
- Real correctness bugs go in a fresh fix-up commit on the PR branch (don't squash silently).
- If you find yourself fighting `git branch --show-current` reporting one thing while you're really on another, you're probably in the wrong working directory. Always use a fresh `/tmp/` clone for each PR's work.

---

## 11. Renderer regressions (the rule that didn't exist before PR #42)

PR #42 cut the terminal grid over to a swash-rasterized atlas (now `crates/sonicterm-shared/src/render/core.rs` + `crates/sonicterm-text/src/swash_rasterizer.rs` + `crates/sonicterm-text/src/glyph_atlas.rs`). It passed the local gate, Haiku review, AND the canonical `pty_dump` e2e — yet shipped a regression that drew every non-ASCII character as a tofu box. Cause: every test, every example, every benchmark used only ASCII, and the rasterizer's "primary family only, no fallback" code path was never exercised on a CJK glyph in CI.

**Rule:** any change to `crates/sonicterm-shared/src/render/*.rs`, `crates/sonicterm-text/src/{swash_rasterizer,glyph_atlas,row_glyph_cache,shape}.rs`, or `crates/sonicterm-gpu/src/{text_pipeline,atlas_upload,quad}.rs` MUST be gated on the capability matrix passing:

```bash
cargo test -p sonicterm-core --test vt_capability_matrix
cargo test -p sonicterm-shared --test render_capability_matrix
cargo run --example pty_dump_unicode -p sonicterm-core --release
```

The matrix exists because **the existing pty_dump e2e cannot catch this class of bug** — its shell payload is pure ASCII. Do NOT delete or weaken the matrix; if a class is intentionally dropped from scope, mark the corresponding test `#[ignore]` with a comment naming the deciding PR, never `#[cfg(skip)]` or deletion.

**Render hot-file rule (closes #283):** any PR that modifies `crates/sonicterm-shared/src/render/core.rs`, `crates/sonicterm-gpu/src/text_pipeline.rs`, `crates/sonicterm-text/src/glyph_atlas.rs`, or `crates/sonicterm-text/src/swash_rasterizer.rs` MUST either keep `bash scripts/check-visual-snapshots.sh` green or explicitly bump the dHash baselines in the same PR (set `UPDATE_SNAPSHOTS=1`, commit refreshed `crates/sonicterm-shared/tests/snapshots/*.hash`, and append a row to `crates/sonicterm-shared/tests/snapshots/README.md`). Silent drift is how PR #282 shipped the glyph-blur P0 fixed in #284. Label such PRs `render` so reviewers see the gate at a glance.

Ignored tests in the matrix document capability gaps awaiting a fix. Removing an `#[ignore]` attribute in that fix's PR is the canonical green light that the gap is closed.

---

## 12. Disk hygiene — scratch clones (MANDATORY)

Every multi-agent PR cycle creates `/tmp/<scratch>` clones (~1.8 GB each: full repo + `target/`). With ~10 PRs in flight in parallel, this trivially fills a 460 GB SSD to 99 % — at which point even `df` and `echo` fail with ENOSPC and the harness silently corrupts in-flight agents.

**Rule for every agent prompt:** the FINAL step of every dispatch MUST be `cd / && rm -rf /tmp/<scratch>`. No exceptions. Include it as an explicit numbered step in the prompt and require the agent to confirm it ran in the reply.

**Rule for the PM (you):** after every merge or every "task-notification" arrival, sweep stragglers:

```bash
du -sh /tmp/* 2>/dev/null | sort -h | tail
# anything >100 MB that's not currently in-flight: rm -rf
```

Acceptable in-flight footprint: one scratch per active agent. After all agents return, `/tmp` should be back near 0 B of sonic clones.

Local `target/` is a separate ~5 GB cost (debug + release + deps + incremental). Run `cargo clean` periodically when not actively building. The shipped `.dmg` is **~22 MB**; the 5 GB is purely build-time intermediates.

---

## 13. GUI smoke test — MANDATORY for every PR that touches rendering / input / VT / window state

The headless local gate (fmt / clippy / test / pty_dump) is necessary but not sufficient. Several real bugs have shipped past it: blank window (PR #36), CJK tofu (PR #42), 100 % idle CPU (PR #31), sRGB gamma washing out theme colors, low-DPI blur on Retina, and **dropped ANSI background colors (#161 → P0 #163)**. None of these show up in `cargo test` because they need a real wgpu surface + real macOS window + real glyph upload.

**Rule:** every PR that touches any file under `crates/sonicterm-shared/src/render/`, `crates/sonicterm-text/src/`, `crates/sonicterm-gpu/src/`, `crates/sonicterm-app/src/app/`, `crates/sonicterm-ui/src/{tabbar_view,overlays,cursor,selection,search}.rs`, `crates/sonicterm-vt/src/vt.rs`, `crates/sonicterm-grid/src/grid.rs`, or any theme/keymap asset MUST run this GUI smoke before requesting review.

**Prefer the harness:** run `just visual mac` (or `just visual-case <id> mac`) from `testing/workflows/mac.sh` against `testing/cases.toml` — that is the canonical, repeatable form of the ad-hoc snippet below, with per-case screenshots archived under `testing/results/mac-<sha>/`. The snippet below remains valid for one-off checks.

```bash
pkill -9 -f sonicterm-mac 2>/dev/null; sleep 0.3
./target/release/sonicterm-mac > /tmp/gui-smoke.log 2>&1 &
sleep 2.5
PID=$(pgrep -f sonicterm-mac | head -1)

# 1. Bring to front + position (so the screenshot actually captures SonicTerm, not whatever was front)
osascript <<EOF
tell application "System Events"
  tell process "sonicterm-mac"
    set frontmost to true
    set position of window 1 to {500, 200}
    set size of window 1 to {1000, 700}
  end tell
end tell
EOF
sleep 0.5

# 2. Inject a representative keystroke payload that exercises:
#    - ASCII echo round-trip (PTY ↔ grid)
#    - CJK glyph rasterization + font fallback
#    - emoji color rendering
#    - ANSI background color (the #163 regression class — must show a red stripe)
#    - Enter / RET handling
osascript -e 'tell application "System Events" to keystroke "printf '"'"'\\033[41mRED-BG\\033[0m echo 中文 🎉 sonic\\n'"'"' && date"'
sleep 0.3
osascript -e 'tell application "System Events" to key code 36'   # Enter / Return
sleep 1

# 3. Screencap full main display (-D 1)
screencapture -x -D 1 /tmp/gui-smoke.png

# 4. Inspect /tmp/gui-smoke.png — sample background pixel (theme.background should
#    match the configured hex), confirm text renders, confirm no tofu boxes, confirm
#    cursor present, confirm the RED-BG cells show a red rectangle (not theme bg).

kill -9 $PID 2>/dev/null
```

Things to check on the screenshot (open + look — do not trust your absence of an error message):

- Window background pixel value matches `theme.colors.background` (no sRGB/linear double-gamma)
- **Per-cell ANSI bg renders** — the `RED-BG` cells are red, not theme background (regression-guard for #163)
- CJK 中 文 render as glyphs, not `?` and not tofu boxes
- 🎉 renders in color (not monochrome silhouette)
- Cursor is visible (not blank)
- Text is sharp (no HiDPI upscale blur on Retina)
- No 0 %–100 % CPU sweep (check `ps -p $PID -o %cpu` during the 5 s window — should sit < 5 %)

If any of those fail, the PR is not ready. Include the screenshot path AND a 1-line observation per check in the PR body. Reviewers will compare against the previous baseline screenshot.

**For background agents:** the same gate applies — every agent dispatch for a render/input/VT PR must include the GUI smoke as a numbered step in the prompt, and the agent must paste the screenshot path + observation list in its reply. If the agent has no display (CI / sandbox), it MUST flag that fact explicitly and the PM (you) runs the smoke locally before merging.

Per §15, the other-platform PM runs §13 for their platform on
cross-platform render/input PRs. Merge requires both smoke
comments present.

---

## 14. Honest perf status (v1.0-RC)

We claim "fast" in the README and in the North Star. Right now that is aspirational, not measured. Latest `vtebench` run (see `/tmp/sonic-vs-wezterm.md` notes or re-run locally) shows SonicTerm **6×–302× slower than WezTerm** depending on the benchmark — the worst offenders are heavy SGR-attribute streams and dense scrollback writes. The 8 perf PRs that landed this session (#129 #130 #131 #132 #136 #138 #140 #141 #142, plus #162 burst-flag fix) closed ~30–60 % of the gap on the cat-large-file and tail-f hot paths but did NOT achieve parity.

**Rule:** do not describe perf work as "done" in commit messages or PR bodies. Phase E (perf parity) is ongoing. Concrete remaining items live in the v1.x section of `docs/ROADMAP.md`.

### Perf-bench gate (`scripts/bench.sh` + `baseline.json`)

Because perf has shipped regressions silently (e.g. PR #161's dropped per-cell bg), we now run a small bench subset on every render-touching PR and compare against `baseline.json` at the repo root. The gate:

- **Local** (`bash scripts/bench.sh`): prints a table, warns on any metric that regresses by more than `_regression_threshold_pct` (default 20 %), **exits 0** so a dev iterating on perf can re-run cheaply.
- **CI** (`bash scripts/bench.sh --ci`, or with `CI=1`): same table, but **exits 1** on a >20 % regression. The CI job (`perf-bench` in `.github/workflows/ci.yml`) is paths-filtered to perf-sensitive crates (`sonicterm-vt`, `sonicterm-grid`, `sonicterm-text`, `sonicterm-gpu`, `sonicterm-shared/src/render/`, `sonicterm-app/`, `baseline.json`, `scripts/bench.sh`).
- **Intentional perf change** (`bash scripts/bench.sh --record`): re-measures and overwrites `baseline.json`. Commit the diff alongside the change so the regression bar tracks reality.
- **Testing the gate itself** (`BENCH_SKIP_MEASURE=1 bash scripts/bench.sh [--ci]`): skips the measurement step and reuses the existing `current.json` as-is, so you can inject a synthetic regression into `current.json` (e.g. `jq '.metrics.cat_10mb_ascii_sec = 0.30' current.json > tmp && mv tmp current.json`) and confirm the comparison step fails in CI mode / warns in local mode. Without this flag every invocation re-measures and would overwrite the injected values before comparing.

Subset measured always: `cat_10mb_ascii_sec`, `cat_4mb_ansi_sec`, `idle_cpu_pct`, `rss_mb`. The three `vtebench_*` metrics are run **only if `vtebench` is on PATH**; if absent they're emitted as `null` and skipped from the diff so a missing tool never fails the gate. CI's Linux runner tries `cargo install vtebench --locked`; if install fails (e.g. transient registry issue) the job still passes on the subset rather than blocking unrelated work.

A criterion microbench lives at `crates/sonicterm-shared/benches/render_throughput.rs` (run with `cargo bench -p sonicterm-shared --bench render_throughput`). It covers the hot pure-CPU helpers (`hex_to_rgba`, `srgb_u8_to_linear_lut`) the render pipeline calls per-cell so algorithmic regressions in those show up without a GPU surface. Add a new bench function when you add a new hot pure-CPU helper.

---

## 15. Multi-agent coordination (PM ↔ PM)

This repo is staffed by multiple Claude PM sessions in parallel. At
time of writing: one on macOS, one on Windows. The split is
**platform-primary** — the Windows PM owns everything that only
Windows can verify, the macOS PM owns everything that only macOS
can verify. Cross-platform work goes to whoever claims it first.

### Ownership

| Domain | Owner | Why |
|---|---|---|
| `crates/sonicterm-mac/` + macOS-only paths (NSMenu, libproc) | mac-PM | only Mac can §13 |
| `crates/sonicterm-windows/` + Windows-only paths (ConPTY, muda, Mica, OLE drag) | win-PM | only Win can §13 |
| Cross-platform hot files: `render/core.rs`, `app/*.rs`, `keymap.rs`, `vt.rs`, `grid.rs` | first to claim, blocks the other | high merge-conflict risk |
| Cross-platform pure-data: `sonicterm-vt/`, `sonicterm-grid/`, `sonicterm-cfg/theme.rs`, `assets/themes/*.toml`, `docs/specs/` | either, coordinate via touches: line | safe parallel |
| `CLAUDE.md`, `docs/ROADMAP.md`, `docs/RELEASE_TESTING.md`, `CHANGELOG.md` | current release-tag owner | one writer per release window |

### Mandatory touches: line

Every PR body MUST start with a single line listing the files/crates
touched:

    touches: crates/sonicterm-app/src/app/window_event.rs, crates/sonicterm-shared/src/render/core.rs

Before opening a PR on a hot file, run:

    gh pr list -R D0n9X1n/sonic --state open --json headRefName,body \
      --jq '.[] | select(.body | test("touches:.*<your-file>"))'

If a hit comes back, the other PM already reserved it — wait or
coordinate via a comment on their PR. Don't race.

### Mandatory `dev:*` label

Every PR MUST carry exactly one `dev:*` label identifying the PM machine
that authored it:

- `dev:mac` — opened from the macOS PM session
- `dev:windows` — opened from the Windows PM session
- `dev:linux` — opened from the Linux PM session (future)

The `dev:*` label applies to **both PRs and issues**:

- On a PR: identifies which PM authored it.
- On a new issue: identifies which PM is the **primary owner**
  (typically the PM who filed it, OR the PM whose platform is affected).
- For issues affecting multiple platforms, use BOTH `dev:mac` AND
  `dev:windows` if both PMs are expected to contribute. Otherwise pick
  one based on who's leading.
- When triaging an existing issue without a `dev:*` label, the first PM
  to commit to working on it claims it by adding their `dev:*` label.

This is set IMMEDIATELY after `gh pr create`, e.g.:

    PR_URL=$(gh pr create --title "..." --body "..." | tail -1)
    PR_NUM=${PR_URL##*/}
    gh pr edit $PR_NUM --add-label dev:mac

The dev label is distinct from `platform:*`:
- `platform:*` describes which OS the CHANGE affects (build target).
- `dev:*` describes which PM AUTHORED the PR (review channel).

So a `dev:mac` PR can be `platform:windows` (rare — coordinate first via
issue, since the authoring PM can't §13 smoke that platform). Normal case:
`dev:mac` pairs with `platform:mac` or `platform:both`.

When a `gh pr list` filter is needed to see "my work" vs "other PM's
work", filter on `dev:*`:

    gh pr list --label dev:mac    # my open PRs
    gh pr list --label dev:windows --state open  # peek peer's work

### Per-platform GUI smoke

Render/input/VT/window-state PRs need §13 smoke on BOTH platforms
before merge.

- Originating PM runs §13 on their own platform.
- The other-platform PM runs §13 for the platform they own and posts
  the screenshot path + outcome as a PR comment. **This is the only
  cross-PM interaction required.**
- Merge requires BOTH smoke results recorded.

**Visual-harness results are the cross-PM channel.** Attach the
`testing/results/<plat>-<sha>/<case-id>/screen.png` path on the PR
instead of pasting ad-hoc descriptions. Both PMs run `just visual <plat>`
against the same `testing/cases.toml`, so the artifacts are directly
comparable.

### No cross-PM PR review

Each PM is responsible for their own PRs end-to-end:
- They dispatch their own Haiku/Sonnet reviewer.
- They address review findings.
- They merge their own PR (with `--admin` per §6).

The other PM **does not review, comment, or merge** the first PM's
PRs unless explicitly @-mentioned. This avoids dead-locking waiting
for cross-PM availability and keeps each PM's loop self-contained.
The only cross-PM channel is the per-platform GUI smoke comment
described above.

### Release tagging

One PM owns each release tag end-to-end: runs the full 49-section
`docs/RELEASE_TESTING.md` checklist, runs `bash scripts/bench.sh`,
runs signing pipeline, pushes the tag. Tag ownership rotates per
release. The tag owner is the sole writer of CLAUDE.md / ROADMAP /
CHANGELOG during their release window.

### Issue / label hygiene

At triage:
- `platform:mac` / `platform:windows` / `platform:both` on every
  issue so the right PM picks it up.
- `hot-file` on any issue whose fix will touch render/app/keymap/vt/grid
  — flag for coordination.

The non-owning PM may file issues / open PRs on the owner's platform
but does NOT commit on the owner's open branches without invitation.

---

## 15. Filing bugs against SonicTerm

When filing a bug, attach the last 200 lines of the most recent log file:

- macOS: `tail -200 ~/Library/Logs/SonicTerm/sonicterm.log.*`
- Windows: `Get-Content "$env:LOCALAPPDATA\SonicTerm\Logs\sonicterm.log.*" -Tail 200`

If the bug crashed the app, include the matching `crashes/crash-*.log` from the same directory. If logs are gone (auto-cleaned by the 14-day / 5-rotated-file retention policy), say roughly when the bug happened so we can correlate. Full retention + level docs live in `docs/LOGGING.md`.
