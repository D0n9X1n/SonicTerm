# Development and Release / 开发与发布

## English

> Canonical packaging instructions:
> [`docs/packaging/`](https://github.com/D0n9X1n/SonicTerm/tree/main/docs/packaging).
> Canonical release boundary:
> [`docs/ARCHITECTURE.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/ARCHITECTURE.md).

## Repository layout

```text
Cargo.toml                 workspace members, shared version/dependencies/lints
crates/                    23 first-party Rust crates
assets/                    fonts, themes, keymaps, icons, i18n, screenshots
docs/                      canonical architecture/logging/modules/packaging docs
wiki/                      canonical bilingual user documentation
scripts/                   flat first-party shell/PowerShell automation
.github/                   CI, release, issue, PR, and dependency automation
```

The workspace uses Rust edition 2021 and a pinned minimum Rust version from the
root manifest. `rust-toolchain.toml` selects stable with rustfmt and clippy.
`Cargo.toml [workspace.package].version` is authoritative for every first-party
crate and internal path requirement.

## Build and run

```sh
cargo build
cargo run -p sonicterm-mac       # macOS
cargo run -p sonicterm-windows   # Windows
```

Windows CI and release builds install static Cairo through vcpkg. macOS release
builders install Cairo and pkg-config through Homebrew. Native Fontconfig,
FreeType, HarfBuzz, AppKit, Win32, and installer behavior require their relevant
platform or build boundary; do not replace those checks with empty symbol tests.

## Crate-local guidance

Every crate has a local `CLAUDE.md` describing purpose, key files, local gate,
guardrails, and cross-references. Read the root instructions and the relevant
crate instructions before changing a boundary.

The short crate map is [Crate Reference](Crate-Reference) and
[`docs/MODULES.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/MODULES.md).

## Test organization

Unit tests follow one exact flat sibling convention:

```text
foo.rs
foo_tests.rs
```

`foo.rs` declares:

```rust
#[cfg(test)]
#[path = "foo_tests.rs"]
mod foo_tests;
```

Crate roots use `lib_tests.rs` or `main_tests.rs`. Do not create inline
`mod tests`, generic `tests.rs`, or `<module>/tests.rs`. A crate's `tests/`
directory is reserved for genuine integration tests that exercise public or
cross-crate behavior.

The test surface includes:

- reducer intent/effect contracts;
- VT control-sequence and same-frame dirty-row regressions;
- grid wide-cell, scrollback, line-storage, and hyperlink behavior;
- UI tabs/search/selection/IME/pane layout;
- text atlas and row-cache behavior;
- GPU pure damage/color/software-composition helpers;
- app cross-window, redraw, broadcast, resize, and update flows;
- platform-specific CLI and software-present primitives.

## Local gates

The complete local verification set documented by the architecture and PR
template is:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo metadata --no-deps --format-version 1
cargo test --workspace --lib --bins
bash scripts/check-no-raw-process-exit.sh
bash scripts/check-workspace-crates.sh
scripts/rust-logic-coverage.sh
bash scripts/test-release-notes.sh
bash scripts/pty-backend-feasibility.sh --check
bash scripts/test-resource-inventory.sh
bash scripts/test-soak-harness.sh
bash scripts/test-resource-baseline-evidence.sh
```

The first eight are the gate documented in the root `CLAUDE.md` and the
architecture document; the last four are the resource-evidence and soak checks
CI also runs. Note that `cargo test --workspace --lib --bins` excludes
integration tests — the cross-crate suites, including the counting-allocator
heap-truth tests, run under `scripts/rust-logic-coverage.sh`.

Release preparation additionally builds the shipping platform binary, for
example:

```sh
cargo build --release -p sonicterm-mac
```

`check-no-raw-process-exit.sh` requires shipped process exits to pass through
`sonicterm_logging::exit_with`. `check-workspace-crates.sh` derives all members
from Cargo metadata and runs each crate's supported unit/build gate.
`rust-logic-coverage.sh` uses `cargo llvm-cov`, excludes native/non-deterministic
boundaries by explicit regex, and requires 80% line coverage of deterministic
Rust logic.

## Pull-request CI

`.github/workflows/ci.yml` runs on pull requests and on pushes to the branches
its `on.push.branches` list names — currently `main` and the active release
branch — with a macOS 14 / Windows latest matrix:

```text
checkout + stable Rust
  -> Windows: cache/install Cairo with vcpkg
  -> process-exit policy
  -> cargo test --workspace --lib --bins
  -> per-crate unit/build gate
  -> release-note unit test
  -> frozen PTY feasibility evidence check
  -> resource inventory verification
  -> deterministic soak control gate
  -> resource baseline evidence collector test + capture
  -> macOS only: install cargo-llvm-cov and enforce coverage
```

At the current repository revision, CI does **not** execute the documented
`cargo fmt` or `cargo clippy` commands, and `deny.toml` is not enforced by a
`cargo deny check` job. These are tracked as improvement proposals in
[Architecture Evolution](Architecture-Evolution), not described as current gates.

## Coverage boundary

Coverage is designed for deterministic Rust logic. The script excludes native
GPU surfaces, real PTYs/SSH, AppKit/Win32, generated FFI, installer code, and
similar boundaries where a unit test would not prove the real behavior. Those
surfaces rely on platform CI, integration tests, release builds, and manual smoke
checks.

The coverage job currently runs only on macOS. Windows-only deterministic logic
is compiled and tested by Windows CI but does not contribute to the llvm-cov
threshold.

## Release sequence

Pushing a matching `v*` tag starts `.github/workflows/release.yml`:

```text
vX.Y.Z tag
  -> macOS workspace/per-crate/release-note tests
  -> Windows workspace/per-crate/release-note tests
  -> build macOS x86_64 binary
  -> build macOS aarch64 binary
  -> verify architectures and package two DMGs
  -> build Windows x64 binary and WiX MSI
  -> download all artifacts on Ubuntu
  -> require at least one DMG and MSI
  -> generate SHA256SUMS.txt and release notes
  -> publish GitHub Release
```

Pre-release tags containing `-` are marked prerelease automatically.

### macOS assets

The workflow publishes:

- `SonicTerm-<version>-mac-aarch64.dmg`
- `SonicTerm-<version>-mac-x86_64.dmg`

The package includes themes, keymaps, fonts, icons, i18n, app metadata, and an
ad-hoc signature. Release verification checks each binary's architecture with
`lipo` before building the DMGs.

### Windows assets

The workflow publishes an x64 `.msi` built with `cargo wix` and WiX 3.14. The
MSI contains the executable, themes, keymaps, bundled fonts, and shortcuts.

### Shared assets

The publish job creates `SHA256SUMS.txt` over all DMG/MSI files and generates
release notes from commits since the previous tag. The release workflow fails
if either a DMG or MSI is absent.

## Manual release checks

Before a release, verify user-facing README, canonical docs, and all tracked
`wiki/` pages against changed config, input, logging, palette, rendering, and
window behavior. Recommended smoke checks include:

- launch the packaged app;
- exercise Vim/nvim alternate-screen entry, scrolling, and exit;
- create busy multi-pane output;
- tear a tab into a child window, close it, and confirm surviving windows remain
  responsive and child processes are reaped;
- inspect hardware/software adapter logs where relevant.

## Documentation and Wiki workflow

The repository-tracked `wiki/` directory is the only source of truth for
SonicTerm's bilingual user documentation. Edit the relevant Markdown pages in
the same branch and pull request as the behavior they describe so code and user
guidance are reviewed and versioned together. Do not maintain or publish a
separate wiki repository.

## Other automation

| File | Purpose |
| --- | --- |
| `scripts/release-notes.sh` | commit-derived release notes and asset list |
| `scripts/test-release-notes.sh` | throwaway-repository unit test for notes |
| `scripts/bake-icons.sh` | regenerate platform icon exports |
| `scripts/regenerate-freetype.sh` | regenerate FreeType bindings |
| `scripts/regenerate-harfbuzz.sh` | regenerate HarfBuzz bindings |
| `scripts/setup-windows-cairo.ps1` | install/export static Cairo via vcpkg |
| `deny.toml` | advisory, license, source, and wildcard-dependency policy |
| `.github/dependabot.yml` | weekly dependency update policy |

## Where to read the code

| Topic | Primary paths |
| --- | --- |
| Contributor workflow | `CONTRIBUTING.md`, `.github/pull_request_template.md` |
| CI | `.github/workflows/ci.yml` |
| Release | `.github/workflows/release.yml` |
| Coverage | `scripts/rust-logic-coverage.sh` |
| Per-crate gate | `scripts/check-workspace-crates.sh` |
| Exit policy | `scripts/check-no-raw-process-exit.sh` |
| macOS package | `scripts/make-macos-dmg.sh`, `docs/packaging/macos.md` |
| Windows package | `crates/sonicterm-windows/wix/main.wxs`, `docs/packaging/windows.md` |
| Wiki publication rule | root `CLAUDE.md` under “Wiki” |

## 中文

> 规范打包说明：
> [`docs/packaging/`](https://github.com/D0n9X1n/SonicTerm/tree/main/docs/packaging)。
> 规范发布边界：
> [`docs/ARCHITECTURE.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/ARCHITECTURE.md)。

