# Runtime Lifecycle / 运行时生命周期

## English

This page follows SonicTerm from process start to process exit. For terminal
bytes and rendering internals, continue with [Terminal IO and VT](Terminal-IO-and-VT)
and [Rendering and Fonts](Rendering-and-Fonts).

## Startup sequence

```text
main()
  -> install panic and exit tracing
  -> load sonicterm.toml (collect warnings)
  -> initialize logging with [logging]
  -> load theme and keymap
  -> create AppStateMachine
  -> build MacShell or WindowsShell
  -> create winit EventLoop<UserEvent>
  -> create App and run ApplicationHandler
  -> resumed: create first native window and GpuRenderer
  -> create first tab and spawn its PTY pane
```

Both platform binaries install panic/exit diagnostics before normal startup
work, then defer the tracing subscriber until after the user logging config has
been read. Loading failures can therefore be surfaced without permanently
locking in the wrong log level.

Windows sets per-monitor-v2 DPI awareness before winit creates an HWND and may
accept a private `--tear-out-payload` used for cross-process tab drag. macOS
disables AppKit automatic window tabbing before the event loop starts.

## Shell construction

`MacShell` and `WindowsShell` are builders around the same cross-platform app.
They install platform hooks for:

- native menus;
- window-ready work that requires a real NSWindow or HWND;
- OS drag/drop handoff;
- theme/keymap asset loader closures;
- an optional pending tab payload.

`run()` creates `EventLoop<UserEvent>`, installs proxy bridges, constructs
`App`, and calls `run_app`.

## Winit lifecycle

`App` implements `ApplicationHandler<UserEvent>`. Its callbacks delegate to
small lifecycle methods:

| Callback | Main responsibility |
| --- | --- |
| `resumed` | create the first window, renderer, and first tab/pane |
| `user_event` | menu, OS-drag, update, and typed redraw events |
| `window_event` | keyboard, mouse, IME, resize, focus, redraw, and close |
| `new_events` | wake scheduled redraws when `WaitUntil` expires |
| `about_to_wait` | combine frame pacing, cursor blink, notifications, quit deadlines, and periodic resource-retention sampling |
| `exiting` | record an orderly event-loop exit |

A load-bearing rule in `user_event` is that pending window creation is drained
before pending OS-drag teardown. Otherwise a tab tear-out could clean up state
before the destination window has entered the window map.

## Window, tab, and pane ownership

```text
App
  windows: HashMap<WindowId, WindowState>
    WindowState
      native window + GpuRenderer
      TabBar + Vec<TabState>
        TabState
          PaneTree
          active pane id
          per-tab search state
      panes: HashMap<PaneId, PaneState>
        PaneState
          Parser/Grid behind Arc<Mutex<_>>
          optional PtyHandle
          redraw target WindowId
          inline images and terminal mode atomics
```

The main window is one entry in the same map as child windows. Accessors resolve
`main_window_id`; child-window event handling uses the same underlying
`WindowState` representation.

A new tab spawns one pane, adds a `Tab`, and creates a leaf `PaneTree`. Splitting
spawns another pane and replaces the focused leaf with a horizontal or vertical
split. The tree computes pane rectangles; every visible grid and PTY is resized
to its allocated cell area.

## Resource ownership and retention charging

Alongside the window/tab/pane tree, `App` holds a process-local **resource
governor** from `sonicterm-resource` that tracks retained memory. The owner tree
it instantiates mirrors the first two levels of the topology above:

```text
Process (created with App)
  Window   (one per live window)
    AppPane (one per live pane)
```

The resource contract also permits `AppPane -> LocalPty`, shared font/raster/
atlas owners, and a mux branch, but nothing creates those today — they are
reserved capacity, not live topology.

The governor is an accounting layer, not a limiter. It is constructed with an
unlimited process ceiling, and window owners are created with tracking-only
limits; the per-seam caps described in [Logging](Logging) are what actually
bound memory. Only the per-pane owner carries a budget, and that figure is
derived from the seam caps and sits above them, so it acts as a tripwire for a
seam that has stopped bounding rather than as a second limit.

