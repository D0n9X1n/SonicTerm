# Architecture Internals / 架构内幕

Engineering detail behind the architecture: how accounting claims are verified,
which rendering invariants are load-bearing, where the native boundaries sit,
and what the release gate actually checks.

架构背后的工程细节：记账声明如何被验证、哪些渲染不变量是承重的、
原生边界位于何处，以及发布闸门实际检查了什么。

For the shape of the system, start with [Architecture](Architecture). For what
each crate does, see [Crate Reference](Crate-Reference). Memory accounting and
the resource governor live in [Memory](Memory).

## English

### Accounting verification

Accounting claims are verified against **real heap**, not against the number the
figure was derived from.

Every accounting defect found during this work shared one shape: a reported
figure measured against its own derivation, each with a test that passed. A grid
figure under-reported by 1.67×, a queued-output figure restated a constant, and
the hyperlink registry under-reported by 4.8×.

The ground truth is a counting `#[global_allocator]` that tracks live bytes
across allocate, deallocate, and reallocate. These tests assert both directions:

- a reported figure does not **understate** real heap — an undercount admits
  allocation past the cap;
- and does not wildly **overstate** it — an overcount refuses work while memory
  is available;
- and that real heap, not merely the reported figure, stops below the cap.

With tables uncounted, the hyperlink registry stopped at 8,388,244 bytes against
a cap of 8,388,608 — compliant on its own number — while actually holding
roughly 12.1 MB.

Two constraints are structural rather than stylistic:

- **`#[global_allocator]` is crate-wide**, so these must live in `tests/` as
  integration tests. They cannot be flat sibling unit tests.
- **The counting allocator is process-global**, so every test in such a file must
  serialize on a file-local `Mutex`. Two tests measuring concurrently attribute
  each other's allocations to whichever one is reading. Measured: all pass
  serially and all fail in parallel, reporting a 5.80× "undercount" that was
  entirely sibling noise. The lock is used rather than `--test-threads=1`,
  because the gate cannot be told to serialize one file — and a suite that only
  works under a flag is a suite that will eventually run without it.

Allocation-measuring tests must also build their fixture data **before** the
measurement window. A `format!` per iteration inside the window attributes the
harness's own garbage to the subject under test.

### Rendering and redraw invariants

SonicTerm retains rendered pixels between frames, so damage calculation is part
of terminal correctness rather than a paint optimization.

- A dirty alternate-screen pane repaints its complete surface-clipped pane.
  Primary-screen panes retain narrow dirty-row damage.
- VT/grid mutations mark affected rows in the same frame — including scrolling,
  insert/delete line, reverse index, erase, resize, and wide-cell repair.
- Grid geometry budgets include retained row allocation, not only visible
  `cols × rows`. Material column shrink compacts surviving rows while adjacent
  resize oscillation retains reusable capacity, and history-limit reductions
  release excess `VecDeque` capacity.
- Clipboard serialization preserves isolated or incomplete right-edge
  box-drawing text, and removes only a coherent multi-row side ending in a
  lower-right frame corner.
- CAN/SUB cancellation resets VT escape accounting before cancelled DCS media
  can reach `unhook` and emit an incomplete image.
- Windows software rendering keeps the established full-surface presenter path;
  it is not coupled to retained GPU damage decisions.
- **Pane VT workers never call native window APIs.** After output coalescing
  they copy the pane's `WindowId` under a short mutex guard and send
  `UserEvent::RequestRedraw`; the winit event-loop thread resolves the live
  window and calls `request_redraw()` after the guard has been released.
- Tear-out and tab transfer update the pane's redraw target by `WindowId`, so a
  worker survives migration without retaining an `Arc<Window>` or calling
  AppKit/Win32 from the worker thread.

### Font and native boundaries

Font discovery, shaping, and rasterization stay split from renderer policy.
Generated FreeType/HarfBuzz/Fontconfig bindings stay in their wrapper crates;
`sonicterm-font` owns safe allocation and fallback behaviour.

Variable-font metadata is optional: malformed, missing, or out-of-range
variation metadata falls back to base OS/2 default weight and width rather than
aborting the app. Embedded bitmap strikes are loaded metrics-only and checked
against the glyph allocation budget before FreeType may decode their pixels.

Atlas behaviour is deliberately lazy:

- Glyph and image atlas textures initialize through dirty-tile uploads.
- Same-dimension atlas resets clear metadata and packing state in place, without
  zeroing or replacing the retained CPU pixel allocation. Cached UV generations
  are invalidated before newly inserted tiles can overwrite sampled rectangles.
