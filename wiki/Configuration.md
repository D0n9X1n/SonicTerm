# Configuration / 配置

## English

Config file paths:

- macOS: `~/Library/Application Support/SonicTerm/sonicterm.toml`
- Windows: `%APPDATA%\SonicTerm\sonicterm.toml`

Minimal example:

```toml
theme = "wezterm"
keymap = "sonicterm-macos"
locale = ""

[font]
family = "Rec Mono St.Helens"
size = 14
line_height = 1.1

[window]
cols = 100
rows = 30
padding_left = 12
padding_right = 12
padding_top = 4
padding_bottom = 4

[terminal]
cursor_blink = true
cursor_shape = "block"

[appearance]
opacity = 1.0
panel_padding = 2.0
scrollbar = "auto"
```

Use the command palette entries **Edit sonicterm.toml**, **Edit keymap.toml**,
and **Reload Config** to edit and reload settings.

## 中文

配置文件路径：

- macOS: `~/Library/Application Support/SonicTerm/sonicterm.toml`
- Windows: `%APPDATA%\SonicTerm\sonicterm.toml`

最小示例同上。可以通过命令面板里的 **Edit sonicterm.toml**、
**Edit keymap.toml** 和 **Reload Config** 编辑并热加载配置。
