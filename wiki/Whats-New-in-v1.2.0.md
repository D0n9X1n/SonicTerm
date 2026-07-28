# What's New in v1.2.0 / v1.2.0 更新说明

## English

v1.2.0 is a memory release. There is no new feature on this page and nothing to
configure. The whole of it is that SonicTerm now knows how much memory it is
using, and stays inside it.

### What was wrong

v1.1.x did not have a memory bug. It had memory *limits* — one per subsystem,
each of which looked correct, and each of which was checked against itself.

Every subsystem reported its own usage, and those reports were wrong. Always in
the same direction: under. Enforcement then compared a subsystem's own report
against that subsystem's cap, so a subsystem could sit far over its limit and
report compliance the entire time. Nothing in that arrangement was capable of
noticing.

That is why the sessions behind this release — one that reached 80 GB on macOS,
another that reserved tens of gigabytes on Windows before its GPU driver ran out
of memory — never produced a single culprit. There was not one to find. No
allocator was ever proven responsible for them, and v1.2.0 does not claim
otherwise. What it changes is that memory is now contained, and that a question
about memory now has an answer.

### What changed

**Usage is attributed, not estimated.** Every subsystem's memory now belongs to
a specific pane, in a specific window. "The terminal is using 900 MB" becomes
"this pane's scrollback is holding 21 MB of it."

**The reported figures were checked against real memory.** Every accounting
figure in this release was measured against what the process actually holds,
rather than against the constant it was derived from. That is why these numbers
can be trusted now and could not before: a figure can be perfectly
self-consistent — returning to the same value across every round trip — and
still be the wrong figure. Only an independent measurement tells the two apart.

**Enforcement runs where the memory is actually spent.** Several bounds existed
but never executed in a shipped build. One was checked when you switched between
the normal and alternate screen and never while scrolling, so a grid growing
through ordinary output was never checked at all. Another charged memory only
when debug logging happened to be on. Cleanup for closed panes never ran.

**Panes give memory back.** A pane that took a large budget when few panes were
open now releases it as more open, instead of holding its early share for the
rest of the session.

### What you get

- **The terminal stops growing during a long session.** A working set that
  levels off, rather than one that climbs for as long as the window is open.
- **A pane full of images no longer starves the rest.** Image memory is bounded
  per pane and across the whole process together.
- **Scrollback stays bounded whether or not you switch screens.** Running vim,
  less, or tmux and coming back no longer decides whether the bound applies.
- **Memory questions have an answer.** Every subsystem's usage is attributed to
  a specific pane and window.

Measured before and after, against a counting allocator:

| | v1.1.x | v1.2.0 |
| --- | --- | --- |
| grid with combining marks or ZWJ emoji | undercounted 1.99×, which disabled the budget — 52,958,568 bytes held against a 25,165,824 budget, more than twice the limit, while reporting compliant | bounded, and the reported figure matches real memory exactly |
| deep scrollback | enforcement never ran on the scroll path — 63 MiB against a 24 MiB budget | 25 MiB |
| 64 panes with inline media | 496 MiB against a 256 MiB ceiling | 256 MiB |
| 64 panes capturing images | 374 MiB against a stated 64 MiB | 64.0 MiB, flat |
| hyperlink-heavy sessions | reported 8,388,244 against an 8,388,608 cap while holding 12,124,160 — 44.5% over | at cap |
| idle panes holding budgets earned early | 64 MiB each | 4 MiB each |

The last row is the four panes opened early in a 64-pane session, measured once
the rest have arrived — not a four-pane session.

### What did not change

**Rendering quality.** No image renders worse and no font renders differently.

**One visible trade.** Under extreme concurrent image load — more panes than the
guaranteed count, all receiving images at the same moment — a pane past that
count is refused rather than shown a partial picture. This is deliberate. A
truncated Sixel payload decodes to a real image that is a byte-identical prefix
of the whole one, so a half-picture cannot be told apart from a complete one.
Refusing shows you no less than truncating would, and it does not quietly hand
you a wrong image. Refusals are logged at `warn`.

**Configuration.** There is no new setting. The limits are internal, and derived
from the memory backing them rather than chosen, so there is no knob to tune and
none is needed.

**Memory diagnostics stay opt-in**, via `[logging] level = "debug"`. Reading
them is documented on the [Logging](Logging) page.

## 中文

v1.2.0 是一次围绕内存的发布。本页没有新功能，也没有需要配置的选项。它的全部内容
是：SonicTerm 现在知道自己占用了多少内存，并且会待在这个范围内。

### 此前的问题

