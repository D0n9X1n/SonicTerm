# Terminal IO and VT / 终端 IO 与 VT

## English

This page explains the path between a shell process and the cells shown by
SonicTerm. Local PTY operation is the shipping path. SSH and the mux daemon are
present in the workspace but have narrower status described below.

## End-to-end byte flow

```text
child process
  -> PTY master reader thread
  -> crossbeam Receiver<Bytes>
  -> per-pane VT worker
  -> vte::Parser + SonicTerm Performer
  -> Grid cells / scrollback / dirty rows / VtEvent
  -> typed redraw event
  -> app + renderer

keyboard / paste / terminal reply
  -> Sender<Vec<u8>>
  -> PTY writer thread
  -> child process
```

The reader uses `bytes::Bytes` so parsed output can move through the channel
without repeatedly copying large bursts. Input uses owned `Vec<u8>` messages.
Terminal replies such as DSR, DA, XTVERSION, palette queries, and kitty keyboard
queries use the same writer channel as user input, but parser/grid locks are
released before those writes occur.

## Local PTY lifecycle

`PtyHandle` owns:

- the output receiver and input sender;
- a deduplicating resize closure;
- the child process behind a mutex;
- the selected shell program path.

Spawn sequence:

```text
resolve configured/default shell
  -> open native PTY pair through portable-pty
  -> set TERM, COLORTERM, TERM_PROGRAM, TERM_PROGRAM_VERSION
  -> spawn child and close the slave in the parent
  -> start sonic-pty-reader
  -> start sonic-pty-writer
```

On Windows, automatic shell selection prefers PowerShell 7 (`pwsh.exe`),
including Microsoft Store installs, then Windows PowerShell, then `cmd.exe`.
On Unix it uses `$SHELL`, falling back to `/bin/zsh`. macOS zsh/bash launches as
a login shell in normal operation. Clean end-to-end test mode suppresses shell
profiles.

The resize closure packs rows and columns into one atomic value and suppresses
identical resizes. This avoids unnecessary SIGWINCH/ConPTY reflow when switching
tabs or recomputing an unchanged layout.

`PtyHandle::drop` is a process-lifecycle boundary: Unix sends SIGKILL first,
then the portable child kill method is attempted, followed by a bounded wait.
A warning is logged rather than hanging the UI forever if the child cannot be
reaped within the deadline.

## App-side pane workers

Each pane combines a `PtyHandle`, `Parser`, and `Grid`. The app starts:

- a VT reply thread that forwards parser-generated replies to the PTY writer;
- a VT worker that receives child output, advances the parser, extracts title,
  media, and command side effects, and coalesces redraw notifications.

The worker holds the parser mutex only while reading and mutating terminal state.
It then releases the guard before copying the current redraw target and posting
`UserEvent::RequestRedraw`. This prevents an AppKit/Win32 call while a parser
lock is held and lets the target window change during tab transfer.

## VT parser

`sonicterm-vt::Parser` wraps `vte::Parser` and a SonicTerm `Performer`. The
performer owns the grid and current terminal attributes. The parser includes an
ASCII fast path: while the state machine is in ground state, printable runs are
inserted in bulk until an escape/control byte appears.

Supported behavior includes:

- cursor movement, save/restore, insert/delete characters and lines;
- erase operations with background-color erase semantics;
- SGR attributes, indexed color, true color, underline styles and colors;
- primary and alternate screens, DECSTBM scroll regions, reverse index;
- autowrap, application cursor keys, cursor visibility and shape;
- bracketed paste, focus reporting, mouse tracking, SGR mouse mode;
- kitty keyboard flag push/pop/set/query;
- OSC titles, working directory, hyperlinks, clipboard events, palette/color queries;
- OSC 133 shell-integration prompt markers;
- iTerm2, kitty, and Sixel media events with a 16 MiB payload cap.

