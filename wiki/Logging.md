# Logging / 日志

## English

This page is the canonical guide to SonicTerm logs, retention, performance and
memory diagnostics, crash evidence, and bug-report data.

## Paths

- Log files: `~/.sonicterm/logs/sonicterm.log.*`
- Fatal-signal fallback path: `~/.sonicterm/logs/sonicterm.log`
- Panic artifacts: `~/.sonicterm/logs/crashes/`
- Session markers: `~/.sonicterm/logs/sessions/`
- Bounded breadcrumbs: `~/.sonicterm/logs/breadcrumbs/`

`tracing-appender` uses daily names such as `sonicterm.log.YYYY-MM-DD`; the file
with the newest modification time is active. Size rotation may add a Unix-time
suffix. On Windows, `~` means the current user's profile directory. Native
runtime smokes on macOS, Windows, and Linux use the explicit `logs/` child of
`SONICTERM_RUNTIME_SMOKE_DIR` instead of the user log tree; their separate
`config/` child is used for config/reload state, and `HOME` is preserved. The
outer runner removes inherited `NO_COLOR` and retains failed stdout/stderr plus
SonicTerm logs for CI artifacts.

## Configuration and retention

```toml
[logging]
level = "warn"                    # error | warn | info | debug
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2
max_crash_bytes = 10485760        # 10 MiB
max_breadcrumb_files = 10
max_breadcrumb_age_days = 2
max_breadcrumb_bytes = 1048576    # 1 MiB
```

`warn` is the default. SonicTerm reads `[logging]` before installing the tracing
subscriber, so the configured level applies to normal startup. `RUST_LOG`
overrides the configured filter for one run. Stderr follows that same filter
with an added global `warn` fallback; more specific target directives can still
emit `debug` lines there, so `warn` is not a hard stderr ceiling.

Before the appender opens, SonicTerm rotates an active log over
`max_file_size_mb` unless the value is `0`, then applies age and count limits to
older log files. The active file is never deleted. Crash and breadcrumb
artifacts are each bounded independently by count, age, and aggregate bytes;
oldest files are removed until all enabled limits are satisfied. Setting an age
or aggregate-byte limit to `0` disables that axis. Cleanup is fail-soft and
artifact cleanup runs on a background thread.

## Levels and diagnostic targets

| Level | What it admits |
| --- | --- |
| `error` | errors only |
| `warn` | warnings, errors, `sonic_exit`, and user-visible reclamation/exhaustion warnings |
| `info` | normal SonicTerm information plus the aggregate `memory snapshot` |
| `debug` | detailed SonicTerm diagnostics, pane/renderer memory lines, state-machine events, `render_timing`, and `tear_out_timing` |

`wgpu`, `naga`, `sonicterm-vt`, and `sonicterm-grid` remain warning-oriented in
the configured filters. Very hot font-shaper dumps are `trace`; no configured
level admits them. Use a targeted `RUST_LOG` directive only when investigating
that path.

## PTY input rejection diagnostics

The default `warn` level reports input that was refused, including terminal
parser replies. The producer assigns `source`: `Keyboard`, `Paste`, `FileDrop`,
`Ime`, `PointerButton`, `PointerMotion`, `Wheel`, `FocusReport`, `TerminalReply`,
`ScriptDraft`, or `StateMachine`. Sources are never guessed from payload bytes.

| Field | Meaning |
| --- | --- |
| `pane_id` | Stable pane identity supplied at the producer |
| `window_id` | The pane's current window when the event loop handles the rejection; absent after pane closure or when the event loop is unavailable |
| `source`, `rejected_bytes`, `reason` | Input category, refused byte length, and payload-free reason |
| `observation="concurrent"` | Queue and writer fields are independent observations, not one rejection-time transaction |
| `queued_messages`, `queued_bytes`, `queue_capacity` | Waiting message count, payload byte count, and four-slot limit; excludes the active native write |
| `writer_phase` | `Idle`, `Writing`, `Flushing`, or `Stopped`; a boundary observation, not a child-health verdict |
| `in_flight_bytes`, `in_flight_millis` | Active message size and time spent in the observed write or flush; time is absent when idle/stopped |
| `completed_messages` | Successful `write_all` calls whose best-effort flush attempt returned |

The event carries no rejected payload. Its debug representation, warnings, and
notification never include typed text, commands, paths, or clipboard content.
Notification follows the pane's current window after tab transfers; a closed
pane produces a warning but no notification on an unrelated window. When the
proxy is absent or event delivery fails, the producer logs the same metadata
without a current-window identity.

Each pane's parser-reply worker posts at most one rejection notification during
its lifetime. Further refused replies are counted in fixed-size metadata and
logged at most once per second, with a final flush when the worker stops. The
summary fields `rejected_messages`, `rejected_bytes`, `queue_full`,
`message_too_large`, and `writer_disconnected` count additional refusals since
the previous summary; they exclude the first, individually reported rejection.
Idle workers with no pending summary do not wake periodically. This bounds
background diagnostic traffic without hiding loss or retaining reply payloads.

