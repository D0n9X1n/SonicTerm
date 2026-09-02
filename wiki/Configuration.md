# Configuration / 配置

## English

### Files and lookup

SonicTerm uses one cross-platform config file:

```text
~/.sonicterm/sonicterm.toml
```

The first launch creates this file and seeds editable examples under
`~/.sonicterm/themes/` and `~/.sonicterm/keymaps/`.

`theme` and `keymap` accept either a name or a TOML path. A named value first
checks the matching user directory, then the bundled `assets/` directory. A
path-like value is used directly.

SonicTerm tolerates and preserves unknown TOML keys. Unknown keys do not change
behavior unless the running build implements them.

### Supported keys and defaults

#### Top level

| Key | Default | Behavior |
| --- | --- | --- |
| `theme` | `"wezterm"` | Selects a theme. See [Themes](Themes). |
| `keymap` | `"sonicterm-macos"`, `"sonicterm-windows"`, or `"sonicterm-linux"` | Selects the platform keymap. See [Keybindings](Keybindings). |
| `locale` | `""` | Selects `en`, `zh-CN`, or `ja`. Empty uses `SONIC_LOCALE`, then the OS locale, then `en`. |
| `quit_on_last_window_close` | `true` | On macOS, `false` keeps the process available from the Dock after the last window closes. Other platforms always exit with no windows. |
| `tab_max_width` | `240` | Maximum width of one tab in logical pixels. A non-finite or non-positive value is ignored. Crowded tabs still share the available width. |

#### `[font]`

| Key | Default | Behavior |
| --- | --- | --- |
| `family` | `"Rec Mono St.Helens"` | Primary font family. Missing glyphs use the fallback chain. |
| `size` | `13` | Font size in points. |
| `line_height` | `1.3` | Line-height multiplier. |
| `weight_scale` | `1.0` | Regular-text weight for the configured family. Valid values are `0.5..=5.0`; other values become `1.0`. Cell metrics, fallback glyphs, color emoji, and SGR bold do not change. |

Font changes apply to terminal text and regular application text. Changes to
`family`, `size`, or `line_height` resize every visible pane and its PTY.
`weight_scale` keeps the existing metrics. For shaping and fallback details, see
[Rendering and Fonts](Rendering-and-Fonts).

#### `[window]`

| Key | Default | Behavior |
| --- | --- | --- |
| `cols` | `100` | Initial columns for a new window. |
| `rows` | `30` | Initial rows for a new window. |
| `padding_left` | `12` | Left content padding in logical pixels. |
| `padding_right` | `12` | Right content padding in logical pixels. |
| `padding_top` | `8` | Top content padding in logical pixels. |
| `padding_bottom` | `4` | Bottom content padding in logical pixels. |
| `decorations` | `true` | Enables native title-bar decorations for new windows. |
| `warm_window_pool` | `1` | Number of hidden child windows kept for fast tab tear-out. `0` disables the pool. Hardware rendering caps it at `5`; software rendering caps any nonzero value at `1`. |

`cols` and `rows` set only the initial size. Every native terminal window has a
hard, non-configurable minimum inner size equal to 30 columns by 10 rows. The
pixel floor is recomputed from the live font, DPI, padding, titlebar, and tab-bar
geometry, including after live font/padding reloads and tab-bar visibility
changes.

Grid dimensions are never allowed to allocate without bounds. Each axis is at
most `4096`, the visible grid is at most `524288` cells, and the complete grid
including history is at most `1048576` cells.

#### `[terminal]`

| Key | Default | Behavior |
| --- | --- | --- |
| `shell` | omitted | Shell for new panes. Windows tries `pwsh.exe` from `PATH`, registered PowerShell 7, the real Microsoft Store package, Windows PowerShell, then `cmd.exe`. Unix tries an executable `$SHELL`, the current user’s executable passwd shell, then `/bin/sh`. An explicit non-empty value wins. |
| `term_program` | `"SonicTerm"` | `TERM_PROGRAM` for new child PTYs. `TERM_PROGRAM_VERSION` is SonicTerm’s version, except `term_program = "WezTerm"` advertises `20230712-072601`. |
| `scrollback` | `1000` | Requested history rows per pane. `0` disables history. Grid and retained-byte budgets may lower the effective limit. |
| `clickable_local_targets` | `true` | Allows validated raw local files and directories to activate. On macOS, inert source/script files are reveal-only. URI and OSC 8 links are independent. |
| `clickable_bare_names` | `true` | Allows contextual names to resolve against the exact pane’s trusted local OSC 7 working directory. Separator-relative paths require that same trusted pane CWD. It only works when `clickable_local_targets` is also `true`. |
| `cursor_blink` | `false` | Enables cursor blinking. |
| `cursor_shape` | `"block"` | Accepts `block`, `bar`, or `underline`. |

The scrollback row setting and the memory budget both apply. Rich rows can hit
the byte budget before the row count. See [Memory](Memory).

#### `[appearance]`

