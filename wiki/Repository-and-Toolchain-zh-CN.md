# 仓库与工具链

[English](Repository-and-Toolchain)

本页说明仓库布局、工具链、构建入口、代码约定，以及内嵌原生源码的维护。

```text
Cargo.toml     workspace member、共享 package metadata、依赖、profile、lint
crates/        23 个第一方 Rust crate
assets/        字体、主题、键位、图标、本地化、截图
wiki/          规范双语文档
scripts/       扁平的第一方 shell 与 PowerShell 自动化
.github/       CI、release、Wiki 发布、issue、PR 与依赖自动化
```

Workspace 使用 resolver 2、Rust edition 2021，最低 Rust 版本为 1.95。
`rust-toolchain.toml` 选择 stable，并安装 rustfmt 与 clippy。权威版本位于
`Cargo.toml [workspace.package].version`；所有 workspace package 与内部 path requirement
都使用该版本。

请在对应原生主机上构建或运行平台入口：

```sh
cargo build
cargo run -p sonicterm-mac       # macOS
cargo run -p sonicterm-windows   # Windows
cargo run -p sonicterm-linux     # Linux；可执行文件名为 sonicterm
```

每个第一方 workspace crate 都有本地 `CLAUDE.md`。单元测试采用扁平 sibling 形式 `foo.rs` +
`foo_tests.rs`，并由 `#[cfg(test)] #[path = "foo_tests.rs"] mod foo_tests;` 声明。
Crate root 使用 `lib_tests.rs` 或 `main_tests.rs`；`tests/` 只用于通过 public API 或跨
crate 行为的 integration test。`sonicterm-ui` 与 `sonicterm-render-model` 的 crate-root
测试会清点每个直接源码模块，并要求它具有准确的 sibling 声明或一条非空的显式豁免说明。
已声明的 sibling 文件必须存在且包含 `#[test]`；源码目录模块会使这项扁平清单失败。模块一旦
获得自己的 sibling suite，对应豁免就会立即变为过期并使测试失败。

对捕获到的 tracing 输出做断言的测试使用 `sonicterm_logging::test_capture`。
它的 `with_default` 包装保留每个测试自己的 subscriber、filter 和 sink，同时用一个不记录事件的
进程全局 dispatcher，避免未捕获线程的首次调用把调用点禁用。捕获范围之外的线程仍不接收事件。
不要在同一个测试进程中将此辅助模块与生产日志初始化混用。

名称要说明它保存的内容。变量、参数、闭包、循环绑定、字段、函数和常量，无论在生产代码还是测试中，
都不使用单个字符、字母后接数字，或不在 `clippy.toml` 的 `allowed-idents-below-min-chars`
列表中的两个字母；应写出量及其单位，例如 `row_count`、`timeout_s` 或 `width_px`。该列表取代
Clippy 默认的允许列表（后者允许 `i`、`x` 和 `y`），收录惯用缩写、真实单词、标题、尺寸与版本记号、
比较 trait 的方法名，以及生命周期 `'a` 和 `'_`。列表还收录 `vt`，即 `sonicterm-vt` 终端模块的名称
（`pub mod vt`）：重命名该模块会改变其默认日志 target，而且没有属性能只豁免模块名本身，因为在
`pub mod vt` 上加 `allow` 会关闭整个 `vt.rs` 的该 lint。泛型类型参数、生命周期、const 泛型、`_`、
vendored 代码、照抄 C 头文件的 `extern` 声明与 `#[repr(C)]` 字段，以及由外部契约固定的名称
（serde 键、日志字段、配置键、CLI 参数）不受此限。Clippy 的 `min_ident_chars` 在启用它的每个
crate 中执行该规则。脚本方面，`script-identifiers` gate 步骤对 Git 跟踪的 `scripts/*.py` 文件运行
`scripts/check-script-identifiers.py`：它检查赋值、`for`、推导式、`with ... as` 与 `except ... as`
目标、函数与 lambda 参数、函数名与类名，以及 import 别名；跳过 `_`、以 `_` 开头的名称、`self`
与 `cls`；将每个发现输出为 `path:line name`；仍有发现时退出码为 1，无法读取 `clippy.toml`、
跟踪文件列表或某个脚本时退出码为 2。

## 原生依赖维护

`scripts/native-dependencies.json` 是内嵌原生库及固定版本 winit 源码的机器可读清单。每个条目固定上游
发布提交、归档校验和、明确的源码子集、在该发布之上携带的上游修复，以及完整导入源码树
的摘要。`upstream_fixes` 记录只表示来源：完整的上游修订号和可供阅读的 URL。仓库不再
保存本地补丁文件，因为 SonicTerm 构建的事实来源是已导入的第三方源码本身，而不是
「归档加补丁序列」。保留必需源码、头文件、许可证和变更日志；不需要的上游示例及 CI
目录不属于构建输入。这独立于 Cargo.lock 和由平台提供的 Cairo/Fontconfig。

