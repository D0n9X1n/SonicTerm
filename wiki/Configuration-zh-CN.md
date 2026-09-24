# 配置

[English](Configuration)

### 文件与查找顺序

SonicTerm 在所有平台使用同一个配置文件：

```text
~/.sonicterm/sonicterm.toml
```

首次启动会创建这个文件，并在 `~/.sonicterm/themes/` 和
`~/.sonicterm/keymaps/` 中写入可编辑示例。

`theme` 和 `keymap` 可以写名称，也可以写 TOML 路径。使用名称时，SonicTerm
先查找用户目录，再查找内置 `assets/` 目录。看起来像路径的值会直接使用。

未实现的 TOML key 不影响行为。运行时保存会保留它们、注释和格式；Rust `Config`
serializer 则只保留顶层未知 key，不保留注释、格式或嵌套未知 key。

### 支持的 key 与默认值

#### 顶层

| Key | 默认值 | 行为 |
| --- | --- | --- |
| `theme` | `"wezterm"` | 选择主题。参见 [主题](Themes-zh-CN)。 |
| `keymap` | `"sonicterm-macos"`、`"sonicterm-windows"` 或 `"sonicterm-linux"` | 选择当前平台的 keymap。参见 [快捷键](Keybindings-zh-CN)。 |
| `locale` | `""` | 选择 `en`、`zh-CN` 或 `ja`。空值依次使用 `SONIC_LOCALE`、系统 locale、`en`。 |
| `quit_on_last_window_close` | `true` | 为兼容而接受，但会被忽略。无论取值如何，在所有平台上，最后一个窗口关闭时 SonicTerm 都会退出。 |
| `tab_max_width` | `240` | 单个标签页的首选最大逻辑像素宽度。非有限值或非正值会被忽略。由字体/缩放推导的可读最小宽度优先于更小的最大值；拥挤时显示包含活动标签页的区段和溢出选择器。极窄窗口会放宽最小宽度以保留两个点击区域。即使终端内容空闲，宽度策略变化也会使保留的标签界面失效并重绘。 |

#### `[font]`

| Key | 默认值 | 行为 |
| --- | --- | --- |
| `family` | `"Rec Mono St.Helens"` | 主字体族。缺失的字符使用回退字体。 |
| `size` | `13` | 字号，单位为 point。 |
| `line_height` | `1.3` | 行高倍率。 |
| `weight_scale` | `1.0` | 选定字体后统一调整所有单色字形的粗细，包括粗体、斜体和回退字体。有效范围是 `0.5..=5.0`；其它值会变成 `1.0`。固定字号和 DPI 时，单元格度量、位图尺寸、bearing 与推进量不变。彩色图像内容不变。 |
| `subpixel_aa` | `"off"` | LCD 覆盖率顺序：`off`、`rgb` 或 `bgr`。生效条件见下文。 |

字体设置会同时用于终端文字和普通应用文字。修改 `family`、`size` 或
`line_height` 时，每个可见 pane 的 grid 与 PTY 都会重新调整大小。只修改
`weight_scale` 不会改变 metrics。`subpixel_aa` 仅在 Windows 上满足以下条件时
生效：配置的 backdrop 是 `opaque`、实际 opacity 为 `1`，并且 Windows 软件
presenter 正在使用或 GPU 支持 dual-source blending。其它所有组合都会确定性回退
到灰度，包括 Mica、Acrylic、Tabbed、opacity 小于 `1`、不支持的 GPU 和非 Windows
主机。`off` 保留 alpha 最大值灰度输出；`rgb` 把逻辑红、绿、蓝覆盖率映射到对应
显示通道；`bgr` 交换红、蓝通道。字体 shaping 与 fallback 的详细说明见
[渲染与字体](Rendering-and-Fonts-zh-CN)。

#### `[window]`

| Key | 默认值 | 行为 |
| --- | --- | --- |
| `cols` | `100` | 新窗口的初始列数。 |
| `rows` | `30` | 新窗口的初始行数。 |
| `padding_left` | `12` | 内容左侧 padding，单位为逻辑像素。 |
| `padding_right` | `12` | 内容右侧 padding，单位为逻辑像素。 |
| `padding_top` | `8` | 内容上方 padding，单位为逻辑像素。 |
| `padding_bottom` | `4` | 内容下方 padding，单位为逻辑像素。 |
| `decorations` | `true` | 为新窗口启用原生标题栏装饰。 |
| `warm_window_pool` | `1` | 为快速拖出标签页预留的隐藏子窗口数量。`0` 关闭预热池。硬件渲染最多保留 `5` 个；软件渲染会把任何非零值限制为 `1`。 |

