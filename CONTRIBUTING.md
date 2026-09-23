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
   cargo run -p sonicterm-linux      # Linux (binary name: sonicterm)
   ```

Crates live under `crates/`. Before changing boundaries or diagnostics, read
[Architecture](wiki/Architecture.md),
[Architecture Internals](wiki/Architecture-Internals.md),
[Crate Reference](wiki/Crate-Reference.md), [Logging](wiki/Logging.md), and
[Packaging](wiki/Packaging.md).

## Before opening a PR

Run the full [local verification gate](wiki/Development-and-Release.md#local-verification-gate),
including the host-specific checks. That page is the canonical command list and
also defines exact-head PR CI and post-merge Wiki verification.

Documentation uses separate English `<Page>.md` and Chinese `<Page>-zh-CN.md`
files under `wiki/`. Update both translations against the implementation in the
same PR, keep links in the page's language, and use Mermaid for flow explanations.
Load only English files for routine agent context; read Chinese files when editing
or verifying translations.

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

Scope is the crate or component (`app-core`, `gpu`, `mac`, `windows`, `linux`,
`types`, `ci`, `assets`, ...). This drives the auto-generated changelog at release time.

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

Maintainers follow [Development and Release](wiki/Development-and-Release.md#release-workflow)
for version validation, exact-commit CI provenance, owner-approved tags, package
verification, and publication. Do not use a duplicate checklist in place of that gate.

## License

By contributing you agree to license your work under the MIT License.
