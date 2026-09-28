# 代码所有权

[English](Code-Ownership)

每个路径属于一个平台区域。任何区域或路径都不分配给开发 agent；agent 通过认领来接手工作。

## 平台区域

| 区域 | 路径 |
| --- | --- |
| macOS | `crates/sonicterm-mac/`、所有 `macos.rs` 与 `macos_tests.rs`，以及其他只为 macOS 编译的文件 |
| Windows | `crates/sonicterm-windows/`、所有 `windows.rs` 与 `windows_tests.rs`，以及其他只为 Windows 编译的文件 |
| Linux | `crates/sonicterm-linux/`、所有 `linux.rs` 与 `linux_tests.rs`，以及其他只为 Linux 编译的文件 |
| Unix | 所有 `unix.rs` 与 `unix_tests.rs`，由 macOS 与 Linux 共用 |
| 共享 | 其余所有路径，包括 `.github/`、`scripts/`、`wiki/` 与 `CLAUDE.md` |

路径与 URL 检测为共享：`crates/sonicterm-cfg/src/url_scan.rs` 与
`crates/sonicterm-app/src/app/path_target.rs`。只有 `path_target/*.rs` 与 `url_open/*.rs` 按平台区分。

## 接手工作

- 任意数量的 agent 可以同时工作。
- agent 在编辑前认领 issue：在该 issue 或其跟踪 issue 上评论，并列出将要修改的路径。先认领者得，
  除非维护者另行指派。
- 认领者创建 pull request，加上自己的 agent 标签、其他标签与 milestone，并负责该 pull request
  直到合并或关闭：跟踪 CI、修复失败并完成合并。
- 除非认领者提出，其他 agent 不编辑已认领的路径，也不向认领者的 pull request 推送，不重跑、取消或
  合并它。需要修改已认领的路径时，先在 issue 上询问认领者。
- pull request 合并或关闭，或认领者释放认领时，认领结束。