Four small messages can fill the channel before a healthy writer is scheduled.
Controlled tests use the production admission and writer loop to demonstrate
that burst draining preserves order. Separate blocked-write and blocked-flush
fixtures demonstrate zero queued bytes with one in-flight message, followed by
four additional queued messages and explicit refusal of the next message.
These fixtures distinguish mechanisms; they do not retrospectively identify
which producer or native condition caused an older un-attributed warning.

Queue capacity, per-message limits, FIFO delivery, and cancellation are
unchanged. Overload remains observable; no input is automatically replayed,
coalesced, or admitted by enlarging the queue. Interpret repeated observations
of the same pane and progress counter rather than a single `QueueFull` warning.

## Render and performance diagnostics

Set `level = "debug"`, restart, and reproduce the problem. The
`render_timing` target records frame phases including grid walking, overlay
assembly, glyph upload, surface acquisition, submission, and presentation. It
identifies main or child renderers and full or partial assembly. There is no
separate render-timing option.

Startup logs the selected wgpu adapter, device type, and software-adapter
classification. On RDP, VM, or VDI hosts, look for `software-render degrade
engaged` and compare it with `[appearance].software_render_mode` on
[Configuration](Configuration). At `level = "debug"`, each renderer also writes
`renderer LCD subpixel policy` at startup and whenever mode, opacity, theme, or
presenter state changes. Its `requested`, `effective`, `windows_host`,
`opaque_target`, `software_presenter`, and `dual_source_supported` fields explain
every LCD-to-grayscale fallback without relying on a screenshot.

## Memory diagnostics

### Aggregate snapshot at `info`

At `level = "info"`, `target="memory"` writes one `memory snapshot` at most
every 30 seconds. It combines OS process figures, all sampled pane seams, visible
and warm renderers, and one shared-device allocator reading:

```text
memory snapshot process_private_committed_bytes=unsupported process_resident_bytes=412876800
                process_virtual_bytes=419923525632 process_private_committed_delta=unavailable
                process_resident_delta=+1841152 process_virtual_delta=+0
                session_total_bytes=182451840 session_delta=+1048576
                grid_visible_bytes=97320960 grid_history_bytes=76841472 grid_alternate_bytes=0
                parser_bytes=0 hyperlink_bytes=3050880 inline_media_bytes=5238528
                pty_output_bytes=0 pty_input_bytes=0 panes_total=12 panes_sampled=12 panes_contended=0
                renderer_total_bytes=36962304 renderer_total_items=1242
                renderer_row_glyph_cache_bytes=1048576 renderer_row_glyph_cache_items=120
                renderer_row_quad_cache_bytes=262144 renderer_row_quad_cache_items=80 renderer_delta=+0
                live_renderers=2 renderers="visible[WindowId(1)] glyph=16777216/1038 image=2097152/4 row_glyph=1048576/120 row_quad=262144/80 software=0/0 total=20185088/1242; warm[0] glyph=16777216/0 image=0/0 row_glyph=0/0 row_quad=0/0 software=0/0 total=16777216/0"
                allocator_state=measured allocator_source=main allocator_label=WindowId(1)
                allocator_allocated_bytes=8388608 allocator_reserved_bytes=33554432
                allocator_allocations=4 allocator_blocks=2 allocator_largest_block_bytes=16777216
```

Process figures come from the OS, so they include allocator fragmentation,
retired pages, mapped files, GPU-driver mappings, and thread stacks that
SonicTerm's own seams do not count.

| Field | Meaning |
| --- | --- |
| `process_private_committed_bytes` | memory charged to this process alone; Windows reports `PrivateUsage`, macOS and Linux report `unsupported` |
| `process_resident_bytes` | pages currently resident in physical memory; macOS reports resident size, Windows reports `WorkingSetSize`, and Linux reports `unsupported` |
| `process_virtual_bytes` | reserved address space; macOS and Windows measure it, Linux reports `unsupported`; measured values can be hundreds of gigabytes without representing consumed memory |
| `*_delta` | change since the preceding snapshot; `+0` is measured, while `unavailable` means no comparable sample |
| `panes_total` | all panes visited |
| `panes_sampled` | panes included in `session_total_bytes` |
| `panes_contended` | panes skipped because a parser lock was held; non-zero makes the session total partial |
| `renderer_total_bytes` / `renderer_total_items` | CPU-side storage across visible and warm renderers |
| `renderer_row_glyph_cache_bytes` / `renderer_row_glyph_cache_items` | per-row glyph-instance and decoration cache storage and cached row count across renderers |
| `renderer_row_quad_cache_bytes` / `renderer_row_quad_cache_items` | per-row background/decoration quad cache storage and cached row count across renderers |
| `live_renderers` | process-wide renderer count; a count above the `renderers` entries can expose an unreachable live renderer |
| `renderers` | per-renderer role and glyph/image/row-cache/software storage breakdown |
| `allocator_state` | `measured`, `unsupported` for a backend without a report, or `none` before a renderer exists |
| `allocator_source` / `allocator_label` | renderer class and identifier used for the one shared-device reading |
| `allocator_allocated_bytes` | bytes assigned to live wgpu allocations |
| `allocator_reserved_bytes` | bytes reserved in wgpu allocator blocks |
| `allocator_allocations` | live allocation count |
| `allocator_blocks` | allocator block count |
| `allocator_largest_block_bytes` | largest allocator block in bytes |

