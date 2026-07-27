# Logging / 日志

## English

> Canonical developer diagnostics:
> [`docs/LOGGING.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/LOGGING.md).

### Paths

- Active log: `~/.sonicterm/logs/sonicterm.log`
- Rotated logs: `~/.sonicterm/logs/sonicterm.log.*`
- Crash dumps and exit traces: `~/.sonicterm/logs/crashes/`

On Windows, `~` means the current user's profile directory.

### Configuration

```toml
[logging]
level = "warn"          # error | warn | info | debug
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2
```

`warn` is the default level. SonicTerm loads `sonicterm.toml` before installing
the tracing subscriber, so the configured level applies from normal startup
onward. `RUST_LOG` can override the configured filter for a diagnostic run.
Stderr remains warning-oriented so a broad debug filter does not flood the
console.

Cleanup runs asynchronously. By default, rotated logs and crash dumps older
than two days are removed, at most three rotated logs and ten crash dumps are
kept, and the active log is never deleted by the retention pass. Set an age to
`0` to disable age-based eviction for that file class.

### Render and performance diagnostics

Set the level to `debug` and reproduce the problem:

```toml
[logging]
level = "debug"
```

The `render_timing` target records per-frame phases such as grid walking,
overlay assembly, glyph upload, surface acquisition, submission, and present.
It also identifies the main or child window and records whether the frame used
full or partial assembly. There is no separate render-timing option.

At startup, the renderer logs the selected wgpu adapter, device type, and
whether it appears to be a software adapter. Under RDP, a VM, or VDI, look for a
`software-render degrade engaged` line and review
`[appearance].software_render_mode` on the [Configuration](Configuration) page.

### Memory diagnostics

Set the level to `debug` and let the session run for a minute or two:

```toml
[logging]
level = "debug"
```

The `memory` target samples what each pane holds every 30 seconds and writes
one `pane retention` line per pane followed by one `session retention` line
for the whole process. Seven figures are reported separately because the
remedy differs:

| Field | What it covers | What you can do |
| --- | --- | --- |
| `grid_visible_bytes` | the cells currently on screen | nothing — it is the screen |
| `grid_history_bytes` | scrollback | lower `scrollback` |
| `grid_alternate_bytes` | the screen saved behind a full-screen program | nothing — it frees itself on exit |
| `parser_bytes` | escape sequences and inline-media capture buffers being parsed right now | nothing — transient |
| `hyperlink_bytes` | OSC 8 link targets | nothing — reclaimed as links scroll away |
| `inline_media_bytes` | decoded inline images | lower image usage, or open fewer panes |
| `pty_output_bytes` | the read buffer held by shell output waiting to be parsed | nothing — it drains as the terminal catches up |
| `pty_input_bytes` | input waiting to reach the shell, usually a large paste | nothing — it drains as the shell reads it |

Read `largest_seam` first. A total tells you a pane is large; that field tells
you which subsystem to look at. A pane holding 60 MB of inline images is
working as designed — a pane holding 60 MB of grid is not.

Compare successive `session retention` lines rather than one sample. The shape
of the curve is what separates a working set that levels off from memory that
keeps climbing.

Not every figure levels off, and that is intended. Cells reach a steady state
once scrollback fills. Interned hyperlinks keep growing until their limit is
reached, because a link stays reachable for as long as the cells referencing it
remain in retained scrollback — freeing it earlier would break a link the user
can still scroll back to.

### Triaging a session that feels heavy

The section above says what each figure means. This one is the order to read
them in when SonicTerm is using more memory than you expect, and it ends at a
pane and a subsystem you can name in a report.

**1. Turn the lines on.** Memory lines are written only at `debug`. At the
default `warn` — and at `info` — they are absent entirely, so an empty grep
means the level is wrong, not that the session is clean. Set the level in
`~/.sonicterm/sonicterm.toml` (on Windows, under your user profile), restart
SonicTerm, and use it normally for a few minutes:

```toml
[logging]
level = "debug"
```

**2. Find the heaviest pane.** Logs rotate into `sonicterm.log.<date>`, and a
busy day can add a second file, so pick the newest by modification time rather
than by name. This prints every pane's most recent total, largest first:

```sh
LOG=$(ls -1t ~/.sonicterm/logs/sonicterm.log* | head -1)
grep 'pane retention' "$LOG" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^pane=/)        { p = substr($i, 6); gsub(/"/, "", p) }
    if ($i ~ /^total_bytes=/)   t = substr($i, 13)
  }
  last[p] = t
} END { for (p in last) printf "%10.2f MB  %s\n", last[p] / 1048576, p }' \
  | sort -rn
