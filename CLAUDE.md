# CLAUDE.md — SonicTerm

SonicTerm is a GPU-accelerated terminal for macOS and Windows. Keep changes
small, typed, and cross-platform unless the crate is explicitly platform-only.
The workspace version is the source of truth (`Cargo.toml` `[workspace.package]`).

## Read first

- `docs/ARCHITECTURE.md` — architecture and data flow.
- `docs/MODULES.md` — crate map.
- `docs/LOGGING.md` — logs and diagnostics.
- `docs/RELEASE.md` — tag-driven release process.
- `wiki/` — bilingual user-facing usage/config/keybinding/log/theme docs.

When auditing docs for release blockers, typos, renamed paths, or user-facing
terminology, include `wiki/` alongside README and `docs/`; the wiki is part of
the monitored documentation surface. Editing `wiki/` here does **not** change
the live GitHub Wiki — see [Wiki](#wiki) for the two-repo publish step.

When touching a crate, also read that crate's local `CLAUDE.md`.

## Crates

| Crate | Role |
| --- | --- |
| `sonicterm-types` | Shared contract types and trait seams. |
| `sonicterm-vt` | VT/ANSI parsing. |
| `sonicterm-grid` | Cells, scrollback, dirty rows. |
| `sonicterm-cfg` | Config, themes, keymaps, URL safety. |
| `sonicterm-io` | PTY/process/SSH IO. |
| `sonicterm-text` | Glyph atlas and row text cache. |
| `sonicterm-font` | Font discovery, shaping, fallback, rasterization. |
| `sonicterm-font-config` | Font configuration model shared by the font stack. |
| `sonicterm-freetype` | FreeType rasterization FFI wrapper. |
| `sonicterm-harfbuzz` | HarfBuzz shaping FFI wrapper. |
| `sonicterm-fontconfig` | Fontconfig discovery FFI wrapper (non-macOS). |
| `sonicterm-engine` | Font-facing engine seam (`FontStack`, cell metrics). |
| `sonicterm-block-glyph` | Box/block/Powerline/Braille geometry. |
| `sonicterm-render-model` | Renderer-agnostic frame data. |
| `sonicterm-ui` | Tabs, palette, search, selection, IME. |
| `sonicterm-gpu` | wgpu renderer. |
| `sonicterm-app-core` | Winit-independent reducer/state. |
| `sonicterm-app` | Cross-platform app orchestration. |
| `sonicterm-mac` | macOS binary/glue. |
| `sonicterm-windows` | Windows binary/glue. |
| `sonicterm-mux` | Future mux daemon. |
| `sonicterm-logging` | Logs, panic hook, exit tracing. |

## Local gate

Normal PR/main CI runs workspace unit tests plus a per-crate unit/build gate:

```bash
cargo test --workspace --lib --bins
bash scripts/check-workspace-crates.sh
scripts/coverage/rust-logic.sh
```

For release prep also run:

```bash
cargo build --release -p sonicterm-mac
bash scripts/test-release-notes.sh
```

Before opening a release PR, verify user-facing docs in README, `docs/`, and
`wiki/` match any changed config, logging, window, palette, or input behavior.
After pushing a release tag, verify the GitHub release workflow finishes and
publishes the expected macOS DMG(s), Windows MSI, and checksum assets.

## Conventions

- **Unit tests live in `tests/`, never in `src/`.** Every module's unit tests
  go in `crates/<crate>/tests/<module>_tests.rs` (matching the source file
  name: `vt.rs` → `tests/vt_tests.rs`). No `#[cfg(test)] mod tests { … }`
  blocks inside source files, and no `tests.rs` siblings under `src/`.
- **How private access is preserved.** Each crate sets `autotests = false` in
  `Cargo.toml`. Unit-test files are pulled back into the lib from their source
  module with `#[cfg(test)] #[path = "../tests/<module>_tests.rs"] mod tests;`
  (adjust the `../` depth for nested modules), so they compile in-crate and keep
  `use super::*;` private-item access. Real integration tests (that only need
  the public API) are each registered with an explicit `[[test]]` entry in the
  same `Cargo.toml`.
- **Adding a test file is not automatic.** Because `autotests = false`, a new
  integration test must be registered with a `[[test]]` block, and a new unit
  test must be `#[path]`-included from its module — otherwise it will not run.
- **Comments describe behavior, not history.** Explain what the code does and
  the problem it solves; do not cite issue/PR/Epic numbers or reviewer names
  in comments, log strings, or panic messages.

## Release

SonicTerm releases are created by pushing a `v*` tag. The tag workflow builds:

- macOS universal `.dmg`
- Windows x64 `.msi`
- release notes from commits since the previous tag

## Wiki

The GitHub Wiki is a **separate git repository** from this one. There are two
copies of the same Markdown:

| Location | Repo | Serves |
| --- | --- | --- |
| `wiki/` folder here | `D0n9X1n/SonicTerm.git` (this repo) | source/mirror copy, reviewed in PRs |
| Live Wiki tab | `D0n9X1n/SonicTerm.wiki.git` | the rendered pages at `/SonicTerm/wiki/...` |

Editing `wiki/*.md` in this repo updates only the mirror. The live page does
**not** change until the file is also pushed to the wiki repo. Always update
both so they stay in sync.

To publish a wiki change to the live site (page file name = page title, e.g.
`Keybindings.md` → `/wiki/Keybindings`):

```bash
WT=$(mktemp -d)
git clone git@github.com:D0n9X1n/SonicTerm.wiki.git "$WT"
cp wiki/Keybindings.md "$WT/Keybindings.md"   # repeat for each changed page
git -C "$WT" add -A
git -C "$WT" commit -m "docs(wiki): <summary>"
git -C "$WT" push origin master                # wiki default branch is `master`
rm -rf "$WT"
```

Then commit the matching `wiki/` edits in this repo (on a branch) so the mirror
and the live wiki do not drift.

## WezTerm

SonicTerm thanks WezTerm and uses it as the reference for terminal behavior,
font behavior, keymap conventions, and rendering edge cases. Absorb proven
behavior into Sonic-owned crates; do not reintroduce a `vendor/` dependency.
