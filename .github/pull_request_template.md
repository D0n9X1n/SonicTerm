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
- [ ] `python3 scripts/local-gate.py`
- [ ] Relevant release/platform build or manual launch completed
- [ ] Screenshots / recordings attached (UI changes)

## Notes for reviewers
<!-- anything tricky, follow-ups, known gaps -->
