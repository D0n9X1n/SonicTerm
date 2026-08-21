# Configuration / 配置

## English

SonicTerm reads one cross-platform config file:

```text
~/.sonicterm/sonicterm.toml
```

The file is created on first launch. Named user themes and keymaps live beside
it under `themes/` and `keymaps/`. Loading accepts unknown top-level TOML keys.
The targeted **Save Current Settings** operation described below preserves
unknown top-level and nested keys instead of rewriting the complete config.

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
# Cmd/Ctrl-click asynchronously validated local files and directories.
clickable_local_targets = true
# Also resolve whole bare tokens against the exact pane's trusted OSC 7 CWD.
clickable_bare_names = true
cursor_blink = false
cursor_shape = "block"       # block | bar | underline

[logging]
level = "warn"               # error | warn | info | debug
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2
max_crash_bytes = 10485760        # 10 MiB
max_breadcrumb_files = 10
max_breadcrumb_age_days = 2
max_breadcrumb_bytes = 1048576    # 1 MiB

[appearance]
backdrop = "opaque"           # opaque | mica | acrylic | tabbed
opacity = 1.0
scrollbar = "auto"            # auto | always | never
panel_padding = 2.0
software_render_mode = "auto" # auto | force | off

[render]
# Reserved compatibility key; the current renderer does not read it.
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
text, above `1.0` thickens it, and neither changes cell metrics or replaces
SGR bold.

**`weight_scale` acts on the configured `family` alone.** A character that
family does not contain is drawn from a fallback font, at the weight that
font's own designer chose, and stays there whatever this setting says. That
boundary is deliberate: the setting names one family, and reweighting a
different one applies your intent for your font to a font you never chose. The
mismatch shows wherever the two sit side by side — a fallback glyph growing or
thinning while its neighbour from your family does not move with it.

`weight_scale` works in two stages, and both run at every value other than
`1.0`. The first remaps glyph coverage, which shifts the antialiased edge only.
Because a stem pixel that is already fully opaque can be neither darkened nor
lightened, coverage alone has little effect on HiDPI/Retina displays, where
stem cores are solid. The second changes the rasterized outline — growing above
`1.0`, eroding below it — which adds or removes real ink and stays visible at
any display scale. Growth is cropped back into the original glyph tile and
capped at one raster pixel independently of the glyph's spare bitmap margin.
Tile dimensions, origin, cell metrics, and advances therefore stay fixed, so a
weight change does not double as a size change and flat-sided glyphs are not
suppressed merely because they touch a tile edge. Values around `2.0`-`3.0`
suit most HiDPI screens; `5.0` is deliberately heavy, and `0.5` deliberately
light.

The platform rasterizer policy is internal: DirectWrite is the Windows
default with FreeType fallback, while macOS/other Unix use FreeType. There is
currently no `[font].font_rasterizer` key in `sonicterm.toml`.

`weight_scale` is also adjustable from the command palette without editing the
file: **Increase Font Weight (Bolder)** and **Decrease Font Weight (Thinner)**
step it by `0.25`, and **Reset Font Weight to Config** returns to the configured
value. Searching `bolder`, `thinner`, `heavier`, or `lighter` finds them. Weight
adjustments are session-only unless you run **Save Current Settings**; otherwise
a reload or restart returns to the value in the file. None of the three weight
actions has a default key binding, but all can be bound — see
[Keybindings](Keybindings).

Live `size` and `weight_scale` changes rebuild the shared configured font stack
used by regular terminal text, tab titles, notifications, and the command
palette's query, rows, shortcuts, footer, and other regular chrome. The weight
change applies only to regular glyphs from the configured family: fallback
glyphs, color emoji, and SGR or otherwise intentional bold retain their existing
behavior. Font changes also invalidate text caches. Family, size, and line-height
changes resize each visible pane's grid and PTY using its own pane rectangle;
weight-only changes preserve metrics.

### Window and appearance