The allocator is reported once per shared device/context, not once per renderer.
Sampling shares the retention cadence. An idle session wakes for a due sample,
but that wake suppresses redraw and draws no frame.

### Pane and session detail at `debug`

At `level = "debug"`, the same cadence writes one `pane retention` line for each
sampled pane, then one `session retention` line. A contended pane is skipped, not
waited on.

```text
pane retention pane="WindowId(1)/7" total_bytes=15204320
               grid_visible_bytes=8110080 grid_history_bytes=6405120
               grid_alternate_bytes=0 parser_bytes=0 hyperlink_bytes=254240
               inline_media_bytes=434880 pty_output_bytes=0 pty_input_bytes=0
               largest_seam="grid_visible" largest_seam_bytes=8110080
session retention panes=12 total_bytes=182451840 grid_visible_bytes=97320960
                  grid_history_bytes=76841472 grid_alternate_bytes=0 parser_bytes=0
                  hyperlink_bytes=3050880 inline_media_bytes=5238528
                  pty_output_bytes=0 pty_input_bytes=0
```

The eight seam fields are disjoint and sum to `total_bytes`:

| Field | What it owns | First response |
| --- | --- | --- |
| `grid_visible_bytes` | visible primary-grid cells and rows | no action; this is the screen |
| `grid_history_bytes` | retained scrollback | lower `scrollback` if needed |
| `grid_alternate_bytes` | saved alternate-screen storage | leave the full-screen program |
| `parser_bytes` | in-flight escape or media-capture buffers | recheck the next sample; normally transient |
| `hyperlink_bytes` | interned OSC 8 URI and id strings | no action; bounded and reclaimed when links leave retained history |
| `inline_media_bytes` | decoded inline images retained by panes | display fewer images or close image-heavy panes |
| `pty_output_bytes` | local PTY output queued or in flight | let output drain |
| `pty_input_bytes` | input queued toward the shell, usually a large paste | let the shell drain it |

Read `largest_seam` first, then compare at least five consecutive samples. A
large flat working set is different from a value that rises every sample. Pane
labels contain window and pane identifiers; a pane keeps its pane id after a tab
moves even though the window id changes.

### Renderer detail at `debug`

Each visible or warm renderer also writes one `renderer retention` line:

```text
renderer retention window="WindowId(1)" role="visible" total_bytes=18612224
                   glyph_atlas_bytes=16777216 glyph_atlas_items=412
                   image_atlas_bytes=524288 image_atlas_items=3
                   row_glyph_cache_bytes=1048576 row_glyph_cache_items=120
                   row_quad_cache_bytes=262144 row_quad_cache_items=80 software_frame_bytes=0
renderer retention window="warm[0]" role="warm" total_bytes=16777216
                   glyph_atlas_bytes=16777216 glyph_atlas_items=0
                   image_atlas_bytes=0 image_atlas_items=0
                   row_glyph_cache_bytes=0 row_glyph_cache_items=0
                   row_quad_cache_bytes=0 row_quad_cache_items=0 software_frame_bytes=0
```

| Field | What it owns | First response |
| --- | --- | --- |
| `glyph_atlas_bytes` | CPU glyph-atlas capacity for this renderer | bounded; a warm entry is controlled by `warm_window_pool` |
| `glyph_atlas_items` | glyph entries in that atlas | use with bytes to distinguish occupancy from capacity |
| `image_atlas_bytes` | CPU-side inline-image atlas pixels | reduce image use or renderer count |
| `image_atlas_items` | inline-image atlas entries | use with bytes to identify image occupancy |
| `row_glyph_cache_bytes` | hash-table backing plus cached glyph, underline, tofu, and missing-character vector capacities | compare with cached rows; pane departure releases that pane's payload while table capacity can remain at its high-water mark |
| `row_glyph_cache_items` | cached glyph rows | a falling count with flat bytes can mean reusable table capacity remains |
| `row_quad_cache_bytes` | hash-table backing plus cached background/decoration quad vector capacities | compare with cached rows and pane/window churn |
| `row_quad_cache_items` | cached quad rows | a falling count confirms row eviction even when table capacity is sticky |
| `software_frame_bytes` | full-window Windows software-present buffer | reduce window size; zero outside that path |

`role="warm"` means the renderer belongs to the standby pool, not a visible
window; closing a window does not release it. Renderer figures are host memory,
not GPU video memory.

### Reclamation warnings

The default `warn` filter admits warnings on `memory::reclaimed` because they
explain visible loss:

```sh
grep 'memory::reclaimed' ~/.sonicterm/logs/sonicterm.log*
```

- `cancelled a media capture that stopped receiving` means no bytes arrived for
  two 30-second samples. The incomplete image will not appear and staging was
  reclaimed.
