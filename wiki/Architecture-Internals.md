# Architecture Internals / 架构内部机制

## English

This page owns the checks and invariants that keep the architecture honest. See
[Architecture](Architecture) for system shape, [Runtime Lifecycle](Runtime-Lifecycle)
for ownership changes, and [Memory](Memory) for the full resource inventory.

### Heap-truth accounting

A retained-memory report is tested against live heap, not against the formula
that produced the report. The relevant integration tests install a counting
`#[global_allocator]` and observe allocation, deallocation, and reallocation.
They check three properties:

- the reported value does not materially understate live heap;
- the reported value does not materially overstate live heap;
- live heap itself stops within the enforced cap and stated tolerance.

These tests must stay in `tests/`. A `#[global_allocator]` applies to the whole
crate, so a sibling unit-test module cannot isolate it.

The allocator is process-global. Every test in one measurement binary holds a
file-local `Mutex` for its full measurement lifetime. The test builds fixture
strings and buffers before opening the measurement window. Otherwise sibling
work or test-harness allocations become part of the subject's result.

The heap-truth checks cover the grid, hyperlink registry, PTY queues, VT capture
staging, inline media, owner close, and long-lived atlas. Important enforced
limits include:

| Seam | Current limit |
| --- | --- |
| Grid storage | `MAX_GRID_CELLS = 1,048,576`; visible geometry is capped by `MAX_VISIBLE_GRID_CELLS = 524,288` |
| Hyperlink registry | `MAX_HYPERLINKS = 16,384`, `MAX_HYPERLINK_URI_BYTES = 8 KiB`, `MAX_HYPERLINK_CLIENT_ID_BYTES = 1 KiB`, `MAX_HYPERLINK_METADATA_BYTES = 8 MiB` |
| VT media payload | `MAX_MEDIA_PAYLOAD_BYTES = 16 MiB` |
| Process VT capture staging | `MAX_PROCESS_CAPTURE_STAGING_BYTES = 64 MiB`, with a `MIN_CAPTURE_STAGING_BYTES = 4 MiB` floor and `GUARANTEED_CONCURRENT_CAPTURES = 13` |
| PTY input | 4 queued messages; each message is at most 16 MiB |
| PTY output | 64 queued chunks plus one blocked sender chunk; each retained reader ring is 64 KiB; structural worst case is 65 rings, or 4.0625 MiB |
| Retained inline media | 128 images and 64 MiB per pane; 256 MiB process target plus at most one 4 MiB newest-image residual per live pane before reclamation converges; each rendered side is at most 1,024 pixels |

A grid report includes cell storage, rare attributes, combining text, row
containers, and reserved capacity. Scrollback is limited by configured rows and
retained bytes. The scroll path checks the byte budget in amortized batches.

The hyperlink registry counts its URI strings and both hash tables. `clear`
shrinks those tables. The registry may reclaim unreferenced entries when full;
it does not sweep the grid for every OSC 8 link.

The PTY output report counts ring allocations pinned by queued `Bytes` views.
It does not multiply the slot count by an assumed chunk size, and it does not
mistake payload length for retained allocation.

### Resource ledger invariants

The GUI creates this live owner tree:

```text
Process
  Window
    AppPane
```

Process and window owners use tracking-only limits. Each `AppPane` owner uses
`PANE_COMMITTED_BUDGET_BYTES`. That value is twice
`PANE_SEAM_CAP_SUM_BYTES`, which is calculated from the grid, inline-media,
hyperlink, parser-capture, PTY-output, and PTY-input seam caps.

The seam caps remain the primary enforcement points. The pane budget is a
backstop. Retention is measured before it is charged, so a failed charge does
not undo memory already retained. A failed growth keeps the previous charge. A
failed new charge leaves that class absent and writes a `memory` debug record.

A pane owns one `CommittedReservation` per charged `ResourceClass`. A charge is
resized in place with `try_grow` or `shrink`; it is not released and recreated
between samples. Owner reattribution passes the complete charge set to
`CommittedReservation::transfer_batch`. Every token must share one ledger and
source owner; the batch preserves classes and validates all source balances,
target states, and target limits under one ordered lock set before mutation.
Success changes owner-path accounting and token owner ids without changing
process or per-class totals. Failure leaves every token and ledger shard at the
source.

Close order is load-bearing:

1. clear pane charges;
2. close each `AppPane` owner;
3. close the parent `Window` owner.

`PaneState` declares `charges` before `owner`. `WindowState` declares `panes`
before its window `owner`. Rust drops fields in declaration order, so the normal
drop path follows the same leaf-first rule. `finish_close` refuses an owner with
live charges or children. `OwnerGuard::drop` logs a warning and retains a refused
record; it does not retry.

A failed window-owner registration leaves the window usable but omits that
window and its panes from hierarchy accounting for the rest of the window's
life. A failed pane-owner registration leaves the pane usable; periodic
reconciliation can retry it. Renderer-owned surfaces, glyph atlases, row glyph
and quad caches, and software frames are measured outside this ledger. The row
cache classes are explicit `UnchargedRetention`: reports carry exact current
allocation while the coverage table records conservative per-renderer high-water
envelopes. No report invents GPU memory or presents an uncharged class as a
governor reservation.

