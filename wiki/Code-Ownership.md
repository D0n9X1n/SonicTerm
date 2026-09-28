# Code Ownership

[简体中文](Code-Ownership-zh-CN)

Each path belongs to a platform area. No area or path is assigned to a
development agent; agents take work by claiming it.

## Platform areas

| Area | Paths |
| --- | --- |
| macOS | `crates/sonicterm-mac/`, every `macos.rs` and `macos_tests.rs`, and any other file compiled only for macOS |
| Windows | `crates/sonicterm-windows/`, every `windows.rs` and `windows_tests.rs`, and any other file compiled only for Windows |
| Linux | `crates/sonicterm-linux/`, every `linux.rs` and `linux_tests.rs`, and any other file compiled only for Linux |
| Unix | every `unix.rs` and `unix_tests.rs`, shared by macOS and Linux |
| Shared | every other path, including `.github/`, `scripts/`, `wiki/` and `CLAUDE.md` |

Path and URL detection is shared: `crates/sonicterm-cfg/src/url_scan.rs` and
`crates/sonicterm-app/src/app/path_target.rs`. Only `path_target/*.rs` and
`url_open/*.rs` are per platform.

## Taking work

- Any number of agents work at the same time.
- An agent claims an issue before editing: it comments on the issue or its
  tracker issue and names the paths it will change. The first claim wins,
  unless the maintainer assigns the issue.
- The claimant opens the pull request, with its agent label, the other labels
  and the milestone. It owns the pull request until it merges or closes: it
  watches the CI, fixes failures and merges.
- Other agents do not edit a claimed path, or push to, re-run, cancel or merge
  the claimant's pull request, unless the claimant asks. To change a claimed
  path, ask the claimant on the issue.
- A claim ends when its pull request merges or closes, or when the claimant
  releases it.