v1.1.x 并不存在某一个内存缺陷。它存在的是一组内存**上限**——每个子系统一个，
每一个看上去都正确，而每一个都是拿自己去对照自己检查的。

每个子系统各自报告自己的占用，而这些报告是错的，并且始终偏向同一个方向：偏低。
随后，限额检查拿子系统自己的报告去比对该子系统的上限，因此一个子系统可以远远超出
自己的限额，同时全程报告合规。这样的结构本身没有任何环节能够发现问题。

这也正是本次发布背后那些会话——一个在 macOS 上增长到 80 GB，另一个在 Windows 上
预留了数十 GB 直至 GPU 驱动内存耗尽——始终无法归结到单一元凶的原因：本来就不存在
这样一个元凶。从未有任何分配器被证实为其成因，v1.2.0 也不作此声称。它改变的是：
内存现在被约束住了，并且关于内存的问题现在有了答案。

### 本次变化

**占用是被归属的，而不是被估算的。** 每个子系统的内存现在都归属到某个具体窗口中的
某个具体面板。「终端占用了 900 MB」变成了「其中 21 MB 是这个面板的回滚缓冲」。

**上报数值已与真实内存核对。** 本次发布中的每一个记账数值，都是对照进程实际持有的
内存测量的，而不是对照它自身推导所依据的常量。这正是这些数字现在可信、而此前不可信
的原因：一个数值可以完全自洽——在每一次往返操作后都回到相同取值——却依然是错的数值。
只有独立测量才能区分这两者。

**限额检查运行在真正消耗内存的路径上。** 有几处限额虽然存在，却从未在发布版本中执行
过。其中一处只在主屏幕与备用屏幕切换时检查，滚动时从不检查，因此一个通过普通输出持续
增长的网格根本不会被检查到。另一处只有在恰好开启 debug 日志时才记账。已关闭面板的
清理则从未运行。

**面板会归还内存。** 在打开的面板还很少时取得较大预算的面板，现在会随着更多面板打开
而释放它，而不是在整个会话余下的时间里一直占用早期获得的份额。

### 你会得到什么

- **长时间会话中，终端不再持续增长。** 工作集会趋于平稳，而不是只要窗口开着就一直
  攀升。
- **装满图像的面板不再挤占其它面板。** 图像内存由面板与整个进程共同限制。
- **无论是否切换屏幕，回滚缓冲都保持有界。** 运行 vim、less 或 tmux 后再返回，不再
  决定该限额是否生效。
- **关于内存的问题有了答案。** 每个子系统的占用都归属到具体的面板和窗口。

对照计数分配器测得的前后数据：

| | v1.1.x | v1.2.0 |
| --- | --- | --- |
| 含组合符号或 ZWJ emoji 的网格 | 少计 1.99×，并因此使限额失效——实际持有 52,958,568 字节，而限额为 25,165,824 字节，超出一倍以上，同时报告合规 | 有界，且上报数值与真实内存完全一致 |
| 较深的回滚缓冲 | 限额检查从未在滚动路径上运行——63 MiB，而限额为 24 MiB | 25 MiB |
| 64 个面板带内联媒体 | 496 MiB，而上限为 256 MiB | 256 MiB |
| 64 个面板同时捕获图像 | 374 MiB，而声称的上限为 64 MiB | 稳定在 64.0 MiB |
| 超链接密集的会话 | 报告 8,388,244 字节、上限 8,388,608 字节，实际持有 12,124,160 字节——超出 44.5% | 处于上限之内 |
| 早期取得预算后转入空闲的面板 | 每个 64 MiB | 每个 4 MiB |

最后一行指的是在一个 64 面板会话中最早打开的四个面板，并在其余面板都打开之后测得的
结果，而不是一个四面板会话。

### 未发生变化的部分

**渲染质量。** 没有任何图像渲染得更差，也没有任何字体的渲染发生变化。

**一处可见的取舍。** 在极端的并发图像负载下——同时接收图像的面板数超过可保证的数量
——超出该数量的面板会被拒绝，而不是显示一张不完整的图片。这是有意为之：被截断的
Sixel 数据会解码出一张真实的图像，它与完整图像的前缀逐字节相同，因此半张图片无法与
完整图片区分开来。拒绝所展示的内容并不比截断更少，而且不会悄悄给出一张错误的图像。
拒绝会记录为 `warn` 级别日志。

**配置。** 没有新增设置项。这些限额是内部的，并且由其背后的内存推导而来、而非人为
选定，因此既没有可调项，也不需要。

**内存诊断仍为可选开启**，通过 `[logging] level = "debug"` 启用。其读取方式记录在
[日志 / Logging](Logging) 页面。
