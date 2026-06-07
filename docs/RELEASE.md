# Release

SonicTerm releases are tag-driven.

## Version

The workspace version is `0.9.2`. Release tags use `v<major>.<minor>.<patch>`,
for example:

```sh
git tag v0.9.2
git push origin v0.9.2
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
cargo test --workspace --lib --bins
bash scripts/check-workspace-crates.sh
cargo build --release --target aarch64-apple-darwin -p sonicterm-mac
cargo build --release --target x86_64-apple-darwin -p sonicterm-mac
bash scripts/test-release-notes.sh
```

Windows packaging is produced with `cargo wix` from
`crates/sonicterm-windows/wix/main.wxs`.