`cols` 与 `rows` 设置启动尺寸，以及没有可用来源窗口时的默认尺寸。新建和拖出窗口继承发起窗口
的逻辑客户区尺寸，预热窗口也相同。尺寸在延迟创建之前记录，后续焦点变化不会替换它；不继承
最大化或全屏状态。来源缺失、最小化或尺寸为零时采用配置默认值。目标屏幕 DPI、原生最小尺寸
和实际接受的 resize 仍生效；把标签页移入已有窗口不会改变该窗口尺寸。每个原生终端窗口都有不可配置的硬最小内区大小：
30 列 × 10 行。像素下限会按当前字体、DPI、padding、标题栏和标签栏 geometry 重新计算，
包括实时重载字体/padding 以及切换标签栏可见性之后。

Grid 尺寸始终有上限。每个轴最多是 `4096`，可见 grid 最多包含
`524288` 个 cell，包含历史记录的完整 grid 最多包含 `1048576` 个 cell。

#### `[terminal]`

| Key | 默认值 | 行为 |
| --- | --- | --- |
| `shell` | 省略 | 新 pane 使用的 shell。Windows 依次尝试 `PATH` 中的 `pwsh.exe`、已注册的 PowerShell 7、Microsoft Store 中的真实程序、Windows PowerShell、`cmd.exe`。Unix 依次尝试可执行的 `$SHELL`、当前用户 passwd 中的可执行 shell、`/bin/sh`。非空显式值优先。 |
| `term_program` | `"SonicTerm"` | 新子 PTY 的 `TERM_PROGRAM`。`TERM_PROGRAM_VERSION` 通常是 SonicTerm 版本；`term_program = "WezTerm"` 时为 `20230712-072601`。 |
| `scrollback` | `1000` | 每个 pane 请求保留的历史行数。`0` 关闭历史记录。Grid 和内存字节预算可能进一步降低实际值。 |
| `keypad_mode` | `"auto"` | `auto` 保留协商的旧式小键盘映射及操作系统解析出的数字文本。显式选择 `numeric` 后，运算符和 Enter 不受 DECKPAM 影响，使用普通文本/Return 规则，导航遵循逻辑按键。Kitty 输入不变。参见[快捷键](Keybindings-zh-CN)。 |
| `clickable_local_targets` | `true` | 所有平台都打开经过验证的本地目录，或在所在文件夹中选中文件。包括本地 file URI 和本机路径 OSC 8 链接；网页和邮件链接不受控制。 |
| `clickable_bare_names` | `true` | 允许按准确 pane 的可信本机 OSC 7 工作目录解析上下文名称。带分隔符的相对路径也要求同一可信 pane CWD。只有 `clickable_local_targets` 同时为 `true` 时才生效。 |
| `cursor_blink` | `false` | 让光标闪烁。 |
| `cursor_shape` | `"block"` | 可选 `block`、`bar` 或 `underline`。 |

Scrollback 行数与内存预算会同时限制历史记录。包含丰富属性的行可能先达到
字节预算。参见 [内存](Memory-zh-CN)。

#### `[appearance]`

| Key | 默认值 | 行为 |
| --- | --- | --- |
| `backdrop` | `"opaque"` | 可选 `opaque`、`mica`、`acrylic`、`tabbed`。Windows 会尽力应用对应 DWM 材质。Linux 在每次启动和显式重载时都会收敛为 `opaque`，请求其它值时记录一次 warning。macOS 只把非 `opaque` 值当作需要 alpha 的窗口；这些 Windows 材质名称不会选择 macOS 材质。 |
| `opacity` | `1.0` | 终端背景透明度，会限制在 `0.0..=1.0`。 |
| `scrollbar` | `"auto"` | 可选 `auto`、`always`、`never`。没有可滚动历史时，`always` 也不会显示。 |
| `panel_padding` | `2.0` | 浮动面板内部 padding，单位为逻辑像素。负值按 `0` 处理。 |
| `software_render_mode` | `"auto"` | `auto` 在检测到软件 adapter 时降级；`force` 始终降级；`off` 从不降级。 |

软件降级会降低帧率与动画成本。Windows 中，`software_render_mode = "force"`
还会让新窗口变为不透明，因为软件 presenter 不能合成透明效果。如果配置了非
`opaque` backdrop，SonicTerm 会记录包含配置值与实际值的 warning。`auto`
不会覆盖 backdrop。

滚动条 thumb 可以拖动。点击 track 会滚动一个 viewport。`auto` 会在滚动、
拖动或鼠标靠近 pane 右边缘时显示。

#### `[accessibility]`

