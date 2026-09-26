# 日志

[English](Logging)

先找下方路径中最新的日志。帧耗时用 `debug`，内存快照用 `info`；提交问题前查看末尾
清单。崩溃与卡死证据另有专节；没有崩溃转储不代表正常退出。

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
| `warn` | warning、error、`sonic_exit`、`sonic::gpu` 设备记录，以及用户可见的回收或耗尽提示 |
| `info` | SonicTerm 常规信息和聚合 `memory snapshot` |
| `debug` | 详细诊断、窗格/渲染器内存、状态机事件、`render_timing` 和 `tear_out_timing` |

配置过滤器始终把 `wgpu`、`naga`、`sonicterm-vt` 和 `sonicterm-grid` 保持在 warning
级别。字体塑形热路径的海量输出位于 `trace`，任何配置级别都不会启用；只有专门排查该
路径时才使用精确的 `RUST_LOG` 指令。

## 本地路径点击诊断

复现显式本地路径点击失败前，设置 `[logging] level = "debug"`，或对单次运行使用
`RUST_LOG=sonicterm_app::app::path_target=debug`。当已检测的显式路径没有当前有效的
文件系统授权目标时，`local path activation unverified` 事件会记录该次点击。
它使用不可变的点击快照，不重新读取文件系统或 parser。

| 字段 | 含义 |
| --- | --- |
| `window_id`、`pane_id`、`pointed`、`view_top` | 被点击的窗口/窗格、绝对单元格与视口起点 |
| `screen_epoch`、`scrollback_evicted` | 屏幕与保留历史的身份 |
| `cwd`、`cwd_revision` | 该窗格的 OSC 7 authority/path 与版本，不是进程 CWD |
| `clicked_path` | 与点击关联的显式路径文本 |
| `candidates` | 有界候选集合，含类型来源、解析后的路径、单元格范围及完整字面缺失前提 |
| `reason` | 当前探测失败键；没有匹配失败结果时为 `path-error-pending` |

pending 不是文件不存在的证据。事件包含可能敏感的路径，但不含整行终端内容或环境转储；
分享前应检查。路径以转义后的 debug 格式输出；即使候选枚举有上限，单条事件仍可能较大。
默认 `warn` 级别不输出它，悬停和未经验证的裸文件名点击也不输出。

没有该事件不能证明成功或失败：可能没有开启日志、没有检测到目标，或被拒绝的目标/原生打开
失败走了其它分支。原生打开失败保留独立的 `path open failed` 告警。调查相对路径失败时，
在同一窗格比较同一个文件的相对路径、绝对路径与本地文件 URI；保留对应点击身份，
不要用另一个 shell 的 CWD 推断该窗格目录。

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
| `completed_messages` | `write_all` 与 `flush` 均成功的原生写入数 |

事件不携带被拒绝的负载。其 debug 表示、warning 和通知均不包含输入文本、命令、路径或剪贴板内容。
标签页转移后，通知跟随窗格的当前窗口；已关闭的窗格只记录 warning，不在无关窗口显示通知。
代理缺失或事件投递失败时，生产者直接记录同样的元数据，但不附带无法确认的当前窗口标识。

生产终端回复在解析器与副作用锁外进入独立 FIFO，使用 64 KiB 内存和私有临时文件溢出存储。
UI 队列饱和不会丢弃回复、产生拒绝 warning 或停止输出处理。存储或原生失败会锁存，并以
`terminal reply delivery failed` 报告一次，包含窗格标识与字节数；输出、重绘和退出观察继续。
原生 writer 与暂存读取错误独立记录。正常销毁会释放暂存文件，不产生拒绝通知。
存储与交付限制见 [终端 IO 与 VT](Terminal-IO-and-VT-zh-CN)。

健康 writer 尚未被调度时，四条小消息就能填满通道。受控测试使用生产环境的入队与 writer 循环，
证明突发排空仍保持顺序。另设阻塞写入和阻塞 flush 的夹具，证明排队字节为零时仍可能有一条
在途消息；再加入四条等待消息后，下一条消息会被显式拒绝。这些夹具区分机制，不会倒推出
以前缺少归属信息的 warning 究竟由哪个生产者或原生条件引起。

