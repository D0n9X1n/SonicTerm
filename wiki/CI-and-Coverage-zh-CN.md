# CI 与 Coverage

[English](CI-and-Coverage)

本页说明 pull-request 与 `main` CI、coverage gate 及通过的 gate 不能证明的内容，以及工作流供应链。
Coverage 证据与重新建立基线见[开发与发布](Development-and-Release-zh-CN#coverage-证据与重新建立基线)。

## Pull-request 与 main CI

`.github/workflows/ci.yml` 在 pull request 和推送到 `main` 时运行。Pull-request run 使用
按 ref 区分的 concurrency group；ref 前进时会取消已过时的 run。每次 `main` push 则使用按
SHA 区分的 group，且不会在运行中被取消，因此后续合并不能抹去前一个 merge SHA 的精确验证记录。

`.github/workflows/perf.yml`（`Performance comparison`）单独运行，且不是必需的 job。它为带 `perf` 标签的
pull request 在 30 分钟内测量一张快速的前后对比表，并为每个 release tag 运行完整对比；两种模式详见
[开发与发布](Development-and-Release-zh-CN#ci-能测量什么)。

只要任一必需的 pull-request job 仍在排队、运行、缺失、被取消、意外跳过或失败，就绝不能
合并，也不能启用 auto-merge。macOS、Windows 与 Ubuntu job 必须都在完全相同的已审核 head
commit 上成功结束后才能合并。Windows 成功是强制条件，因为只有该 job 能可靠编译并运行
Windows-only 测试；本地、macOS、Ubuntu 或 review 结果都不能替代它。每次合并后，必须先验证
Wiki 发布，再开始下一个串行 pull request。成功的 exact-head PR CI 是 PR 工作的 CI 门槛；
`main` CI 仅作为 release 来源验证门槛，不阻塞下一个 PR。

所有等待都必须从主 agent 移出。每个需要等待或监控的生命周期——长时间本地 gate、pull-request
CI、合并后的 Wiki 发布、release 来源验证所需的 `main` CI，或 release workflow——启动一个专用 watcher subagent，
而不是每个 job 启动一个 subagent。交接内容必须不可变并包含 repository/worktree 路径、预期 commit
SHA、PR 编号或 run ID、准确的必需 job 或命令、timeout 与成功标准。Watcher 负责该生命周期，直到
`SUCCESS`、`FAILURE`、`BLOCKED` 或 `STALE`，并报告预期与实际 SHA、run ID、每个必需结果和可执行的
失败证据。若 head 改变，或必需 job 失败、取消、意外跳过，它必须立即返回；绝不能静默跟随替代 run，
也不能只按 branch 名接受 green 结果。

Watcher 运行期间，主 agent 只在基于当前默认分支的独立 worktree 中推进不重叠的工作项，绝不修改
正在测试的 worktree。Watcher 不得 push、merge、tag、publish 或清理共享状态。同一主机一次最多运行
一个完整 Cargo gate 或 build，并且并发 worktree 绝不能共享 `CARGO_TARGET_DIR`；重型 gate 运行期间，
主 agent 应进行 research、编辑或轻量检查。Watcher 报告 failure、blocker 或 stale SHA 时，主 agent
必须立即返回当前生命周期处理。

并发不会放宽发布顺序：当前 pull request 的 exact-head 检查通过前不得合并；当前 pull request
合并且其 exact merge-SHA Wiki 发布验证完成前，不得打开下一个 pull request。推进 PR 工作不等待
`main` CI；验证 release commit 时才要求它成功。之后先把
下一个 worktree 更新到新的默认分支 tip，并重新运行受影响的验证，再发布。这些 gate 通过后，fetch
并 prune 默认 remote，再按它的 symbolic default branch 清理本地状态。只移除 HEAD 已合并到该分支
的干净、未锁定 worktree，并且只删除已合并且未被保留 worktree 使用的本地分支。绝不能强制移除或
丢弃 dirty、未合并、已锁定的 worktree，也不能丢弃任何 stash。

### macOS 14 与 Windows latest

稳定的必需检查是 fail-closed 汇总 job：`macos-14 / unit tests` 同时依赖 `macos-core`、
`macos-coverage` 与 `macos-smoke`，而 `windows-latest / unit tests` 同时依赖
`windows-native`、`windows-checks`、`windows-tests` 与 `windows-smoke`。每个汇总 job
都使用 `if: always()`，且只接受显式 `success`，因此任一 shard 失败、取消或跳过都不会变成
成功的必需检查。

macOS core shard 在 Cargo 缓存恢复后先测量真实 PTY 关闭基线，再运行源码策略检查、严格 Rustdoc、一次性 workspace 测试 gate、workspace doctest、host probe、
工具测试与真实 resource baseline 采集。独立的 coverage shard 安装固定版本的
`cargo-llvm-cov`，运行确定性 logic coverage gate，并在 coverage 步骤开始后，于成功和失败后上传证据
artifact。
`macos-smoke` 矩阵分别在 macOS 14 Apple Silicon 和 macOS 15 Intel 上构建 release
二进制，使用不同依赖缓存键。Intel lane 仅在推送到 `main` 时可保存依赖；Apple Silicon
lane 只恢复缓存。
在 release 构建之前，两个 lane 先构建并运行原生分屏选择 fixture（[原生分屏选择](Local-Gate-zh-CN#原生分屏选择)），
再运行性能场景 smoke，它检查对比工具本身而不计时（[性能场景 smoke](Local-Gate-zh-CN#性能场景-smoke)）；
任一 smoke 失败时，job 会上传其证据。release 构建之后，两个 lane 都要求原始二进制的有界 smoke 成功，
然后在相同架构主机生成并挂载 DMG。
另有带原生进程期限的独立步骤，要求原始二进制的 `frame-validation` 与 `device-recovery` 场景 smoke 成功。
安装后的 bundle 验证相对动态库依赖、签名、部署下限、拒绝 Homebrew 读取时的应用/Cairo
绘制，以及实际 bundle 字体注册；同一可执行文件的镜像对比记录压缩后字体节省量。
macOS 汇总 gate 要求两个 lane 都成功。Release job 同样在对应架构打包，最终 macOS
产物 job 只汇集已经验证的 DMG。

Windows 先通过 vcpkg 准备静态 Cairo。它先恢复 binary cache，冷 miss 时完成构建，并在三个依赖
shard 启动前立即保存结果。托管镜像或 vcpkg 版本变化后，恢复的回退归档可能不含任何 ABI
兼容的包，因此消费方仍执行 Cairo 安装，必要时进行冷构建。CI 不设置 job 或步骤的超时覆盖项。
前置的 App-only 基线构建可能在 workspace 统一 dev-dependency feature 后重新编译。
checks shard 运行 format、Clippy、源码策略、注释、脚本标识符与 Rustdoc gate；
tests shard 在 Cargo 缓存恢复后先测量真实 PTY 关闭基线，再运行一次性 workspace 测试、doctest、host probe、fail-closed GDI 呈现验证、WARP allocator、
software-selection presentation、工具测试与真实 resource baseline 采集。GDI wrapper 只接受
唯一的 `capability=EXERCISED` verdict；`HOST_INCAPABLE` 仍是信息性结果，不能满足必需 gate。
只恢复缓存的 `windows-smoke` shard 会构建发布用 release 二进制，并要求其有界原生 smoke 成功；
另有带原生进程期限的独立步骤，要求其 `frame-validation` 与 `device-recovery` 场景 smoke 成功。

同一平台及架构中使用 Rust 的 shard 共用依赖 cache key，不缓存 workspace crate artifact。
Apple Silicon core、Windows checks 和 Linux core 分别是各自 key 的唯一写入者。Intel
macOS 没有 core shard，因此其 smoke lane 是该架构的唯一写入者。所有写入都仅限推送到
`main`；其它 shard 和全部 pull-request lane 只恢复缓存。Release 构建既不恢复也不保存
Rust 缓存。这样既限制条目，也避免同一次工作流内出现重复写入者；相互重叠的 `main`
run 仍可能竞争保存同一个不可变 key。

兼容且成功的 `main` job 必须先填充 key，后续 run 才可能命中；编译器或依赖变化仍可能
导致 miss。缓存复用可减少依赖编译，不能缩短托管 runner 的排队时间。冷缓存构建以及
所有既有测试、原生和打包 gate 仍是必需的。

CI、Release 和 Wiki 发布均不设置 job 或步骤的 `timeout-minutes` 覆盖项；
GitHub Actions 平台限制仍然适用。本地与原生进程期限、输出上限及清理策略保持独立。真实 resource baseline 采集器仍把每个聚焦
PTY 命令限制为 30 秒，把 live soak 限制为 90 秒。超时会终止该命令的整个进程树，在证据包中
记录退出码 124 和部分 stdout/stderr，并继续写入校验和。

Python `*_tests.py` 入口默认使用 verbose `unittest` 输出：测试执行前立即刷新测试名称，
随后报告结果。原生依赖检查向 stderr 输出并立即刷新
`[native-dependencies] start NAME` 和 `finish NAME exit=N`。native-smoke CLI 在启动前输出
`[native-smoke] start timeout=Ns`，在 capability 验证后输出 `finish exit=N`；最终状态之前
先刷新捕获的子进程输出。安装包检查输出 `[package-check] start LABEL timeout=Ns` 和
`finish LABEL exit=N`；字体/Cairo 探针则在报告验证后以 `result=PASS` 或 `result=FAIL` 结束。
这些进度行立即刷新到 stderr，不改变 stdout 载荷、退出码或捕获的日志文件。
resource-baseline 采集器同样输出并刷新每条命令的开始/结束进度。开始行只表明正在执行，
不代表检查已通过。

### Ubuntu 22.04

稳定的 `ubuntu 22.04 / workspace, packages, X11, Wayland` 汇总 job 同时依赖
`linux-core` 与 `linux-packages`，并使用与 macOS、Windows 相同的 fail-closed 结果检查。
core shard 安装 Linux 编译依赖，并为 GPU 测试和 adapter probe 安装 Vulkan/lavapipe，
在 Cargo 缓存恢复后测量真实 PTY 关闭基线，随后运行 format、Clippy、Rustdoc（包括带 `test-util` feature 的 `sonicterm-resource`）、一次性
workspace 测试、doctest、第一方注释、脚本标识符、exit、Rust 版本、window-owner、工作流供应链、Linux package、
release-asset、release-note 与 Wiki publisher gate。

CI 和 Release 的 Ubuntu 依赖安装步骤均不设置工作流超时覆盖项；
其命令、shard 的 fail-closed 结果和 release provenance 边界保持不变。独立的
package/runtime shard 安装 Mesa Vulkan/lavapipe、
Xvfb、Weston 和 Debian 打包工具，随后：

1. 以 release 模式构建 `sonicterm-linux`；
2. 从 Cargo metadata 推导唯一 workspace 版本；
3. 生成并验证 x86_64 `.tar.gz` 与 `.deb`；
4. 验证 desktop/AppStream metadata，并以 advisory 方式运行 `lintian`；
5. 用 Vulkan/lavapipe 在 X11/Xvfb 和 Wayland/Weston 上运行两种 package layout，以独立步骤
   分别执行默认、frame-validation 和 device-recovery 场景；
6. 上传 package，失败时上传名称包含场景的 smoke log。

任何平台的默认 smoke 若没有原生窗口、渲染器/设备、实时 grid 中观察到的平台 shell PTY marker、
之后的原生 frame 呈现、默认预热渲染器的创建/报告/采用/子窗口呈现/释放并恢复进程渲染器计数，
以及 GPU 故障阶段，就不能通过：隔离故障之后仍须有一帧呈现；保留资源故障须停止所有呈现，而
重新执行的 PTY marker 仍须到达；设备销毁须记录为丢失，同时另一个 marker 须到达。每次调用都
使用分开的临时 config/log 根目录和可回收完整进程树的 wrapper；预热生命周期失败使用退出码
`16`，故障隔离失败使用 `17`，设备丢失失败使用 `18`。每个新建的 frame-validation 进程则要求
初次原生呈现、使后续呈现停止的持续故障，以及停止后新执行的 PTY marker。独立的 device-recovery
进程证明两个可见窗口和一个预热渲染器只经历一次共享设备重建、原 PTY 后续呈现新 marker、旧代次
事件被忽略，以及渲染器完成释放；失败返回 `19`。隔离场景保持禁用恢复。Linux 的三个场景矩阵
使用独立步骤及不同的状态/日志路径，每个原生进程仍有自己的期限。其它阶段成功但原生清理未完成时退出码
为 `20`；更早的失败保留原退出码。core shard 是唯一可在 `main` 写入 Linux 依赖 cache 的 job；
package shard 只恢复，且 workspace crate artifact 始终排除在 cache 外。

macOS 与 Windows smoke 还会读取原生编号标题，并在启动窗口及预热采用窗口上执行
Unicode 重命名与重置。读回不匹配会在 display 边界失败（退出码 `11`）。Linux 仍需
外部 X11 属性或 Wayland 合成器可见证据：winit 的 X11 getter 未实现，Wayland getter
只返回缓存。这些检查不验证操作系统切换器标签。

默认 Windows smoke 还安装生产 OLE 后端，要求主窗口、预热采用窗口及新建子窗口各自注册并撤销
自定义 drop target；运行后的报告必须证明三对成功操作、零存活注册和零失败，然后才取消
OLE 初始化。仅 Windows 的 COM 测试使用隐藏 HWND 和真实数据对象，验证重复所有者拒绝、
Unicode 文件交付、精确目标身份及清理；它们不合成或验证物理拖放手势。见
[平台集成](Platform-Integration-zh-CN)。

## Gate 盲区

- 一次性 workspace gate 包含全部 23 个 package 的 integration test，但仍只能运行当前 host
  能够编译与执行的 target。
- `rust-logic-coverage.sh` 只对选中的确定性代码子集要求 80% line coverage。其 ignore
  regex 完全排除 9 个 crate，包括 `sonicterm-app` 与 `sonicterm-gpu`，还排除其它 crate
  中点名的原生/控制器文件。CI 只在 macOS 上运行它，但本地 runner 在 Linux 上也会选择它。
  Coverage 通过不能证明原生窗口、真实 PTY、GPU surface、生成 FFI、installer 或 Windows-only logic。
- 同一次运行还不带 ignore regex 重新报告同一批 profile，打印每个 workspace member 的
  line coverage，并由 `scripts/coverage-floor.py` 把每个已测量 crate 与
  `scripts/coverage-baseline.json` 中的条目比较。crate 比条目低 1.0 个百分点以上或
  没有条目时，以及报告与 workspace member 和已声明 not measured 的 crate 不一致时，CI 失败：
  有条目的 crate 不再被测量、member 既未测量也未声明，或已声明的 crate 开始报告代码行。
  测试、vendored、生成与 build script 代码会被排除并打印计数；没有可计入行的 crate 显示为
  `not measured` 及其原因。该下限只拦截回归，不拦截低覆盖率：一直偏低的 crate 仍会通过。
  `sonicterm-windows` 与 `sonicterm-linux` 两行测量的是在 macOS 上编译的代码，不是这些平台
  上的执行覆盖率。baseline 绑定 macOS arm64 CI runner；其它 host 上的 CI 以不可比较失败，
  本地运行只打印参考性 delta，不给结论。下限数值只能根据已保留运行的已验证证据，通过经过评审的 diff 调整，
  见 [Coverage 证据与重新建立基线](Development-and-Release-zh-CN#coverage-证据与重新建立基线)。
- `deny.toml` 记录 advisory、license、source 与 wildcard dependency policy，但没有 CI job
  运行 `cargo deny check`。
- AppKit、Win32、X11/Wayland、字体发现、PTY、GPU 和 installer 的真实行为仍依赖平台测试、
  package smoke、release build 与手工使用；只检查 symbol 不能证明这些边界。

## 工作流供应链

`scripts/check-workflow-supply-chain.sh` 在本地及 macOS、Windows、Ubuntu core/checks
shard 强制检查 action 固定版本与 token 权限。Release 要求包含这些检查的精确成功
`main` CI run，之后才启动平台 job。

**远程 action 固定到完整小写 40 位提交 SHA**，并附发布版本 `# vX.Y.Z`。不同于 tag 或
分支，这个身份不能在本仓库没有可审阅变更时被重新指向。Checker 拒绝 tag、分支、缩写或
大写 SHA、tag 固定的 `docker://`，以及同一 action 的两个不同固定值。本地 `./` action
无需固定，因为代码在同一 PR 中审阅。

`dtolnay/rust-toolchain` 与其它远程 action 一样，固定到完整 commit SHA，并附 `# v1`
版本注释。每个调用点都显式传入 `toolchain: stable`。Action 实现不可变，但请求的 Rust
channel 仍跟随 stable。版本注释不等于可变的 tag 引用。

**`contents: write` 只存在于执行发布的那个 job。** 每个工作流都默认 `contents: read`，
只有 `release.yml` 与 `publish-wiki.yml` 的 `publish` job 在 job 作用域重新授予写权限。
第三方 action 继承的是所在 job 的 token，因此工作流级别的写授权等于把仓库写权限交给每个
job 中的每个 action——包括那些只做编译和打包的 job。在 release 上的后果很具体：上传步骤
在校验和合并之后运行，因此构建 job 中一个具备写权限的 token 可以发布与已验证集合不同的
字节。被允许的 job 及其准确可写 scope 列在
`scripts/check-workflow-supply-chain.py` 的 `WRITE_BOUNDARY` 中；新增一个是对该列表的
可审阅修改，而不是工作流里一行无人注意的改动。

Checker 只接受为 `jobs`、`steps`、`uses` 和 `permissions` 直接写出的 block mapping。
它会拒绝 flow mapping、显式 mapping key、anchor、alias 和 merge key，而不是尝试局部解释
可能隐藏可变 action 或写权限的 YAML 形式。出于同样原因，显式 YAML type tag 也会被拒绝。
带引号的标量 key 和权限值仍受支持，并会在策略检查前规范化。

Dependabot 的 `github-actions` 生态被刻意设为不过滤，这与 `cargo` 生态只允许 patch 的策略
不同。固定的 SHA 没有浮动 tag 去吸收上游修复，因此一个 Dependabot 无法推进的固定引用就是
一个会腐坏的固定引用，它压住的安全补丁永远不会到达。Dependabot 会同时改写 SHA 和它的尾注
版本号。