| Key | Default | Behavior |
| --- | --- | --- |
| `backdrop` | `"opaque"` | Accepts `opaque`, `mica`, `acrylic`, or `tabbed`. Windows applies the named DWM material on a best-effort basis. Linux starts as `opaque` and warns if another value was requested. macOS treats non-opaque values as alpha-capable windows; the Windows material names do not select a macOS material. |
| `opacity` | `1.0` | Terminal background opacity, clamped to `0.0..=1.0`. |
| `scrollbar` | `"auto"` | Accepts `auto`, `always`, or `never`. `always` is still hidden when there is no history to scroll. |
| `panel_padding` | `2.0` | Inner padding for floating panels in logical pixels. Negative values act as `0`. |
| `software_render_mode` | `"auto"` | `auto` degrades when the adapter is software-rendered, `force` always degrades, and `off` never degrades. |

Software degradation lowers frame and animation cost. On Windows,
`software_render_mode = "force"` also makes new windows opaque because the
software presenter cannot composite transparency. If a non-opaque backdrop was
configured, SonicTerm logs a warning with the configured and applied values.
`auto` does not override the configured backdrop.

The scrollbar thumb can be dragged. Clicking its track moves one viewport.
`auto` shows it during scrolling, dragging, or pointer proximity to the pane’s
right edge.

#### `[accessibility]`

| Key | Default | Behavior |
| --- | --- | --- |
| `high_contrast` | `false` | Replaces the active theme foreground and background with `#ffffff` and `#000000`. |
| `reduced_motion` | `false` | Parsed and retained, but currently has no presentation effect. |
| `strong_focus` | `false` | Parsed and retained, but currently has no presentation effect. |

#### `[notifications]`

| Key | Default | Behavior |
| --- | --- | --- |
| `long_command` | `false` | Enables long-command desktop notifications on Windows. macOS and Linux currently do not send this notification. |
| `threshold_secs` | `10` | A reported command duration must be greater than this value. |

#### `[logging]`

| Key | Default |
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

`level` accepts `error`, `warn`, `info`, or `debug`. Logging is initialized at
startup, so logging changes require a restart. For file locations, cleanup
rules, and diagnostics, see [Logging](Logging).

### Editing and reloading

Use **Edit sonicterm.toml** in the command palette to open the standard config
file. SonicTerm reads it at startup and when you run **Reload Config**. There is
no file watcher.

A reload always re-reads the selected theme and keymap files, even when their
names did not change. The following settings apply to existing windows:

- theme, keymap, and locale;
- font family, size, line height, and weight;
- content padding, opacity, scrollbar, and panel padding;
- cursor shape and blink;
- scrollback and local-target policy;
- tab width, warm-window target, software degradation, accessibility, and
  notification settings.

Some settings affect only objects created after the reload:

- `cols`, `rows`, `decorations`, and the native `backdrop` affect new windows;
- `shell` and `term_program` affect new panes;
- logging settings require a restart.

Changing `backdrop` or `software_render_mode` can involve native window setup.
Restart SonicTerm when you need the complete native-window change, not only the
live renderer policy.

### Saving current font settings

**Save Current Settings** changes only these two values in
`~/.sonicterm/sonicterm.toml`:

```toml
[font]
size = 13
weight_scale = 1.0
```

The saved values are the current session font size and effective
`weight_scale`. The command preserves comments, ordering, line endings, and all
other known or unknown keys. It does not save the session theme or any other
runtime state, and it does not reload because both values are already active.

If the file is missing, SonicTerm creates the starter file first. A
process-local lock and the persistent `sonicterm.toml.save.lock` sidecar prevent
two SonicTerm saves from running together. SonicTerm also compares the exact
file bytes again before replacement. A concurrent editor change, malformed
TOML, invalid font value, or lock conflict refuses the write. The existing file
and reset baselines remain unchanged, and an Error notification appears.

A successful save writes a temporary file in the config directory and replaces
the config atomically. An Info notification confirms the save. This guarantees
that readers see a complete old or new file; it does not guarantee survival of
a sudden power loss.

### Errors and recovery

At startup, an unreadable or malformed config logs a warning and uses defaults
so SonicTerm can still open. An invalid selected theme falls back to bundled
`wezterm`. An invalid selected keymap falls back to the bundled platform
keymap.

During **Reload Config**, an unreadable or malformed `sonicterm.toml` leaves the
entire current config active. If the config itself parses but its theme or
keymap fails, SonicTerm keeps the current theme or keymap, logs the error, and
applies the other valid settings. A structurally valid keymap skips only
bindings whose action cannot be parsed; the other bindings remain active.

## 中文

### 文件与查找顺序

SonicTerm 在所有平台使用同一个配置文件：

```text
~/.sonicterm/sonicterm.toml
```

首次启动会创建这个文件，并在 `~/.sonicterm/themes/` 和
`~/.sonicterm/keymaps/` 中写入可编辑示例。

`theme` 和 `keymap` 可以写名称，也可以写 TOML 路径。使用名称时，SonicTerm
先查找用户目录，再查找内置 `assets/` 目录。看起来像路径的值会直接使用。

SonicTerm 允许并保留 TOML 中的未知 key。当前 build 没有实现的未知 key 不会改变行为。

### 支持的 key 与默认值

#### 顶层

