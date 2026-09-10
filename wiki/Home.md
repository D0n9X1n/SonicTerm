# SonicTerm Wiki / SonicTerm 百科

## English

SonicTerm is a native, GPU-accelerated terminal for macOS, Windows, and Linux.
**Start with [Usage](Usage)** to install it and learn everyday actions.

### User guide

- [Usage](Usage) — install, open tabs, split panes, select text, and use rmux/tmux
- [Configuration](Configuration) — change defaults in `~/.sonicterm/sonicterm.toml`, reload, and save
- [Keybindings](Keybindings) — find a shortcut or write a binding
- [Themes](Themes) — choose or create a color palette
- [Logging](Logging) — find logs, investigate a problem, and prepare a bug report
- [Memory](Memory) — understand resource limits and retained-memory reports

### How SonicTerm works

Read [Architecture](Architecture) for the map, then
[From Keypress to Pixel](From-Keypress-to-Pixel) for one `A`'s round trip.
Use the references below for a particular subsystem.

- [Architecture](Architecture) — system shape and crate boundaries
- [From Keypress to Pixel](From-Keypress-to-Pixel) — input, child output, grid, glyph, and pixel
- [Runtime Lifecycle](Runtime-Lifecycle) — startup, ownership changes, tab transfers, and shutdown
- [Terminal IO and VT](Terminal-IO-and-VT) — PTYs, parser protocols, shell integration, and grid rules
- [Rendering Modes](Rendering-Modes) — adapter selection and frame pacing
- [Rendering and Fonts](Rendering-and-Fonts) — typography, atlases, GPU/CPU drawing, and damage
- [Platform Integration](Platform-Integration) — AppKit, Win32, X11, and Wayland boundaries
- [Architecture Internals](Architecture-Internals) — correctness, accounting, and lifetime invariants
- [Crate Reference](Crate-Reference) — all 23 crates, their interfaces, and dependencies

### Build and contribute

- [Packaging](Packaging) — build local packages and inspect their layouts
- [Development and Release](Development-and-Release) — exact gates, PR workflow, releases, and Wiki publication
- [Home](Home) — return to this index

## 中文

SonicTerm 是面向 macOS、Windows 和 Linux 的原生 GPU 加速终端。
**先读[用法](Usage)**，完成安装并熟悉日常操作。

### 用户手册

- [用法](Usage) — 安装、新建标签页、分屏、选取文字和使用 rmux/tmux
- [配置](Configuration) — 修改 `~/.sonicterm/sonicterm.toml`、重载与保存
- [快捷键](Keybindings) — 查找快捷键或编写绑定
- [主题](Themes) — 选择或创建配色
- [日志](Logging) — 查找日志、排查问题和准备缺陷报告
- [内存](Memory) — 理解资源上限与常驻内存报告

### SonicTerm 如何工作

先用[架构](Architecture)了解全貌，再读[从按键到像素](From-Keypress-to-Pixel)，
跟随一个 `A` 完成往返。需要深入某个子系统时查阅下列参考。

- [架构](Architecture) — 系统结构与 crate 边界
- [从按键到像素](From-Keypress-to-Pixel) — 输入、子进程输出、网格、字形与像素
- [运行时生命周期](Runtime-Lifecycle) — 启动、所有权变化、标签页转移与退出
- [终端 IO 与 VT](Terminal-IO-and-VT) — PTY、解析器协议、shell 集成与网格规则
- [渲染模式](Rendering-Modes) — 适配器选择与帧节奏
- [渲染与字体](Rendering-and-Fonts) — 字体、图集、GPU/CPU 绘制与损伤区域
- [平台集成](Platform-Integration) — AppKit、Win32、X11 与 Wayland 边界
- [架构内部机制](Architecture-Internals) — 正确性、记账与生命周期不变量
- [Crate 参考](Crate-Reference) — 全部 23 个 crate、接口与依赖

### 构建与贡献

- [打包](Packaging) — 本地生成安装包并查看布局
- [开发与发布](Development-and-Release) — 完整 gate、PR 流程、release 与 Wiki 发布
- [首页](Home) — 返回本索引
