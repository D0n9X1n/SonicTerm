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
- [ ] terminal / IO (`sonicterm-vt`, `sonicterm-grid`, `sonicterm-io`, `sonicterm-mux`)
- [ ] config / UI (`sonicterm-cfg`, `sonicterm-ui`)
- [ ] text / fonts (`sonicterm-text`, `sonicterm-font*`, `sonicterm-engine`)
- [ ] rendering (`sonicterm-render-model`, `sonicterm-block-glyph`, `sonicterm-gpu`)
- [ ] app / platform (`sonicterm-app`, `sonicterm-mac`, `sonicterm-windows`)
- [ ] logging / CI / release / docs / assets

## Test plan
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace --lib --bins`
- [ ] `bash scripts/check-workspace-crates.sh`
- [ ] `scripts/coverage/rust-logic.sh`
- [ ] `bash scripts/test-release-notes.sh`
- [ ] Relevant release/platform build or manual launch completed
- [ ] Screenshots / recordings attached (UI changes)

## Notes for reviewers
<!-- anything tricky, follow-ups, known gaps -->