### Rendering correctness invariants

SonicTerm retains rendered pixels between frames. Damage therefore decides
correctness, not only speed.

- A primary-screen pane contributes the union of its dirty-row strips. The strips
  include pane padding and are clipped to the pane and surface.
- A dirty alternate-screen pane contributes its complete surface-clipped pane.
  A clean alternate-screen pane contributes no damage.
- Changes to terminal cells mark affected rows in the same frame. This includes
  scrolling, reverse index, line insertion/deletion, erase, resize, and
  wide-cell repair.
- Each `Line` packs an incoming automatic-wrap bit into its existing content
  sequence word. Only an actual margin wrap sets it. Hard line advances,
  full-row erases, recycled rows, non-reflow resize, and uncertain region
  surgery clear affected boundaries. The bit travels into scrollback and enters
  row equality/hash identity, so an evicted predecessor remains detectable
  without increasing the row header.
- Local-target reconstruction joins at most eight visible rows and 4 KiB only
  across those recorded boundaries. Its authorization key binds every row
  fingerprint and wrap bit, ordered absolute spans, pointed cell, viewport,
  screen epoch, scrollback-eviction generation, and exact-pane OSC 7 state.
  Hard newlines, incomplete chains, unsafe cells, and any identity change fail
  closed before activation-time native revalidation.
- Changes to overlays or window chrome promote damage to the full surface.
  One hovered target carries up to eight ordered viewport fragments in the
  frame key. Active recoloring salts only each intersecting row cache key;
  underline geometry emits one clipped quad per fragment.
  Effective per-pane scrollbar opacity is window chrome: its sorted, quantized
  identity participates in the frame key, and a bucket change damages the full
  surface.
- A degraded wgpu frame with work repaints the full surface. Windows degraded
  presentation also composes a full CPU surface. Degraded scrollbars snap and
  arm one idle-hide deadline; accelerated scrollbars request bounded fade frames.
- `RenderMode::Noop` is available only under resolved degradation when no visible
  signal changed. It does not present or clear dirty rows.
- Windows software glyph presentation stabilizes NDC roundoff at integer and
  half-pixel origins before one-to-one raster placement. The row glyph cache also
  keys the viewport row slot because cached instances carry screen coordinates.
- Every authored quad color is finite premultiplied linear RGBA. Opacity and
  coverage changes scale RGB and alpha together; debug builds validate both quad
  layers immediately before the GPU/software presenter split. Windows software
  quads decode retained sRGB destination channels, blend the same source in
  linear light, encode RGB once, and source-over alpha as linear UNORM.
- Windows LCD text is eligible only for an opaque configured backdrop, effective
  opacity `1`, and either the CPU/GDI presenter or a wgpu device with
  `DUAL_SOURCE_BLENDING`. The effective `off`/`rgb`/`bgr` mode enters the frame
  key. GPU and CPU presenters apply the foreground transform before coverage,
  attenuate destination RGB independently in linear light, and use maximum
  channel weight for alpha. Images and color glyphs have higher branch priority.
- A display-scale transition commits one physical inner size through winit's
  event-scoped writer. That size preserves logical geometry where it fits, stays
  above the 30×10 terminal minimum, and is capped by the destination monitor work
  area; renderer surface, pane grids/PTYs, IME geometry, and redraw follow the
  same target before Windows applies `WM_DPICHANGED`.

The event-loop thread collects a complete frame without waiting on the VT
worker. It uses `try_lock` for every active-tab parser and for required
inline-image stores. If any lock is unavailable, it drops all collected guards,
records a pending redraw, and does not call `GpuRenderer::render`.

Dirty rows clear only in `finish_successful_frame`:

- on Windows CPU presentation, after `SetDIBitsToDevice` returns success;
- on wgpu presentation, after command submission and `queue.present(frame)` are
  invoked.

`SetDIBitsToDevice` can report failure. wgpu's present call has no result that
reports a later presentation failure. Surface timeout, occlusion, outdated,
suboptimal, and lost results invalidate the frame key and request another
redraw. Outdated and suboptimal surfaces are reconfigured. A lost surface is
recreated. Validation errors propagate. None of these acquisition failures
clears dirty rows.

Grid geometry accounts for retained row allocations, not only visible
`cols × rows`. A material column shrink compacts rows. Adjacent resize changes
keep reusable capacity to avoid repeated allocation. Reducing the scrollback
limit releases excess `VecDeque` capacity.

Clipboard serialization keeps isolated or incomplete right-edge box drawing.
It removes only a coherent multi-row side that ends in a lower-right frame
corner. On Windows, a successful OSC 52 write gets one delayed reassertion only
when the clipboard has reverted to the exact text observed before that write;
a newer or unreadable clipboard owner is never overwritten.

Nested applications under rmux/tmux emit OSC 52 through `DCS tmux` passthrough.
Trusted sessions require both options; `set-clipboard` alone does not relay the
wrapper:

```tmux
set -s set-clipboard on
set -g allow-passthrough on
```

Both options trust pane output. Arbitrary DCS passthrough remains outside
SonicTerm's OSC 52 parser; the multiplexer must validate and unwrap it.

CAN and SUB cancel an active escape sequence. The parser resets escape
accounting before a cancelled DCS or APC media sequence can emit a partial
image. A host-cancelled stalled capture discards the remaining payload until its
terminating boundary instead of printing it into the grid.

### Atlas and font invariants

The CPU glyph atlas is fixed at 2,048 × 2,048 BGRA8 pixels, about 16 MiB. Its
metadata holds at most `MAX_ATLAS_ENTRIES = 16,384` entries, including blank and
missing sentinels.

On a miss, the atlas uses reclaimed rectangles before its shelf packer. Under
metadata or packing pressure, it deterministically evicts the coldest quarter.
An eviction changes the atlas epoch. If that happens during frame assembly, the
renderer discards the frame, resets the atlas in place, invalidates UV-bearing
row caches, and requests a new frame. The retry disables eviction until one
frame presents successfully. The fixed pixel allocation does not grow.

`RowGlyphCache` and `LineQuadCache` use keys based on pane id, absolute row, and
row hash. Their capacities are about four times the sum of visible rows across
all panes. A capacity or geometry-size change clears the affected cache. Dirty
rows invalidate their absolute-row entries. Font, theme, scale, surface resize,
and atlas replacement invalidate the corresponding caches. Retention counts the
hash table's allocated key/entry buckets and every nested vector's capacity.
Ordinary clearing leaves table capacity reusable, so bounded churn forms a
high-water envelope rather than a flat byte line. When a pane leaves a renderer,
one event-loop-owned operation removes its glyph-cache entries first and its
quad-cache entries second, preserves peer hits, then asks each table to shrink.
No concurrent retention snapshot can observe only half that ordered eviction.

The inline-image atlas starts as a 1 × 1 CPU/GPU placeholder. It promotes to a
2,048 × 2,048 atlas when renderable media appears. After 240 rendered frames
without inline media, it returns to the placeholder. Text and image atlases are
separate so image pressure cannot evict text glyphs.

On Windows degraded presentation, the full CPU atlases remain available while
GPU atlas textures become 1 × 1 placeholders. Returning to wgpu presentation
recreates matching textures, resets atlas state, invalidates UV-bearing caches,
and forces a full redraw before sampling the new textures.

DirectWrite subpixel tiles remain linear BGRA coverage in both the CPU atlas and
the GPU unorm coverage view. They must never pass through the color-rectangle
conversion or the sRGB color view. Alpha remains the maximum RGB coverage so
ineligible and `off` presentation has a deterministic grayscale value. Changing
LCD mode is presentation-only and must not rebuild or reinterpret either atlas.

Font discovery, shaping, and rasterization stay separate from renderer policy.
Generated FFI bindings remain in their wrapper crates. Malformed, missing, or
out-of-range variable-font metadata falls back to base OS/2 weight and width.
FreeType embedded bitmap strikes are checked against the 2,048-pixel and 16 MiB
glyph allocation limits before pixel decoding.

The hidden warm-window pool defaults to one. Zero disables it. Normal hardware
accepts at most five. An actual software adapter or resolved degradation caps
any nonzero target at one. A live config reload clears the pool; later
`about_to_wait` passes rebuild it one entry at a time.

### PTY and native-thread invariants

Terminal input enqueue is non-blocking. `PtyHandle::send_input_nonblocking`
uses `try_send` on a four-message channel. A message over 16 MiB, a full queue,
or a disconnected writer returns `PtyInputError` with the original bytes. The
app posts `UserEvent::PtyInputRejected`, logs the reason, and shows an error
notification. It does not replay the bytes automatically.

The PTY reader uses a reusable 64 KiB `BytesMut` allocation. It sends
`PtyOutputChunk` views through a 64-slot channel. A full channel blocks the
reader and lets the operating system apply back-pressure; output is not dropped.

A pane VT worker holds only that pane's parser lock while advancing VT state and
snapshotting parser-owned modes. It moves the owned VT events out of that guard,
then handles clipboard and command events, decodes or resizes inline media, and
updates retained stores after unlocking. Main-born and child-born panes use the
same host-event processor. Tear-out changes the shared redraw `WindowId`, so the
worker follows the pane without retaining `Arc<Window>`.

`PtyHandle::drop` always starts with cancellation, synchronous-I/O cancellation
where supported, and child termination. The remaining order differs by platform.

On Unix, `waitid(P_PID, ..., WEXITED | WNOHANG | WNOWAIT)` observes natural
exit without releasing the session id. Teardown kills the original process group
and repeatedly kills active members of the same session. It closes the master
before waiting for I/O threads. Reader and writer each get 500 ms. Termination
retry and child reap each use a separate 500 ms deadline. If session cleanup
cannot be proved, the leader remains unreaped so its id cannot be reused
unsafely.