- `discarded inline images from idle panes to stay within the process ceiling`
  means older decoded images were removed; resend any still needed.

The default filter also admits `inline image atlas full; skipped older images
without evicting text glyphs` on `sonic::glyph_atlas`; renderer atlas pressure
prevented older images from being uploaded. `inline media evicted to hold the
process-wide ceiling` is a warning on the `memory` target and therefore appears
when `level` is `info` or `debug`; it means a pane removed older images while
retaining at least its newest image.

## Crash, hang, and exit evidence

The panic hook runs on every thread and writes a session-tagged
`crashes/crash-<timestamp>.log` containing version, panic payload, source
location, forced backtrace, and the latest 50 tracing events. Normal shutdown
writes `sonic_exit` warning lines. On Unix, SIGSEGV, SIGBUS, SIGILL, SIGABRT, and
SIGFPE append a fixed `FATAL: SIG…` line through an async-signal-safe path, then
re-raise the signal for OS diagnostics. Windows relies on WER or LocalDumps when
the system is configured to create them.

A hang may produce no panic artifact. On macOS, sample before force-quitting:

```sh
sample <pid> 10 -file /tmp/sonicterm-hang.sample.txt
grep -nE 'dispatch_sync_f_slow|redraw_target|__psynch_cvwait' \
  /tmp/sonicterm-hang.sample.txt
```

`SIGKILL`, Force Quit, `TerminateProcess`, power loss, and a hard OOM run no
cleanup code. SonicTerm cannot write a final line or post-failure dump in those
cases. Instead it leaves two pre-failure records:

1. `sessions/session-<id>.marker` records only session id, pid, version,
   platform, start time, and state. A stale marker proves shutdown was not
   reached; it does not identify the cause. Live sibling processes are skipped,
   damaged markers still count as evidence, and each prior marker is reported
   once on the next launch.
2. `breadcrumbs/breadcrumbs-<id>.log` is a bounded atomic snapshot with no
   terminal text, commands, environment values, tokens, or credentials. It pins
   the latest version, platform, renderer, counts, full process resource sample,
   retention, allocator state, and bounded lifecycle transitions. Its
   `event=retention` record includes `renderer_bytes`,
   `row_glyph_cache_bytes` / `row_glyph_cache_items`, and
   `row_quad_cache_bytes` / `row_quad_cache_items`, so the last complete
   pre-failure snapshot preserves both row-cache size and occupancy. A separate
   fixed-cost `event=resource_history private_committed=... resident=...` sample
   is taken immediately and every 5 seconds, retaining at most 48 samples.
   Virtual address space appears only in the full `event=resource` record.

A breadcrumb rewrite replaces the old file atomically. The surviving file after
a hard kill is the latest complete pre-failure snapshot, not a dump and not
proof of cause. The default file budget is 64 KiB; configured limits must be able
to hold all mandatory records, lifecycle capacity, and one maximum-width history
line. The absolute minimum is 4096 bytes.

On the next launch, SonicTerm also checks OS records by conservative filename
convention:

| Platform | Locations checked |
| --- | --- |
| macOS | `~/Library/Logs/DiagnosticReports`, `/Library/Logs/DiagnosticReports` (`.ips`) |
| Windows | `%LOCALAPPDATA%\CrashDumps`, `%LOCALAPPDATA%\Microsoft\Windows\WER\ReportQueue`, `...\ReportArchive` |

A match only “may relate to” SonicTerm. The Windows check does not read WER
registry configuration, so no file means only that the standard locations held
no match.

## Bug-report bundle

Include:

1. SonicTerm and OS versions.
2. The last 200 lines of the newest `sonicterm.log*` file.
3. The relevant panic artifact, OS record, or process sample, if present.
4. Exact reproduction steps and a screenshot or short recording for visual,
   input, VT, font, or layout defects.
5. Hardware/software adapter information for rendering problems.
6. For memory growth, at least five consecutive `memory snapshot` records; with
   `debug`, also include the identified pane's `pane retention` lines and all
   `session retention` and relevant `renderer retention` lines over the same
   interval. State the pane's `largest_seam` and what the session was doing.

Do not post secrets, tokens, full environment dumps, terminal output, or
sensitive command data.

## 中文

本页是 SonicTerm 日志、保留策略、性能与内存诊断、崩溃证据和缺陷报告材料的规范说明。

## 路径

- 日志文件：`~/.sonicterm/logs/sonicterm.log.*`
- 致命信号备用路径：`~/.sonicterm/logs/sonicterm.log`
- Panic 工件：`~/.sonicterm/logs/crashes/`
- 会话标记：`~/.sonicterm/logs/sessions/`
- 有界诊断记录：`~/.sonicterm/logs/breadcrumbs/`