队列容量和单消息上限不变。每个窗格先把原生指针移动合并到一个固定 64 字节槽，再尝试入队。
队列满时保留最新位置，10 ms 后重试但不请求重绘；新位置替换旧位置。离散输入在总长度允许时
把之前待发移动合并为同一队列消息，保持字节顺序且不多占槽。达到单消息上限或被拒绝的离散
输入会取代待发移动，不会在其后重放。writer 断开时只报告一次并清空移动槽；鼠标跟踪/编码/屏幕
模式变化会丢弃待发移动。解析器忙时延迟仅移动的重试，直到可验证其模式；若有后续离散输入，
则取代尚未验证的移动，不延迟按键或重放过期字节；窗格销毁释放槽。
其他 UI 输入仍采用拒绝而非阻塞；worker 持有的终端回复使用溢出 FIFO。应比较同一窗格的连续
观察值和进度计数，而不是凭一条 `QueueFull` warning 下结论。

## PTY 尺寸调整失败诊断

默认 `warn` 级别会把原生层拒绝的 PTY 尺寸调整记为一条 `pty resize failed` 事件。

| 字段 | 含义 |
| --- | --- |
| `pane_id` | PTY 拒绝该次尺寸调整的窗格标识 |
| `cols`、`rows` | 请求的几何尺寸，而不是 pty 当前持有的尺寸 |
| `error` | 由 `Display` 渲染的失败信息。原生拒绝显示平台文本；某一维为零时显示 `refusing pty resize to <cols>x<rows>` |

事件不携带终端内容：只有窗格 id、请求的列数与行数以及错误。

每个窗格仅记录首次 resize 失败，直到成功重置告警闩锁，避免切换标签页或拖动窗口时按输入
频率写日志。尝试仍会执行：IO 边界只跳过无效尺寸和成功的重复请求。网格保留请求尺寸，
没有回滚、重试定时器或失败心跳。

## 渲染与性能诊断

把 `level` 设为 `debug`，重启后复现问题。`render_timing` target 会记录网格遍历、
覆盖层组装、字形上传、surface 获取、提交和呈现等帧阶段，并标明主窗口或子窗口、
`mode=full` 和 `damaged_rows`。无操作帧会在完成帧计时输出前返回。GPU 局部损伤限制的是
绘制裁剪区域，不是帧组装。没有单独的渲染计时开关。

启动日志会记录选中的 wgpu adapter、设备类型和软件 adapter 分类。在 RDP、虚拟机或
VDI 环境中，请查找 `software-render degrade engaged`，并对照[配置](Configuration-zh-CN)中的
`[appearance].software_render_mode`。在 `level = "debug"` 下，每个 renderer 还会在启动以及
模式、opacity、主题或 presenter 状态变化时写入 `renderer LCD subpixel policy`。其中的
`requested`、`effective`、`windows_host`、`opaque_target`、`software_presenter` 和
`dual_source_supported` 字段能解释每次 LCD 到灰度的回退，不必依赖截图推断。

在 `info` 级别，`DPI transition synchronized` 会记录 `window_id`、`old_scale`、
`new_scale`、`native_scale`、`size_scale`，以及 `old_inner`、`suggested`、`minimum`、
`available`、`target`。`renderer_before`/`renderer_after` 是物理表面像素尺寸，
`cell_before`/`cell_after` 是光栅像素单位的单元格尺寸。macOS 的 `size_scale` 使用原生
backing scale，因为 `old_inner` 已按该比例报告；其他平台使用保存的旧比例。这些成对的
输入/输出可区分重复缩放与表面或单元格尺寸不一致，不会记录终端内容。

## GPU 设备错误诊断

每个 wgpu 设备只保留一份错误状态，由同一 GPU 上下文创建的所有窗口共享。`sonic::gpu` target
在每次状态变化时写一条记录，另为第一次隔离故障写一条；默认过滤器中的 `sonic=warn` 会放行这些
记录。重复错误只更新计数，不写新记录。