`[window].cols` and `rows` define the initial grid for new windows. Padding is in
logical pixels around terminal content. `warm_window_pool` controls hidden,
pre-created child windows used to reduce tab tear-out latency. `0` disables the
pool, while the default `1` retains one instant tear-out spare. Hardware honors
configured targets up to `5`; software rendering caps every nonzero value at
one to bound the per-renderer memory baseline. When a pooled window is adopted,
SonicTerm reapplies the current font (including session weight), theme, tab-bar
visibility, native background, display scale, and size before revealing it, so
its first visible frame cannot expose the stale state captured while it waited.

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

**`force` overrides `backdrop` on Windows.** The software presenter cannot
composite transparency, so a window under `force` is opaque whatever `backdrop`
says — `mica`, `acrylic`, and `tabbed` all resolve to `opaque`. Nothing on
screen distinguishes "the backdrop was applied" from "the backdrop was
overridden", so SonicTerm says so at `warn` level:

```
software_render_mode = force overrides the configured backdrop;
the software presenter cannot composite transparency
  configured=Mica applied=Opaque
```

`auto` does not do this: it degrades rendering when detection finds a software
adapter, and leaves a configured backdrop alone. Only `force` overrides it.

### Terminal

If `shell` is absent, Windows searches for PowerShell 7 (including Store
packages), Windows PowerShell, then cmd; macOS/other Unix use `$SHELL`.
`term_program` becomes the child process's `TERM_PROGRAM` value. Some programs
that do not yet recognize SonicTerm may work with `term_program = "WezTerm"`.

`scrollback` is per pane. Reducing it through reload immediately removes the
oldest history; `0` disables history. Cursor shape and blink settings update
live.

`clickable_local_targets` controls raw absolute, current-home (`~/...`),
separator-relative (`src/main.rs`), explicit dot-relative, and contextual bare
filesystem targets, including forms containing ordinary spaces; it defaults to
`true`. Setting it to `false` leaves URI and OSC 8 activation unchanged.
`clickable_bare_names` is a subordinate default-on switch: it permits row-local
candidates to resolve against the exact pane's trustworthy local OSC 7 CWD only
while `clickable_local_targets` is also enabled. A bounded background probe
checks at most 37 candidates of no more than eight non-space parts and selects
the longest unambiguous openable candidate containing the pointed cell; blocked
or equally long ambiguous candidates fail closed. This intentionally
may activate column text when a same-named eligible entry exists in that CWD;
see [Usage](Usage) for grammar and blocked target classes. Reloading either
switch immediately revokes existing local-target hover and probe authorization
in every window.

### Logging

See [Logging](Logging). `warn` is the default. Debug enables renderer timing and
other diagnostic events.

### Render compatibility

`alt_screen_bg_fill` remains deserializable as a reserved compatibility key,
but the current renderer does not read it. New behavior should not depend on
choosing `v1` or `v2` here.

Older files may contain `glyph_fit = "v1"` or `"v2"`. SonicTerm still accepts
that unknown key so those files load, but it was never connected to the runtime
renderer and is no longer emitted or documented as active configuration. The
single-cell status-marker fit is now a renderer correctness invariant rather
than a user-selectable appearance switch.

### Accessibility and notifications

`high_contrast` is active and reapplies theme colors as white-on-black.
`reduced_motion` and `strong_focus` are currently config-only reserved fields:
SonicTerm preserves and reloads them, but they do not yet change presentation.
`long_command` enables completion notifications for commands exceeding
`threshold_secs`.

## Saving, editing, and reloading

Use the command palette entries **Save Current Settings**, **Edit
sonicterm.toml**, **Edit keymap.toml**, and **Reload Config**.

**Save Current Settings** patches only two values in
`~/.sonicterm/sonicterm.toml`: the current zoomed `[font].size` and the active
safe `[font].weight_scale`. It intentionally leaves theme, locale, every
unrelated known setting, unknown top-level and nested keys, comments, and
supported ordering and formatting intact. It also leaves window, pane, tab, and
other runtime modes untouched. The command does not reload or reapply the file,
because both saved values are already live.

If the config file is missing, SonicTerm creates its commented starter file
before patching the two font values. Saves take a non-blocking process and
cross-process lock on the persistent `sonicterm.toml.save.lock` sidecar, then
recheck the exact bytes read immediately before committing. If another SonicTerm
instance is saving, or the final comparison observes an editor change, SonicTerm
refuses the write and asks you to retry rather than overwriting the newer file. The patch
is written to a temporary file in the config directory and atomically replaces
the config, so readers see the old or new complete file; this does not promise
durability through power loss. A malformed file is also refused. On success, the
font-size and weight reset baselines advance to the saved values and an Info
confirmation appears. On any failure, the existing file, live values, and reset
baselines remain intact, and an Error notification appears.

