# Themes / 主题

## English

### Theme files and selection

SonicTerm themes are TOML files. Bundled themes live in `assets/themes/`:

- `catppuccin-mocha`
- `dracula`
- `gruvbox-dark-hard`
- `monokai-pro`
- `nord`
- `one-dark`
- `solarized-dark`
- `tokyo-night`
- `wezterm`

Editable user themes live in:

```text
~/.sonicterm/themes/
```

Select a theme in `~/.sonicterm/sonicterm.toml`:

```toml
theme = "wezterm"
```

A name first checks `~/.sonicterm/themes/<name>.toml`, then bundled assets. A
path-like value is used directly. See [Configuration](Configuration) for config
reload rules.

### Create and apply a theme

Copy the seeded default, edit it, select it, then reload:

```sh
cp ~/.sonicterm/themes/wezterm.toml ~/.sonicterm/themes/my-theme.toml
```

```toml
theme = "my-theme"
```

Run **Reload Config** after saving. SonicTerm re-reads the selected theme file on
every reload, so its name does not need to change.

An `apply_theme` keymap action can change the active theme for the current
session:

```toml
[[binding]]
keys = "super+shift+1"
action = { apply_theme = "nord" }
```

This action does not write `sonicterm.toml`. The next config reload uses the
theme named in that file. See [Keybindings](Keybindings) for binding syntax.

### Schema

A complete theme has this shape:

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

`appearance` accepts `light` or `dark`. It is a palette hint used when SonicTerm
derives UI colors such as hyperlink tint strength. Window transparency and
native materials belong to `[appearance]` in `sonicterm.toml`, not to the theme.

Use six-digit RGB strings in `#rrggbb` form. The TOML loader accepts any string,
but a malformed color renders as black. Missing required fields make the theme
fail to parse. `colors.tab.hover_fg` is the only color slot with a schema
default; when omitted, it becomes `#d5c4a1`.

### Color application

| Slot | Current use |
| --- | --- |
| `background` | Terminal background and the base for application chrome |
| `foreground` | Default terminal and application text |
| `cursor` | Cursor, link underline, and link tint source |
| `cursor_text` | Character under a block cursor |
| `selection_bg` | Selection overlay, drawn at 50% alpha |
| `colors.ansi` | ANSI colors 0–7 and derived UI accents |
| `colors.bright` | ANSI colors 8–15 and derived UI accents |
| `colors.tab.bar_bg` | Tab-bar and search-panel background source |
| `colors.tab.active_bg` / `active_fg` | Active tab |
| `colors.tab.inactive_bg` / `inactive_fg` | Inactive tabs and separators |

Search uses `colors.ansi.yellow` for every match and
`colors.bright.green` for the current match. Both use `background` for the text
on top of the highlight.

The current renderer does not read `selection_fg`, `colors.tab.hover_bg`,
`colors.tab.hover_fg`, or `colors.tab.close_button_fg`. Keep them in custom
files because the current theme schema still requires all except `hover_fg`.
Their values currently have no visual effect.

`accessibility.high_contrast = true` is applied after theme loading. It replaces
only `foreground` with `#ffffff` and `background` with `#000000`; the ANSI,
cursor, selection, and tab slots remain from the theme.

### Reload and failure behavior

A successful reload applies the palette to every window and pane. It also
updates terminal OSC color replies and invalidates text and line caches so the
next frame uses the new colors.

At startup, a missing, unreadable, or malformed selected theme logs a warning
and falls back to bundled `wezterm`. During **Reload Config** or an
`apply_theme` action, a failed theme load logs the error and leaves the current
rendered theme active.

## 中文

### 主题文件与选择

SonicTerm 的主题是 TOML 文件。内置主题位于 `assets/themes/`：

- `catppuccin-mocha`
- `dracula`
- `gruvbox-dark-hard`
- `monokai-pro`
- `nord`
- `one-dark`
- `solarized-dark`
- `tokyo-night`
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
资产。看起来像路径的值会直接使用。配置重载规则见 [配置](Configuration)。

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
Binding 格式见 [快捷键](Keybindings)。

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
| `colors.tab.bar_bg` | 标签栏和搜索面板背景来源 |
| `colors.tab.active_bg` / `active_fg` | 当前标签页 |
| `colors.tab.inactive_bg` / `inactive_fg` | 非当前标签页和分隔线 |

搜索的所有命中使用 `colors.ansi.yellow`，当前命中使用
`colors.bright.green`。两者上方的文字都使用 `background`。

当前 renderer 不读取 `selection_fg`、`colors.tab.hover_bg`、
`colors.tab.hover_fg` 或 `colors.tab.close_button_fg`。自定义主题仍应保留这些
字段，因为当前 schema 除 `hover_fg` 外仍要求它们存在。它们目前没有视觉效果。

主题加载后还会应用 `accessibility.high_contrast = true`。该设置只把
`foreground` 改为 `#ffffff`、把 `background` 改为 `#000000`；ANSI、光标、
选区和标签页字段仍来自主题。

### 重载与失败行为

重载成功后，palette 会应用到所有窗口和 pane。终端的 OSC 颜色回复也会更新。
SonicTerm 还会清除文字与行 cache，让下一帧使用新颜色。

启动时，如果所选主题不存在、不可读或 TOML 格式错误，SonicTerm 会记录 warning，
并回退到内置 `wezterm`。执行 **Reload Config** 或 `apply_theme` action 时，主题
加载失败会记录错误，并继续显示当前主题。