| 消息 | 级别 | 写入时机 |
| --- | --- | --- |
| `GPU device stopped accepting work` | `error` | Validation、OutOfMemory 或 Internal 错误使设备从 `Usable` 变为 `Unusable` |
| `GPU device lost` | `error` | 设备丢失回调记录 `Lost`，包括有意销毁之后 |
| `contained isolated GPU error` | `warn` | 测试故障钩子产生的第一次隔离故障；之后只计数 |

| 字段 | 含义 |
| --- | --- |
| `generation` | 设备在进程内唯一的编号 |
| `state` | 写入记录时的设备状态 |
| `kind` | 引发记录的错误类别：验证、内存不足、内部错误或设备丢失 |
| `operation` | 引发错误的渲染器操作标签，例如 `render.submit`、`try_resize` 或 `glyph_upload.rebuild` |
| `description` | wgpu 给出的错误信息 |
| `lost_reason` | wgpu 给出的丢失原因；只有丢失记录中不为空 |
| `destroy_requested` | 设备是否由 SonicTerm 有意销毁；出现在状态变化记录中 |
| `validation`、`out_of_memory`、`internal`、`isolated`、`lost` | 按错误类别合并的计数 |

出现 `error` 记录后，所有窗口都停止在该设备上绘制；shell、输入、会话和窗口生命周期照常工作。
记录设备丢失后会启动共享设备恢复；没有丢失记录的不可用设备保持停止。每个受影响的渲染器首次
观察到停止时记录一条 warning：主窗口为 `render error`，其他窗口为 `child render error`。
隔离规则见[架构内部机制](Architecture-Internals-zh-CN)。

### 共享设备恢复记录

`sonic::gpu::recovery` 日志目标会被默认 `sonic=warn` 过滤器接纳。warning 记录包括调度、请求
接纳或拒绝、协商与渲染器准备/提交失败、超时、工作线程忙时拒绝、实际请求完成（`outcome` 与
`decision`），以及成功的 `shared GPU recovery committed`。error 记录包括工作线程断连、设备不可用但尚未丢失，以及
`shared GPU recovery exhausted; terminal sessions remain running`。

`generation` 标识已提交或新提交的设备，`ticket` 标识已接纳的请求，`attempt` 是从一开始的预算
位置，`delay_ms` 是毫秒单位的调度退避，`rebound` 是同时提交的渲染器数量。能够取得原生错误时，
失败记录也会包含该错误。这些记录不包含终端输出或输入。

成功提交记录只证明对象替换和设备闸门接纳，不证明原生扫描输出或后续帧已呈现。稳定计时只从已确认
的 `Presented` 帧开始。请求超时不证明原生工作线程已退出，退出时的释放也只是尽力完成；应比较
请求身份和之后的完成记录，而不是把没有日志当作清理成功。重试策略与限制见
[渲染模式](Rendering-Modes-zh-CN)。

## 内存诊断

### `info` 级别的聚合快照

在 `level = "info"` 下，`target="memory"` 最多每 30 秒写一条 `memory snapshot`。
它汇总操作系统进程数据、所有已采样窗格接缝、可见与预热渲染器，以及一次共享设备
分配器读取：

以下日志仅为格式示意，不是采集的实测记录。尖括号值代表运行时字段；`<metric>` 可以是
字节数或 `unsupported`，`<delta>` 可以是带符号的变化量或 `unavailable`。为便于阅读，示例已换行。