## 仓库布局

```text
Cargo.toml                 workspace member、共享版本/依赖/lint
crates/                    23 个第一方 Rust crate
assets/                    字体、主题、keymap、icon、i18n、截图
docs/                      规范架构/日志/module/打包文档
wiki/                      规范双语用户文档
scripts/                   扁平第一方 shell/PowerShell 自动化
.github/                   CI、发布、issue、PR 和依赖自动化
```

workspace 使用 Rust edition 2021，最低 Rust 版本由根 manifest 固定。`rust-toolchain.toml`
选择 stable，并安装 rustfmt 与 clippy。`Cargo.toml [workspace.package].version` 是全部第一方
crate 和内部 path requirement 的权威版本。

## 构建与运行

```sh
cargo build
cargo run -p sonicterm-mac       # macOS
cargo run -p sonicterm-windows   # Windows
```

Windows CI/release 经 vcpkg 安装静态 Cairo；macOS release builder 经 Homebrew 安装 Cairo 与 pkg-config。
Fontconfig、FreeType、HarfBuzz、AppKit、Win32 和 installer 行为需要相应平台/build boundary，
不能用空洞的符号测试代替。

## Crate 本地说明

每个 crate 都有本地 `CLAUDE.md`，记录 purpose、关键文件、local gate、guardrail 和 cross-reference。
修改边界前先阅读根说明和相关 crate 说明。

