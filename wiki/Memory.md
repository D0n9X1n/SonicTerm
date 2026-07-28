# Memory / 内存

## English

SonicTerm bounds what it holds and reports what it is holding. This page
covers both: what each subsystem keeps and what happens when it fills, then
how to read that from the log when a session grows more than you expect.

### Why bounds alone are not the whole story

Every subsystem here has a limit. That is necessary and not sufficient — a
session with many panes can leave every individual limit intact while the sum
climbs, which is exactly the shape behind reported multi-gigabyte growth. So
some limits are per-pane, some are process-wide, and the ones that matter most
are the process-wide ones.

### What each subsystem holds

| What | Limit | What happens at the limit |
| --- | --- | --- |
| Decoded inline images | 256 MiB across the process; each pane gets a share of that, floored at 4 MiB and capped at 64 MiB | Oldest images are discarded. Each pane always keeps its newest one |
| In-flight image transfers | 64 MiB of staging across the process; 4 MiB guaranteed per transfer | A 14th simultaneous transfer is refused and shows nothing |
| One image payload | 16 MiB | The transfer is refused rather than truncated |
| Grid cells | 1,048,576 cells; scrollback additionally bounded by 24 MiB of retained bytes | Oldest scrollback rows are dropped |
| Escape sequence | 1 MiB | The sequence is discarded through its terminator |
| Hyperlink targets | 16,384 links, 8 KiB per URI | Links whose cells have scrolled away are reclaimed, then the new one is admitted |
| PTY input message | 16 MiB, 4 queued | Oversized input is refused |
| PTY output queue | 64 chunks | The reader waits rather than growing |
| Software render frame | 160 MiB | Larger surfaces are refused |

With one to four panes open, each gets the full 64 MiB image allowance. Beyond
that the share shrinks — twenty panes get about 12.8 MiB each — so opening
many panes that all display images will evict older ones.

### Scrollback is bounded twice

`[terminal].scrollback` sets a row count, and retained bytes are bounded
separately at 24 MiB. Rows carrying hyperlinks, combining marks, or non-default
underlines cost more than plain text, so a byte budget can be reached before
the row count is.

At the default 1,000 rows this never happens. If you raise `scrollback`
substantially and your output is link-heavy, expect to retain fewer rows than
the number you set.

### Two things SonicTerm discards that you can see

Most reclamation is invisible and uninteresting. Two cases are neither, so
they are logged at the **default** level — you do not need to have enabled
debug logging beforehand:

```
grep 'memory::reclaimed' ~/.sonicterm/logs/sonicterm.log
```

| Line | What happened | What to do |
| --- | --- | --- |
| `cancelled a media capture that stopped receiving` | An image transfer delivered nothing for a full minute and was abandoned. The image will not appear | Re-send it. Common after a laptop sleeps mid-transfer or an SSH link drops |
| `discarded inline images from idle panes` | Total decoded image memory crossed the process ceiling, so older images were dropped from panes you were not using | Re-send the images you still need, or keep fewer panes holding images |

Both lines carry the byte figures involved.

### Reading what a session is holding

Everything else is diagnostics and needs `debug`:

```toml
[logging]
level = "debug"
```

Two lines are written at most once every 30 seconds, on the idle-wake path —
a window that never wakes produces no samples.

**`pane retention`** — one per pane:

| Field | What it covers | What you can do |
| --- | --- | --- |
| `grid_visible_bytes` | Cells currently on screen | Nothing — it is the screen |
| `grid_history_bytes` | Scrollback | Lower `scrollback` |
| `grid_alternate_bytes` | The screen saved behind a full-screen program | Nothing — it frees itself on exit |
| `parser_bytes` | Escape sequences and image transfers being parsed right now | Nothing — transient |
| `hyperlink_bytes` | OSC 8 link targets | Nothing — reclaimed as links scroll away |
| `inline_media_bytes` | Decoded inline images | Display fewer images, or fewer panes |
| `pty_output_bytes` | Queued PTY output | Nothing — bounded by the queue |
| `pty_input_bytes` | Input queued toward the shell | Nothing unless you paste very large payloads |

`total_bytes` is their sum, and `largest_seam` names the biggest one, which is
where to look first.

**`session retention`** — one for the whole process, summing every pane, with
the same fields plus `panes`. **This is the line that matters for growth**: a
session can hold far more than any single pane suggests, and this is the only
figure that shows it.

### Reading a growth report

Compare `session retention` lines over time, not just the first and last.
Sampling is rate-limited and runs on the idle-wake path, so a gap in the log
means nothing woke the loop — not that memory was flat across it.

Then look at which seam moved. `inline_media_bytes` climbing means images;
`grid_history_bytes` climbing means scrollback; `parser_bytes` staying high
means a transfer is stuck rather than transient.

---

## 中文

SonicTerm 会限制自身占用的内存，并报告实际占用情况。本页涵盖两部分：各子系
统保留什么内容、达到上限时会发生什么，以及当会话内存超出预期时如何从日志中
读取这些信息。

### 为什么仅有上限还不够