On Windows, teardown waits up to 500 ms for the reader and another 500 ms for
the writer before master close. `sonic-conpty-drain` drains a cloned reader while
`sonic-conpty-close` closes the master. Close gets 2 seconds. If close succeeds,
drain gets another 2 seconds. Timeouts detach the helpers. Helper-start or close
failure returns an incomplete-close result and warns. Child exit/reap has a
separate 500 ms bound.

These deadlines keep `Drop` from blocking the UI indefinitely. They do not turn
an incomplete native close into success.

### Release verification boundary

Root `Cargo.toml` `[workspace.package]` is the version source. The release
workflow starts for tags matching `v[0-9]+.[0-9]+.[0-9]+*`. It first peels the
tag ref to its commit, then requires that commit to be in `origin/main` history
and an exact completed successful `CI` push run to exist for it. It continues
only when
`prepare-release-assets.py check-version` parses the tag as a semantic version,
finds that version on every workspace package, and the normal source-consistency
gates pass.

The workflow builds five required package tuples:

| Platform | Architecture | Package |
| --- | --- | --- |
| macOS | `aarch64` | `.dmg` |
| macOS | `x86_64` | `.dmg` |
| Windows | `x86_64` | `.msi` |
| Linux | `x86_64` | `.deb` |
| Linux | `x86_64` | `.tar.gz` |

Each package has a typed JSON fragment. Consolidation requires all five tuples,
rejects duplicate names or tuples, recalculates hashes, and rejects unregistered
`.dmg`, `.msi`, `.deb`, and `.tar.gz` files in `dist`. It emits
`release-assets.json`, deterministic `SHA256SUMS.txt`, and
`release-upload-paths.txt`. The release action uploads only the paths in that
list.

The Windows CI test shard runs:

```bash
cargo test -p sonicterm-gpu --test windows_warp_allocator_baseline -- --nocapture
```

The gate requires WARP and allocator reporting. Production reserved bytes must
be below 64 MiB. The largest block must be below 128 MiB. The
`MemoryHints::MemoryUsage` candidate must reserve fewer bytes than the
`MemoryHints::Performance` control under the same allocations. Release requires
an exact successful `main` CI run for the tag commit before `build-windows`
starts, so a failed gate blocks the MSI and publication without being rerun at
tag time.

Linux package verification builds both `.deb` and `.tar.gz` layouts. The runtime
smoke runs them on X11/Xvfb and Wayland/Weston with Vulkan/lavapipe. It requires
window creation, GPU initialization, a `/bin/sh` PTY marker round trip, and a
later native presentation.

macOS packaging verifies binary architecture and the app's ad-hoc signature.
The workflow does not perform Developer ID signing, notarization, or a packaged
DMG launch smoke. The Windows workflow does not sign or install-run the MSI.
Installer signing is therefore not a verified release invariant.

Release validation requires an exact completed successful `main` CI run for the
tag commit, then validates every workspace package version and the release-asset
tooling. It does not repeat the source, unit, integration, documentation,
platform-runtime, allocator, coverage, or normal-CI package gates. The platform
jobs start directly from that provenance boundary and perform only native
release builds, package validation, MSI metadata validation, and Linux package
smokes. Release Rust target builds do not read or write Rust caches; the Windows
job may restore the vcpkg binary cache published immediately by normal CI.

### Source and check map

| Contract | Primary source or check |
| --- | --- |
| Heap-truth tests | `crates/sonicterm-{grid,io,vt,app,resource,text}/tests/` |
| Resource inventory and baseline | `scripts/test-resource-inventory.sh`, `scripts/test-resource-baseline-evidence.sh` |
| Damage and present completion | `crates/sonicterm-gpu/src/core.rs` |
| Glyph atlas and row caches | `crates/sonicterm-text/src/{glyph_atlas,row_glyph_cache}.rs`, `crates/sonicterm-gpu/src/row_quad_cache.rs` |
| PTY teardown | `crates/sonicterm-io/src/pty.rs` |
| Owner and charge ordering | `crates/sonicterm-app/src/app/{mod,retention}.rs` |
| Release asset contract | `scripts/prepare-release-assets.py`, `scripts/test-release-assets.sh` |
| Release job graph | `.github/workflows/release.yml` |

## 中文

本页集中说明验证架构真实性的检查和关键不变量。系统结构见
[架构](Architecture)，所有权变化见 [运行时生命周期](Runtime-Lifecycle)，完整资源清单见
[内存](Memory)。

### 以真实堆内存验证记账

常驻内存报告必须与实际存活的堆内存比较，不能只验证生成报告的公式。对应的集成测试会安装
计数型 `#[global_allocator]`，记录分配、释放和重新分配。测试同时检查三件事：

- 报告值不能明显低于实际存活堆内存；
- 报告值不能明显高于实际存活堆内存；
- 实际堆内存必须停在已实施的上限与声明的容差内。

这类测试必须放在 `tests/` 中。`#[global_allocator]` 作用于整个 crate，同级单元测试模块
无法把它隔离开。