`tracing-appender` 按天生成 `sonicterm.log.YYYY-MM-DD` 之类的文件；修改时间最新的
文件正在使用。按大小轮转时还可能增加 Unix 时间后缀。在 Windows 上，`~` 表示当前
用户的配置文件目录。macOS、Windows 与 Linux 原生运行时 smoke 使用
`SONICTERM_RUNTIME_SMOKE_DIR` 下显式的 `logs/` 子目录，不写入用户日志树；分开的
`config/` 子目录承载配置与重载状态，并保留原有 `HOME`。外层 runner 会移除继承的
`NO_COLOR`，并保存失败输出和日志证据。

## 配置与保留策略

```toml
[logging]
level = "warn"                    # error | warn | info | debug
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2
max_crash_bytes = 10485760        # 10 MiB
max_breadcrumb_files = 10
max_breadcrumb_age_days = 2
max_breadcrumb_bytes = 1048576    # 1 MiB
```

默认级别为 `warn`。SonicTerm 会先读取 `[logging]`，再安装 tracing subscriber，
因此正常启动会直接采用配置级别。`RUST_LOG` 可在单次运行中覆盖配置过滤器。stderr
使用同一过滤器，并额外加入全局 `warn` fallback；更具体的 target 指令仍可让它输出
`debug`，所以 `warn` 不是 stderr 的硬上限。

打开 appender 前，SonicTerm 会轮转超过 `max_file_size_mb` 的当前日志；设为 `0`
可关闭这条限制。随后按年龄和数量清理旧日志，当前文件永不删除。崩溃工件和诊断记录
分别受数量、年龄与总字节数限制，并从最旧文件开始删除，直到所有启用的限制都满足。
年龄或总字节数设为 `0` 只会关闭对应限制。清理失败不会阻止程序启动，工件清理在后台
线程执行。

## 级别与诊断 target

| 级别 | 会记录的内容 |
| --- | --- |
| `error` | 仅错误 |
| `warn` | warning、error、`sonic_exit`，以及用户可见的回收或耗尽提示 |
| `info` | SonicTerm 常规信息和聚合 `memory snapshot` |
| `debug` | 详细诊断、窗格/渲染器内存、状态机事件、`render_timing` 和 `tear_out_timing` |

配置过滤器始终把 `wgpu`、`naga`、`sonicterm-vt` 和 `sonicterm-grid` 保持在 warning
级别。字体塑形热路径的海量输出位于 `trace`，任何配置级别都不会启用；只有专门排查该
路径时才使用精确的 `RUST_LOG` 指令。

## PTY 输入拒绝诊断

默认 `warn` 级别会报告被拒绝的输入，包括终端解析器回复。生产者直接指定 `source`：
`Keyboard`、`Paste`、`FileDrop`、`Ime`、`PointerButton`、`PointerMotion`、`Wheel`、
`FocusReport`、`TerminalReply`、`ScriptDraft` 或 `StateMachine`；不会从负载字节猜测来源。

| 字段 | 含义 |
| --- | --- |
| `pane_id` | 生产者提供的稳定窗格标识 |
| `window_id` | 事件循环处理拒绝时窗格所属的当前窗口；窗格关闭或事件循环不可用时为空 |
| `source`、`rejected_bytes`、`reason` | 输入类别、被拒绝的字节数及不含负载的原因 |
| `observation="concurrent"` | 队列与 writer 字段为独立并发观察值，不是拒绝瞬间的同一事务快照 |
| `queued_messages`、`queued_bytes`、`queue_capacity` | 等待的消息数、负载字节数和四槽上限；不包含正在进行的原生写入 |
| `writer_phase` | `Idle`、`Writing`、`Flushing` 或 `Stopped`；表示执行边界，不是子进程健康结论 |
| `in_flight_bytes`、`in_flight_millis` | 当前消息大小及已观察写入或 flush 的持续时间；空闲或停止时无时间值 |
| `completed_messages` | `write_all` 成功且尽力而为的 flush 尝试已返回的消息数 |

事件不携带被拒绝的负载。其 debug 表示、warning 和通知均不包含输入文本、命令、路径或剪贴板内容。
标签页转移后，通知跟随窗格的当前窗口；已关闭的窗格只记录 warning，不在无关窗口显示通知。
代理缺失或事件投递失败时，生产者直接记录同样的元数据，但不附带无法确认的当前窗口标识。

每个窗格的解析器回复 worker 在其整个生命周期内最多发送一次拒绝通知。后续被拒绝的回复
使用固定大小的元数据计数，每秒最多记录一次，并在 worker 停止时输出最后一批。
汇总字段 `rejected_messages`、`rejected_bytes`、`queue_full`、`message_too_large` 和
`writer_disconnected` 统计自上次汇总以来额外发生的拒绝，不包含首次单独报告的拒绝。
没有待汇总数据的空闲 worker 不会周期唤醒；这样既限制后台诊断流量，也不隐藏输入丢失或保留回复负载。

健康 writer 尚未被调度时，四条小消息就能填满通道。受控测试使用生产环境的入队与 writer 循环，
证明突发排空仍保持顺序。另设阻塞写入和阻塞 flush 的夹具，证明排队字节为零时仍可能有一条
在途消息；再加入四条等待消息后，下一条消息会被显式拒绝。这些夹具区分机制，不会倒推出
以前缺少归属信息的 warning 究竟由哪个生产者或原生条件引起。

