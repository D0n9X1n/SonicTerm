# Configuration / 配置

## English

Config file path on macOS and Windows: `~/.snoicterm/sonicterm.toml`

Minimal example:

```toml
theme = "wezterm"
# Platform default:
#   macOS   -> sonicterm-macos
#   Windows -> sonicterm-windows
#   Linux   -> sonicterm-linux
keymap = "sonicterm-macos"
locale = ""

[font]
family = "Rec Mono St.Helens"
size = 13
line_height = 1.3

[window]
# Terminal content margins:
# +---------------- window ----------------+
# | padding_top                            |
# |  terminal grid (cols x rows)           |
# | padding_bottom                         |
# +----------------------------------------+
#   ^ padding_left        padding_right ^
cols = 100
rows = 30
padding_left = 12
padding_right = 12
padding_top = 10
padding_bottom = 4

[terminal]
cursor_blink = true
cursor_shape = "block"

[appearance]
# Floating panel inner padding:
# +------------- panel -------------+
# | panel_padding                   |
# |  command palette / cheatsheet   |
# | panel_padding                   |
# +---------------------------------+
opacity = 1.0
panel_padding = 2.0
scrollbar = "auto"
```

Use the command palette entries **Edit sonicterm.toml**, **Edit keymap.toml**,
and **Reload Config** to edit and reload settings.

## 中文

macOS 和 Windows 的配置文件路径：`~/.snoicterm/sonicterm.toml`

最小示例同上。可以通过命令面板里的 **Edit sonicterm.toml**、
**Edit keymap.toml** 和 **Reload Config** 编辑并热加载配置。