```

```powershell
$log = Get-ChildItem ~/.sonicterm/logs/sonicterm.log* |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1
Select-String 'pane retention' $log | ForEach-Object {
  if ($_.Line -match 'pane="([^"]+)".*?total_bytes=(\d+)') {
    [pscustomobject]@{ Pane = $Matches[1]; Bytes = [long]$Matches[2] }
  }
} | Group-Object Pane | ForEach-Object {
  $newest = $_.Group[-1]
  [pscustomobject]@{ MB = [math]::Round($newest.Bytes / 1MB, 2); Pane = $_.Name }
} | Sort-Object MB -Descending
```

The label is the window id and the pane id. The pane id stays with the pane
when a tab moves to another window, but the window id in front of it changes —
so if a pane seems to vanish between samples, look for the same pane id behind
a different window id.

**3. Ask which subsystem.** On the heaviest pane, read `largest_seam`. A total
says a pane is big; this field says where to look. Each value names one row of
the field table above:

| `largest_seam` | Field it points at | First thing to try |
| --- | --- | --- |
| `grid_visible` | `grid_visible_bytes` | nothing — a large window is large |
| `grid_history` | `grid_history_bytes` | lower `scrollback` |
| `grid_alternate` | `grid_alternate_bytes` | quit the full-screen program in that pane |
| `parser` | `parser_bytes` | nothing yet — recheck on the next sample |
| `hyperlinks` | `hyperlink_bytes` | nothing — bounded, reclaimed as links scroll away |
| `inline_media` | `inline_media_bytes` | display fewer images, or close image-heavy panes |
| `pty_output` | `pty_output_bytes` | let the pane finish printing |
| `pty_input` | `pty_input_bytes` | let the shell drain a large paste |

The seam value is `hyperlinks` but the field is `hyperlink_bytes`; grep for the
one you actually want.

**4. Separate large from growing.** This is the step that decides whether you
have a bug. One sample cannot tell them apart — a pane holding 60 MB of images
steadily is working as designed, and a pane climbing every sample is not. Track
one pane across samples, substituting its label from step 2:

```sh
PANE='pane="WindowId(50820809344)/3"'
grep 'pane retention' "$LOG" | grep -F "$PANE" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^total_bytes=/)    t = substr($i, 13)
    if ($i ~ /^largest_seam=/) { s = substr($i, 14); gsub(/"/, "", s) }
  }
  printf "%s  %8.2f MB  %s\n", substr($1, 12, 8), t / 1048576, s
}'
```

```powershell
$pane = 'WindowId(50820809344)/3'
$re = 'pane="' + [regex]::Escape($pane) + '".*?total_bytes=(\d+).*?largest_seam="([^"]+)"'
Select-String 'pane retention' $log | ForEach-Object {
  if ($_.Line -match $re) {
    '{0}  {1,8:N2} MB  {2}' -f $_.Line.Substring(11, 8), ([long]$Matches[1] / 1MB), $Matches[2]
  }
}
```

Flat is fine, even at 60 MB:

```text
05:19:28     60.00 MB  inline_media
05:19:59     60.00 MB  inline_media
05:20:30     60.00 MB  inline_media
```

A steady climb across every sample is what to report:

```text
05:19:28      8.00 MB  grid_history
05:19:59     16.00 MB  grid_history
05:20:30     24.00 MB  grid_history
05:21:02     32.00 MB  grid_history
```

**Do not compare only the endpoints.** Samples are rate-limited to about one
set every 30 seconds, but they are written from the idle-wake path, so a busy
session logs more often than a quiet one. A gap in the log does not mean memory
was flat across it — it means nothing woke to measure. Two lines ten minutes
apart may span a burst you cannot see. Read several consecutive samples and
judge the shape.

**5. Check the process view.** Every pane can sit inside its own ceiling while
the total does not. The `session retention` line is the sum across panes, and
it carries `panes=N` instead of `largest_seam` — the seam breakdown is per pane
only.

```sh
grep 'session retention' "$LOG" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^panes=/)        n = substr($i, 7)
    if ($i ~ /^total_bytes=/)  t = substr($i, 13)
  }
  printf "%s  panes=%-3s %8.2f MB\n", substr($1, 12, 8), n, t / 1048576
}'
```

```powershell
Select-String 'session retention' $log | ForEach-Object {
  if ($_.Line -match 'panes=(\d+) total_bytes=(\d+)') {
    '{0}  panes={1,-3} {2,8:N2} MB' -f $_.Line.Substring(11, 8), $Matches[1], ([long]$Matches[2] / 1MB)
  }
}
```

If `panes` rises alongside the total, the session is growing because it holds
more panes, which is expected. If the total climbs while `panes` holds steady,
go back to step 2 and find which pane is responsible.

**6. Two warnings that are not bugs.** These appear at the default `warn`
level, and both mean SonicTerm corrected something rather than that something
broke. Finding one is not by itself worth reporting:

- `cancelled a media capture that stopped receiving; the transfer was abandoned and its staging is reclaimed` — an image transfer stopped mid-flight and its buffer was released instead of being pinned for the life of the pane.
- `revisited idle panes holding an inline-media budget sized for a smaller session` — panes that filled up early were still holding a share of the image budget from when fewer panes existed, and it was handed back.

### When inline images disappear

SonicTerm bounds decoded inline images per pane and across the whole process.
A pane that keeps displaying new images will eventually drop its oldest ones,
and with many panes open each gets a share of the process budget rather than
the full per-pane amount. Every pane always keeps at least its newest image.

This is deliberate, and it is logged: look for `inline media evicted to hold
the process-wide ceiling` at `warn` level. If images vanish and that line is
absent, it is a bug worth reporting.

Hyperlinks behave differently. When the link table fills, SonicTerm reclaims
entries whose text has scrolled out of history so that new links keep working.
Links that are still on screen are never reclaimed.

### Crash and hang diagnostics

The panic hook writes the panic payload, source location, backtrace, and a
small ring of recent tracing events to the crash directory. Normal event-loop
shutdown also emits a `sonic_exit` marker. A UI deadlock or hang may produce
neither a panic nor a crash file.

On macOS, capture a process sample before force-quitting:

```sh
sample <pid> 10 -file /tmp/sonicterm-hang.sample.txt
grep -nE 'dispatch_sync_f_slow|redraw_target|__psynch_cvwait' \
  /tmp/sonicterm-hang.sample.txt
