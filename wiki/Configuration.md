# Configuration / 配置

## English

SonicTerm reads one cross-platform config file:

```text
~/.sonicterm/sonicterm.toml
```

The file is created on first launch. Named user themes and keymaps live beside
it under `themes/` and `keymaps/`. Unknown top-level TOML keys are preserved
when SonicTerm loads and saves the config, helping newer keys survive an older
round trip.

## Complete starter example

The example below mirrors the currently generated starter surface. Defaults are
shown explicitly; omit fields you do not want to override.

```toml
theme = "wezterm"
# Platform default: sonicterm-macos / sonicterm-windows / sonicterm-linux
keymap = "sonicterm-macos"
# Empty means: SONIC_LOCALE, then OS locale, then English.
locale = ""
# macOS can remain active in the Dock when this is false. Other platforms exit
# when no windows remain.
quit_on_last_window_close = true
tab_max_width = 240

[font]
family = "Rec Mono St.Helens"
size = 13
line_height = 1.3
weight_scale = 1.0

[window]
cols = 100
rows = 30
padding_left = 12
padding_right = 12
padding_top = 8
padding_bottom = 4
decorations = true
# Legacy compatibility keys; prefer [appearance] for new settings.
opacity = 1.0
blur = false
warm_window_pool = 1

[terminal]
# Omit to auto-detect. Windows: pwsh -> Windows PowerShell -> cmd.
# macOS/Linux: $SHELL.
# shell = "pwsh.exe"
term_program = "SonicTerm"
scrollback = 1000
cursor_blink = false
cursor_shape = "block"       # block | bar | underline

[logging]
level = "warn"               # error | warn | info | debug
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2

[appearance]
backdrop = "opaque"           # opaque | mica | acrylic | tabbed
opacity = 1.0
scrollbar = "auto"            # auto | always | never
panel_padding = 2.0
software_render_mode = "auto" # auto | force | off

[render]
# Keep v2 unless bisecting a rendering regression.
glyph_fit = "v2"
alt_screen_bg_fill = "v2"

[accessibility]
high_contrast = false
reduced_motion = false
strong_focus = false

[notifications]
long_command = false
threshold_secs = 10
```

## Sections and behavior

### Top-level selection

- `theme` accepts a name or a direct TOML path. Named lookup checks
  `~/.sonicterm/themes/<name>.toml` before bundled assets.
- `keymap` accepts a name or direct TOML path and follows the same user-before-bundled rule.
- `locale` supports the bundled `en`, `zh-CN`, and `ja` catalogs. An empty value negotiates automatically.
- `tab_max_width` caps a single tab in logical pixels; tabs still shrink evenly when the bar is crowded.
- `quit_on_last_window_close=false` keeps the macOS process available from the Dock after its final window closes.

`tab_close_button_color` is accepted only for compatibility with older config
files. The close button is no longer drawn, so this key currently has no visual
effect and should not be added to new configurations.

### Font

`family`, `size`, `line_height`, and `weight_scale` are the public SonicTerm
font config. `weight_scale = 1.0` preserves native glyph coverage. Accepted
values are `0.5..=5.0`; invalid values fall back to `1.0`. Below `1.0` thins
regular text, above `1.0` thickens it, and neither changes cell metrics or
replaces SGR bold.

`weight_scale` works in two stages. Small adjustments near `1.0` (such as `1.1`)
remap glyph coverage, which shifts the antialiased edge only. Because a stem
pixel that is already fully opaque cannot be darkened further, coverage alone
has little effect on HiDPI/Retina displays, where stem cores are solid. Larger
values additionally grow the glyph outline, which adds real ink and stays
visible at any display scale. Values around `2.0`-`3.0` suit most HiDPI screens;
`5.0` is deliberately heavy.

The platform rasterizer policy is internal: DirectWrite is the Windows
default with FreeType fallback, while macOS/other Unix use FreeType. There is
currently no `[font].font_rasterizer` key in `sonicterm.toml`.