Media transfers draw staging from a 64 MiB process-wide pool, which
guarantees 13 simultaneous transfers at the 4 MiB floor. A transfer that
cannot be staged is refused and renders nothing rather than rendering
partially, and an oversized or truncated Sixel is likewise dropped: a cut
Sixel is byte-identical to a complete short one, so a partial render would
be indistinguishable from correct output. A transfer that stops receiving
for a full minute is abandoned and its staging reclaimed. See
[Memory](Memory) for the bounds and how to read them from the log.

The parser returns `VtEvent`s such as title changes, bell, hyperlink, clipboard,
media, command state, and cursor visibility. The app consumes these outside the
parser hot path.

A raw OSC 4 capture exists because vte limits the number of OSC parameters. It
collects the full palette query and suppresses the truncated duplicate callback.

## Grid model

A `Grid` owns:

- visible rows and bounded scrollback as `VecDeque<Line>`;
- cursor and default cell state;
- an optional boxed alternate-screen grid;
- a revision counter and per-visible-row dirty flags;
- up to 256 prompt regions in scrollback-absolute coordinates;
- autowrap and pending-wrap state.

Every meaningful mutation increments the revision and marks the affected rows.
`clear_dirty` is presentation bookkeeping and does not increment the revision.
A fresh grid starts fully dirty.

### Wide and combining characters

Width-two characters use a lead cell with `WIDE` plus a continuation cell with
`WIDE_CONT`. Mutations expand or repair ranges so half a wide glyph cannot be
left behind. Zero-width combining characters attach to the preceding lead
cell's `extras` field, walking backward over a continuation cell if necessary.

### Scrolling and history

Full-screen upward scrolling moves ejected rows into the scrollback deque and
reuses old storage where possible. Region scrolling changes only the configured
margin area; a full-height region deliberately uses the history-producing path.
Changing the scrollback limit trims the oldest rows immediately.

### Line storage

`Line` switches transparently between:

- `Flat(Vec<Cell>)` for arbitrary content;
- run-length `Cluster(Vec<Cluster>)` for repetitive rows.

A line compresses only when the clustered representation is materially smaller.
Content-equal flat and clustered lines iterate and hash identically. Mutating a
clustered row degrades it to flat only when required, preserving rare metadata
such as hyperlinks, combining codepoints, colors, and wide-cell flags.

### Prompt regions and selection access

OSC 133 prompt boundaries are stored in absolute history coordinates, enabling
“previous/next prompt” navigation after scrolling. Selection state lives in
`sonicterm-ui`; the grid exposes visible, scrollback, and absolute-row accessors
so selection and search can operate across history.

## Optional SSH status

The `sonicterm-io/ssh` feature adds a `russh` transport with PTY-like input,
output, and resize channels. A dedicated thread runs a current-thread Tokio
runtime. Authentication checks an explicit key or common `~/.ssh/id_ed25519`
and `id_rsa` files.

Current limitations:

- host keys are accepted unconditionally; no key is persisted or compared on later connections, so server identity is not authenticated;
- ssh-agent, password, and keyboard-interactive auth are not implemented;
- the GUI parses/validates SSH targets, but no shipping app call site currently connects an `SshHandle`.

Treat SSH as an implementation seam, not a fully integrated user feature.

## Mux daemon status

`sonicterm-mux` is a standalone future persistent-PTY daemon. It is a workspace
member and depends on `sonicterm-io`, but the GUI/platform crates do not depend
on it and the release workflow does not package it.

Its current protocol uses length-prefixed bincode messages for list, spawn,
attach, detach, input, resize, and kill. Each session owns a PTY, a 256 KiB raw
byte replay ring, and a bounded subscriber channel that drops the oldest event
under back-pressure. It forwards raw bytes; it does not yet run server-side VT
parsing or expose grid-aware scrollback.

## Concurrency rules