`crates/sonicterm-winit` 保留上游 winit 0.30.13 的 Windows/macOS/Linux 源码子集，
以及仅 Windows 使用的原生按键元数据扩展。包名仍为 `winit`，不属于第一方 workspace
成员；目录名不表示它采用 SonicTerm 的版本号。Cargo 固定该版本并通过 patch 指向本地
源码。保留共享代码、桌面后端、必需测试数据、单元测试、Send/Sync/serde 集成测试和
Apache-2.0 许可证；不保留示例、仅供示例使用的开发依赖、历史文档以及
Android/iOS/Web/Redox 后端，不支持的目标会被拒绝。

源码清单保留原始归档校验和及上游修订号，记录导入子集，并固定本地树中每个文件的
摘要。子集列表不会让额外文件逃过验证。修改过的上游文件携带本地修改声明，不将这些
修改列为上游修复。Apache-2.0 许可证随各桌面安装包分发，见[打包](Packaging-zh-CN)。

保留的 winit 上游源码不参加第一方 authored-comment 扫描；自行编写的键盘同级测试
仍接受检查。审查者逐一检查混合上游文件中修改的代码块是否具备用途、安全与控制流说明。
源码树摘要能发现字节漂移，但不能代替该审查。每种桌面主机的 gate 都显式运行该依赖
启用 `serde` 的单元及集成测试，以及将警告视为错误的 Rustdoc，因为 workspace 排除
也会使它不参加 workspace 测试与文档生成。Windows 包含原生元数据单元测试。
两次调用均使用 `--locked` 和子集独立精简后的 `Cargo.lock`；只检查 workspace lockfile
不能覆盖这套依赖图。构建产物写入仓库的 target 目录，不写入固定的源码树。

验证工具只使用 Python 标准库，不访问网络，且只做检查：

```sh
python3 scripts/native-dependencies.py check
python3 scripts/native-dependencies.py check --library freetype
python3 scripts/native-dependencies_tests.py
```

`check` 拒绝缺失、修改或额外的第三方文件，拒绝未固定的源码树摘要，拒绝仍声明已废弃
`patches` 键的清单条目，也拒绝缺少修订号或 URL、或指向本地文件的 `upstream_fixes`
记录。摘要包含路径和原始字节，不包含可执行位及空目录。`.gitattributes` 对第三方源码树
关闭换行转换，使 Windows 检查相同的上游字节。

摘要一致只证明工作副本仍是经过审查并提交的那些字节；它不等于发布者身份验证，也不证明
不存在漏洞，后者来自清单记录的基础发布版本，以及在上游逐条阅读所记录的修复。本工具没有
任何命令可以重建带本地补丁的源码树，也不会假装具备该能力。

每次以单个库为单位进行可审查的更新：

1. 阅读官方发布和安全公告，包括新版本引入的回归。验证发布者/来源和可用签名或独立
   发布的校验和，记录验证限制。新版本不自动等于已修复版本，不使用浮动分支或无人审核导入。
2. 用不可变发布身份和已验证的归档摘要更新清单条目。把每个必需上游修复记录为完整修订号
   和 URL，并在上游审查其前置条件。仅在证明新版本已包含该修复后才移除记录，不禁用无关
   原生功能。
3. 单独下载清单记录的归档并在仓库之外解包。手工导入记录的源码子集，并依据上游提交本身
   逐条应用所记录的上游修复——与所导入的发布版本比对，而不是与仓库内的副本比对。将结果
   与当前第三方目录做差异比较，只替换干净的对应目录；审查该差异后，再把 `check` 输出的
   摘要填入新的 `tree_sha256`。核对新增/删除的 C/C++ 文件和 `build.rs`；导入成功不是
   编译成功。
4. 更新 FreeType/HarfBuzz 时，用 `cargo install bindgen-cli --version 0.71.1 --locked`
   安装工具，再运行 `bash scripts/regenerate-freetype.sh` 或
   `bash scripts/regenerate-harfbuzz.sh`。`BINDGEN` 可指定单独安装的相同版本可执行文件。
   两个脚本编译小型 `scripts/freetype-config.rs` 辅助程序，复用构建时的配置头文件。
   审查 ABI 与行为变化，不能只改版本号；重新生成必须保留 crate 自有模块和测试。
5. 运行离线检查、原生 crate 测试、完整本地 gate，以及正常颜色配置下的原生渲染检查；
   配置/日志使用独立临时目录并保留 HOME。比较可变/彩色字体、CJK、emoji、连字、回退及
   光栅输出。要求准确 head 的平台 CI；本地 macOS 测试不验证 Windows 代码或 Intel
   二进制。在同一 PR 更新 wiki 的两个语言部分。

仓库内的 FreeType 带有 2.14.3 之后两项多余坐标处理修复；仓库内的 zlib 带有 1.3.2 之后
的无效距离解码修复及相关 gzip 写入修复。这些修复已存在于导入的源码中，清单记录了每项
修复的完整上游提交号。这些选择不证明每个公告都能从 SonicTerm 触发，也不表示所有剩余
上游缺陷均已修复。准备每次依赖更新和发布前都应重新检查上游版本/公告；更新始终经过
审查，而不是自动合并。
