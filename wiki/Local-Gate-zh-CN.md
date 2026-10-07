# 本地 gate

[English](Local-Gate)

步骤表与 `python3 scripts/local-gate.py` 调用见[开发与发布](Development-and-Release-zh-CN#本地验证-gate)。
本页说明 runner 如何执行这些步骤。

## PTY 关闭基线

`pty-close-baseline` 在每个桌面主机上显式运行标记为 ignored 的真实 PTY 测量。
本地上限保持为 1200 秒；CI 紧接 Cargo 依赖缓存恢复运行它，并包含测试二进制构建，
但不设置 job 或步骤的超时覆盖项。只有基线使用 640 秒隔离子进程观察预算和 1 MiB 完整输出上限。
输出溢出时明确失败，但仍持续排空两条管道，绝不把截断报告当作成功。普通 `isolated()` 调用
仍保持 60 秒期限、64 KiB 诊断尾部和成功静默行为。在 Linux 和 macOS 上，隔离子进程启动时
会关闭从父进程继承、编号高于 stderr 的描述符，因此不会让其他测试的捕获管道保持打开。

观察预算容纳 ordinary 与 stalled 各 20 个样本。ordinary 设置有一次 4 秒等待；Windows stalled
设置另有一次 4 秒 flood 等待。原生代码已配置的接收与重试等待合计 6.5 秒：cancel 共用一次
500 ms，reader、writer、termination retry、reap 各 500 ms，ConPTY close 与 drain 各 2 秒。
观察者的 4 秒限时与 2 秒完成余量和 close 重叠，因此取两条路径的最大值，不重复相加。
每个样本另留 2 秒 fixture 清理，启动、调度和报告共留 60 秒，得到 640 秒测量预算。
这不是生产 close 的耗时上界：原生调用、锁和 join 仍可能卡住。真正的 close 卡死仍使 harness
失败，并在总期限到达时清理子进程树；有界但未完成的样本仍可输出汇总。

ordinary 与 stalled 场景使用相同 shell、一个 pane，每场景 20 个样本。
设置、settlement 和清理仍受隔离子进程期限约束，汇总使用 nearest-rank 百分位。
每条 `PTY_CLOSE_BASELINE` 样本及 p50/p95/max 汇总区分 `close_pty_pane` 调用线程的阻塞时间，
与独立进程观察者记录的 settlement 时间，两者都从 close 开始计时。调用者是无窗口的 App
所属测试线程，不是运行中的原生事件循环。Windows 填满 ConPTY 输出队列；Unix 由 shell
session 外的测试进程持有 slave 描述符，直到观察结束。Windows 观察者保留 shell、其后代
及从外部识别的 conhost/OpenConsole 进程句柄。Unix 用 PID 和启动时间匹配身份：shell 必须
已消失（`observation=reaped`），后代可消失或成为 zombie（`observation=exited_or_zombie`）；
接管它的新父进程不回收时，zombie 条目可能保留。测试从不回收 PTY shell。
settlement 达到四秒上限后记录截尾样本 `>4000.000`，每条汇总写明 `censored` 数量并保留
百分位的下界标记；之后的清理不能把它改成成功测量。
测试不因耗时而失败，只因设置或观察失败而失败。close 本身不返回时，由隔离子进程期限终止运行并
保留最后阶段，不会声称已得到完整样本。原生场景设置成功不代表历史 teardown 卡顿已复现。

## 步骤选择与进程组

runner 选择当前主机的 `local` 步骤，并按表格顺序运行。`--with-release` 加入当前主机的
`release` 步骤，`--with-optional` 加入 `optional` 步骤，`--step ID` 只运行指定步骤，
`--list` 列出所选步骤及其超时、前置条件和 CI job。POSIX 步骤在独立进程组中运行，截止时间到达时终止
该进程组，并复用 native smoke runner 的启动与整树终止逻辑。Windows 使用下述拥有的 job；某一步
失败、超时或无法启动后，后续步骤仍会运行。在 macOS 与 Linux 上，如果步骤的进程组成员在 leader 退出两秒后仍在运行，
该步骤也会失败：runner 终止这些进程，并在步骤日志和两份 summary 中记录数量。runner 观察 leader
的退出但不回收它：Python 提供 `os.waitid` 时使用 `os.waitid` 与 `WNOWAIT`，否则（在没有
`os.waitid` 的 macOS Python 构建上）使用 kqueue 退出事件。leader 保持为未回收的僵尸进程，因此在
runner 轮询并终止该进程组期间，它的 PID（即进程组 id）不会被无关的进程组复用；此后 runner 才回收
leader。在 macOS 上，当未回收的 leader 是进程组中仅剩的成员时，终止进程组会以 EPERM 失败，而 Linux
报告成功。如果截止时间或 Ctrl-C 在 leader 退出之后到来，runner 会在步骤的 detail 中记录这次拒绝，并仍然终止或回收
leader，因此步骤会记录其结果，两份 summary 也会写入。在 macOS 与 Linux 上，如果 SIGCHLD 被忽略，runner
拒绝启动（退出码 2），因为此时内核可能在 runner 读取 leader 的退出状态或保留其进程组 id 之前回收每个
leader。当 runner 发现在步骤运行期间有其它回收者回收了 leader 时，该步骤失败，其退出状态记为不可用而从不记为 0，
runner 也不会向该 leader 的 pid 或进程组发送任何信号。在 runner 已看到 leader 退出之后、自己回收它之前回收
leader 的并发回收者不受支持：这段时间内的进程组扫描或终止可能指向一个没有任何进程保留的进程组 id。如果 POSIX
主机的 Python 两种机制都不提供，runner 会像以前一样先回收
leader，因此在这类主机上，进程组 id 可能在该进程组被终止之前被复用；这类主机也无法发现被其它回收者回收的
leader，因为此时 Popen 报告退出码 0。进程组是否为空取决于成员列表：Linux 上读取 `/proc`，其它主机
上读取 `ps`，列表不含僵尸进程；宽限期结束时仍无法读取成员列表，runner 就终止该进程组，步骤失败，残留
进程数记为未知。在 macOS 与 Linux 上，残留检查只能看到步骤的进程组：调用
`setsid` 或以其它方式离开该进程组的子进程既不会被发现，也不会被终止；如果它还把输出重定向到
步骤管道之外，runner 完全不会约束它，因为截止时间只终止该进程组。

## Windows Job Object 与准备阶段

Windows 上只有本地 gate 使用不允许 breakaway 的未命名 kill-on-close Job Object。
受信任的 bootstrap 必须先通过保留的进程句柄加入该 job，才能启动目标；分配失败或启动协议失败
会拒绝执行。等待输出 EOF 之前先查询 job 的 `ActiveProcesses`，宽限期为两秒且不超过步骤期限，
清理预算为两秒。清理只终止拥有的 job，不按进程名或重新打开的 PID 选择进程。计数查询、协议、
输出或清理错误使步骤失败；关闭 job 句柄本身不能证明已验证 job 为空。分配前 Python 启动阶段
卡住的情况仍不属于父进程崩溃时的约束保证。

Windows 策略默认为严格模式：混合测试、doctest、workspace 脚本和原生步骤存在存活后代时均失败，
其中的编译辅助进程也不例外。在独立命令中，只有 `clippy`、`doc`、`doc-resource-features`、`release-windows`、`windows-perf-build`、`perf-scenarios-counters-clippy` 与
`perf-scenarios-frame-texture-clippy`
在目标退出码为 0、捕获和协议完整、且已验证 job 为空后允许强制编译清理。`perf-compare.py` 自己的 Cargo 构建
（gate 中表外的 `PERF_BUILDS`）同样允许：MSVC 的链接器可能在 Cargo 退出后仍留下 `vctip.exe` 辅助进程。结果记为
`CLEANED_NOT_NATURAL`，不是 `PASS`。日志和 JSON 保留原始无符号目标退出码、策略、job 计数
与清理结果；文本汇总单独记录 cleaned 数量。只有 `PASS` 和允许的 `CLEANED_NOT_NATURAL`
步骤时运行退出码为 0，但只要发生清理，总 verdict 仍为 `CLEANED_NOT_NATURAL`。
混合冷构建步骤仍可能失败，不使用进程名豁免改变这一边界。

Windows 上，`pty-close-baseline` 和 `windows-warp-allocator` 先在 `--` 前插入
`--no-run`，编译原命令选中的测试。`pty-feasibility` 先构建证据示例但不运行它。
`workspace-crates` 按顺序准备固定版本 winit 的测试、文档和 workspace 测试。
每个准备阶段使用独立的自有 job，允许编译清理；之后原步骤命令不变，在新的严格模式 job 中执行。
Cargo 仍自行选择测试并提供运行环境。准备成功不证明 Cargo 会复用缓存。
严格执行阶段任何后代进程存活仍会导致失败。Doctest 不拆分，也不豁免。

显式准备记录必须与两个脚本中的每个 Cargo 调用按源码顺序一一匹配。窄范围校验器合并反斜杠续行并
统一换行形式；在任何阶段启动前拒绝不支持的 shell 布局、缺失或变更的记录、命令或环境作用域漂移。
对应校验失败表示原脚本为 `NOT_RUN`。Winit 使用调用者非空的 `CARGO_TARGET_DIR`，否则使用仓库
`target` 目录；只在其文档准备阶段覆盖 `RUSTDOCFLAGS=-D warnings -A rustdoc::invalid_html_tags`。准备输出写入步骤日志，
不会进入 feasibility 的证据／散列管道。只有规范步骤表中的对象能够授权准备阶段或独立编译清理；
相同 ID 的合成步骤不能借用这项权限。

所有阶段共享原步骤期限和可选的子进程输出字节预算，不会逐阶段重置。期限过后仍保留既有的有界清理。
普通非零准备退出码使整体结果保持 `FAIL`，但只要预算仍足够，且 job 为空、bootstrap 已回收、
协议与捕获完整并且没有错误，仍继续其余准备阶段和原命令。进程约束、启动、协议或捕获失败会停止
该步骤；中断或超时也会停止，未启动的阶段保留为 `NOT_RUN`。日志及文本／JSON 汇总分别显示每个
阶段实际 argv、环境覆盖、退出码和进程约束结果；步骤退出码仍是原执行命令的退出码，若未执行则
不可用。只有全部准备阶段成功且原严格执行自然通过时，准备阶段清理才能产生被接受的整体
`CLEANED_NOT_NATURAL`。即使原执行随后通过，普通准备失败仍使整体失败。

## 输出、日志与 Git 状态

本地 gate 保留 DEVNULL 输入、argv、工作目录、环境变量和既有颜色设置。一条合并输出管道将原始
字节持续写入磁盘，不在内存保留完整输出。没有显式上限时记录完整输出；有显式上限时保留前缀、
持续排空超出部分，并因溢出而失败。控制台仍只显示步骤进度和日志尾部。
`native-smoke-runner.py` 及 CI/Release 的直接调用保留既有行为，本地进程约束策略不适用于这些调用。
Windows 进程约束回归测试通过 `local-gate_tests.py` 运行，该测试组包含清理在内的预算为 60 秒；
完整本地 supply-chain 步骤仍保留 120 秒预算。

每步日志、`summary.txt` 与 `summary.json` 写入新的临时目录或 `--log-dir`，后者不能是仓库根目录或
其祖先目录（退出码 2）；任一步骤失败时退出码非零。步骤日志保留每个步骤输出的原始字节；控制台无法编码的
文本（例如 cp1252 Windows 控制台上的中日韩文字）会以转义形式输出，而不会中止运行。由于 summary 在最后
一次快照之后写入，runner
还会在任何步骤运行之前拒绝 runner 自有的输出路径（步骤日志、`summary.txt` 或 `summary.json`），
只要该路径已被跟踪（按不区分大小写比较）、是符号链接或是硬链接（退出码 2）。runner 以新文件的形式创建每个
步骤日志与 summary：该文件在日志目录中以独占方式创建，再重命名到输出路径上，因此在那里发现的符号链接或
硬链接会被替换，而不会经由它写入其指向的内容；读取日志尾部时也不跟随符号链接。运行期间出现在输出路径上的
符号链接或硬链接，或者运行期间被替换的日志目录，都会使运行失败，并给出指明该路径的消息。runner
发现日志目录被替换时，会停止启动步骤：其余步骤不会运行，也不会写入任何 summary。runner 在运行前后
记录已跟踪与未跟踪的 Git 状态，包括每个路径的类型与权限位：运行前已有的改动报告为既有改动，
运行期间产生的改动会使 gate 失败，runner 从不清理工作树。只有 runner 自己的未跟踪日志与 summary
不参与这项比较。

## 超时与 CI 一致性

每个本地步骤都有独立于 CI 超时策略的显式期限；没有 CI job 运行的步骤使用远高于实测耗时的上限。
因此，慢速机器或冷构建可能让本会通过的步骤报告 `TIMEOUT`；构建预热后，请用 `--step ID` 重新运行该步骤。

`ci.yml` 保留显式步骤以显示逐步进度，但不设置 job 或步骤的超时覆盖项；GitHub Actions 平台限制仍然适用。
表格校验命令与 job 的对应关系，不校验超时一致性，也不生成工作流。
`scripts/local-gate_tests.py` 通过 `check-workflow-supply-chain.sh` 在 `macos-core`、
`windows-checks` 与 `linux-core` 中运行。以下情况会使它失败：表格命令没有出现在它所列的 CI job 中；
`ci.yml` 步骤运行了 `scripts/` gate 或 `cargo fmt|clippy|doc|test` 命令，但它既不是表格步骤，
也不在附带理由的仅 CI 列表中；[开发与发布](Development-and-Release-zh-CN#本地验证-gate)页面、其英文页面或 `CLAUDE.md` 的 gate 块与
`python3 scripts/local-gate.py --render zh-CN` 或 `--render en` 的输出不一致。`ci.yml` 读取器按本仓库的
workflow 布局建模，遇到任何未建模的 `run:` 写法都会报错，使一致性检查明确失败，而不是跳过某个步骤。
workflow 顶层或 job 中的 `defaults:` 键同样会报错，无论块形式还是流形式，因为继承的 `run` 默认值
（`working-directory`、`shell`）作用于每个 run 步骤，而读取器不对其建模；任何不是普通 `key:` 行的
顶层行也会报错。步骤的 `shell:` 必须是普通的 `bash` 或 `pwsh`：其它 shell、自定义模板，或者带引号、
流形式、块形式或跨行续写的写法都会报错，与 `working-directory:` 相同。分类命令之前，它会规范化 `cargo +toolchain`、带引号的脚本路径和 `scripts\` 分隔符；
它检查以 `&&` 或 `;` 连接的每条命令以及 `run:` 块的每一行，并报告它无法证明的 gate，例如位于管道、
`||`、包装命令或命令替换中的 gate。它还会报告出现在决定运行内容的任何位置上的 `${{ }}` workflow
表达式：命令词、cargo 子命令（包括位于 `+toolchain` 或开头的 cargo 选项之后的子命令）、解释器的
脚本参数（包括位于解释器选项之后的脚本参数），以及第一方脚本 `--` 分隔符之后的命令。普通数据参数
中的表达式仍受支持。分类器不分析 shell 退出状态：检查复合行或块中的每条命令，并不能证明失败会被
传递。`;` 之前或块中较早一行的失败是否使步骤失败，取决于 shell 自身的错误处理，例如 `bash -e` 或
PowerShell 的最后退出码，而一致性检查不对此建模。一致性检查只比较命令文本，因此既不对 job 或 workflow 的 `env:` 建模（例如会改变
未改动 gate 的行为的 `BASH_ENV` 或 `RUSTFLAGS`），也不对在运行时提供 gate 词或脚本路径的 shell 展开建模，例如
`cargo $SUB`、`bash "$SCRIPT"` 或 `cargo $(echo test)`；workflow 的修改与其它修改一样经过审查。每个仅 CI 条目都附带理由：依赖安装；
对同一 job 的 workspace 步骤已运行的
integration test 做证据重跑；或需要托管 runner、release 二进制或已构建 package 的运行时与 package
证据。只在 CI 中运行的第一方测试或 `cargo fmt|clippy|doc` 不能列为仅 CI，因此缺失的本地测试或
gate 会使一致性检查失败。

## 步骤说明

必须单独运行 `doc-resource-features` 步骤：`test-util` 是 workspace 唯一的
optional feature，而 `cargo doc` 不构建 dev-dependency。`sonicterm-logging` 以
dev-dependency 使用 `test-util`，因此 workspace Clippy 和测试已经编译它。字体栈没有可选
vendor feature：St.Helens 是普通的已跟踪资源，其它回退字体来自原生平台发现。
`check-workspace-crates.sh` 先运行原生源码验证器的单元测试、离线完整性检查和可跨平台运行的
macOS bundle 测试，再对默认
feature 运行一次 fail-complete 的 `cargo test --workspace --lib --bins --tests --no-fail-fast`；
即使前一阶段失败，后续阶段仍会执行。它覆盖全部
workspace library、binary 和 integration-test target，且不会再用逐 package 串行循环重复执行
unit 与 binary target。它的固定 winit 阶段沿用调用方设置的 `CARGO_TARGET_DIR`。该命令不编译
doctest。`doctests` 步骤编译并运行普通 doctest，只编译不运行 `no_run` 示例，并跳过 `ignore` 示例。
在各 Cargo 阶段之前，它还对固定 winit 中自行编写的 Windows `keyboard_tests.rs` 运行 `rustfmt --check`：
保留的依赖不参与 workspace 格式化，但其自行编写的测试仍需检查格式。
它的固定 winit 文档阶段拒绝除 `rustdoc::invalid_html_tags` 之外的所有 rustdoc 警告：该 crate 的
文档注释是上游原文，离线完整性检查逐字节固定这些内容；Rust 1.99 起的 rustdoc 把
`KeyCode::NumpadMultiply` 上的 `<kbd>*</kbd>` 列表读作嵌套不当的 Markdown 强调。

第一方注释 checker 要求有效公开函数和公开 trait 函数带用途 Rustdoc，公开 unsafe 函数带
`# Safety`，并检查准确锚定的 `// When:`、`// SAFETY:`、`// Lock order:`、
`// Ordering:` 和 `// Lifecycle:` 契约。`check-no-raw-process-exit.sh` 要求发布代码通过
`sonicterm_logging::exit_with` 退出。`check-workflow-supply-chain.sh` 强制执行
[工作流供应链](CI-and-Coverage-zh-CN#工作流供应链)所述的工作流契约；它会先运行自己的解析器测试，
因此一次静默停止匹配的扫描不会被当成通过的 gate。它还会运行 local-gate runner 与一致性测试，
以及原生选择 smoke 与性能对比脚本的测试。

`windows-warp-allocator` 步骤是 Windows 上会阻断 release 的确定性 allocator 测试。它要求
DX12 WARP adapter 和 allocator report。生产策略 reserved bytes 必须低于 64 MiB，最大 block
低于 128 MiB，且生产策略 reserved bytes 低于旧默认 control。Windows CI 测试 shard 显式运行它，Release 只接受包含该 shard 的精确成功
`main` CI 运行。只有 Windows CI 能可靠编译并运行
`#![cfg(target_os = "windows")]` 测试；在 macOS 上，这类文件可能编译成零个测试。

可选的 `windows-target` 步骤是 macOS 上的 pre-push 辅助检查，绝不是 CI gate。
`scripts/check-windows-target.sh` 在某个 workspace 成员不属于它的两个列表中任何一个时失败，
随后对不需要 Windows C 工具链的 13 个成员运行
`cargo clippy --locked --target x86_64-pc-windows-msvc --all-targets -- -D warnings`：
`sonicterm-types`、`-grid`、`-vt`、`-cfg`、`-logging`、`-resource`、`-text`、`-ui`、
`-app-core`、`-io`（包括其 ConPTY 代码与 Windows-gated 测试）、`-render-model`、
`-block-glyph` 与 `-font-config`。检查范围是默认 feature、全部 target，以及该 target 专属的
dev 与 build 依赖闭包，并使用独立的 target 目录。它还对固定版本的 winit 以 `serde` 运行
`cargo check --locked`，使其 Windows 键盘测试得到编译；缺少该 target 时，它会给出
`rustup target add x86_64-pc-windows-msvc` 提示并失败。它不检查其余十个成员：
`sonicterm-freetype` 与 `sonicterm-harfbuzz` 运行原生 C/C++ 构建，`sonicterm-fontconfig`
通过 pkg-config 发现系统库，`sonicterm-font`、`-engine`、`-gpu`、`-app`、`-mac`、
`-windows` 与 `-linux` 需要尚未验证的原生字体与 Cairo 依赖闭包。Cairo 是系统依赖而非
vendored 源码：它的 pkg-config 探测会拒绝交叉编译 target；绕过后，第一个原生阻塞点是
`sonicterm-freetype` 中 vendored zlib 所需的 Windows CRT 头文件。该检查只做编译与 lint；
Windows 代码仍只在 Windows CI 中运行。CI 不运行该步骤，而是运行一项静态的分类完整性检查：
`scripts/local-gate_tests.py` 在 `macos-core`、`windows-checks` 与 `linux-core` 中比对该脚本的两个
crate 列表与 workspace 成员，而不运行该脚本，因此每个新 crate 都必须归类。

Windows 的 `windows_font_weight_present` 测试在设置、渲染、捕获、每次字重操作和缓存
检查之间返回原生消息循环。除每个缩放比例的首次渲染外，每个阶段都检查窗口仍能响应。首次渲染
在构建该缩放比例的字体、字形图集和 GPU 管线时不处理窗口消息，因此在较慢的 runner 上，即使
仍在工作，也可能触发 `IsHungAppWindow` 的 5 秒规则。单个阶段运行超过 60 秒或整次运行超过
240 秒时，watchdog 线程会终止测试进程，因为不返回的阶段也会使事件循环线程上的检查停止。
出错和完成时都释放 renderer，并验证存活 renderer 数量恢复到基线。缺失重绘会在测试的 180 秒
截止时间到达时失败。原生 GDI 像素比较仍是必要条件，包括通过 `SONICTERM_FONT_PROBE_DIR`
开启密集读回和图像记录时。

Release 准备还要构建发布平台二进制：`python3 scripts/local-gate.py --with-release` 会加入当前主机的
`release` 步骤。

## 原生分屏选择

`windows_native_split_selection` 随 Windows 工作区集成测试运行。它创建原生窗口和
renderer，再把进程内合成的指针事件送入生产 App 处理路径。主窗口和子窗口覆盖左右、上下和
嵌套分屏，断言检查选区所属窗格、完整复制文本、终端鼠标报告、Shift 选择和帧计数前进。
复制使用内存剪贴板，不使用 PTY 或系统剪贴板。这不是物理拖动手势或像素读回证据。

Fixture 在两次呈现之间把控制权交还原生事件循环。每次原生重绘把实际窗口 id 映射到
fixture 的 App 条目，只尝试一次生产绘制。本地按下和释放阶段各自要求完成一帧；物理指针
和键盘输入被忽略。阶段与超时记录包含两个窗口 id、原生/App 重绘次数、完成帧数，以及
最后观察到的原生遮挡事件。没有事件不能证明窗口可见，`is_visible` 也不是原生遮挡状态。
缺少重绘路由时报告失败；已尝试绘制但首帧始终未完成时报告 `BLOCKED`，不推断原因；
后续阶段超时则报告失败。这些结果都不能满足原生验收。

macOS 通过进程主线程上的 example 执行同一个 fixture。普通工作区测试和覆盖率不会运行
这个 example，因此 macOS 本地 gate 显式构建并运行它。两个必需的 `macos-smoke` CI
矩阵分支在 release 构建与打包之前执行相同命令，不设置 CI job 或步骤的超时覆盖项。本地 example 构建
仍保留 25 分钟上限；选择测试的运行时上限独立设置且保持不变。macOS fixture 将
`new_events` 和 `about_to_wait` 转发给 App，在用例截止时间之外保留 App 更早的期限或
轮询请求；延迟重试由 App 负责，而不是由 fixture 循环请求重绘。该 fixture 禁用无关的
预热窗口池。Windows 保留原有回调和重试路径。

每个 macOS 用例先成功呈现一帧，再通过既有 renderer 测试入口注入一次后端遮挡的获取结果。
它必须观察到一次未呈现尝试，以及随后完成的一帧。从注入到恢复帧之间，任何窗口尺寸、缩放
或遮挡事件都会使该用例失败，且不重新注入：这些事件可能绕过按期限驱动的恢复。这个首帧之后
的控制不能复现零帧的启动失败；若启动失败再次出现，仍然阻止验收。

```sh
cargo build --locked -p sonicterm-app --example native_split_selection
python3 scripts/native-selection-smoke.py
```

校验器选择 Metal，开启 renderer 在 stderr 上的适配器记录，移除继承的 `NO_COLOR`，
并保留 `HOME`。它把 Python 选定的操作系统临时目录根路径作为 `TMPDIR` 传入，使 Rust
使用相同根路径。校验器从仓库根目录启动
`cargo run --locked -p sonicterm-app --example native_split_selection -- --run <fixture>`，
而不是猜测可执行文件路径。Cargo 解析 `CARGO_TARGET_DIR`、`CARGO_BUILD_TARGET_DIR`、
`build.target-dir` 和配置的 target，并在执行前检查构建是否需要更新。找不到 Cargo、
构建或配置错误、超时都会使 gate 失败，不会退回默认目录中的旧程序。Fixture 使用操作系统
临时目录下一个尚不存在的子目录，分别隔离配置和日志，不假定 `RUNNER_TEMP` 就是原生
临时目录。进程 watchdog 仍为 180 秒，每个窗口/分屏布局用例的截止时间仍为 20 秒。
校验器直接复用本地 gate 的进程组启动器，对 Cargo 和 example（包括必要的重新构建）
设置 190 秒上限，保留未回收 leader 的所有权，并在退出后检查残留进程。应先运行独立的
构建步骤，让冷编译使用自己的预算。本地 gate 已说明的 `setsid` 逃离限制同样适用。

通过要求退出码为 0，每个主窗口/子窗口布局各有唯一 PASS，并有唯一最终 PASS；每个用例
还必须记录 Metal、非 CPU 设备类型和 `software_rendering=false` 的适配器选择结果。
每个用例还必须先记录唯一的 `PASS native surface retry`，其中 `baseline_frames` 为正数，
`resumed_frames` 更大，且 `recovery_events=0`；缺失、重复、格式错误或顺序错误的恢复证据
都会失败。缺失或重复用例、`NOT_EXERCISED`、`BLOCKED`、panic、清理警告、残留 fixture 目录或
进程组成员都会使 gate 失败。启动器最多保留 8 MiB 子进程输出，超限后继续排空管道并报告
失败，不接受截断结果。证据保存在输出所示的操作系统临时目录中；CI 失败时上传该目录。
只保留必要证据，然后清理目录。Windows 通过不能替代 macOS 执行，直接调用 example
但不传 `--run` 也不能算验收。

## 性能场景 smoke

`perf-scenarios-tests` 在每个主机上，以及在 `macos-core`、`windows-tests-harness` 与 `linux-core` 中，运行
harness 自身的单元测试 `cargo test --locked -p sonicterm-app --example perf_scenarios`，因为
`workspace-crates` 运行的 `cargo test --workspace --lib --bins --tests` 不包含 example。
`perf-scenarios-counters-tests` 在相同的 job 中以 `--features perf-counters` 运行同一组测试；
`perf-scenarios-counters-clippy` 在运行 `clippy` 的每个 job（`macos-core`、`windows-checks` 与
`linux-core`）中带该 feature 检查此 example，因此计数器代码在每个主机上都会被构建、测试和检查。
`perf-scenarios-frame-texture-tests` 与 `perf-scenarios-frame-texture-clippy` 以 `--features perf-frame-texture`
做同样的事，从而编译进 harness 的帧纹理读取。在 Windows 上，这些 feature 测试改在
`windows-tests-harness-features` shard 中与 glyph 工作集测量一起运行，因此 CI 在每个平台上对每个 feature 的测试
恰好运行一次，而不是放在普通测试所在的 job 中。

`macos-perf-smoke` 检查的是对比工具本身，而不是性能。它运行
`python3 scripts/perf-compare.py --smoke`：以 debug 构建当前树的 `perf_scenarios` example，不使用
base ref、worktree 或 release 构建，并以 harness 的 `--short` 运行三个简短用例，每个用例都使用新进程
和自己的 scratch 目录，并以仓库根目录为工作目录，App 在那里找到已跟踪的字体：

1. S1；
2. S3；
3. S1，会话一启动就像到达截止时间的运行那样被终止。

使用 `--short` 时，每段保持只持续 5 秒，每个场景结尾的空闲期至少持续到负载开始后 5 秒而不是 60 秒，
S3 则输出 `head -n 200000` 与一个 5 MB 文件。前两个用例在结果符合结果 schema、焦点按下文的规则是安全的、
清理后没有残留进程时通过。App 报告配置的主字体加载失败时，smoke 立即失败。被终止的用例只有在 `run_step`
于脚本发出 SIGKILL 之后自己回收了 harness（状态为 FAIL、退出码为 `-9`、没有残留的进程组成员），且清理
完成、没有进程残留时才通过。脚本只在 harness 仍具有被接受时记录的 pid 与启动时间时，才发送该信号。发出
信号之后的任何其它结果都会使 smoke 失败，`run_step` 从未收集到的 harness 退出属于未解决的清理。每个用例
还会在前后为 `~/.sonicterm` 做快照，
并用哨兵文件标记开始；其中出现新增、修改或删除的文件会使 smoke 失败。例外是属于另一个 SonicTerm 实例
的改动：以其它进程命名的 breadcrumb 文件，以及另一个实例运行期间按天日志的增长或日志的删除。
`.DS_Store` 会被忽略；harness 在启动时记录自己的 scratch 路径，因此误写的日志能被识别出来
（[隔离检查](Development-and-Release-zh-CN#隔离检查)）。该检查也经由目标覆盖那里的符号链接，因此经由
链接的写入也算改动；无法读取目标或达到遍历上限时，检查无法完成，smoke 失败。smoke 不断言任何
耗时数值；只有在空闲主机上的对比才测量速度或内存。

| 退出码 | 结果 | 条件 |
| --- | --- | --- |
| 0 | 通过 | 每个用例都按上述规则通过 |
| 1 | 失败 | 既不有效、也不是遮挡、也不是 `BLOCKED` 的用例，或资源无法解析的源码树；立即失败，不重试 |
| 3 | `BLOCKED` | 没有得到有效且实际执行的运行 |

既不有效、也不是遮挡（在上限内重试）、也不是 `BLOCKED` 的用例会使 smoke 立即失败（见
`scripts/perf-compare.py` 中的 `smoke_verdict`）。`classify_outcome` 先按
[开发与发布](Development-and-Release-zh-CN#对比的执行过程)给出的顺序判断停止原因：未解决的清理、schema
失败与拒绝运行。带有其中之一的用例即使同时有遮挡，也会使 smoke 失败。以退出码 1 结束的原因包括：

- schema、焦点安全或隔离失败；
- 会话记录问题；
- 清理未解决：有残留进程、会话成员没有有效的锚进程、进程组成员比 harness 存活得更久或无法计数，或
  `run_step` 的截止时间或 Ctrl-C（此时 `run_step` 终止并回收 harness，但不对其进程组计数）；
- `run_step` 从未收集到的 harness 退出；
- `finish_session` 未完成，无论以哪种退出码结束，包括遮挡：它在遮挡检查之前判定，因此不重试。只有截止
  时间用例的计划内终止会跳过这项检查与 schema 检查；
- 截止时间用例中，`run_step` 没有在脚本发出 SIGKILL 之后自己回收 harness；
- 遮挡以外的 harness 无效判定，例如某个检查点的 `.done` 始终没有出现；
- harness 超时（退出码 4）；
- 拒绝运行（退出码 2）；
- 主字体加载失败；
- 无法完成的 home 检查；
- harness 以退出码 0 结束，但 `run_step` 报告 PASS 以外的状态；
- harness 意外退出；
- 资源无法解析的源码树，在任何用例运行之前发现。

只有遮挡会被重试，每个用例最多重试 3 次；某个用例没有得到有效且实际执行的运行时，smoke
报告 `BLOCKED`。本地 gate 只接受退出码 0，因此 `BLOCKED` 会使该步骤失败。场景在 macOS 与 Windows 上运行；在
Linux 上 harness 输出 `NOT_EXERCISED`，因此该步骤在 macOS 上运行，并以 `windows-perf-smoke` 在 Windows
上运行（[Windows](#windows)）。

smoke 判断焦点的方式与对比相同：另一个应用在前台时 harness 成为前台应用即为抢占，前台应用采样失败会使该
用例失败。唯一的例外是 GitHub 托管的 runner（`GITHUB_ACTIONS=true` 且
`RUNNER_ENVIRONMENT=github-hosted`），那里没有用户持有焦点，因此这次激活只被记录，不被判为抢占，与该类 runner 上的对比相同。日志会
记下它，该用例的 `outcome.json` 也会把它保存在 `focus_notes` 中；采样失败仍会使 smoke 失败。在自托管
runner 上，或缺少这两个值中的任何一个时，smoke 保持完整的规则。在没有前台应用的主机上，harness 成为活动
应用从不算抢占。smoke 的日志会列出一次它采用的规则，以及它读取的 runner 变量。

主显示器（harness 打开窗口的显示器）必须显示桌面 Space，而不是全屏应用：若那里有全屏应用，harness
窗口会打开在被隐藏的桌面 Space 上，不呈现任何帧。主窗口打开后 10 秒内没有呈现任何帧时，harness 把该次
运行判为无效并结束（退出码 3）。原因会说明 10 秒内没有帧呈现，因此该次运行被视为疑似遮挡，可能是其
显示器上的全屏应用所致；没有帧并不能证明发生了遮挡。smoke 把它作为遮挡重试，没有得到有效运行时报告
`BLOCKED`。

本地预算为 45 分钟：沿用选择构建 25 分钟的冷构建额度，再为最多 12 次 harness 运行各留 100 秒，因为三个
用例每个最多重试 3 次。这 100 秒是每次运行的 `run_step` 截止时间。在 smoke 之外，该截止时间比 harness
自己的截止时间晚 30 秒，正好在 harness 的 watchdog 将要中止 harness 的时刻或之前。smoke 把它限制为
100 秒，因此对于在 `--short` 下截止时间为 80 秒的 S1 与 S3，它比该截止时间晚 20 秒。两个必需的
`macos-smoke` CI 矩阵分支在原生分屏选择之后、release 构建之前运行相同命令，不设置 CI job 或步骤的超时
覆盖项。该步骤一旦加上 `if:` 或 `continue-on-error:`，或移出这一位置，CI 一致性检查就会失败。

smoke 在 CI 中失败时，job 会上传其证据。Windows smoke 在交付回放经过重试后通过时，同样保留其证据，
把 `SONICTERM_PERF_REPLAY_RETRIED=1` 追加到 `$GITHUB_ENV`，job 会上传它。`perf-compare.py --smoke` 把
`SONICTERM_PERF_EVIDENCE_DIR=<dir>` 追加到 `$GITHUB_ENV`，该目录包含每个用例的 `result.json`、
`outcome.json` 与日志、会话记录、`front-samples.log`，以及 `cleanup.json` 与 `home-check.json` 中的
清理与 home 检查结论。smoke 对每种不同的 `lsappinfo` 采样形式只打印一次。

每个用例的证据还包含它的 `progress.json`，其中是该次运行到当时为止完成的每项测量，以 `result.json` 的
形状放在顶层：各阶段、会输入文字的场景的延迟报告（样本、已归因数与总数，以及覆盖率）、吞吐量、取消遮挡
时间、保留的回滚行数，以及检查点。此外它还包含 schema 版本、harness 哈希与状态 `running`。两个文件的
测量键来自同一个序列化器（`perf_scenarios/record.rs` 中的 `Measurements` 与 `write_progress`）。harness
在 Startup 之后、每个阶段之后以及每个完成的检查点之后，把该文件写入自己的 scratch 目录，因此被
`run_step` 超时或 harness 的 watchdog 终止的运行仍能显示它已测得的内容。`progress.json` 只是证据；
`result.json` 仍是唯一的结果。

每次写入都发生在阶段之间、测量窗口之外，但并非没有代价。它只缩短共享的结尾空闲阶段：该阶段在 GO 之后
60 秒结束（使用 `--short` 时为 5 秒），并在上一阶段的写入之后开始；S2、S6 与 S10 的每个变体，以及 S7、
S8 与 S9，都以它结尾。其它每个计时区间都在写入之后开始，或在某个事件发生时结束。各测一次时，带有 S2 的
200 个延迟样本（48 KB，测于样本带有拆分字段之前；拆分会使写入略大）的一次写入约需 2.4 毫秒，没有样本时为 0.1 到 0.5 毫秒。写入还会推迟其后的工作，
并可能影响缓存与后台 I/O。对比的两侧运行同一个 harness，因此两侧都承担这一开销；两侧使用同一个 ref 的
A/A 对比也在测量中包含它。

`scripts/perf-compare_tests.py` 测试该脚本，包括上述失败规则；`check-workflow-supply-chain.sh` 在
macOS、Windows 与 Linux 上运行它。如何运行和阅读对比见[开发与发布](Development-and-Release-zh-CN#性能对比)。

### Windows

`windows-perf-smoke` 在 Windows 上运行 `python scripts/perf-compare.py --smoke`。只编译的
`windows-perf-build` 步骤先运行，构建同一个 debug example
（`cargo build --locked -p sonicterm-app --example perf_scenarios`），因此 smoke 自己的构建会发现它已是最新。
编译器留下仍在运行的辅助进程时，在该步骤或 smoke 自己的构建中清理，后者同样是只编译步骤
（[Windows Job Object 与准备阶段](#windows-job-object-与准备阶段)）。

在运行用例之前，Windows smoke 先通过 ConPTY 回放 S10 的 `sync` 变体：harness 的 `--capture-delivery`
模式在 250x70 的伪控制台中启动该场景的程序，不打开窗口，并写出 `delivery.json`。回放遵循对比的重试规则
（[开发与发布](Development-and-Release-zh-CN)）：只有从未找到的帧标记会被重试，最多 3 次尝试。回放仍有检查
未通过，或结束时没有与其退出码一致的记录，smoke 报告 `BLOCKED`，其原因写明每次尝试。回放的清理未解决时
（例如 job 的托管未经验证），smoke 在运行任何用例之前失败。每次尝试的记录、交付文本与日志都保存在证据目录中；有尝试被重试后通过时，该目录会保留并上传。

Windows smoke 运行上述三个用例，再加两个：

4. S1 `wgpu`，关闭软件呈现器：该次运行必须通过 wgpu 呈现且不降级，无法做到的运行为 `BLOCKED`；
5. S1 `role-exit`，其角色程序在 GO 之后立即以 1 退出：只有当该次运行以无效结束、且原因指出程序退出的
   pane 时才通过，以有效结束则 smoke 失败。

每次运行都在自己的 Windows Job Object 中执行，嵌套在 gate 的 job 之下。截止时间用例通过的条件是：
`run_step` 自己结束了 harness，状态为 FAIL、退出码 124，且该 job 的托管记录显示它已清空；其它用例在
harness 退出后 job 中仍有存活成员时失败。通过的 smoke 会删除其证据（交付回放经过重试时除外），因此每次尝试还会打印一行
`members:`，列出清理前 job 的成员：pid、映像名，以及原始 FILETIME 形式的创建时间，最多 16 个，
其后注明还有多少个。

焦点依据前台窗口判断，而不是 `lsappinfo`。第一个位于前台的应用是基线，之后前台进程的任何变化都会使
该次运行无效。在 GitHub 托管的 runner 上，没有用户会话持有焦点，变化只被记录在 `outcome.json` 的
`foreground_changes` 中。在整个运行期间，harness 还用 `LockSetForegroundWindow` 锁定前台切换，因此其窗口
打开时不会获得焦点；按下 Alt 或点击其它窗口会结束锁定，锁定失败时记录在结果的 `notes` 中。

本地预算为 70 分钟（4200 秒）：25 分钟的冷构建余量（1500 秒），五个 Windows 用例每个最多 4 次、每次 100 秒
的运行（2000 秒），以及 S10/sync 交付回放最多 3 次、每次 100 秒的尝试（300 秒），最坏情况共 3800 秒，另留
400 秒余量。必需的 `windows-tests-runtime` CI job 在 "Verify Windows selection presentation" 之后先运行构建、
再运行 smoke，smoke 失败或在交付回放重试后通过时上传证据目录。任一步骤加上 `if:` 或 `continue-on-error:`，或 smoke 排在构建
之前时，CI 一致性检查失败。托管的 Windows runner 使用软件适配器渲染，因此在那里 smoke 检查结果 schema、
回收、wgpu 呈现器与角色退出，从不检查计时。

## 经过评审的块字形栅格

`sonicterm-block-glyph` 在 `crates/sonicterm-block-glyph/raster-digests.golden.tsv`
中保存经过评审的栅格摘要表。每一行记录一个 codepoint、单元格宽度、高度与下划线粗细、
alpha 总和、墨迹包围盒，以及对 tile 尺寸和逐行 alpha（renderer 唯一保留的通道）计算的
FNV-1a 64 摘要。该表包含 37 个 codepoint，`from_char` 映射的每个 block key 类别至少有
一个。测试要求为这些 codepoint 与六种 case 尺寸的全部 222 个组合提供经过评审的行：5×9/1、
8×16/1、15×31/2、16×32/2、30×40/2 和 45×60/3（单元格宽 × 高 / 下划线，单位为 texel）。
`raster_digests_match_reviewed_table` 要求在每个 host 上都完全一致，不允许任何容差；
对每个不一致的行，它会打印新旧值和重新生成命令。

Spinner 片段在窄小单元格中仍可栅格化。内部清除圆退化时不会产生路径，也不会清除任何像素；
外圆填充和其余扇区清除路径仍继续执行。这不会改变非空路径，也不会改变孔半径为正时的栅格。
Spinner 回归测试包含 1×1 单元格，以及 `min(宽, 高) = 6 × 下划线` 边界两侧的两种方向。
极小单元格中的亚像素扇区可以完全透明，但 tile 仍必须具有请求的尺寸和存储长度。

只有在明确命名的几何变更时才重新生成该表：

```sh
SONICTERM_BLESS_BLOCK_GLYPH=1 cargo test -p sonicterm-block-glyph raster_digests
cargo test -p sonicterm-block-glyph
```

bless 运行会重写该表、以十六进制打印每个变化的栅格，并且总是失败，因此只有随后的普通
重新运行才能通过。评审者比较每个变化行的新旧 alpha 总和与包围盒。摘要变化必须对应一个
明确命名的几何变更并附上栅格，否则就是回归。

摘要不是唯一的判据。`customglyph_tests.rs` 不依赖存储数据，在 8×16/1 与 16×32/2 两种
尺寸下栅格化每个已映射 codepoint，要求尺寸符合请求并有可见墨迹，只有 U+2800 允许空白；
它还固定实心块不透明度、阴影级别、线条居中与粗细、框线连接、盲文点位置以及 Powerline
覆盖面积。如果不同 host 在抗锯齿 texel 上不一致，请报告逐 texel
差异；由维护者选择有文档记录的容差或按平台分开的表。