| Key | 默认值 | 行为 |
| --- | --- | --- |
| `high_contrast` | `false` | 把当前主题的前景色和背景色改为 `#ffffff` 与 `#000000`。 |
| `reduced_motion` | `false` | 可以解析和保留，但目前不会改变界面。 |
| `strong_focus` | `false` | 可以解析和保留，但目前不会改变界面。 |

#### `[notifications]`

| Key | 默认值 | 行为 |
| --- | --- | --- |
| `long_command` | `false` | 在 Windows 上启用长命令桌面通知。macOS 和 Linux 目前不发送此通知。 |
| `threshold_secs` | `10` | 命令报告的耗时必须大于此值。 |

#### `[logging]`

| Key | 默认值 |
| --- | --- |
| `level` | `"warn"` |
| `max_file_size_mb` | `10` |
| `max_rotated_files` | `3` |
| `max_age_days` | `2` |
| `max_crash_dumps` | `10` |
| `max_crash_age_days` | `2` |
| `max_crash_bytes` | `10485760` |
| `max_breadcrumb_files` | `10` |
| `max_breadcrumb_age_days` | `2` |
| `max_breadcrumb_bytes` | `1048576` |

`level` 可选 `error`、`warn`、`info`、`debug`。Logging 在启动时初始化，
所以修改这些值后需要重启。文件位置、清理规则和诊断方法见 [日志](Logging-zh-CN)。

### 编辑与重载

在命令面板中执行 **Edit sonicterm.toml** 可以打开标准配置文件。SonicTerm
只在启动和执行 **Reload Config** 时读取它，没有文件 watcher。

每次重载都会重新读取所选主题与 keymap 文件，即使名称没有变化。以下设置会
应用到现有窗口：

- 主题、keymap 与 locale；
- 字体族、字号、行高、字重与 LCD 次像素模式；
- 内容 padding、opacity、滚动条和 panel padding；
- 光标形状与闪烁；
- scrollback、小键盘模式与本地目标策略；
- 标签页宽度、预热窗口目标、软件降级、无障碍与通知设置。

有些设置只影响重载后新建的对象：

- `cols`、`rows`、`decorations` 和原生 `backdrop` 只影响新窗口；
- `shell` 与 `term_program` 只影响新 pane；
- logging 设置需要重启。

平台能力收敛会在启动或重载配置成为会话基线前执行。因此 Linux 永远不会存储不支持的
backdrop：现有状态和之后所有预热、新建或拆出窗口都会从已收敛配置读取 `opaque`。warning
只在该次收敛真正修改值时写一次；再次处理已经为 `opaque` 的结果不会重复 warning。

修改 `backdrop` 或 `software_render_mode` 可能涉及原生窗口初始化。如果需要完整
应用原生窗口变化，而不只是更新 renderer 策略，请重启 SonicTerm。实时修改
`subpixel_aa` 只会使已呈现帧失效并重绘，不会重建字体、光栅图块或任一图集。

### 保存当前字体设置

**Save Current Settings** 只修改 `~/.sonicterm/sonicterm.toml` 中的两个值：

```toml
[font]
size = 13
weight_scale = 1.0
```

保存写入当前字号和有效 `weight_scale`，保留注释、顺序、换行及其它所有 key。
它不保存主题或其它运行状态，也不重载已经生效的数值。

如果文件不存在，SonicTerm 会先创建初始文件。进程内锁和持久的
`sonicterm.toml.save.lock` sidecar 会阻止两个 SonicTerm 同时保存。替换文件前，
程序还会再次比较精确字节。编辑器并发修改、TOML 格式错误、字体值无效或锁冲突
都会拒绝写入。现有文件和 reset 基线保持不变，并显示 Error 通知。

保存成功时，SonicTerm 会先在配置目录写入临时文件，再原子替换配置，并显示
Info 通知。读取者只会看到完整旧文件或完整新文件；突然断电时的持久性不在此保证内。

### 错误与恢复

启动时，如果配置不可读或 TOML 格式错误，SonicTerm 会记录 warning 并使用默认值，
保证应用仍可打开。所选主题无效时回退到内置 `wezterm`。所选 keymap 无效时回退到
当前平台的内置 keymap。

执行 **Reload Config** 时，如果已有的 `sonicterm.toml` 不可读或格式错误，当前整套配置
保持不变；文件不存在时则加载默认值。如果配置本身有效，但主题或 keymap 读取失败，
SonicTerm 会保留当前主题或 keymap、记录错误，并应用其它有效设置。结构正确的 keymap 中，如果只有某个
binding 的 action 无法解析，SonicTerm 只跳过该 binding，其它 binding 仍会生效。