SonicTerm reads configuration at startup and then only when you run **Reload
Config**. There is no background file watcher, so saving an external edit does
not apply it — this keeps a watcher thread and its filesystem handles out of the
running process. Edit freely, then reload when you want the changes to take
effect.

A reload re-reads `sonicterm.toml` together with the theme and keymap files it
names, even when the `theme` and `keymap` selectors are unchanged. Editing the
contents of a custom theme or custom-named keymap therefore applies on reload;
no rename or restart is needed.

A malformed file is logged and the previous active config remains in use.
Startup is more forgiving: an invalid existing config produces a warning and
starts with defaults so the app remains launchable.

Font, locale, cursor, padding, scrollbar policy, scrollback, theme, and keymap
all apply on reload. Native decorations and certain platform setup may still
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
读取时允许未知顶层 TOML key。下文的定向 **Save Current Settings** 操作会保留未知顶层与
嵌套 key，而不是重写完整配置。

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
# 使用 Cmd/Ctrl 点击经过异步验证的本地文件和目录。
clickable_local_targets = true
# 同时按准确 pane 的可信 OSC 7 CWD 解析完整裸 token。
clickable_bare_names = true
cursor_blink = false
cursor_shape = "block"       # block | bar | underline

[logging]
level = "warn"               # error | warn | info | debug
max_file_size_mb = 10
max_rotated_files = 3
max_age_days = 2
max_crash_dumps = 10
max_crash_age_days = 2
max_crash_bytes = 10485760        # 10 MiB
max_breadcrumb_files = 10
max_breadcrumb_age_days = 2
max_breadcrumb_bytes = 1048576    # 1 MiB

[appearance]
backdrop = "opaque"           # opaque | mica | acrylic | tabbed
opacity = 1.0
scrollbar = "auto"            # auto | always | never
panel_padding = 2.0
software_render_mode = "auto" # auto | force | off

[render]
# 保留的兼容 key；当前 renderer 不读取它。
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
小于 `1.0` 会让文本更细，大于 `1.0` 会让其更粗；两者都不会改变 cell metrics
或替代 SGR bold。

**`weight_scale` 只作用于所配置的 `family`。** 该字体不包含的字符会由回退字体绘制，
其粗细由那个字体自己的设计者决定，无论本设置取何值都保持不变。这条边界是刻意的：
该设置指名的是一个字体族，而去改变另一个字体族，等于把你对自己字体的意图施加到
一个你从未选择的字体上。两者相邻时这种不一致最为明显——回退字形变粗或变细，而
来自你所配置字体的邻居纹丝不动。

`weight_scale` 分两个阶段生效，且在 `1.0` 以外的任何取值下两个阶段都会执行。第一阶段
重映射字形覆盖率，仅影响抗锯齿边缘。由于已经完全不透明的字干像素既无法进一步加深也
无法变浅，在 HiDPI/Retina 屏幕上字干核心本身就是实心的，因此仅靠覆盖率几乎看不出变化。
第二阶段改变光栅化轮廓——大于 `1.0` 时扩张，小于 `1.0` 时收缩——真正增减墨量，在任何
缩放比例下都可见。扩张结果会裁回原始字形 tile，并使用与字形空白边距无关的一个光栅像素
上限。因此 tile 尺寸、原点、cell metrics 与 advance 均保持不变：调整粗细不会同时改变
视觉尺寸，平直边缘的字形也不会仅因接触 tile 边缘而失去加粗效果。HiDPI 屏幕通常适合
`2.0`-`3.0`；`5.0` 属于刻意加粗的极值，`0.5` 则是刻意变细的极值。

平台 rasterizer policy 在内部决定：Windows 默认 DirectWrite 并以 FreeType 回退；
macOS/其它 Unix 使用 FreeType。当前 `sonicterm.toml` 中不存在
`[font].font_rasterizer` key。

