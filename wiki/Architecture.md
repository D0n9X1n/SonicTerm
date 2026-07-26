# Architecture / 架构

## English

> Canonical invariants:
> [`docs/ARCHITECTURE.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/ARCHITECTURE.md).
> Canonical crate map:
> [`docs/MODULES.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/MODULES.md).

This page explains the implementation behind those documents. It describes the
current repository, including transitional boundaries that are not yet the
final architecture.

## System goals

SonicTerm is organized around five goals:

1. keep the terminal protocol and grid independent of the window system;
2. keep native AppKit/Win32 work at the platform edge;
3. keep PTY workers off the winit event-loop thread;
4. carry renderer input through one explicit render-model seam;
5. retain pixels and rebuild only damaged terminal rows when correctness allows.

## Runtime data flow

```text
macOS/Windows binary
        |
        v
sonicterm-app (winit orchestration and authoritative live topology)
   |          |                    |
   |          | intents/effects    | frame inputs
   |          v                    v
   |   sonicterm-app-core   sonicterm-render-model ---> sonicterm-gpu ---> screen
   |                                                 |       |
   v                                                 |       v
sonicterm-io ---> sonicterm-vt ---> sonicterm-grid ---+   text/font stack
   PTY bytes        ANSI/VT          cells/history         shaping/raster/atlas
```

The arrows above describe runtime data flow, not every Cargo dependency. In the
actual dependency graph, `sonicterm-gpu` depends on `sonicterm-render-model`, and
`sonicterm-render-model` depends on and re-exports grid/config/UI crates through
`boundary::{grid,cfg,ui}`. The GPU crate therefore does not directly depend on
those three implementation crates.

## Layer responsibilities

| Layer | Owns | Must not own |
| --- | --- | --- |
| `sonicterm-types` | small values and backend-free trait contracts | winit, wgpu, PTY processes |
| `sonicterm-resource` | owner tree, retained-memory ledger, reservation tokens | the memory itself, or per-seam enforcement policy |
| `sonicterm-app-core` | pure intents, effects, reducer state, effect ordering | native handles or blocking IO |
| `sonicterm-io` | local PTY/process boundary and optional SSH transport | terminal interpretation or UI |
| `sonicterm-vt` / `sonicterm-grid` | ANSI/VT behavior, cells, history, dirty rows | native windows or GPU resources |
| `sonicterm-ui` | renderer-independent tabs, search, selection, IME, pane layout | draw calls or platform APIs |
| `sonicterm-render-model` | renderer-facing pane/overlay/geometry bundles | wgpu policy |
| `sonicterm-text` / font crates | shaping, fallback, rasterization, glyph caches | window lifecycle |
| `sonicterm-gpu` | frame assembly, damage, caches, wgpu/software drawing | PTY ownership or app topology |
| `sonicterm-app` | authoritative windows/tabs/panes/PTYs and event routing | platform-specific AppKit/Win32 policy where a shell seam exists |
| platform crates | startup, native menus, drag/drop, installers | reusable cross-platform behavior |

## State ownership today

There are two application-state layers:

- `sonicterm-app::App` and each `WindowState` own the authoritative live objects:
  winit windows, renderers, tab vectors, pane trees, PTY handles, parsers,
  selections, search, IME, drag state, and redraw scheduling.
- `sonicterm-app-core::AppState` is a backend-free reducer mirror. It tracks
  serializable counters and state transitions and emits ordered `AppEffect`s,
  but several boundary effects are currently record-only or translated back to
  existing `sonicterm-app` operations.

That distinction matters. The reducer is real and tested, but it is not yet the
single source of truth for complete window/tab/pane topology.

## Important seams

### Intent/effect seam

Input or lifecycle code sends an `AppIntent` through
`AppStateMachine::handle`. The reducer updates `AppState` and returns effects in
stable class order: PTY writes, render requests, OS drag, clipboard, window
operations, menu updates, then logs. `sonicterm-app` executes the effects that
need winit, clipboard, PTY, or OS access.