Renderer retention — glyph atlas and software frame — is measured and logged but
is **not** charged to the governor; `sonicterm-gpu` has no dependency on the
resource crate. A governor total is a total of what the app charges, not of what
the process holds.

Lifecycle:

- **Startup.** `App` creates the governor and its immutable process root.
- **Window registration.** A new window registers a `Window` owner and
  immediately reconciles its panes, because a window arrives with panes already
  populated. A window that fails to register keeps working, but is never
  retried: reconcile skips a window that has no owner, so it and its panes stay
  outside accounting for that window's life.
- **Pane registration.** A pane gets its `AppPane` owner when it is created — a
  new tab and a split each reconcile immediately rather than waiting for the
  next sample — and panes migrated during tear-out are registered as part of
  that migration. This happens at every log level.
- **Charging.** The idle-wake path samples what each pane retains and moves that
  pane's charges to match. Charges are resized in place rather than released and
  re-taken, so the ledger never briefly reads zero for memory that is still
  held. **This runs at every log level**, because charging is not a diagnostic:
  it is what fills the ledger the per-pane budget is enforced against, and a
  governor that only charged while someone was watching would apply no limit in
  the sessions that ship. A pane whose parser lock is busy is skipped and keeps
  its previous charge until the next pass.
- **Migration.** Moving a pane between windows re-attributes its charges to the
  destination window's owner, so a torn-out tab does not leave its memory
  accounted to the window it left. Re-attribution runs in the same pass as
  charging, at every log level.
- **Release.** Charges are pane-owned RAII tokens, and teardown is two ordered
  steps: the pane's charge tokens release first, then the owner guards close the
  pane and window records. The order is load-bearing — closing an owner is
  refused while it still holds charges, so `PaneState` declares `charges` before
  `owner` and relies on Rust dropping fields in declaration order. A window
  dropping does not itself free pane memory; its panes' charges release first,
  and a leaked charge surfaces as a refused close rather than a silent
  undercount.

The retention log lines are the diagnostic half of this pass: setting the
`memory` target to debug adds a `pane retention` line per pane and a
`session retention` line per session, at most once every 30 seconds. Charging
and re-attribution above run regardless. Neither half blocks the loop — a pane
whose parser lock is contended is skipped rather than waited on.

## Input routing order

Keyboard input is handled in this order:

1. quit confirmation chord;
2. command palette editing;
3. active IME composition;
4. terminal search editing;
5. READONLY/copy-mode handling;
6. configured keymap actions;
7. terminal byte encoding and PTY write.

This order prevents search or IME text from leaking into the shell. In READONLY
mode, disallowed actions are consumed rather than executed or forwarded.

Key encoding reads terminal modes exposed by the active parser:

- DECCKM selects SS3 versus CSI cursor-key sequences;
- kitty keyboard flags select CSI-u encodings where needed;
- ordinary text and unbound chords fall through to PTY bytes.

If broadcast mode is active, bytes written by the source pane are copied to the
calculated receiver panes after the source write.

## Intent and effect dispatch

```text
input/lifecycle code
  -> App::dispatch_intent(AppIntent)
  -> AppStateMachine::handle
  -> reducer updates mirror AppState
  -> stable effect-class sort
  -> App::dispatch_effects
  -> PTY / redraw / clipboard / URL / window / native boundary
```

Current implementation note: `sonicterm-app-core` mirrors many transitions, but
`WindowState` and `PaneTree` remain authoritative for live topology. Some
effects are therefore observability records while the existing app operation
performs the real mutation.

## Redraw scheduling

PTY workers never redraw per byte. They accumulate output and flush a typed
`UserEvent::RequestRedraw(WindowId)` after a byte threshold or short time
threshold. The event-loop thread resolves the current window and invokes native
`request_redraw`.

A redraw request may still be delayed to the next frame boundary when output is
streaming. The effective period follows the monitor refresh rate on hardware,
and uses lower software-render limits under software adapters. Cursor blinking,
notification expiry, quit confirmation, and deferred redraws are folded into
one `ControlFlow::WaitUntil` deadline rather than a permanent heartbeat.

