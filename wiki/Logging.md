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

不要公开密钥、token、完整环境变量或敏感命令输出。