分配器是进程全局状态。同一个测量测试二进制中的每个测试，都要在整个测量期间持有文件内
`Mutex`。夹具字符串和缓冲区要在测量窗口开始前创建。否则，并发测试或测试框架本身的分配
会被算进被测对象。

真实堆内存检查覆盖网格、超链接注册表、PTY 队列、VT 捕获暂存、内联媒体、所有者关闭和
长寿命图集。主要上限如下：

| 接缝 | 当前上限 |
| --- | --- |
| 网格存储 | `MAX_GRID_CELLS = 1,048,576`；可见几何受 `MAX_VISIBLE_GRID_CELLS = 524,288` 限制 |
| 超链接注册表 | `MAX_HYPERLINKS = 16,384`、`MAX_HYPERLINK_URI_BYTES = 8 KiB`、`MAX_HYPERLINK_CLIENT_ID_BYTES = 1 KiB`、`MAX_HYPERLINK_METADATA_BYTES = 8 MiB` |
| VT 媒体负载 | `MAX_MEDIA_PAYLOAD_BYTES = 16 MiB` |
| 进程级 VT 捕获暂存 | `MAX_PROCESS_CAPTURE_STAGING_BYTES = 64 MiB`，保底值 `MIN_CAPTURE_STAGING_BYTES = 4 MiB`，`GUARANTEED_CONCURRENT_CAPTURES = 13` |
| PTY 输入 | 最多排队 4 条消息；每条最多 16 MiB |
| PTY 输出 | 最多 64 个排队数据块，另有一个阻塞中的发送数据块；每个读缓冲环为 64 KiB；结构最坏值为 65 个缓冲环，即 4.0625 MiB |
| 常驻内联媒体 | 每窗格最多 128 张图和 64 MiB；进程目标为 256 MiB，回收收敛前每个存活窗格最多另保留一份 4 MiB 最新图像余量；参与渲染的图像单边最多 1,024 像素 |

网格报告包含单元格、少见属性、组合文字、行容器和预留容量。回滚历史同时受配置行数和
常驻字节数限制。滚动路径按批次摊销检查字节预算。

超链接注册表会统计 URI 字符串和两张哈希表。`clear` 会收缩这些表。注册表满时可以清理
未被引用的条目，但不会在每条 OSC 8 超链接到来时遍历整个网格。

PTY 输出报告统计被排队 `Bytes` 视图占住的读缓冲环。它不会用槽位数乘一个假定的数据块
大小，也不会把负载长度误当成常驻分配量。

### 资源总账不变量

图形界面建立的实际所有者树如下：

```text
Process
  Window
    AppPane
```

进程和窗口所有者只跟踪数据，不设置有限额度。每个 `AppPane` 所有者使用
`PANE_COMMITTED_BUDGET_BYTES`。该值等于 `PANE_SEAM_CAP_SUM_BYTES` 的两倍，后者由网格、
内联媒体、超链接、解析器捕获、PTY 输出和 PTY 输入接缝上限计算得出。

各接缝上限仍是主要限制点。窗格预算只是总账警戒线。代码先保留内存，再测量并计费，因此
计费失败不会撤销已经保留的内存。增长失败时保留原计费值。新类别计费失败时，该类别保持
缺失，并写一条 `memory` debug 记录。

窗格为每个已计费的 `ResourceClass` 持有一个 `CommittedReservation`。计费通过
`try_grow` 或 `shrink` 原地调整，不会在两次采样之间先释放再重新创建。重新归属所有者时，
代码把完整计费集合交给 `CommittedReservation::transfer_batch`。每个令牌必须属于同一账本和
同一源所有者；批次会保留分类，并在修改前用一套有序锁完整验证源余额、目标状态和目标上限。
成功时只修改所有者路径记账和令牌所有者 id，不改变进程总量或各分类总量；失败时全部令牌和
账本分片都精确保留在源端。

关闭顺序不能改变：

1. 清空窗格计费；
2. 关闭每个 `AppPane` 所有者；
3. 关闭父级 `Window` 所有者。

`PaneState` 把 `charges` 声明在 `owner` 前面。`WindowState` 把 `panes` 声明在窗口
`owner` 前面。Rust 按声明顺序析构字段，因此普通析构路径也遵循先叶子、后父级的规则。
`finish_close` 会拒绝仍有计费或子节点的所有者。`OwnerGuard::drop` 遇到拒绝时会记录
warning 并保留该记录，不会重试。

窗口所有者注册失败时，窗口仍可使用，但该窗口及其窗格在剩余寿命内都不会进入层级记账。
窗格所有者注册失败时，窗格仍可使用；周期协调可以再次尝试。渲染器持有的表面、字形图集、
逐行字形与 quad 缓存及软件帧不进入这份总账，另行测量。两个行缓存类明确标记为
`UnchargedRetention`：报告给出当前精确分配，覆盖表记录保守的每渲染器高水位包络。
报告不会虚构 GPU 内存，也不会把未收费类写成 governor 预留。

### 渲染正确性不变量

SonicTerm 会跨帧保留已经画好的像素。因此，损伤区域决定画面是否正确，不只是性能优化。