| Boundary | Rule |
| --- | --- |
| PTY reader/writer | dedicated named threads; channel handoff |
| Child process object | mutex only for pid/kill/wait operations |
| Parser/Grid | one mutable owner at a time through the pane parser lock |
| PTY replies | never write while holding parser/grid locks |
| Redraw target | copy `WindowId` under a short guard, then post after release |
| Mux | session map lock outside per-pane replay/subscriber locks |

## Representative sequences

### Terminal reply

```text
application sends CSI 6 n
  -> vte dispatches DSR query
  -> Performer computes cursor row/column
  -> reply Sender<Vec<u8>>
  -> VT-reply/PTY writer path
  -> child receives CSI <row>;<col> R
```

### Alternate-screen scroll

```text
TUI enters DECSET 1049
  -> save cursor + enter blank alternate grid
  -> mark full pane dirty
  -> linefeed/scroll inside alternate grid
  -> any dirty alt-screen row causes full clipped-pane repaint
  -> DECRST 1049 restores primary grid and marks it dirty
```

## Where to read the code

| Topic | Primary paths |
| --- | --- |
| PTY/process boundary | `crates/sonicterm-io/src/pty.rs` |
| Optional SSH | `crates/sonicterm-io/src/ssh.rs` |
| Pane worker pump | `crates/sonicterm-app/src/app/spawn_pane.rs` |
| VT parsing | `crates/sonicterm-vt/src/vt.rs` |
| Grid and dirty rows | `crates/sonicterm-grid/src/grid.rs` |
| Line compression | `crates/sonicterm-grid/src/line.rs` |
| Hyperlink registry | `crates/sonicterm-grid/src/hyperlink.rs` |
| Mux protocol/server | `crates/sonicterm-mux/src/{proto,frame,server,main}.rs` |

## 中文

本页解释 shell 进程与 SonicTerm 屏幕单元格之间的路径。本地 PTY 是当前发布版本的实际路径；
SSH 和 mux daemon 虽在工作区中，但状态更受限，见下文。

## 端到端字节流

```text
子进程
  -> PTY master reader 线程
  -> crossbeam Receiver<Bytes>
  -> 每窗格 VT worker
  -> vte::Parser + SonicTerm Performer
  -> Grid 单元格 / scrollback / 脏行 / VtEvent
  -> 类型化重绘事件
  -> app + renderer

键盘 / 粘贴 / 终端回复
  -> Sender<Vec<u8>>
  -> PTY writer 线程
  -> 子进程
```

reader 使用 `bytes::Bytes`，使大批输出通过 channel 时避免反复复制；输入使用拥有所有权的
`Vec<u8>`。DSR、DA、XTVERSION、调色板查询和 kitty keyboard 查询等回复与用户输入共用
writer channel，但执行写入前会释放 parser/grid 锁。

## 本地 PTY 生命周期

`PtyHandle` 拥有：

- 输出 receiver 和输入 sender；
- 去重 resize 闭包；
- mutex 后的子进程对象；
- 已选择的 shell 路径。

启动流程：

```text
解析配置/default shell
  -> 通过 portable-pty 打开原生 PTY pair
  -> 设置 TERM、COLORTERM、TERM_PROGRAM、TERM_PROGRAM_VERSION
  -> 启动子进程，父进程关闭 slave
  -> 启动 sonic-pty-reader
  -> 启动 sonic-pty-writer
```

Windows 自动选择顺序是 PowerShell 7（含 Microsoft Store 安装）、Windows PowerShell、
`cmd.exe`。Unix 使用 `$SHELL`，找不到时回退 `/bin/zsh`。正常 macOS 运行中 zsh/bash
以 login shell 启动；干净 E2E 测试模式会禁用 shell profile。

resize 闭包把行列打包进一个原子值，并忽略完全相同的 resize，避免切换标签页或重复布局时产生
多余 SIGWINCH/ConPTY reflow。