`weight_scale` 也可以直接在命令面板中调整，无需编辑文件：**Increase Font Weight
(Bolder)** 和 **Decrease Font Weight (Thinner)** 每次调整 `0.25`，**Reset Font
Weight to Config** 回到配置值。搜索 `bolder`、`thinner`、`heavier`、`lighter` 均可
找到。除非运行 **Save Current Settings**，否则字重调整只在当前会话生效；重载或重启会
回到文件中的值。三种字重 action 默认都没有快捷键，但都可以绑定——参见
[Keybindings](Keybindings)。

运行时改变 `size` 或 `weight_scale` 会重建共享的已配置字体栈；普通终端文本、Tab 标题、
通知，以及命令面板的查询文本、结果行、快捷键提示、footer 和其它普通 chrome 都使用该字体栈。
字重变化只作用于已配置字体族中的普通字形；回退字形、彩色 emoji、SGR 或其它有意加粗的文本
保持原有行为。字体变更还会使文本 cache 失效。family、size、line-height 变更会按各自
pane rect resize 每个可见窗格的 grid 与 PTY；仅修改 weight 不改变 metrics。

### 窗口与外观

`[window].cols`、`rows` 定义新窗口初始 grid。padding 是终端内容周围的逻辑像素。
`warm_window_pool` 控制用于降低 tab tear-out 延迟的隐藏预创建子窗口。设为 `0` 会关闭
该池；默认值 `1` 保留一个可立即 tear-out 的预热窗口。硬件渲染会遵循不超过 `5` 的
配置目标；软件渲染会把任何非零值限制为 `1`，以约束每个 renderer 的内存基线。预热窗口
被采用时，SonicTerm 会先重新应用当前字体（包括会话内字重）、主题、标签栏可见性、原生
背景、显示缩放与尺寸，再将窗口显示出来，因此首个可见帧不会暴露等待期间缓存的旧状态。

当前生效的外观配置是 `[appearance].opacity` 与 `backdrop`。旧的 `[window].opacity`、
`blur` 仍可反序列化，但当前启动与重载路径不会使用它们，不应依赖。Backdrop material
依赖平台；不支持的选项会回退，不改变终端语义。

滚动条支持交互：拖动 thumb 可浏览历史，点击 thumb 上方或下方的 track 会按一整个视口翻页。
`auto` 模式会在滚动/拖动活动或指针靠近窗格右边缘时显示。

`software_render_mode` 控制无 GPU 情况：

- `auto` 跟随 adapter 检测；
- `force` 始终使用降级/软件策略；
- `off` 从不启用降级。

降级会降低帧率与动画成本。Windows 可通过 GDI 合成并呈现确定性的 CPU BGRA frame。

**在 Windows 上，`force` 会覆盖 `backdrop`。** 软件呈现器无法合成透明效果，因此
`force` 下的窗口一律不透明，无论 `backdrop` 配置为何 —— `mica`、`acrylic`、
`tabbed` 都会被解析为 `opaque`。屏幕上无法区分「背景效果已生效」与「背景效果被
覆盖」，因此 SonicTerm 会在 `warn` 级别写出说明：

```
software_render_mode = force overrides the configured backdrop;
the software presenter cannot composite transparency
  configured=Mica applied=Opaque
```

`auto` 不会这样做：它在检测到软件 adapter 时降级渲染，但保留已配置的背景效果。
只有 `force` 会覆盖它。

### 终端

未配置 `shell` 时，Windows 依次查找 PowerShell 7（含 Store package）、Windows PowerShell、cmd；
macOS/其它 Unix 使用 `$SHELL`。`term_program` 写入子进程的 `TERM_PROGRAM`。
某些暂不识别 SonicTerm 的程序可尝试 `term_program = "WezTerm"`。

`scrollback` 按窗格保存；重载调小会立即删除最老历史，`0` 关闭历史。光标形状与 blink 会在重载时生效。

