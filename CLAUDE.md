# CLAUDE.md — SonicTerm

SonicTerm is a GPU-accelerated terminal for macOS and Windows. Keep changes
small, typed, and cross-platform unless the crate is explicitly platform-only.
The workspace version is the source of truth (`Cargo.toml` `[workspace.package]`).

## Read first

- [`wiki/Architecture.md`](wiki/Architecture.md) — system shape, data flow, seams.
- [`wiki/Architecture-Internals.md`](wiki/Architecture-Internals.md) — accounting verification, rendering invariants, native boundaries, release gate.
- [`wiki/Crate-Reference.md`](wiki/Crate-Reference.md) — crate map and per-crate detail.
- [`wiki/Logging.md`](wiki/Logging.md) — logs, diagnostics, retention, hang investigation.
- [`wiki/Memory.md`](wiki/Memory.md) — what each subsystem holds, and the resource governor.
- [`wiki/Rendering-Modes.md`](wiki/Rendering-Modes.md) — software vs GPU rendering and frame pacing.
- [`wiki/Packaging.md`](wiki/Packaging.md) — local macOS and Windows packaging.

**Canonical documentation rule:** `wiki/` is the single documentation surface,
for agents and humans alike. It carries the technical detail — architecture,
invariants, verification, governance, packaging, release boundary — alongside
user-facing usage, configuration, keybindings, themes, and the feature
requirements. There is no separate maintainer-only documentation tree; a fact
worth writing down belongs on a wiki page, in both language halves.

Every page is bilingual: an `## English` half and a `## 中文` half with the same
structure. A page edited in one language only is half-wrong for the next reader,
so update both halves in the same change. Pages link to each other by bare page
name — `[Logging](Logging)`, not `[Logging](Logging.md)` — because that is what
resolves on the published wiki.

Do not track standalone implementation specs, plans, review audits, or
version-audit documents. `docs/specs/`, `docs/plans/`, and `docs/reviews/`
remain ignored local working folders.

When touching a crate, also read that crate's local `CLAUDE.md`.

## Searching

**Search is filtered by default, and the filter is silent.** A root `.ignore`
excludes the four vendored upstream trees — FreeType, libpng, zlib, HarfBuzz —
from `rg` and from most editors and agents that read it. That is 2,021 of the
2,529 tracked files, so a default search covers 504.

This matters for reading a result, not just for speed: **an empty result may
mean the match is in a filtered tree, not that it does not exist.** When a
symbol is expected and search finds nothing, re-run with `--no-ignore` before
concluding it is absent.

```bash
rg 'FT_Load_Glyph'                                   # first-party only
rg --no-ignore 'FT_Load_Glyph'                       # including vendored
git grep 'FT_Load_Glyph'                             # git ignores .ignore entirely
```

`git grep` and `git ls-files` are unaffected, which makes them the right tool
when the question is "what does the repository contain" rather than "where is
our code". `.github/` is explicitly un-hidden, since `rg` skips dot-directories
by default and the CI workflows are first-party files worth finding.

## Crates

| Crate | Role |
| --- | --- |
| `sonicterm-types` | Shared contract types and trait seams. |
| `sonicterm-resource` | Resource governor: ledger, owner hierarchy, reservations, reaper. |
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
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p sonicterm-io --features ssh --all-targets -- -D warnings
cargo test --workspace --lib --bins
bash scripts/check-no-raw-process-exit.sh
bash scripts/check-rust-version.sh
bash scripts/check-window-owner-registration.sh
bash scripts/check-workspace-crates.sh
bash scripts/pty-backend-feasibility.sh --check
bash scripts/test-resource-inventory.sh
bash scripts/test-resource-baseline-evidence.sh
bash scripts/test-soak-harness.sh
bash scripts/test-release-notes.sh
scripts/rust-logic-coverage.sh
```

**Run the list to the end before concluding anything.** `--lib --bins`
excludes every `tests/` binary, so it can pass while an integration test is
broken; `check-workspace-crates.sh` is the step that runs `--tests` per crate
and catches that. A green `cargo test --workspace --lib --bins` on its own
means the unit tests pass, not that CI will.

The second clippy line is not a duplicate. `--workspace --all-targets` does
not imply `--all-features`, and `ssh` is off by default, so the SSH backend is
compiled by no other command in this list.

Two more limits worth knowing before trusting a green run:

- `rust-logic-coverage.sh` measures a deterministic-logic subset and skips 11
  of the 23 crates outright, including `sonicterm-app` and `sonicterm-gpu`. A
  passing coverage figure says nothing about code in those crates. It is also
  macOS-only in CI.
- Tests behind `#![cfg(target_os = "windows")]` compile to nothing on macOS,
  so a Windows-gated test file that would fail to *compile* still reports
  `ok` locally. Cross-compiling to check is not available — the vendored
  Cairo dependency is host-architecture-only. Windows CI is the only place
  those are exercised.

