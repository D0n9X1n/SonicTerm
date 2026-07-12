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
[Architecture](docs/ARCHITECTURE.md), [Modules](docs/MODULES.md), and
[Logging](docs/LOGGING.md).

## Before opening a PR

CI runs workspace unit tests and a per-crate unit/build gate on macOS and
Windows. Run them locally first:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib --bins
bash scripts/check-workspace-crates.sh
scripts/coverage/rust-logic.sh
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
- Public APIs should be documented.
- Keep code production-focused and small; tests should cover meaningful behavior
  and edge cases rather than derives, getters, or exports.

## Releasing

Maintainers only:

1. Ensure the workspace version in `Cargo.toml` is `1.1.0`.
2. Tag: `git tag v1.1.0 && git push origin v1.1.0`.
3. `release.yml` builds `.dmg` + `.msi` and publishes a GitHub Release.

Pre-release tags (e.g. `v1.2.0-alpha.1`) are auto-marked as pre-release.

## License

By contributing you agree to license your work under the MIT License.