| Key | 默认值 | 行为 |
| --- | --- | --- |
| `theme` | `"wezterm"` | 选择主题。参见 [主题](Themes)。 |
| `keymap` | `"sonicterm-macos"`、`"sonicterm-windows"` 或 `"sonicterm-linux"` | 选择当前平台的 keymap。参见 [快捷键](Keybindings)。 |
| `locale` | `""` | 选择 `en`、`zh-CN` 或 `ja`。空值依次使用 `SONIC_LOCALE`、系统 locale、`en`。 |
| `quit_on_last_window_close` | `true` | macOS 中设为 `false` 后，最后一个窗口关闭时进程仍留在 Dock。其它平台没有窗口时一定退出。 |
| `tab_max_width` | `240` | 单个标签页的最大逻辑像素宽度。非有限值或非正值会被忽略。标签太多时仍会平均分配宽度。 |

#### `[font]`

| Key | 默认值 | 行为 |
| --- | --- | --- |
| `family` | `"Rec Mono St.Helens"` | 主字体族。缺失的字符使用回退字体。 |
| `size` | `13` | 字号，单位为 point。 |
| `line_height` | `1.3` | 行高倍率。 |
| `weight_scale` | `1.0` | 只调整所配置字体族的普通文字粗细。有效范围是 `0.5..=5.0`；其它值会变成 `1.0`。Cell metrics、回退字形、彩色 emoji 和 SGR bold 不变。 |

字体设置会同时用于终端文字和普通应用文字。修改 `family`、`size` 或
`line_height` 时，每个可见 pane 的 grid 与 PTY 都会重新调整大小。只修改
`weight_scale` 不会改变 metrics。字体 shaping 与 fallback 的详细说明见
[渲染与字体](Rendering-and-Fonts)。

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

`cols` 与 `rows` 只设置初始大小。每个原生终端窗口都有不可配置的硬最小内区大小：
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
| `clickable_local_targets` | `true` | 允许操作经过验证的原始本地文件和目录。macOS 上的普通源文件或脚本只能在 Finder 中显示。URI 与 OSC 8 link 不受它控制。 |
| `clickable_bare_names` | `true` | 允许按准确 pane 的可信本机 OSC 7 工作目录解析上下文名称。带分隔符的相对路径也要求同一可信 pane CWD。只有 `clickable_local_targets` 同时为 `true` 时才生效。 |
| `cursor_blink` | `false` | 让光标闪烁。 |
| `cursor_shape` | `"block"` | 可选 `block`、`bar` 或 `underline`。 |

Scrollback 行数与内存预算会同时限制历史记录。包含丰富属性的行可能先达到
字节预算。参见 [内存](Memory)。

#### `[appearance]`

| Key | 默认值 | 行为 |
| --- | --- | --- |
| `backdrop` | `"opaque"` | 可选 `opaque`、`mica`、`acrylic`、`tabbed`。Windows 会尽力应用对应 DWM 材质。Linux 启动时只使用 `opaque`，请求其它值会记录 warning。macOS 只把非 `opaque` 值当作需要 alpha 的窗口；这些 Windows 材质名称不会选择 macOS 材质。 |
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
所以修改这些值后需要重启。文件位置、清理规则和诊断方法见 [日志](Logging)。

### 编辑与重载

在命令面板中执行 **Edit sonicterm.toml** 可以打开标准配置文件。SonicTerm
只在启动和执行 **Reload Config** 时读取它，没有文件 watcher。

每次重载都会重新读取所选主题与 keymap 文件，即使名称没有变化。以下设置会
应用到现有窗口：

- 主题、keymap 与 locale；
- 字体族、字号、行高与字重；
- 内容 padding、opacity、滚动条和 panel padding；
- 光标形状与闪烁；
- scrollback 与本地目标策略；
- 标签页宽度、预热窗口目标、软件降级、无障碍与通知设置。

有些设置只影响重载后新建的对象：

- `cols`、`rows`、`decorations` 和原生 `backdrop` 只影响新窗口；
- `shell` 与 `term_program` 只影响新 pane；
- logging 设置需要重启。

修改 `backdrop` 或 `software_render_mode` 可能涉及原生窗口初始化。如果需要完整
应用原生窗口变化，而不只是更新 renderer 策略，请重启 SonicTerm。

### 保存当前字体设置

**Save Current Settings** 只修改 `~/.sonicterm/sonicterm.toml` 中的两个值：

```toml
[font]
size = 13
weight_scale = 1.0
```

写入的是当前会话字号和当前有效的 `weight_scale`。该命令会保留注释、顺序、
换行格式，以及其它所有已知或未知 key。它不会保存当前会话主题或其它运行状态。
这两个值已经生效，因此保存后不会重载。

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

执行 **Reload Config** 时，如果 `sonicterm.toml` 不可读或格式错误，当前整套配置
保持不变。如果配置本身有效，但主题或 keymap 读取失败，SonicTerm 会保留当前主题
或 keymap、记录错误，并应用其它有效设置。结构正确的 keymap 中，如果只有某个
binding 的 action 无法解析，SonicTerm 只跳过该 binding，其它 binding 仍会生效。