Changing font fields live updates every renderer and invalidates text caches.
Family, size, and line-height changes also resize each visible pane's grid and
PTY using its own pane rectangle; weight-only changes preserve metrics.

### Window and appearance

`[window].cols` and `rows` define the initial grid for new windows. Padding is in
logical pixels around terminal content. `warm_window_pool` controls hidden,
pre-created child windows used to reduce tab tear-out latency. `0` disables the
pool, while the default `1` retains one instant tear-out spare. Hardware honors
configured targets up to `5`; software rendering caps every nonzero value at
one to bound the per-renderer memory baseline.

Use `[appearance].opacity` and `backdrop` for active appearance configuration.
The older `[window].opacity` and `blur` fields still deserialize, but current
startup and reload paths do not use them; they should not be relied upon.
Backdrop materials are platform-dependent; unsupported choices fall back rather
than changing terminal semantics.

The scrollbar is interactive: drag its thumb to move through history, or click
above/below the thumb to page by one viewport. In `auto` mode it appears around
scroll/drag activity or pointer proximity to the pane's right edge.

`software_render_mode` controls no-GPU handling:

- `auto` follows adapter detection;
- `force` always uses the degraded/software policy;
- `off` never engages it.

The degraded path lowers frame frequency and animation cost. Windows can compose
and present a deterministic CPU BGRA frame through GDI.

### Terminal

If `shell` is absent, Windows searches for PowerShell 7 (including Store
packages), Windows PowerShell, then cmd; macOS/other Unix use `$SHELL`.
`term_program` becomes the child process's `TERM_PROGRAM` value. Some programs
that do not yet recognize SonicTerm may work with `term_program = "WezTerm"`.

`scrollback` is per pane. Reducing it through reload immediately removes the
oldest history; `0` disables history. Cursor shape and blink settings update
live.

### Logging

See [Logging](Logging). `warn` is the default. Debug enables renderer timing and
other diagnostic events.

### Render switches

`glyph_fit` and `alt_screen_bg_fill` select the current `v2` behavior or a
legacy `v1` fallback for regression diagnosis. They are implementation rollback
switches, not appearance preferences; keep `v2` unless investigating a specific
rendering problem.

### Accessibility and notifications

`high_contrast` is active and reapplies theme colors as white-on-black.
`reduced_motion` and `strong_focus` are currently config-only reserved fields:
SonicTerm preserves and reloads them, but they do not yet change presentation.
`long_command` enables completion notifications for commands exceeding
`threshold_secs`.

## Editing and hot reload

Use the command palette entries **Edit sonicterm.toml**, **Edit keymap.toml**,
and **Reload Config**. SonicTerm also watches the config directory. Editors
commonly save by remove-and-rename, so the watcher observes the parent directory,
coalesces 80 ms event bursts, and wakes the winit loop immediately.

A malformed hot-reload file is logged and the previous active config remains in
use. Startup is more forgiving: an invalid existing config produces a warning
and starts with defaults so the app remains launchable.

Font, locale, cursor, padding, scrollbar policy, scrollback, and selector
changes for `theme`/`keymap` are live. Editing the contents of the currently
selected custom theme or custom-named keymap without changing its selector does
not reliably reload that asset today. The file watcher observes
`sonicterm.toml` and the platform-default keymap basename, not arbitrary active
theme/keymap files. To apply a custom asset edit, change/reselect its name or
restart after saving. Native decorations and certain platform setup may also
require a new window or restart.

## Layout diagrams

```text
+----------------------- window -----------------------+
| tab/title UI                                         |
| padding_top                                          |
| padding_left  terminal pane grid  padding_right      |
| padding_bottom                                       |
+------------------------------------------------------+

+------------- floating panel -------------+
| panel_padding                           |
| command/search content                  |
| panel_padding                           |
+-----------------------------------------+
```

## 中文

SonicTerm 使用一个跨平台配置文件：

```text
~/.sonicterm/sonicterm.toml
```

