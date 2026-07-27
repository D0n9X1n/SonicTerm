# CLAUDE.md — SonicTerm

SonicTerm is a GPU-accelerated terminal for macOS and Windows. Keep changes
small, typed, and cross-platform unless the crate is explicitly platform-only.
The workspace version is the source of truth (`Cargo.toml` `[workspace.package]`).

## Read first

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — architecture, invariants, verification, and release boundary.
- [`docs/MODULES.md`](docs/MODULES.md) — crate map.
- [`docs/LOGGING.md`](docs/LOGGING.md) — logs, diagnostics, and hang investigation.
- [`docs/packaging/README.md`](docs/packaging/README.md) — maintained macOS and Windows packaging guidance.
- `wiki/` — bilingual user-facing usage/config/keybinding/log/theme docs.

When auditing docs for release blockers, typos, renamed paths, or user-facing
terminology, include `wiki/` alongside README and `docs/`; `wiki/` is the
canonical, repository-tracked user documentation surface.

**Canonical documentation rule:** the core tracked `docs/` surface is
`ARCHITECTURE.md`, `LOGGING.md`, and `MODULES.md`; maintained packaging guidance
lives under `docs/packaging/`. Keep durable architecture, invariants,
verification, and release-boundary information in `docs/ARCHITECTURE.md`; keep
operational logging, diagnostics, and hang-investigation guidance in
`docs/LOGGING.md`; keep `docs/MODULES.md` limited to the crate map. Do not track
standalone implementation specs, plans, review audits, version-audit documents,
or a separate release document in `docs/`. `docs/specs/`, `docs/plans/`, and
`docs/reviews/` are ignored local working folders. Update README/`wiki/` only
when user-facing behavior changes.

When touching a crate, also read that crate's local `CLAUDE.md`.

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
cargo test --workspace --lib --bins
bash scripts/check-no-raw-process-exit.sh
bash scripts/check-window-owner-registration.sh
bash scripts/check-workspace-crates.sh
scripts/rust-logic-coverage.sh
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

- **Keep first-party shell automation flat in `scripts/`.** Every SonicTerm-owned
  `.sh` and `.ps1` file must be a direct child of `scripts/`; do not create
  nested script folders or top-level `tools/` or `packaging/` directories.
  Embedded upstream FreeType, libpng, zlib, and HarfBuzz scripts retain their
  vendored layouts and are exempt. Packaging executables belong in `scripts/`,
  while maintained packaging instructions belong in `docs/packaging/`.
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
