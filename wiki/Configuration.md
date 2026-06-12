# Configuration / 配置

## English

Config file path on macOS and Windows: `~/.sonicterm/sonicterm.toml`

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
padding_top = 8
padding_bottom = 4

[terminal]
# TERM_PROGRAM passed to child PTYs. Some tools, such as Copilot, do not
# recognize SonicTerm yet; setting term_program = "WezTerm" can bypass their
# terminal checks and enable their WezTerm/new terminal UI path.
term_program = "SonicTerm"
# Scrollback lines kept per pane. Lowering this at runtime drops the oldest
# history immediately; 0 disables scrollback.
scrollback = 10000
cursor_blink = true
cursor_shape = "block"

[appearance]
# Floating panel inner padding:
# +------------- panel -------------+
# | panel_padding                   |
# |        command palette          |
# | panel_padding                   |
# +---------------------------------+
opacity = 1.0
panel_padding = 2.0
scrollbar = "auto"
# No-GPU handling. When there is no usable GPU (RDP / VM / VDI) the renderer
# falls back to a CPU rasterizer; "auto" detects that and lowers the frame cap
# + disables per-frame fade animation to stay responsive. "force" always
# degrades; "off" never does.
software_render_mode = "auto"
```

Notes:

- **scrollbar** drag works: grab the thumb to scroll, click the track to page.
- **scrollback** is per pane; changing it via Reload Config applies to every
  open pane immediately.

Use the command palette entries **Edit sonicterm.toml**, **Edit keymap.toml**,
and **Reload Config** to edit and reload settings.

## 中文

macOS 和 Windows 的配置文件路径：`~/.sonicterm/sonicterm.toml`

最小示例同上。可以通过命令面板里的 **Edit sonicterm.toml**、
**Edit keymap.toml** 和 **Reload Config** 编辑并热加载配置。

常用配置项：

- `[terminal] scrollback`：每个面板保留的历史行数；运行时调小会立即丢弃最旧
  的历史，设为 `0` 关闭回滚。
- `[appearance] scrollbar`：`auto`（悬停/滚动时显示）/`always`/`never`。
  滚动条支持鼠标拖动滑块、点击轨道翻页。
- `[appearance] software_render_mode`：无 GPU（RDP / 虚拟机 / VDI）时渲染会
  回退到 CPU 软件光栅。`auto` 自动检测并降帧、关闭逐帧淡入动画以保持响应；
  `force` 始终降级；`off` 从不降级。