During `RedrawRequested`, the app attempts to lock every visible pane parser
without blocking. One failed lock defers the complete frame. Successful guards
supply all `PaneRender` inputs to the renderer.

## Config reload

Configuration is read at startup and then only when the user runs **Reload
Config** from the command palette (`Action::ReloadConfig`). There is no
background watcher, so nothing re-reads the config on a timer, on a filesystem
event, or during ordinary window events.

A reload re-parses `sonicterm.toml` and re-reads the theme and keymap files it
names, then applies changes to all live windows and panes. Depending on the
field, reload can update theme colors, key hints, padding, scrollback limits,
cursor, renderer cache state, and the warm-window pool. Invalid input is
reported rather than silently replacing the active config.

## Tab drag and tear-out

There are two related paths:

- in-process dragging uses global screen rectangles to merge a tab into another
  SonicTerm window;
- OS handoff uses pasteboard payload polling on macOS (without a native
  `NSDraggingSession`) and OLE drag/drop on Windows; either path can carry a
  serialized payload to another process.

For an in-process transfer, the live tab state and every `PaneState` move. The
PTY is not cloned or respawned. Each pane's shared redraw target is updated to
the destination `WindowId`. Transfer validates source and destination before
mutation because dropping a pane would terminate its child process.

A small pool of hidden, fully initialized child windows reduces tear-out
latency. Consuming one schedules replenishment up to the configured cap.

## Shutdown

Closing a pane drops its `PtyHandle`, which terminates and boundedly reaps the
child. Closing a main window while child windows remain can hide the main window
instead of exiting. When no active windows remain, `pending_exit` is consumed in
`about_to_wait`, which calls `ActiveEventLoop::exit`.

On macOS, `Cmd+Q` uses a two-press confirmation: the first non-repeat press shows
“Press ⌘Q one more time to quit”; a second press within five seconds exits. The
native menu's explicit Quit command can exit immediately.

## Representative sequences

### New split

```text
keymap split_right
  -> run action for frontmost window
  -> spawn new PTY + Parser/Grid + worker threads
  -> PaneTree::split(active, Right, new_id)
  -> resize every visible pane/grid/PTY
  -> focus new leaf
  -> request redraw
```

### PTY output

```text
child bytes -> PTY reader channel -> VT worker
  -> parser/grid mutation under lock
  -> update mode/title/media side effects
  -> release lock
  -> coalesced RequestRedraw(WindowId)
  -> winit thread -> RedrawRequested -> try_lock all panes -> render
```

## Where to read the code

| Topic | Primary paths |
| --- | --- |
| Platform startup | `crates/sonicterm-{mac,windows}/src/main.rs` |
| Shell builder | `crates/sonicterm-app/src/shell.rs` |
| App and WindowState | `crates/sonicterm-app/src/app/mod.rs` |
| Winit callbacks | `crates/sonicterm-app/src/app/{event_loop,window_event}.rs` |
| Input actions/encoding | `crates/sonicterm-app/src/app/{keymap_dispatch,key_encoding}.rs` |
| Pane spawning | `crates/sonicterm-app/src/app/spawn_pane.rs` |
| Drag/transfer | `crates/sonicterm-app/src/app/{tear_out,tab_transfer,tab_state}.rs` |
| Config reload | `crates/sonicterm-app/src/app/config_apply.rs` |
| Reducer boundary | `crates/sonicterm-app-core/src/` |

## 中文

本页从进程启动一直跟踪到退出。终端字节和渲染细节分别见
[终端 IO 与 VT](Terminal-IO-and-VT) 和 [渲染与字体](Rendering-and-Fonts)。

## 启动顺序

```text
main()
  -> 安装 panic 与退出追踪
  -> 读取 sonicterm.toml（收集 warning）
  -> 根据 [logging] 初始化日志
  -> 读取主题和 keymap
  -> 创建 AppStateMachine
  -> 构建 MacShell 或 WindowsShell
  -> 创建 winit EventLoop<UserEvent>
  -> 创建 App 并运行 ApplicationHandler
  -> resumed：创建首个原生窗口和 GpuRenderer
  -> 创建首个标签页并启动它的 PTY 窗格
```

