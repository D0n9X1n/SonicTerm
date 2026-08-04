# Contributing to SonicTerm Terminal

Thanks for your interest! SonicTerm is in early development — issues, ideas, and
PRs are all welcome.

## Development setup

1. Install Rust (stable; `rust-toolchain.toml` will auto-select).
2. Clone and build:
   ```bash
   git clone git@github.com:D0n9X1n/SonicTerm.git
   cd SonicTerm
   cargo build
   ```
3. Run on your platform:
   ```bash
   cargo run -p sonicterm-mac        # macOS
   cargo run -p sonicterm-windows    # Windows
   ```

Crates live under `crates/`. Before changing boundaries or diagnostics, read
[Architecture](wiki/Architecture.md),
[Architecture Internals](wiki/Architecture-Internals.md),
[Crate Reference](wiki/Crate-Reference.md), [Logging](wiki/Logging.md), and
[Packaging](wiki/Packaging.md).

## Before opening a PR

CI runs workspace unit tests and a per-crate unit/build gate on macOS and
Windows. Run them locally first:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p sonicterm-io --features ssh --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
RUSTDOCFLAGS="-D warnings" cargo doc -p sonicterm-io --no-deps --features ssh
cargo test --workspace --lib --bins
bash scripts/check-authored-rust-comments.sh
bash scripts/check-no-raw-process-exit.sh
bash scripts/check-rust-version.sh
bash scripts/check-window-owner-registration.sh
bash scripts/check-workspace-crates.sh
bash scripts/pty-backend-feasibility.sh --check
bash scripts/test-resource-inventory.sh
bash scripts/test-resource-baseline-evidence.sh
bash scripts/test-soak-harness.sh
bash scripts/test-release-notes.sh
bash scripts/test-wiki-publish.sh
scripts/rust-logic-coverage.sh
```

## Branches

- `main` is always releasable.
- Feature branches: `feat/<topic>`, `fix/<topic>`, `perf/<topic>`,
  `refactor/<topic>`, `docs/<topic>`, `chore/<topic>`.
- Open a PR against `main`.

## Commit messages

We follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(mac):    add native tab drag handler
fix(core):    handle malformed CSI without panic
perf(render): batch glyph atlas uploads
docs:         add config schema
chore(ci):    cache cargo registry
```

Scope is the crate or component (`app-core`, `gpu`, `mac`, `windows`, `types`,
`ci`, `assets`, ...). This drives the auto-generated changelog at release time.

## Code style

- `rustfmt` settings live in `rustfmt.toml`.
- `clippy` settings live in `clippy.toml`.
- Effectively public authored functions and public trait functions need purpose
  Rustdoc; public unsafe functions also need a `# Safety` section.
- Use substantive `// When:`, `// SAFETY:`, `// Lock order:`, `// Ordering:`,
  and `// Lifecycle:` comments at the exact boundaries required by
  `scripts/check-authored-rust-comments.sh`. Keep marker prose tied to the
  relevant identifiers, within two comment lines and 160 characters, and about
  current behavior rather than issue or implementation history.
- Keep code production-focused and small; tests should cover meaningful behavior
  and edge cases rather than derives, getters, or exports.

## Releasing

Maintainers only:

1. Ensure the workspace version in `Cargo.toml` is `1.1.7`.
2. Tag: `git tag v1.1.7 && git push origin v1.1.7`.
3. `release.yml` builds `.dmg` + `.msi` and publishes a GitHub Release.

Pre-release tags (e.g. `v1.2.0-alpha.1`) are auto-marked as pre-release.

## License

By contributing you agree to license your work under the MIT License.