`PtyHandle::drop` 是进程生命周期边界：Unix 先发 SIGKILL，再调用 portable child kill，
然后限时等待。超过 deadline 时记录 warning，而不是让 UI 永久卡住。

## App 侧窗格 worker

每个窗格组合一个 `PtyHandle`、`Parser` 和 `Grid`。app 启动：

- VT reply 线程，把 parser 生成的回复转发给 PTY writer；
- VT worker，接收子进程输出、推进 parser、提取标题/媒体/命令 side effect，并合并重绘通知。

worker 只在读取和修改终端状态时持有 parser mutex；之后释放 guard，再复制当前重绘目标并发送
`UserEvent::RequestRedraw`。这样既不在 parser 锁内调用 AppKit/Win32，也允许标签页转移时更换目标窗口。

## VT parser

`sonicterm-vt::Parser` 包装 `vte::Parser` 与 SonicTerm `Performer`；performer 拥有 grid 和当前终端属性。
当状态机处于 ground state 时，parser 的 ASCII 快速路径会批量插入可打印字符，直到遇到 escape/control byte。

支持行为包括：

- 光标移动、保存/恢复、插入/删除字符和行；
- 带背景色擦除语义的 erase；
- SGR 属性、索引色、真彩色、下划线样式和颜色；
- 主/备用屏幕、DECSTBM 滚动区域、reverse index；
- 自动换行、应用光标键、光标可见性和形状；
- bracketed paste、focus report、mouse tracking、SGR mouse；
- kitty keyboard flag 的 push/pop/set/query；
- OSC 标题、工作目录、超链接、剪贴板事件、调色板/颜色查询；
- OSC 133 shell integration prompt marker；
- iTerm2、kitty 和 Sixel 媒体事件，payload 上限 16 MiB。

媒体传输从进程级 64 MiB 暂存池中申请空间，可保证 13 个并发传输获得 4 MiB
的下限配额。无法获得暂存空间的传输会被拒绝并且不显示任何内容，而不是显示
残缺图像；超长或被截断的 Sixel 同样会被丢弃：被截断的 Sixel 与完整的短
Sixel 在字节层面无法区分，因此部分渲染的结果与正确输出无从辨别。整整一分钟
没有收到新数据的传输会被放弃，其暂存空间随之回收。限制范围及如何从日志中
查看，参见 [内存 / Memory](Memory)。

parser 返回标题变化、bell、超链接、剪贴板、媒体、命令状态和光标可见性等 `VtEvent`，
app 在 parser 热路径之外消费。

由于 vte 限制 OSC 参数数量，代码对 OSC 4 做原始捕获，收集完整调色板查询并抑制截断后的重复 callback。

## Grid 模型

`Grid` 拥有：

- 以 `VecDeque<Line>` 保存的可见行和有界 scrollback；
- 光标与默认 cell 状态；
- 可选 boxed 备用屏幕 grid；
- revision 计数和可见行脏标记；
- 最多 256 个 scrollback 绝对坐标 prompt region；
- autowrap 与 pending-wrap 状态。

每次有效修改都会递增 revision 并标记相关行。`clear_dirty` 只是呈现 bookkeeping，不递增 revision。
新 grid 初始时全部为脏。

### 宽字符与组合字符

双宽字符使用带 `WIDE` 的 lead cell 与带 `WIDE_CONT` 的 continuation cell。修改范围会扩展或修复，
避免留下半个宽字形。零宽组合字符附着到前一个 lead cell 的 `extras`；必要时跨过 continuation 回找。

### 滚动和历史

全屏向上滚动会把离开屏幕的行放入 scrollback，并尽量复用旧存储。region scroll 只修改 margin；
如果 region 覆盖完整高度，则故意走会产生历史的路径。调小 scrollback 上限会立即丢弃最老行。

### Line 存储

`Line` 可透明切换：

- 任意内容使用 `Flat(Vec<Cell>)`；
- 重复内容使用 run-length `Cluster(Vec<Cluster>)`。