首次启动会创建该文件。同级的 `themes/` 与 `keymaps/` 保存命名用户主题和键位映射。
SonicTerm 在读写配置时会保留未知顶层 TOML key，避免较新 key 被旧版本往返保存时丢失。

## 完整初始示例

下面示例对应当前生成的 starter surface，并显式写出默认值；不需要覆盖的字段可以省略。

```toml
theme = "wezterm"
# 平台默认：sonicterm-macos / sonicterm-windows / sonicterm-linux
keymap = "sonicterm-macos"
# 空字符串表示：SONIC_LOCALE -> OS locale -> English。
locale = ""
# macOS 中设为 false 时，最后一个窗口关闭后可继续留在 Dock；其它平台无窗口时退出。
quit_on_last_window_close = true
tab_max_width = 240

[font]
family = "Rec Mono St.Helens"
size = 13
line_height = 1.3
weight_scale = 1.0

[window]
cols = 100
rows = 30
padding_left = 12
padding_right = 12
padding_top = 8
padding_bottom = 4
decorations = true
# 旧版兼容 key；新配置优先使用 [appearance]。
opacity = 1.0
blur = false
warm_window_pool = 1

[terminal]
# 省略则自动探测。Windows：pwsh -> Windows PowerShell -> cmd；
# macOS/Linux：$SHELL。
# shell = "pwsh.exe"
term_program = "SonicTerm"
scrollback = 1000
cursor_blink = false
cursor_shape = "block"       # block | bar | underline

[logging]
level = "warn"               # error | warn | info | debug
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2

[appearance]
backdrop = "opaque"           # opaque | mica | acrylic | tabbed
opacity = 1.0
scrollbar = "auto"            # auto | always | never
panel_padding = 2.0
software_render_mode = "auto" # auto | force | off

[render]
# 除非定位渲染回归，否则保持 v2。
glyph_fit = "v2"
alt_screen_bg_fill = "v2"

[accessibility]
high_contrast = false
reduced_motion = false
strong_focus = false

[notifications]
long_command = false
threshold_secs = 10
```

## 配置分区与行为

### 顶层选择

- `theme` 接受名称或直接 TOML 路径。命名查找先看 `~/.sonicterm/themes/<name>.toml`，再看内置资产。
- `keymap` 同样接受名称或直接路径，并遵循用户文件优先。
- `locale` 支持内置 `en`、`zh-CN`、`ja`；空值自动协商。
- `tab_max_width` 限制单个标签页逻辑像素宽度；标签拥挤时仍会平均缩小。
- `quit_on_last_window_close=false` 允许 macOS 最后窗口关闭后进程仍留在 Dock。

`tab_close_button_color` 仅为兼容旧配置而继续接受。当前不再绘制关闭按钮，因此该 key
没有视觉效果，不应加入新配置。

### 字体

`family`、`size`、`line_height`、`weight_scale` 是 SonicTerm 对外字体配置。
`weight_scale = 1.0` 保持字体原始覆盖率。允许范围为 `0.5..=5.0`；无效值回退为 `1.0`。
小于 `1.0` 会让常规文本更细，大于 `1.0` 会让其更粗；两者都不会改变 cell metrics
或替代 SGR bold。

`weight_scale` 分两个阶段生效。接近 `1.0` 的微调（例如 `1.1`）只重映射字形覆盖率，
仅影响抗锯齿边缘。由于已经完全不透明的字干像素无法进一步加深，在 HiDPI/Retina
屏幕上字干核心本身就是实心的，因此仅靠覆盖率几乎看不出变化。更大的取值会额外扩张
字形轮廓，真正增加墨量，在任何缩放比例下都可见。HiDPI 屏幕通常适合 `2.0`-`3.0`；
`5.0` 属于刻意加粗的极值。

平台 rasterizer policy 在内部决定：Windows 默认 DirectWrite 并以 FreeType 回退；
macOS/其它 Unix 使用 FreeType。当前 `sonicterm.toml` 中不存在
`[font].font_rasterizer` key。

