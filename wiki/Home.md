# SonicTerm Wiki / SonicTerm 百科

<div align="center">
  <img src="../assets/icons/exports/png/sonic-256.png" alt="SonicTerm" width="150" height="150"/>
</div>

## English

SonicTerm is a GPU-accelerated terminal for macOS and Windows. This wiki covers
daily usage, configuration, keybindings, logging, and theme authoring.

## How SonicTerm works

SonicTerm is split into small Rust crates with a clear data flow:

1. `sonicterm-mac` / `sonicterm-windows` start the native app, load config, and
   initialize logging.
2. `sonicterm-app` owns windows, tabs, panes, PTYs, drag/drop, command palette,
   search, IME state, notifications, and redraw scheduling.
3. Each pane has a PTY process handled by `sonicterm-io`; bytes from the PTY are
   parsed by `sonicterm-vt` into a `sonicterm-grid` screen with scrollback and
   dirty rows.
4. `sonicterm-render-model` carries renderer-agnostic frame data into
   `sonicterm-gpu`.
5. `sonicterm-gpu` uses wgpu, a glyph atlas, retained-frame damage regions, and
   batched quads/glyphs to draw the terminal. On software renderers it lowers
   frame pressure and uses dirty regions to avoid unnecessary work.
6. Tab tear-out uses a warm hidden-window pool so dropping a tab into a new window
   can reuse pre-created window/renderer state; the pool remains useful on
   no-GPU/software-render machines.
7. Config, keymaps, themes, and logs live under `~/.sonicterm`; command-palette
   actions let you edit and reload them without leaving the terminal.

- [Usage](Usage)
- [Configuration](Configuration)
- [Keybindings](Keybindings)
- [Logging](Logging)
- [Themes](Themes)

## 中文

SonicTerm 是一个面向 macOS 和 Windows 的 GPU 加速终端。这个 Wiki 覆盖日常使用、
配置、快捷键、日志和主题制作。

## SonicTerm 如何工作

SonicTerm 由多个小型 Rust crate 组成，数据流清晰：

1. `sonicterm-mac` / `sonicterm-windows` 启动原生应用，加载配置并初始化日志。
2. `sonicterm-app` 管理窗口、Tab、Pane、PTY、拖拽、命令面板、搜索、输入法、通知和重绘调度。
3. 每个 Pane 都有一个由 `sonicterm-io` 管理的 PTY 进程；PTY 输出由 `sonicterm-vt` 解析到 `sonicterm-grid`，Grid 负责屏幕内容、scrollback 和 dirty rows。
4. `sonicterm-render-model` 把与渲染器无关的 frame 数据传给 `sonicterm-gpu`。
5. `sonicterm-gpu` 基于 wgpu、glyph atlas、retained-frame damage region 和批量 quad/glyph 绘制终端。遇到软件渲染器时会降低帧压力，并用 dirty region 减少不必要的工作。
6. Tab 拖出窗口使用隐藏预热窗口池：新窗口可以复用预先创建好的 window/renderer 状态；无 GPU / 软件渲染环境下也会使用这个池。
7. 配置、keymap、主题和日志都在 `~/.sonicterm` 下；可以通过命令面板编辑并重新加载。

- [用法 / Usage](Usage)
- [配置 / Configuration](Configuration)
- [快捷键 / Keybindings](Keybindings)
- [日志 / Logging](Logging)
- [主题 / Themes](Themes)