```

For a torn-out-window problem, also record whether multiple panes were
producing continuous output, whether surviving windows remained responsive,
and whether the pane child processes exited after the window closed.

### Bug-report bundle

Include:

1. SonicTerm version and OS version.
2. The last 200 lines of the newest `sonicterm.log`.
3. The relevant crash dump or process sample, if any.
4. A screenshot or short recording for rendering, font, VT, input, or pane-layout issues.
5. Exact reproduction steps and whether the problem occurs on a hardware or software GPU.
6. For a memory report, the `pane retention` lines for the pane identified in
   [Triaging a session that feels heavy](#triaging-a-session-that-feels-heavy)
   and every `session retention` line over the same window — at least five
   consecutive samples, roughly three minutes of a session in use, not two
   lines far apart. Say what the session was doing, how many panes were open,
   and quote the pane's `largest_seam`.

Avoid posting secrets, tokens, environment dumps, or sensitive command output.

## 中文

> 规范开发诊断文档：
> [`docs/LOGGING.md`](https://github.com/D0n9X1n/SonicTerm/blob/main/docs/LOGGING.md)。

### 路径

- 当前日志：`~/.sonicterm/logs/sonicterm.log`
- 轮转日志：`~/.sonicterm/logs/sonicterm.log.*`
- 崩溃转储和退出追踪：`~/.sonicterm/logs/crashes/`

在 Windows 上，`~` 表示当前用户的配置文件目录。

### 配置

```toml
[logging]
level = "warn"          # error | warn | info | debug
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2
```

默认级别是 `warn`。SonicTerm 会先读取 `sonicterm.toml`，再安装 tracing
subscriber，因此正常启动阶段会直接使用配置的级别。诊断时可以通过 `RUST_LOG`
临时覆盖过滤器；stderr 仍以 warning 为下限，避免宽泛的 debug 过滤器刷满控制台。

清理任务在后台异步执行。默认删除两天以前的轮转日志和崩溃转储，最多保留三个轮转日志和
十个崩溃转储；保留策略不会删除当前活动日志。把对应的 age 设置为 `0` 可以关闭该类文件的
按年龄清理。

### 渲染与性能诊断

把日志级别改成 `debug` 后复现问题：

```toml
[logging]
level = "debug"
```

`render_timing` target 会记录每帧的网格遍历、overlay 组装、字形上传、surface 获取、
提交和 present 等阶段，并标明主窗口或子窗口，以及帧使用完整还是局部组装。没有单独的
render-timing 配置项。

启动时，渲染器会记录选中的 wgpu adapter、设备类型和是否检测为软件 adapter。在 RDP、
虚拟机或 VDI 中，请先查找 `software-render degrade engaged`，并结合
[配置 / Configuration](Configuration) 中的 `[appearance].software_render_mode` 排查。

### 内存诊断

将级别设为 `debug`，让会话运行一两分钟：

```toml
[logging]
level = "debug"
```

`memory` 目标每 30 秒采样一次每个面板占用的内存，为每个面板写入一行
`pane retention`，随后为整个进程写入一行 `session retention`。各数值分别
报告，因为对应的处理方式不同：

| 字段 | 含义 | 可采取的措施 |
| --- | --- | --- |
| `grid_visible_bytes` | 当前显示在屏幕上的单元格 | 无 — 这就是屏幕本身 |
| `grid_history_bytes` | 回滚缓冲 | 调低 `scrollback` |
| `grid_alternate_bytes` | 全屏程序背后保存的主屏幕 | 无 — 程序退出时自动释放 |
| `parser_bytes` | 当前正在解析的转义序列与内联媒体暂存缓冲 | 无 — 瞬时占用 |
| `hyperlink_bytes` | OSC 8 链接目标 | 无 — 链接滚出后自动回收 |
| `inline_media_bytes` | 已解码的内联图像 | 减少图像使用，或减少面板数量 |
| `pty_output_bytes` | 等待解析的 shell 输出所占用的读取缓冲区 | 无 — 终端处理完毕后自动释放 |
| `pty_input_bytes` | 等待送往 shell 的输入，通常来自大段粘贴 | 无 — shell 读取后自动释放 |

先看 `largest_seam`。总量只能说明面板占用大，而该字段指出应当检查哪个子系统。
占用 60 MB 内联图像的面板属于正常设计；占用 60 MB 网格的面板则不正常。

请比较连续多行 `session retention`，而不是只看单次采样。曲线的形状才能区分
「工作集趋于平稳」与「内存持续增长」。

并非每个数值都会趋于平稳，这是有意为之。回滚缓冲填满后，单元格占用达到稳态；
而暂存的超链接会持续增长直至上限，因为只要引用它们的单元格仍留在回滚历史中，
这些链接就仍可通过向上滚动访问。

### 排查内存偏高的会话

上一节说明每个数值的含义。本节给出的是当 SonicTerm 占用超出预期时，应当按什么
顺序去读这些数值，最终定位到可以写进报告里的具体面板和子系统。

**1. 先打开这些日志行。** 内存日志只在 `debug` 级别写入。在默认的 `warn`
以及 `info` 级别下完全不会出现，因此 grep 不到内容说明级别不对，而不是会话
没有问题。请在 `~/.sonicterm/sonicterm.toml`（Windows 上位于用户配置文件目录）
中设置级别，重启 SonicTerm，并正常使用几分钟：

```toml
[logging]
level = "debug"
```

**2. 找出占用最高的面板。** 日志会轮转为 `sonicterm.log.<date>`，繁忙的一天还
可能产生第二个文件，因此请按修改时间而不是文件名挑选最新的一个。下面的命令按
从大到小列出每个面板最近一次的总量：

```sh
LOG=$(ls -1t ~/.sonicterm/logs/sonicterm.log* | head -1)
grep 'pane retention' "$LOG" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^pane=/)        { p = substr($i, 6); gsub(/"/, "", p) }
    if ($i ~ /^total_bytes=/)   t = substr($i, 13)
  }
  last[p] = t
} END { for (p in last) printf "%10.2f MB  %s\n", last[p] / 1048576, p }' \
  | sort -rn