运行时改变字体会更新全部 renderer 并使文本 cache 失效。family、size、line-height
变更还会按各自 pane rect resize 每个可见窗格的 grid 与 PTY；仅修改 weight 不改变 metrics。

### 窗口与外观

`[window].cols`、`rows` 定义新窗口初始 grid。padding 是终端内容周围的逻辑像素。
`warm_window_pool` 控制用于降低 tab tear-out 延迟的隐藏预创建子窗口。设为 `0` 会关闭
该池；默认值 `1` 保留一个可立即 tear-out 的预热窗口。硬件渲染会遵循不超过 `5` 的
配置目标；软件渲染会把任何非零值限制为 `1`，以约束每个 renderer 的内存基线。

当前生效的外观配置是 `[appearance].opacity` 与 `backdrop`。旧的 `[window].opacity`、
`blur` 仍可反序列化，但当前启动与热重载路径不会使用它们，不应依赖。Backdrop material
依赖平台；不支持的选项会回退，不改变终端语义。

滚动条支持交互：拖动 thumb 可浏览历史，点击 thumb 上方或下方的 track 会按一整个视口翻页。
`auto` 模式会在滚动/拖动活动或指针靠近窗格右边缘时显示。

`software_render_mode` 控制无 GPU 情况：

- `auto` 跟随 adapter 检测；
- `force` 始终使用降级/软件策略；
- `off` 从不启用降级。

降级会降低帧率与动画成本。Windows 可通过 GDI 合成并呈现确定性的 CPU BGRA frame。

### 终端

未配置 `shell` 时，Windows 依次查找 PowerShell 7（含 Store package）、Windows PowerShell、cmd；
macOS/其它 Unix 使用 `$SHELL`。`term_program` 写入子进程的 `TERM_PROGRAM`。
某些暂不识别 SonicTerm 的程序可尝试 `term_program = "WezTerm"`。

`scrollback` 按窗格保存；热重载调小会立即删除最老历史，`0` 关闭历史。光标形状与 blink 可热更新。

### 日志

见 [日志 / Logging](Logging)。默认 `warn`；`debug` 会启用 renderer timing 与更多诊断事件。

### Render switch

`glyph_fit` 和 `alt_screen_bg_fill` 可选择当前 `v2` 或用于回归定位的旧 `v1`。
它们是实现 rollback switch，不是外观偏好；除非调查具体渲染问题，否则保持 `v2`。

### 无障碍与通知

`high_contrast` 已生效，会把主题重新应用为白字黑底。`reduced_motion` 和 `strong_focus`
目前是仅配置层保留字段：SonicTerm 会保存并热重载它们，但尚不会改变呈现。
`long_command` 为运行超过 `threshold_secs` 的命令启用完成通知。

## 编辑与热重载

使用命令面板中的 **Edit sonicterm.toml**、**Edit keymap.toml**、**Reload Config**。
SonicTerm 也会监控配置目录。由于编辑器常用 remove-and-rename 保存，watcher 监控父目录、合并 80 ms 事件 burst，
并立即唤醒 winit loop。

热重载文件损坏时会记录错误并继续使用旧配置。启动更宽容：已有配置损坏时发 warning 并以默认值启动，保证 app 可打开。

字体、locale、光标、padding、scrollbar policy、scrollback，以及 `theme`/`keymap`
选择器变化可以热更新。但如果不改变选择器，只修改当前自定义主题或自定义命名 keymap 的文件内容，
目前不能可靠重载。watcher 只监控 `sonicterm.toml` 和平台默认 keymap 的 basename，不监控任意活动
主题/keymap 文件。应用自定义资产修改时，请改变/重新选择名称，或保存后重启。原生 decorations
和部分平台初始化也可能需要新窗口或重启。

## 布局示意

```text
+----------------------- window -----------------------+
| tab/title UI                                         |
| padding_top                                          |
| padding_left  terminal pane grid  padding_right      |
| padding_bottom                                       |
+------------------------------------------------------+

+------------- floating panel -------------+
| panel_padding                           |
| command/search content                  |
| panel_padding                           |
+-----------------------------------------+
```
