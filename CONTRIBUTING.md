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

Crates live under `crates/`.

## Before opening a PR

CI runs unit tests on macOS and Windows. Run them locally first:

```bash
cargo test --workspace --lib --bins
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
- Keep code production-focused and small; the previous Rust test suite was
  intentionally cleared and will be rebuilt incrementally.

## Releasing

Maintainers only:

1. Ensure `Cargo.toml` says `1.0.0`.
2. Tag: `git tag v1.0.0 && git push origin v1.0.0`.
3. `release.yml` builds `.dmg` + `.msi` and publishes a GitHub Release.

Pre-release tags (e.g. `v0.7.0-alpha.1`) are auto-marked as pre-release.

## License

By contributing you agree to license your work under the MIT License.