```text
memory snapshot process_private_committed_bytes=<metric> process_resident_bytes=<metric>
                process_virtual_bytes=<metric> process_private_committed_delta=<delta>
                process_resident_delta=<delta> process_virtual_delta=<delta>
                session_total_bytes=<bytes> session_delta=<delta>
                grid_visible_bytes=<bytes> grid_history_bytes=<bytes> grid_alternate_bytes=<bytes>
                parser_bytes=<bytes> hyperlink_bytes=<bytes> inline_media_bytes=<bytes>
                pty_output_bytes=<bytes> pty_input_bytes=<bytes> panes_total=<count> panes_sampled=<count> panes_contended=<count>
                renderer_total_bytes=<bytes> renderer_total_items=<count>
                renderer_row_glyph_cache_bytes=<bytes> renderer_row_glyph_cache_items=<count>
                renderer_row_quad_cache_bytes=<bytes> renderer_row_quad_cache_items=<count> renderer_delta=<delta>
                live_renderers=<count> renderers="visible[<window-id>] glyph=<bytes>/<items> image=<bytes>/<items> row_glyph=<bytes>/<items> row_quad=<bytes>/<items> software=<bytes>/<items> total=<bytes>/<items>; warm[<slot>] glyph=<bytes>/<items> image=<bytes>/<items> row_glyph=<bytes>/<items> row_quad=<bytes>/<items> software=<bytes>/<items> total=<bytes>/<items>"
                allocator_state=measured allocator_source=main allocator_label=<window-id>
                allocator_allocated_bytes=<bytes> allocator_reserved_bytes=<bytes>
                allocator_allocations=<count> allocator_blocks=<count> allocator_largest_block_bytes=<bytes>
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
| `panes_contended` | 因解析器或内联图像锁被占用而跳过的窗格；非零表示会话总量不完整 |
| `renderer_total_bytes` / `renderer_total_items` | 所有可见与预热渲染器的 CPU 存储 |
| `renderer_row_glyph_cache_bytes` / `renderer_row_glyph_cache_items` | 所有渲染器的逐行字形实例与装饰缓存存储及缓存行数 |
| `renderer_row_quad_cache_bytes` / `renderer_row_quad_cache_items` | 所有渲染器的逐行背景/装饰 quad 缓存存储及缓存行数 |
| `live_renderers` | 进程级渲染器数量；若高于 `renderers` 条目数，可能存在仍存活但无法访问的渲染器 |
| `renderers` | 各渲染器角色及字形/图像/行缓存/软件帧存储明细 |
| `allocator_state` | `measured`、后端不支持报告或 GPU 设备已停止时的 `unsupported`，或还没有渲染器时的 `none` |
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
随后写一条 `session retention`。解析器或内联图像锁被占用的窗格会被跳过，不会等待。

```text
pane retention pane="<window-id>/<pane-id>" total_bytes=<bytes>
               grid_visible_bytes=<bytes> grid_history_bytes=<bytes>
               grid_alternate_bytes=<bytes> parser_bytes=<bytes> hyperlink_bytes=<bytes>
               inline_media_bytes=<bytes> pty_output_bytes=<bytes> pty_input_bytes=<bytes>
               largest_seam="<seam-name>" largest_seam_bytes=<bytes>
session retention panes=<count> total_bytes=<bytes> grid_visible_bytes=<bytes>
                  grid_history_bytes=<bytes> grid_alternate_bytes=<bytes> parser_bytes=<bytes>
                  hyperlink_bytes=<bytes> inline_media_bytes=<bytes>
                  pty_output_bytes=<bytes> pty_input_bytes=<bytes>
```

八个接缝互不重叠，相加等于 `total_bytes`：

| 字段 | 归属内容 | 首先处理 |
| --- | --- | --- |
| `grid_visible_bytes` | 当前屏幕行、提示符存储、稀有属性及网格容器开销 | 无需处理，其中包含屏幕本身 |
| `grid_history_bytes` | 保留的回滚缓冲 | 必要时调低 `scrollback` |
| `grid_alternate_bytes` | 备用屏幕激活时保存的主屏幕行和历史 | 退出全屏程序 |
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
renderer retention window="<window-id>" role="visible" total_bytes=<bytes>
                   glyph_atlas_bytes=<bytes> glyph_atlas_items=<count>
                   image_atlas_bytes=<bytes> image_atlas_items=<count>
                   row_glyph_cache_bytes=<bytes> row_glyph_cache_items=<count>
                   row_quad_cache_bytes=<bytes> row_quad_cache_items=<count> software_frame_bytes=<bytes>
renderer retention window="warm[<slot>]" role="warm" total_bytes=<bytes>
                   glyph_atlas_bytes=<bytes> glyph_atlas_items=<count>
                   image_atlas_bytes=<bytes> image_atlas_items=<count>
                   row_glyph_cache_bytes=<bytes> row_glyph_cache_items=<count>
                   row_quad_cache_bytes=<bytes> row_quad_cache_items=<count> software_frame_bytes=<bytes>
```

