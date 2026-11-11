# 主题

[English](Themes)

### 主题文件与选择

SonicTerm 的主题是 TOML 文件。内置主题位于 `assets/themes/`：

- `catppuccin-latte`（浅色）
- `catppuccin-mocha`
- `dracula`
- `github-light`（浅色）
- `gruvbox-dark-hard`
- `monokai-pro`
- `nord`
- `one-dark`
- `rose-pine-dawn`（浅色）
- `solarized-dark`
- `tokyo-night`
- `tokyo-night-day`（浅色）
- `wezterm`

用户可编辑主题位于：

```text
~/.sonicterm/themes/
```

在 `~/.sonicterm/sonicterm.toml` 中选择主题：

```toml
theme = "wezterm"
```

使用名称时，SonicTerm 先查找 `~/.sonicterm/themes/<name>.toml`，再查找内置
资产。看起来像路径的值会直接使用。配置重载规则见 [配置](Configuration-zh-CN)。

### 预览和切换主题

命令面板中的 **Theme: Choose…**（`choose_theme`）列出所有内置主题，以及
`~/.sonicterm/themes/` 中的所有 `*.toml` 主题：先浅色、后深色，各自按名称排序。
同名的用户主题会替换内置主题。每一行显示主题名和“浅色”或“深色”；当前主题标有
`✓`。输入文字可以筛选，输入“浅色”或“深色”可只看一类主题。

用方向键移动高亮时，高亮的主题会立即应用到所有窗口。按 Enter 或点击会在本次会话中
保留它；按 Escape 或点击面板外会恢复打开选择器时的主题，即使该主题文件在此期间被修改
或删除。无法解析的文件不会列出，日志会写明文件名。选择器打开时执行 **Reload Config**
会关闭选择器，并应用 `sonicterm.toml` 指定的主题。选择器不会写入 `sonicterm.toml`；
要在重启后保留主题，请在其中设置 `theme`。

### 创建并应用主题

复制初始主题，修改文件，选择它，再重载：

```sh
cp ~/.sonicterm/themes/wezterm.toml ~/.sonicterm/themes/my-theme.toml
```

```toml
theme = "my-theme"
```

保存后执行 **Reload Config**。每次重载都会重新读取当前主题文件，所以不需要修改
主题名称。

也可以通过 keymap 中的 `apply_theme` action 临时切换当前会话主题：

```toml
[[binding]]
keys = "super+shift+1"
action = { apply_theme = "nord" }
```

该 action 不会写入 `sonicterm.toml`。下次重载仍使用配置文件中指定的主题。
Binding 格式见 [快捷键](Keybindings-zh-CN)。

### Schema

完整主题使用以下结构：

```toml
name = "My Theme"
appearance = "dark"

[colors]
background = "#141617"
foreground = "#d5c4a1"
cursor = "#fabd2f"
cursor_text = "#141617"
selection_bg = "#3c3836"
selection_fg = "#d5c4a1"

[colors.ansi]
black = "#1d2021"
red = "#fb4934"
green = "#b8bb26"
yellow = "#fabd2f"
blue = "#83a598"
magenta = "#d3869b"
cyan = "#8ec07c"
white = "#d5c4a1"

[colors.bright]
black = "#665c54"
red = "#fb4934"
green = "#b8bb26"
yellow = "#fabd2f"
blue = "#83a598"
magenta = "#d3869b"
cyan = "#8ec07c"
white = "#fbf1c7"

[colors.tab]
bar_bg = "#141617"
active_bg = "#141617"
active_fg = "#fabd2f"
inactive_bg = "#141617"
inactive_fg = "#928374"
hover_bg = "#1c1f20"
hover_fg = "#d5c4a1"
close_button_fg = "#ff5555"
```

`appearance` 可选 `light` 或 `dark`。SonicTerm 用它作为 palette 提示，推导 link
tint 强度等 UI 颜色。窗口透明度和原生材质属于 `sonicterm.toml` 中的
`[appearance]`，不属于主题文件。

颜色应使用六位 RGB 字符串 `#rrggbb`。TOML loader 会接受任意字符串，但格式错误的
颜色会显示为黑色。缺少必需字段会导致主题解析失败。只有
`colors.tab.hover_fg` 有 schema 默认值；省略时为 `#d5c4a1`。

### 颜色应用

| 字段 | 当前用途 |
| --- | --- |
| `background` | 终端背景和应用 UI 的基础背景 |
| `foreground` | 默认终端文字和应用文字 |
| `cursor` | 光标、link 下划线和 link tint 来源 |
| `cursor_text` | 块状光标下的字符 |
| `selection_bg` | 选区 overlay，以 50% alpha 绘制 |
| `colors.ansi` | ANSI 颜色 0–7 和派生 UI accent |
| `colors.bright` | ANSI 颜色 8–15 和派生 UI accent |
| `colors.tab.bar_bg` | 标签栏、状态栏、集成标题栏带和搜索面板的背景，按主题设置的值原样使用。SonicTerm 不会从终端背景色推导更暗的颜色，也不做对比度筛选，因此请选择能让 `foreground` 和 `colors.tab.active_fg` 清晰可读的 `bar_bg`。 |
| `colors.tab.active_bg` / `active_fg` | 当前标签页 |
| `colors.tab.inactive_bg` / `inactive_fg` | 非当前标签页和分隔线 |

搜索的所有命中使用 `colors.ansi.yellow`，当前命中使用
`colors.bright.green`。两者上方的文字都使用 `background`。

自定义主题应保留 `selection_fg`、`colors.tab.hover_bg`、`colors.tab.hover_fg` 和
`colors.tab.close_button_fg`：除 `hover_fg` 外都为 schema 必需字段，尽管目前均不影响渲染。

主题加载后还会应用 `accessibility.high_contrast = true`。该设置把
`foreground` 改为 `#ffffff`、把 `background` 改为 `#000000`，并把 `colors.tab`
各字段改为黑底白字：`bar_bg`、`active_bg`、`inactive_bg` 为 `#000000`，`hover_bg`
为 `#333333`，`active_fg`、`hover_fg` 为 `#ffffff`，`inactive_fg` 为 `#c0c0c0`。
ANSI、光标和选区颜色仍来自主题。

### 重载与失败行为

重载成功后，palette 会应用到所有窗口和 pane。终端的 OSC 颜色回复也会更新。
SonicTerm 还会清除文字与行 cache，让下一帧使用新颜色。

启动时，如果所选主题不存在、不可读或 TOML 格式错误，SonicTerm 会记录 warning，
并回退到内置 `wezterm`。执行 **Reload Config** 或 `apply_theme` action 时，主题
加载失败会记录错误，并继续显示当前主题。