- The inline-image atlas starts as a 1×1 CPU/GPU placeholder and promotes to its
  bounded full size only when a renderable image first appears.
- On Windows, deterministic software presentation keeps the full CPU glyph atlas
  but replaces GPU atlas textures with 1×1 placeholders. Returning to GPU
  presentation recreates matching textures, resets atlas metadata and UV-bearing
  caches, and forces a full redraw before the new textures can be sampled.

The hidden warm-renderer pool defaults to one on every adapter. A configured
value of zero disables it; hardware honours values up to five, while software
rendering caps every nonzero target at one.

PTY handles own their native reader and writer threads. Unix natural exit is
observed with `waitid(..., WNOWAIT)`; teardown repeatedly terminates every
process in the unreaped leader's session before reaping, so session identity
cannot be reused first. Windows teardown caches process exit and keeps a
dedicated cloned output reader draining concurrently with ConPTY master close.
The Unix and Windows implementations both use bounded thread, close, and
child-exit deadlines.

Terminal-input enqueue is non-blocking and bounded. Saturation, disconnection,
and oversized messages return typed errors that **retain the rejected bytes**
instead of reporting false success; callers forward those bytes to the event
loop for a visible retry notification.

Native GPU presentation, real PTYs/SSH, AppKit/Win32/X11/Wayland handles,
generated C ABI behaviour, and installer signing are verified by build,
integration, platform CI, and release smoke checks rather than hollow unit
tests.

### Release and verification boundary

The workspace version in root `Cargo.toml` is authoritative for all first-party
crates and internal requirements. Releases are created only by pushing an
owner-approved `v*` tag whose version matches every workspace package. The tag
workflow builds two macOS DMGs, one Windows MSI, and Linux x86_64 `.deb` and
`.tar.gz` packages. Each package is registered in a typed fragment; publication
requires all five tuples, revalidates hashes, rejects unregistered release-like
files, and emits `release-assets.json`, deterministic `SHA256SUMS.txt`, and an
exact upload-path list.

Packaging procedure is documented in [Packaging](Packaging); the release
sequence and CI layout are in
[Development and Release](Development-and-Release).

## 中文

### 记账验证

记账声明是针对**真实堆内存**验证的，而不是针对该数字自身的推导过程验证。

本轮工作中发现的每一个记账缺陷都有同一种形态：一个上报数字仅与它自己的推导方式
互相印证，而且各自都有一个通过的测试。网格数字少报了 1.67 倍，排队输出数字只是
复述了一个常量，超链接注册表少报了 4.8 倍。

基准事实来自一个计数型 `#[global_allocator]`，它在分配、释放、重分配全过程中
跟踪存活字节。这些测试同时断言两个方向：

- 上报数字不得**低估**真实堆内存——少计会让分配越过上限；
- 也不得严重**高估**——多计会在内存尚可用时拒绝工作；
- 并且真正停在上限之下的必须是真实堆内存，而不仅仅是上报数字。

在表结构未被计入时，超链接注册表停在 8,388,244 字节、上限为 8,388,608——
按它自己的数字看完全合规——而实际持有约 12.1 MB。

有两条约束是结构性的，而非风格问题：

- **`#[global_allocator]` 作用于整个 crate**，因此这类测试必须作为集成测试放在
  `tests/` 中，不能写成平级的同名单元测试文件。
- **计数分配器是进程全局的**，因此这类文件中的每个测试都必须在文件级 `Mutex`
  上串行化。两个同时测量的测试会把彼此的分配算到正在读取的那一个头上。
  实测：串行时全部通过，并行时全部失败，并报出 5.80 倍的「少计」，
  而那完全是同文件测试造成的噪声。这里使用锁而不是 `--test-threads=1`，
  是因为无法只让闸门对某一个文件串行——而一套只有加了标志才正确的测试，
  终将在没有该标志的情况下运行。

测量分配的测试还必须在测量窗口**之前**构建其夹具数据。
在窗口内每轮迭代调用一次 `format!`，会把测试框架自身产生的垃圾算到被测对象头上。

### 渲染与重绘不变量

SonicTerm 会在帧之间保留已渲染的像素，因此损伤区域（damage）计算属于终端正确性的
一部分，而不只是绘制层面的优化。

- 处于备用屏幕且被标脏的窗格，会重绘其完整的、经表面裁剪的窗格区域；
  主屏幕窗格则保留窄粒度的脏行损伤。
- VT/网格的修改会在同一帧内标记受影响的行——包括滚动、插入/删除行、反向索引、
  擦除、调整大小以及宽字符单元修复。
