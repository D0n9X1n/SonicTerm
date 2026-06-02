# CLAUDE.md — SonicTerm

Guidance for Claude Code (claude.ai/code). Dense on purpose — read once.

The intent is that **any Claude agent dropped into this repo is productive
in 5 minutes**. Most agents do NOT need this whole file — see the routing
table below.

---

## Routing table (load only what you need)

| You're touching… | Load these (in order) |
|---|---|
| Anything | this file (§0–§3), `docs/agents/_common.md` |
| `crates/sonicterm-<x>/...` | + `crates/sonicterm-<x>/CLAUDE.md` |
| A `pub` item in `sonicterm-types` | + `docs/CONTRACTS.md` |
| A landmine-flagged file | + `landmines.toml` entry for the LM-ID |
| Render / VT / app pipeline | + the touched crate's CLAUDE.md (§ Land-mines) |
| Release tag | + `docs/RELEASE_TESTING.md` |

Each crate-local CLAUDE.md stays ≤ 80 lines. If one grows past, the
crate is too big — file a split.

---

## §0 North Star

A **GPU-accelerated, cross-platform terminal** for macOS + Windows.

- Performance first. Beat WezTerm if possible — not there yet
  (see `crates/sonicterm-app/CLAUDE.md` § perf-status).
- Linux, code signing, auto-update, session restore are deferred past v1.0.
- WezTerm-compatible keymap default.
- The icon is canonical and user-supplied — don't replace it.

Authoritative running status: `docs/ROADMAP.md`. Read first; update on
milestone ship.

---

## §1 Crates (under `crates/`)

| Crate | Role |
|---|---|
| `sonicterm-types` | Zero-dep value types + trait seams (the contract crate) |
| `sonicterm-vt` | VT/ANSI parser + SWAR ASCII fast-path |
| `sonicterm-grid` | Cells, scrollback, wide chars, dirty bitset |
| `sonicterm-cfg` | Config, theme, keymap, URL safety |
| `sonicterm-io` | PTY, proc_info, SSH |
| `sonicterm-text` | Shape LRU, swash, atlas |
| `sonicterm-render-model` | Renderer-agnostic frame model |
| `sonicterm-ui` | Tabs, palette, search, selection, IME |
| `sonicterm-gpu` | wgpu quad + text pipelines |
| `sonicterm-app-core` | Winit-agnostic state machine (M6a) |
| `sonicterm-app` | Winit ApplicationHandler glue |
| `sonicterm-mac` | NSMenu, libproc, OS-drag |
| `sonicterm-windows` | ConPTY, muda, Mica, OLE drag |
| `sonicterm-mux` | Persistent PTY mux daemon |
| `sonicterm-logging` | Panic hook + rolling logs |
| `sonicterm-core` | 💀 deprecated façade — removed v1.1 |
| `sonicterm-shared` | 💀 dissolved at M7 |

Dep-graph rationale: `docs/ARCHITECTURE.md`.

---

## §2 Local gate

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/check-no-raw-process-exit.sh
bash scripts/check-deny.sh
bash tools/check-landmines.sh
bash tools/check-contract-docs.sh
bash tools/check-ownership.sh
cargo run --example pty_dump -p sonicterm-core --release
cargo run --example pty_dump_unicode -p sonicterm-core --release
cargo run --example pty_dump_unicode -p sonicterm-core --release
bash scripts/check-visual-snapshots.sh
cargo build --release -p sonicterm-mac
bash scripts/bench.sh
```

**Test floor: 1445.** Never regress. Confirm with:

```bash
cargo test --workspace 2>&1 | grep "test result:" | awk '{s+=$4} END {print "TOTAL:",s}'
```

CI matrix: `macos-14` + `windows-latest`. PR/main quick gate runs
fmt+clippy+`cargo test --workspace --lib --bins`. Tag push (`v*`) runs
the full integration sweep + `scripts/bench.sh --ci` + unsigned dmg/msi.

---

## §3 Workflow + agent dispatch

This repo is staffed by Claude PMs. Every PR follows the 5-step
rotation: **Raise → Investigate → Review diagnosis → Implement →
Review code.** Diagnose-only agents do NOT write production code.
Issues go through Haiku intake review before `gh issue create`. See
`docs/agents/_common.md` for the shared rules.

Every PR body MUST start with a `touches:` line listing the files
changed. Every PR MUST carry exactly one `dev:*` label
(`dev:mac` | `dev:windows`).

When working in this repo, always work in a `/tmp/<scratch>` clone, not
in the canonical worktree — multiple agents in parallel will trash each
other's tree. Final step of every agent dispatch:
`cd / && rm -rf /tmp/<scratch>`.

**No cross-PM PR review.** Each PM dispatches and merges their own PRs.
The one exception is GUI-smoke comments on render/input/VT PRs.

---

## §15 multi-PM coordination

| Domain | Owner |
|---|---|
| `crates/sonicterm-mac/` + macOS-only paths | mac-PM |
| `crates/sonicterm-windows/` + Windows-only paths | win-PM |
| Hot files (see `docs/HOT_FILES.md` — incl. `sonicterm-gpu/src/core.rs`, `app/*.rs`, `keymap.rs`, `vt.rs`, `grid.rs`) | first to claim, blocks the other |
| Cross-platform pure-data (vt, grid, cfg, themes) | either, coordinate via `touches:` |
| `CLAUDE.md`, `ROADMAP.md`, `RELEASE_TESTING.md`, `CHANGELOG.md` | current release-tag owner |

Render/input/VT/window PRs need §13-style GUI smoke on **both**
platforms before merge. Originating PM runs §13 on their platform; the
other-platform PM posts the screenshot path as a PR comment. See
`docs/HOT_FILES.md` for the full list.

The mac §13 smoke (`just visual mac` or the ad-hoc snippet in
`crates/sonicterm-app/CLAUDE.md`) MUST verify sonicterm-mac is
frontmost before any keystroke and use window-local `screencapture
-l "$WINDOW_ID"` rather than `-D 1`. The full-display fallback and
unverified-focus pattern leaked keystrokes into other apps (#464).

---

## See also

- `docs/agents/_common.md` — §4 land-mines, commit convention, scratch hygiene
- `docs/CONTRACTS.md` — trait surface + 2-PR deprecation protocol
- `docs/HOT_FILES.md` — 2-PM sign-off list with rationale
- `docs/MODULARIZATION_PILOT.md` — exit criteria for the modularization epic
- `docs/migrations/` — one per breaking `sonicterm-types` change
