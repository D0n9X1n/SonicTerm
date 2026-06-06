# Release

SonicTerm releases are tag-driven.

## Version

The workspace version is `1.0.0`. Release tags use `v<major>.<minor>.<patch>`,
for example:

```sh
git tag v1.0.0
git push origin v1.0.0
```

## Automation

`.github/workflows/release.yml` runs on every `v*` tag:

1. Unit tests on macOS and Windows.
2. macOS universal release build and unsigned `.dmg`.
3. Windows x64 release build and unsigned `.msi`.
4. Release notes from `scripts/release-notes.sh`, summarizing commits since the
   previous version tag.
5. GitHub Release publication with both installers attached.

## Local release checks

```sh
cargo test --workspace --lib --bins
cargo build --release -p sonicterm-mac
bash scripts/test-release-notes.sh
```

Windows packaging is produced with `cargo wix` from
`crates/sonicterm-windows/wix/main.wxs`.