这里的每个子系统都有限制。这是必要的，但并不充分——面板较多的会话可能每个
单项限制都未被突破，而总量却在攀升，这正是此前报告的数 GB 内存增长的形态。
因此部分限制是按面板计的，部分是进程级的，而最关键的是进程级限制。

### 各子系统保留的内容

| 内容 | 上限 | 达到上限时 |
| --- | --- | --- |
| 已解码的内联图像 | 进程共 256 MiB；每个面板分得其中一份，下限 4 MiB，上限 64 MiB | 丢弃最早的图像。每个面板始终保留最新的一张 |
| 传输中的图像 | 进程共 64 MiB 暂存；每个传输保证 4 MiB | 第 14 个并发传输会被拒绝且不显示 |
| 单张图像负载 | 16 MiB | 拒绝传输，而非截断 |
| 网格单元 | 1,048,576 个单元；回滚缓冲另受 24 MiB 保留字节限制 | 丢弃最早的回滚行 |
| 转义序列 | 1 MiB | 丢弃该序列直至其终止符 |
| 超链接目标 | 16,384 个链接，每个 URI 8 KiB | 回收已滚出屏幕的链接，然后接纳新链接 |
| PTY 输入消息 | 16 MiB，队列 4 条 | 拒绝超大输入 |
| PTY 输出队列 | 64 个数据块 | 读取端等待，而非增长 |
| 软件渲染帧 | 160 MiB | 拒绝更大的表面 |

打开 1 至 4 个面板时，每个面板可获得完整的 64 MiB 图像配额。超出之后配额会
缩小——20 个面板时每个约 12.8 MiB——因此打开大量同时显示图像的面板会导致较
早的图像被清除。

### 回滚缓冲受两重限制

`[terminal].scrollback` 设定行数，而保留字节另有 24 MiB 的独立限制。带有超
链接、组合字符或非默认下划线的行比纯文本占用更多，因此可能在达到行数上限前
先触及字节预算。

在默认的 1,000 行下不会发生这种情况。如果你大幅调高 `scrollback` 且输出包含
大量链接，实际保留的行数会少于所设定的数值。

### SonicTerm 会丢弃的两类可见内容

大多数内存回收是不可见且无需关注的。以下两种情况并非如此，因此它们在**默认**
级别下即会记录——你无需事先开启 debug 日志：

```
grep 'memory::reclaimed' ~/.sonicterm/logs/sonicterm.log
```

| 日志行 | 发生了什么 | 可采取的措施 |
| --- | --- | --- |
| `cancelled a media capture that stopped receiving` | 图像传输整整一分钟没有新数据，已被放弃。该图像不会显示 | 重新发送。笔记本在传输中途休眠或 SSH 连接中断后常见 |
| `discarded inline images from idle panes` | 已解码图像的总内存超出进程上限，因此丢弃了未使用面板中较早的图像 | 重新发送仍需要的图像，或减少持有图像的面板数量 |

两种日志行都会附带相关的字节数。

### 查看会话的实际占用

其余内容均属诊断信息，需要 `debug` 级别：

```toml
[logging]
level = "debug"
```

以下两行最多每 30 秒写入一次，且发生在空闲唤醒路径上——完全不唤醒的窗口不会
产生任何采样。

**`pane retention`**——每个面板一行：

| 字段 | 含义 | 可采取的措施 |
| --- | --- | --- |
| `grid_visible_bytes` | 当前显示在屏幕上的单元格 | 无 — 这就是屏幕本身 |
| `grid_history_bytes` | 回滚缓冲 | 调低 `scrollback` |
| `grid_alternate_bytes` | 全屏程序背后保存的主屏幕 | 无 — 程序退出时自动释放 |
| `parser_bytes` | 当前正在解析的转义序列与图像传输 | 无 — 瞬时占用 |
| `hyperlink_bytes` | OSC 8 链接目标 | 无 — 链接滚出后自动回收 |
| `inline_media_bytes` | 已解码的内联图像 | 减少图像使用，或减少面板数量 |
| `pty_output_bytes` | 排队中的 PTY 输出 | 无 — 受队列限制 |
| `pty_input_bytes` | 排队发往 shell 的输入 | 除非粘贴超大内容，否则无需处理 |

`total_bytes` 是它们之和，`largest_seam` 指出占用最大的一项，应优先从该项
着手排查。

**`session retention`**——整个进程一行，汇总所有面板，字段与上表相同并额外
包含 `panes`。**这是排查内存增长时最关键的一行**：会话的实际占用可能远超任
何单个面板所显示的数值，而只有这个数字能揭示这一点。

### 解读内存增长报告

应比较不同时间点的 `session retention` 行，而不只看首尾两行。采样受速率限制
且运行在空闲唤醒路径上，因此日志中的间隔仅表示期间没有唤醒事件，并不代表内存
在此期间保持平稳。

随后查看是哪一项发生了变化。`inline_media_bytes` 上升说明是图像；
`grid_history_bytes` 上升说明是回滚缓冲；`parser_bytes` 持续偏高则说明某个
传输卡住了，而非瞬时占用。