- 主屏幕窗格贡献所有脏行条带的并集。条带包含窗格内边距，并裁剪到窗格和表面。
- 备用屏幕窗格只要有脏行，就贡献整个经表面裁剪的窗格。没有脏行时不贡献损伤区域。
- 终端单元格变化会在同一帧标记受影响的行，包括滚动、反向索引、插入或删除行、擦除、
  调整大小和宽字符修复。
- 每个 `Line` 会把“由前一行自动软换行而来”的 bit 打包进现有内容序号 word。只有真实的
  右边界自动换行会设置它；硬换行、整行擦除、行复用、不做 reflow 的 resize，以及无法证明
  连续性的区域调整会清除相关边界。该 bit 会随行进入 scrollback，并参与行相等性与 hash，
  因此前驱被淘汰后仍可检测，同时不会增大行 header。
- 本地目标只会跨这些已记录边界重建最多 8 个可见行和 4 KiB。授权 key 绑定每个行
  fingerprint 与 wrap bit、有序绝对 span、鼠标绝对 cell、viewport、screen epoch、scrollback
  淘汰代次和准确 pane 的 OSC 7 状态。硬换行、不完整链、不安全 cell 或任一身份变化都会在
  激活时原生重新验证之前 fail closed。
- 界面浮层或窗口装饰变化会把损伤区域扩大到整个表面。一个 hover 目标最多携带 8 个有序
  viewport 片段进入帧键。活动变色只给相交行的缓存 key 加 salt；下划线几何为每个片段发射
  一个经过裁剪的 quad。每个窗格的有效滚动条透明度也属于窗口装饰：按窗格编号排序并量化后
  的身份会进入帧键，桶值变化会损伤整个表面。
- 已降级的 wgpu 帧只要有工作，就重画整个表面。Windows 降级呈现也会合成完整 CPU 表面。
  降级滚动条直接跳变并只设置一个空闲隐藏截止时间；加速滚动条请求有限的淡入淡出帧。
- 只有最终降级状态启用且没有可见信号变化时，才能使用 `RenderMode::Noop`。该路径不呈现，
  也不清除脏行。
- Windows 软件字形呈现会在一对一光栅定位前，稳定 NDC 反算在整数与半像素原点附近的误差。
  行字形缓存还会把视口行槽纳入键值，因为缓存实例携带屏幕坐标。
- 每个由项目生成的 quad 颜色都是有限值的预乘线性 RGBA。不透明度和覆盖率变化必须同时缩放
  RGB 与 alpha；debug build 会在 GPU/软件 presenter 分流前立即校验两层 quad。Windows 软件
  矩形会解码保留帧中的 sRGB 目标通道，在线性光空间混合相同的源颜色，只对 RGB 编码一次，
  并把 alpha 作为线性 UNORM 做 source-over。
- Windows LCD 文字只有在配置为不透明 backdrop、实际 opacity 为 `1`，并且使用 CPU/GDI
  presenter 或 wgpu 设备支持 `DUAL_SOURCE_BLENDING` 时才生效。实际 `off`/`rgb`/`bgr` 模式
  进入帧键。GPU 与 CPU presenter 都在应用覆盖率之前变换前景色，在线性光空间分别衰减目标
  RGB，并用最大通道权重计算 alpha。图像和彩色字形拥有更高分支优先级。
- 显示缩放切换会通过 winit 的事件内 writer 一次提交一个物理 inner size。该尺寸在可容纳时
  保持逻辑几何，不低于 30×10 终端下限，并受目标显示器 work area 限制；renderer surface、
  pane grid/PTY、IME 几何与重绘都在 Windows 应用 `WM_DPICHANGED` 前跟随同一个目标。

事件循环线程获取完整帧时不会等待 VT 工作线程。它对活动标签页的每个解析器和所需内联图像
存储使用 `try_lock`。任一锁不可用时，代码释放已经取得的所有保护对象，记录待重绘状态，
并且不调用 `GpuRenderer::render`。

脏行只在 `finish_successful_frame` 中清除：

- Windows CPU 呈现要等 `SetDIBitsToDevice` 成功返回；
- wgpu 呈现要等命令提交并调用 `queue.present(frame)`。

`SetDIBitsToDevice` 可以报告失败。wgpu 的 present 调用不会返回能够表示后续呈现失败的结果。
表面超时、遮挡、过期、次优或丢失时，代码会使帧键失效并请求重绘。过期和次优表面会重新
配置。丢失的表面会重新创建。验证错误向上传播。这些表面获取失败都不会清除脏行。

网格几何记账包含保留的行分配，不只计算可见的 `cols × rows`。列数大幅减少时会压紧行。
相邻尺寸变化会保留可复用容量，避免反复分配。降低回滚历史上限会释放多余的
`VecDeque` 容量。

复制到剪贴板时会保留孤立或不完整的右边框线。只有连贯的多行侧边框，并且最终以右下角
框线字符收尾时，才会删除该边框。在 Windows 上，成功的 OSC 52 写入最多只会延迟重写一次，
且仅当剪贴板回到写入前读取到的同一文本时执行；新的或无法读取的剪贴板所有者不会被覆盖。

