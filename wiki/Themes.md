# Themes / 主题

## English

Bundled themes live in `assets/themes/`. The default `wezterm` theme is a
modified Gruvbox dark hard palette with SonicTerm's near-black background and a
yellow cursor.

Theme file shape:

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

Place custom themes under `~/.snoicterm/themes/<name>.toml` and set
`theme = "<name>"` in `sonicterm.toml`.

## 中文

内置主题位于 `assets/themes/`。默认 `wezterm` 主题是修改过背景色的 Gruvbox
dark hard：背景是 SonicTerm 的 near-black，cursor 是黄色。

自定义主题放在 `~/.snoicterm/themes/<name>.toml`，然后在 `sonicterm.toml`
里设置 `theme = "<name>"`。主题字段结构见上方示例。