队列容量、单消息上限、FIFO 交付和取消行为不变。过载仍可观察；不会自动重放、合并输入，
也不会扩大队列来接纳它。应比较同一窗格的连续观察值和进度计数，而不是凭一条 `QueueFull`
warning 下结论。

## 渲染与性能诊断

把 `level` 设为 `debug`，重启后复现问题。`render_timing` target 会记录网格遍历、
覆盖层组装、字形上传、surface 获取、提交和呈现等帧阶段，并区分主窗口与子窗口、完整
与局部组装。没有单独的渲染计时开关。

启动日志会记录选中的 wgpu adapter、设备类型和软件 adapter 分类。在 RDP、虚拟机或
VDI 环境中，请查找 `software-render degrade engaged`，并对照[配置](Configuration)中的
`[appearance].software_render_mode`。在 `level = "debug"` 下，每个 renderer 还会在启动以及
模式、opacity、主题或 presenter 状态变化时写入 `renderer LCD subpixel policy`。其中的
`requested`、`effective`、`windows_host`、`opaque_target`、`software_presenter` 和
`dual_source_supported` 字段能解释每次 LCD 到灰度的回退，不必依赖截图推断。

## 内存诊断

### `info` 级别的聚合快照

在 `level = "info"` 下，`target="memory"` 最多每 30 秒写一条 `memory snapshot`。
它汇总操作系统进程数据、所有已采样窗格接缝、可见与预热渲染器，以及一次共享设备
分配器读取：

```text
memory snapshot process_private_committed_bytes=unsupported process_resident_bytes=412876800
                process_virtual_bytes=419923525632 process_private_committed_delta=unavailable
                process_resident_delta=+1841152 process_virtual_delta=+0
                session_total_bytes=182451840 session_delta=+1048576
                grid_visible_bytes=97320960 grid_history_bytes=76841472 grid_alternate_bytes=0
                parser_bytes=0 hyperlink_bytes=3050880 inline_media_bytes=5238528
                pty_output_bytes=0 pty_input_bytes=0 panes_total=12 panes_sampled=12 panes_contended=0
                renderer_total_bytes=36962304 renderer_total_items=1242
                renderer_row_glyph_cache_bytes=1048576 renderer_row_glyph_cache_items=120
                renderer_row_quad_cache_bytes=262144 renderer_row_quad_cache_items=80 renderer_delta=+0
                live_renderers=2 renderers="visible[WindowId(1)] glyph=16777216/1038 image=2097152/4 row_glyph=1048576/120 row_quad=262144/80 software=0/0 total=20185088/1242; warm[0] glyph=16777216/0 image=0/0 row_glyph=0/0 row_quad=0/0 software=0/0 total=16777216/0"
                allocator_state=measured allocator_source=main allocator_label=WindowId(1)
                allocator_allocated_bytes=8388608 allocator_reserved_bytes=33554432
                allocator_allocations=4 allocator_blocks=2 allocator_largest_block_bytes=16777216
```

进程数据来自操作系统，因此包含 SonicTerm 自身接缝未统计的分配器碎片、尚未归还的页、
映射文件、GPU 驱动映射和线程栈。

| 字段 | 含义 |
| --- | --- |
| `process_private_committed_bytes` | 仅归本进程的内存；Windows 使用 `PrivateUsage`，macOS 与 Linux 报 `unsupported` |
| `process_resident_bytes` | 当前位于物理内存中的页；macOS 报常驻大小，Windows 报 `WorkingSetSize`，Linux 报 `unsupported` |
| `process_virtual_bytes` | 保留的地址空间；macOS 与 Windows 可测量，Linux 报 `unsupported`；实测值达到数百 GB 也可能正常，并不代表实际占用 |
| `*_delta` | 相比上次快照的变化；`+0` 是实测，`unavailable` 表示没有可比较样本 |
| `panes_total` | 本轮访问的全部窗格 |
| `panes_sampled` | 计入 `session_total_bytes` 的窗格 |
| `panes_contended` | 因解析器锁被占用而跳过的窗格；非零表示会话总量不完整 |
| `renderer_total_bytes` / `renderer_total_items` | 所有可见与预热渲染器的 CPU 存储 |
| `renderer_row_glyph_cache_bytes` / `renderer_row_glyph_cache_items` | 所有渲染器的逐行字形实例与装饰缓存存储及缓存行数 |
| `renderer_row_quad_cache_bytes` / `renderer_row_quad_cache_items` | 所有渲染器的逐行背景/装饰 quad 缓存存储及缓存行数 |
| `live_renderers` | 进程级渲染器数量；若高于 `renderers` 条目数，可能存在仍存活但无法访问的渲染器 |
| `renderers` | 各渲染器角色及字形/图像/行缓存/软件帧存储明细 |
| `allocator_state` | `measured`、后端不支持报告时的 `unsupported`，或还没有渲染器时的 `none` |
| `allocator_source` / `allocator_label` | 这次共享设备读取所用的渲染器类别和标识 |
| `allocator_allocated_bytes` | 分配给存活 wgpu allocation 的字节数 |
| `allocator_reserved_bytes` | wgpu 分配器 block 中保留的字节数 |
| `allocator_allocations` | 存活 allocation 数量 |
| `allocator_blocks` | 分配器 block 数量 |
| `allocator_largest_block_bytes` | 最大分配器 block 的字节数 |