rmux/tmux 内的程序会通过 `DCS tmux` passthrough 发出 OSC 52。可信会话必须同时启用以下
两项；只启用 `set-clipboard` 不会转发该 wrapper：

```tmux
set -s set-clipboard on
set -g allow-passthrough on
```

两项设置都表示信任 pane 输出。任意 DCS passthrough 不进入 SonicTerm 的 OSC 52 解析器；
multiplexer 必须先验证并解包。

CAN 和 SUB 会取消当前转义序列。解析器会先重置转义记账，防止被取消的 DCS 或 APC 媒体
序列输出不完整图像。主机取消停滞捕获后，解析器会丢弃剩余负载直到结束边界，不会把它打印
到网格。

### 图集与字体不变量

CPU 字形图集固定为 2,048 × 2,048 个 BGRA8 像素，约 16 MiB。元数据最多保存
`MAX_ATLAS_ENTRIES = 16,384` 个条目，包括空白和缺失哨兵。

未命中时，图集先使用回收矩形，再使用分层打包器。元数据或打包空间不足时，图集会按确定
规则淘汰最冷的四分之一。淘汰会改变图集代次。如果组帧期间发生淘汰，渲染器会放弃该帧，
就地重置图集，使携带 UV 的行缓存失效，并请求新帧。重试期间会关闭淘汰，直到成功呈现一帧。
固定像素分配不会增长。

`RowGlyphCache` 和 `LineQuadCache` 的键由窗格编号、绝对行号和行哈希组成。两者容量约为
所有窗格可见行总数的四倍。容量或几何尺寸变化会清空对应缓存。脏行会使其绝对行条目失效。
字体、主题、缩放、表面尺寸和图集替换会使对应缓存失效。保留量会计算哈希表已分配的
键/条目 bucket，以及每个嵌套向量的容量。普通清空会保留可复用表容量，因此有界变化形成
高水位包络，而不是完全平坦的字节线。窗格离开渲染器时，一个由事件循环独占的操作会先删除
其字形缓存条目，再删除 quad 缓存条目，保留其它窗格的命中，随后请求压紧两个表。并发保留量
快照无法观察到只完成一半的有序淘汰。

内联图像图集从 1 × 1 的 CPU/GPU 占位符开始。出现可渲染媒体时，它扩展为
2,048 × 2,048。连续 240 个已渲染帧没有内联媒体后，它回到占位符。文字和图像使用独立
图集，因此图像压力不会淘汰文字字形。

Windows 降级呈现会保留完整 CPU 图集，同时把 GPU 图集纹理缩成 1 × 1 占位符。回到 wgpu
呈现时，代码重新创建匹配纹理、重置图集状态、使所有携带 UV 的缓存失效，并在采样新纹理前
强制完整重绘。

DirectWrite 次像素图块在 CPU 图集与 GPU unorm 覆盖率 view 中始终是线性 BGRA 覆盖率。
它们绝不能经过彩色矩形转换或 sRGB 彩色 view。Alpha 保持为 RGB 覆盖率最大值，因此不满足
条件以及 `off` 呈现都有确定的灰度值。修改 LCD 模式只影响呈现，不得重建或重新解释任一图集。

字体发现、塑形和光栅化与渲染器策略分离。生成的 FFI 绑定只留在各自包装 crate 内。
可变字体元数据格式错误、缺失或越界时，代码回退到基础 OS/2 字重和字宽。FreeType 内嵌
位图字形会在解码像素前先检查 2,048 像素和 16 MiB 的字形分配上限。

隐藏预热窗口池默认为 1。设为 0 会关闭它。普通硬件路径最多接受 5。真实软件适配器或最终
降级状态启用时，任何非零目标都限制为 1。实时配置重载会清空池；后续
`about_to_wait` 每次最多重建一个条目。

### PTY 与原生线程不变量

终端输入采用非阻塞入队。`PtyHandle::send_input_nonblocking` 对四条消息的通道调用
`try_send`。消息超过 16 MiB、队列已满或 writer 已断开时，会返回保留原始字节的
`PtyInputError`。应用发送 `UserEvent::PtyInputRejected`，记录原因并显示错误通知。
它不会自动重放这些字节。

PTY reader 使用可复用的 64 KiB `BytesMut` 分配，并通过 64 槽通道发送
`PtyOutputChunk` 视图。通道满时 reader 阻塞，让操作系统施加背压；输出不会被丢弃。

每窗格 VT 工作线程只在推进 VT 状态和快照解析器所拥有的模式时持有该窗格的解析器锁。
它会把自有的 VT 事件移出该锁保护区，然后在解锁后处理剪贴板和命令事件、解码或缩放内联
媒体，并更新常驻存储。主窗口和子窗口中创建的窗格使用同一个宿主事件处理器。拆出操作只
修改共享的重绘 `WindowId`，因此工作线程可以跟随窗格，无需持有 `Arc<Window>`。

`PtyHandle::drop` 都先发送取消信号，在平台支持时取消同步 I/O，然后终止子进程。后续顺序
按平台区分。

