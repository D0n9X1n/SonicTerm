# Code Ownership

[简体中文](Code-Ownership-zh-CN)

This page records the current owner of each part of the repository.

## Lanes

Two lanes own code: dev:mac and dev:windows. There is no Linux lane; Linux code
is owned through the crate and file rules below. A lane edits another lane's
paths only after that lane confirms on the tracker issue. A shared path has no
single owner: either lane edits it after claiming the change on the tracker
issue.

## Crates

A crate's primary owner is the lane with more merged pull requests that changed
it. Where the smaller count is at least 75% of the larger, the crate is shared,
so either lane claims a change before editing it. `sonicterm-resource` is tied
and has no primary owner. The counts are merged pull requests from each lane.

| Crate | dev:mac PRs | dev:windows PRs | Primary owner | Shared |
| --- | ---: | ---: | --- | --- |
| `sonicterm-app` | 77 | 56 | dev:mac | no |
| `sonicterm-gpu` | 21 | 42 | dev:windows | no |
| `sonicterm-ui` | 14 | 11 | dev:mac | yes |
| `sonicterm-cfg` | 13 | 10 | dev:mac | yes |
| `sonicterm-io` | 12 | 7 | dev:mac | no |
| `sonicterm-text` | 8 | 19 | dev:windows | no |
| `sonicterm-windows` | 9 | 16 | dev:windows | no |
| `sonicterm-mac` | 11 | 6 | dev:mac | no |
| `sonicterm-logging` | 6 | 8 | dev:windows | yes |
| `sonicterm-types` | 7 | 6 | dev:mac | yes |
| `sonicterm-font` | 5 | 6 | dev:windows | yes |
| `sonicterm-grid` | 9 | 4 | dev:mac | no |
| `sonicterm-vt` | 8 | 2 | dev:mac | no |
| `sonicterm-render-model` | 8 | 2 | dev:mac | no |
| `sonicterm-app-core` | 5 | 0 | dev:mac | no |
| `sonicterm-font-config` | 3 | 1 | dev:mac | no |
| `sonicterm-resource` | 2 | 2 | none (tie) | yes |
| `sonicterm-engine` | 1 | 3 | dev:windows | no |
| `sonicterm-linux` | 2 | 5 | dev:windows | no |
| `sonicterm-block-glyph` | 2 | 0 | dev:mac | no |
| `sonicterm-fontconfig` | 2 | 0 | dev:mac | no |
| `sonicterm-harfbuzz` | 2 | 0 | dev:mac | no |
| `sonicterm-freetype` | 2 | 0 | dev:mac | no |

## Operating-system files

A file for one operating system belongs to that system's lane, whichever lane
owns the rest of its crate: macOS files belong to dev:mac and Windows files to
dev:windows. There is no Linux lane, so Linux files, and Unix files that macOS
and Linux share, follow the primary owner of their crate.

In every crate, shared crates included, the file name decides:

- `windows.rs` and `windows_tests.rs` belong to dev:windows;
- `macos.rs` and `macos_tests.rs` belong to dev:mac;
- `linux.rs`, `unix.rs` and their tests belong to the crate's primary owner.

`sonicterm-resource` has no primary owner, so its Linux and Unix files are
shared like the rest of the crate.

## Shared paths

`.github/`, `scripts/`, `wiki/` and the root `CLAUDE.md` are shared. The CI
timeout policy belongs to dev:windows, although `.github/` is shared. Software
rendering, described in [Rendering Modes](Rendering-Modes), belongs to
dev:windows.
