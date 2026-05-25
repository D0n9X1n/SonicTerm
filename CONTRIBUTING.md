# Contributing to Sonic Terminal

Thanks for your interest! Sonic is in early development — issues, ideas, and
PRs are all welcome.

## Development setup

1. Install Rust (stable; `rust-toolchain.toml` will auto-select).
2. Clone and build:
   ```bash
   git clone git@github.com:D0n9X1n/sonic.git
   cd sonic
   cargo build
   ```
3. Run on your platform:
   ```bash
   cargo run -p sonic-mac        # macOS
   cargo run -p sonic-windows    # Windows
   ```

Crates live at the top level of the repo (`sonic-core/`, `sonic-shared/`,
`sonic-mac/`, `sonic-windows/`) — there is **no** `crates/` directory.

## Before opening a PR

The CI runs these exact commands on macOS-14 and windows-latest. Run them
locally first:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # currently 171 tests across the workspace
cargo deny check                # optional locally; required in CI
```

Headless smoke examples (handy when GPU/window aren't available):

```bash
cargo run --example pty_dump        -p sonic-core   --release   # prints "[e2e] OK"
cargo run --example altscreen_smoke -p sonic-core
cargo run --example pane_smoke      -p sonic-shared
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

Scope is the crate or component (`core`, `mac`, `windows`, `shared`, `ci`,
`assets`, ...). This drives the auto-generated changelog at release time.

## Code style

- `rustfmt` settings live in `rustfmt.toml`.
- `clippy` settings live in `clippy.toml`.
- Public APIs should be documented.
- **Tests live in each crate's `tests/` folder** (integration-style against
  the public API), not inline `#[cfg(test)] mod tests` blocks. New tests
  should follow that convention so the public surface stays exercised.

## Releasing

Maintainers only:

1. Bump versions in `Cargo.toml` (workspace `package.version`).
2. Update `CHANGELOG.md`.
3. Tag: `git tag v0.6.0 && git push origin v0.6.0`.
4. `release.yml` builds `.dmg` + `.msi` and publishes a GitHub Release.

Pre-release tags (e.g. `v0.7.0-alpha.1`) are auto-marked as pre-release.

## License

By contributing you agree to license your work under the MIT License.