只有 clustered 表示显著更小时才压缩。内容相同的 flat 和 cluster 行迭代及 hash 结果一致。
修改 clustered 行只在必要时退化为 flat，并保留超链接、组合码点、颜色和宽字符标记等稀有元数据。

### Prompt region 与选区访问

OSC 133 prompt 边界以绝对历史坐标保存，因此滚动后仍可跳转上一个/下一个 prompt。选区状态位于
`sonicterm-ui`；grid 暴露可见行、scrollback 和绝对行访问器，供选区与搜索跨历史工作。

## 可选 SSH 状态

`sonicterm-io/ssh` feature 增加基于 `russh` 的 transport，提供类似 PTY 的输入、输出和 resize channel。
专用线程运行 current-thread Tokio runtime。认证检查显式 key，或常见的
`~/.ssh/id_ed25519`、`id_rsa`。

当前限制：

- host key 被无条件接受；不会保存 key，也不会在后续连接中比较，因此服务器身份没有经过认证；
- 尚无 ssh-agent、密码或 keyboard-interactive auth；
- GUI 会解析/校验 SSH target，但发布应用中当前没有连接 `SshHandle` 的调用点。

因此应把 SSH 看作实现接缝，而不是已完整集成的用户功能。

## Mux daemon 状态

`sonicterm-mux` 是未来的持久 PTY daemon。它是 workspace member 并依赖 `sonicterm-io`，
但 GUI/平台 crate 不依赖它，release workflow 也不打包它。

当前协议使用带长度前缀的 bincode message，支持 list、spawn、attach、detach、input、resize 和 kill。
每个 session 拥有 PTY、256 KiB 原始字节 replay ring，以及在背压时丢弃最老事件的有界 subscriber channel。
它只转发原始字节，尚未做服务端 VT 解析或 grid-aware scrollback。

## 并发规则

| 边界 | 规则 |
| --- | --- |
| PTY reader/writer | 专用命名线程，通过 channel 交接 |
| 子进程对象 | 仅 pid/kill/wait 操作持有 mutex |
| Parser/Grid | 通过窗格 parser 锁保证单一可变所有者 |
| PTY 回复 | 持有 parser/grid 锁时绝不写 PTY |
| 重绘目标 | 短 guard 内复制 `WindowId`，释放后再发事件 |
| Mux | session map 外层锁先于每窗格 replay/subscriber 锁 |

## 代表性流程

### 终端回复

```text
应用发送 CSI 6 n
  -> vte 派发 DSR 查询
  -> Performer 计算光标行列
  -> reply Sender<Vec<u8>>
  -> VT-reply/PTY writer 路径
  -> 子进程收到 CSI <row>;<col> R
```

### 备用屏幕滚动

```text
TUI 进入 DECSET 1049
  -> 保存光标并进入空白 alternate grid
  -> 标记完整窗格为脏
  -> alternate grid 内 linefeed/scroll
  -> 任一 alt-screen 脏行导致完整裁剪窗格重绘
  -> DECRST 1049 恢复 primary grid 并标脏
```

## 从哪里阅读源码

| 主题 | 主要路径 |
| --- | --- |
| PTY/进程边界 | `crates/sonicterm-io/src/pty.rs` |
| 可选 SSH | `crates/sonicterm-io/src/ssh.rs` |
| 窗格 worker pump | `crates/sonicterm-app/src/app/spawn_pane.rs` |
| VT 解析 | `crates/sonicterm-vt/src/vt.rs` |
| Grid 与脏行 | `crates/sonicterm-grid/src/grid.rs` |
| Line 压缩 | `crates/sonicterm-grid/src/line.rs` |
| 超链接 registry | `crates/sonicterm-grid/src/hyperlink.rs` |
| Mux 协议/server | `crates/sonicterm-mux/src/{proto,frame,server,main}.rs` |