```

```powershell
$log = Get-ChildItem ~/.sonicterm/logs/sonicterm.log* |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1
Select-String 'pane retention' $log | ForEach-Object {
  if ($_.Line -match 'pane="([^"]+)".*?total_bytes=(\d+)') {
    [pscustomobject]@{ Pane = $Matches[1]; Bytes = [long]$Matches[2] }
  }
} | Group-Object Pane | ForEach-Object {
  $newest = $_.Group[-1]
  [pscustomobject]@{ MB = [math]::Round($newest.Bytes / 1MB, 2); Pane = $_.Name }
} | Sort-Object MB -Descending
```

标签由窗口 id 和面板 id 组成。标签页移动到另一个窗口时，面板 id 会跟随面板保持
不变，但前面的窗口 id 会改变 —— 因此如果某个面板在两次采样之间「消失」了，
请在不同的窗口 id 下查找相同的面板 id。

**3. 判断是哪个子系统。** 在占用最高的面板上读 `largest_seam`。总量只能说明
面板占用大，该字段指出应当检查哪里。每个取值都对应上一节字段表中的一行：

| `largest_seam` | 对应字段 | 首先可以尝试 |
| --- | --- | --- |
| `grid_visible` | `grid_visible_bytes` | 无 — 窗口大，占用自然大 |
| `grid_history` | `grid_history_bytes` | 调低 `scrollback` |
| `grid_alternate` | `grid_alternate_bytes` | 退出该面板中的全屏程序 |
| `parser` | `parser_bytes` | 暂时无需处理 — 在下次采样时复查 |
| `hyperlinks` | `hyperlink_bytes` | 无 — 有上限，链接滚出后自动回收 |
| `inline_media` | `inline_media_bytes` | 减少显示图像，或关闭图像较多的面板 |
| `pty_output` | `pty_output_bytes` | 等待该面板输出完毕 |
| `pty_input` | `pty_input_bytes` | 等待 shell 读完大段粘贴 |

注意 seam 取值是 `hyperlinks`，而字段名是 `hyperlink_bytes`；请按实际需要的
那个去 grep。

**4. 区分「占用大」与「持续增长」。** 这一步决定是否真的存在缺陷。单次采样无法
区分两者 —— 稳定占用 60 MB 图像的面板属于正常设计，而每次采样都在上涨的面板则
不是。把第 2 步得到的标签代入下面的命令，跟踪同一个面板：

```sh
PANE='pane="WindowId(50820809344)/3"'
grep 'pane retention' "$LOG" | grep -F "$PANE" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^total_bytes=/)    t = substr($i, 13)
    if ($i ~ /^largest_seam=/) { s = substr($i, 14); gsub(/"/, "", s) }
  }
  printf "%s  %8.2f MB  %s\n", substr($1, 12, 8), t / 1048576, s
}'
```

```powershell
$pane = 'WindowId(50820809344)/3'
$re = 'pane="' + [regex]::Escape($pane) + '".*?total_bytes=(\d+).*?largest_seam="([^"]+)"'
Select-String 'pane retention' $log | ForEach-Object {
  if ($_.Line -match $re) {
    '{0}  {1,8:N2} MB  {2}' -f $_.Line.Substring(11, 8), ([long]$Matches[1] / 1MB), $Matches[2]
  }
}
```

保持平稳就没有问题，即使是 60 MB：

```text
05:19:28     60.00 MB  inline_media
05:19:59     60.00 MB  inline_media
05:20:30     60.00 MB  inline_media
```

每次采样都稳定上涨才是值得上报的情况：

```text
05:19:28      8.00 MB  grid_history
05:19:59     16.00 MB  grid_history
05:20:30     24.00 MB  grid_history
05:21:02     32.00 MB  grid_history
```

**不要只比较首尾两行。** 采样被限制为大约每 30 秒一组，但它们是在空闲唤醒路径上
写出的，因此繁忙的会话记录得比空闲的会话更频繁。日志中的空档并不表示这段时间内存
是平的 —— 只表示期间没有唤醒去测量。相隔十分钟的两行之间，可能跨过了你看不到的
一次突发。请读连续多次采样，据此判断曲线形状。

**5. 查看进程整体。** 每个面板都可能在各自的上限之内，而总量并非如此。
`session retention` 行是各面板的求和，它带有 `panes=N` 而没有 `largest_seam`
—— 按 seam 的拆分只存在于单个面板。

```sh
grep 'session retention' "$LOG" | awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /^panes=/)        n = substr($i, 7)
    if ($i ~ /^total_bytes=/)  t = substr($i, 13)
  }
  printf "%s  panes=%-3s %8.2f MB\n", substr($1, 12, 8), n, t / 1048576
}'
```

```powershell
Select-String 'session retention' $log | ForEach-Object {
  if ($_.Line -match 'panes=(\d+) total_bytes=(\d+)') {
    '{0}  panes={1,-3} {2,8:N2} MB' -f $_.Line.Substring(11, 8), $Matches[1], ([long]$Matches[2] / 1MB)
  }
}
```

如果 `panes` 与总量一起上升，说明会话增长是因为打开了更多面板，属于预期行为。
如果 `panes` 保持不变而总量持续上涨，请回到第 2 步，找出是哪个面板导致的。

**6. 两条不代表缺陷的警告。** 这两行在默认的 `warn` 级别下也会出现，且都表示
SonicTerm 已经纠正了某个状况，而不是出了故障。仅仅看到它们并不值得上报：

- `cancelled a media capture that stopped receiving; the transfer was abandoned and its staging is reclaimed` —— 图像传输中途停止，其缓冲已被释放，而不是被占用到面板结束为止。
- `revisited idle panes holding an inline-media budget sized for a smaller session` —— 早期填满的面板仍持有面板数较少时分得的图像预算份额，现已归还。

### 内联图像消失时

SonicTerm 对已解码的内联图像按面板和整个进程分别设限。持续显示新图像的面板
最终会丢弃最旧的图像；打开多个面板时，每个面板获得进程预算的一份份额，而非
完整的单面板额度。每个面板始终至少保留最新的一张图像。

这是有意行为，并且会记录日志：查找 `warn` 级别的
`inline media evicted to hold the process-wide ceiling`。若图像消失而该行缺失，
则属于值得上报的缺陷。

超链接的行为不同。链接表填满时，SonicTerm 会回收其文本已滚出历史记录的条目，
使新链接继续可用。仍显示在屏幕上的链接永远不会被回收。

### 崩溃与卡死诊断

panic hook 会把 panic 内容、源码位置、backtrace 和最近一小段 tracing 事件写入崩溃目录。
正常事件循环退出还会记录 `sonic_exit`。UI 死锁或卡死不一定产生 panic 或 crash 文件。

macOS 上请在强制退出前抓取进程 sample：

```sh
sample <pid> 10 -file /tmp/sonicterm-hang.sample.txt
grep -nE 'dispatch_sync_f_slow|redraw_target|__psynch_cvwait' \
  /tmp/sonicterm-hang.sample.txt
```

如果问题与拖出的子窗口有关，还应说明：是否有多个窗格持续输出、其它窗口是否仍有响应，
以及关闭窗口后对应窗格的子进程是否退出。

### Bug 报告材料

请附上：

1. SonicTerm 版本和操作系统版本。
2. 最新 `sonicterm.log` 的最后 200 行。
3. 相关崩溃转储或进程 sample（如果存在）。
4. 渲染、字体、VT、输入或窗格布局问题的截图或短录屏。
5. 精确复现步骤，以及问题发生在硬件 GPU 还是软件 GPU 上。
6. 内存问题请附上：在
   [排查内存偏高的会话](#排查内存偏高的会话) 中定位到的那个面板的
   `pane retention` 行，以及同一时间段内所有 `session retention` 行 ——
   至少五次连续采样，约相当于三分钟的实际使用，而不是相隔很远的两行。
   同时说明当时会话在做什么、打开了多少个面板，并附上该面板的 `largest_seam`。

不要公开密钥、token、完整环境变量或敏感命令输出。