- 网格几何预算包含保留的行分配，而不仅是可见的 `cols × rows`。
  实质性的列收缩会压紧存活行，而相邻的反复缩放会保留可复用容量，
  历史行数上限下调则会释放多余的 `VecDeque` 容量。
- 剪贴板序列化会保留孤立或不完整的右边缘制表符文本，
  仅移除以右下角框线结尾的、完整连贯的多行边框。
- CAN/SUB 取消会在被取消的 DCS 媒体到达 `unhook` 并输出不完整图像之前，
  重置 VT 转义序列记账。
- Windows 软件渲染沿用既有的整表面呈现路径；它不与保留式 GPU 损伤决策耦合。
- **窗格的 VT 工作线程从不调用原生窗口 API。** 在合并输出之后，
  它们在短暂的互斥保护下复制窗格的 `WindowId` 并发送 `UserEvent::RequestRedraw`；
  由 winit 事件循环线程解析出存活窗口，并在释放保护之后调用 `request_redraw()`。
- 拆出（tear-out）与标签页转移会按 `WindowId` 更新窗格的重绘目标，
  因此工作线程无需持有 `Arc<Window>`、也无需从工作线程调用 AppKit/Win32
  即可在迁移后继续存活。

### 字体与原生边界

字体发现、整形与光栅化始终与渲染器策略分离。生成的
FreeType/HarfBuzz/Fontconfig 绑定保留在各自的包装 crate 中；
`sonicterm-font` 负责安全的分配与回退行为。

可变字体元数据是可选的：格式错误、缺失或超出范围的变体元数据会回退到
基础 OS/2 默认字重与字宽，而不是让应用中止。内嵌位图 strike 仅按度量信息加载，
并在 FreeType 解码其像素之前先对照字形分配预算进行检查。

图集（atlas）的行为是刻意惰性的：

- 字形与图像图集纹理通过脏瓦片上传来初始化。
- 同尺寸的图集重置会就地清除元数据与打包状态，
  而不会清零或替换已保留的 CPU 像素分配。缓存的 UV 代次会在新插入的瓦片
  覆盖已被采样的矩形之前失效。
- 内联图像图集以 1×1 的 CPU/GPU 占位符起步，
  仅在首次出现可渲染图像时才提升到其受限的完整尺寸。
- 在 Windows 上，确定性软件呈现会保留完整的 CPU 字形图集，
  但将 GPU 图集纹理替换为 1×1 占位符。回到 GPU 呈现时会重建匹配的纹理、
  重置图集元数据与携带 UV 的缓存，并在新纹理可被采样之前强制一次完整重绘。

隐藏的预热渲染器池在所有适配器上默认为 1。配置为 0 表示禁用；
硬件渲染最多接受 5，而软件渲染会把任何非零目标都限制为 1。

PTY 句柄拥有各自的原生读写线程。Unix 上的自然退出通过
`waitid(..., WNOWAIT)` 观察；拆除时会在回收之前反复终止未回收 leader
所在会话中的每一个进程，因此会话标识不可能被抢先复用。
Windows 上的拆除会缓存进程退出状态，并保持一个专用的克隆输出读取器
与 ConPTY 主端关闭并发地持续排空。Unix 与 Windows 实现都使用有上限的线程、
关闭与子进程退出期限。

终端输入的入队是非阻塞且有界的。饱和、断开与超大消息会返回带类型的错误，
并**保留被拒绝的字节**，而不是谎报成功；调用方会把这些字节转发给事件循环，
以便给出可见的重试提示。

原生 GPU 呈现、真实 PTY/SSH、AppKit/Win32/X11/Wayland 句柄、生成的 C ABI
行为以及安装包签名，都由构建、集成、平台 CI 与发布冒烟检查来验证，而不是靠空洞的
单元测试。

### 发布与验证边界

根 `Cargo.toml` 中的 workspace 版本对所有第一方 crate 与内部依赖要求具有权威性。
发布只能通过推送经所有者批准、且版本与全部 workspace package 一致的 `v*` 标签来创建。
该标签工作流会构建两个 macOS DMG、一个 Windows MSI，以及 Linux x86_64 `.deb`
与 `.tar.gz`。每个平台 package 都登记到类型化 fragment；发布要求五个 tuple 全部存在、
重新验证 hash、拒绝未登记的 release-like 文件，并生成 `release-assets.json`、确定性的
`SHA256SUMS.txt` 与精确 upload-path list。

打包步骤见 [Packaging](Packaging)；发布流程与 CI 布局见
[Development and Release](Development-and-Release)。