### Render-model seam

The app holds parser guards long enough to expose each mutable grid as a
`PaneRender`. The bundle includes pane geometry, viewport position, focus,
cursor style, broadcast state, scrollbar alpha, and inline images. UI snapshots
and overlays travel in `RenderInputs`. `sonicterm-gpu` consumes these types and
reaches grid/config/UI identities only through render-model boundary re-exports.

### Font seam

`sonicterm-engine::FontStack` adapts `sonicterm-font` to the renderer. HarfBuzz
shapes text; platform locators discover fonts; DirectWrite is the default glyph
rasterizer on Windows and FreeType elsewhere, with FreeType fallback on Windows.
The renderer sees raster tiles, glyph metrics, and shape results rather than raw
FFI handles.

## Concurrency and correctness invariants

1. A pane VT worker parses output under its parser lock, then releases the lock
   before posting a typed redraw event. Worker threads do not call native window
   APIs.
2. The render path uses non-blocking `try_lock`. If any pane parser is busy, the
   frame is deferred rather than presenting a mixture of old and new panes.
3. Tab transfer moves the live `PaneState` and `PtyHandle`; it updates a shared
   `WindowId` redraw target so the existing worker follows the pane.
4. VT/grid mutations mark dirty rows in the same frame. A dirty alternate-screen
   pane repaints its whole clipped pane; primary-screen panes may use narrow
   dirty-row damage.
5. Windows software rendering keeps its full-surface presentation semantics,
   separate from retained GPU damage decisions.
6. Dropping a `PtyHandle` terminates and boundedly reaps its child. Transfer code
   validates both endpoints before moving state to avoid accidental process loss.
7. Config/theme/font changes invalidate the relevant renderer caches and mark
   open panes dirty.

## Cargo dependency groups

```text
contracts:  sonicterm-types
accounting: sonicterm-resource (charged by app and GPU; owns no frame data)
terminal:   sonicterm-io -> sonicterm-vt -> sonicterm-grid
UI/model:   sonicterm-cfg -> sonicterm-ui -> sonicterm-render-model
fonts:      font-config/fontconfig/freetype/harfbuzz -> font -> engine/text
rendering:  render-model + text + engine + block-glyph -> sonicterm-gpu
app:        app-core + terminal + UI/model + GPU -> sonicterm-app
platform:   sonicterm-app -> sonicterm-mac / sonicterm-windows
future:     sonicterm-io -> sonicterm-mux (not used by the GUI today)
```

This is a conceptual grouping; the exact edge list is in [Crate Reference](Crate-Reference).

## Where to read the code

| Topic | Primary paths |
| --- | --- |
| Canonical invariants | `docs/ARCHITECTURE.md`, `docs/MODULES.md` |
| Intent/effect contracts | `crates/sonicterm-app-core/src/{intent,effect,reducer,state_machine}.rs` |
| Authoritative app state | `crates/sonicterm-app/src/app/mod.rs` |
| Resource governor | `crates/sonicterm-resource/src/{ledger,owner,reservation}.rs` |
| Retention sampling and charging | `crates/sonicterm-app/src/app/retention.rs` |
| PTY worker and redraw handoff | `crates/sonicterm-app/src/app/spawn_pane.rs` |
| Render boundary | `crates/sonicterm-render-model/src/{pane_render,inputs,lib}.rs` |
| Renderer | `crates/sonicterm-gpu/src/core.rs` |
| Font adapter | `crates/sonicterm-engine/src/fontstack.rs` |
| Platform entry points | `crates/sonicterm-{mac,windows}/src/main.rs` |

For proposed changes to these boundaries, see [Architecture Evolution](Architecture-Evolution).

## 中文

> 规范架构不变量：
> [`docs/ARCHITECTURE.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/ARCHITECTURE.md)。
> 规范 crate 映射：
> [`docs/MODULES.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/MODULES.md)。

