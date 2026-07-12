# Release

SonicTerm releases are tag-driven.

## Version

The workspace version is `1.1.0`. Release tags use `v<major>.<minor>.<patch>`,
for example:

```sh
git tag v1.1.0
git push origin v1.1.0
```

## Automation

`.github/workflows/release.yml` runs on every `v*` tag:

1. Unit tests on macOS and Windows.
2. macOS Apple Silicon and Intel release builds and unsigned `.dmg` files.
3. Windows x64 release build and unsigned `.msi`.
4. Release notes from `scripts/release-notes.sh`, summarizing commits since the
   previous version tag.
5. GitHub Release publication with both installers and `SHA256SUMS.txt`
   attached as downloadable files.

## Local release checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo metadata --no-deps --format-version 1
cargo test --workspace --lib --bins
bash scripts/check-workspace-crates.sh
scripts/coverage/rust-logic.sh
bash scripts/test-release-notes.sh
cargo build --release -p sonicterm-mac
```

Windows packaging is produced with `cargo wix` from
`crates/sonicterm-windows/wix/main.wxs`.