两个平台二进制都会先安装 panic/退出诊断，再执行普通启动工作；tracing subscriber 则等到
读取用户日志配置后才安装。这样既能暴露配置加载错误，又不会永久锁定错误日志级别。

Windows 会在 winit 创建 HWND 前启用 per-monitor-v2 DPI awareness，并可接收内部
`--tear-out-payload` 以支持跨进程标签页拖动。macOS 在事件循环启动前关闭 AppKit 自动窗口标签化。

## Shell 构建

`MacShell` 和 `WindowsShell` 都是同一个跨平台 app 的 builder。它们安装：

- 原生菜单；
- 只有真实 NSWindow 或 HWND 出现后才能执行的 window-ready 工作；
- OS 拖放交接；
- 主题与 keymap 资产加载闭包；
- 可选的待接收标签页 payload。

`run()` 创建 `EventLoop<UserEvent>`，安装 proxy bridge，构建 `App`，然后调用 `run_app`。

## Winit 生命周期

`App` 实现 `ApplicationHandler<UserEvent>`：

| 回调 | 主要职责 |
| --- | --- |
| `resumed` | 创建首个窗口、renderer 和首个标签页/窗格 |
| `user_event` | 菜单、OS 拖动、更新和类型化重绘事件 |
| `window_event` | 键盘、鼠标、IME、resize、focus、redraw 和 close |
| `new_events` | `WaitUntil` 到期时唤醒计划重绘 |
| `about_to_wait` | 合并帧节奏、光标闪烁、通知、退出 deadline 和周期性资源占用采样 |
| `exiting` | 记录有序事件循环退出 |

`user_event` 中一个关键顺序是：先排空待创建窗口，再清理待处理 OS 拖动状态，否则标签页拖出时，
目标窗口还没进入窗口 map，旧状态就可能先被清理。

## 窗口、标签页和窗格所有权

```text
App
  windows: HashMap<WindowId, WindowState>
    WindowState
      原生窗口 + GpuRenderer
      TabBar + Vec<TabState>
        TabState
          PaneTree
          活动窗格 id
          每标签页搜索状态
      panes: HashMap<PaneId, PaneState>
        PaneState
          Arc<Mutex<_>> 内的 Parser/Grid
          可选 PtyHandle
          重绘目标 WindowId
          内联图像和终端模式原子值
```

主窗口也是该 map 中的一项，由 `main_window_id` 定位。子窗口事件处理使用同一种
`WindowState` 表示。

新标签页会启动一个窗格、添加 `Tab`，并创建单叶 `PaneTree`。分屏会启动另一个窗格，
把当前叶节点替换为横向或纵向 split。树计算窗格矩形，每个可见 grid 和 PTY 随其单元格区域 resize。

## 资源所有权与占用计费

除窗口/标签页/窗格树外，`App` 还持有一个来自 `sonicterm-resource` 的进程内
**资源 governor**，用于跟踪常驻内存。它实际建立的 owner 树对应上面拓扑的前两层：

```text
Process（随 App 创建）
  Window   （每个存活窗口一个）
    AppPane （每个存活窗格一个）
```

资源契约同时允许 `AppPane -> LocalPty`、共享字体/光栅/atlas owner 以及 mux 分支，
但目前没有任何代码创建它们——它们是契约中预留的容量，而非当前的实际拓扑。

governor 是记账层而非限额层。它以无上限的进程额度构建，窗口 owner 只做跟踪而不
设限；真正约束内存的是 [日志 / Logging](Logging) 中描述的各接缝上限。只有窗格
owner 带有预算，且该数值由各接缝上限推导并高于它们，因此它是用于捕捉「某个接缝
已不再限制自身」的绊线，而不是第二道限额。