本页解释这些文档背后的实现，并如实描述当前仓库，包括仍处于迁移阶段的边界。

## 系统目标

SonicTerm 的组织方式围绕五个目标：

1. 终端协议和网格不依赖窗口系统；
2. AppKit/Win32 原生工作留在平台边缘；
3. PTY worker 不阻塞 winit 事件循环线程；
4. 渲染输入只通过明确的 render-model 接缝传递；
5. 保留上一帧像素，在正确性允许时只重建受损的终端行。

## 运行时数据流

```text
macOS/Windows 二进制
        |
        v
sonicterm-app（winit 编排与权威实时拓扑）
   |          |                    |
   |          | intent/effect      | 帧输入
   |          v                    v
   |   sonicterm-app-core   sonicterm-render-model ---> sonicterm-gpu ---> 屏幕
   |                                                 |       |
   v                                                 |       v
sonicterm-io ---> sonicterm-vt ---> sonicterm-grid ---+   文本/字体栈
   PTY 字节          ANSI/VT          单元格/历史          塑形/光栅/图集
```

上图表示运行时数据流，不等同于全部 Cargo 依赖。真实依赖中，`sonicterm-gpu`
依赖 `sonicterm-render-model`；后者依赖并通过 `boundary::{grid,cfg,ui}`
重新导出 grid/config/UI crate。这样 GPU crate 不直接依赖这三个实现 crate。

## 各层职责

| 层 | 负责 | 不应负责 |
| --- | --- | --- |
| `sonicterm-types` | 小型值类型和后端无关 trait 契约 | winit、wgpu、PTY 进程 |
| `sonicterm-resource` | owner 树、常驻内存账本、reservation token | 内存本身，或各接缝的限额策略 |
| `sonicterm-app-core` | 纯数据 intent、effect、reducer state、effect 排序 | 原生句柄或阻塞 IO |
| `sonicterm-io` | 本地 PTY/进程边界和可选 SSH transport | 终端语义或 UI |
| `sonicterm-vt` / `sonicterm-grid` | ANSI/VT 行为、单元格、历史、脏行 | 原生窗口或 GPU 资源 |
| `sonicterm-ui` | 与渲染器无关的标签页、搜索、选区、IME、窗格布局 | 绘制调用或平台 API |
| `sonicterm-render-model` | 面向渲染器的窗格、overlay、几何数据 | wgpu 策略 |
| `sonicterm-text` / 字体 crate | 塑形、回退、光栅化和字形缓存 | 窗口生命周期 |
| `sonicterm-gpu` | 帧组装、损坏计算、缓存、wgpu/软件绘制 | PTY 所有权或应用拓扑 |
| `sonicterm-app` | 权威窗口/标签页/窗格/PTY 与事件路由 | 已有 shell 接缝可承载的平台专属策略 |
| 平台 crate | 启动、原生菜单、拖放和安装包 | 可复用的跨平台行为 |

## 当前状态所有权

应用状态目前有两层：

- `sonicterm-app::App` 和每个 `WindowState` 拥有权威实时对象：winit 窗口、
  renderer、标签页向量、窗格树、PTY handle、parser、选区、搜索、IME、拖动状态与重绘调度。
- `sonicterm-app-core::AppState` 是后端无关的 reducer 镜像。它跟踪可序列化的计数和
  状态转换并产生有序 `AppEffect`；但若干边界 effect 目前只用于记录，或仍翻译为原有
  `sonicterm-app` 操作。

因此 reducer 已真实运行并有测试，但尚未成为完整窗口/标签页/窗格拓扑的唯一事实来源。

## 关键接缝

### Intent/effect 接缝

输入和生命周期代码通过 `AppStateMachine::handle` 发送 `AppIntent`。reducer 更新
`AppState`，并按稳定分类顺序返回 effect：PTY 写入、渲染、OS 拖动、剪贴板、窗口操作、
菜单更新、日志。`sonicterm-app` 执行需要 winit、剪贴板、PTY 或 OS 的部分。

