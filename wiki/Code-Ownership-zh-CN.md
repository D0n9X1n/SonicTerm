# 代码所有权

[English](Code-Ownership)

本页记录仓库各部分当前的所有者。

## Lane

代码由两个 lane 负责：dev:mac 与 dev:windows。没有 Linux lane；Linux 代码按下文的 crate
与文件规则确定所有者。一个 lane 只有在另一 lane 于跟踪 issue 上确认之后，才能编辑属于对方的路径。
共享路径没有单一所有者：任一 lane 先在跟踪 issue 上认领改动，再进行编辑。

## Crate

crate 的主要所有者是合并过更多修改该 crate 的 pull request 的 lane。若较小的计数不低于较大计数的
75%，该 crate 即为共享，任一 lane 都要先认领改动再编辑。`sonicterm-resource` 两个计数相同，
没有主要所有者。表中计数为各 lane 已合并的 pull request 数。

| Crate | dev:mac PR 数 | dev:windows PR 数 | 主要所有者 | 共享 |
| --- | ---: | ---: | --- | --- |
| `sonicterm-app` | 77 | 56 | dev:mac | 否 |
| `sonicterm-gpu` | 21 | 42 | dev:windows | 否 |
| `sonicterm-ui` | 14 | 11 | dev:mac | 是 |
| `sonicterm-cfg` | 13 | 10 | dev:mac | 是 |
| `sonicterm-io` | 12 | 7 | dev:mac | 否 |
| `sonicterm-text` | 8 | 19 | dev:windows | 否 |
| `sonicterm-windows` | 9 | 16 | dev:windows | 否 |
| `sonicterm-mac` | 11 | 6 | dev:mac | 否 |
| `sonicterm-logging` | 6 | 8 | dev:windows | 是 |
| `sonicterm-types` | 7 | 6 | dev:mac | 是 |
| `sonicterm-font` | 5 | 6 | dev:windows | 是 |
| `sonicterm-grid` | 9 | 4 | dev:mac | 否 |
| `sonicterm-vt` | 8 | 2 | dev:mac | 否 |
| `sonicterm-render-model` | 8 | 2 | dev:mac | 否 |
| `sonicterm-app-core` | 5 | 0 | dev:mac | 否 |
| `sonicterm-font-config` | 3 | 1 | dev:mac | 否 |
| `sonicterm-resource` | 2 | 2 | 无（计数相同） | 是 |
| `sonicterm-engine` | 1 | 3 | dev:windows | 否 |
| `sonicterm-linux` | 2 | 5 | dev:windows | 否 |
| `sonicterm-block-glyph` | 2 | 0 | dev:mac | 否 |
| `sonicterm-fontconfig` | 2 | 0 | dev:mac | 否 |
| `sonicterm-harfbuzz` | 2 | 0 | dev:mac | 否 |
| `sonicterm-freetype` | 2 | 0 | dev:mac | 否 |

## 平台专属文件

只服务于某一操作系统的文件属于该系统对应的 lane，与其所在 crate 其余部分由哪个 lane 负责无关：
macOS 文件属于 dev:mac，Windows 文件属于 dev:windows。由于没有 Linux lane，Linux 文件以及
macOS 与 Linux 共用的 Unix 文件，归其 crate 的主要所有者。

在所有 crate 中（包括共享 crate），由文件名决定归属：

- `windows.rs` 与 `windows_tests.rs` 属于 dev:windows；
- `macos.rs` 与 `macos_tests.rs` 属于 dev:mac；
- `linux.rs`、`unix.rs` 及其测试文件属于该 crate 的主要所有者。

`sonicterm-resource` 没有主要所有者，因此其 Linux 与 Unix 文件和该 crate 其余部分一样为共享。

路径与 URL 检测是各操作系统共用的一个组件，而不是按操作系统拆分的代码：
`crates/sonicterm-cfg/src/url_scan.rs` 与 `crates/sonicterm-app/src/app/path_target.rs`
中与平台无关的部分。Windows 也可能显示 POSIX 路径，例如在 WSL shell 中，因此检测逻辑以及
POSIX 与 Windows 两种路径语法保持共用，并在每个操作系统上测试。
只有负责原生打开、在文件管理器中显示与目标分类的文件（`path_target/*.rs` 与 `url_open/*.rs`）按操作系统区分。

## 共享路径

`.github/`、`scripts/`、`wiki/` 与根目录 `CLAUDE.md` 为共享路径。CI 超时策略属于 dev:windows，
尽管 `.github/` 为共享路径。软件渲染属于 dev:windows，见[渲染模式](Rendering-Modes-zh-CN)。