Unix 使用 `waitid(P_PID, ..., WEXITED | WNOHANG | WNOWAIT)` 观察自然退出，
但不释放会话编号。拆除时先杀死原进程组，再反复杀死同会话中的活动成员。主端在等待 I/O
线程前关闭。reader 和 writer 各有 500 ms。终止重试和子进程回收各有独立的 500 ms。
若无法证明会话清理完成，leader 保持未回收，避免编号被不安全地复用。

Windows 拆除先给 reader 500 ms，再给 writer 500 ms，然后关闭主端。
`sonic-conpty-drain` 通过克隆 reader 排空输出，`sonic-conpty-close` 关闭主端。
关闭最多等待 2 秒。关闭成功后，排空再独立等待 2 秒。超时会分离辅助线程。
辅助线程启动失败或关闭失败会返回未完成结果并记录 warning。子进程退出与回收另有
500 ms 上限。

这些期限保证 `Drop` 不会无限阻塞界面。原生关闭未完成时，代码不会谎报成功。

### 发布验证边界

根目录 `Cargo.toml` 的 `[workspace.package]` 是版本来源。发布工作流只由匹配
`v[0-9]+.[0-9]+.[0-9]+*` 的 tag 启动。它首先把 tag ref 解引用到对应 commit，要求该
commit 位于 `origin/main` 历史中，并要求它存在一个完全相同、已完成且成功的 `CI` push run。随后
`prepare-release-assets.py check-version` 必须把 tag 解析为语义版本，确认该版本与每个
workspace package 一致，并要求普通源码一致性 gate 通过。

工作流构建五组必需包：

| 平台 | 架构 | 包 |
| --- | --- | --- |
| macOS | `aarch64` | `.dmg` |
| macOS | `x86_64` | `.dmg` |
| Windows | `x86_64` | `.msi` |
| Linux | `x86_64` | `.deb` |
| Linux | `x86_64` | `.tar.gz` |

每个包都有类型化 JSON 片段。汇总步骤要求五组全部存在，拒绝重复文件名或重复元组，重新
计算哈希，并拒绝 `dist` 中未登记的 `.dmg`、`.msi`、`.deb` 和 `.tar.gz` 文件。它生成
`release-assets.json`、确定性 `SHA256SUMS.txt` 和 `release-upload-paths.txt`。发布 action
只上传该路径清单中的文件。

Windows CI tests shard 运行：

```bash
cargo test -p sonicterm-gpu --test windows_warp_allocator_baseline -- --nocapture
```

该闸门要求 WARP 和分配器报告可用。生产策略预留字节必须低于 64 MiB，最大块必须低于
128 MiB。在相同分配负载下，`MemoryHints::MemoryUsage` 候选必须比
`MemoryHints::Performance` 对照预留更少字节。Release 要求 tag commit 存在完全相同且成功的
`main` CI run，之后才启动 `build-windows`，因此失败会阻止 MSI 和发布，且无需在 tag 阶段重跑。

Linux 包验证会构建 `.deb` 与 `.tar.gz` 两种布局。运行冒烟测试在 X11/Xvfb 和
Wayland/Weston 上使用 Vulkan/lavapipe。测试要求窗口创建、GPU 初始化、`/bin/sh` PTY
标记往返，以及之后一次原生呈现。

macOS 打包会检查二进制架构和应用的 ad-hoc 签名。工作流没有执行 Developer ID 签名、
公证或 DMG 打包后启动冒烟测试。Windows 工作流也没有签名 MSI 或安装运行它。因此，
安装包签名不是当前已验证的发布不变量。

发布验证要求 tag commit 存在完全相同、已完成且成功的 `main` CI run，随后核对每个
workspace package 版本并验证 release asset 工具。它不会重复源码、unit、integration、文档、
平台 runtime、allocator、coverage 或普通 CI 的 package gate。各平台 job 从该 provenance
边界直接开始，只执行原生 release build、package 验证、MSI metadata 验证与 Linux package
smoke。Release Rust target build 不读写 Rust cache；Windows job 可以恢复由普通 CI 立即发布的
vcpkg binary cache。

### 源码与检查索引

| 契约 | 主要源码或检查 |
| --- | --- |
| 真实堆内存测试 | `crates/sonicterm-{grid,io,vt,app,resource,text}/tests/` |
| 资源清单与基线 | `scripts/test-resource-inventory.sh`、`scripts/test-resource-baseline-evidence.sh` |
| 损伤区域与呈现完成 | `crates/sonicterm-gpu/src/core.rs` |
| 字形图集与行缓存 | `crates/sonicterm-text/src/{glyph_atlas,row_glyph_cache}.rs`、`crates/sonicterm-gpu/src/row_quad_cache.rs` |
| PTY 拆除 | `crates/sonicterm-io/src/pty.rs` |
| 所有者与计费顺序 | `crates/sonicterm-app/src/app/{mod,retention}.rs` |
| 发布资产契约 | `scripts/prepare-release-assets.py`、`scripts/test-release-assets.sh` |
| 发布任务依赖图 | `.github/workflows/release.yml` |
