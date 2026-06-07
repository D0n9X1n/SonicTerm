# Themes / 主题

## English

SonicTerm themes are TOML files. Bundled themes live in `assets/themes/`, and
editable user themes live in:

```text
~/.sonicterm/themes/
```

The active theme is selected from `~/.sonicterm/sonicterm.toml`:

```toml
theme = "wezterm"
```

You can also point `theme` at any TOML file path.

### Create a custom theme

1. Copy the seeded default theme:

   ```sh
   cp ~/.sonicterm/themes/wezterm.toml ~/.sonicterm/themes/my-theme.toml
   ```

2. Edit `~/.sonicterm/themes/my-theme.toml`.
3. Set the theme name in `~/.sonicterm/sonicterm.toml`:

   ```toml
   theme = "my-theme"
   ```

4. Run **Reload Config** from the command palette.

### Theme file shape

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

### Color slots

| Slot | Used for |
| --- | --- |
| `background` | Terminal background and default chrome base |
| `foreground` | Default terminal text |
| `cursor` / `cursor_text` | Cursor block and character under the cursor |
| `selection_bg` / `selection_fg` | Text selection |
| `colors.ansi` | Normal ANSI colors 0-7 |
| `colors.bright` | Bright ANSI colors 8-15 |
| `colors.tab` | Tab bar, active/inactive tabs, hover state, close button |

Search highlighting also follows the active theme: all matches use the theme's
yellow with background-colored text, while the current match uses the theme's
bright green with background-colored text.

### Theme design tips

- Keep `background` and `foreground` high contrast enough for long terminal
  sessions.
- Make `cursor` stand out from `background`, and set `cursor_text` to a color
  readable on top of `cursor`.
- Tune tab colors as part of the theme, not as a separate afterthought. The tab
  bar should feel like it belongs to the same palette as the terminal.
- Use `ansi.yellow` and `bright.green` intentionally because search highlighting
  derives from them.
- Use only `#rrggbb` colors.

If a theme file fails to parse, SonicTerm logs the error and falls back to the
bundled `wezterm` theme.

## 中文

SonicTerm 的主题是 TOML 文件。内置主题在 `assets/themes/`，用户可编辑主题在：

```text
~/.sonicterm/themes/
```

当前主题由 `~/.sonicterm/sonicterm.toml` 决定：

```toml
theme = "wezterm"
```

也可以把 `theme` 写成任意 TOML 文件路径。

### 制作自定义主题

1. 复制首次启动时生成的默认主题：

   ```sh
   cp ~/.sonicterm/themes/wezterm.toml ~/.sonicterm/themes/my-theme.toml
   ```

2. 编辑 `~/.sonicterm/themes/my-theme.toml`。
3. 在 `~/.sonicterm/sonicterm.toml` 里启用它：

   ```toml
   theme = "my-theme"
   ```

4. 打开命令面板，执行 **Reload Config**。

### 主题文件结构

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

### 颜色字段说明

| 字段 | 用途 |
| --- | --- |
| `background` | 终端背景和 UI chrome 的基础背景 |
| `foreground` | 默认终端文字 |
| `cursor` / `cursor_text` | 光标块和光标下文字 |
| `selection_bg` / `selection_fg` | 文本选区 |
| `colors.ansi` | 普通 ANSI 颜色 0-7 |
| `colors.bright` | 高亮 ANSI 颜色 8-15 |
| `colors.tab` | Tab bar、active/inactive tab、hover 状态、关闭按钮 |

搜索高亮也跟随当前主题：所有命中使用主题里的黄色，并用背景色作为文字颜色；
当前选中命中使用主题里的 bright green，并同样用背景色作为文字颜色。

### 主题设计建议

- `background` 和 `foreground` 要有足够对比度，适合长时间看代码和终端输出。
- `cursor` 要能从背景中跳出来，`cursor_text` 要在 `cursor` 上清晰可读。
- Tab 颜色也应该是主题的一部分，不要和终端配色割裂。
- `ansi.yellow` 和 `bright.green` 会影响搜索高亮，所以要认真选择。
- 颜色只使用 `#rrggbb` 格式。

如果主题文件解析失败，SonicTerm 会写日志，并回退到内置 `wezterm` 主题。