For release prep also run:

```bash
cargo build --release -p sonicterm-mac
```

Before opening a release PR, verify that README and `wiki/` match any changed
config, logging, window, palette, or input behavior.
After pushing a release tag, verify the GitHub release workflow finishes and
publishes the expected macOS DMG(s), Windows MSI, and checksum assets.

## Conventions

- **Every issue and pull request carries labels and a milestone.** Set them
  when you open the item, not afterwards: an unlabelled issue with no
  milestone does not appear in the filtered views the work is tracked
  through, so it is invisible to everyone who is not reading the raw list.
  Pick labels that describe the change — `bug`, `enhancement`,
  `documentation`, `chore`, `refactor`, `perf`, `regression` — plus a
  `platform:` label when the change is not cross-platform. Use the milestone
  the work ships in. `gh issue create` and `gh pr create` take `--label` and
  `--milestone` directly; `gh issue edit` and `gh pr edit` fix an item that
  was opened without them.
- **Flowcharts and data-flow diagrams in markdown are `mermaid` fenced blocks.**

  Hand-drawn ASCII loses alignment across fonts and cannot be edited without
  redrawing the whole picture. Directory trees and layout wireframes stay as
  plain text — their meaning lives in the character positions, which Mermaid
  discards. Because `wiki/` pages are bilingual, a converted diagram must be
  converted in both halves with localized labels and identical structure.
- **Keep first-party shell automation flat in `scripts/`.** Every SonicTerm-owned
  `.sh` and `.ps1` file must be a direct child of `scripts/`; do not create
  nested script folders or top-level `tools/` or `packaging/` directories.
  Embedded upstream FreeType, libpng, zlib, and HarfBuzz scripts retain their
  vendored layouts and are exempt. Packaging executables belong in `scripts/`,
  while maintained packaging instructions belong on the `Packaging` wiki page.
- **Unit tests use the exact flat `file_tests.rs` sibling pattern, never inline.**
  For every source file `foo.rs`, put its unit tests beside it in
  `foo_tests.rs` and declare them from `foo.rs` with
  `#[cfg(test)] #[path = "foo_tests.rs"] mod foo_tests;`. Crate-root tests use
  `lib_tests.rs` (or `main_tests.rs` for a binary) with the same declaration
  pattern. Do not use `#[cfg(test)] mod tests { … }`, a generic `tests.rs`, or a
  `<module>/tests.rs` subdirectory. Tests stay in-crate for private-item access
  via `use super::*;`; Rust does not discover sibling files automatically, so
  every `file_tests.rs` requires its source-module declaration.
- **`tests/` is for cross-crate integration only.** Reserve each crate's
  `tests/` directory for genuine integration tests that exercise the crate
  through its public API or across crate boundaries. Do not put trivial
  "does this symbol export" checks there — fold those into `lib_tests.rs`.
- **Some test state is process-global; take the lock.** The media staging
  pools, the live-capture count, and the inline-media charge counters are
  process-wide, so a test that creates a capture or a charge perturbs any
  sibling measuring one — the sibling fails, reporting a defect that is not
  there. `MEDIA_COUNTER_LOCK` (`app/media.rs`) and `serialised_captures()`
  (`vt_tests.rs`) exist for this and carry the measured failure rate in their
  docs. Hold one for the whole life of any capture or charge the test creates,
  not merely while asserting about it.
- **Comments describe behavior, not history.** Explain what the code does and
  the problem it solves; do not cite issue/PR/Epic numbers or reviewer names
  in comments, log strings, or panic messages.

## Release

SonicTerm releases are created by pushing a `v*` tag. The tag workflow builds:

- macOS Apple Silicon and Intel `.dmg` files
- Windows x64 `.msi`
- release notes from commits since the previous tag

## Wiki

The repository-tracked `wiki/` directory is the **only source of truth** for
SonicTerm's bilingual user documentation. Edit and review wiki pages in the
same branch and pull request as the behavior they describe. Do not clone,
refresh from, publish to, or otherwise maintain a separate wiki repository.

## WezTerm

SonicTerm thanks WezTerm and uses it as the reference for terminal behavior,
font behavior, keymap conventions, and rendering edge cases. Absorb proven
behavior into Sonic-owned crates; do not reintroduce a `vendor/` dependency.