### Render-model 接缝

app 保持 parser guard，在一帧内把每个可变 grid 暴露为 `PaneRender`。其中包含窗格几何、
视口位置、焦点、光标样式、广播状态、滚动条透明度和内联图像。UI snapshot 和 overlay
通过 `RenderInputs` 传递。`sonicterm-gpu` 消费这些类型，并且只经 render-model 的边界
重新导出访问 grid/config/UI 类型身份。

### 字体接缝

`sonicterm-engine::FontStack` 把 `sonicterm-font` 适配给渲染器。HarfBuzz 负责文本塑形；
平台 locator 发现字体；Windows 默认使用 DirectWrite 光栅化，其它平台默认 FreeType，
Windows 也用 FreeType 作回退。渲染器只接触光栅 tile、字形度量和塑形结果，不接触原始 FFI 句柄。

## 并发与正确性不变量

1. 窗格 VT worker 在 parser 锁内解析输出，释放锁以后才发送类型化重绘事件；worker 不调用原生窗口 API。
2. 渲染路径使用非阻塞 `try_lock`。任一窗格 parser 正忙时推迟整帧，而不是呈现新旧混合的窗格。
3. 标签页转移会移动现有 `PaneState` 和 `PtyHandle`，并更新共享的 `WindowId` 重绘目标，使原 worker 跟随窗格。
4. VT/grid 修改必须在同一帧标记脏行。备用屏幕窗格出现任一脏行时重绘完整裁剪窗格；主屏幕可使用窄脏行损坏区域。
5. Windows 软件渲染保持完整 surface 呈现语义，与保留式 GPU 损坏决策分离。
6. `PtyHandle` 析构会终止并限时回收子进程。转移代码先验证两端，再移动状态，以免意外丢失进程。
7. 配置、主题或字体变化会使相关 renderer 缓存失效，并把打开的窗格标脏。

## Cargo 依赖分组

```text
契约：      sonicterm-types
记账：      sonicterm-resource（由 app 与 GPU 计费；不持有帧数据）
终端：      sonicterm-io -> sonicterm-vt -> sonicterm-grid
UI/模型：   sonicterm-cfg -> sonicterm-ui -> sonicterm-render-model
字体：      font-config/fontconfig/freetype/harfbuzz -> font -> engine/text
渲染：      render-model + text + engine + block-glyph -> sonicterm-gpu
应用：      app-core + terminal + UI/model + GPU -> sonicterm-app
平台：      sonicterm-app -> sonicterm-mac / sonicterm-windows
未来：      sonicterm-io -> sonicterm-mux（当前 GUI 未使用）
```

这是概念分组；精确依赖边见 [Crate 参考 / Crate Reference](Crate-Reference)。

## 从哪里阅读源码

| 主题 | 主要路径 |
| --- | --- |
| 规范不变量 | `docs/ARCHITECTURE.md`, `docs/MODULES.md` |
| Intent/effect 契约 | `crates/sonicterm-app-core/src/{intent,effect,reducer,state_machine}.rs` |
| 权威应用状态 | `crates/sonicterm-app/src/app/mod.rs` |
| 资源 governor | `crates/sonicterm-resource/src/{ledger,owner,reservation}.rs` |
| 常驻内存采样与计费 | `crates/sonicterm-app/src/app/retention.rs` |
| PTY worker 与重绘交接 | `crates/sonicterm-app/src/app/spawn_pane.rs` |
| 渲染边界 | `crates/sonicterm-render-model/src/{pane_render,inputs,lib}.rs` |
| 渲染器 | `crates/sonicterm-gpu/src/core.rs` |
| 字体适配 | `crates/sonicterm-engine/src/fontstack.rs` |
| 平台入口 | `crates/sonicterm-{mac,windows}/src/main.rs` |

这些边界的改进建议见 [架构演进 / Architecture Evolution](Architecture-Evolution)。