渲染器的常驻内存（字形 atlas 与软件帧）会被测量并记录日志，但**不会**计入
governor：`sonicterm-gpu` 并不依赖 resource crate。因此 governor 的总量是「app 计费
的总量」，而不是「进程实际持有的总量」。

生命周期：

- **启动。** `App` 创建 governor 及其不可变进程根。
- **窗口注册。** 新窗口注册 `Window` owner 后立即协调其窗格，因为窗口出现时其
  窗格已经存在。注册失败的窗口仍可正常工作，但不会重试：协调过程会跳过没有 owner
  的窗口，因此在该窗口的整个生命周期内，它及其窗格都不会出现在记账中。
- **窗格注册。** 窗格在创建时即获得 `AppPane` owner——新建标签页与分屏都会立即
  协调，而不等待下一次采样；撕离迁移的窗格则在迁移过程中完成注册。这在任何日志
  级别下都会发生。
- **计费。** 空闲唤醒路径会采样每个窗格的常驻内存，并把该窗格的 charge 调整到
  相应数值。charge 采用原地缩放而非先释放再重新申请，因此账本不会在内存仍被持有
  时短暂读到零。**这在任何日志级别下都会发生**，因为计费并非诊断：它负责填充
  「窗格预算据以生效」的账本，而一个只在有人观察时才计费的 governor，在实际发布
  的会话中不会施加任何限制。parser 锁被占用的窗格会被跳过，并保留上一次的
  charge，直到下一轮采样。
- **迁移。** 在窗口之间移动窗格会把其 charge 重新归属到目标窗口的 owner，因此
  撕离的标签页不会把内存继续记在原窗口名下。重新归属与计费在同一轮中执行，且在
  任何日志级别下都会发生。
- **释放。** charge 是归属于窗格的 RAII token，拆除分为有序的两步：先释放该窗格的
  charge token，再由 owner guard 关闭窗格与窗口记录。这个顺序是关键——owner 仍持有
  charge 时会拒绝关闭，因此 `PaneState` 把 `charges` 声明在 `owner` 之前，依赖 Rust
  按声明顺序 drop 字段。窗口被 drop 本身并不直接释放窗格内存：先释放其窗格的
  charge，而泄漏的 charge 会表现为关闭被拒绝，而不是无声的少计。

这一轮中属于诊断的部分是占用日志：把 `memory` target 设为 debug 后，会为每个窗格
输出一行 `pane retention`，并为整个会话输出一行 `session retention`，且最多每 30
秒一次。上面的计费与重新归属则不受此影响，始终执行。两者都不会阻塞事件循环——
parser 锁被占用的窗格会被跳过而非等待。

## 输入路由顺序

键盘输入按以下顺序处理：

1. 退出确认组合键；
2. 命令面板编辑；
3. 活跃 IME 组合输入；
4. 终端搜索编辑；
5. READONLY/复制模式；
6. 用户 keymap action；
7. 终端字节编码和 PTY 写入。

这个顺序防止搜索或 IME 文本泄漏到 shell。READONLY 中不允许的 action 会被消费，而不是执行或转发。

编码逻辑读取活动 parser 暴露的终端模式：

- DECCKM 决定光标键使用 SS3 还是 CSI；
- kitty keyboard flag 在需要时选择 CSI-u；
- 普通文本和未绑定组合键落入 PTY 字节路径。

广播开启后，源窗格写入完成，再把同一字节复制到计算出的接收窗格。

## Intent 与 effect 派发

```text
输入/生命周期代码
  -> App::dispatch_intent(AppIntent)
  -> AppStateMachine::handle
  -> reducer 更新镜像 AppState
  -> 稳定 effect 分类排序
  -> App::dispatch_effects
  -> PTY / 重绘 / 剪贴板 / URL / 窗口 / 原生边界
```

当前 `sonicterm-app-core` 已镜像许多转换，但实时拓扑仍以 `WindowState` 和 `PaneTree` 为准。
因此部分 effect 是可观测记录，实际修改仍由现有 app 操作完成。

## 重绘调度

PTY worker 不会逐字节重绘，而是累积输出，在达到字节阈值或短时间阈值后发送类型化
`UserEvent::RequestRedraw(WindowId)`。事件循环线程解析当前窗口，再调用原生 `request_redraw`。