简短 crate 映射见 [Crate 参考](Crate-Reference) 与
[`docs/MODULES.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/MODULES.md)。

## 测试组织

单元测试使用唯一的扁平 sibling 规范：

```text
foo.rs
foo_tests.rs
```

`foo.rs` 中声明：

```rust
#[cfg(test)]
#[path = "foo_tests.rs"]
mod foo_tests;
```

crate root 使用 `lib_tests.rs` 或 `main_tests.rs`。不要创建 inline `mod tests`、通用 `tests.rs`
或 `<module>/tests.rs`。crate 的 `tests/` 目录只用于真正通过 public API 或跨 crate 行为的 integration test。

测试覆盖包括：

- reducer intent/effect 契约；
- VT 控制序列与同帧脏行回归；
- grid 宽字符、scrollback、line storage、hyperlink；
- UI 标签页、搜索、选区、IME、pane layout；
- text atlas 与 row cache；
- GPU 纯 damage/color/software composition helper；
- app 跨窗口、redraw、broadcast、resize 与 update flow；
- 平台专属 CLI 与 software-present primitive。

## 本地 gate

架构文档与 PR template 记录的完整本地验证集合：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo metadata --no-deps --format-version 1
cargo test --workspace --lib --bins
bash scripts/check-no-raw-process-exit.sh
bash scripts/check-workspace-crates.sh
scripts/rust-logic-coverage.sh
bash scripts/test-release-notes.sh
bash scripts/pty-backend-feasibility.sh --check
bash scripts/test-resource-inventory.sh
bash scripts/test-soak-harness.sh
bash scripts/test-resource-baseline-evidence.sh
```

前八条是根 `CLAUDE.md` 与架构文档记录的 gate；后四条是 CI 同样执行的资源证据与
soak 检查。注意 `cargo test --workspace --lib --bins` 不包含集成测试——跨 crate
套件（含计数分配器 heap-truth 测试）由 `scripts/rust-logic-coverage.sh` 执行。

release prep 还要构建发布平台二进制，例如：

```sh
cargo build --release -p sonicterm-mac
```

`check-no-raw-process-exit.sh` 要求发布代码只通过 `sonicterm_logging::exit_with` 退出。
`check-workspace-crates.sh` 从 Cargo metadata 推导 member 并运行各 crate 可支持的 unit/build gate。
`rust-logic-coverage.sh` 使用 `cargo llvm-cov`，通过显式 regex 排除原生/非确定性边界，并要求确定性 Rust logic
达到 80% line coverage。

## Pull-request CI

`.github/workflows/ci.yml` 在 PR 以及推送到其 `on.push.branches` 所列分支（当前为
`main` 与当前发布分支）时运行 macOS 14 / Windows latest matrix：

```text
checkout + stable Rust
  -> Windows：缓存/安装 vcpkg Cairo
  -> process-exit policy
  -> cargo test --workspace --lib --bins
  -> per-crate unit/build gate
  -> release-note unit test
  -> 冻结的 PTY feasibility evidence 校验
  -> resource inventory 校验
  -> 确定性 soak control gate
  -> resource baseline evidence 收集器测试与采集
  -> 仅 macOS：安装 cargo-llvm-cov 并执行 coverage gate
```

当前仓库 revision 的 CI **不执行**文档要求的 `cargo fmt` 或 `cargo clippy`，`deny.toml` 也没有由
`cargo deny check` job 强制。它们在 [架构演进](Architecture-Evolution) 中作为建议跟踪，不能描述为现有 gate。

## Coverage 边界

coverage 面向确定性 Rust logic。脚本排除原生 GPU surface、真实 PTY/SSH、AppKit/Win32、生成 FFI、
installer 等单元测试无法证明真实行为的边界。这些 surface 依赖平台 CI、integration test、release build 和手工 smoke check。

coverage job 当前只在 macOS 运行。Windows-only 确定性 logic 会在 Windows CI 编译和测试，但不计入 llvm-cov threshold。

## 发布顺序

push 匹配的 `v*` tag 会启动 `.github/workflows/release.yml`：

```text
vX.Y.Z tag
  -> macOS workspace/per-crate/release-note 测试
  -> Windows workspace/per-crate/release-note 测试
  -> 构建 macOS x86_64 binary
  -> 构建 macOS aarch64 binary
  -> 校验架构并打包两个 DMG
  -> 构建 Windows x64 binary 和 WiX MSI
  -> Ubuntu 下载所有 artifact
  -> 要求至少存在 DMG 和 MSI
  -> 生成 SHA256SUMS.txt 与 release notes
  -> 发布 GitHub Release
```

包含 `-` 的 prerelease tag 会自动标记为 prerelease。

### macOS 资产

workflow 发布：

- `SonicTerm-<version>-mac-aarch64.dmg`
- `SonicTerm-<version>-mac-x86_64.dmg`

package 包含 theme、keymap、font、icon、i18n、app metadata 和 ad-hoc signature。
构建 DMG 前用 `lipo` 校验 binary architecture。

### Windows 资产

workflow 发布由 `cargo wix` 和 WiX 3.14 生成的 x64 `.msi`，包含 executable、theme、keymap、
内置字体和 shortcut。

### 共享资产

publish job 为全部 DMG/MSI 生成 `SHA256SUMS.txt`，并根据上一个 tag 之后的 commit 生成 release notes。
缺少 DMG 或 MSI 时 workflow 失败。

## 手工发布检查

release 前，对照配置、输入、日志、palette、rendering 和 window 行为检查 README、规范 docs 和仓库内全部 `wiki/` 页面。
建议 smoke check：

- 启动打包后的 app；
- 测试 Vim/nvim 备用屏幕进入、滚动与退出；
- 创建繁忙多窗格输出；
- 把 tab 拖到子窗口并关闭，确认存活窗口仍响应、子进程被回收；
- 相关情况下检查硬件/软件 adapter 日志。

## 文档与 Wiki 工作流

仓库内受版本控制的 `wiki/` 目录是 SonicTerm 双语用户文档的唯一事实来源。请在描述相关行为的同一分支和 pull request 中编辑对应 Markdown 页面，让代码和用户指南一起接受审查并保持版本一致。不要维护或发布独立的 wiki 仓库。

## 其它自动化

| 文件 | 用途 |
| --- | --- |
| `scripts/release-notes.sh` | 根据 commit 生成 release note 与资产列表 |
| `scripts/test-release-notes.sh` | 在临时仓库中测试 note 脚本 |
| `scripts/bake-icons.sh` | 重新生成平台 icon export |
| `scripts/regenerate-freetype.sh` | 重新生成 FreeType binding |
| `scripts/regenerate-harfbuzz.sh` | 重新生成 HarfBuzz binding |
| `scripts/setup-windows-cairo.ps1` | 通过 vcpkg 安装/导出静态 Cairo |
| `deny.toml` | advisory、license、source 与 wildcard dependency policy |
| `.github/dependabot.yml` | 每周依赖更新策略 |

## 从哪里阅读源码

| 主题 | 主要路径 |
| --- | --- |
| Contributor workflow | `CONTRIBUTING.md`, `.github/pull_request_template.md` |
| CI | `.github/workflows/ci.yml` |
| Release | `.github/workflows/release.yml` |
| Coverage | `scripts/rust-logic-coverage.sh` |
| Per-crate gate | `scripts/check-workspace-crates.sh` |
| Exit policy | `scripts/check-no-raw-process-exit.sh` |
| macOS package | `scripts/make-macos-dmg.sh`, `docs/packaging/macos.md` |
| Windows package | `crates/sonicterm-windows/wix/main.wxs`, `docs/packaging/windows.md` |
| Wiki 发布规则 | 根 `CLAUDE.md` 的“Wiki”章节 |