共享设备/context 的分配器只报告一次，不会按每个渲染器重复。采样沿用保留量节奏；
空闲会话会为到期采样唤醒，但该次唤醒会抑制重绘，不绘制任何帧。

### `debug` 级别的窗格与会话明细

在 `level = "debug"` 下，同一周期会为每个已采样窗格写一条 `pane retention`，
随后写一条 `session retention`。锁被占用的窗格会被跳过，不会等待。

```text
pane retention pane="WindowId(1)/7" total_bytes=15204320
               grid_visible_bytes=8110080 grid_history_bytes=6405120
               grid_alternate_bytes=0 parser_bytes=0 hyperlink_bytes=254240
               inline_media_bytes=434880 pty_output_bytes=0 pty_input_bytes=0
               largest_seam="grid_visible" largest_seam_bytes=8110080
session retention panes=12 total_bytes=182451840 grid_visible_bytes=97320960
                  grid_history_bytes=76841472 grid_alternate_bytes=0 parser_bytes=0
                  hyperlink_bytes=3050880 inline_media_bytes=5238528
                  pty_output_bytes=0 pty_input_bytes=0
```

八个接缝互不重叠，相加等于 `total_bytes`：

| 字段 | 归属内容 | 首先处理 |
| --- | --- | --- |
| `grid_visible_bytes` | 可见主网格的单元格和行 | 无需处理，这是屏幕本身 |
| `grid_history_bytes` | 保留的回滚缓冲 | 必要时调低 `scrollback` |
| `grid_alternate_bytes` | 保存的备用屏幕 | 退出全屏程序 |
| `parser_bytes` | 处理中的转义序列或媒体捕获缓冲 | 下一次采样复查；通常只是瞬时占用 |
| `hyperlink_bytes` | 驻留的 OSC 8 URI 与 id 字符串 | 无需处理；有上限，引用离开保留历史后回收 |
| `inline_media_bytes` | 窗格保留的已解码内联图像 | 减少图像或关闭图像较多的窗格 |
| `pty_output_bytes` | 已排队或传输中的本地 PTY 输出 | 等待输出排空 |
| `pty_input_bytes` | 等待送往 shell 的输入，通常是大段粘贴 | 等待 shell 读取 |

先读 `largest_seam`，再比较至少五次连续采样。数值很大但保持平稳，与每次采样都增长
不是同一问题。窗格标签包含窗口 id 和窗格 id；标签页移动后窗格 id 不变，窗口 id 会变。

### `debug` 级别的渲染器明细

每个可见或预热渲染器还会写一条 `renderer retention`：

```text
renderer retention window="WindowId(1)" role="visible" total_bytes=18612224
                   glyph_atlas_bytes=16777216 glyph_atlas_items=412
                   image_atlas_bytes=524288 image_atlas_items=3
                   row_glyph_cache_bytes=1048576 row_glyph_cache_items=120
                   row_quad_cache_bytes=262144 row_quad_cache_items=80 software_frame_bytes=0
renderer retention window="warm[0]" role="warm" total_bytes=16777216
                   glyph_atlas_bytes=16777216 glyph_atlas_items=0
                   image_atlas_bytes=0 image_atlas_items=0
                   row_glyph_cache_bytes=0 row_glyph_cache_items=0
                   row_quad_cache_bytes=0 row_quad_cache_items=0 software_frame_bytes=0
```

| 字段 | 归属内容 | 首先处理 |
| --- | --- | --- |
| `glyph_atlas_bytes` | 该渲染器的 CPU 字形图集容量 | 有上限；预热条目由 `warm_window_pool` 控制 |
| `glyph_atlas_items` | 图集中的字形条目数 | 与字节数一起判断实际占用与容量 |
| `image_atlas_bytes` | CPU 侧内联图像图集像素 | 减少图像或渲染器数量 |
| `image_atlas_items` | 内联图像图集条目数 | 与字节数一起识别图像占用 |
| `row_glyph_cache_bytes` | 哈希表后备存储，以及缓存字形、下划线、tofu 与缺失字符向量的容量 | 与缓存行数对照；窗格离开时释放其负载，但表容量可能保持高水位 |
| `row_glyph_cache_items` | 已缓存的字形行数 | 行数下降而字节不变，可能表示可复用表容量仍保留 |
| `row_quad_cache_bytes` | 哈希表后备存储，以及缓存背景/装饰 quad 向量的容量 | 与缓存行数及窗格/窗口变化对照 |
| `row_quad_cache_items` | 已缓存的 quad 行数 | 即使表容量有粘性，行数下降也能确认条目已淘汰 |
| `software_frame_bytes` | Windows 软件呈现的整窗缓冲 | 缩小窗口；其它路径为零 |

