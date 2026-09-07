## Summary
<!-- 1-3 bullets: what changed and why -->

## Type
- [ ] feat
- [ ] fix
- [ ] perf
- [ ] refactor
- [ ] docs
- [ ] chore / ci

## Scope
- [ ] contracts / app-core (`sonicterm-types`, `sonicterm-app-core`)
- [ ] terminal / IO (`sonicterm-vt`, `sonicterm-grid`, `sonicterm-io`)
- [ ] config / UI (`sonicterm-cfg`, `sonicterm-ui`)
- [ ] text / fonts (`sonicterm-text`, `sonicterm-font*`, `sonicterm-engine`)
- [ ] rendering (`sonicterm-render-model`, `sonicterm-block-glyph`, `sonicterm-gpu`)
- [ ] app / platform (`sonicterm-app`, `sonicterm-mac`, `sonicterm-windows`, `sonicterm-linux`)
- [ ] logging / CI / release / docs / assets

## Authored Rust contract
- [ ] Effectively public functions and public trait functions have purpose Rustdoc; public unsafe functions include `# Safety`.
- [ ] Required `// When:`, `// SAFETY:`, `// Lock order:`, `// Ordering:`, and `// Lifecycle:` markers are substantive and checker-anchored.
- [ ] Marker prose names the relevant identifiers, stays within two lines / 160 characters, and describes current behavior rather than task history.

## Test plan
- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo clippy -p sonicterm-io --features ssh --all-targets -- -D warnings`
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc -p sonicterm-io --no-deps --features ssh`
- [ ] `cargo test --workspace --lib --bins`
- [ ] `bash scripts/check-authored-rust-comments.sh`
- [ ] `bash scripts/check-no-raw-process-exit.sh`
- [ ] `bash scripts/check-rust-version.sh`
- [ ] `bash scripts/check-window-owner-registration.sh`
- [ ] `bash scripts/check-workspace-crates.sh`
- [ ] `bash scripts/pty-backend-feasibility.sh --check`
- [ ] `bash scripts/test-resource-inventory.sh`
- [ ] `bash scripts/test-resource-baseline-evidence.sh`
- [ ] `bash scripts/test-soak-harness.sh`
- [ ] `bash scripts/test-linux-packages.sh`
- [ ] `bash scripts/test-release-assets.sh`
- [ ] `bash scripts/test-release-notes.sh`
- [ ] `bash scripts/test-wiki-publish.sh`
- [ ] `scripts/rust-logic-coverage.sh`
- [ ] Relevant release/platform build or manual launch completed
- [ ] Screenshots / recordings attached (UI changes)

## Notes for reviewers
<!-- anything tricky, follow-ups, known gaps -->