| 字段 | 归属内容 | 首先处理 |
| --- | --- | --- |
| `glyph_atlas_bytes` | 该渲染器的 CPU 字形图集容量 | 有上限；预热条目由 `warm_window_pool` 控制 |
| `glyph_atlas_items` | 图集中的字形条目数 | 与字节数一起判断实际占用与容量 |
| `image_atlas_bytes` | CPU 内联图像图集像素缓冲容量，包含非空的 1×1 占位分配 | 减少图像或渲染器数量 |
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
包含版本、panic 内容、源码位置、强制 backtrace 和最多 50 条获准的 tracing 事件。正常关闭会
写入 `sonic_exit` warning。Unix 上 SIGSEGV、SIGBUS、SIGILL、SIGABRT 和 SIGFPE 中最先
到达的一个会通过信号安全路径向日志追加固定 `FATAL: SIG…` 行。随后处理器以原始信号信息
调用在它之前安装的动作（每个进程至多一次），因此 Rust 运行时仍能指出栈溢出的线程。
该动作返回后，或原本没有动作时，处理器以默认动作再次触发该信号。这会结束进程，操作系统
仍可生成诊断，但诊断描述的是再次触发的信号，而非最初的故障；原本被忽略的致命信号同样会
结束进程。因此栈溢出记为 `FATAL: SIGSEGV`（或 `SIGBUS`），而进程随后因 Rust 报告之后的
SIGABRT 结束。Windows 在系统已配置时使用 WER 或 LocalDumps。

崩溃历史不会扩展所选 `RUST_LOG`/配置 filter，而是再与 DEBUG 上限及显式持久化规则取
交集。即使输出 sink 显式启用了 TRACE，历史也不保留 TRACE。字体塑形文本与集合使用
`sonicterm_font::payload`，在任何级别都被排除；普通 warning/error 目标保留安全的阶段和
数量诊断。正常白色文字与不染色彩色字形的发射不会产生 warning。

每条记录的自有可变负载最多 4 KiB，其中 target 最多 256 字节。环形历史还实施合计
64 KiB 可变容量和 50 条记录上限，淘汰最早记录。格式化使用有界存储并在 UTF-8 边界截断，
`[truncated]` 也计入上限。固定元数据另受记录数量限制。Panic 负载文本与格式化摘要各自
最多 4 KiB，从借用的 panic 数据读取。

Backtrace 捕获/输出和串联的 panic hook 属于独立范围。这些上限约束 recorder 可控制的
格式化/保留，不限制任意生产端 `Debug` 实现内部的分配，也不保证任意日志都不含敏感信息。
负载 TRACE 仍可作为显式启用的普通 sink 证据；结构化 breadcrumbs 保持独立的仅元数据契约。

卡死不一定产生 panic 工件。macOS 上应在强制退出前采样：

```sh
sample <pid> 10 -file /tmp/sonicterm-hang.sample.txt
grep -nE 'dispatch_sync_f_slow|redraw_target|__psynch_cvwait' \
  /tmp/sonicterm-hang.sample.txt
```

`SIGKILL`、强制退出、`TerminateProcess`、断电和硬 OOM 不会运行清理代码。SonicTerm
无法在这些情况发生后写最后一行或转储，只能预先留下两类记录：

1. `sessions/session-<id>.marker` 只记录会话 id、pid、版本、平台、启动时间和状态。
   残留标记表示会话没有被标为干净；原生 PTY 清理未完成时也会保留它。
   它不能说明原因。仍在运行的兄弟进程会被跳过，
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