持续输出时，重绘请求还可能推迟到下一个帧边界。硬件路径按显示器刷新率；软件 adapter
使用更低帧率。光标闪烁、通知过期、退出确认和延迟重绘被合并到一个
`ControlFlow::WaitUntil` deadline，而不是永久 heartbeat。

收到 `RedrawRequested` 时，app 非阻塞尝试锁住每个可见窗格 parser。任一个失败就推迟整帧；
全部成功后才向 renderer 提供完整 `PaneRender`。

## 配置重载

配置只在启动时读取，之后仅在用户从命令面板运行 **Reload Config**
（`Action::ReloadConfig`）时重新读取。没有后台 watcher，因此不会有定时、文件系统事件
或普通窗口事件触发的重复读取。

重载会重新解析 `sonicterm.toml`，并重新读取它所指定的主题与 keymap 文件，然后应用到所有
实时窗口和窗格。不同字段可更新主题颜色、快捷键提示、padding、scrollback 上限、光标、
renderer 缓存和预热窗口池。无效输入会报告错误，而不是默默替换活动配置。

## 标签页拖动和撕离

存在两条相关路径：

- 进程内拖动使用全局屏幕矩形，把标签页合并到另一个 SonicTerm 窗口；
- OS handoff 在 macOS 使用 pasteboard payload polling（没有原生 `NSDraggingSession`），在 Windows 使用 OLE drag/drop；两者都可把序列化 payload 交给另一个进程。

进程内转移会移动现有 tab state 和所有 `PaneState`。PTY 不克隆、不重启；每个窗格共享的
重绘目标更新为目标 `WindowId`。转移在修改前验证源和目标，因为误删窗格会终止其子进程。

一小组隐藏且已完整初始化的子窗口可降低拖出延迟；消耗一个后会按配置上限补充。

## 退出

关闭窗格会析构 `PtyHandle`，从而终止并限时回收子进程。若仍有子窗口，关闭主窗口可只隐藏主窗口。
没有活动窗口时，`about_to_wait` 消费 `pending_exit` 并调用 `ActiveEventLoop::exit`。

macOS 的 `Cmd+Q` 使用两次按键确认：第一次非重复按键显示“Press ⌘Q one more time to quit”；
五秒内第二次按键才退出。原生菜单的明确 Quit 命令可立即退出。

## 代表性流程

### 新分屏

```text
keymap split_right
  -> 对最前窗口执行 action
  -> 启动新 PTY + Parser/Grid + worker thread
  -> PaneTree::split(active, Right, new_id)
  -> resize 所有可见窗格/grid/PTY
  -> 聚焦新叶节点
  -> 请求重绘
```

### PTY 输出

```text
子进程字节 -> PTY reader channel -> VT worker
  -> 锁内修改 parser/grid
  -> 更新模式/标题/媒体 side effect
  -> 释放锁
  -> 合并的 RequestRedraw(WindowId)
  -> winit 线程 -> RedrawRequested -> try_lock 全部窗格 -> render
```

## 从哪里阅读源码

| 主题 | 主要路径 |
| --- | --- |
| 平台启动 | `crates/sonicterm-{mac,windows}/src/main.rs` |
| Shell builder | `crates/sonicterm-app/src/shell.rs` |
| App 与 WindowState | `crates/sonicterm-app/src/app/mod.rs` |
| Winit 回调 | `crates/sonicterm-app/src/app/{event_loop,window_event}.rs` |
| 输入 action/编码 | `crates/sonicterm-app/src/app/{keymap_dispatch,key_encoding}.rs` |
| 窗格启动 | `crates/sonicterm-app/src/app/spawn_pane.rs` |
| 拖动/转移 | `crates/sonicterm-app/src/app/{tear_out,tab_transfer,tab_state}.rs` |
| 配置重载 | `crates/sonicterm-app/src/app/config_apply.rs` |
| Reducer 边界 | `crates/sonicterm-app-core/src/` |
