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
# Max width (logical px) of a single tab when the bar has room. Tabs grow up to
# this so long titles stay readable; with many tabs they shrink to share the bar.
tab_max_width = 240

[font]
family = "Rec Mono St.Helens"
size = 13
line_height = 1.3
# Glyph rasterizer. Platform default: "directwrite" on Windows (falling back to
# FreeType if DirectWrite cannot rasterize a glyph), "freetype" on macOS/Linux.
# Override with "freetype", "harfbuzz", or "directwrite".
# font_rasterizer = "directwrite"

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
# Hidden pre-created child windows for fast tab tear-out. The default keeps
# one spare available even immediately after consuming a warm window.
warm_window_pool = 2

[terminal]
# Shell to spawn. Omit to auto-detect (Windows: PowerShell 7 `pwsh.exe`,
# including the Microsoft Store install, falling back to Windows PowerShell
# then cmd.exe; macOS/Linux: $SHELL). Set to override:
# shell = "pwsh.exe"
# TERM_PROGRAM passed to child PTYs. Some tools, such as Copilot, do not
# recognize SonicTerm yet; setting term_program = "WezTerm" can bypass their
# terminal checks and enable their WezTerm/new terminal UI path.
term_program = "SonicTerm"
# Scrollback lines kept per pane. Lowering this at runtime drops the oldest
# history immediately; 0 disables scrollback.
scrollback = 1000
# Cursor behavior. cursor_blink defaults to false (a steady cursor avoids
# unnecessary redraws, which matters most on the software-render path).
cursor_blink = false
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
# degrades; "off" never does. On Windows the software path repaints the whole
# surface deterministically each frame for stable output over RDP.
software_render_mode = "auto"
```

Notes:

- **tab_max_width** caps how wide a single tab gets (logical px, default 240).
  Tabs grow up to this width when the bar has room so long titles stay readable;
  with many tabs open they shrink to share the bar evenly. Applies on Reload Config.
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

- `tab_max_width`：单个标签页的最大宽度（逻辑像素，默认 `240`）。标签栏有空间时，
  标签会增宽到该值以便显示完整标题；标签很多时则会等分压缩。Reload Config 后生效。
- `[font] font_rasterizer`：字形光栅器。平台默认值：Windows 为 `directwrite`
  （某个字形无法用 DirectWrite 光栅化时回退到 FreeType），macOS/Linux 为
  `freetype`。可显式覆盖为 `freetype`、`harfbuzz` 或 `directwrite`。
- `[terminal] cursor_blink`：光标是否闪烁，默认 `false`（稳定光标可减少不必要的
  重绘，对软件渲染路径尤其有意义）。
- `[terminal] shell`：要启动的 shell。留空则自动探测（Windows：优先
  PowerShell 7 `pwsh.exe`，含 Microsoft Store 安装，找不到再依次回退到
  Windows PowerShell、`cmd.exe`；macOS/Linux：`$SHELL`）。可显式覆盖，
  例如 `shell = "pwsh.exe"`。
- `[terminal] scrollback`：每个面板保留的历史行数；运行时调小会立即丢弃最旧
  的历史，设为 `0` 关闭回滚。
- `[appearance] scrollbar`：`auto`（悬停/滚动时显示）/`always`/`never`。
  滚动条支持鼠标拖动滑块、点击轨道翻页。
- `[appearance] software_render_mode`：无 GPU（RDP / 虚拟机 / VDI）时渲染会
  回退到 CPU 软件光栅。`auto` 自动检测并降帧、关闭逐帧淡入动画以保持响应；
  `force` 始终降级；`off` 从不降级。Windows 上软件路径会每帧确定性地全屏重绘，
  以保证 RDP 等场景下输出稳定。
- `[window] warm_window_pool`：后台保持隐藏的预热窗口数量，用于让 tab 拖出新窗口
  更快；默认 `2`，这样消耗一个预热窗口后仍保留一个备用。