`clickable_local_targets` 控制原始绝对路径、当前用户主目录路径（`~/...`）、带分隔符的
相对路径（`src/main.rs`）、显式点相对路径和 contextual 裸文件系统目标，也支持包含普通空格
的形式；默认为 `true`，设为 `false` 不影响 URI 与 OSC 8 activation。
`clickable_bare_names` 是默认开启的从属 switch：只有 `clickable_local_targets` 同时启用时，
它才允许行内候选按准确 pane 的可信本机 OSC 7 CWD 解析。有界后台 probe 最多检查 37 个候选，
每个候选最多跨越 8 个非空格部分，并选取包含鼠标 cell 的最长、无歧义且可打开候选；被阻止或
同长度有歧义的候选会 fail closed。这会有意允许列文本
在 CWD 中存在同名合格条目时变为目标；grammar 与阻止的目标类别见 [用法 / Usage](Usage)。
重载任一 switch 都会立即撤销所有窗口中已有的本地目标 hover 与 probe authorization。

### 日志

见 [日志 / Logging](Logging)。默认 `warn`；`debug` 会启用 renderer timing 与更多诊断事件。

### Render 兼容设置

`alt_screen_bg_fill` 仍可反序列化为保留的兼容 key，但当前 renderer 不读取它。新行为
不应依赖在这里选择 `v1` 或 `v2`。

旧配置可能包含 `glyph_fit = "v1"` 或 `"v2"`。SonicTerm 仍把这个未知 key 作为兼容
输入接受，因此旧文件可以继续加载；但它从未连接到运行时 renderer，现在也不再由配置
序列化或文档列为生效设置。单 cell 状态标记的适配现在是 renderer 正确性约束，而不是
用户可选的外观开关。

### 无障碍与通知

`high_contrast` 已生效，会把主题重新应用为白字黑底。`reduced_motion` 和 `strong_focus`
目前是仅配置层保留字段：SonicTerm 会保存并在重载时应用它们，但尚不会改变呈现。
`long_command` 为运行超过 `threshold_secs` 的命令启用完成通知。

## 保存、编辑与重载

使用命令面板中的 **Save Current Settings**、**Edit sonicterm.toml**、**Edit
keymap.toml** 和 **Reload Config**。

**Save Current Settings** 只会修补 `~/.sonicterm/sonicterm.toml` 中的两个值：当前缩放后的
`[font].size` 与当前有效且安全的 `[font].weight_scale`。它会有意保留主题、locale、所有
无关的已知设置、未知顶层与嵌套 key、注释，以及受支持的顺序和格式；窗口、窗格、Tab 与其它
运行时模式也不会变化。该命令不重载或重新应用文件，因为保存的两个值已经实时生效。

若配置文件不存在，SonicTerm 会先创建带注释的初始配置，再修补这两个字体值。保存时会以
非阻塞方式取得进程内锁和跨进程锁；跨进程锁使用持久的 `sonicterm.toml.save.lock` sidecar，
并在提交前立即重新核对最初读取的精确字节。若另一个 SonicTerm 实例正在保存，或最终核对发现
编辑器已经修改配置，SonicTerm 会拒绝写入并提示重试，而不会覆盖较新的文件。修补内容先写入配置目录
中的临时文件，再以原子方式替换配置，因此读取者会看到完整旧文件或完整新文件；这不承诺断电后
的持久性。格式错误的文件同样会被拒绝。成功时，字号与字重的重置基线会推进到所保存的值，并
显示 Info 确认；任何失败都会保持已有文件、实时值与重置基线不变，并显示 Error 通知。

SonicTerm 只在启动时读取配置，之后仅在运行 **Reload Config** 时重新读取。没有后台
文件 watcher，因此保存外部编辑不会自动生效——这样运行进程中就不存在 watcher 线程及其
文件系统句柄。可以自由编辑，需要生效时再重载。

重载会连同 `sonicterm.toml` 一起重新读取它所指定的主题与 keymap 文件，即使 `theme`
和 `keymap` 选择器没有变化。因此修改自定义主题或自定义命名 keymap 的文件内容也会在
重载时生效，无需改名或重启。

文件损坏时会记录错误并继续使用旧配置。启动更宽容：已有配置损坏时发 warning 并以默认值
启动，保证 app 可打开。

字体、locale、光标、padding、scrollbar policy、scrollback、主题和 keymap 都会在重载时
生效。原生 decorations 和部分平台初始化仍可能需要新窗口或重启。

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