`role="warm"` 表示渲染器位于待命池，不属于可见窗口；关闭窗口不会释放它。
这些数值是主机内存，不是 GPU 显存。

### 回收 warning

默认 `warn` 过滤器会接收 `memory::reclaimed` warning，因为它们解释了可见内容为何消失：

```sh
grep 'memory::reclaimed' ~/.sonicterm/logs/sonicterm.log*
```

- `cancelled a media capture that stopped receiving` 表示连续两个 30 秒采样都没有新字节；
  不完整图像不会显示，暂存已回收。
- `discarded inline images from idle panes to stay within the process ceiling`
  表示空闲窗格中的旧解码图像已被删除；仍需要时请重新发送。

默认过滤器也会接收 `sonic::glyph_atlas` 上的
`inline image atlas full; skipped older images without evicting text glyphs`；这表示渲染器
图集压力阻止了旧图像上传。`inline media evicted to hold the process-wide ceiling` 位于
`memory` target，因此只在 `level` 为 `info` 或 `debug` 时出现；它表示窗格删除了旧图像，
但至少保留最新一张。

## 崩溃、卡死与退出证据

Panic hook 对所有线程生效，会写入带会话标识的 `crashes/crash-<timestamp>.log`，
包含版本、panic 内容、源码位置、强制 backtrace 和最近 50 条 tracing 事件。正常关闭会
写入 `sonic_exit` warning。Unix 上的 SIGSEGV、SIGBUS、SIGILL、SIGABRT 和 SIGFPE
会先通过信号安全路径向日志追加固定 `FATAL: SIG…` 行，再重新触发信号，让操作系统生成
诊断。Windows 在系统已配置时使用 WER 或 LocalDumps。

卡死不一定产生 panic 工件。macOS 上应在强制退出前采样：

```sh
sample <pid> 10 -file /tmp/sonicterm-hang.sample.txt
grep -nE 'dispatch_sync_f_slow|redraw_target|__psynch_cvwait' \
  /tmp/sonicterm-hang.sample.txt
```

`SIGKILL`、强制退出、`TerminateProcess`、断电和硬 OOM 不会运行清理代码。SonicTerm
无法在这些情况发生后写最后一行或转储，只能预先留下两类记录：

1. `sessions/session-<id>.marker` 只记录会话 id、pid、版本、平台、启动时间和状态。
   残留标记只能证明没有走到关闭路径，不能说明原因。仍在运行的兄弟进程会被跳过，
   损坏的标记仍算证据，每个旧标记只在下次启动时报告一次。
2. `breadcrumbs/breadcrumbs-<id>.log` 是有界的原子快照，不含终端文本、命令、环境值、
   token 或凭据。它固定保留最新版本、平台、渲染器、数量、完整进程资源、保留量、
   分配器状态和有界生命周期事件。其中 `event=retention` 记录包含 `renderer_bytes`、
   `row_glyph_cache_bytes` / `row_glyph_cache_items` 和
   `row_quad_cache_bytes` / `row_quad_cache_items`，因此最后一份完整故障前快照会同时保留
   两个行缓存的大小与占用。另有固定成本的
   `event=resource_history private_committed=... resident=...` 采样：启动时立即一次，
   之后每 5 秒一次，最多保留 48 条。虚拟地址空间只出现在完整 `event=resource` 记录中。

诊断记录通过原子替换重写。硬终止后留下的是最近一份完整的故障前快照，不是转储，也不
证明原因。默认文件预算为 64 KiB；自定义限制必须能容纳全部必保记录、生命周期容量和
一条最大宽度历史记录。绝对下限为 4096 字节。

下次启动时，SonicTerm 还会按保守的文件名约定检查操作系统记录：

| 平台 | 检查位置 |
| --- | --- |
| macOS | `~/Library/Logs/DiagnosticReports`、`/Library/Logs/DiagnosticReports`（`.ips`） |
| Windows | `%LOCALAPPDATA%\CrashDumps`、`%LOCALAPPDATA%\Microsoft\Windows\WER\ReportQueue`、`...\ReportArchive` |

匹配结果只表示“可能与 SonicTerm 有关”。Windows 检查不会读取 WER registry 配置，
因此没有文件只说明标准位置没有匹配记录。

## 缺陷报告材料

请附上：

1. SonicTerm 版本和操作系统版本。
2. 修改时间最新的 `sonicterm.log*` 文件最后 200 行。
3. 相关 panic 工件、操作系统记录或进程 sample（如果存在）。
4. 精确复现步骤；视觉、输入、VT、字体或布局问题还要附截图或短录屏。
5. 渲染问题的硬件/软件 adapter 信息。
6. 内存增长问题至少附五次连续 `memory snapshot`；若开启 `debug`，还要附定位窗格的
   `pane retention`，以及同一时段全部 `session retention` 和相关
   `renderer retention`。说明该窗格的 `largest_seam` 和当时会话正在做什么。

不要公开密钥、token、完整环境变量、终端输出或敏感命令内容。
